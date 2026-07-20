//! Persistence for the post-session review ([C6.5]), plus the one query that only a
//! review needs: which personal records a *particular session* set.

use anyhow::Context as _;
use rusqlite::{OptionalExtension as _, params};

use super::database::Database;
use super::models::{MeasurementType, SessionPersonalRecord, StoredReview};

impl Database {
    /// Store a review, replacing any review already held for that session.
    ///
    /// `session_id` is UNIQUE, so the upsert is the whole regeneration story: an
    /// effort correction re-runs the generator and the new snapshot takes the old
    /// one's place, rather than accumulating a pile of near-identical reviews whose
    /// only distinguishing feature is which one came last.
    pub fn upsert_session_review(
        &self,
        session_id: i64,
        user_id: i64,
        roster_id: Option<i64>,
        kind: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        self.conn()
            .execute(
                "INSERT INTO session_reviews (session_id, user_id, roster_id, kind, body) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(session_id) DO UPDATE SET \
                     roster_id = excluded.roster_id, \
                     kind = excluded.kind, \
                     body = excluded.body, \
                     created_at = datetime('now')",
                params![session_id, user_id, roster_id, kind, body],
            )
            .with_context(|| format!("Failed to store the review for session {session_id}"))?;
        Ok(())
    }

    /// The review held for one session, if it has been reviewed.
    pub fn get_session_review(&self, session_id: i64) -> anyhow::Result<Option<StoredReview>> {
        self.conn()
            .query_row(
                "SELECT session_id, roster_id, kind, body, created_at FROM session_reviews WHERE session_id = ?1",
                params![session_id],
                row_to_stored_review,
            )
            .optional()
            .context("Failed to read the session review")
    }

    /// The user's most recent review — what `/review` renders.
    ///
    /// Ordered by the session that was reviewed, not by when the review was written:
    /// regenerating an older session's review must not make it the latest one.
    pub fn latest_session_review(&self, user_id: i64) -> anyhow::Result<Option<StoredReview>> {
        self.conn()
            .query_row(
                "SELECT sr.session_id, sr.roster_id, sr.kind, sr.body, sr.created_at \
                 FROM session_reviews sr \
                 JOIN sessions s ON s.id = sr.session_id \
                 WHERE sr.user_id = ?1 \
                 ORDER BY s.started_at DESC, s.id DESC \
                 LIMIT 1",
                params![user_id],
                row_to_stored_review,
            )
            .optional()
            .context("Failed to read the latest session review")
    }

    /// The personal records set *during one session*, each with the best that stood
    /// before it.
    ///
    /// Distinct from [`Database::personal_records`], which answers "what are this
    /// user's all-time bests" and cannot answer this one: it carries no session
    /// linkage, and date-matching its `achieved_at` against the session window would
    /// miscredit a second session on the same day, or a set logged outside any
    /// session at all.
    ///
    /// A record here means the session's best set for an exercise beat every set of
    /// that exercise logged before the session started. `previous` is `None` for a
    /// first-ever effort, which the caller is expected to present as such rather than
    /// dress up as a record broken.
    pub fn session_personal_records(&self, session_id: i64) -> anyhow::Result<Vec<SessionPersonalRecord>> {
        let mut stmt = self.conn().prepare(
            "WITH session_bests AS ( \
                 SELECT s.exercise_type_id, s.measurement_type_id, \
                        MAX(s.value) AS best_value, \
                        (SELECT s2.count FROM sets s2 \
                         JOIN exercise_entries ee2 ON ee2.id = s2.exercise_entry_id \
                         WHERE ee2.session_id = ?1 AND s2.exercise_type_id = s.exercise_type_id \
                         ORDER BY s2.value DESC, s2.count DESC, s2.id DESC LIMIT 1) AS best_count \
                 FROM sets s \
                 JOIN exercise_entries ee ON ee.id = s.exercise_entry_id \
                 WHERE ee.session_id = ?1 \
                 GROUP BY s.exercise_type_id, s.measurement_type_id \
             ), \
             prior AS ( \
                 SELECT s.exercise_type_id, MAX(s.value) AS prior_value \
                 FROM sets s \
                 JOIN exercise_entries ee ON ee.id = s.exercise_entry_id \
                 WHERE ee.user_id = (SELECT user_id FROM sessions WHERE id = ?1) \
                   AND s.logged_at < (SELECT started_at FROM sessions WHERE id = ?1) \
                 GROUP BY s.exercise_type_id \
             ) \
             SELECT et.name, mt.name, sb.best_value, sb.best_count, prior.prior_value, \
                    (SELECT s3.count FROM sets s3 \
                     JOIN exercise_entries ee3 ON ee3.id = s3.exercise_entry_id \
                     WHERE ee3.user_id = (SELECT user_id FROM sessions WHERE id = ?1) \
                       AND s3.exercise_type_id = sb.exercise_type_id \
                       AND s3.logged_at < (SELECT started_at FROM sessions WHERE id = ?1) \
                     ORDER BY s3.value DESC, s3.count DESC, s3.id DESC LIMIT 1) AS prior_count \
             FROM session_bests sb \
             JOIN exercise_types et ON et.id = sb.exercise_type_id \
             JOIN measurement_types mt ON mt.id = sb.measurement_type_id \
             LEFT JOIN prior ON prior.exercise_type_id = sb.exercise_type_id \
             WHERE prior.prior_value IS NULL OR sb.best_value > prior.prior_value \
             ORDER BY et.name",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(SessionPersonalRecord {
                exercise_name: row.get(0)?,
                measurement_type: MeasurementType::from_str_loose(&row.get::<_, String>(1)?),
                value: row.get(2)?,
                count: row.get(3)?,
                previous_value: row.get(4)?,
                previous_count: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to query the session's personal records")
    }
}

fn row_to_stored_review(row: &rusqlite::Row) -> rusqlite::Result<StoredReview> {
    Ok(StoredReview {
        session_id: row.get(0)?,
        roster_id: row.get(1)?,
        kind: row.get(2)?,
        body: row.get(3)?,
        created_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{new_exercise_entry_at, new_exercise_set, new_user};

    fn fixture() -> (Database, i64, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        (db, user_id, bp.id)
    }

    /// Log one weight_reps set into a session, at a given time.
    fn log_set(db: &Database, user_id: i64, session_id: i64, exercise_id: i64, at: &str, weight: f64, reps: i32) {
        let entry_id = db.insert_entry(&new_exercise_entry_at(user_id, Some(session_id), None, at)).unwrap();
        let mut set = new_exercise_set(entry_id, exercise_id, MeasurementType::WeightReps, weight);
        set.count = Some(reps);
        set.logged_at = at.to_string();
        db.insert_set(&set).unwrap();
    }

    #[test]
    fn a_review_is_stored_and_read_back() {
        let (db, user_id, _) = fixture();
        let session_id = db.start_session_at(user_id, "2026-07-01 09:00:00", Some("2026-07-01 10:00:00"), None).unwrap();

        db.upsert_session_review(session_id, user_id, None, "summary", r#"{"headline":"ok"}"#).unwrap();

        let stored = db.get_session_review(session_id).unwrap().unwrap();
        assert_eq!(stored.session_id, session_id);
        assert_eq!(stored.kind, "summary");
        assert_eq!(stored.body, r#"{"headline":"ok"}"#);
        assert!(stored.roster_id.is_none());
    }

    /// Regenerating replaces rather than accumulates — `session_id` is UNIQUE, and a
    /// correction must leave exactly one review standing.
    #[test]
    fn regenerating_a_review_replaces_the_old_one() {
        let (db, user_id, _) = fixture();
        let session_id = db.start_session_at(user_id, "2026-07-01 09:00:00", Some("2026-07-01 10:00:00"), None).unwrap();

        db.upsert_session_review(session_id, user_id, None, "summary", r#"{"effort":"derived"}"#).unwrap();
        db.upsert_session_review(session_id, user_id, None, "report", r#"{"effort":"confirmed"}"#).unwrap();

        let stored = db.get_session_review(session_id).unwrap().unwrap();
        assert_eq!(stored.kind, "report");
        assert_eq!(stored.body, r#"{"effort":"confirmed"}"#);

        let count: i64 =
            db.conn().query_row("SELECT COUNT(*) FROM session_reviews WHERE session_id = ?1", params![session_id], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "the upsert must replace, not accumulate");
    }

    /// `/review` shows the latest *session's* review. Regenerating an older session's
    /// review rewrites history, so it must not also reorder it.
    #[test]
    fn latest_review_follows_the_session_not_the_write() {
        let (db, user_id, _) = fixture();
        let older = db.start_session_at(user_id, "2026-07-01 09:00:00", Some("2026-07-01 10:00:00"), None).unwrap();
        let newer = db.start_session_at(user_id, "2026-07-05 09:00:00", Some("2026-07-05 10:00:00"), None).unwrap();

        db.upsert_session_review(newer, user_id, None, "summary", r#"{"n":"newer"}"#).unwrap();
        // Written last, but for the earlier session.
        db.upsert_session_review(older, user_id, None, "summary", r#"{"n":"older"}"#).unwrap();

        let latest = db.latest_session_review(user_id).unwrap().unwrap();
        assert_eq!(latest.session_id, newer);
        assert_eq!(latest.body, r#"{"n":"newer"}"#);
    }

    #[test]
    fn no_review_yet_is_not_an_error() {
        let (db, user_id, _) = fixture();
        assert!(db.latest_session_review(user_id).unwrap().is_none());
        assert!(db.get_session_review(999).unwrap().is_none());
    }

    /// The core of the session-PR query: only sets that beat everything logged before
    /// the session started count, and the mark they beat travels with them.
    #[test]
    fn session_records_report_only_what_beat_the_previous_best() {
        let (db, user_id, bp_id) = fixture();

        let old = db.start_session_at(user_id, "2026-06-01 09:00:00", Some("2026-06-01 10:00:00"), None).unwrap();
        log_set(&db, user_id, old, bp_id, "2026-06-01 09:10:00", 80.0, 5);

        let new = db.start_session_at(user_id, "2026-07-01 09:00:00", Some("2026-07-01 10:00:00"), None).unwrap();
        log_set(&db, user_id, new, bp_id, "2026-07-01 09:10:00", 85.0, 5);

        let records = db.session_personal_records(new).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].exercise_name, "Bench Press");
        assert_eq!(records[0].value, 85.0);
        assert_eq!(records[0].count, Some(5));
        assert_eq!(records[0].previous_value, Some(80.0), "the record names the mark it beat");
        assert_eq!(records[0].previous_count, Some(5));

        // The session that set the 80kg mark had nothing before it: a first effort, not
        // a record broken.
        let first = db.session_personal_records(old).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].previous_value.is_none(), "a first-ever effort has no previous best");
    }

    /// Repeating a load is not a record. The all-time `personal_records` query would
    /// still list the exercise; this one must not.
    #[test]
    fn matching_a_previous_best_is_not_a_record() {
        let (db, user_id, bp_id) = fixture();

        let old = db.start_session_at(user_id, "2026-06-01 09:00:00", Some("2026-06-01 10:00:00"), None).unwrap();
        log_set(&db, user_id, old, bp_id, "2026-06-01 09:10:00", 80.0, 5);

        let new = db.start_session_at(user_id, "2026-07-01 09:00:00", Some("2026-07-01 10:00:00"), None).unwrap();
        log_set(&db, user_id, new, bp_id, "2026-07-01 09:10:00", 80.0, 5);

        assert!(db.session_personal_records(new).unwrap().is_empty(), "equalling a best is not beating it");
    }

    /// Two sessions on the same day is the case a date-based filter over the all-time
    /// records gets wrong — the morning's PR must not be credited to the evening.
    #[test]
    fn a_record_belongs_only_to_the_session_that_set_it() {
        let (db, user_id, bp_id) = fixture();

        let morning = db.start_session_at(user_id, "2026-07-01 08:00:00", Some("2026-07-01 09:00:00"), None).unwrap();
        log_set(&db, user_id, morning, bp_id, "2026-07-01 08:10:00", 90.0, 3);

        let evening = db.start_session_at(user_id, "2026-07-01 18:00:00", Some("2026-07-01 19:00:00"), None).unwrap();
        log_set(&db, user_id, evening, bp_id, "2026-07-01 18:10:00", 85.0, 3);

        assert_eq!(db.session_personal_records(morning).unwrap().len(), 1, "the morning set the record");
        assert!(db.session_personal_records(evening).unwrap().is_empty(), "the evening did not beat it, same day or not");
    }
}

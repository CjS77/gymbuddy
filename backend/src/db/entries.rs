use anyhow::Context as _;
use rusqlite::{Row, params};

use super::database::Database;
use super::models::{Difficulty, ExerciseEntry, ExerciseSet, ExerciseTypeWithAncestry, MeasurementType, Session, SessionSummary};

// ── Sessions ───────────────────────────────────────────────────────────────────

fn row_to_session(row: &Row) -> rusqlite::Result<Session> {
    Ok(Session { id: row.get(0)?, user_id: row.get(1)?, started_at: row.get(2)?, ended_at: row.get(3)?, notes: row.get(4)? })
}

const SELECT_SESSION: &str = "SELECT id, user_id, started_at, ended_at, notes FROM sessions";

impl Database {
    pub fn start_session(&self, user_id: i64, notes: Option<&str>) -> anyhow::Result<Session> {
        self.conn().execute("INSERT INTO sessions (user_id, notes) VALUES (?1, ?2)", params![user_id, notes])?;
        let id = self.conn().last_insert_rowid();
        let session = self.get_session(id)?.context("Session disappeared immediately after insert")?;
        Ok(session)
    }

    /// End a session and cascade-close every still-open exercise_entry that
    /// belongs to it. Both writes use the same `datetime('now')` value so the
    /// session and its entries share a precise end timestamp.
    pub fn end_session(&self, session_id: i64) -> anyhow::Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let rows = tx.execute("UPDATE sessions SET ended_at = datetime('now') WHERE id = ?1 AND ended_at IS NULL", params![session_id])?;
        anyhow::ensure!(rows > 0, "session id {session_id} not found or already ended");
        tx.execute(
            "UPDATE exercise_entry \
             SET end_timestamp = (SELECT ended_at FROM sessions WHERE id = ?1) \
             WHERE session_id = ?1 AND end_timestamp IS NULL",
            params![session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_session(&self, id: i64) -> anyhow::Result<Option<Session>> {
        let sql = format!("{SELECT_SESSION} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_session)?;
        rows.next().transpose().context("Failed to read session row")
    }

    pub fn get_active_session(&self, user_id: i64) -> anyhow::Result<Option<Session>> {
        let sql = format!("{SELECT_SESSION} WHERE user_id = ?1 AND ended_at IS NULL ORDER BY started_at DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_session)?;
        rows.next().transpose().context("Failed to read active session")
    }

    pub fn list_sessions(&self, user_id: i64, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Vec<Session>> {
        let sql = format!(
            "{SELECT_SESSION} \
             WHERE user_id = ?1 \
               AND (?2 IS NULL OR started_at >= ?2) \
               AND (?3 IS NULL OR started_at <= ?3) \
             ORDER BY started_at DESC"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, from, to], row_to_session)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list sessions")
    }

    pub fn list_session_summaries(&self, user_id: i64, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Vec<SessionSummary>> {
        let mut stmt = self.conn().prepare(
            "SELECT s.id, s.user_id, s.started_at, s.ended_at, s.notes, \
                    COUNT(DISTINCT ee.id) AS exercise_count, \
                    CASE WHEN s.ended_at IS NULL THEN NULL \
                         ELSE CAST((julianday(s.ended_at) - julianday(s.started_at)) * 24 * 60 AS INTEGER) \
                    END AS duration_mins \
             FROM sessions s \
             LEFT JOIN exercise_entry ee ON ee.session_id = s.id \
             WHERE s.user_id = ?1 \
               AND (?2 IS NULL OR s.started_at >= ?2) \
               AND (?3 IS NULL OR s.started_at <= ?3) \
             GROUP BY s.id \
             ORDER BY s.started_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id, from, to], |row| {
            let session =
                Session { id: row.get(0)?, user_id: row.get(1)?, started_at: row.get(2)?, ended_at: row.get(3)?, notes: row.get(4)? };
            Ok(SessionSummary { session, exercise_count: row.get(5)?, duration_mins: row.get(6)? })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list session summaries")
    }

    pub fn session_count_by_week(&self, user_id: i64, weeks: i32) -> anyhow::Result<Vec<(String, i32)>> {
        let mut stmt = self.conn().prepare(
            "SELECT strftime('%G-W%V', started_at) AS week, COUNT(*) \
             FROM sessions \
             WHERE user_id = ?1 \
               AND started_at >= datetime('now', ?2) \
             GROUP BY week \
             ORDER BY week",
        )?;
        let period = format!("-{weeks} days", weeks = weeks * 7);
        let rows = stmt.query_map(params![user_id, period], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to query session count by week")
    }
}

// ── Exercise entries ───────────────────────────────────────────────────────────

fn row_to_entry(row: &Row) -> rusqlite::Result<ExerciseEntry> {
    Ok(ExerciseEntry {
        id: row.get(0)?,
        user_id: row.get(1)?,
        session_id: row.get(2)?,
        start_timestamp: row.get(3)?,
        end_timestamp: row.get(4)?,
        comment: row.get(5)?,
    })
}

const SELECT_ENTRY: &str = "SELECT id, user_id, session_id, start_timestamp, end_timestamp, comment FROM exercise_entry";

impl Database {
    pub fn insert_entry(&self, entry: &ExerciseEntry) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO exercise_entry (user_id, session_id, start_timestamp, end_timestamp, comment) \
             VALUES (?1, ?2, COALESCE(?3, datetime('now')), ?4, ?5)",
            params![
                entry.user_id,
                entry.session_id,
                if entry.start_timestamp.is_empty() { None } else { Some(&entry.start_timestamp) },
                entry.end_timestamp,
                entry.comment,
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn end_entry(&self, entry_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE exercise_entry SET end_timestamp = datetime('now') WHERE id = ?1 AND end_timestamp IS NULL",
            params![entry_id],
        )?;
        anyhow::ensure!(rows > 0, "exercise_entry id {entry_id} not found or already closed");
        Ok(())
    }

    pub fn get_entry(&self, id: i64) -> anyhow::Result<Option<ExerciseEntry>> {
        let sql = format!("{SELECT_ENTRY} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_entry)?;
        rows.next().transpose().context("Failed to read exercise_entry")
    }

    pub fn list_entries_for_session(&self, session_id: i64) -> anyhow::Result<Vec<ExerciseEntry>> {
        let sql = format!("{SELECT_ENTRY} WHERE session_id = ?1 ORDER BY start_timestamp");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![session_id], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list entries for session")
    }

    pub fn list_entries_for_user(&self, user_id: i64, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Vec<ExerciseEntry>> {
        let sql = format!(
            "{SELECT_ENTRY} \
             WHERE user_id = ?1 \
               AND (?2 IS NULL OR start_timestamp >= ?2) \
               AND (?3 IS NULL OR start_timestamp <= ?3) \
             ORDER BY start_timestamp DESC"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, from, to], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list entries for user")
    }

    /// Open exercise_entry (no `end_timestamp`) in `session_id` whose sets include
    /// `exercise_type_id`. Used to decide whether to reuse or create a new entry
    /// when the user logs another set of an exercise.
    pub fn find_open_entry_for_exercise(
        &self,
        user_id: i64,
        session_id: i64,
        exercise_type_id: i64,
    ) -> anyhow::Result<Option<ExerciseEntry>> {
        let mut stmt = self.conn().prepare(
            "SELECT ee.id, ee.user_id, ee.session_id, ee.start_timestamp, ee.end_timestamp, ee.comment \
             FROM exercise_entry ee \
             WHERE ee.user_id = ?1 AND ee.session_id = ?2 AND ee.end_timestamp IS NULL \
               AND EXISTS (SELECT 1 FROM sets s \
                           WHERE s.exercise_entry_id = ee.id AND s.exercise_type_id = ?3) \
             ORDER BY ee.start_timestamp DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![user_id, session_id, exercise_type_id], row_to_entry)?;
        rows.next().transpose().context("Failed to read open entry for exercise")
    }

    /// An open exercise_entry in `session_id` whose exercise type is a transitive
    /// ancestor OR descendant of `exercise_type_id` (never the same type). Used to
    /// detect that a logged set may belong to an exercise already in progress
    /// rather than being a new superset. Returns the entry plus its own
    /// exercise_type_id (taken from its sets). Most-recently-started first.
    pub fn find_open_related_entry(&self, session_id: i64, exercise_type_id: i64) -> anyhow::Result<Option<(ExerciseEntry, i64)>> {
        let mut stmt = self.conn().prepare(
            "WITH RECURSIVE \
             ancestors(id) AS ( \
                 SELECT parent_id FROM exercise_types WHERE id = ?2 AND parent_id IS NOT NULL \
                 UNION ALL \
                 SELECT et.parent_id FROM exercise_types et JOIN ancestors a ON et.id = a.id \
                 WHERE et.parent_id IS NOT NULL \
             ), \
             descendants(id) AS ( \
                 SELECT id FROM exercise_types WHERE parent_id = ?2 \
                 UNION ALL \
                 SELECT et.id FROM exercise_types et JOIN descendants d ON et.parent_id = d.id \
             ), \
             related(id) AS (SELECT id FROM ancestors UNION SELECT id FROM descendants) \
             SELECT ee.id, ee.user_id, ee.session_id, ee.start_timestamp, ee.end_timestamp, ee.comment, \
                    s.exercise_type_id \
             FROM exercise_entry ee \
             JOIN sets s ON s.exercise_entry_id = ee.id \
             WHERE ee.session_id = ?1 AND ee.end_timestamp IS NULL \
               AND s.exercise_type_id IN (SELECT id FROM related) \
             ORDER BY ee.start_timestamp DESC, s.order_idx ASC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![session_id, exercise_type_id], |row| Ok((row_to_entry(row)?, row.get::<_, i64>(6)?)))?;
        rows.next().transpose().context("Failed to read open related entry")
    }

    /// All open exercise_entries in a session, oldest first. >1 row = a superset.
    pub fn list_open_entries_for_session(&self, session_id: i64) -> anyhow::Result<Vec<ExerciseEntry>> {
        let sql = format!("{SELECT_ENTRY} WHERE session_id = ?1 AND end_timestamp IS NULL ORDER BY start_timestamp");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![session_id], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list open entries for session")
    }

    /// All open exercise_entries for a user, across sessions. Used to detect leaks
    /// from previously-ended sessions and from older code paths that did not close
    /// entries.
    pub fn list_open_entries_for_user(&self, user_id: i64) -> anyhow::Result<Vec<ExerciseEntry>> {
        let sql = format!("{SELECT_ENTRY} WHERE user_id = ?1 AND end_timestamp IS NULL ORDER BY start_timestamp");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id], row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list open entries for user")
    }

    /// Number of sets in an exercise_entry. Used by the set-count checkpoint and
    /// the premature-close pushback in the assistant handler.
    pub fn count_sets_for_entry(&self, entry_id: i64) -> anyhow::Result<i64> {
        let count: i64 =
            self.conn().query_row("SELECT COUNT(*) FROM sets WHERE exercise_entry_id = ?1", params![entry_id], |row| row.get(0))?;
        Ok(count)
    }

    /// Bulk-close every open exercise_entry in `session_id`. When `end_timestamp`
    /// is None, uses `datetime('now')`. Returns the number of rows updated.
    pub fn close_open_entries_for_session(&self, session_id: i64, end_timestamp: Option<&str>) -> anyhow::Result<usize> {
        let rows = match end_timestamp {
            Some(ts) => self.conn().execute(
                "UPDATE exercise_entry SET end_timestamp = ?2 \
                 WHERE session_id = ?1 AND end_timestamp IS NULL",
                params![session_id, ts],
            )?,
            None => self.conn().execute(
                "UPDATE exercise_entry SET end_timestamp = datetime('now') \
                 WHERE session_id = ?1 AND end_timestamp IS NULL",
                params![session_id],
            )?,
        };
        Ok(rows)
    }

    pub fn delete_entry(&self, entry_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM exercise_entry WHERE id = ?1", params![entry_id])?;
        anyhow::ensure!(rows > 0, "exercise_entry id {entry_id} not found");
        Ok(())
    }
}

// ── Sets ───────────────────────────────────────────────────────────────────────

fn row_to_set(row: &Row) -> rusqlite::Result<ExerciseSet> {
    Ok(ExerciseSet {
        id: row.get(0)?,
        exercise_entry_id: row.get(1)?,
        exercise_type_id: row.get(2)?,
        order_idx: row.get(3)?,
        measurement_type: MeasurementType::from_id(row.get::<_, i64>(4)?),
        count: row.get(5)?,
        value: row.get(6)?,
        perceived_difficulty: Difficulty::from_str_loose(&row.get::<_, String>(7)?),
        comment: row.get(8)?,
        logged_at: row.get(9)?,
    })
}

const SELECT_SET: &str = "\
    SELECT id, exercise_entry_id, exercise_type_id, order_idx, \
           measurement_type_id, count, value, perceived_difficulty, comment, logged_at \
    FROM sets";

impl Database {
    pub fn insert_set(&self, set: &ExerciseSet) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO sets (exercise_entry_id, exercise_type_id, order_idx, measurement_type_id, \
                               count, value, perceived_difficulty, comment, logged_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE(?9, datetime('now')))",
            params![
                set.exercise_entry_id,
                set.exercise_type_id,
                set.order_idx,
                set.measurement_type.id(),
                set.count,
                set.value,
                set.perceived_difficulty.as_str(),
                set.comment,
                if set.logged_at.is_empty() { None } else { Some(&set.logged_at) },
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn update_set(&self, set: &ExerciseSet) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE sets SET exercise_entry_id = ?1, exercise_type_id = ?2, order_idx = ?3, \
                              measurement_type_id = ?4, count = ?5, value = ?6, \
                              perceived_difficulty = ?7, comment = ?8, logged_at = ?9 \
             WHERE id = ?10",
            params![
                set.exercise_entry_id,
                set.exercise_type_id,
                set.order_idx,
                set.measurement_type.id(),
                set.count,
                set.value,
                set.perceived_difficulty.as_str(),
                set.comment,
                set.logged_at,
                set.id,
            ],
        )?;
        anyhow::ensure!(rows > 0, "set id {} not found", set.id);
        Ok(())
    }

    pub fn delete_set(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM sets WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "set id {id} not found");
        Ok(())
    }

    pub fn get_set(&self, id: i64) -> anyhow::Result<Option<ExerciseSet>> {
        let sql = format!("{SELECT_SET} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_set)?;
        rows.next().transpose().context("Failed to read set row")
    }

    pub fn list_sets_for_entry(&self, entry_id: i64) -> anyhow::Result<Vec<ExerciseSet>> {
        let sql = format!("{SELECT_SET} WHERE exercise_entry_id = ?1 ORDER BY order_idx, id");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![entry_id], row_to_set)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list sets for entry")
    }

    pub fn list_sets_for_user(&self, user_id: i64, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Vec<ExerciseSet>> {
        let mut stmt = self.conn().prepare(
            "SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 \
               AND (?2 IS NULL OR s.logged_at >= ?2) \
               AND (?3 IS NULL OR s.logged_at <= ?3) \
             ORDER BY s.logged_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id, from, to], row_to_set)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list sets for user")
    }

    /// Sets logged against `exercise_type_id`. When `include_descendants` is true,
    /// the query walks the taxonomy and matches any descendant of `exercise_type_id`.
    pub fn list_sets_for_exercise_type(
        &self,
        user_id: i64,
        exercise_type_id: i64,
        limit: usize,
        include_descendants: bool,
    ) -> anyhow::Result<Vec<ExerciseSet>> {
        let sql = if include_descendants {
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM exercise_types WHERE id = ?1 \
                 UNION ALL \
                 SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
             ) \
             SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?2 AND s.exercise_type_id IN (SELECT id FROM tree) \
             ORDER BY s.logged_at DESC LIMIT ?3"
        } else {
            "SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?2 AND s.exercise_type_id = ?1 \
             ORDER BY s.logged_at DESC LIMIT ?3"
        };
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map(params![exercise_type_id, user_id, limit as i64], row_to_set)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list sets for exercise_type")
    }

    pub fn list_recent_sets(&self, user_id: i64, days: i32) -> anyhow::Result<Vec<ExerciseSet>> {
        let cutoff = format!("-{days} days");
        let mut stmt = self.conn().prepare(
            "SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 AND s.logged_at >= datetime('now', ?2) \
             ORDER BY s.logged_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id, cutoff], row_to_set)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list recent sets")
    }

    /// Personal record (max value) for a given exercise_type, optionally rolling up
    /// descendants. Returns the best `ExerciseSet`.
    pub fn personal_record(&self, user_id: i64, exercise_type_id: i64, include_descendants: bool) -> anyhow::Result<Option<ExerciseSet>> {
        let sql = if include_descendants {
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM exercise_types WHERE id = ?1 \
                 UNION ALL \
                 SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
             ) \
             SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?2 AND s.exercise_type_id IN (SELECT id FROM tree) \
             ORDER BY s.value DESC LIMIT 1"
        } else {
            "SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?2 AND s.exercise_type_id = ?1 \
             ORDER BY s.value DESC LIMIT 1"
        };
        let mut stmt = self.conn().prepare(sql)?;
        let mut rows = stmt.query_map(params![exercise_type_id, user_id], row_to_set)?;
        rows.next().transpose().context("Failed to read PR row")
    }
}

// ── Set editing ─────────────────────────────────────────────────────────────────

/// A partial edit to an existing set. Every field is optional; `None` means
/// "leave unchanged". `exercise_type_id` holds a *resolved* id — name→id
/// resolution happens in the caller (web layer or assistant catalogue lookup).
/// `count` and `comment` are doubly-optional: the outer `None` means unchanged,
/// the inner `None` clears the column.
#[derive(Debug, Default, Clone)]
pub struct SetEdit {
    pub exercise_type_id: Option<i64>,
    pub count: Option<Option<i32>>,
    pub value: Option<f64>,
    pub perceived_difficulty: Option<Difficulty>,
    pub comment: Option<Option<String>>,
}

impl SetEdit {
    pub fn is_empty(&self) -> bool {
        self.exercise_type_id.is_none()
            && self.count.is_none()
            && self.value.is_none()
            && self.perceived_difficulty.is_none()
            && self.comment.is_none()
    }
}

/// The set as it was before the edit and as it is after — returned so callers
/// can build a human-readable before→after confirmation.
#[derive(Debug, Clone)]
pub struct SetEditOutcome {
    pub before: ExerciseSet,
    pub after: ExerciseSet,
}

/// Outcome of reclassifying a whole exercise_entry to a different exercise.
#[derive(Debug, Clone)]
pub struct EntryReclassifyOutcome {
    pub entry_id: i64,
    pub old_exercise_type_id: i64,
    pub new_exercise_type_id: i64,
    pub sets_updated: usize,
}

/// Failure modes of an edit. `NotFound`/`Forbidden` are deliberately distinct
/// so callers can decide whether to collapse them (the REST layer does, to
/// avoid leaking set-id existence).
#[derive(Debug)]
pub enum SetEditError {
    NotFound(i64),
    Forbidden(i64),
    MeasurementTypeMismatch { from: String, to: String },
    Empty,
    Db(anyhow::Error),
}

impl std::fmt::Display for SetEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "set {id} not found"),
            Self::Forbidden(id) => write!(f, "set {id} does not belong to the requesting user"),
            Self::MeasurementTypeMismatch { from, to } => write!(
                f,
                "{from} and {to} are not measured the same way — supply the new value when changing the exercise"
            ),
            Self::Empty => write!(f, "no changes were supplied"),
            Self::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SetEditError {}

impl Database {
    /// Owning user_id of a set (via `exercise_entry.user_id`), or `None` if the
    /// set does not exist.
    pub fn set_owner(&self, set_id: i64) -> anyhow::Result<Option<i64>> {
        let mut stmt = self.conn().prepare(
            "SELECT ee.user_id FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE s.id = ?1",
        )?;
        let mut rows = stmt.query_map(params![set_id], |row| row.get::<_, i64>(0))?;
        rows.next().transpose().context("Failed to read set owner")
    }

    /// Most recently logged set for `user_id`, optionally restricted to a single
    /// exercise type. The `id DESC` tie-breaker is required: sets logged in one
    /// batch can share an identical `logged_at` string.
    pub fn most_recent_set_for_user(&self, user_id: i64, exercise_type_id: Option<i64>) -> anyhow::Result<Option<ExerciseSet>> {
        let mut stmt = self.conn().prepare(
            "SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                    s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 AND (?2 IS NULL OR s.exercise_type_id = ?2) \
             ORDER BY s.logged_at DESC, s.id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![user_id, exercise_type_id], row_to_set)?;
        rows.next().transpose().context("Failed to read most recent set")
    }

    /// Re-point every set in an entry at a different exercise type and
    /// measurement type. Returns the number of rows updated.
    pub fn set_exercise_type_for_entry(&self, entry_id: i64, exercise_type_id: i64, mt: MeasurementType) -> anyhow::Result<usize> {
        let rows = self.conn().execute(
            "UPDATE sets SET exercise_type_id = ?2, measurement_type_id = ?3 WHERE exercise_entry_id = ?1",
            params![entry_id, exercise_type_id, mt.id()],
        )?;
        Ok(rows)
    }

    /// Apply a partial edit to a single set. Verifies `requesting_user_id` owns
    /// the set (own set, or `can_write` over the owner) and enforces that a
    /// change of exercise type does not silently change the measurement type
    /// unless a fresh `value` is supplied alongside.
    pub fn edit_set(
        &self,
        set_id: i64,
        requesting_user_id: i64,
        catalogue: &[ExerciseTypeWithAncestry],
        edit: &SetEdit,
    ) -> Result<SetEditOutcome, SetEditError> {
        if edit.is_empty() {
            return Err(SetEditError::Empty);
        }
        let before = self.get_set(set_id).map_err(SetEditError::Db)?.ok_or(SetEditError::NotFound(set_id))?;
        let owner = self.set_owner(set_id).map_err(SetEditError::Db)?.ok_or(SetEditError::NotFound(set_id))?;
        if owner != requesting_user_id && !self.can_write(requesting_user_id, owner).map_err(SetEditError::Db)? {
            return Err(SetEditError::Forbidden(set_id));
        }

        let mut after = before.clone();
        if let Some(new_type_id) = edit.exercise_type_id
            && new_type_id != before.exercise_type_id
        {
            let new_type = catalogue
                .iter()
                .find(|e| e.exercise_type.id == new_type_id)
                .ok_or_else(|| SetEditError::Db(anyhow::anyhow!("exercise type {new_type_id} not found")))?;
            let new_mt = new_type
                .exercise_type
                .measurement_type
                .ok_or_else(|| SetEditError::Db(anyhow::anyhow!("exercise type {new_type_id} has no measurement type")))?;
            if new_mt != before.measurement_type && edit.value.is_none() {
                let from = exercise_name(catalogue, before.exercise_type_id);
                return Err(SetEditError::MeasurementTypeMismatch { from, to: new_type.exercise_type.name.clone() });
            }
            after.exercise_type_id = new_type_id;
            after.measurement_type = new_mt;
            // A non-weight_reps measurement type has no rep count; drop a stale
            // one unless the caller explicitly sets a new count.
            if new_mt != MeasurementType::WeightReps {
                after.count = None;
            }
        }
        if let Some(c) = edit.count {
            after.count = c;
        }
        if let Some(v) = edit.value {
            after.value = v;
        }
        if let Some(d) = edit.perceived_difficulty {
            after.perceived_difficulty = d;
        }
        if let Some(c) = &edit.comment {
            after.comment = c.clone();
        }

        self.update_set(&after).map_err(SetEditError::Db)?;
        Ok(SetEditOutcome { before, after })
    }

    /// Reclassify a whole exercise_entry (block of sets) to a different
    /// exercise. Verifies ownership and rejects a change that would alter the
    /// measurement type, since a bulk change cannot supply per-set values.
    pub fn reclassify_entry_exercise(
        &self,
        entry_id: i64,
        requesting_user_id: i64,
        catalogue: &[ExerciseTypeWithAncestry],
        new_exercise_type_id: i64,
    ) -> Result<EntryReclassifyOutcome, SetEditError> {
        let entry = self.get_entry(entry_id).map_err(SetEditError::Db)?.ok_or(SetEditError::NotFound(entry_id))?;
        if entry.user_id != requesting_user_id && !self.can_write(requesting_user_id, entry.user_id).map_err(SetEditError::Db)? {
            return Err(SetEditError::Forbidden(entry_id));
        }
        let sets = self.list_sets_for_entry(entry_id).map_err(SetEditError::Db)?;
        let first = sets.first().ok_or(SetEditError::NotFound(entry_id))?;
        let current_mt = first.measurement_type;
        if sets.iter().any(|s| s.measurement_type != current_mt) {
            return Err(SetEditError::Db(anyhow::anyhow!("entry {entry_id} has mixed measurement types")));
        }

        let new_type = catalogue
            .iter()
            .find(|e| e.exercise_type.id == new_exercise_type_id)
            .ok_or_else(|| SetEditError::Db(anyhow::anyhow!("exercise type {new_exercise_type_id} not found")))?;
        let new_mt = new_type
            .exercise_type
            .measurement_type
            .ok_or_else(|| SetEditError::Db(anyhow::anyhow!("exercise type {new_exercise_type_id} has no measurement type")))?;
        if new_mt != current_mt {
            return Err(SetEditError::MeasurementTypeMismatch {
                from: exercise_name(catalogue, first.exercise_type_id),
                to: new_type.exercise_type.name.clone(),
            });
        }

        let old_exercise_type_id = first.exercise_type_id;
        let sets_updated = self
            .set_exercise_type_for_entry(entry_id, new_exercise_type_id, new_mt)
            .map_err(SetEditError::Db)?;
        Ok(EntryReclassifyOutcome { entry_id, old_exercise_type_id, new_exercise_type_id, sets_updated })
    }
}

/// Catalogue name of an exercise type id, falling back to a generic label.
fn exercise_name(catalogue: &[ExerciseTypeWithAncestry], exercise_type_id: i64) -> String {
    catalogue
        .iter()
        .find(|e| e.exercise_type.id == exercise_type_id)
        .map(|e| e.exercise_type.name.clone())
        .unwrap_or_else(|| "the previous exercise".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{new_exercise_entry, new_exercise_set, new_user};

    fn fixture() -> (Database, i64, i64) {
        let db = Database::open_in_memory().unwrap();
        let user = new_user("Tester", None, "UTC");
        let user_id = db.insert_user(&user).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        (db, user_id, bp.id)
    }

    #[test]
    fn start_session_returns_session_with_id() {
        let (db, user_id, _) = fixture();
        let s = db.start_session(user_id, Some("warm-up")).unwrap();
        assert!(s.id > 0);
        assert_eq!(s.user_id, user_id);
        assert!(s.ended_at.is_none());
    }

    #[test]
    fn entry_then_sets_then_pr() {
        let (db, user_id, bp_id) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        let entry = new_exercise_entry(user_id, Some(session.id), Some("morning"));
        let entry_id = db.insert_entry(&entry).unwrap();

        for w in [60.0, 80.0, 70.0] {
            let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, w);
            s.count = Some(5);
            db.insert_set(&s).unwrap();
        }

        let sets = db.list_sets_for_entry(entry_id).unwrap();
        assert_eq!(sets.len(), 3);

        let pr = db.personal_record(user_id, bp_id, false).unwrap().unwrap();
        assert!((pr.value - 80.0).abs() < 1e-6);
    }

    #[test]
    fn personal_record_with_descendants_rolls_up() {
        let (db, user_id, bp_id) = fixture();
        let flat = db.get_exercise_type_by_name("Flat Barbell Bench Press").unwrap().unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, flat.id, MeasurementType::WeightReps, 100.0);
        s.count = Some(5);
        db.insert_set(&s).unwrap();

        // Querying Bench Press (parent) without descendants → no PR.
        assert!(db.personal_record(user_id, bp_id, false).unwrap().is_none());
        // With descendants → finds the variation's set.
        let pr = db.personal_record(user_id, bp_id, true).unwrap().unwrap();
        assert!((pr.value - 100.0).abs() < 1e-6);
    }

    #[test]
    fn end_entry_sets_end_timestamp() {
        let (db, user_id, _) = fixture();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        db.end_entry(entry_id).unwrap();
        let entry = db.get_entry(entry_id).unwrap().unwrap();
        assert!(entry.end_timestamp.is_some());
    }

    #[test]
    fn find_open_entry_for_exercise_matches_only_same_type() {
        let (db, user_id, bp_id) = fixture();
        let squat_id = db.get_exercise_type_by_name("Squat").unwrap().unwrap().id;
        let session = db.start_session(user_id, None).unwrap();

        let bench_entry = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let mut s = new_exercise_set(bench_entry, bp_id, MeasurementType::WeightReps, 80.0);
        s.count = Some(8);
        db.insert_set(&s).unwrap();

        let squat_entry = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let mut s = new_exercise_set(squat_entry, squat_id, MeasurementType::WeightReps, 100.0);
        s.count = Some(5);
        db.insert_set(&s).unwrap();

        let found = db.find_open_entry_for_exercise(user_id, session.id, bp_id).unwrap().unwrap();
        assert_eq!(found.id, bench_entry);
        let found = db.find_open_entry_for_exercise(user_id, session.id, squat_id).unwrap().unwrap();
        assert_eq!(found.id, squat_entry);
    }

    #[test]
    fn find_open_entry_for_exercise_ignores_closed_entries() {
        let (db, user_id, bp_id) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, 80.0);
        s.count = Some(8);
        db.insert_set(&s).unwrap();
        db.end_entry(entry_id).unwrap();

        assert!(db.find_open_entry_for_exercise(user_id, session.id, bp_id).unwrap().is_none());
    }

    /// Open an entry in `session_id` and log one set of `exercise_type_id` into it.
    fn open_entry_with_set(db: &Database, user_id: i64, session_id: i64, exercise_type_id: i64) -> i64 {
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, Some(session_id), None)).unwrap();
        let mut s = new_exercise_set(entry_id, exercise_type_id, MeasurementType::WeightReps, 80.0);
        s.count = Some(8);
        db.insert_set(&s).unwrap();
        entry_id
    }

    #[test]
    fn find_open_related_entry_matches_descendant() {
        let (db, user_id, bp_id) = fixture();
        let flat_id = db.get_exercise_type_by_name("Flat Barbell Bench Press").unwrap().unwrap().id;
        let session = db.start_session(user_id, None).unwrap();
        let entry_id = open_entry_with_set(&db, user_id, session.id, flat_id);

        // Querying the ancestor (Bench Press) finds the open descendant-variation entry.
        let (found, matched_type) = db.find_open_related_entry(session.id, bp_id).unwrap().unwrap();
        assert_eq!(found.id, entry_id);
        assert_eq!(matched_type, flat_id);
    }

    #[test]
    fn find_open_related_entry_matches_ancestor() {
        let (db, user_id, bp_id) = fixture();
        let flat_id = db.get_exercise_type_by_name("Flat Barbell Bench Press").unwrap().unwrap().id;
        let session = db.start_session(user_id, None).unwrap();
        let entry_id = open_entry_with_set(&db, user_id, session.id, bp_id);

        // Querying the descendant (Flat Barbell Bench Press) finds the open ancestor entry.
        let (found, matched_type) = db.find_open_related_entry(session.id, flat_id).unwrap().unwrap();
        assert_eq!(found.id, entry_id);
        assert_eq!(matched_type, bp_id);
    }

    #[test]
    fn find_open_related_entry_ignores_exact_type() {
        let (db, user_id, bp_id) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        open_entry_with_set(&db, user_id, session.id, bp_id);

        // An exact-type open entry is the exact-match path's job, not this one.
        assert!(db.find_open_related_entry(session.id, bp_id).unwrap().is_none());
    }

    #[test]
    fn find_open_related_entry_ignores_unrelated() {
        let (db, user_id, bp_id) = fixture();
        let squat_id = db.get_exercise_type_by_name("Squat").unwrap().unwrap().id;
        let session = db.start_session(user_id, None).unwrap();
        open_entry_with_set(&db, user_id, session.id, squat_id);

        // Squat is a different taxonomy branch — a genuine superset, not ambiguous.
        assert!(db.find_open_related_entry(session.id, bp_id).unwrap().is_none());
    }

    #[test]
    fn find_open_related_entry_ignores_closed_entries() {
        let (db, user_id, bp_id) = fixture();
        let flat_id = db.get_exercise_type_by_name("Flat Barbell Bench Press").unwrap().unwrap().id;
        let session = db.start_session(user_id, None).unwrap();
        let entry_id = open_entry_with_set(&db, user_id, session.id, flat_id);
        db.end_entry(entry_id).unwrap();

        assert!(db.find_open_related_entry(session.id, bp_id).unwrap().is_none());
    }

    #[test]
    fn count_sets_for_entry_basic() {
        let (db, user_id, bp_id) = fixture();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        assert_eq!(db.count_sets_for_entry(entry_id).unwrap(), 0);
        for _ in 0..3 {
            let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, 80.0);
            s.count = Some(8);
            db.insert_set(&s).unwrap();
        }
        assert_eq!(db.count_sets_for_entry(entry_id).unwrap(), 3);
    }

    #[test]
    fn list_open_entries_for_session_returns_concurrent_supersets() {
        let (db, user_id, bp_id) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        let id1 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let id2 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let id3 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        // close the middle one
        let mut s = new_exercise_set(id2, bp_id, MeasurementType::WeightReps, 80.0);
        s.count = Some(8);
        db.insert_set(&s).unwrap();
        db.end_entry(id2).unwrap();

        let open = db.list_open_entries_for_session(session.id).unwrap();
        let ids: Vec<i64> = open.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![id1, id3]);
    }

    #[test]
    fn end_session_cascades_close_open_entries() {
        let (db, user_id, _) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        let id1 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let id2 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        db.end_session(session.id).unwrap();
        assert!(db.get_entry(id1).unwrap().unwrap().end_timestamp.is_some());
        assert!(db.get_entry(id2).unwrap().unwrap().end_timestamp.is_some());
        let session = db.get_session(session.id).unwrap().unwrap();
        assert!(session.ended_at.is_some());
    }

    #[test]
    fn close_open_entries_for_session_bulk_closes() {
        let (db, user_id, _) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        let id1 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let _id2 = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let n = db.close_open_entries_for_session(session.id, Some("2025-01-01 00:00:00")).unwrap();
        assert_eq!(n, 2);
        assert_eq!(db.get_entry(id1).unwrap().unwrap().end_timestamp.as_deref(), Some("2025-01-01 00:00:00"));
    }

    #[test]
    fn list_open_entries_for_user_spans_sessions() {
        let (db, user_id, _) = fixture();
        let s1 = db.start_session(user_id, None).unwrap();
        let id_a = db.insert_entry(&new_exercise_entry(user_id, Some(s1.id), None)).unwrap();
        // simulate a session that was ended without cascading entry-close (legacy path)
        db.conn().execute("UPDATE sessions SET ended_at = datetime('now') WHERE id = ?1", params![s1.id]).unwrap();
        let s2 = db.start_session(user_id, None).unwrap();
        let id_b = db.insert_entry(&new_exercise_entry(user_id, Some(s2.id), None)).unwrap();

        let open = db.list_open_entries_for_user(user_id).unwrap();
        let ids: Vec<i64> = open.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![id_a, id_b]);
    }

    // ── Set editing ────────────────────────────────────────────────────────────

    fn catalogue(db: &Database) -> Vec<ExerciseTypeWithAncestry> {
        db.list_exercise_types_with_ancestry().unwrap()
    }

    /// Insert a weight_reps set into a fresh standalone entry, returning the set id.
    fn seed_set(db: &Database, user_id: i64, exercise_type_id: i64, weight: f64, reps: i32) -> i64 {
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, exercise_type_id, MeasurementType::WeightReps, weight);
        s.count = Some(reps);
        db.insert_set(&s).unwrap()
    }

    #[test]
    fn set_owner_returns_owning_user() {
        let (db, user_id, bp_id) = fixture();
        let set_id = seed_set(&db, user_id, bp_id, 60.0, 5);
        assert_eq!(db.set_owner(set_id).unwrap(), Some(user_id));
        assert_eq!(db.set_owner(999_999).unwrap(), None);
    }

    #[test]
    fn most_recent_set_orders_by_logged_at_then_id() {
        let (db, user_id, bp_id) = fixture();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut last_id = 0;
        // Identical logged_at on every set forces the id tie-breaker to decide.
        for w in [60.0, 70.0, 80.0] {
            let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, w);
            s.count = Some(5);
            s.logged_at = "2025-01-01 10:00:00".to_string();
            last_id = db.insert_set(&s).unwrap();
        }
        let recent = db.most_recent_set_for_user(user_id, None).unwrap().unwrap();
        assert_eq!(recent.id, last_id);
        assert!((recent.value - 80.0).abs() < 1e-6);
    }

    #[test]
    fn most_recent_set_filtered_by_exercise_type() {
        let (db, user_id, bp_id) = fixture();
        let squat_id = db.get_exercise_type_by_name("Squat").unwrap().unwrap().id;
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut bench = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, 80.0);
        bench.count = Some(5);
        bench.logged_at = "2025-01-01 10:00:00".to_string();
        let bench_id = db.insert_set(&bench).unwrap();
        let mut squat = new_exercise_set(entry_id, squat_id, MeasurementType::WeightReps, 100.0);
        squat.count = Some(5);
        squat.logged_at = "2025-01-01 11:00:00".to_string();
        db.insert_set(&squat).unwrap();

        assert_eq!(db.most_recent_set_for_user(user_id, None).unwrap().unwrap().exercise_type_id, squat_id);
        assert_eq!(db.most_recent_set_for_user(user_id, Some(bp_id)).unwrap().unwrap().id, bench_id);
    }

    #[test]
    fn most_recent_set_only_returns_own_sets() {
        let (db, user_id, bp_id) = fixture();
        let other = db.insert_user(&new_user("Other", None, "UTC")).unwrap();
        seed_set(&db, other, bp_id, 50.0, 5);
        assert!(db.most_recent_set_for_user(user_id, None).unwrap().is_none());
    }

    #[test]
    fn edit_set_changes_value_only() {
        let (db, user_id, bp_id) = fixture();
        let set_id = seed_set(&db, user_id, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        let edit = SetEdit { value: Some(40.0), ..Default::default() };
        let outcome = db.edit_set(set_id, user_id, &cat, &edit).unwrap();
        assert!((outcome.after.value - 40.0).abs() < 1e-6);
        assert_eq!(outcome.after.count, Some(8));
        assert_eq!(outcome.after.exercise_type_id, bp_id);
        let stored = db.get_set(set_id).unwrap().unwrap();
        assert!((stored.value - 40.0).abs() < 1e-6);
    }

    #[test]
    fn edit_set_changes_exercise_same_measurement_type() {
        let (db, user_id, bp_id) = fixture();
        let squat_id = db.get_exercise_type_by_name("Squat").unwrap().unwrap().id;
        let set_id = seed_set(&db, user_id, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        let edit = SetEdit { exercise_type_id: Some(squat_id), ..Default::default() };
        let outcome = db.edit_set(set_id, user_id, &cat, &edit).unwrap();
        assert_eq!(outcome.after.exercise_type_id, squat_id);
        assert_eq!(outcome.after.measurement_type, MeasurementType::WeightReps);
        assert!((outcome.after.value - 30.0).abs() < 1e-6);
        assert_eq!(outcome.after.count, Some(8));
    }

    #[test]
    fn edit_set_rejects_cross_measurement_type_without_value() {
        let (db, user_id, bp_id) = fixture();
        let plank_id = db.get_exercise_type_by_name("Plank").unwrap().unwrap().id;
        let set_id = seed_set(&db, user_id, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        let edit = SetEdit { exercise_type_id: Some(plank_id), ..Default::default() };
        assert!(matches!(db.edit_set(set_id, user_id, &cat, &edit).unwrap_err(), SetEditError::MeasurementTypeMismatch { .. }));
    }

    #[test]
    fn edit_set_accepts_cross_measurement_type_with_value() {
        let (db, user_id, bp_id) = fixture();
        let plank_id = db.get_exercise_type_by_name("Plank").unwrap().unwrap().id;
        let set_id = seed_set(&db, user_id, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        let edit = SetEdit { exercise_type_id: Some(plank_id), value: Some(60.0), ..Default::default() };
        let outcome = db.edit_set(set_id, user_id, &cat, &edit).unwrap();
        assert_eq!(outcome.after.measurement_type, MeasurementType::TimeBased);
        assert!((outcome.after.value - 60.0).abs() < 1e-6);
        assert_eq!(outcome.after.count, None);
    }

    #[test]
    fn edit_set_rejects_other_users_set() {
        let (db, user_id, bp_id) = fixture();
        let other = db.insert_user(&new_user("Other", None, "UTC")).unwrap();
        let set_id = seed_set(&db, user_id, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        let edit = SetEdit { value: Some(40.0), ..Default::default() };
        assert!(matches!(db.edit_set(set_id, other, &cat, &edit).unwrap_err(), SetEditError::Forbidden(_)));
    }

    #[test]
    fn edit_set_allows_group_write_member() {
        use crate::db::{AccessLevel, Group};
        let (db, owner, bp_id) = fixture();
        let editor = db.insert_user(&new_user("Editor", None, "UTC")).unwrap();
        let group = db
            .insert_group(&Group { id: 0, name: "Gym".into(), description: None, created_at: "2025-01-01 00:00:00".into() })
            .unwrap();
        db.add_member(editor, group, AccessLevel::Write).unwrap();
        db.add_member(owner, group, AccessLevel::Read).unwrap();
        let set_id = seed_set(&db, owner, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        let edit = SetEdit { value: Some(45.0), ..Default::default() };
        let outcome = db.edit_set(set_id, editor, &cat, &edit).unwrap();
        assert!((outcome.after.value - 45.0).abs() < 1e-6);
    }

    #[test]
    fn edit_set_missing_set_returns_not_found() {
        let (db, user_id, _) = fixture();
        let cat = catalogue(&db);
        let edit = SetEdit { value: Some(40.0), ..Default::default() };
        assert!(matches!(db.edit_set(999_999, user_id, &cat, &edit).unwrap_err(), SetEditError::NotFound(_)));
    }

    #[test]
    fn edit_set_empty_edit_rejected() {
        let (db, user_id, bp_id) = fixture();
        let set_id = seed_set(&db, user_id, bp_id, 30.0, 8);
        let cat = catalogue(&db);
        assert!(matches!(db.edit_set(set_id, user_id, &cat, &SetEdit::default()).unwrap_err(), SetEditError::Empty));
    }

    #[test]
    fn reclassify_entry_exercise_updates_every_set() {
        let (db, user_id, bp_id) = fixture();
        let squat_id = db.get_exercise_type_by_name("Squat").unwrap().unwrap().id;
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        for w in [60.0, 70.0, 80.0] {
            let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, w);
            s.count = Some(5);
            db.insert_set(&s).unwrap();
        }
        let cat = catalogue(&db);
        let outcome = db.reclassify_entry_exercise(entry_id, user_id, &cat, squat_id).unwrap();
        assert_eq!(outcome.sets_updated, 3);
        assert_eq!(outcome.old_exercise_type_id, bp_id);
        assert!(db.list_sets_for_entry(entry_id).unwrap().iter().all(|s| s.exercise_type_id == squat_id));
    }

    #[test]
    fn reclassify_entry_exercise_rejects_measurement_type_change() {
        let (db, user_id, bp_id) = fixture();
        let plank_id = db.get_exercise_type_by_name("Plank").unwrap().unwrap().id;
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, 60.0);
        s.count = Some(5);
        db.insert_set(&s).unwrap();
        let cat = catalogue(&db);
        let err = db.reclassify_entry_exercise(entry_id, user_id, &cat, plank_id).unwrap_err();
        assert!(matches!(err, SetEditError::MeasurementTypeMismatch { .. }));
    }

    #[test]
    fn reclassify_entry_exercise_rejects_other_users_entry() {
        let (db, user_id, bp_id) = fixture();
        let squat_id = db.get_exercise_type_by_name("Squat").unwrap().unwrap().id;
        let other = db.insert_user(&new_user("Other", None, "UTC")).unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, 60.0);
        s.count = Some(5);
        db.insert_set(&s).unwrap();
        let cat = catalogue(&db);
        assert!(matches!(db.reclassify_entry_exercise(entry_id, other, &cat, squat_id).unwrap_err(), SetEditError::Forbidden(_)));
    }
}

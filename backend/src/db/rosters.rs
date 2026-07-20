//! Persistence for session rosters: the set of exercises designed for one
//! session, its prescribed exercises, and the prescribed-vs-actual comparison
//! against the session it was bound to.
//!
//! **This module is the only writer of `session_rosters`.** Everything that
//! changes a roster row goes through here — including
//! [`Database::bind_roster_to_slot`], which the programmes DAO used to write
//! directly. Programmes own their own tables and reach rosters through these
//! methods, so "what can change a roster's status?" has one answer.
//!
//! Designing or storing a roster never logs a set — that stays on the
//! sessions/exercise_entries/sets path in [`super::entries`].

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{
    ExerciseDelta, ExerciseSet, LifecycleStatus, MeasurementType, PerformedRollup, RosterExercise, RosterVsActual, SessionRoster,
    SkippedExercise, SlotStatus, UnplannedExercise,
};

/// A session's logged sets grouped by `exercise_type_id`, for rolling performance
/// up per exercise to diff against a roster's prescription.
type PerformedSets = HashMap<i64, Vec<ExerciseSet>>;

fn row_to_roster(row: &rusqlite::Row) -> rusqlite::Result<SessionRoster> {
    Ok(SessionRoster {
        id: row.get(0)?,
        user_id: row.get(1)?,
        title: row.get(2)?,
        rationale: row.get(3)?,
        philosophy_id: row.get(4)?,
        status: LifecycleStatus::from_str_loose(&row.get::<_, String>(5)?),
        session_id: row.get(6)?,
        override_note: row.get(7)?,
        programme_slot_id: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_roster_exercise(row: &rusqlite::Row) -> rusqlite::Result<RosterExercise> {
    Ok(RosterExercise {
        id: row.get(0)?,
        roster_id: row.get(1)?,
        exercise_type_id: row.get(2)?,
        order_idx: row.get(3)?,
        target_sets: row.get(4)?,
        target_reps: row.get(5)?,
        target_weight_kg: row.get(6)?,
        target_secs: row.get(7)?,
        notes: row.get(8)?,
    })
}

const SELECT_ROSTER: &str = "\
    SELECT id, user_id, title, rationale, philosophy_id, status, session_id, override_note, programme_slot_id, created_at, updated_at \
    FROM session_rosters";

const SELECT_ROSTER_EXERCISE: &str = "\
    SELECT id, roster_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes \
    FROM roster_exercises";

impl Database {
    // ── Rosters ───────────────────────────────────────────────────────────────────

    pub fn create_roster(&self, user_id: i64, title: &str, rationale: Option<&str>, philosophy_id: Option<i64>) -> anyhow::Result<i64> {
        // A user keeps at most one live design: supersede any earlier draft rosters
        // so they neither accumulate nor bind to a later session.
        self.conn().execute(
            "UPDATE session_rosters SET status = ?1, updated_at = datetime('now') WHERE user_id = ?2 AND status = ?3",
            params![LifecycleStatus::Abandoned.as_str(), user_id, LifecycleStatus::Draft.as_str()],
        )?;
        self.conn().execute(
            "INSERT INTO session_rosters (user_id, title, rationale, philosophy_id) VALUES (?1, ?2, ?3, ?4)",
            params![user_id, title, rationale, philosophy_id],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn add_roster_exercise(&self, e: &RosterExercise) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO roster_exercises \
                 (roster_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.roster_id,
                e.exercise_type_id,
                e.order_idx,
                e.target_sets,
                e.target_reps,
                e.target_weight_kg,
                e.target_secs,
                e.notes
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_roster(&self, roster_id: i64) -> anyhow::Result<Option<SessionRoster>> {
        let sql = format!("{SELECT_ROSTER} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![roster_id], row_to_roster)?;
        rows.next().transpose().context("Failed to read roster row")
    }

    pub fn list_roster_exercises(&self, roster_id: i64) -> anyhow::Result<Vec<RosterExercise>> {
        let sql = format!("{SELECT_ROSTER_EXERCISE} WHERE roster_id = ?1 ORDER BY order_idx");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![roster_id], row_to_roster_exercise)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list roster exercises")
    }

    /// The most recent roster still awaiting execution (used to activate after `/nextworkout`).
    pub fn latest_draft_roster(&self, user_id: i64) -> anyhow::Result<Option<SessionRoster>> {
        let sql = format!("{SELECT_ROSTER} WHERE user_id = ?1 AND status = 'draft' ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_roster)?;
        rows.next().transpose().context("Failed to read draft roster")
    }

    /// The user's currently active (in-progress) roster, if any.
    pub fn active_roster_for_user(&self, user_id: i64) -> anyhow::Result<Option<SessionRoster>> {
        let sql = format!("{SELECT_ROSTER} WHERE user_id = ?1 AND status = 'active' ORDER BY updated_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_roster)?;
        rows.next().transpose().context("Failed to read active roster")
    }

    pub fn set_roster_status(&self, roster_id: i64, status: LifecycleStatus) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE session_rosters SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), roster_id],
        )?;
        anyhow::ensure!(rows > 0, "Session roster with id {roster_id} not found");
        Ok(())
    }

    /// Bind a draft roster to a session and mark it active for guided execution.
    pub fn bind_roster_to_session(&self, roster_id: i64, session_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE session_rosters SET session_id = ?1, status = 'active', updated_at = datetime('now') WHERE id = ?2",
            params![session_id, roster_id],
        )?;
        anyhow::ensure!(rows > 0, "Session roster with id {roster_id} not found");
        Ok(())
    }

    /// Stamp a designed roster with the programme slot it fills, and mark the slot
    /// filled. Ad-hoc rosters never call this — their `programme_slot_id` stays NULL.
    ///
    /// The roster half of the join is written here because `session_rosters` has one
    /// writer; the slot half delegates to [`Database::set_slot_status`], which owns
    /// `programme_slots`. Neither module reaches into the other's table.
    pub fn bind_roster_to_slot(&self, roster_id: i64, slot_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE session_rosters SET programme_slot_id = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![slot_id, roster_id],
        )?;
        anyhow::ensure!(rows > 0, "Session roster with id {roster_id} not found");
        self.set_slot_status(slot_id, SlotStatus::Filled)
    }

    /// The roster that filled `slot_id`, if one has.
    pub fn roster_for_slot(&self, slot_id: i64) -> anyhow::Result<Option<SessionRoster>> {
        let sql = format!("{SELECT_ROSTER} WHERE programme_slot_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![slot_id], row_to_roster)?;
        rows.next().transpose().context("Failed to read roster for slot")
    }

    /// The roster currently in flight for the user: the active (guided) roster if one
    /// is bound to a live session, otherwise the most recent draft that was never
    /// started. This is the target a mid-workout, today-only override attaches to.
    pub fn inflight_roster_for_user(&self, user_id: i64) -> anyhow::Result<Option<SessionRoster>> {
        match self.active_roster_for_user(user_id)? {
            Some(roster) => Ok(Some(roster)),
            None => self.latest_draft_roster(user_id),
        }
    }

    /// Append a today-only override (e.g. "no bench today, do flys instead") to a
    /// roster as a new `"- {note}"` bullet. Scoped to the roster row, so it expires
    /// when the roster completes or is superseded and NEVER reaches the philosophy.
    pub fn append_roster_override(&self, roster_id: i64, note: &str) -> anyhow::Result<()> {
        let existing: Option<String> = self.get_roster(roster_id)?.and_then(|r| r.override_note);
        let combined = match existing.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(base) => format!("{base}\n- {note}"),
            None => format!("- {note}"),
        };
        let rows = self.conn().execute(
            "UPDATE session_rosters SET override_note = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![combined, roster_id],
        )?;
        anyhow::ensure!(rows > 0, "Session roster with id {roster_id} not found");
        Ok(())
    }

    // ── Prescribed vs actual ──────────────────────────────────────────────────────

    /// Compare a roster's prescription against what its bound session actually
    /// performed. The roster carries the `session_id` binding (see
    /// [`bind_roster_to_session`](Self::bind_roster_to_session)); performed sets are
    /// rolled up per exercise_type and diffed against the roster's per-exercise
    /// targets. See [`RosterVsActual`] / [`ExerciseDelta`] for the signed-deviation
    /// semantics: deltas are `performed − prescribed`, and deviation is signal, not
    /// error. Errors if the roster is unknown or not yet bound to a session.
    pub fn roster_vs_actual(&self, roster_id: i64) -> anyhow::Result<RosterVsActual> {
        let roster = self.get_roster(roster_id)?.with_context(|| format!("Session roster {roster_id} not found"))?;
        let session_id = roster.session_id.with_context(|| format!("Session roster {roster_id} is not bound to a session"))?;

        let prescribed = self.list_roster_exercises(roster_id)?;
        let prescribed_ids: HashSet<i64> = prescribed.iter().map(|re| re.exercise_type_id).collect();
        let (performed, performed_order) = self.performed_by_exercise(session_id)?;

        let matched = prescribed
            .iter()
            .filter_map(|re| performed.get(&re.exercise_type_id).map(|sets| self.exercise_delta(re, sets)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let skipped = prescribed
            .iter()
            .filter(|re| !performed.contains_key(&re.exercise_type_id))
            .map(|re| self.skipped_exercise(re))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let unplanned = performed_order
            .iter()
            .filter(|type_id| !prescribed_ids.contains(type_id))
            .map(|&type_id| self.unplanned_exercise(type_id, &performed[&type_id]))
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(RosterVsActual { roster_id, session_id, matched, skipped, unplanned })
    }

    /// Every set logged in `session_id`, grouped by `exercise_type_id`, plus the
    /// exercise_type ids in the order they were first logged (so the unplanned list
    /// keeps a stable, session-chronological order).
    fn performed_by_exercise(&self, session_id: i64) -> anyhow::Result<(PerformedSets, Vec<i64>)> {
        let sets = self
            .list_entries_for_session(session_id)?
            .iter()
            .map(|entry| self.list_sets_for_entry(entry.id))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .flatten();

        let mut grouped: PerformedSets = HashMap::new();
        let mut order: Vec<i64> = Vec::new();
        sets.for_each(|set| {
            let type_id = set.exercise_type_id;
            if !grouped.contains_key(&type_id) {
                order.push(type_id);
            }
            grouped.entry(type_id).or_default().push(set);
        });
        Ok((grouped, order))
    }

    fn exercise_delta(&self, re: &RosterExercise, sets: &[ExerciseSet]) -> anyhow::Result<ExerciseDelta> {
        let measurement_type = sets.first().map(|s| s.measurement_type).unwrap_or(MeasurementType::WeightReps);
        let performed = rollup(sets, measurement_type);
        Ok(ExerciseDelta {
            exercise_name: self.exercise_name(re.exercise_type_id)?,
            measurement_type,
            sets_delta: re.target_sets.map(|t| performed.performed_sets - i64::from(t)),
            reps_delta: signed_delta(performed.avg_reps, re.target_reps.map(f64::from)),
            weight_delta_kg: signed_delta(performed.avg_weight_kg, re.target_weight_kg),
            secs_delta: signed_delta(performed.avg_secs, re.target_secs.map(f64::from)),
            prescribed: re.clone(),
            performed,
        })
    }

    fn skipped_exercise(&self, re: &RosterExercise) -> anyhow::Result<SkippedExercise> {
        Ok(SkippedExercise { exercise_name: self.exercise_name(re.exercise_type_id)?, prescribed: re.clone() })
    }

    fn unplanned_exercise(&self, exercise_type_id: i64, sets: &[ExerciseSet]) -> anyhow::Result<UnplannedExercise> {
        let measurement_type = sets.first().map(|s| s.measurement_type).unwrap_or(MeasurementType::WeightReps);
        Ok(UnplannedExercise {
            exercise_type_id,
            exercise_name: self.exercise_name(exercise_type_id)?,
            measurement_type,
            performed: rollup(sets, measurement_type),
        })
    }

    fn exercise_name(&self, exercise_type_id: i64) -> anyhow::Result<String> {
        Ok(self
            .get_exercise_type(exercise_type_id)?
            .map(|et| et.name)
            .unwrap_or_else(|| format!("exercise {exercise_type_id}")))
    }
}

/// Mean of the values, or `None` when there are none (so an absent dimension
/// reads as "unspecified", never a misleading `0.0`).
fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Signed `performed − prescribed`, defined only when both sides are present.
fn signed_delta(performed: Option<f64>, prescribed: Option<f64>) -> Option<f64> {
    performed.zip(prescribed).map(|(p, t)| p - t)
}

/// Roll a session's sets for one exercise up into a single performance to diff
/// against the roster's per-exercise prescription. `value` is interpreted per the
/// measurement type: weight for weight_reps, seconds for time_based; other types
/// have no weight/secs prescription to compare against, so both stay `None`.
fn rollup(sets: &[ExerciseSet], measurement_type: MeasurementType) -> PerformedRollup {
    let reps: Vec<f64> = sets.iter().filter_map(|s| s.count).map(f64::from).collect();
    let values: Vec<f64> = sets.iter().map(|s| s.value).collect();
    let avg_value = mean(&values);
    let (avg_weight_kg, avg_secs) = match measurement_type {
        MeasurementType::WeightReps => (avg_value, None),
        MeasurementType::TimeBased => (None, avg_value),
        _ => (None, None),
    };
    PerformedRollup { performed_sets: sets.len() as i64, avg_reps: mean(&reps), avg_weight_kg, avg_secs }
}

#[cfg(test)]
mod tests {
    use super::super::models::{MeasurementType, RosterExercise, new_exercise_entry, new_exercise_set, new_user};
    use super::*;

    fn test_db() -> (Database, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        (db, user_id)
    }

    #[test]
    fn roster_create_load_status_and_bind() {
        let (db, user_id) = test_db();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let roster_id = db.create_roster(user_id, "Push focus", Some("2 days rest, push the bench"), None).unwrap();
        db.add_roster_exercise(&RosterExercise {
            id: 0,
            roster_id,
            exercise_type_id: bp.id,
            order_idx: 0,
            target_sets: Some(3),
            target_reps: Some(6),
            target_weight_kg: Some(65.0),
            target_secs: None,
            notes: Some("last session was easy".into()),
        })
        .unwrap();

        let roster = db.get_roster(roster_id).unwrap().unwrap();
        assert_eq!(roster.status, LifecycleStatus::Draft);
        assert_eq!(roster.title, "Push focus");

        let exercises = db.list_roster_exercises(roster_id).unwrap();
        assert_eq!(exercises.len(), 1);
        assert_eq!(exercises[0].target_weight_kg, Some(65.0));

        assert_eq!(db.latest_draft_roster(user_id).unwrap().unwrap().id, roster_id);
        assert!(db.active_roster_for_user(user_id).unwrap().is_none());

        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(roster_id, session.id).unwrap();
        let active = db.active_roster_for_user(user_id).unwrap().unwrap();
        assert_eq!(active.id, roster_id);
        assert_eq!(active.session_id, Some(session.id));

        db.set_roster_status(roster_id, LifecycleStatus::Completed).unwrap();
        assert!(db.active_roster_for_user(user_id).unwrap().is_none());
    }

    /// A roster row written as v1's `proposed` still reads back as [`LifecycleStatus::Draft`].
    /// Nothing writes that value any more, but the importer replays dumps of legacy
    /// databases that do, so the loose parse must keep accepting it.
    #[test]
    fn a_legacy_proposed_status_reads_as_draft() {
        assert_eq!(LifecycleStatus::from_str_loose("proposed"), LifecycleStatus::Draft);
        assert_eq!(LifecycleStatus::from_str_loose("draft"), LifecycleStatus::Draft);
    }

    #[test]
    fn roster_override_appends_and_does_not_carry_to_next_roster() {
        let (db, user_id) = test_db();
        let first = db.create_roster(user_id, "Push", None, None).unwrap();

        assert!(db.get_roster(first).unwrap().unwrap().override_note.is_none());
        db.append_roster_override(first, "no bench today, do flys").unwrap();
        db.append_roster_override(first, "skip the last set").unwrap();
        let note = db.get_roster(first).unwrap().unwrap().override_note.unwrap();
        assert!(note.contains("- no bench today, do flys"));
        assert!(note.contains("- skip the last set"));

        // A fresh design starts clean — the one-off is scoped to its own roster.
        let second = db.create_roster(user_id, "Pull", None, None).unwrap();
        assert!(db.get_roster(second).unwrap().unwrap().override_note.is_none());
    }

    #[test]
    fn inflight_roster_prefers_active_then_latest_draft() {
        let (db, user_id) = test_db();
        assert!(db.inflight_roster_for_user(user_id).unwrap().is_none());

        let draft = db.create_roster(user_id, "Ready", None, None).unwrap();
        assert_eq!(db.inflight_roster_for_user(user_id).unwrap().unwrap().id, draft);

        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(draft, session.id).unwrap();
        assert_eq!(db.inflight_roster_for_user(user_id).unwrap().unwrap().id, draft, "active roster is the in-flight one");
    }

    #[test]
    fn designing_a_new_roster_abandons_the_previous_draft() {
        let (db, user_id) = test_db();
        let first = db.create_roster(user_id, "Roster A", None, None).unwrap();
        let second = db.create_roster(user_id, "Roster B", None, None).unwrap();

        assert_eq!(db.get_roster(first).unwrap().unwrap().status, LifecycleStatus::Abandoned);
        assert_eq!(db.get_roster(second).unwrap().unwrap().status, LifecycleStatus::Draft);
        // Only the newest draft is live and bindable.
        assert_eq!(db.latest_draft_roster(user_id).unwrap().unwrap().id, second);
    }

    /// Log `reps`×`weight` `n` times against `exercise_type_id` in one entry.
    fn log_sets(db: &Database, user_id: i64, session_id: i64, exercise_type_id: i64, n: usize, reps: i32, weight: f64) {
        let entry = db.insert_entry(&new_exercise_entry(user_id, Some(session_id), None)).unwrap();
        (0..n).for_each(|_| {
            let mut set = new_exercise_set(entry, exercise_type_id, MeasurementType::WeightReps, weight);
            set.count = Some(reps);
            db.insert_set(&set).unwrap();
        });
    }

    #[test]
    fn roster_vs_actual_matches_skips_and_flags_unplanned() {
        let (db, user_id) = test_db();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let squat = db.get_exercise_type_by_name("Squat").unwrap().unwrap();
        let deadlift = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap();

        let roster_id = db.create_roster(user_id, "Lower + press", None, None).unwrap();
        db.add_roster_exercise(&RosterExercise {
            id: 0,
            roster_id,
            exercise_type_id: bench.id,
            order_idx: 0,
            target_sets: Some(3),
            target_reps: Some(6),
            target_weight_kg: Some(65.0),
            target_secs: None,
            notes: None,
        })
        .unwrap();
        db.add_roster_exercise(&RosterExercise {
            id: 0,
            roster_id,
            exercise_type_id: squat.id,
            order_idx: 1,
            target_sets: Some(3),
            target_reps: Some(5),
            target_weight_kg: Some(100.0),
            target_secs: None,
            notes: None,
        })
        .unwrap();

        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(roster_id, session.id).unwrap();

        // Bench: 4 sets @ 6 reps @ 70kg — beat the prescription on sets and weight, met reps.
        log_sets(&db, user_id, session.id, bench.id, 4, 6, 70.0);
        // Squat: prescribed but never performed → skipped.
        // Deadlift: performed but never prescribed → unplanned.
        log_sets(&db, user_id, session.id, deadlift.id, 2, 5, 120.0);

        let cmp = db.roster_vs_actual(roster_id).unwrap();
        assert_eq!(cmp.roster_id, roster_id);
        assert_eq!(cmp.session_id, session.id);

        assert_eq!(cmp.matched.len(), 1);
        let bench_delta = &cmp.matched[0];
        assert_eq!(bench_delta.exercise_name, "Bench Press");
        assert_eq!(bench_delta.performed.performed_sets, 4);
        assert_eq!(bench_delta.sets_delta, Some(1)); // 4 − 3, exceeded
        assert_eq!(bench_delta.reps_delta, Some(0.0)); // 6 − 6, met
        assert_eq!(bench_delta.weight_delta_kg, Some(5.0)); // 70 − 65, exceeded

        assert_eq!(cmp.skipped.len(), 1);
        assert_eq!(cmp.skipped[0].exercise_name, "Squat");
        assert_eq!(cmp.skipped[0].prescribed.target_weight_kg, Some(100.0));

        assert_eq!(cmp.unplanned.len(), 1);
        assert_eq!(cmp.unplanned[0].exercise_name, "Deadlift");
        assert_eq!(cmp.unplanned[0].performed.performed_sets, 2);
        assert_eq!(cmp.unplanned[0].performed.avg_weight_kg, Some(120.0));
    }

    #[test]
    fn roster_vs_actual_signs_shortfalls_negative_and_requires_a_binding() {
        let (db, user_id) = test_db();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let roster_id = db.create_roster(user_id, "Press", None, None).unwrap();
        db.add_roster_exercise(&RosterExercise {
            id: 0,
            roster_id,
            exercise_type_id: bench.id,
            order_idx: 0,
            target_sets: Some(4),
            target_reps: Some(8),
            target_weight_kg: Some(80.0),
            target_secs: None,
            notes: None,
        })
        .unwrap();

        // A draft-but-unbound roster has no session to compare against.
        assert!(db.roster_vs_actual(roster_id).is_err());

        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(roster_id, session.id).unwrap();
        // 2 of 4 sets, lighter and fewer reps than prescribed — a shortfall on every dimension.
        log_sets(&db, user_id, session.id, bench.id, 2, 6, 72.5);

        let cmp = db.roster_vs_actual(roster_id).unwrap();
        assert!(cmp.skipped.is_empty());
        assert!(cmp.unplanned.is_empty());
        let delta = &cmp.matched[0];
        assert_eq!(delta.sets_delta, Some(-2)); // 2 − 4, missed
        assert_eq!(delta.reps_delta, Some(-2.0)); // 6 − 8, missed
        assert_eq!(delta.weight_delta_kg, Some(-7.5)); // 72.5 − 80, missed
    }
}

//! Persistence for the workout planner: the append-only training philosophy, the
//! per-(user, platform) interview state that builds it, and generated workout
//! plans. Designing or storing a plan never logs a set — that stays on the
//! sessions/exercise_entry/sets path.

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{
    ExerciseDelta, ExerciseEntry, ExerciseSet, InterviewState, MeasurementType, PerformedRollup, PlanStatus, PlanVsActual, Session,
    SkippedExercise, UnplannedExercise, WorkoutPlan, WorkoutPlanExercise, WorkoutPhilosophy,
};

/// An exercise entry paired with the sets logged into it.
pub type EntryWithSets = (ExerciseEntry, Vec<ExerciseSet>);

/// A session paired with its entries and their sets — how the `/nextworkout`
/// designer reads recent history.
pub type SessionWithSets = (Session, Vec<EntryWithSets>);

/// A session's logged sets grouped by `exercise_type_id`, for rolling performance
/// up per exercise to diff against a plan's prescription.
type PerformedSets = HashMap<i64, Vec<ExerciseSet>>;

fn row_to_philosophy(row: &rusqlite::Row) -> rusqlite::Result<WorkoutPhilosophy> {
    Ok(WorkoutPhilosophy {
        id: row.get(0)?,
        user_id: row.get(1)?,
        content: row.get(2)?,
        source: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn row_to_interview_state(row: &rusqlite::Row) -> rusqlite::Result<InterviewState> {
    Ok(InterviewState {
        user_id: row.get(0)?,
        platform: row.get(1)?,
        mode: row.get(2)?,
        draft: row.get(3)?,
        turns: row.get(4)?,
        started_at: row.get(5)?,
    })
}

fn row_to_plan(row: &rusqlite::Row) -> rusqlite::Result<WorkoutPlan> {
    Ok(WorkoutPlan {
        id: row.get(0)?,
        user_id: row.get(1)?,
        title: row.get(2)?,
        rationale: row.get(3)?,
        philosophy_id: row.get(4)?,
        status: PlanStatus::from_str_loose(&row.get::<_, String>(5)?),
        session_id: row.get(6)?,
        override_note: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn row_to_plan_exercise(row: &rusqlite::Row) -> rusqlite::Result<WorkoutPlanExercise> {
    Ok(WorkoutPlanExercise {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        exercise_type_id: row.get(2)?,
        order_idx: row.get(3)?,
        target_sets: row.get(4)?,
        target_reps: row.get(5)?,
        target_weight_kg: row.get(6)?,
        target_secs: row.get(7)?,
        notes: row.get(8)?,
    })
}

const SELECT_PHILOSOPHY: &str = "SELECT id, user_id, content, source, created_at FROM workout_philosophy";

const SELECT_PLAN: &str = "\
    SELECT id, user_id, title, rationale, philosophy_id, status, session_id, override_note, created_at, updated_at \
    FROM workout_plans";

const SELECT_PLAN_EXERCISE: &str = "\
    SELECT id, plan_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes \
    FROM workout_plan_exercises";

impl Database {
    // ── Philosophy ──────────────────────────────────────────────────────────────

    /// Append a philosophy entry (the table is append-only). Returns its row id.
    pub fn insert_philosophy(&self, user_id: i64, content: &str, source: &str) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO workout_philosophy (user_id, content, source) VALUES (?1, ?2, ?3)",
            params![user_id, content, source],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// The user's current philosophy: the most recently inserted entry.
    pub fn latest_philosophy(&self, user_id: i64) -> anyhow::Result<Option<WorkoutPhilosophy>> {
        let sql = format!("{SELECT_PHILOSOPHY} WHERE user_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_philosophy)?;
        rows.next().transpose().context("Failed to read philosophy row")
    }

    /// Append a durable preference captured mid-workout: read the latest content,
    /// append `"- {note}"` as a new bullet, and insert it as a fresh `note` entry
    /// so the table stays append-only and `latest_philosophy` keeps the full text.
    pub fn append_philosophy_note(&self, user_id: i64, note: &str) -> anyhow::Result<i64> {
        let base = self.latest_philosophy(user_id)?.map(|p| p.content).unwrap_or_default();
        let content = if base.trim().is_empty() { format!("- {note}") } else { format!("{base}\n- {note}") };
        self.insert_philosophy(user_id, &content, "note")
    }

    // ── Interview state ───────────────────────────────────────────────────────────

    pub fn get_interview_state(&self, user_id: i64, platform: &str) -> anyhow::Result<Option<InterviewState>> {
        let mut stmt = self.conn().prepare(
            "SELECT user_id, platform, mode, draft, turns, started_at \
             FROM interview_state WHERE user_id = ?1 AND platform = ?2",
        )?;
        let mut rows = stmt.query_map(params![user_id, platform], row_to_interview_state)?;
        rows.next().transpose().context("Failed to read interview state")
    }

    pub fn set_interview_state(&self, user_id: i64, platform: &str, mode: &str, draft: &str, turns: i32) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO interview_state (user_id, platform, mode, draft, turns) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_id, platform) DO UPDATE SET mode = excluded.mode, draft = excluded.draft, turns = excluded.turns",
            params![user_id, platform, mode, draft, turns],
        )?;
        Ok(())
    }

    pub fn clear_interview_state(&self, user_id: i64, platform: &str) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM interview_state WHERE user_id = ?1 AND platform = ?2", params![user_id, platform])?;
        Ok(())
    }

    // ── Plans ─────────────────────────────────────────────────────────────────────

    pub fn create_plan(&self, user_id: i64, title: &str, rationale: Option<&str>, philosophy_id: Option<i64>) -> anyhow::Result<i64> {
        // A user keeps at most one live proposal: supersede any earlier `proposed`
        // plans so they neither accumulate nor bind to a later session.
        self.conn().execute(
            "UPDATE workout_plans SET status = ?1, updated_at = datetime('now') WHERE user_id = ?2 AND status = ?3",
            params![PlanStatus::Abandoned.as_str(), user_id, PlanStatus::Proposed.as_str()],
        )?;
        self.conn().execute(
            "INSERT INTO workout_plans (user_id, title, rationale, philosophy_id) VALUES (?1, ?2, ?3, ?4)",
            params![user_id, title, rationale, philosophy_id],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn add_plan_exercise(&self, e: &WorkoutPlanExercise) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO workout_plan_exercises \
                 (plan_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![e.plan_id, e.exercise_type_id, e.order_idx, e.target_sets, e.target_reps, e.target_weight_kg, e.target_secs, e.notes],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_plan(&self, plan_id: i64) -> anyhow::Result<Option<WorkoutPlan>> {
        let sql = format!("{SELECT_PLAN} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![plan_id], row_to_plan)?;
        rows.next().transpose().context("Failed to read plan row")
    }

    pub fn list_plan_exercises(&self, plan_id: i64) -> anyhow::Result<Vec<WorkoutPlanExercise>> {
        let sql = format!("{SELECT_PLAN_EXERCISE} WHERE plan_id = ?1 ORDER BY order_idx");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![plan_id], row_to_plan_exercise)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list plan exercises")
    }

    /// The most recent plan still awaiting execution (used to activate after `/nextworkout`).
    pub fn latest_proposed_plan(&self, user_id: i64) -> anyhow::Result<Option<WorkoutPlan>> {
        let sql = format!("{SELECT_PLAN} WHERE user_id = ?1 AND status = 'proposed' ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_plan)?;
        rows.next().transpose().context("Failed to read proposed plan")
    }

    /// The user's currently active (in-progress) plan, if any.
    pub fn active_plan_for_user(&self, user_id: i64) -> anyhow::Result<Option<WorkoutPlan>> {
        let sql = format!("{SELECT_PLAN} WHERE user_id = ?1 AND status = 'active' ORDER BY updated_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_plan)?;
        rows.next().transpose().context("Failed to read active plan")
    }

    pub fn set_plan_status(&self, plan_id: i64, status: PlanStatus) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE workout_plans SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), plan_id],
        )?;
        anyhow::ensure!(rows > 0, "Workout plan with id {plan_id} not found");
        Ok(())
    }

    /// Bind a proposed plan to a session and mark it active for guided execution.
    pub fn bind_plan_to_session(&self, plan_id: i64, session_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE workout_plans SET session_id = ?1, status = 'active', updated_at = datetime('now') WHERE id = ?2",
            params![session_id, plan_id],
        )?;
        anyhow::ensure!(rows > 0, "Workout plan with id {plan_id} not found");
        Ok(())
    }

    /// The plan currently in flight for the user: the active (guided) plan if one is
    /// bound to a live session, otherwise the most recently proposed-but-unstarted
    /// design. This is the target a mid-workout, today-only override attaches to.
    pub fn inflight_plan_for_user(&self, user_id: i64) -> anyhow::Result<Option<WorkoutPlan>> {
        match self.active_plan_for_user(user_id)? {
            Some(plan) => Ok(Some(plan)),
            None => self.latest_proposed_plan(user_id),
        }
    }

    /// Append a today-only override (e.g. "no bench today, do flys instead") to a
    /// plan as a new `"- {note}"` bullet. Scoped to the plan row, so it expires when
    /// the plan completes or is superseded and NEVER reaches the philosophy.
    pub fn append_plan_override(&self, plan_id: i64, note: &str) -> anyhow::Result<()> {
        let existing: Option<String> = self.get_plan(plan_id)?.and_then(|p| p.override_note);
        let combined = match existing.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(base) => format!("{base}\n- {note}"),
            None => format!("- {note}"),
        };
        let rows = self.conn().execute(
            "UPDATE workout_plans SET override_note = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![combined, plan_id],
        )?;
        anyhow::ensure!(rows > 0, "Workout plan with id {plan_id} not found");
        Ok(())
    }

    // ── History for the designer ──────────────────────────────────────────────────

    /// The last `limit` sessions (most recent first), each with its exercise
    /// entries and their sets, for the `/nextworkout` designer prompt.
    pub fn recent_sessions_with_sets(&self, user_id: i64, limit: usize) -> anyhow::Result<Vec<SessionWithSets>> {
        let sessions = self.list_sessions(user_id, None, None)?;
        sessions
            .into_iter()
            .take(limit)
            .map(|session| {
                let entries = self
                    .list_entries_for_session(session.id)?
                    .into_iter()
                    .map(|entry| {
                        let sets = self.list_sets_for_entry(entry.id)?;
                        Ok((entry, sets))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                Ok((session, entries))
            })
            .collect()
    }

    // ── Prescribed vs actual ──────────────────────────────────────────────────────

    /// Compare a plan's prescription against what its bound session actually
    /// performed. The plan carries the `session_id` binding (see
    /// [`bind_plan_to_session`](Self::bind_plan_to_session)); performed sets are
    /// rolled up per exercise_type and diffed against the plan's per-exercise
    /// targets. See [`PlanVsActual`] / [`ExerciseDelta`] for the signed-deviation
    /// semantics: deltas are `performed − prescribed`, and deviation is signal, not
    /// error. Errors if the plan is unknown or not yet bound to a session.
    pub fn plan_vs_actual(&self, plan_id: i64) -> anyhow::Result<PlanVsActual> {
        let plan = self.get_plan(plan_id)?.with_context(|| format!("Workout plan {plan_id} not found"))?;
        let session_id = plan.session_id.with_context(|| format!("Workout plan {plan_id} is not bound to a session"))?;

        let planned = self.list_plan_exercises(plan_id)?;
        let planned_ids: HashSet<i64> = planned.iter().map(|pe| pe.exercise_type_id).collect();
        let (performed, performed_order) = self.performed_by_exercise(session_id)?;

        let matched = planned
            .iter()
            .filter_map(|pe| performed.get(&pe.exercise_type_id).map(|sets| self.exercise_delta(pe, sets)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let skipped = planned
            .iter()
            .filter(|pe| !performed.contains_key(&pe.exercise_type_id))
            .map(|pe| self.skipped_exercise(pe))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let unplanned = performed_order
            .iter()
            .filter(|type_id| !planned_ids.contains(type_id))
            .map(|&type_id| self.unplanned_exercise(type_id, &performed[&type_id]))
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(PlanVsActual { plan_id, session_id, matched, skipped, unplanned })
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

    fn exercise_delta(&self, pe: &WorkoutPlanExercise, sets: &[ExerciseSet]) -> anyhow::Result<ExerciseDelta> {
        let measurement_type = sets.first().map(|s| s.measurement_type).unwrap_or(MeasurementType::WeightReps);
        let performed = rollup(sets, measurement_type);
        Ok(ExerciseDelta {
            exercise_name: self.exercise_name(pe.exercise_type_id)?,
            measurement_type,
            sets_delta: pe.target_sets.map(|t| performed.performed_sets - i64::from(t)),
            reps_delta: signed_delta(performed.avg_reps, pe.target_reps.map(f64::from)),
            weight_delta_kg: signed_delta(performed.avg_weight_kg, pe.target_weight_kg),
            secs_delta: signed_delta(performed.avg_secs, pe.target_secs.map(f64::from)),
            prescribed: pe.clone(),
            performed,
        })
    }

    fn skipped_exercise(&self, pe: &WorkoutPlanExercise) -> anyhow::Result<SkippedExercise> {
        Ok(SkippedExercise { exercise_name: self.exercise_name(pe.exercise_type_id)?, prescribed: pe.clone() })
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
/// against the plan's per-exercise prescription. `value` is interpreted per the
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
    use super::super::models::{new_exercise_entry, new_exercise_set, new_user, MeasurementType, PlanStatus, WorkoutPlanExercise};
    use super::*;

    fn test_db() -> (Database, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        (db, user_id)
    }

    #[test]
    fn philosophy_insert_and_latest() {
        let (db, user_id) = test_db();
        assert!(db.latest_philosophy(user_id).unwrap().is_none());

        db.insert_philosophy(user_id, "v1: hypertrophy", "interview").unwrap();
        db.insert_philosophy(user_id, "v2: 5x5 strength, home gym squat rack 120kg", "interview").unwrap();

        let latest = db.latest_philosophy(user_id).unwrap().unwrap();
        assert!(latest.content.contains("v2"));
        assert_eq!(latest.source, "interview");
    }

    #[test]
    fn append_note_adds_bullet_and_row() {
        let (db, user_id) = test_db();
        db.insert_philosophy(user_id, "5x5 strength", "interview").unwrap();

        db.append_philosophy_note(user_id, "dislikes barbell squats, prefer goblet").unwrap();

        let latest = db.latest_philosophy(user_id).unwrap().unwrap();
        assert_eq!(latest.source, "note");
        assert!(latest.content.starts_with("5x5 strength"));
        assert!(latest.content.contains("- dislikes barbell squats"));
    }

    #[test]
    fn append_note_without_existing_philosophy() {
        let (db, user_id) = test_db();
        db.append_philosophy_note(user_id, "trains mornings only").unwrap();
        let latest = db.latest_philosophy(user_id).unwrap().unwrap();
        assert_eq!(latest.content, "- trains mornings only");
    }

    #[test]
    fn interview_state_upsert_and_clear() {
        let (db, user_id) = test_db();
        assert!(db.get_interview_state(user_id, "telegram").unwrap().is_none());

        db.set_interview_state(user_id, "telegram", "philosophy", "", 0).unwrap();
        db.set_interview_state(user_id, "telegram", "philosophy", "goal=hypertrophy", 1).unwrap();
        let state = db.get_interview_state(user_id, "telegram").unwrap().unwrap();
        assert_eq!(state.turns, 1);
        assert_eq!(state.draft, "goal=hypertrophy");

        // Per-platform isolation: confide unaffected.
        assert!(db.get_interview_state(user_id, "confide").unwrap().is_none());

        db.clear_interview_state(user_id, "telegram").unwrap();
        assert!(db.get_interview_state(user_id, "telegram").unwrap().is_none());
    }

    #[test]
    fn plan_create_load_status_and_bind() {
        let (db, user_id) = test_db();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let plan_id = db.create_plan(user_id, "Push focus", Some("2 days rest, push the bench"), None).unwrap();
        db.add_plan_exercise(&WorkoutPlanExercise {
            id: 0,
            plan_id,
            exercise_type_id: bp.id,
            order_idx: 0,
            target_sets: Some(3),
            target_reps: Some(6),
            target_weight_kg: Some(65.0),
            target_secs: None,
            notes: Some("last session was easy".into()),
        })
        .unwrap();

        let plan = db.get_plan(plan_id).unwrap().unwrap();
        assert_eq!(plan.status, PlanStatus::Proposed);
        assert_eq!(plan.title, "Push focus");

        let exercises = db.list_plan_exercises(plan_id).unwrap();
        assert_eq!(exercises.len(), 1);
        assert_eq!(exercises[0].target_weight_kg, Some(65.0));

        assert_eq!(db.latest_proposed_plan(user_id).unwrap().unwrap().id, plan_id);
        assert!(db.active_plan_for_user(user_id).unwrap().is_none());

        let session = db.start_session(user_id, None).unwrap();
        db.bind_plan_to_session(plan_id, session.id).unwrap();
        let active = db.active_plan_for_user(user_id).unwrap().unwrap();
        assert_eq!(active.id, plan_id);
        assert_eq!(active.session_id, Some(session.id));

        db.set_plan_status(plan_id, PlanStatus::Completed).unwrap();
        assert!(db.active_plan_for_user(user_id).unwrap().is_none());
    }

    #[test]
    fn plan_override_appends_and_does_not_carry_to_next_plan() {
        let (db, user_id) = test_db();
        let first = db.create_plan(user_id, "Push", None, None).unwrap();

        assert!(db.get_plan(first).unwrap().unwrap().override_note.is_none());
        db.append_plan_override(first, "no bench today, do flys").unwrap();
        db.append_plan_override(first, "skip the last set").unwrap();
        let note = db.get_plan(first).unwrap().unwrap().override_note.unwrap();
        assert!(note.contains("- no bench today, do flys"));
        assert!(note.contains("- skip the last set"));

        // A fresh design starts clean — the one-off is scoped to its own plan.
        let second = db.create_plan(user_id, "Pull", None, None).unwrap();
        assert!(db.get_plan(second).unwrap().unwrap().override_note.is_none());
    }

    #[test]
    fn inflight_plan_prefers_active_then_latest_proposed() {
        let (db, user_id) = test_db();
        assert!(db.inflight_plan_for_user(user_id).unwrap().is_none());

        let proposed = db.create_plan(user_id, "Ready", None, None).unwrap();
        assert_eq!(db.inflight_plan_for_user(user_id).unwrap().unwrap().id, proposed);

        let session = db.start_session(user_id, None).unwrap();
        db.bind_plan_to_session(proposed, session.id).unwrap();
        assert_eq!(db.inflight_plan_for_user(user_id).unwrap().unwrap().id, proposed, "active plan is the in-flight one");
    }

    #[test]
    fn designing_a_new_plan_abandons_the_previous_proposal() {
        let (db, user_id) = test_db();
        let first = db.create_plan(user_id, "Plan A", None, None).unwrap();
        let second = db.create_plan(user_id, "Plan B", None, None).unwrap();

        assert_eq!(db.get_plan(first).unwrap().unwrap().status, PlanStatus::Abandoned);
        assert_eq!(db.get_plan(second).unwrap().unwrap().status, PlanStatus::Proposed);
        // Only the newest proposal is live and bindable.
        assert_eq!(db.latest_proposed_plan(user_id).unwrap().unwrap().id, second);
    }

    #[test]
    fn recent_sessions_with_sets_returns_history() {
        let (db, user_id) = test_db();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let session = db.start_session(user_id, None).unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let mut set = new_exercise_set(entry_id, bp.id, MeasurementType::WeightReps, 60.0);
        set.count = Some(8);
        db.insert_set(&set).unwrap();

        let history = db.recent_sessions_with_sets(user_id, 5).unwrap();
        assert_eq!(history.len(), 1);
        let (got_session, entries) = &history[0];
        assert_eq!(got_session.id, session.id);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.len(), 1);
        assert_eq!(entries[0].1[0].value, 60.0);
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
    fn plan_vs_actual_matches_skips_and_flags_unplanned() {
        let (db, user_id) = test_db();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let squat = db.get_exercise_type_by_name("Squat").unwrap().unwrap();
        let deadlift = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap();

        let plan_id = db.create_plan(user_id, "Lower + press", None, None).unwrap();
        db.add_plan_exercise(&WorkoutPlanExercise {
            id: 0,
            plan_id,
            exercise_type_id: bench.id,
            order_idx: 0,
            target_sets: Some(3),
            target_reps: Some(6),
            target_weight_kg: Some(65.0),
            target_secs: None,
            notes: None,
        })
        .unwrap();
        db.add_plan_exercise(&WorkoutPlanExercise {
            id: 0,
            plan_id,
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
        db.bind_plan_to_session(plan_id, session.id).unwrap();

        // Bench: 4 sets @ 6 reps @ 70kg — beat the prescription on sets and weight, met reps.
        log_sets(&db, user_id, session.id, bench.id, 4, 6, 70.0);
        // Squat: prescribed but never performed → skipped.
        // Deadlift: performed but never prescribed → unplanned.
        log_sets(&db, user_id, session.id, deadlift.id, 2, 5, 120.0);

        let cmp = db.plan_vs_actual(plan_id).unwrap();
        assert_eq!(cmp.plan_id, plan_id);
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
    fn plan_vs_actual_signs_shortfalls_negative_and_requires_a_binding() {
        let (db, user_id) = test_db();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let plan_id = db.create_plan(user_id, "Press", None, None).unwrap();
        db.add_plan_exercise(&WorkoutPlanExercise {
            id: 0,
            plan_id,
            exercise_type_id: bench.id,
            order_idx: 0,
            target_sets: Some(4),
            target_reps: Some(8),
            target_weight_kg: Some(80.0),
            target_secs: None,
            notes: None,
        })
        .unwrap();

        // A proposed-but-unbound plan has no session to compare against.
        assert!(db.plan_vs_actual(plan_id).is_err());

        let session = db.start_session(user_id, None).unwrap();
        db.bind_plan_to_session(plan_id, session.id).unwrap();
        // 2 of 4 sets, lighter and fewer reps than prescribed — a shortfall on every dimension.
        log_sets(&db, user_id, session.id, bench.id, 2, 6, 72.5);

        let cmp = db.plan_vs_actual(plan_id).unwrap();
        assert!(cmp.skipped.is_empty());
        assert!(cmp.unplanned.is_empty());
        let delta = &cmp.matched[0];
        assert_eq!(delta.sets_delta, Some(-2)); // 2 − 4, missed
        assert_eq!(delta.reps_delta, Some(-2.0)); // 6 − 8, missed
        assert_eq!(delta.weight_delta_kg, Some(-7.5)); // 72.5 − 80, missed
    }
}

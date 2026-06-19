//! Persistence for the workout planner: the append-only training philosophy, the
//! per-(user, platform) interview state that builds it, and generated workout
//! plans. Designing or storing a plan never logs a set — that stays on the
//! sessions/exercise_entry/sets path.

use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{ExerciseEntry, ExerciseSet, InterviewState, PlanStatus, Session, WorkoutPlan, WorkoutPlanExercise, WorkoutPhilosophy};

/// An exercise entry paired with the sets logged into it.
pub type EntryWithSets = (ExerciseEntry, Vec<ExerciseSet>);

/// A session paired with its entries and their sets — how the `/nextworkout`
/// designer reads recent history.
pub type SessionWithSets = (Session, Vec<EntryWithSets>);

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
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
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
    SELECT id, user_id, title, rationale, philosophy_id, status, session_id, created_at, updated_at \
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
}

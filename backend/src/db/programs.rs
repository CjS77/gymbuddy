//! Persistence for long-term training programmes: the skeleton (goals served,
//! dates, split, mesocycle blocks, progression policy), the week/day slot grid,
//! and the join from a designed workout plan back to the slot it filled. A
//! programme never designs or logs a session itself — sessions keep being
//! designed on demand against it, and plans with no slot stay first-class
//! ad-hoc work.

use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::goals::{SELECT_GOAL, row_to_goal};
use super::models::{Goal, Program, ProgramBlock, ProgramSlot, ProgramStatus, SlotStatus, TrainingMode, WorkoutPlan};
use super::planner::{SELECT_PLAN, row_to_plan};

fn row_to_program(row: &rusqlite::Row) -> rusqlite::Result<Program> {
    Ok(Program {
        id: row.get(0)?,
        user_id: row.get(1)?,
        title: row.get(2)?,
        start_date: row.get(3)?,
        target_end_date: row.get(4)?,
        days_per_week: row.get(5)?,
        split: row.get(6)?,
        progression_policy: row.get(7)?,
        status: ProgramStatus::from_str_loose(&row.get::<_, String>(8)?),
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_block(row: &rusqlite::Row) -> rusqlite::Result<ProgramBlock> {
    Ok(ProgramBlock {
        id: row.get(0)?,
        program_id: row.get(1)?,
        start_week: row.get(2)?,
        end_week: row.get(3)?,
        focus: row.get(4)?,
        notes: row.get(5)?,
    })
}

fn row_to_slot(row: &rusqlite::Row) -> rusqlite::Result<ProgramSlot> {
    Ok(ProgramSlot {
        id: row.get(0)?,
        program_id: row.get(1)?,
        week_idx: row.get(2)?,
        day_idx: row.get(3)?,
        focus: row.get(4)?,
        status: SlotStatus::from_str_loose(&row.get::<_, String>(5)?),
        updated_at: row.get(6)?,
    })
}

const SELECT_PROGRAM: &str = "\
    SELECT id, user_id, title, start_date, target_end_date, days_per_week, split, progression_policy, status, created_at, updated_at \
    FROM programs";

const SELECT_BLOCK: &str = "SELECT id, program_id, start_week, end_week, focus, notes FROM program_blocks";

const SELECT_SLOT: &str = "SELECT id, program_id, week_idx, day_idx, focus, status, updated_at FROM program_slots";

impl Database {
    // ── Programmes ────────────────────────────────────────────────────────────────

    /// Insert a programme as a `draft`. A user keeps at most one live draft:
    /// earlier drafts are abandoned, mirroring how `create_plan` supersedes
    /// earlier `proposed` plans.
    pub fn create_program(&self, p: &Program) -> anyhow::Result<i64> {
        self.conn().execute(
            "UPDATE programs SET status = ?1, updated_at = datetime('now') WHERE user_id = ?2 AND status = ?3",
            params![ProgramStatus::Abandoned.as_str(), p.user_id, ProgramStatus::Draft.as_str()],
        )?;
        self.conn().execute(
            "INSERT INTO programs (user_id, title, start_date, target_end_date, days_per_week, split, progression_policy) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![p.user_id, p.title, p.start_date, p.target_end_date, p.days_per_week, p.split, p.progression_policy],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_program(&self, program_id: i64) -> anyhow::Result<Option<Program>> {
        let sql = format!("{SELECT_PROGRAM} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![program_id], row_to_program)?;
        rows.next().transpose().context("Failed to read program row")
    }

    /// The most recent draft still awaiting activation.
    pub fn latest_draft_program(&self, user_id: i64) -> anyhow::Result<Option<Program>> {
        let sql = format!("{SELECT_PROGRAM} WHERE user_id = ?1 AND status = 'draft' ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_program)?;
        rows.next().transpose().context("Failed to read draft program")
    }

    /// The user's currently active programme, if any. `activate_program` keeps
    /// this at most one.
    pub fn active_program_for_user(&self, user_id: i64) -> anyhow::Result<Option<Program>> {
        let sql = format!("{SELECT_PROGRAM} WHERE user_id = ?1 AND status = 'active' ORDER BY updated_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_program)?;
        rows.next().transpose().context("Failed to read active program")
    }

    /// Activate a programme, abandoning any other active programme of the same
    /// user first — one active programme per user, the way `create_plan` keeps
    /// one live proposal.
    pub fn activate_program(&self, program_id: i64) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE programs SET status = 'abandoned', updated_at = datetime('now') \
             WHERE status = 'active' AND id != ?1 AND user_id = (SELECT user_id FROM programs WHERE id = ?1)",
            params![program_id],
        )?;
        let rows = self.conn().execute(
            "UPDATE programs SET status = 'active', updated_at = datetime('now') WHERE id = ?1",
            params![program_id],
        )?;
        anyhow::ensure!(rows > 0, "Program with id {program_id} not found");
        Ok(())
    }

    pub fn set_program_status(&self, program_id: i64, status: ProgramStatus) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE programs SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), program_id],
        )?;
        anyhow::ensure!(rows > 0, "Program with id {program_id} not found");
        Ok(())
    }

    // ── Goals served ──────────────────────────────────────────────────────────────

    /// Link a goal to the programme that serves it. Idempotent.
    pub fn add_program_goal(&self, program_id: i64, goal_id: i64) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO program_goals (program_id, goal_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            params![program_id, goal_id],
        )?;
        Ok(())
    }

    /// The goals a programme serves, highest priority first.
    pub fn list_program_goals(&self, program_id: i64) -> anyhow::Result<Vec<Goal>> {
        let sql = format!(
            "{SELECT_GOAL} WHERE id IN (SELECT goal_id FROM program_goals WHERE program_id = ?1) ORDER BY priority DESC, id"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![program_id], row_to_goal)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list program goals")
    }

    // ── Mesocycle blocks ──────────────────────────────────────────────────────────

    pub fn add_program_block(&self, b: &ProgramBlock) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO program_blocks (program_id, start_week, end_week, focus, notes) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![b.program_id, b.start_week, b.end_week, b.focus, b.notes],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn list_program_blocks(&self, program_id: i64) -> anyhow::Result<Vec<ProgramBlock>> {
        let sql = format!("{SELECT_BLOCK} WHERE program_id = ?1 ORDER BY start_week");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![program_id], row_to_block)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list program blocks")
    }

    // ── The week/day slot grid ────────────────────────────────────────────────────

    pub fn add_program_slot(&self, s: &ProgramSlot) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO program_slots (program_id, week_idx, day_idx, focus) VALUES (?1, ?2, ?3, ?4)",
            params![s.program_id, s.week_idx, s.day_idx, s.focus],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_program_slot(&self, slot_id: i64) -> anyhow::Result<Option<ProgramSlot>> {
        let sql = format!("{SELECT_SLOT} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![slot_id], row_to_slot)?;
        rows.next().transpose().context("Failed to read program slot")
    }

    pub fn list_program_slots(&self, program_id: i64) -> anyhow::Result<Vec<ProgramSlot>> {
        let sql = format!("{SELECT_SLOT} WHERE program_id = ?1 ORDER BY week_idx, day_idx");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![program_id], row_to_slot)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list program slots")
    }

    pub fn set_slot_status(&self, slot_id: i64, status: SlotStatus) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE program_slots SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), slot_id],
        )?;
        anyhow::ensure!(rows > 0, "Program slot with id {slot_id} not found");
        Ok(())
    }

    /// The slot a new `/nextworkout` design targets: the earliest cell (week,
    /// day order) not yet conclusively resolved. A slot is unresolved while it
    /// is `pending`, and also while it is `filled` but no plan bound to it has
    /// ever been executed (`active`/`completed`) — so redesigning re-targets the
    /// same slot instead of burning the next one, including when an earlier
    /// design went stale and was abandoned. `missed` and `skipped` slots are
    /// settled history and never come back.
    pub fn next_design_slot(&self, program_id: i64) -> anyhow::Result<Option<ProgramSlot>> {
        let sql = format!(
            "{SELECT_SLOT} WHERE program_id = ?1 AND (status = 'pending' \
                OR (status = 'filled' AND NOT EXISTS ( \
                    SELECT 1 FROM workout_plans p WHERE p.program_slot_id = program_slots.id AND p.status IN ('active', 'completed')))) \
             ORDER BY week_idx, day_idx LIMIT 1"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![program_id], row_to_slot)?;
        rows.next().transpose().context("Failed to read next design slot")
    }

    // ── Training mode ([C1.4]) ────────────────────────────────────────────────────

    /// Resolve the [`TrainingMode`] a new session design runs in. With no active
    /// programme the mode is plain ad-hoc — exactly the pre-programme behaviour.
    /// With one, the design targets the programme's next unresolved slot, unless
    /// `force_ad_hoc` records the user's explicit "leave my programme alone" (or
    /// the grid has no designable slot left): then the programme is reported but
    /// no slot is targeted, so adherence can never mutate.
    pub fn training_mode_for_design(&self, user_id: i64, force_ad_hoc: bool) -> anyhow::Result<TrainingMode> {
        let Some(program) = self.active_program_for_user(user_id)? else {
            return Ok(TrainingMode::AdHoc { program: None });
        };
        if force_ad_hoc {
            return Ok(TrainingMode::AdHoc { program: Some(program) });
        }
        match self.next_design_slot(program.id)? {
            Some(slot) => Ok(TrainingMode::Program { program, slot }),
            None => Ok(TrainingMode::AdHoc { program: Some(program) }),
        }
    }

    // ── Plan ↔ slot join ──────────────────────────────────────────────────────────

    /// Stamp a designed plan with the programme slot it fills and mark the slot
    /// filled. Ad-hoc plans never call this — their `program_slot_id` stays NULL.
    pub fn bind_plan_to_slot(&self, plan_id: i64, slot_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE workout_plans SET program_slot_id = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![slot_id, plan_id],
        )?;
        anyhow::ensure!(rows > 0, "Workout plan with id {plan_id} not found");
        self.set_slot_status(slot_id, SlotStatus::Filled)
    }

    /// The plan that filled a slot, if one has bound to it.
    pub fn plan_for_slot(&self, slot_id: i64) -> anyhow::Result<Option<WorkoutPlan>> {
        let sql = format!("{SELECT_PLAN} WHERE program_slot_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![slot_id], row_to_plan)?;
        rows.next().transpose().context("Failed to read plan for slot")
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{PlanStatus, new_exercise_goal, new_program, new_program_block, new_program_slot, new_user};
    use super::*;

    fn test_db() -> (Database, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        (db, user_id)
    }

    fn draft_program(db: &Database, user_id: i64, title: &str) -> i64 {
        db.create_program(&new_program(user_id, title, 4, "upper/lower", "double progression: add reps, then load")).unwrap()
    }

    #[test]
    fn program_create_load_round_trips() {
        let (db, user_id) = test_db();
        assert!(db.latest_draft_program(user_id).unwrap().is_none());

        let mut draft = new_program(user_id, "12-week hypertrophy", 4, "upper/lower", "add 2.5kg when all sets hit top of range");
        draft.start_date = "2026-08-01".into();
        draft.target_end_date = Some("2026-10-24".into());
        let id = db.create_program(&draft).unwrap();

        let p = db.get_program(id).unwrap().unwrap();
        assert_eq!(p.status, ProgramStatus::Draft);
        assert_eq!(p.title, "12-week hypertrophy");
        assert_eq!(p.days_per_week, 4);
        assert_eq!(p.start_date, "2026-08-01");
        assert_eq!(p.target_end_date.as_deref(), Some("2026-10-24"));
        assert_eq!(db.latest_draft_program(user_id).unwrap().unwrap().id, id);
    }

    #[test]
    fn creating_a_new_program_abandons_the_previous_draft() {
        let (db, user_id) = test_db();
        let first = draft_program(&db, user_id, "Draft A");
        let second = draft_program(&db, user_id, "Draft B");

        assert_eq!(db.get_program(first).unwrap().unwrap().status, ProgramStatus::Abandoned);
        assert_eq!(db.get_program(second).unwrap().unwrap().status, ProgramStatus::Draft);
        assert_eq!(db.latest_draft_program(user_id).unwrap().unwrap().id, second);
    }

    #[test]
    fn activating_a_program_abandons_the_previous_active_one() {
        let (db, user_id) = test_db();
        let first = draft_program(&db, user_id, "Programme A");
        db.activate_program(first).unwrap();
        assert_eq!(db.active_program_for_user(user_id).unwrap().unwrap().id, first);

        let second = draft_program(&db, user_id, "Programme B");
        db.activate_program(second).unwrap();

        assert_eq!(db.get_program(first).unwrap().unwrap().status, ProgramStatus::Abandoned);
        assert_eq!(db.active_program_for_user(user_id).unwrap().unwrap().id, second);
    }

    #[test]
    fn activation_is_scoped_to_the_owning_user() {
        let (db, user_id) = test_db();
        let other_id = db.insert_user(&new_user("Other", None, "UTC")).unwrap();

        let mine = draft_program(&db, user_id, "Mine");
        let theirs = draft_program(&db, other_id, "Theirs");
        db.activate_program(mine).unwrap();
        db.activate_program(theirs).unwrap();

        // Each user keeps their own active programme.
        assert_eq!(db.active_program_for_user(user_id).unwrap().unwrap().id, mine);
        assert_eq!(db.active_program_for_user(other_id).unwrap().unwrap().id, theirs);
    }

    #[test]
    fn program_goals_link_and_list_by_priority() {
        let (db, user_id) = test_db();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let dl = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap();

        let mut low = new_exercise_goal(user_id, bp.id, 100.0);
        low.priority = 1;
        let low_id = db.insert_goal(&low).unwrap();
        let mut high = new_exercise_goal(user_id, dl.id, 180.0);
        high.priority = 5;
        let high_id = db.insert_goal(&high).unwrap();

        let program_id = draft_program(&db, user_id, "Strength block");
        db.add_program_goal(program_id, low_id).unwrap();
        db.add_program_goal(program_id, high_id).unwrap();
        // Idempotent: relinking is not an error and adds nothing.
        db.add_program_goal(program_id, high_id).unwrap();

        let goals = db.list_program_goals(program_id).unwrap();
        assert_eq!(goals.iter().map(|g| g.id).collect::<Vec<_>>(), vec![high_id, low_id]);
    }

    #[test]
    fn blocks_list_in_week_order() {
        let (db, user_id) = test_db();
        let program_id = draft_program(&db, user_id, "Meso");

        db.add_program_block(&new_program_block(program_id, 5, 6, "deload")).unwrap();
        let mut hyper = new_program_block(program_id, 1, 4, "hypertrophy");
        hyper.notes = Some("RIR 2, add a set per week".into());
        db.add_program_block(&hyper).unwrap();

        let blocks = db.list_program_blocks(program_id).unwrap();
        assert_eq!(blocks.iter().map(|b| (b.start_week, b.end_week)).collect::<Vec<_>>(), vec![(1, 4), (5, 6)]);
        assert_eq!(blocks[0].focus, "hypertrophy");
        assert_eq!(blocks[0].notes.as_deref(), Some("RIR 2, add a set per week"));
    }

    #[test]
    fn slot_grid_lists_in_order_and_rejects_duplicates() {
        let (db, user_id) = test_db();
        let program_id = draft_program(&db, user_id, "Grid");

        db.add_program_slot(&new_program_slot(program_id, 2, 1, "lower")).unwrap();
        db.add_program_slot(&new_program_slot(program_id, 1, 2, "lower")).unwrap();
        let slot = db.add_program_slot(&new_program_slot(program_id, 1, 1, "upper")).unwrap();

        // The (program, week, day) cell is unique.
        assert!(db.add_program_slot(&new_program_slot(program_id, 1, 1, "upper again")).is_err());

        let slots = db.list_program_slots(program_id).unwrap();
        assert_eq!(slots.iter().map(|s| (s.week_idx, s.day_idx)).collect::<Vec<_>>(), vec![(1, 1), (1, 2), (2, 1)]);
        assert!(slots.iter().all(|s| s.status == SlotStatus::Pending));

        db.set_slot_status(slot, SlotStatus::Skipped).unwrap();
        assert_eq!(db.get_program_slot(slot).unwrap().unwrap().status, SlotStatus::Skipped);
    }

    #[test]
    fn binding_a_plan_fills_its_slot() {
        let (db, user_id) = test_db();
        let program_id = draft_program(&db, user_id, "Programme");
        let slot_id = db.add_program_slot(&new_program_slot(program_id, 1, 1, "upper")).unwrap();

        let plan_id = db.create_plan(user_id, "Week 1 day 1: upper", None, None).unwrap();
        db.bind_plan_to_slot(plan_id, slot_id).unwrap();

        assert_eq!(db.get_plan(plan_id).unwrap().unwrap().program_slot_id, Some(slot_id));
        assert_eq!(db.get_program_slot(slot_id).unwrap().unwrap().status, SlotStatus::Filled);
        assert_eq!(db.plan_for_slot(slot_id).unwrap().unwrap().id, plan_id);
    }

    /// An active programme with a 2×2 grid, returning (program_id, slot ids in
    /// (week, day) order).
    fn active_program_with_grid(db: &Database, user_id: i64) -> (i64, Vec<i64>) {
        let program_id = draft_program(db, user_id, "12-week hypertrophy");
        db.activate_program(program_id).unwrap();
        let slots = [(1, 1, "upper"), (1, 2, "lower"), (2, 1, "upper"), (2, 2, "lower")]
            .iter()
            .map(|(w, d, focus)| db.add_program_slot(&new_program_slot(program_id, *w, *d, focus)).unwrap())
            .collect();
        (program_id, slots)
    }

    #[test]
    fn next_design_slot_walks_the_grid_and_retargets_unexecuted_fills() {
        let (db, user_id) = test_db();
        let (program_id, slots) = active_program_with_grid(&db, user_id);

        assert_eq!(db.next_design_slot(program_id).unwrap().unwrap().id, slots[0]);

        // A merely-proposed (or later abandoned) plan does not resolve the slot:
        // a redesign re-targets it rather than burning the next one.
        let proposed = db.create_plan(user_id, "W1D1 upper", None, None).unwrap();
        db.bind_plan_to_slot(proposed, slots[0]).unwrap();
        assert_eq!(db.next_design_slot(program_id).unwrap().unwrap().id, slots[0]);

        // Executing the plan resolves the slot; design moves on.
        let session = db.start_session(user_id, None).unwrap();
        db.bind_plan_to_session(proposed, session.id).unwrap();
        assert_eq!(db.next_design_slot(program_id).unwrap().unwrap().id, slots[1]);

        // Missed and skipped slots are settled history.
        db.set_slot_status(slots[1], SlotStatus::Missed).unwrap();
        db.set_slot_status(slots[2], SlotStatus::Skipped).unwrap();
        assert_eq!(db.next_design_slot(program_id).unwrap().unwrap().id, slots[3]);

        db.set_slot_status(slots[3], SlotStatus::Filled).unwrap();
        let last = db.create_plan(user_id, "W2D2 lower", None, None).unwrap();
        db.bind_plan_to_slot(last, slots[3]).unwrap();
        db.set_plan_status(last, PlanStatus::Completed).unwrap();
        assert!(db.next_design_slot(program_id).unwrap().is_none(), "a fully resolved grid has no design slot");
    }

    #[test]
    fn training_mode_without_a_programme_is_plain_ad_hoc() {
        let (db, user_id) = test_db();
        // No programme at all, and also a draft one: neither puts the user in programme mode.
        assert!(matches!(db.training_mode_for_design(user_id, false).unwrap(), TrainingMode::AdHoc { program: None }));
        draft_program(&db, user_id, "still a draft");
        assert!(matches!(db.training_mode_for_design(user_id, false).unwrap(), TrainingMode::AdHoc { program: None }));
    }

    #[test]
    fn training_mode_with_active_programme_targets_the_current_slot() {
        let (db, user_id) = test_db();
        let (program_id, slots) = active_program_with_grid(&db, user_id);

        match db.training_mode_for_design(user_id, false).unwrap() {
            TrainingMode::Program { program, slot } => {
                assert_eq!(program.id, program_id);
                assert_eq!(slot.id, slots[0]);
            }
            other => panic!("expected programme mode, got {other:?}"),
        }
    }

    #[test]
    fn forced_ad_hoc_reports_the_programme_but_targets_no_slot() {
        let (db, user_id) = test_db();
        let (program_id, _) = active_program_with_grid(&db, user_id);

        match db.training_mode_for_design(user_id, true).unwrap() {
            TrainingMode::AdHoc { program: Some(program) } => assert_eq!(program.id, program_id),
            other => panic!("expected ad-hoc-with-programme, got {other:?}"),
        }
    }

    #[test]
    fn exhausted_grid_falls_back_to_ad_hoc_with_the_programme_reported() {
        let (db, user_id) = test_db();
        let (program_id, slots) = active_program_with_grid(&db, user_id);
        slots.iter().for_each(|slot| db.set_slot_status(*slot, SlotStatus::Skipped).unwrap());

        match db.training_mode_for_design(user_id, false).unwrap() {
            TrainingMode::AdHoc { program: Some(program) } => assert_eq!(program.id, program_id),
            other => panic!("expected ad-hoc fallback, got {other:?}"),
        }
    }

    #[test]
    fn ad_hoc_plans_stay_first_class_with_no_slot() {
        let (db, user_id) = test_db();
        let plan_id = db.create_plan(user_id, "Ad-hoc push day", None, None).unwrap();

        let plan = db.get_plan(plan_id).unwrap().unwrap();
        assert_eq!(plan.program_slot_id, None);

        // The full plan lifecycle works without any programme existing at all.
        let session = db.start_session(user_id, None).unwrap();
        db.bind_plan_to_session(plan_id, session.id).unwrap();
        assert_eq!(db.active_plan_for_user(user_id).unwrap().unwrap().program_slot_id, None);
    }
}

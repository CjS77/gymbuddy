//! Persistence for long-term training programmes: the skeleton (goals served,
//! dates, split, mesocycle blocks, progression policy) and the week/day slot
//! grid. A programme never designs or logs a session itself — sessions keep
//! being designed on demand against it, and rosters with no slot stay
//! first-class ad-hoc work.
//!
//! This module owns `programmes`, `programme_goals`, `programme_blocks` and
//! `programme_slots`, and nothing else. The roster↔slot join is written from the
//! roster side by [`Database::bind_roster_to_slot`] in [`super::rosters`], which
//! calls back into [`Database::set_slot_status`] for the slot half.

use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::goals::{SELECT_GOAL, row_to_goal};
use super::models::{
    Goal, LifecycleStatus, Programme, ProgrammeBlock, ProgrammeContext, ProgrammeSlot, SlotAdherence, SlotStatus, TrainingMode,
};

fn row_to_programme(row: &rusqlite::Row) -> rusqlite::Result<Programme> {
    Ok(Programme {
        id: row.get(0)?,
        user_id: row.get(1)?,
        title: row.get(2)?,
        start_date: row.get(3)?,
        target_end_date: row.get(4)?,
        days_per_week: row.get(5)?,
        split: row.get(6)?,
        progression_policy: row.get(7)?,
        status: LifecycleStatus::from_str_loose(&row.get::<_, String>(8)?),
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_block(row: &rusqlite::Row) -> rusqlite::Result<ProgrammeBlock> {
    Ok(ProgrammeBlock {
        id: row.get(0)?,
        programme_id: row.get(1)?,
        start_week: row.get(2)?,
        end_week: row.get(3)?,
        focus: row.get(4)?,
        notes: row.get(5)?,
    })
}

fn row_to_slot(row: &rusqlite::Row) -> rusqlite::Result<ProgrammeSlot> {
    Ok(ProgrammeSlot {
        id: row.get(0)?,
        programme_id: row.get(1)?,
        week_idx: row.get(2)?,
        day_idx: row.get(3)?,
        focus: row.get(4)?,
        status: SlotStatus::from_str_loose(&row.get::<_, String>(5)?),
        updated_at: row.get(6)?,
    })
}

const SELECT_PROGRAMME: &str = "\
    SELECT id, user_id, title, start_date, target_end_date, days_per_week, split, progression_policy, status, created_at, updated_at \
    FROM programmes";

const SELECT_BLOCK: &str = "SELECT id, programme_id, start_week, end_week, focus, notes FROM programme_blocks";

const SELECT_SLOT: &str = "SELECT id, programme_id, week_idx, day_idx, focus, status, updated_at FROM programme_slots";

impl Database {
    // ── Programmes ────────────────────────────────────────────────────────────────

    /// Insert a programme as a `draft`. A user keeps at most one live draft:
    /// earlier drafts are abandoned, mirroring how `create_roster` supersedes
    /// earlier draft rosters.
    pub fn create_programme(&self, p: &Programme) -> anyhow::Result<i64> {
        self.conn().execute(
            "UPDATE programmes SET status = ?1, updated_at = datetime('now') WHERE user_id = ?2 AND status = ?3",
            params![LifecycleStatus::Abandoned.as_str(), p.user_id, LifecycleStatus::Draft.as_str()],
        )?;
        self.conn().execute(
            "INSERT INTO programmes (user_id, title, start_date, target_end_date, days_per_week, split, progression_policy) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![p.user_id, p.title, p.start_date, p.target_end_date, p.days_per_week, p.split, p.progression_policy],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_programme(&self, programme_id: i64) -> anyhow::Result<Option<Programme>> {
        let sql = format!("{SELECT_PROGRAMME} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![programme_id], row_to_programme)?;
        rows.next().transpose().context("Failed to read programme row")
    }

    /// The most recent draft still awaiting activation.
    pub fn latest_draft_programme(&self, user_id: i64) -> anyhow::Result<Option<Programme>> {
        let sql = format!("{SELECT_PROGRAMME} WHERE user_id = ?1 AND status = 'draft' ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_programme)?;
        rows.next().transpose().context("Failed to read draft programme")
    }

    /// The user's currently active programme, if any. `activate_programme` keeps
    /// this at most one.
    pub fn active_programme_for_user(&self, user_id: i64) -> anyhow::Result<Option<Programme>> {
        let sql = format!("{SELECT_PROGRAMME} WHERE user_id = ?1 AND status = 'active' ORDER BY updated_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_programme)?;
        rows.next().transpose().context("Failed to read active programme")
    }

    /// Activate a programme, abandoning any other active programme of the same
    /// user first — one active programme per user, the way `create_roster` keeps
    /// one live draft.
    pub fn activate_programme(&self, programme_id: i64) -> anyhow::Result<()> {
        self.conn().execute(
            "UPDATE programmes SET status = 'abandoned', updated_at = datetime('now') \
             WHERE status = 'active' AND id != ?1 AND user_id = (SELECT user_id FROM programmes WHERE id = ?1)",
            params![programme_id],
        )?;
        let rows = self.conn().execute(
            "UPDATE programmes SET status = 'active', updated_at = datetime('now') WHERE id = ?1",
            params![programme_id],
        )?;
        anyhow::ensure!(rows > 0, "Programme with id {programme_id} not found");
        Ok(())
    }

    pub fn set_programme_status(&self, programme_id: i64, status: LifecycleStatus) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE programmes SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), programme_id],
        )?;
        anyhow::ensure!(rows > 0, "Programme with id {programme_id} not found");
        Ok(())
    }

    // ── Goals served ──────────────────────────────────────────────────────────────

    /// Link a goal to the programme that serves it. Idempotent.
    pub fn add_programme_goal(&self, programme_id: i64, goal_id: i64) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO programme_goals (programme_id, goal_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            params![programme_id, goal_id],
        )?;
        Ok(())
    }

    /// The goals a programme serves, highest priority first.
    pub fn list_programme_goals(&self, programme_id: i64) -> anyhow::Result<Vec<Goal>> {
        let sql = format!(
            "{SELECT_GOAL} WHERE id IN (SELECT goal_id FROM programme_goals WHERE programme_id = ?1) ORDER BY priority DESC, id"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![programme_id], row_to_goal)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list programme goals")
    }

    // ── Mesocycle blocks ──────────────────────────────────────────────────────────

    pub fn add_programme_block(&self, b: &ProgrammeBlock) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO programme_blocks (programme_id, start_week, end_week, focus, notes) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![b.programme_id, b.start_week, b.end_week, b.focus, b.notes],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn list_programme_blocks(&self, programme_id: i64) -> anyhow::Result<Vec<ProgrammeBlock>> {
        let sql = format!("{SELECT_BLOCK} WHERE programme_id = ?1 ORDER BY start_week");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![programme_id], row_to_block)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list programme blocks")
    }

    /// The block whose inclusive week range covers `week_idx`, if the programme has one there.
    /// Blocks are not required to tile the whole programme, so a week between them has no block —
    /// which reads as ordinary progression, never as a deload ([C5.3]).
    ///
    /// Overlapping blocks are not prevented by the schema; the earliest-starting one wins, matching
    /// [`list_programme_blocks`](Self::list_programme_blocks)'s order.
    pub fn block_for_week(&self, programme_id: i64, week_idx: i32) -> anyhow::Result<Option<ProgrammeBlock>> {
        let sql = format!("{SELECT_BLOCK} WHERE programme_id = ?1 AND start_week <= ?2 AND end_week >= ?2 ORDER BY start_week LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![programme_id, week_idx], row_to_block)?;
        rows.next().transpose().context("Failed to read the block for a week")
    }

    // ── The week/day slot grid ────────────────────────────────────────────────────

    pub fn add_programme_slot(&self, s: &ProgrammeSlot) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO programme_slots (programme_id, week_idx, day_idx, focus) VALUES (?1, ?2, ?3, ?4)",
            params![s.programme_id, s.week_idx, s.day_idx, s.focus],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_programme_slot(&self, slot_id: i64) -> anyhow::Result<Option<ProgrammeSlot>> {
        let sql = format!("{SELECT_SLOT} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![slot_id], row_to_slot)?;
        rows.next().transpose().context("Failed to read programme slot")
    }

    pub fn list_programme_slots(&self, programme_id: i64) -> anyhow::Result<Vec<ProgrammeSlot>> {
        let sql = format!("{SELECT_SLOT} WHERE programme_id = ?1 ORDER BY week_idx, day_idx");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![programme_id], row_to_slot)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list programme slots")
    }

    pub fn set_slot_status(&self, slot_id: i64, status: SlotStatus) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE programme_slots SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status.as_str(), slot_id],
        )?;
        anyhow::ensure!(rows > 0, "Programme slot with id {slot_id} not found");
        Ok(())
    }

    /// The slot a new `/nextworkout` design targets: the earliest cell (week,
    /// day order) not yet conclusively resolved. A slot is unresolved while it
    /// is `pending`, and also while it is `filled` but no roster bound to it has
    /// ever been executed (`active`/`completed`) — so redesigning re-targets the
    /// same slot instead of burning the next one, including when an earlier
    /// design went stale and was abandoned. `missed` and `skipped` slots are
    /// settled history and never come back.
    pub fn next_design_slot(&self, programme_id: i64) -> anyhow::Result<Option<ProgrammeSlot>> {
        let sql = format!(
            "{SELECT_SLOT} WHERE programme_id = ?1 AND (status = 'pending' \
                OR (status = 'filled' AND NOT EXISTS ( \
                    SELECT 1 FROM session_rosters r \
                    WHERE r.programme_slot_id = programme_slots.id AND r.status IN ('active', 'completed')))) \
             ORDER BY week_idx, day_idx LIMIT 1"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![programme_id], row_to_slot)?;
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
        let Some(programme) = self.active_programme_for_user(user_id)? else {
            return Ok(TrainingMode::AdHoc { programme: None });
        };
        if force_ad_hoc {
            return Ok(TrainingMode::AdHoc { programme: Some(programme) });
        }
        match self.next_design_slot(programme.id)? {
            Some(slot) => Ok(TrainingMode::Programme { programme, slot }),
            None => Ok(TrainingMode::AdHoc { programme: Some(programme) }),
        }
    }

    // ── Programme context for a design ([C4.3]) ───────────────────────────────────

    /// Where a design sits in its programme: the slot's week and day against the grid's span, the
    /// block covering that week, the slot's focus, and adherence over everything before it.
    ///
    /// `None` for every ad-hoc mode, including a deliberate one-off under an active programme: an
    /// ad-hoc design fills no slot, so it has no position to report and nothing to build on.
    pub fn programme_context(&self, mode: &TrainingMode) -> anyhow::Result<Option<ProgrammeContext>> {
        let TrainingMode::Programme { programme, slot } = mode else { return Ok(None) };
        Ok(Some(ProgrammeContext {
            programme_title: programme.title.clone(),
            week_idx: slot.week_idx,
            total_weeks: self.programme_span_weeks(programme.id)?,
            day_idx: slot.day_idx,
            days_per_week: programme.days_per_week,
            slot_focus: slot.focus.clone(),
            block: self.block_for_week(programme.id, slot.week_idx)?,
            adherence: self.slot_adherence(programme.id, slot)?,
        }))
    }

    /// How many weeks the programme's slot grid actually spans. Read off the grid rather than off
    /// `target_end_date`, which is a nullable aspiration — "week 3 of 8" has to mean eight weeks of
    /// slots the user can train, or the fraction is not one.
    fn programme_span_weeks(&self, programme_id: i64) -> anyhow::Result<i32> {
        let sql = "SELECT COALESCE(MAX(week_idx), 0) FROM programme_slots WHERE programme_id = ?1";
        self.conn().query_row(sql, params![programme_id], |row| row.get(0)).context("Failed to read the programme's week span")
    }

    /// Adherence over the slots strictly earlier in the grid than `slot` — the sessions that were
    /// scheduled before the one being designed. The slot being designed is excluded on purpose: it
    /// has not happened yet, and counting it would report the user short by one every session.
    fn slot_adherence(&self, programme_id: i64, slot: &ProgrammeSlot) -> anyhow::Result<SlotAdherence> {
        let sql = "\
            SELECT COUNT(*), \
                   COALESCE(SUM(EXISTS (SELECT 1 FROM session_rosters r \
                       WHERE r.programme_slot_id = s.id AND r.status IN ('active', 'completed'))), 0), \
                   COALESCE(SUM(s.status = 'missed'), 0), \
                   COALESCE(SUM(s.status = 'skipped'), 0) \
            FROM programme_slots s \
            WHERE s.programme_id = ?1 AND (s.week_idx < ?2 OR (s.week_idx = ?2 AND s.day_idx < ?3))";
        self.conn()
            .query_row(sql, params![programme_id, slot.week_idx, slot.day_idx], |row| {
                Ok(SlotAdherence { due: row.get(0)?, trained: row.get(1)?, missed: row.get(2)?, skipped: row.get(3)? })
            })
            .context("Failed to read programme slot adherence")
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{new_exercise_goal, new_programme, new_programme_block, new_programme_slot, new_user};
    use super::*;

    fn test_db() -> (Database, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        (db, user_id)
    }

    fn draft_programme(db: &Database, user_id: i64, title: &str) -> i64 {
        db.create_programme(&new_programme(user_id, title, 4, "upper/lower", "double progression: add reps, then load")).unwrap()
    }

    #[test]
    fn programme_create_load_round_trips() {
        let (db, user_id) = test_db();
        assert!(db.latest_draft_programme(user_id).unwrap().is_none());

        let mut draft = new_programme(user_id, "12-week hypertrophy", 4, "upper/lower", "add 2.5kg when all sets hit top of range");
        draft.start_date = "2026-08-01".into();
        draft.target_end_date = Some("2026-10-24".into());
        let id = db.create_programme(&draft).unwrap();

        let p = db.get_programme(id).unwrap().unwrap();
        assert_eq!(p.status, LifecycleStatus::Draft);
        assert_eq!(p.title, "12-week hypertrophy");
        assert_eq!(p.days_per_week, 4);
        assert_eq!(p.start_date, "2026-08-01");
        assert_eq!(p.target_end_date.as_deref(), Some("2026-10-24"));
        assert_eq!(db.latest_draft_programme(user_id).unwrap().unwrap().id, id);
    }

    #[test]
    fn creating_a_new_programme_abandons_the_previous_draft() {
        let (db, user_id) = test_db();
        let first = draft_programme(&db, user_id, "Draft A");
        let second = draft_programme(&db, user_id, "Draft B");

        assert_eq!(db.get_programme(first).unwrap().unwrap().status, LifecycleStatus::Abandoned);
        assert_eq!(db.get_programme(second).unwrap().unwrap().status, LifecycleStatus::Draft);
        assert_eq!(db.latest_draft_programme(user_id).unwrap().unwrap().id, second);
    }

    #[test]
    fn activating_a_programme_abandons_the_previous_active_one() {
        let (db, user_id) = test_db();
        let first = draft_programme(&db, user_id, "Programme A");
        db.activate_programme(first).unwrap();
        assert_eq!(db.active_programme_for_user(user_id).unwrap().unwrap().id, first);

        let second = draft_programme(&db, user_id, "Programme B");
        db.activate_programme(second).unwrap();

        assert_eq!(db.get_programme(first).unwrap().unwrap().status, LifecycleStatus::Abandoned);
        assert_eq!(db.active_programme_for_user(user_id).unwrap().unwrap().id, second);
    }

    #[test]
    fn activation_is_scoped_to_the_owning_user() {
        let (db, user_id) = test_db();
        let other_id = db.insert_user(&new_user("Other", None, "UTC")).unwrap();

        let mine = draft_programme(&db, user_id, "Mine");
        let theirs = draft_programme(&db, other_id, "Theirs");
        db.activate_programme(mine).unwrap();
        db.activate_programme(theirs).unwrap();

        // Each user keeps their own active programme.
        assert_eq!(db.active_programme_for_user(user_id).unwrap().unwrap().id, mine);
        assert_eq!(db.active_programme_for_user(other_id).unwrap().unwrap().id, theirs);
    }

    #[test]
    fn programme_goals_link_and_list_by_priority() {
        let (db, user_id) = test_db();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let dl = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap();

        let mut low = new_exercise_goal(user_id, bp.id, 100.0);
        low.priority = 1;
        let low_id = db.insert_goal(&low).unwrap();
        let mut high = new_exercise_goal(user_id, dl.id, 180.0);
        high.priority = 5;
        let high_id = db.insert_goal(&high).unwrap();

        let programme_id = draft_programme(&db, user_id, "Strength block");
        db.add_programme_goal(programme_id, low_id).unwrap();
        db.add_programme_goal(programme_id, high_id).unwrap();
        // Idempotent: relinking is not an error and adds nothing.
        db.add_programme_goal(programme_id, high_id).unwrap();

        let goals = db.list_programme_goals(programme_id).unwrap();
        assert_eq!(goals.iter().map(|g| g.id).collect::<Vec<_>>(), vec![high_id, low_id]);
    }

    #[test]
    fn blocks_list_in_week_order() {
        let (db, user_id) = test_db();
        let programme_id = draft_programme(&db, user_id, "Meso");

        db.add_programme_block(&new_programme_block(programme_id, 5, 6, "deload")).unwrap();
        let mut hyper = new_programme_block(programme_id, 1, 4, "hypertrophy");
        hyper.notes = Some("RIR 2, add a set per week".into());
        db.add_programme_block(&hyper).unwrap();

        let blocks = db.list_programme_blocks(programme_id).unwrap();
        assert_eq!(blocks.iter().map(|b| (b.start_week, b.end_week)).collect::<Vec<_>>(), vec![(1, 4), (5, 6)]);
        assert_eq!(blocks[0].focus, "hypertrophy");
        assert_eq!(blocks[0].notes.as_deref(), Some("RIR 2, add a set per week"));
    }

    #[test]
    fn slot_grid_lists_in_order_and_rejects_duplicates() {
        let (db, user_id) = test_db();
        let programme_id = draft_programme(&db, user_id, "Grid");

        db.add_programme_slot(&new_programme_slot(programme_id, 2, 1, "lower")).unwrap();
        db.add_programme_slot(&new_programme_slot(programme_id, 1, 2, "lower")).unwrap();
        let slot = db.add_programme_slot(&new_programme_slot(programme_id, 1, 1, "upper")).unwrap();

        // The (programme, week, day) cell is unique.
        assert!(db.add_programme_slot(&new_programme_slot(programme_id, 1, 1, "upper again")).is_err());

        let slots = db.list_programme_slots(programme_id).unwrap();
        assert_eq!(slots.iter().map(|s| (s.week_idx, s.day_idx)).collect::<Vec<_>>(), vec![(1, 1), (1, 2), (2, 1)]);
        assert!(slots.iter().all(|s| s.status == SlotStatus::Pending));

        db.set_slot_status(slot, SlotStatus::Skipped).unwrap();
        assert_eq!(db.get_programme_slot(slot).unwrap().unwrap().status, SlotStatus::Skipped);
    }

    #[test]
    fn binding_a_roster_fills_its_slot() {
        let (db, user_id) = test_db();
        let programme_id = draft_programme(&db, user_id, "Programme");
        let slot_id = db.add_programme_slot(&new_programme_slot(programme_id, 1, 1, "upper")).unwrap();

        let roster_id = db.create_roster(user_id, "Week 1 day 1: upper", None, None).unwrap();
        db.bind_roster_to_slot(roster_id, slot_id).unwrap();

        assert_eq!(db.get_roster(roster_id).unwrap().unwrap().programme_slot_id, Some(slot_id));
        assert_eq!(db.get_programme_slot(slot_id).unwrap().unwrap().status, SlotStatus::Filled);
        assert_eq!(db.roster_for_slot(slot_id).unwrap().unwrap().id, roster_id);
    }

    /// An active programme with a 2×2 grid, returning (programme_id, slot ids in
    /// (week, day) order).
    fn active_programme_with_grid(db: &Database, user_id: i64) -> (i64, Vec<i64>) {
        let programme_id = draft_programme(db, user_id, "12-week hypertrophy");
        db.activate_programme(programme_id).unwrap();
        let slots = [(1, 1, "upper"), (1, 2, "lower"), (2, 1, "upper"), (2, 2, "lower")]
            .iter()
            .map(|(w, d, focus)| db.add_programme_slot(&new_programme_slot(programme_id, *w, *d, focus)).unwrap())
            .collect();
        (programme_id, slots)
    }

    #[test]
    fn next_design_slot_walks_the_grid_and_retargets_unexecuted_fills() {
        let (db, user_id) = test_db();
        let (programme_id, slots) = active_programme_with_grid(&db, user_id);

        assert_eq!(db.next_design_slot(programme_id).unwrap().unwrap().id, slots[0]);

        // A merely-drafted (or later abandoned) roster does not resolve the slot:
        // a redesign re-targets it rather than burning the next one.
        let draft = db.create_roster(user_id, "W1D1 upper", None, None).unwrap();
        db.bind_roster_to_slot(draft, slots[0]).unwrap();
        assert_eq!(db.next_design_slot(programme_id).unwrap().unwrap().id, slots[0]);

        // Executing the roster resolves the slot; design moves on.
        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(draft, session.id).unwrap();
        assert_eq!(db.next_design_slot(programme_id).unwrap().unwrap().id, slots[1]);

        // Missed and skipped slots are settled history.
        db.set_slot_status(slots[1], SlotStatus::Missed).unwrap();
        db.set_slot_status(slots[2], SlotStatus::Skipped).unwrap();
        assert_eq!(db.next_design_slot(programme_id).unwrap().unwrap().id, slots[3]);

        db.set_slot_status(slots[3], SlotStatus::Filled).unwrap();
        let last = db.create_roster(user_id, "W2D2 lower", None, None).unwrap();
        db.bind_roster_to_slot(last, slots[3]).unwrap();
        db.set_roster_status(last, LifecycleStatus::Completed).unwrap();
        assert!(db.next_design_slot(programme_id).unwrap().is_none(), "a fully resolved grid has no design slot");
    }

    #[test]
    fn training_mode_without_a_programme_is_plain_ad_hoc() {
        let (db, user_id) = test_db();
        // No programme at all, and also a draft one: neither puts the user in programme mode.
        assert!(matches!(db.training_mode_for_design(user_id, false).unwrap(), TrainingMode::AdHoc { programme: None }));
        draft_programme(&db, user_id, "still a draft");
        assert!(matches!(db.training_mode_for_design(user_id, false).unwrap(), TrainingMode::AdHoc { programme: None }));
    }

    #[test]
    fn training_mode_with_active_programme_targets_the_current_slot() {
        let (db, user_id) = test_db();
        let (programme_id, slots) = active_programme_with_grid(&db, user_id);

        match db.training_mode_for_design(user_id, false).unwrap() {
            TrainingMode::Programme { programme, slot } => {
                assert_eq!(programme.id, programme_id);
                assert_eq!(slot.id, slots[0]);
            }
            other => panic!("expected programme mode, got {other:?}"),
        }
    }

    #[test]
    fn forced_ad_hoc_reports_the_programme_but_targets_no_slot() {
        let (db, user_id) = test_db();
        let (programme_id, _) = active_programme_with_grid(&db, user_id);

        match db.training_mode_for_design(user_id, true).unwrap() {
            TrainingMode::AdHoc { programme: Some(programme) } => assert_eq!(programme.id, programme_id),
            other => panic!("expected ad-hoc-with-programme, got {other:?}"),
        }
    }

    #[test]
    fn exhausted_grid_falls_back_to_ad_hoc_with_the_programme_reported() {
        let (db, user_id) = test_db();
        let (programme_id, slots) = active_programme_with_grid(&db, user_id);
        slots.iter().for_each(|slot| db.set_slot_status(*slot, SlotStatus::Skipped).unwrap());

        match db.training_mode_for_design(user_id, false).unwrap() {
            TrainingMode::AdHoc { programme: Some(programme) } => assert_eq!(programme.id, programme_id),
            other => panic!("expected ad-hoc fallback, got {other:?}"),
        }
    }

    /// Train the slot at `slot_id`: a roster bound to it and executed against a session, which is
    /// what makes a slot count toward adherence.
    fn train_slot(db: &Database, user_id: i64, slot_id: i64) {
        let roster = db.create_roster(user_id, "Trained", None, None).unwrap();
        db.bind_roster_to_slot(roster, slot_id).unwrap();
        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(roster, session.id).unwrap();
    }

    /// [C4.3]: the context the designer prompt reads — the slot's position in the grid, the block
    /// covering its week, and adherence over everything scheduled before it.
    #[test]
    fn programme_context_reports_position_block_and_adherence() {
        let (db, user_id) = test_db();
        let programme_id = draft_programme(&db, user_id, "12-week hypertrophy");
        db.activate_programme(programme_id).unwrap();
        db.add_programme_block(&new_programme_block(programme_id, 1, 2, "hypertrophy")).unwrap();
        db.add_programme_block(&new_programme_block(programme_id, 3, 3, "deload")).unwrap();
        let slots: Vec<i64> = [(1, 1, "upper"), (1, 2, "lower"), (2, 1, "push"), (2, 2, "pull"), (3, 1, "full body")]
            .iter()
            .map(|(w, d, focus)| db.add_programme_slot(&new_programme_slot(programme_id, *w, *d, focus)).unwrap())
            .collect();

        // Week 1 fully trained; week 2 day 1 skipped, so the design lands on week 2 day 2.
        train_slot(&db, user_id, slots[0]);
        train_slot(&db, user_id, slots[1]);
        db.set_slot_status(slots[2], SlotStatus::Skipped).unwrap();

        let mode = db.training_mode_for_design(user_id, false).unwrap();
        let ctx = db.programme_context(&mode).unwrap().expect("programme mode resolves a context");
        assert_eq!((ctx.week_idx, ctx.total_weeks), (2, 3), "the span comes off the grid, not the target date");
        assert_eq!((ctx.day_idx, ctx.days_per_week), (2, 4));
        assert_eq!(ctx.slot_focus, "pull");
        assert_eq!(ctx.block.as_ref().map(|b| b.focus.as_str()), Some("hypertrophy"), "the block covering week 2");
        assert_eq!(ctx.adherence, SlotAdherence { due: 3, trained: 2, missed: 0, skipped: 1 });
    }

    /// A slot merely `filled` with a design nobody executed is not adherence — the same test
    /// `next_design_slot` applies, so the two can never disagree about what has been trained.
    #[test]
    fn a_designed_but_untrained_slot_does_not_count_as_adherence() {
        let (db, user_id) = test_db();
        let (programme_id, slots) = active_programme_with_grid(&db, user_id);
        let designed = db.create_roster(user_id, "W1D1 upper", None, None).unwrap();
        db.bind_roster_to_slot(designed, slots[0]).unwrap();
        assert_eq!(db.get_programme_slot(slots[0]).unwrap().unwrap().status, SlotStatus::Filled);

        // The design re-targets its own slot, so adherence is still the empty first-session case.
        let ctx = db.programme_context(&db.training_mode_for_design(user_id, false).unwrap()).unwrap().unwrap();
        assert_eq!(ctx.adherence, SlotAdherence::default(), "designing is not training");

        // Executing it moves both: the next slot is targeted and the first one counts.
        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(designed, session.id).unwrap();
        let ctx = db.programme_context(&db.training_mode_for_design(user_id, false).unwrap()).unwrap().unwrap();
        assert_eq!(ctx.adherence, SlotAdherence { due: 1, trained: 1, missed: 0, skipped: 0 });
        assert_eq!(programme_id, db.active_programme_for_user(user_id).unwrap().unwrap().id);
    }

    /// Every ad-hoc mode resolves no context: a one-off fills no slot, so it has no position to
    /// report and nothing to build on — including under an active programme.
    #[test]
    fn ad_hoc_modes_resolve_no_programme_context() {
        let (db, user_id) = test_db();
        assert!(db.programme_context(&db.training_mode_for_design(user_id, false).unwrap()).unwrap().is_none());

        active_programme_with_grid(&db, user_id);
        assert!(db.programme_context(&db.training_mode_for_design(user_id, true).unwrap()).unwrap().is_none());
        assert!(db.programme_context(&db.training_mode_for_design(user_id, false).unwrap()).unwrap().is_some());
    }

    #[test]
    fn ad_hoc_rosters_stay_first_class_with_no_slot() {
        let (db, user_id) = test_db();
        let roster_id = db.create_roster(user_id, "Ad-hoc push day", None, None).unwrap();

        let roster = db.get_roster(roster_id).unwrap().unwrap();
        assert_eq!(roster.programme_slot_id, None);

        // The full roster lifecycle works without any programme existing at all.
        let session = db.start_session(user_id, None).unwrap();
        db.bind_roster_to_session(roster_id, session.id).unwrap();
        assert_eq!(db.active_roster_for_user(user_id).unwrap().unwrap().programme_slot_id, None);
    }
}

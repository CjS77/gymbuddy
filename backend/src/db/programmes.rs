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
    Goal, LifecycleStatus, Programme, ProgrammeBlock, ProgrammeContext, ProgrammeDrift, ProgrammeSlot, ProgrammeStatus, ReschedulePolicy,
    SlotAdherence, SlotCounts, SlotDrift, SlotStatus, TrainingMode, today_str,
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

    // ── The missed-slot sweep ([R2.1]) ────────────────────────────────────────────

    /// Settle the slots the user has now run out of time for: every slot still
    /// `pending` whose week has *fully* passed becomes `missed`. Returns how many
    /// moved.
    ///
    /// Week `n` covers the seven days from `start_date + (n-1)*7`, so it has fully
    /// passed once `today` reaches `start_date + n*7` — the day after its last. A
    /// week still in progress is left alone: the user can train it today.
    ///
    /// Only `pending` moves. A `filled` slot has a design against it and belongs to
    /// [C4.4]'s drift question, not this one; `missed` and `skipped` are already
    /// settled. That makes the sweep idempotent, which matters because it runs on
    /// every design rather than on a schedule.
    ///
    /// Both dates go through SQLite's `date()`, so a stored `YYYY-MM-DD HH:MM:SS`
    /// and a bare `YYYY-MM-DD` compare alike. An unparseable `today` yields NULL,
    /// the comparison fails, and nothing is swept — the sweep fails closed, never
    /// marking a slot missed on the strength of a date it could not read.
    pub fn mark_missed_slots(&self, programme_id: i64, today: &str) -> anyhow::Result<usize> {
        let swept = self
            .conn()
            .execute(
                "UPDATE programme_slots SET status = 'missed', updated_at = datetime('now') \
                 WHERE programme_id = ?1 AND status = 'pending' \
                   AND date(?2) >= date((SELECT start_date FROM programmes WHERE id = ?1), '+' || (week_idx * 7) || ' days')",
                params![programme_id, today],
            )
            .context("sweeping missed programme slots")?;
        if swept > 0 {
            tracing::info!(programme_id, swept, "marked programme slots missed");
        }
        Ok(swept)
    }

    // ── Training mode ([C1.4]) ────────────────────────────────────────────────────

    /// Resolve the [`TrainingMode`] a new session design runs in. With no active
    /// programme the mode is plain ad-hoc — exactly the pre-programme behaviour.
    /// With one, the design targets the programme's next unresolved slot, unless
    /// `force_ad_hoc` records the user's explicit "leave my programme alone" (or
    /// the grid has no designable slot left): then the programme is reported but
    /// no slot is targeted, so adherence can never mutate.
    ///
    /// This is also where [`mark_missed_slots`](Self::mark_missed_slots) runs ([R2.1]).
    /// Lazily, and *before* the slot is chosen: weeks the user has run out of time for
    /// have to be settled first, or a design six weeks in would target week 1 and the
    /// programme would silently become a to-do list that never advances. Doing it here
    /// rather than on a timer keeps the whole feature free of a scheduler — the only
    /// moment a stale slot can do harm is the moment one is picked.
    pub fn training_mode_for_design(&self, user_id: i64, force_ad_hoc: bool) -> anyhow::Result<TrainingMode> {
        let Some(programme) = self.active_programme_for_user(user_id)? else {
            return Ok(TrainingMode::AdHoc { programme: None });
        };
        self.mark_missed_slots(programme.id, &today_str())?;
        if force_ad_hoc {
            return Ok(TrainingMode::AdHoc { programme: Some(programme) });
        }
        match self.next_design_slot(programme.id)? {
            Some(slot) => Ok(TrainingMode::Programme { programme, slot }),
            None => Ok(TrainingMode::AdHoc { programme: Some(programme) }),
        }
    }

    // ── Programme status ([R2.1]) ─────────────────────────────────────────────────

    /// Where a live programme has got to: the calendar week against the grid's span,
    /// the block covering it, the next session due, and how every slot has resolved.
    ///
    /// Sweeps first, so the status can never show a pending week the calendar has
    /// already closed — the read and the design path agree about what is still owed.
    pub fn programme_status(&self, programme: &Programme, today: &str) -> anyhow::Result<ProgrammeStatus> {
        self.mark_missed_slots(programme.id, today)?;

        let total_weeks = self.programme_span_weeks(programme.id)?;
        let current_week = self.weeks_elapsed(&programme.start_date, today)?.saturating_add(1).clamp(1, total_weeks.max(1));
        Ok(ProgrammeStatus {
            current_week,
            total_weeks,
            block: self.block_for_week(programme.id, current_week)?,
            next_slot: self.next_design_slot(programme.id)?,
            counts: self.slot_counts(programme.id)?,
        })
    }

    /// Whole weeks between two dates, floored, and never negative — a programme whose
    /// start date is in the future has not begun, which is week 1, not week zero.
    fn weeks_elapsed(&self, start_date: &str, today: &str) -> anyhow::Result<i32> {
        let sql = "SELECT CAST(MAX(julianday(date(?2)) - julianday(date(?1)), 0) / 7 AS INTEGER)";
        self.conn()
            .query_row(sql, params![start_date, today], |row| row.get::<_, Option<i32>>(0))
            .map(|weeks| weeks.unwrap_or(0))
            .context("Failed to read a programme's elapsed weeks")
    }

    /// Every slot of the grid in exactly one bucket. `trained` applies
    /// [`next_design_slot`](Self::next_design_slot)'s test — an executed roster, not a
    /// merely designed one — and excludes slots settled as missed or skipped, so the
    /// buckets stay disjoint and sum to the grid.
    fn slot_counts(&self, programme_id: i64) -> anyhow::Result<SlotCounts> {
        let sql = "\
            SELECT COUNT(*), \
                   COALESCE(SUM(s.status NOT IN ('missed', 'skipped') AND EXISTS (SELECT 1 FROM session_rosters r \
                       WHERE r.programme_slot_id = s.id AND r.status IN ('active', 'completed'))), 0), \
                   COALESCE(SUM(s.status = 'missed'), 0), \
                   COALESCE(SUM(s.status = 'skipped'), 0) \
            FROM programme_slots s WHERE s.programme_id = ?1";
        self.conn()
            .query_row(sql, params![programme_id], |row| {
                Ok(SlotCounts { total: row.get(0)?, trained: row.get(1)?, missed: row.get(2)?, skipped: row.get(3)? })
            })
            .context("Failed to read programme slot counts")
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

    // ── Drift detection and the reschedule policy ([C4.4]) ────────────────────────

    /// Detect drift over a live programme and recommend the reschedule that answers it.
    ///
    /// Sweeps first ([`mark_missed_slots`](Self::mark_missed_slots)), so the verdict reads the
    /// very `missed` statuses the design path and the status read do — drift is a predicate over
    /// the *marked* grid, never a second count of its own. It then groups the settled slots by
    /// their recurring `day_idx` and asks, per day, whether that day is consistently missed
    /// ([`SlotDrift::is_drifting`]). Measured over slots, not over every session: an ad-hoc
    /// roster fills no slot ([C1.4]), so it is structurally absent here and cannot mask a day the
    /// programme itself keeps missing.
    ///
    /// The recommendation is one explicit [`ReschedulePolicy`] — the point of the ticket is that
    /// this decision is named and deterministic, not emergent from a prompt.
    pub fn programme_drift(&self, programme: &Programme, today: &str) -> anyhow::Result<ProgrammeDrift> {
        self.mark_missed_slots(programme.id, today)?;
        let per_day = self.day_drift(programme.id)?;
        let drifting: Vec<SlotDrift> = per_day.iter().filter(|d| d.is_drifting()).cloned().collect();
        let recommendation = self.recommend_reschedule(programme, &per_day, &drifting, today)?;
        Ok(ProgrammeDrift { drifting, recommendation })
    }

    /// Per recurring `day_idx`, how its slots across every week have resolved — one [`SlotDrift`]
    /// row per training day of the repeating week, in day order. `focus` is the day's shared
    /// template focus (identical across the weeks it repeats in, so `MIN` returns it), and the
    /// two counts apply the same tests as [`Self::slot_counts`].
    fn day_drift(&self, programme_id: i64) -> anyhow::Result<Vec<SlotDrift>> {
        let sql = "\
            SELECT s.day_idx, MIN(s.focus), \
                   COALESCE(SUM(s.status NOT IN ('missed', 'skipped') AND EXISTS (SELECT 1 FROM session_rosters r \
                       WHERE r.programme_slot_id = s.id AND r.status IN ('active', 'completed'))), 0), \
                   COALESCE(SUM(s.status = 'missed'), 0) \
            FROM programme_slots s WHERE s.programme_id = ?1 GROUP BY s.day_idx ORDER BY s.day_idx";
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map(params![programme_id], |row| {
            Ok(SlotDrift { day_idx: row.get(0)?, focus: row.get(1)?, trained: row.get(2)?, missed: row.get(3)? })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to read per-day programme drift")
    }

    /// Choose the one reschedule that answers the detected drift, or `None` when nothing drifts.
    ///
    /// The order encodes the priority a PT would apply:
    /// 1. **Compress** when the calendar is the binding constraint — the target end date is
    ///    closing in faster than the grid's remaining weeks can be walked one at a time, so no
    ///    per-day move can make it fit; only consolidating what is left keeps the date.
    /// 2. **Shift** when a single recurring day is the whole problem and the rest of the week is
    ///    being trained — move that day, provided there is more than one day to move it among.
    /// 3. **Drop** otherwise — the user is training fewer days than the plan asks, so the
    ///    worst-adhered drifting day comes out of the weeks ahead.
    fn recommend_reschedule(
        &self,
        programme: &Programme,
        per_day: &[SlotDrift],
        drifting: &[SlotDrift],
        today: &str,
    ) -> anyhow::Result<Option<ReschedulePolicy>> {
        if drifting.is_empty() {
            return Ok(None);
        }
        if self.deadline_binds(programme, today)? {
            return Ok(Some(ReschedulePolicy::Compress));
        }
        // The worst offender: most misses, then the widest miss-over-train margin, then the
        // earliest day, so the choice is deterministic when days tie.
        let worst = drifting
            .iter()
            .max_by_key(|d| (d.missed, d.missed - d.trained, std::cmp::Reverse(d.day_idx)))
            .expect("drifting is non-empty");
        if drifting.len() == 1 && per_day.len() >= 2 {
            return Ok(Some(ReschedulePolicy::Shift { day_idx: worst.day_idx, focus: worst.focus.clone() }));
        }
        Ok(Some(ReschedulePolicy::Drop { day_idx: worst.day_idx, focus: worst.focus.clone() }))
    }

    /// Whether the target end date leaves too little calendar to finish the grid at one week per
    /// week. `None` target — an open-ended programme — never binds: there is no date to miss.
    /// Read off the grid and the calendar, so it agrees with [`Self::programme_status`]'s week.
    fn deadline_binds(&self, programme: &Programme, today: &str) -> anyhow::Result<bool> {
        let Some(target) = programme.target_end_date.as_deref() else {
            return Ok(false);
        };
        let total_weeks = self.programme_span_weeks(programme.id)?;
        let current_week = self.weeks_elapsed(&programme.start_date, today)?.saturating_add(1).clamp(1, total_weeks.max(1));
        let grid_weeks_remaining = (total_weeks - current_week).max(0);
        let weeks_left = self.weeks_elapsed(today, target)?.max(0);
        Ok(weeks_left < grid_weeks_remaining)
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

    // ── The missed-slot sweep ([R2.1]) ────────────────────────────────────────

    /// A programme whose grid starts on `start_date`, with `weeks × 2` pending slots.
    fn programme_starting(db: &Database, user_id: i64, start_date: &str, weeks: i32) -> i64 {
        let mut draft = new_programme(user_id, "Sweep", 2, "upper/lower", "linear");
        draft.start_date = start_date.into();
        let programme_id = db.create_programme(&draft).unwrap();
        db.activate_programme(programme_id).unwrap();
        (1..=weeks).for_each(|week| {
            db.add_programme_slot(&new_programme_slot(programme_id, week, 1, "upper")).unwrap();
            db.add_programme_slot(&new_programme_slot(programme_id, week, 2, "lower")).unwrap();
        });
        programme_id
    }

    fn statuses(db: &Database, programme_id: i64) -> Vec<SlotStatus> {
        db.list_programme_slots(programme_id).unwrap().iter().map(|s| s.status).collect()
    }

    /// The boundary the whole sweep turns on: week 1 covers the seven days from the
    /// start date, so it is still trainable on its last day and only settles the day
    /// after. Sweeping a day early would mark a session missed the user could still do.
    #[test]
    fn a_week_is_swept_only_once_it_has_fully_passed() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);

        // Day 7 of week 1: the last day the user can still train it.
        assert_eq!(db.mark_missed_slots(programme_id, "2026-07-07").unwrap(), 0);
        assert!(statuses(&db, programme_id).iter().all(|s| *s == SlotStatus::Pending));

        // Day 8: week 1 is over, and only week 1 goes.
        assert_eq!(db.mark_missed_slots(programme_id, "2026-07-08").unwrap(), 2);
        assert_eq!(
            statuses(&db, programme_id),
            vec![SlotStatus::Missed, SlotStatus::Missed, SlotStatus::Pending, SlotStatus::Pending, SlotStatus::Pending, SlotStatus::Pending]
        );
    }

    /// Every later week that has also elapsed goes in the same pass — a user returning
    /// after a month must not need one design per skipped week to catch the grid up.
    #[test]
    fn one_sweep_settles_every_elapsed_week_and_is_idempotent() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 4);

        assert_eq!(db.mark_missed_slots(programme_id, "2026-07-25").unwrap(), 6, "weeks 1-3 have all passed");
        assert_eq!(db.mark_missed_slots(programme_id, "2026-07-25").unwrap(), 0, "a second sweep has nothing left to do");
        assert_eq!(statuses(&db, programme_id).iter().filter(|s| **s == SlotStatus::Missed).count(), 6);
    }

    /// Only `pending` moves. `filled` is a slot with a design against it — whether that
    /// design was ever executed is [C4.4]'s drift question, not this sweep's — and
    /// `skipped` is already settled by the user's own decision.
    #[test]
    fn the_sweep_moves_pending_slots_only() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 2);
        let slots = db.list_programme_slots(programme_id).unwrap();

        // Week 1: one designed but never executed, one deliberately skipped.
        let roster = db.create_roster(user_id, "W1D1", None, None).unwrap();
        db.bind_roster_to_slot(roster, slots[0].id).unwrap();
        db.set_slot_status(slots[1].id, SlotStatus::Skipped).unwrap();

        assert_eq!(db.mark_missed_slots(programme_id, "2026-07-08").unwrap(), 0, "week 1 has no pending slot left to sweep");
        assert_eq!(statuses(&db, programme_id)[..2], [SlotStatus::Filled, SlotStatus::Skipped]);
    }

    /// A date the sweep cannot read must settle nothing. Marking a session missed is a
    /// destructive, user-visible verdict, so the failure mode has to be "do nothing".
    #[test]
    fn the_sweep_fails_closed_on_a_date_it_cannot_read() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 2);

        assert_eq!(db.mark_missed_slots(programme_id, "not a date").unwrap(), 0);
        assert_eq!(db.mark_missed_slots(programme_id, "").unwrap(), 0);
        assert!(statuses(&db, programme_id).iter().all(|s| *s == SlotStatus::Pending), "an unreadable date settles nothing");
    }

    /// The ordering the feature rests on: the sweep runs *before* the slot is chosen.
    /// Without it, a user coming back in week 4 would be handed week 1 day 1 for ever
    /// and the programme would quietly become a to-do list that never advances.
    #[test]
    fn training_mode_sweeps_stale_weeks_before_choosing_a_slot() {
        let (db, user_id) = test_db();
        let start = (chrono::Utc::now() - chrono::Duration::weeks(3)).format("%Y-%m-%d").to_string();
        let programme_id = programme_starting(&db, user_id, &start, 6);

        match db.training_mode_for_design(user_id, false).unwrap() {
            TrainingMode::Programme { slot, .. } => assert_eq!(slot.week_idx, 4, "three whole weeks have passed untrained"),
            other => panic!("expected programme mode, got {other:?}"),
        }
        assert_eq!(statuses(&db, programme_id).iter().filter(|s| **s == SlotStatus::Missed).count(), 6, "weeks 1-3 settled as missed");
    }

    /// A deliberate one-off still sweeps: the user's "leave my programme alone" is about
    /// not *filling* a slot, not about pretending the calendar stopped.
    #[test]
    fn a_forced_ad_hoc_design_still_sweeps() {
        let (db, user_id) = test_db();
        let start = (chrono::Utc::now() - chrono::Duration::weeks(2)).format("%Y-%m-%d").to_string();
        let programme_id = programme_starting(&db, user_id, &start, 4);

        assert!(matches!(db.training_mode_for_design(user_id, true).unwrap(), TrainingMode::AdHoc { programme: Some(_) }));
        assert_eq!(statuses(&db, programme_id).iter().filter(|s| **s == SlotStatus::Missed).count(), 4);
    }

    // ── Programme status ([R2.1]) ─────────────────────────────────────────────

    /// The whole status read: calendar position, the block over it, the next session due,
    /// and four counts that are disjoint and account for every slot in the grid.
    #[test]
    fn programme_status_reports_position_next_slot_and_whole_grid_counts() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 4);
        db.add_programme_block(&new_programme_block(programme_id, 1, 2, "accumulation")).unwrap();
        db.add_programme_block(&new_programme_block(programme_id, 3, 4, "intensification")).unwrap();
        let slots = db.list_programme_slots(programme_id).unwrap();

        // Week 1 trained, week 2 day 1 skipped by hand; the sweep settles the rest of week 2.
        train_slot(&db, user_id, slots[0].id);
        train_slot(&db, user_id, slots[1].id);
        db.set_slot_status(slots[2].id, SlotStatus::Skipped).unwrap();

        let programme = db.get_programme(programme_id).unwrap().unwrap();
        let status = db.programme_status(&programme, "2026-07-15").unwrap();

        assert_eq!((status.current_week, status.total_weeks), (3, 4));
        assert_eq!(status.block.as_ref().map(|b| b.focus.as_str()), Some("intensification"), "the block over the *current* week");
        let next = status.next_slot.expect("week 3 day 1 is due");
        assert_eq!((next.week_idx, next.day_idx), (3, 1));
        assert_eq!(status.counts, SlotCounts { total: 8, trained: 2, missed: 1, skipped: 1 });
        assert_eq!(status.counts.remaining(), 4, "the four buckets account for every slot");
    }

    /// Designing a session is not training it — the status has to agree with
    /// `next_design_slot`, which re-targets an unexecuted design rather than moving on.
    #[test]
    fn programme_status_does_not_count_a_designed_but_untrained_slot() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 2);
        let slots = db.list_programme_slots(programme_id).unwrap();
        let roster = db.create_roster(user_id, "W1D1", None, None).unwrap();
        db.bind_roster_to_slot(roster, slots[0].id).unwrap();

        let programme = db.get_programme(programme_id).unwrap().unwrap();
        let status = db.programme_status(&programme, "2026-07-02").unwrap();
        assert_eq!(status.counts.trained, 0, "designing is not training");
        assert_eq!(status.next_slot.map(|s| s.id), Some(slots[0].id), "and the design re-targets its own slot");
    }

    /// A programme left running past its last week still reports a week the grid has,
    /// and one whose start date is in the future has not begun rather than begun at zero.
    #[test]
    fn programme_status_clamps_the_current_week_into_the_grid() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        assert_eq!(db.programme_status(&programme, "2026-12-01").unwrap().current_week, 3, "clamped to the last week");
        assert_eq!(db.programme_status(&programme, "2026-06-01").unwrap().current_week, 1, "a programme not yet begun is week 1");
    }

    /// Every slot settled means there is nothing left to design — the signal [R4.1] reads
    /// to decide a programme is complete.
    #[test]
    fn a_fully_swept_grid_reports_no_next_slot() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 2);
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let status = db.programme_status(&programme, "2026-08-01").unwrap();
        assert!(status.next_slot.is_none(), "a settled grid has nothing due");
        assert_eq!(status.counts, SlotCounts { total: 4, trained: 0, missed: 4, skipped: 0 });
        assert_eq!(status.counts.remaining(), 0);
    }

    // ── Drift detection and the reschedule policy ([C4.4]) ─────────────────────

    /// A day trained every week it comes round is not drifting, and a kept programme needs no
    /// reschedule at all — the empty, on-track case the rest build against.
    #[test]
    fn a_day_that_is_kept_to_does_not_drift() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let slots = db.list_programme_slots(programme_id).unwrap();
        slots.iter().for_each(|s| train_slot(&db, user_id, s.id));
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert!(drift.drifting.is_empty(), "every scheduled day was trained");
        assert!(drift.is_on_track(), "a kept programme needs no reschedule");
    }

    /// One missed week of a day is a life event, not a pattern: below [`DRIFT_MIN_MISSES`] it does
    /// not drift, so the programme is left alone.
    #[test]
    fn a_single_missed_week_is_not_yet_drift() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let slots = db.list_programme_slots(programme_id).unwrap();
        // Day 1 kept throughout; day 2 kept for weeks 1-2 and missed once in week 3.
        [slots[0].id, slots[2].id, slots[4].id, slots[1].id, slots[3].id].iter().for_each(|id| train_slot(&db, user_id, *id));
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert!(drift.drifting.is_empty(), "one miss is not consistent missing");
        assert!(drift.is_on_track());
    }

    /// The card's leg-day case: one recurring day is missed week after week while the rest of the
    /// week holds. That day drifts, and the explicit answer is to *shift* it, not scold.
    #[test]
    fn a_consistently_missed_day_drifts_and_recommends_a_shift() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let slots = db.list_programme_slots(programme_id).unwrap();
        // Day 2 ("lower") trained every week; day 1 ("upper") left to lapse each week.
        [slots[1].id, slots[3].id, slots[5].id].iter().for_each(|id| train_slot(&db, user_id, *id));
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert_eq!(drift.drifting.len(), 1, "only the missed day drifts");
        assert_eq!(drift.drifting[0], SlotDrift { day_idx: 1, focus: "upper".into(), trained: 0, missed: 3 });
        assert_eq!(drift.recommendation, Some(ReschedulePolicy::Shift { day_idx: 1, focus: "upper".into() }));
    }

    /// When more than one day is being missed the user is simply training fewer days than the plan
    /// asks: no single move fixes it, so the worst-adhered day is dropped from the weeks ahead.
    #[test]
    fn broad_missing_recommends_dropping_the_worst_day() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let slots = db.list_programme_slots(programme_id).unwrap();
        // Day 1 salvaged once (missed twice); day 2 never trained (missed three times).
        train_slot(&db, user_id, slots[0].id);
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert_eq!(drift.drifting.len(), 2, "both days drift");
        assert_eq!(
            drift.recommendation,
            Some(ReschedulePolicy::Drop { day_idx: 2, focus: "lower".into() }),
            "the worse-adhered day is the one dropped"
        );
    }

    /// Compress is the calendar's answer: with a target end date closing in faster than the grid's
    /// remaining weeks can be walked one at a time, no per-day move fits it — only consolidating does.
    #[test]
    fn a_binding_target_date_recommends_compressing() {
        let (db, user_id) = test_db();
        let mut draft = new_programme(user_id, "Deadline", 2, "upper/lower", "linear");
        draft.start_date = "2026-07-01".into();
        draft.target_end_date = Some("2026-07-29".into());
        let programme_id = db.create_programme(&draft).unwrap();
        db.activate_programme(programme_id).unwrap();
        (1..=8).for_each(|week| {
            db.add_programme_slot(&new_programme_slot(programme_id, week, 1, "upper")).unwrap();
            db.add_programme_slot(&new_programme_slot(programme_id, week, 2, "lower")).unwrap();
        });
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        // Weeks 1-3 have elapsed untrained; five grid weeks remain but only one calendar week does.
        let drift = db.programme_drift(&programme, "2026-07-22").unwrap();
        assert!(!drift.drifting.is_empty(), "the elapsed weeks drift");
        assert_eq!(drift.recommendation, Some(ReschedulePolicy::Compress), "the deadline binds, so no day-move can help");
    }

    /// A deliberately skipped day is the user's own call, not drift: it counts as neither trained
    /// nor missed, so a day skipped every week never triggers a reschedule.
    #[test]
    fn a_deliberately_skipped_day_is_not_drift() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let slots = db.list_programme_slots(programme_id).unwrap();
        // Day 2 trained; day 1 skipped by hand every week.
        [slots[1].id, slots[3].id, slots[5].id].iter().for_each(|id| train_slot(&db, user_id, *id));
        [slots[0].id, slots[2].id, slots[4].id].iter().for_each(|id| db.set_slot_status(*id, SlotStatus::Skipped).unwrap());
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert!(drift.is_on_track(), "skipping on purpose is not drifting");
    }

    /// Drift is measured over *slots*, not over every session ([C1.4]): ad-hoc training under an
    /// active programme fills no slot, so it cannot paper over a programme day the user keeps missing.
    #[test]
    fn drift_reads_slots_not_ad_hoc_sessions() {
        let (db, user_id) = test_db();
        let programme_id = programme_starting(&db, user_id, "2026-07-01", 3);
        let slots = db.list_programme_slots(programme_id).unwrap();
        [slots[1].id, slots[3].id, slots[5].id].iter().for_each(|id| train_slot(&db, user_id, *id));

        // Plenty of ad-hoc work — rosters bound to a session but to no slot.
        (0..3).for_each(|_| {
            let roster = db.create_roster(user_id, "Ad-hoc", None, None).unwrap();
            let session = db.start_session(user_id, None).unwrap();
            db.bind_roster_to_session(roster, session.id).unwrap();
        });
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert_eq!(drift.recommendation, Some(ReschedulePolicy::Shift { day_idx: 1, focus: "upper".into() }));
        assert_eq!(drift.drifting[0].missed, 3, "ad-hoc sessions do not count toward the missed programme day");
    }

    /// A one-day-a-week programme has nowhere to shift a missed day to, so a consistently missed
    /// single day drops rather than shifts.
    #[test]
    fn a_single_day_week_drops_rather_than_shifts() {
        let (db, user_id) = test_db();
        let mut draft = new_programme(user_id, "Once a week", 1, "full body", "linear");
        draft.start_date = "2026-07-01".into();
        let programme_id = db.create_programme(&draft).unwrap();
        db.activate_programme(programme_id).unwrap();
        (1..=3).for_each(|week| {
            db.add_programme_slot(&new_programme_slot(programme_id, week, 1, "full body")).unwrap();
        });
        let programme = db.get_programme(programme_id).unwrap().unwrap();

        let drift = db.programme_drift(&programme, "2026-07-25").unwrap();
        assert_eq!(drift.drifting.len(), 1);
        assert_eq!(
            drift.recommendation,
            Some(ReschedulePolicy::Drop { day_idx: 1, focus: "full body".into() }),
            "with one training day there is nowhere to shift it"
        );
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

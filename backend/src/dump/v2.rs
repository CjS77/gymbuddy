//! Reader for schema v2 — the SessionRoster schema.
//!
//! The v1 reader translates on the way out; this one does not have to, because the dump format
//! *is* v2 vocabulary. What it does have to be is **exhaustive in the same way**: the two readers
//! emit the identical envelope for the identical data, and `gymbuddy migrate --verify` calls any
//! difference between an export and a re-export a bug. A column this reader forgets is a column
//! the verifier cannot see.
//!
//! Like [`super::v1`], it talks raw SQL rather than going through `db/`. A DAO returns the shape
//! the application wants; an exporter needs the shape the *table* has, including the columns no
//! feature reads yet.
//!
//! Ordering mirrors the v1 reader column for column, so a v1 dump and a v2 dump of the same data
//! come out in the same order and the structural compare has less to normalise away.

use std::collections::HashMap;

use anyhow::Context as _;
use rusqlite::Connection;

use super::model::*;

/// Read an entire v2 database into the dump envelope.
pub fn read(conn: &Connection, source: SourceSchema, exported_at: String) -> anyhow::Result<Dump> {
    let reader = V2Reader::new(conn)?;
    Ok(Dump {
        format: DUMP_FORMAT.to_string(),
        dump_version: DUMP_VERSION,
        source_schema: source,
        exported_at,
        groups: reader.groups()?,
        users: reader.users()?,
    })
}

/// Holds the catalogue indexes the row readers need, so name resolution is a map lookup rather
/// than a join repeated on every row.
struct V2Reader<'a> {
    conn: &'a Connection,
    exercises: HashMap<i64, ExerciseRef>,
    measurements: HashMap<i64, String>,
    metrics: HashMap<i64, String>,
}

impl<'a> V2Reader<'a> {
    fn new(conn: &'a Connection) -> anyhow::Result<Self> {
        Ok(Self {
            conn,
            exercises: exercise_index(conn)?,
            measurements: name_index(conn, "measurement_types")?,
            metrics: name_index(conn, "metrics")?,
        })
    }

    /// Catalogue id → canonical name + parent name. Ids never leave this module.
    fn exercise(&self, id: i64) -> anyhow::Result<ExerciseRef> {
        self.exercises.get(&id).cloned().with_context(|| format!("exercise_types row {id} is referenced but missing"))
    }

    fn measurement(&self, id: i64) -> anyhow::Result<String> {
        self.measurements.get(&id).cloned().with_context(|| format!("measurement_types row {id} is referenced but missing"))
    }

    fn metric(&self, id: i64) -> anyhow::Result<String> {
        self.metrics.get(&id).cloned().with_context(|| format!("metrics row {id} is referenced but missing"))
    }

    fn groups(&self) -> anyhow::Result<Vec<Group>> {
        let mut stmt = self.conn.prepare("SELECT name, description, created_at FROM groups ORDER BY id")?;
        let groups = stmt
            .query_map([], |row| Ok(Group { name: row.get(0)?, description: row.get(1)?, created_at: row.get(2)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(groups)
    }

    fn users(&self) -> anyhow::Result<Vec<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, telegram_id, timezone, beta_tester, pubkey, timers_enabled, created_at, updated_at \
             FROM users ORDER BY id",
        )?;
        let rows = stmt.query_map([], UserRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|row| self.user(row)).collect()
    }

    fn user(&self, row: UserRow) -> anyhow::Result<User> {
        let id = row.id;
        Ok(User {
            id,
            name: row.name,
            telegram_id: row.telegram_id,
            pubkey: row.pubkey,
            timezone: row.timezone,
            beta_tester: row.beta_tester,
            timers_enabled: row.timers_enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
            group_memberships: self.group_memberships(id)?,
            philosophies: self.philosophies(id)?,
            interview_states: self.interview_states(id)?,
            goals: self.goals(id)?,
            sessions: self.sessions(id)?,
            unsessioned_entries: self.unsessioned_entries(id)?,
            session_rosters: self.session_rosters(id)?,
            programmes: self.programmes(id)?,
            health_entries: self.health_entries(id)?,
            body_metrics: self.body_metrics(id)?,
            conversation_history: self.conversation_history(id)?,
            session_reviews: self.session_reviews(id)?,
            // Schema v2 has none of it: no `signal_id` column, no `schedules` table, and no
            // sentinel in `intent`. An empty block is the correct answer, not a missing one.
            legacy: Legacy::default(),
        })
    }

    fn group_memberships(&self, user_id: i64) -> anyhow::Result<Vec<GroupMembership>> {
        let mut stmt = self.conn.prepare(
            "SELECT g.name, m.level, m.granted_at FROM group_members m JOIN groups g ON g.id = m.group_id \
             WHERE m.user_id = ?1 ORDER BY g.id",
        )?;
        let rows = stmt
            .query_map([user_id], |row| Ok(GroupMembership { group: row.get(0)?, level: row.get(1)?, granted_at: row.get(2)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn philosophies(&self, user_id: i64) -> anyhow::Result<Vec<Philosophy>> {
        let mut stmt = self.conn.prepare("SELECT id, content, source, created_at FROM philosophies WHERE user_id = ?1 ORDER BY id")?;
        let rows = stmt
            .query_map([user_id], |row| {
                Ok(Philosophy { id: row.get(0)?, content: row.get(1)?, source: row.get(2)?, created_at: row.get(3)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn interview_states(&self, user_id: i64) -> anyhow::Result<Vec<InterviewState>> {
        let mut stmt = self
            .conn
            .prepare("SELECT platform, mode, draft, turns, started_at FROM interview_states WHERE user_id = ?1 ORDER BY platform")?;
        let rows = stmt
            .query_map([user_id], |row| {
                Ok(InterviewState {
                    platform: row.get(0)?,
                    mode: row.get(1)?,
                    draft: row.get(2)?,
                    turns: row.get(3)?,
                    started_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn goals(&self, user_id: i64) -> anyhow::Result<Vec<Goal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, exercise_type_id, metric_id, target_value, direction, priority, start_date, target_date, \
                    achieved, achieved_at, notes, created_at, updated_at \
             FROM goals WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([user_id], GoalRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|row| self.goal(row)).collect()
    }

    fn goal(&self, row: GoalRow) -> anyhow::Result<Goal> {
        Ok(Goal {
            id: row.id,
            kind: row.kind,
            exercise: row.exercise_type_id.map(|id| self.exercise(id)).transpose()?,
            metric: row.metric_id.map(|id| self.metric(id)).transpose()?,
            target_value: row.target_value,
            direction: row.direction,
            priority: row.priority,
            start_date: row.start_date,
            target_date: row.target_date,
            achieved: row.achieved,
            achieved_at: row.achieved_at,
            notes: row.notes,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    fn sessions(&self, user_id: i64) -> anyhow::Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, intent, overall_effort, effort_source, felt, cut_short, cut_short_reason \
             FROM sessions WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([user_id], SessionRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|row| self.session(row)).collect()
    }

    fn session(&self, row: SessionRow) -> anyhow::Result<Session> {
        Ok(Session {
            id: row.id,
            started_at: row.started_at,
            ended_at: row.ended_at,
            intent: row.intent,
            overall_effort: row.overall_effort,
            effort_source: row.effort_source,
            felt: row.felt,
            cut_short: row.cut_short,
            cut_short_reason: row.cut_short_reason,
            entries: self.entries("session_id = ?1", row.id)?,
        })
    }

    /// Entries logged outside any session (`session_id IS NULL`) — keyed by user instead.
    fn unsessioned_entries(&self, user_id: i64) -> anyhow::Result<Vec<ExerciseEntry>> {
        self.entries("session_id IS NULL AND user_id = ?1", user_id)
    }

    fn entries(&self, predicate: &str, key: i64) -> anyhow::Result<Vec<ExerciseEntry>> {
        let sql = format!(
            "SELECT id, start_timestamp, end_timestamp, comment FROM exercise_entries WHERE {predicate} ORDER BY start_timestamp, id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([key], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, start_timestamp, end_timestamp, comment)| {
                Ok(ExerciseEntry { id, start_timestamp, end_timestamp, comment, sets: self.sets(id)? })
            })
            .collect()
    }

    fn sets(&self, entry_id: i64) -> anyhow::Result<Vec<Set>> {
        let mut stmt = self.conn.prepare(
            "SELECT exercise_type_id, order_idx, measurement_type_id, count, value, perceived_difficulty, comment, logged_at \
             FROM sets WHERE exercise_entry_id = ?1 ORDER BY order_idx, id",
        )?;
        let rows = stmt.query_map([entry_id], SetRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|row| self.set(row)).collect()
    }

    fn set(&self, row: SetRow) -> anyhow::Result<Set> {
        Ok(Set {
            exercise: self.exercise(row.exercise_type_id)?,
            order_idx: row.order_idx,
            measurement_type: self.measurement(row.measurement_type_id)?,
            count: row.count,
            value: row.value,
            perceived_difficulty: row.perceived_difficulty,
            comment: row.comment,
            logged_at: row.logged_at,
        })
    }

    fn session_rosters(&self, user_id: i64) -> anyhow::Result<Vec<SessionRoster>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, rationale, philosophy_id, status, session_id, programme_slot_id, override_note, created_at, updated_at \
             FROM session_rosters WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([user_id], RosterRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|row| self.session_roster(row)).collect()
    }

    fn session_roster(&self, row: RosterRow) -> anyhow::Result<SessionRoster> {
        Ok(SessionRoster {
            id: row.id,
            title: row.title,
            rationale: row.rationale,
            philosophy_id: row.philosophy_id,
            status: row.status,
            session_id: row.session_id,
            programme_slot_id: row.programme_slot_id,
            override_note: row.override_note,
            created_at: row.created_at,
            updated_at: row.updated_at,
            exercises: self.roster_exercises(row.id)?,
        })
    }

    fn roster_exercises(&self, roster_id: i64) -> anyhow::Result<Vec<RosterExercise>> {
        let mut stmt = self.conn.prepare(
            "SELECT exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes \
             FROM roster_exercises WHERE roster_id = ?1 ORDER BY order_idx, id",
        )?;
        let rows = stmt.query_map([roster_id], RosterExerciseRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|row| {
                Ok(RosterExercise {
                    exercise: self.exercise(row.exercise_type_id)?,
                    order_idx: row.order_idx,
                    target_sets: row.target_sets,
                    target_reps: row.target_reps,
                    target_weight_kg: row.target_weight_kg,
                    target_secs: row.target_secs,
                    notes: row.notes,
                })
            })
            .collect()
    }

    fn programmes(&self, user_id: i64) -> anyhow::Result<Vec<Programme>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, start_date, target_end_date, days_per_week, split, progression_policy, status, created_at, updated_at \
             FROM programmes WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([user_id], ProgrammeRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|row| self.programme(row)).collect()
    }

    fn programme(&self, row: ProgrammeRow) -> anyhow::Result<Programme> {
        Ok(Programme {
            id: row.id,
            title: row.title,
            start_date: row.start_date,
            target_end_date: row.target_end_date,
            days_per_week: row.days_per_week,
            split: row.split,
            progression_policy: row.progression_policy,
            status: row.status,
            created_at: row.created_at,
            updated_at: row.updated_at,
            goal_ids: self.programme_goal_ids(row.id)?,
            blocks: self.programme_blocks(row.id)?,
            slots: self.programme_slots(row.id)?,
        })
    }

    fn programme_goal_ids(&self, programme_id: i64) -> anyhow::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare("SELECT goal_id FROM programme_goals WHERE programme_id = ?1 ORDER BY goal_id")?;
        let ids = stmt.query_map([programme_id], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    fn programme_blocks(&self, programme_id: i64) -> anyhow::Result<Vec<ProgrammeBlock>> {
        let mut stmt = self
            .conn
            .prepare("SELECT start_week, end_week, focus, notes FROM programme_blocks WHERE programme_id = ?1 ORDER BY start_week, id")?;
        let rows = stmt
            .query_map([programme_id], |row| {
                Ok(ProgrammeBlock { start_week: row.get(0)?, end_week: row.get(1)?, focus: row.get(2)?, notes: row.get(3)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn programme_slots(&self, programme_id: i64) -> anyhow::Result<Vec<ProgrammeSlot>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, week_idx, day_idx, focus, status, updated_at FROM programme_slots WHERE programme_id = ?1 \
             ORDER BY week_idx, day_idx",
        )?;
        let rows = stmt
            .query_map([programme_id], |row| {
                Ok(ProgrammeSlot {
                    id: row.get(0)?,
                    week_idx: row.get(1)?,
                    day_idx: row.get(2)?,
                    focus: row.get(3)?,
                    status: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn health_entries(&self, user_id: i64) -> anyhow::Result<Vec<HealthEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_type, body_part, severity, description, started_at, resolved_at, notes, updated_at \
             FROM health_entries WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map([user_id], |row| {
                Ok(HealthEntry {
                    entry_type: row.get(0)?,
                    body_part: row.get(1)?,
                    severity: row.get(2)?,
                    description: row.get(3)?,
                    started_at: row.get(4)?,
                    resolved_at: row.get(5)?,
                    notes: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn body_metrics(&self, user_id: i64) -> anyhow::Result<Vec<BodyMetric>> {
        let mut stmt = self.conn.prepare(
            "SELECT metric_id, value, measured_at FROM body_metrics WHERE user_id = ?1 ORDER BY measured_at, id",
        )?;
        let rows = stmt
            .query_map([user_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?, row.get::<_, String>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(metric_id, value, measured_at)| Ok(BodyMetric { metric: self.metric(metric_id)?, value, measured_at }))
            .collect()
    }

    fn conversation_history(&self, user_id: i64) -> anyhow::Result<Vec<ConversationMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT platform, role, content, timestamp, exclude_from_context \
             FROM conversation_history WHERE user_id = ?1 ORDER BY timestamp, id",
        )?;
        let rows = stmt
            .query_map([user_id], |row| {
                Ok(ConversationMessage {
                    platform: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    timestamp: row.get(3)?,
                    exclude_from_context: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// v2-only. `body` is opaque JSON and travels as the string the column holds — reserialising it
    /// would rewrite a snapshot of what was true when it was generated.
    fn session_reviews(&self, user_id: i64) -> anyhow::Result<Vec<SessionReview>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id, roster_id, kind, body, created_at FROM session_reviews WHERE user_id = ?1 ORDER BY id")?;
        let rows = stmt
            .query_map([user_id], |row| {
                Ok(SessionReview {
                    session_id: row.get(0)?,
                    roster_id: row.get(1)?,
                    kind: row.get(2)?,
                    body: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn exercise_index(conn: &Connection) -> anyhow::Result<HashMap<i64, ExerciseRef>> {
    let mut stmt = conn.prepare("SELECT id, name, parent_id FROM exercise_types")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<i64>>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let names: HashMap<i64, String> = rows.iter().map(|(id, name, _)| (*id, name.clone())).collect();
    let index = rows
        .iter()
        .map(|(id, name, parent_id)| {
            let parent = parent_id.and_then(|parent_id| names.get(&parent_id).cloned());
            (*id, ExerciseRef { name: name.clone(), parent })
        })
        .collect();
    Ok(index)
}

/// `id → name` over any `(id, name)` reference table.
fn name_index(conn: &Connection, table: &str) -> anyhow::Result<HashMap<i64, String>> {
    let mut stmt = conn.prepare(&format!("SELECT id, name FROM {table}"))?;
    let index = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(index)
}

// -----------------------------------------------------------------------------------------------
// Raw row structs. Reading a row and translating it are separate steps because translation can fail
// (an exercise id with no catalogue row) while `query_map`'s closure may only return
// `rusqlite::Error`.
// -----------------------------------------------------------------------------------------------

struct UserRow {
    id: i64,
    name: String,
    telegram_id: Option<String>,
    timezone: String,
    beta_tester: bool,
    pubkey: Option<String>,
    timers_enabled: bool,
    created_at: String,
    updated_at: String,
}

impl UserRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            telegram_id: row.get(2)?,
            timezone: row.get(3)?,
            beta_tester: row.get(4)?,
            pubkey: row.get(5)?,
            timers_enabled: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }
}

struct GoalRow {
    id: i64,
    kind: String,
    exercise_type_id: Option<i64>,
    metric_id: Option<i64>,
    target_value: f64,
    direction: String,
    priority: i64,
    start_date: String,
    target_date: Option<String>,
    achieved: bool,
    achieved_at: Option<String>,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl GoalRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            kind: row.get(1)?,
            exercise_type_id: row.get(2)?,
            metric_id: row.get(3)?,
            target_value: row.get(4)?,
            direction: row.get(5)?,
            priority: row.get(6)?,
            start_date: row.get(7)?,
            target_date: row.get(8)?,
            achieved: row.get(9)?,
            achieved_at: row.get(10)?,
            notes: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        })
    }
}

struct SessionRow {
    id: i64,
    started_at: String,
    ended_at: Option<String>,
    intent: Option<String>,
    overall_effort: Option<String>,
    effort_source: Option<String>,
    felt: Option<String>,
    cut_short: bool,
    cut_short_reason: Option<String>,
}

impl SessionRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            started_at: row.get(1)?,
            ended_at: row.get(2)?,
            intent: row.get(3)?,
            overall_effort: row.get(4)?,
            effort_source: row.get(5)?,
            felt: row.get(6)?,
            cut_short: row.get(7)?,
            cut_short_reason: row.get(8)?,
        })
    }
}

struct SetRow {
    exercise_type_id: i64,
    order_idx: i64,
    measurement_type_id: i64,
    count: Option<i64>,
    value: f64,
    perceived_difficulty: String,
    comment: Option<String>,
    logged_at: String,
}

impl SetRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            exercise_type_id: row.get(0)?,
            order_idx: row.get(1)?,
            measurement_type_id: row.get(2)?,
            count: row.get(3)?,
            value: row.get(4)?,
            perceived_difficulty: row.get(5)?,
            comment: row.get(6)?,
            logged_at: row.get(7)?,
        })
    }
}

struct RosterRow {
    id: i64,
    title: String,
    rationale: Option<String>,
    philosophy_id: Option<i64>,
    status: String,
    session_id: Option<i64>,
    programme_slot_id: Option<i64>,
    override_note: Option<String>,
    created_at: String,
    updated_at: String,
}

impl RosterRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            title: row.get(1)?,
            rationale: row.get(2)?,
            philosophy_id: row.get(3)?,
            status: row.get(4)?,
            session_id: row.get(5)?,
            programme_slot_id: row.get(6)?,
            override_note: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }
}

struct RosterExerciseRow {
    exercise_type_id: i64,
    order_idx: i64,
    target_sets: Option<i64>,
    target_reps: Option<i64>,
    target_weight_kg: Option<f64>,
    target_secs: Option<i64>,
    notes: Option<String>,
}

impl RosterExerciseRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            exercise_type_id: row.get(0)?,
            order_idx: row.get(1)?,
            target_sets: row.get(2)?,
            target_reps: row.get(3)?,
            target_weight_kg: row.get(4)?,
            target_secs: row.get(5)?,
            notes: row.get(6)?,
        })
    }
}

struct ProgrammeRow {
    id: i64,
    title: String,
    start_date: String,
    target_end_date: Option<String>,
    days_per_week: i64,
    split: String,
    progression_policy: String,
    status: String,
    created_at: String,
    updated_at: String,
}

impl ProgrammeRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            title: row.get(1)?,
            start_date: row.get(2)?,
            target_end_date: row.get(3)?,
            days_per_week: row.get(4)?,
            split: row.get(5)?,
            progression_policy: row.get(6)?,
            status: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }
}

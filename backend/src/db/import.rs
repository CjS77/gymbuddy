//! Load a dump into a fresh schema v2 database.
//!
//! This is the second half of `gymbuddy migrate`, and the only code path that will ever write a
//! real user's history into the new schema. Three rules follow from that:
//!
//! **It refuses a database that already holds data.** Importing into a populated database would
//! merge two id spaces with no way to tell the halves apart afterwards. Reference data (the
//! exercise catalogue, `measurement_types`, `metrics`) does not count as data — a v2 database is
//! never empty of it, since `Database::open` seeds it.
//!
//! **It writes every timestamp verbatim.** Nothing here uses `datetime('now')`; every INSERT names
//! its `created_at` / `updated_at` / `logged_at` column explicitly and binds the string the dump
//! carries. A migration that restamps rows with the migration date destroys the history it exists
//! to preserve, and — because `--verify` compares timestamps exactly — would also fail loud.
//!
//! **It fails loud on an unknown exercise.** A v1 database can only contain names the v1
//! migrations seeded, and schema v2's catalogue is a superset of those. A name that does not
//! resolve is therefore a bug in the catalogue or the reader, not a stray row to skip. Skipping
//! would drop the user's sets while reporting success.
//!
//! # Why it lives in `db/`
//!
//! `Database::conn` is `pub(in crate::db)` on purpose (see [`super::database`]). The importer
//! needs raw INSERTs with explicit ids and explicit timestamps — precisely what the DAO methods
//! refuse to offer, since every one of them stamps its own `created_at`. Rather than widen the
//! connection's visibility for one caller, the importer moves inside the boundary.
//!
//! # Id translation
//!
//! Dump ids are *source* ids and are never inserted verbatim. Each user tree is imported in
//! dependency order — sessions, philosophies, goals, programmes, slots, rosters — and each step
//! records `source id → new id` in a map the later steps resolve their references through. A
//! reference that cannot be resolved is an error, not a NULL: a roster silently detached from its
//! session is exactly the kind of quiet loss this whole module exists to prevent.

use std::collections::{BTreeMap, HashMap};

use anyhow::Context as _;
use rusqlite::{Connection, params};

use super::database::Database;
use crate::dump::model::*;

/// The user-data tables, paired with the dump collection whose rows land in them.
///
/// Two jobs, and it is deliberately one list for both: deciding whether a target database is empty,
/// and counting what an import actually wrote. A table missing here would be neither checked before
/// the import nor counted after it — so `import_row_counts` is reconciled against the dump's own
/// [`crate::dump::RowCounts`], and [`crate::dump::counts::COLLECTIONS`] is pinned against this list
/// by a test.
const USER_DATA_TABLES: &[(&str, &str)] = &[
    ("users", "users"),
    ("groups", "groups"),
    ("group_memberships", "group_members"),
    ("philosophies", "philosophies"),
    ("interview_states", "interview_states"),
    ("goals", "goals"),
    ("sessions", "sessions"),
    ("exercise_entries", "exercise_entries"),
    ("sets", "sets"),
    ("session_rosters", "session_rosters"),
    ("roster_exercises", "roster_exercises"),
    ("programmes", "programmes"),
    ("programme_goals", "programme_goals"),
    ("programme_blocks", "programme_blocks"),
    ("programme_slots", "programme_slots"),
    ("health_entries", "health_entries"),
    ("body_metrics", "body_metrics"),
    ("conversation_history", "conversation_history"),
    ("session_reviews", "session_reviews"),
];

impl Database {
    /// Import `dump` into this database, which must hold no user data.
    ///
    /// All or nothing: the whole import runs in one transaction, so a failure half way through
    /// leaves the target exactly as it was rather than half-migrated.
    pub fn import_dump(&self, dump: &Dump) -> anyhow::Result<()> {
        self.ensure_empty()?;
        let tx = self.conn().unchecked_transaction()?;
        {
            let importer = Importer::new(self, &tx)?;
            importer.groups(&dump.groups)?;
            dump.users.iter().try_for_each(|user| importer.user(user))?;
        }
        tx.commit().context("committing the import")?;
        Ok(())
    }

    /// Rows per user-data table, keyed by the dump collection each corresponds to.
    ///
    /// The other half of the count invariant: [`crate::dump::RowCounts`] says what the dump
    /// carried, this says what the database now holds, and `migrate --verify` asserts they agree.
    /// Neither is derived from the other, so agreeing is evidence rather than a tautology.
    pub fn import_row_counts(&self) -> anyhow::Result<BTreeMap<&'static str, usize>> {
        USER_DATA_TABLES
            .iter()
            .map(|(collection, table)| Ok((*collection, self.count_table(table)?)))
            .collect::<anyhow::Result<BTreeMap<_, _>>>()
    }

    fn count_table(&self, table: &str) -> anyhow::Result<usize> {
        let count: i64 = self
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .with_context(|| format!("counting rows in `{table}`"))?;
        Ok(count as usize)
    }

    /// Refuse a target that already holds user data, naming the table that is not empty.
    fn ensure_empty(&self) -> anyhow::Result<()> {
        let populated = USER_DATA_TABLES
            .iter()
            .map(|(_, table)| Ok((*table, self.count_table(table)?)))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(table, count)| format!("{table} ({count})"))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            populated.is_empty(),
            "refusing to import into a database that already holds data: {}.\n\
             Import targets a fresh database — point --db at a path that does not exist yet.",
            populated.join(", ")
        );
        Ok(())
    }
}

/// Per-user id translation maps, rebuilt for each user tree.
///
/// Scoped to one user because the dump's references are: a roster points at a session in the same
/// tree, never another user's. Rebuilding per user also means a source id colliding across users
/// cannot silently resolve to the wrong row.
#[derive(Default)]
struct IdMaps {
    sessions: HashMap<i64, i64>,
    philosophies: HashMap<i64, i64>,
    goals: HashMap<i64, i64>,
    slots: HashMap<i64, i64>,
    rosters: HashMap<i64, i64>,
}

fn resolve(map: &HashMap<i64, i64>, source_id: i64, what: &str) -> anyhow::Result<i64> {
    map.get(&source_id).copied().with_context(|| format!("{what} {source_id} is referenced but is not in the dump"))
}

struct Importer<'a> {
    tx: &'a Connection,
    db: &'a Database,
    /// `(lowercased name, lowercased parent name)` → catalogue id. Lowercased because
    /// `exercise_types.name` is `COLLATE NOCASE`, so the target treats the two spellings as one row
    /// and the lookup must agree.
    exercises: HashMap<(String, Option<String>), i64>,
    measurements: HashMap<String, i64>,
    /// Group name → id, filled in as the global groups are inserted.
    groups: std::cell::RefCell<HashMap<String, i64>>,
}

impl<'a> Importer<'a> {
    fn new(db: &'a Database, tx: &'a Connection) -> anyhow::Result<Self> {
        Ok(Self {
            tx,
            db,
            exercises: exercise_index(tx)?,
            measurements: measurement_index(tx)?,
            groups: std::cell::RefCell::new(HashMap::new()),
        })
    }

    /// Resolve an [`ExerciseRef`] against the v2 catalogue, or fail.
    ///
    /// Never returns `None` and never skips the row: see the module doc. The error names both
    /// halves of the pair, because "Squat" alone does not say which parent failed to match.
    fn exercise(&self, reference: &ExerciseRef) -> anyhow::Result<i64> {
        let key = (reference.name.to_lowercase(), reference.parent.as_ref().map(|parent| parent.to_lowercase()));
        self.exercises.get(&key).copied().with_context(|| {
            let parent = reference.parent.as_deref().unwrap_or("<none>");
            format!(
                "exercise `{}` (parent `{parent}`) is not in the schema v2 catalogue. A dump can only name exercises the \
                 catalogue seeded, so this is a catalogue or reader bug — refusing rather than dropping the row.",
                reference.name
            )
        })
    }

    fn measurement(&self, name: &str) -> anyhow::Result<i64> {
        self.measurements.get(name).copied().with_context(|| format!("measurement type `{name}` is not in schema v2"))
    }

    fn groups(&self, groups: &[Group]) -> anyhow::Result<()> {
        groups.iter().try_for_each(|group| {
            self.tx
                .execute(
                    "INSERT INTO groups (name, description, created_at) VALUES (?1, ?2, ?3)",
                    params![group.name, group.description, group.created_at],
                )
                .with_context(|| format!("inserting group `{}`", group.name))?;
            self.groups.borrow_mut().insert(group.name.clone(), self.tx.last_insert_rowid());
            Ok(())
        })
    }

    /// One whole user tree, in dependency order.
    fn user(&self, user: &User) -> anyhow::Result<()> {
        let user_id = self.insert_user(user)?;
        let mut maps = IdMaps::default();
        self.group_memberships(user_id, user)?;
        self.philosophies(user_id, user, &mut maps)?;
        self.goals(user_id, user, &mut maps)?;
        self.sessions(user_id, user, &mut maps)?;
        self.unsessioned_entries(user_id, user)?;
        self.programmes(user_id, user, &mut maps)?;
        self.session_rosters(user_id, user, &mut maps)?;
        self.session_reviews(user_id, user, &maps)?;
        self.interview_states(user_id, user)?;
        self.health_entries(user_id, user)?;
        self.body_metrics(user_id, user)?;
        self.conversation_history(user_id, user)?;
        // `user.legacy` is deliberately not imported: schema v2 has nowhere to put `signal_id` or
        // `schedules`, and the dump keeps them so the file remains a complete backup of its source.
        Ok(())
    }

    fn insert_user(&self, user: &User) -> anyhow::Result<i64> {
        self.tx
            .execute(
                "INSERT INTO users (name, telegram_id, pubkey, timezone, beta_tester, timers_enabled, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    user.name,
                    user.telegram_id,
                    user.pubkey,
                    user.timezone,
                    user.beta_tester,
                    user.timers_enabled,
                    user.created_at,
                    user.updated_at
                ],
            )
            .with_context(|| format!("inserting user `{}`", user.name))?;
        Ok(self.tx.last_insert_rowid())
    }

    fn group_memberships(&self, user_id: i64, user: &User) -> anyhow::Result<()> {
        user.group_memberships.iter().try_for_each(|membership| {
            let group_id = self
                .groups
                .borrow()
                .get(&membership.group)
                .copied()
                .with_context(|| format!("group `{}` is referenced by a membership but is not in the dump", membership.group))?;
            self.tx
                .execute(
                    "INSERT INTO group_members (user_id, group_id, level, granted_at) VALUES (?1, ?2, ?3, ?4)",
                    params![user_id, group_id, membership.level, membership.granted_at],
                )
                .with_context(|| format!("inserting membership of `{}`", membership.group))?;
            Ok(())
        })
    }

    fn philosophies(&self, user_id: i64, user: &User, maps: &mut IdMaps) -> anyhow::Result<()> {
        user.philosophies.iter().try_for_each(|philosophy| {
            self.tx
                .execute(
                    "INSERT INTO philosophies (user_id, content, source, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![user_id, philosophy.content, philosophy.source, philosophy.created_at],
                )
                .context("inserting a philosophy")?;
            maps.philosophies.insert(philosophy.id, self.tx.last_insert_rowid());
            Ok(())
        })
    }

    fn interview_states(&self, user_id: i64, user: &User) -> anyhow::Result<()> {
        user.interview_states.iter().try_for_each(|state| {
            self.tx
                .execute(
                    "INSERT INTO interview_states (user_id, platform, mode, draft, turns, started_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![user_id, state.platform, state.mode, state.draft, state.turns, state.started_at],
                )
                .with_context(|| format!("inserting the {} interview state", state.platform))?;
            Ok(())
        })
    }

    /// Goals, resolving the metric name to a `metrics` row — creating it if the target has never
    /// seen that name. The seeded metrics are already there with their units; one the user invented
    /// arrives here with a NULL unit, exactly as it would have on first use.
    fn goals(&self, user_id: i64, user: &User, maps: &mut IdMaps) -> anyhow::Result<()> {
        user.goals.iter().try_for_each(|goal| {
            let exercise_id = goal.exercise.as_ref().map(|reference| self.exercise(reference)).transpose()?;
            let metric_id = goal.metric.as_deref().map(|name| self.db.get_or_create_metric(name)).transpose()?;
            self.tx
                .execute(
                    "INSERT INTO goals (user_id, kind, exercise_type_id, metric_id, target_value, direction, priority, \
                                        start_date, target_date, achieved, achieved_at, notes, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    params![
                        user_id,
                        goal.kind,
                        exercise_id,
                        metric_id,
                        goal.target_value,
                        goal.direction,
                        goal.priority,
                        goal.start_date,
                        goal.target_date,
                        goal.achieved,
                        goal.achieved_at,
                        goal.notes,
                        goal.created_at,
                        goal.updated_at
                    ],
                )
                .context("inserting a goal")?;
            maps.goals.insert(goal.id, self.tx.last_insert_rowid());
            Ok(())
        })
    }

    fn sessions(&self, user_id: i64, user: &User, maps: &mut IdMaps) -> anyhow::Result<()> {
        user.sessions.iter().try_for_each(|session| {
            self.tx
                .execute(
                    "INSERT INTO sessions (user_id, started_at, ended_at, intent, overall_effort, effort_source, felt, \
                                           cut_short, cut_short_reason) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        user_id,
                        session.started_at,
                        session.ended_at,
                        session.intent,
                        session.overall_effort,
                        session.effort_source,
                        session.felt,
                        session.cut_short,
                        session.cut_short_reason
                    ],
                )
                .context("inserting a session")?;
            let session_id = self.tx.last_insert_rowid();
            maps.sessions.insert(session.id, session_id);
            session.entries.iter().try_for_each(|entry| self.entry(user_id, Some(session_id), entry))
        })
    }

    /// Entries the dump hung off the user because their `session_id` was NULL.
    ///
    /// The easiest thing in the whole format to drop on a round trip — they are not reachable by
    /// walking sessions — so they get their own step rather than riding along inside one.
    fn unsessioned_entries(&self, user_id: i64, user: &User) -> anyhow::Result<()> {
        user.unsessioned_entries.iter().try_for_each(|entry| self.entry(user_id, None, entry))
    }

    fn entry(&self, user_id: i64, session_id: Option<i64>, entry: &ExerciseEntry) -> anyhow::Result<()> {
        self.tx
            .execute(
                "INSERT INTO exercise_entries (user_id, session_id, start_timestamp, end_timestamp, comment) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![user_id, session_id, entry.start_timestamp, entry.end_timestamp, entry.comment],
            )
            .context("inserting an exercise entry")?;
        let entry_id = self.tx.last_insert_rowid();
        entry.sets.iter().try_for_each(|set| self.set(entry_id, set))
    }

    fn set(&self, entry_id: i64, set: &Set) -> anyhow::Result<()> {
        self.tx
            .execute(
                "INSERT INTO sets (exercise_entry_id, exercise_type_id, order_idx, measurement_type_id, count, value, \
                                   perceived_difficulty, comment, logged_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    entry_id,
                    self.exercise(&set.exercise)?,
                    set.order_idx,
                    self.measurement(&set.measurement_type)?,
                    set.count,
                    set.value,
                    set.perceived_difficulty,
                    set.comment,
                    set.logged_at
                ],
            )
            .context("inserting a set")?;
        Ok(())
    }

    /// Programmes, then their blocks, slots and goal links. Goals are already in `maps` — the goal
    /// link table is the one place a programme reaches outside its own subtree.
    fn programmes(&self, user_id: i64, user: &User, maps: &mut IdMaps) -> anyhow::Result<()> {
        user.programmes.iter().try_for_each(|programme| {
            self.tx
                .execute(
                    "INSERT INTO programmes (user_id, title, start_date, target_end_date, days_per_week, split, \
                                             progression_policy, status, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        user_id,
                        programme.title,
                        programme.start_date,
                        programme.target_end_date,
                        programme.days_per_week,
                        programme.split,
                        programme.progression_policy,
                        programme.status,
                        programme.created_at,
                        programme.updated_at
                    ],
                )
                .with_context(|| format!("inserting programme `{}`", programme.title))?;
            let programme_id = self.tx.last_insert_rowid();
            self.programme_blocks(programme_id, programme)?;
            self.programme_slots(programme_id, programme, maps)?;
            self.programme_goals(programme_id, programme, maps)
        })
    }

    fn programme_blocks(&self, programme_id: i64, programme: &Programme) -> anyhow::Result<()> {
        programme.blocks.iter().try_for_each(|block| {
            self.tx
                .execute(
                    "INSERT INTO programme_blocks (programme_id, start_week, end_week, focus, notes) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![programme_id, block.start_week, block.end_week, block.focus, block.notes],
                )
                .context("inserting a programme block")?;
            Ok(())
        })
    }

    fn programme_slots(&self, programme_id: i64, programme: &Programme, maps: &mut IdMaps) -> anyhow::Result<()> {
        programme.slots.iter().try_for_each(|slot| {
            self.tx
                .execute(
                    "INSERT INTO programme_slots (programme_id, week_idx, day_idx, focus, status, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![programme_id, slot.week_idx, slot.day_idx, slot.focus, slot.status, slot.updated_at],
                )
                .context("inserting a programme slot")?;
            maps.slots.insert(slot.id, self.tx.last_insert_rowid());
            Ok(())
        })
    }

    fn programme_goals(&self, programme_id: i64, programme: &Programme, maps: &IdMaps) -> anyhow::Result<()> {
        programme.goal_ids.iter().try_for_each(|goal_id| {
            let goal_id = resolve(&maps.goals, *goal_id, "goal")?;
            self.tx
                .execute("INSERT INTO programme_goals (programme_id, goal_id) VALUES (?1, ?2)", params![programme_id, goal_id])
                .context("linking a programme to a goal")?;
            Ok(())
        })
    }

    /// Rosters last of the referring tables: they point at philosophies, sessions and programme
    /// slots, so all three maps must already be filled.
    fn session_rosters(&self, user_id: i64, user: &User, maps: &mut IdMaps) -> anyhow::Result<()> {
        user.session_rosters.iter().try_for_each(|roster| {
            let philosophy_id = roster.philosophy_id.map(|id| resolve(&maps.philosophies, id, "philosophy")).transpose()?;
            let session_id = roster.session_id.map(|id| resolve(&maps.sessions, id, "session")).transpose()?;
            let slot_id = roster.programme_slot_id.map(|id| resolve(&maps.slots, id, "programme slot")).transpose()?;
            self.tx
                .execute(
                    "INSERT INTO session_rosters (user_id, title, rationale, philosophy_id, status, session_id, \
                                                  programme_slot_id, override_note, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        user_id,
                        roster.title,
                        roster.rationale,
                        philosophy_id,
                        roster.status,
                        session_id,
                        slot_id,
                        roster.override_note,
                        roster.created_at,
                        roster.updated_at
                    ],
                )
                .with_context(|| format!("inserting roster `{}`", roster.title))?;
            let roster_id = self.tx.last_insert_rowid();
            maps.rosters.insert(roster.id, roster_id);
            self.roster_exercises(roster_id, roster)
        })
    }

    fn roster_exercises(&self, roster_id: i64, roster: &SessionRoster) -> anyhow::Result<()> {
        roster.exercises.iter().try_for_each(|exercise| {
            self.tx
                .execute(
                    "INSERT INTO roster_exercises (roster_id, exercise_type_id, order_idx, target_sets, target_reps, \
                                                   target_weight_kg, target_secs, notes) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        roster_id,
                        self.exercise(&exercise.exercise)?,
                        exercise.order_idx,
                        exercise.target_sets,
                        exercise.target_reps,
                        exercise.target_weight_kg,
                        exercise.target_secs,
                        exercise.notes
                    ],
                )
                .context("inserting a roster exercise")?;
            Ok(())
        })
    }

    fn session_reviews(&self, user_id: i64, user: &User, maps: &IdMaps) -> anyhow::Result<()> {
        user.session_reviews.iter().try_for_each(|review| {
            let session_id = resolve(&maps.sessions, review.session_id, "session")?;
            let roster_id = review.roster_id.map(|id| resolve(&maps.rosters, id, "roster")).transpose()?;
            self.tx
                .execute(
                    "INSERT INTO session_reviews (session_id, user_id, roster_id, kind, body, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![session_id, user_id, roster_id, review.kind, review.body, review.created_at],
                )
                .context("inserting a session review")?;
            Ok(())
        })
    }

    fn health_entries(&self, user_id: i64, user: &User) -> anyhow::Result<()> {
        user.health_entries.iter().try_for_each(|entry| {
            self.tx
                .execute(
                    "INSERT INTO health_entries (user_id, entry_type, body_part, severity, description, started_at, \
                                                 resolved_at, notes, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        user_id,
                        entry.entry_type,
                        entry.body_part,
                        entry.severity,
                        entry.description,
                        entry.started_at,
                        entry.resolved_at,
                        entry.notes,
                        entry.updated_at
                    ],
                )
                .context("inserting a health entry")?;
            Ok(())
        })
    }

    fn body_metrics(&self, user_id: i64, user: &User) -> anyhow::Result<()> {
        user.body_metrics.iter().try_for_each(|metric| {
            let metric_id = self.db.get_or_create_metric(&metric.metric)?;
            self.tx
                .execute(
                    "INSERT INTO body_metrics (user_id, metric_id, value, measured_at) VALUES (?1, ?2, ?3, ?4)",
                    params![user_id, metric_id, metric.value, metric.measured_at],
                )
                .with_context(|| format!("inserting a `{}` measurement", metric.metric))?;
            Ok(())
        })
    }

    fn conversation_history(&self, user_id: i64, user: &User) -> anyhow::Result<()> {
        user.conversation_history.iter().try_for_each(|message| {
            self.tx
                .execute(
                    "INSERT INTO conversation_history (user_id, platform, role, content, timestamp, exclude_from_context) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![user_id, message.platform, message.role, message.content, message.timestamp, message.exclude_from_context],
                )
                .context("inserting a conversation message")?;
            Ok(())
        })
    }
}

/// `(name, parent name)` → catalogue id, both halves lowercased to match `COLLATE NOCASE`.
fn exercise_index(conn: &Connection) -> anyhow::Result<HashMap<(String, Option<String>), i64>> {
    let mut stmt = conn.prepare("SELECT e.id, e.name, p.name FROM exercise_types e LEFT JOIN exercise_types p ON p.id = e.parent_id")?;
    let index = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let parent: Option<String> = row.get(2)?;
            Ok(((name.to_lowercase(), parent.map(|parent| parent.to_lowercase())), id))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(index)
}

fn measurement_index(conn: &Connection) -> anyhow::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT name, id FROM measurement_types")?;
    let index = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::counts::COLLECTIONS;

    fn empty_dump() -> Dump {
        Dump {
            format: DUMP_FORMAT.to_string(),
            dump_version: DUMP_VERSION,
            source_schema: SourceSchema { generation: 1, user_version: 13 },
            exported_at: "2026-07-20T12:00:00+00:00".to_string(),
            groups: Vec::new(),
            users: Vec::new(),
        }
    }

    fn user_named(name: &str) -> User {
        User {
            id: 1,
            name: name.to_string(),
            telegram_id: None,
            pubkey: None,
            timezone: "UTC".into(),
            beta_tester: false,
            timers_enabled: true,
            created_at: "2026-01-01 09:00:00".into(),
            updated_at: "2026-01-01 09:00:00".into(),
            group_memberships: Vec::new(),
            philosophies: Vec::new(),
            interview_states: Vec::new(),
            goals: Vec::new(),
            sessions: Vec::new(),
            unsessioned_entries: Vec::new(),
            session_rosters: Vec::new(),
            programmes: Vec::new(),
            health_entries: Vec::new(),
            body_metrics: Vec::new(),
            conversation_history: Vec::new(),
            session_reviews: Vec::new(),
            legacy: Legacy::default(),
        }
    }

    #[test]
    fn importing_into_a_populated_database_is_refused() {
        let db = Database::open_in_memory().unwrap();
        let mut dump = empty_dump();
        dump.users.push(user_named("Alice"));
        db.import_dump(&dump).unwrap();

        let error = db.import_dump(&dump).unwrap_err().to_string();
        assert!(error.contains("already holds data"), "unexpected error: {error}");
        assert!(error.contains("users (1)"), "the error should name the populated table: {error}");
    }

    /// A fresh v2 database is full of reference data — catalogue, measurement types, seeded metrics
    /// — and none of it may count as "already holds data", or no import would ever be allowed.
    #[test]
    fn a_freshly_seeded_database_counts_as_empty() {
        let db = Database::open_in_memory().unwrap();
        db.import_dump(&empty_dump()).unwrap();
    }

    #[test]
    fn an_unknown_exercise_name_fails_loudly() {
        let db = Database::open_in_memory().unwrap();
        let importer_error = {
            let tx = db.conn().unchecked_transaction().unwrap();
            let importer = Importer::new(&db, &tx).unwrap();
            importer.exercise(&ExerciseRef { name: "Moon Press".into(), parent: Some("Deltoid".into()) }).unwrap_err().to_string()
        };
        assert!(importer_error.contains("Moon Press"), "the error should name the exercise: {importer_error}");
        assert!(importer_error.contains("Deltoid"), "the error should name the parent: {importer_error}");
    }

    /// `COLLATE NOCASE` means the catalogue treats "squat" and "Squat" as one row, so the
    /// importer's own index has to as well — otherwise a dump whose casing drifted would be
    /// rejected as an unknown exercise.
    #[test]
    fn exercise_resolution_is_case_insensitive() {
        let db = Database::open_in_memory().unwrap();
        let tx = db.conn().unchecked_transaction().unwrap();
        let importer = Importer::new(&db, &tx).unwrap();
        let canonical = importer.exercise(&ExerciseRef { name: "Squat".into(), parent: Some("Quadriceps".into()) }).unwrap();
        let shouted = importer.exercise(&ExerciseRef { name: "SQUAT".into(), parent: Some("QUADRICEPS".into()) }).unwrap();
        assert_eq!(canonical, shouted);
    }

    /// A collection the dump counts but no table here holds would be exported, counted, and then
    /// silently not imported — the exact failure the count invariant is supposed to catch, hiding
    /// inside the thing that checks for it. The legacy collections are the deliberate exception.
    #[test]
    fn every_dump_collection_is_either_imported_or_deliberately_archival() {
        let imported: Vec<&str> = USER_DATA_TABLES.iter().map(|(collection, _)| *collection).collect();
        COLLECTIONS
            .iter()
            .filter(|name| !name.starts_with("legacy_"))
            // `unsessioned_entries` is a subset of `exercise_entries`, not a table of its own.
            .filter(|name| **name != "unsessioned_entries")
            .for_each(|name| assert!(imported.contains(name), "`{name}` is exported and counted but never imported"));
    }
}

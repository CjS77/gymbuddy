//! Export, and the schema-versioned dump format.
//!
//! This module is the safety net for the schema v2 realignment and the backup tool GymBuddy never
//! had. It reads a database of *either* generation and emits one JSON envelope, always in v2
//! vocabulary — so a dump taken from a legacy database and a dump taken after migration differ
//! only in `source_schema`, never in shape. That property is what lets `gymbuddy migrate --verify`
//! compare an export against a re-export and call the difference a bug.
//!
//! # Envelope
//!
//! ```json
//! {
//!   "format": "gymbuddy.dump",
//!   "dump_version": 1,
//!   "source_schema": { "generation": 1, "user_version": 13 },
//!   "exported_at": "2026-07-20T12:00:00+00:00",
//!   "groups": [ ... ],
//!   "users": [ { "id": 1, "sessions": [ ... ], "legacy": { ... } } ]
//! }
//! ```
//!
//! Data hangs in **per-user trees**: everything reachable from a user sits inside that user's
//! object, so a tree is a self-contained unit and the importer can insert one user at a time.
//! Only genuinely global data (access `groups`) sits beside the trees. The exercise catalogue is
//! *not* exported — it is reference data the target database seeds for itself, which is why
//! exercises travel as names.
//!
//! # Two invariants worth stating plainly
//!
//! **Exercise references are names, not ids.** Catalogue ids were assigned by hand in migrations
//! 02/04/06 and schema v2 reseeds the taxonomy from a catalogue file; the two id spaces are not
//! guaranteed to agree. Every reference therefore travels as [`model::ExerciseRef`] — canonical
//! name plus parent name, since `UNIQUE (parent_id, name)` means the leaf name alone is not a key.
//! The importer resolves against the v2 catalogue and fails loud on an unknown pair rather than
//! silently dropping a set.
//!
//! **Ids that do appear are source ids, and exist only to carry intra-user references** — a roster
//! pointing at the session it was executed as, a programme pointing at the goals it serves. The
//! importer remaps them through translation maps built in dependency order and must never insert
//! them verbatim.
//!
//! # v1 → v2 mapping
//!
//! The v1 reader ([`v1`]) applies every rename on the way out, so the dump never contains legacy
//! vocabulary. `sqlite_master` decides which reader runs (see [`probe`]) because `user_version`
//! ranges overlap: v1 ended at 13 and v2 restarts its count at 1.
//!
//! ## Tables
//!
//! | v1 table                 | dump location                        | notes                                            |
//! |--------------------------|--------------------------------------|--------------------------------------------------|
//! | `users`                  | `users[]`                            | `signal_id` → `legacy.signal_id` (v2 drops it)   |
//! | `groups`                 | `groups[]`                           | global, beside the user trees                    |
//! | `group_members`          | `users[].group_memberships[]`        | `group_id` → group *name*                        |
//! | `exercise_types`         | —                                    | reference data; referenced by name, never dumped |
//! | `measurement_types`      | —                                    | reference data; sets carry the type *name*       |
//! | `sessions`               | `users[].sessions[]`                 | `notes` → `intent`, sentinel stripped            |
//! | `exercise_entry`         | `users[].sessions[].entries[]`       | v2 `exercise_entries`; NULL `session_id` rows go to `users[].unsessioned_entries[]` |
//! | `sets`                   | `…entries[].sets[]`                  | `(count, value)` polymorphism preserved verbatim |
//! | `goals`                  | `users[].goals[]`                    | `metric` text kept as a name for v2's `metrics`  |
//! | `workout_philosophy`     | `users[].philosophies[]`             | v2 `philosophies`                                |
//! | `interview_state`        | `users[].interview_states[]`         | v2 `interview_states`                            |
//! | `workout_plans`          | `users[].session_rosters[]`          | v2 `session_rosters`                             |
//! | `workout_plan_exercises` | `…session_rosters[].exercises[]`     | v2 `roster_exercises`                            |
//! | `programs`               | `users[].programmes[]`               | v2 `programmes`                                  |
//! | `program_goals`          | `…programmes[].goal_ids[]`           | v2 `programme_goals`                             |
//! | `program_blocks`         | `…programmes[].blocks[]`             | v2 `programme_blocks`                            |
//! | `program_slots`          | `…programmes[].slots[]`              | v2 `programme_slots`                             |
//! | `health_entries`         | `users[].health_entries[]`           | unchanged                                        |
//! | `body_metrics`           | `users[].body_metrics[]`             | `metric` text kept as a name for v2's `metrics`  |
//! | `conversation_history`   | `users[].conversation_history[]`     | platform CHECK is dropped in v2; text passes through |
//! | `schedules`              | `users[].legacy.schedules[]`         | **dropped in v2**; archived, importer ignores    |
//! | `schedule_exercises`     | `…legacy.schedules[].exercises[]`    | **dropped in v2**; archived, importer ignores    |
//!
//! ## Columns and values
//!
//! | v1                                     | dump / v2                          | rule                                        |
//! |----------------------------------------|------------------------------------|---------------------------------------------|
//! | `workout_plans.status = 'proposed'`    | `status = "draft"`                 | the rest of the CHECK set is unchanged      |
//! | `workout_plans.plan_id` (child FK)      | nesting                            | roster exercises nest under their roster    |
//! | `workout_plans.program_slot_id`        | `programme_slot_id`                | rename only                                 |
//! | `sessions.notes = "plan:Push\nfoo"`    | `intent = "foo"`                   | sentinel stripped                           |
//! | ↳ the stripped `Push`                  | `legacy.session_plan_names[]`      | schedules are dropped, so this is archival  |
//! | `sessions.notes` without the sentinel  | `intent`, verbatim                 | free text, unchanged                        |
//! | `sets.exercise_type_id`                | `exercise: {name, parent}`         | id → canonical name                         |
//! | `sets.measurement_type_id`             | `measurement_type: "weight_reps"`  | id → name                                   |
//! | `goals.exercise_type_id`               | `exercise: {name, parent}`         | id → canonical name; stays nullable         |
//! | `users.signal_id`                      | `legacy.signal_id`                 | v2 drops the column                         |
//! | `exercise_types.description`           | —                                  | never populated; v2 drops the column        |
//! | *(absent in v1)*                       | `goals[].achieved_at = null`       | v2-only column                              |
//! | *(absent in v1)*                       | `sessions[].effort_source = null`  | v2-only column                              |
//! | *(absent in v1)*                       | `users[].session_reviews = []`     | v2-only table                               |
//!
//! Timestamps are copied as the strings SQLite holds them in. No parsing, no normalisation: a
//! backup that rewrites its own timestamps is not a backup.

pub mod model;
mod probe;
mod v1;

use std::path::Path;

use anyhow::Context as _;
use rusqlite::{Connection, OpenFlags};

pub use model::{DUMP_FORMAT, DUMP_VERSION, Dump};
pub use probe::Generation;

/// Export the database at `path`.
///
/// Opened **read-only**, which is not a detail: the source of an export is frequently the only
/// copy of the user's data, and a migration tool that mutates its own input has no rollback. A
/// read-only handle also means no migrations run, so a legacy database stays legacy.
pub fn export_path(path: &Path) -> anyhow::Result<Dump> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} read-only", path.display()))?;
    export(&conn)
}

/// Export an open connection. Picks the reader from the source's schema generation; both readers
/// emit the identical envelope.
pub fn export(conn: &Connection) -> anyhow::Result<Dump> {
    let (generation, source) = probe::probe(conn)?;
    let exported_at = chrono::Utc::now().to_rfc3339();
    match generation {
        Generation::V1 => v1::read(conn, source, exported_at),
        Generation::V2 => anyhow::bail!(
            "this database is already schema v2 (user_version {}), and the v2 reader lands with schema v2 itself in EP-10",
            source.user_version
        ),
    }
}

/// Serialise a dump as pretty-printed JSON.
///
/// Pretty rather than compact on purpose: a backup an operator cannot read in an editor, diff, or
/// grep is worth much less than the bytes it saves.
pub fn to_json(dump: &Dump) -> anyhow::Result<String> {
    serde_json::to_string_pretty(dump).context("serialising dump to JSON")
}

/// Parse a dump, rejecting anything that is not one this build understands.
pub fn from_json(json: &str) -> anyhow::Result<Dump> {
    let dump: Dump = serde_json::from_str(json).context("parsing dump JSON")?;
    anyhow::ensure!(dump.format == DUMP_FORMAT, "not a GymBuddy dump: format is `{}`, expected `{DUMP_FORMAT}`", dump.format);
    anyhow::ensure!(
        dump.dump_version == DUMP_VERSION,
        "unsupported dump version {}: this build reads version {DUMP_VERSION}",
        dump.dump_version
    );
    Ok(dump)
}

#[cfg(test)]
mod tests;

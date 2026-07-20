//! The schema v1 fixture: a frozen copy of the legacy migration set, plus the seed applied on top.
//!
//! # Why a copy of the migrations
//!
//! `backend/migrations/` holds the *live* schema, and Phase 1 of the realignment replaces it with
//! schema v2. The v1 reader in `dump::v1` must keep reading v1 databases long after that — so the
//! tests that prove it works cannot build their fixture from the live set, or they would quietly
//! start testing v2 the day the live set changes. `v1_migrations/` is therefore a byte copy taken
//! while the two were still identical; once Phase 1 lands it is the only copy of schema v1 left in
//! the tree, and it must not be edited again.
//!
//! # Shared between two test binaries
//!
//! The unit tests in `src/dump/tests.rs` reach this file through `#[path]`, and the integration
//! tests in `tests/` reach it through a plain `mod fixtures;`. Nothing here may refer to the crate
//! under test, since `crate::` means something different on each side of that line.

#![allow(dead_code)] // Each consumer uses a different subset.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use include_dir::{Dir, include_dir};
use rusqlite::Connection;
use rusqlite_migration::Migrations;

/// Frozen copy of the 13 schema v1 migrations. Never regenerate this from `backend/migrations/`.
static V1_MIGRATIONS_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/v1_migrations");

/// The seed applied on top of the migrations. See the file for what it covers and why.
pub const V1_SEED: &str = include_str!("v1_seed.sql");

/// `PRAGMA user_version` a fully-migrated v1 database carries.
pub const V1_USER_VERSION: i64 = 13;

static V1_MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::from_directory(&V1_MIGRATIONS_DIR).expect("v1 fixture migrations are not a valid migration set"));

/// An empty, fully-migrated v1 database in memory.
pub fn empty_v1_db() -> Connection {
    let mut conn = Connection::open_in_memory().expect("opening an in-memory database");
    V1_MIGRATIONS.to_latest(&mut conn).expect("applying the v1 fixture migrations");
    conn
}

/// The seeded v1 database in memory — the fixture nearly every export test runs against.
pub fn seeded_v1_db() -> Connection {
    let conn = empty_v1_db();
    conn.execute_batch(V1_SEED).expect("seeding the v1 fixture");
    conn
}

/// An empty, fully-migrated v1 database written to `path`.
///
/// The file-backed counterpart of [`empty_v1_db`], and the only way to get one now that
/// `Database::open` builds schema v2: a test that wants a *legacy* file on disk has to reach for
/// these frozen migrations explicitly.
pub fn empty_v1_db_at(path: &Path) -> Connection {
    let mut conn = Connection::open(path).expect("creating the fixture database file");
    V1_MIGRATIONS.to_latest(&mut conn).expect("applying the v1 fixture migrations");
    conn
}

/// The same fixture written to `path`, for tests that need a file — the CLI, and anything asserting
/// that an export leaves its source untouched.
pub fn seeded_v1_db_at(path: &Path) {
    let conn = empty_v1_db_at(path);
    conn.execute_batch(V1_SEED).expect("seeding the v1 fixture");
}

/// Row counts taken from the *source* database, keyed by the dump's collection names.
///
/// This is the other half of the count invariant: `Dump::row_counts` counts what came out, this
/// counts what went in, and the test asserts the two maps are equal. Counting the source here
/// rather than hard-coding the seed's totals means the seed can grow without the invariant needing
/// an edit — and a `SELECT COUNT(*)` cannot drift from the rows the exporter walked past.
pub fn source_row_counts(conn: &Connection) -> BTreeMap<&'static str, usize> {
    SOURCE_COUNT_QUERIES.iter().map(|(key, sql)| (*key, count(conn, sql))).collect()
}

fn count(conn: &Connection, sql: &str) -> usize {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap_or_else(|e| panic!("counting rows with `{sql}`: {e}")) as usize
}

/// Every v1 table the exporter reads, paired with the dump collection it lands in.
///
/// `session_reviews` has no v1 source at all and is pinned to zero: schema v2 adds the table, and a
/// v1 export inventing rows for it would be a bug worth catching.
const SOURCE_COUNT_QUERIES: &[(&str, &str)] = &[
    ("users", "SELECT COUNT(*) FROM users"),
    ("groups", "SELECT COUNT(*) FROM groups"),
    ("group_memberships", "SELECT COUNT(*) FROM group_members"),
    ("philosophies", "SELECT COUNT(*) FROM workout_philosophy"),
    ("interview_states", "SELECT COUNT(*) FROM interview_state"),
    ("goals", "SELECT COUNT(*) FROM goals"),
    ("sessions", "SELECT COUNT(*) FROM sessions"),
    ("exercise_entries", "SELECT COUNT(*) FROM exercise_entry"),
    ("unsessioned_entries", "SELECT COUNT(*) FROM exercise_entry WHERE session_id IS NULL"),
    ("sets", "SELECT COUNT(*) FROM sets"),
    ("session_rosters", "SELECT COUNT(*) FROM workout_plans"),
    ("roster_exercises", "SELECT COUNT(*) FROM workout_plan_exercises"),
    ("programmes", "SELECT COUNT(*) FROM programs"),
    ("programme_goals", "SELECT COUNT(*) FROM program_goals"),
    ("programme_blocks", "SELECT COUNT(*) FROM program_blocks"),
    ("programme_slots", "SELECT COUNT(*) FROM program_slots"),
    ("health_entries", "SELECT COUNT(*) FROM health_entries"),
    ("body_metrics", "SELECT COUNT(*) FROM body_metrics"),
    ("conversation_history", "SELECT COUNT(*) FROM conversation_history"),
    ("session_reviews", "SELECT 0"),
    ("legacy_schedules", "SELECT COUNT(*) FROM schedules"),
    ("legacy_schedule_exercises", "SELECT COUNT(*) FROM schedule_exercises"),
    ("legacy_session_plan_names", "SELECT COUNT(*) FROM sessions WHERE notes LIKE 'plan:%'"),
];

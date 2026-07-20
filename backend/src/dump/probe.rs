//! Which schema generation is this database?
//!
//! `PRAGMA user_version` alone cannot answer it: v1 ended at 13 and schema v2 restarts its
//! migration count from 1, so the two ranges overlap. The decision therefore rests on
//! `sqlite_master` — the presence of a table only one generation ever had — and `user_version`
//! rides along as provenance in [`SourceSchema`].

use std::path::Path;

use anyhow::{Context as _, bail};
use rusqlite::{Connection, OpenFlags};

use super::model::SourceSchema;
use crate::db::migrations::V2_USER_VERSION;

/// Table that exists only in schema v2.
const V2_MARKER: &str = "session_rosters";

/// Table that exists only in schema v1.
const V1_MARKER: &str = "workout_plans";

/// A second v1-only table, checked alongside [`V1_MARKER`] before refusing to serve. One marker is
/// enough to pick a *reader*; refusing to start on the only copy of someone's data is worth two.
const V1_MARKER_ALT: &str = "schedules";

/// Which reader to use for a source database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generation {
    /// Legacy: `workout_plans`, `schedules`, `program*`, `sessions.notes`.
    V1,
    /// The SessionRoster schema: `session_rosters`, `programme*`, `sessions.intent`.
    V2,
}

impl Generation {
    fn number(self) -> u32 {
        match self {
            Generation::V1 => 1,
            Generation::V2 => 2,
        }
    }
}

/// Identify a source database, or explain why it is not one.
pub fn probe(conn: &Connection) -> anyhow::Result<(Generation, SourceSchema)> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).context("reading PRAGMA user_version")?;
    let generation = detect_generation(conn)?;
    Ok((generation, SourceSchema { generation: generation.number(), user_version }))
}

fn detect_generation(conn: &Connection) -> anyhow::Result<Generation> {
    // v2 is checked first: a database that somehow carried both markers is mid-migration, and the
    // newer shape is the one that can be read completely.
    if has_table(conn, V2_MARKER)? {
        return Ok(Generation::V2);
    }
    if has_table(conn, V1_MARKER)? {
        return Ok(Generation::V1);
    }
    bail!("not a GymBuddy database: neither `{V2_MARKER}` (schema v2) nor `{V1_MARKER}` (schema v1) is present")
}

fn has_table(conn: &Connection, name: &str) -> anyhow::Result<bool> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |row| row.get(0))
        .with_context(|| format!("probing sqlite_master for `{name}`"))?;
    Ok(count > 0)
}

/// Is the database at `path` a legacy (schema v1) one that `serve` must not open?
///
/// This is the guard rail on the one thing a migration must never do by accident. `Database::open`
/// applies whatever migrations it finds, and what it finds is now schema v2 — so pointing a v2
/// build at a v1 file would create the v2 tables *beside* the v1 ones and leave a database that is
/// neither generation, in place, with no rollback. Refusing costs a restart; the alternative costs
/// the user's training history.
///
/// Two conditions, both required, exactly as the realignment plan specifies:
///
/// * `user_version` at or below the v1 ceiling of 13, and
/// * a v1 marker table actually present in `sqlite_master`.
///
/// The marker is what carries the decision. v2's own `user_version` is small (see
/// [`V2_USER_VERSION`]) and therefore also "at or below 13" — a version test alone would refuse to
/// start on a perfectly good v2 database.
///
/// A missing file is not legacy: that is a first run, and `Database::open` is about to create it.
/// Nor is a file carrying neither marker — an empty or foreign database is somebody else's problem
/// to report, and not one to be described as needing a migration.
pub fn is_legacy_database(path: &Path) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} read-only to check its schema", path.display()))?;
    is_legacy(&conn)
}

fn is_legacy(conn: &Connection) -> anyhow::Result<bool> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).context("reading PRAGMA user_version")?;
    if user_version > V1_CEILING {
        return Ok(false);
    }
    Ok(has_table(conn, V1_MARKER)? || has_table(conn, V1_MARKER_ALT)?)
}

/// The last migration schema v1 ever had.
const V1_CEILING: i64 = 13;

const _: () = assert!(V2_USER_VERSION <= V1_CEILING, "the version ranges overlap, which is why the marker table decides");

#[cfg(test)]
mod tests {
    use super::*;
    // The frozen v1 fixture, not the live migration set: Phase 1 turns the live set into schema v2,
    // and this test must keep asserting what a *v1* database looks like after that happens.
    use crate::dump::fixtures::{V1_USER_VERSION, empty_v1_db};

    #[test]
    fn detects_v1_database() {
        let (generation, source) = probe(&empty_v1_db()).unwrap();
        assert_eq!(generation, Generation::V1);
        assert_eq!(source.generation, 1);
        assert_eq!(source.user_version, V1_USER_VERSION, "v1 ends at migration 13");
    }

    #[test]
    fn detects_v2_by_marker_table_not_by_user_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE session_rosters (id INTEGER PRIMARY KEY); PRAGMA user_version = 1;").unwrap();
        let (generation, source) = probe(&conn).unwrap();
        assert_eq!(generation, Generation::V2);
        // The low user_version is exactly the collision the table probe exists to survive.
        assert_eq!(source.user_version, 1);
    }

    #[test]
    fn rejects_a_database_that_is_not_gymbuddy() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE unrelated (id INTEGER PRIMARY KEY);").unwrap();
        let error = probe(&conn).unwrap_err().to_string();
        assert!(error.contains("not a GymBuddy database"), "unexpected error: {error}");
    }

    #[test]
    fn a_v1_database_is_legacy() {
        assert!(is_legacy(&empty_v1_db()).unwrap());
    }

    /// The collision the whole marker-table approach exists for: schema v2's `user_version` is well
    /// under the v1 ceiling, so a version test on its own would condemn a current database.
    #[test]
    fn a_v2_database_is_not_legacy_despite_its_small_user_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::MIGRATIONS.to_latest(&mut conn).unwrap();
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert!(user_version <= V1_CEILING, "the premise of this test is that the ranges overlap");
        assert!(!is_legacy(&conn).unwrap());
    }

    /// A first run. There is no file yet, so there is nothing to migrate and nothing to refuse.
    #[test]
    fn a_missing_file_is_not_legacy() {
        assert!(!is_legacy_database(Path::new("/nonexistent/gymbuddy/first-run.db")).unwrap());
    }

    /// An empty database carries no marker. `Database::open` will migrate it to v2 in a moment, and
    /// telling the operator to run `gymbuddy migrate` on it would be advice that cannot work.
    #[test]
    fn an_empty_database_is_not_legacy() {
        assert!(!is_legacy(&Connection::open_in_memory().unwrap()).unwrap());
    }
}

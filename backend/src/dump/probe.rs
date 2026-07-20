//! Which schema generation is this database?
//!
//! `PRAGMA user_version` alone cannot answer it: v1 ended at 13 and schema v2 restarts its
//! migration count from 1, so the two ranges overlap. The decision therefore rests on
//! `sqlite_master` — the presence of a table only one generation ever had — and `user_version`
//! rides along as provenance in [`SourceSchema`].

use anyhow::{Context as _, bail};
use rusqlite::Connection;

use super::model::SourceSchema;

/// Table that exists only in schema v2.
const V2_MARKER: &str = "session_rosters";

/// Table that exists only in schema v1.
const V1_MARKER: &str = "workout_plans";

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::MIGRATIONS;

    fn v1_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_latest(&mut conn).unwrap();
        conn
    }

    #[test]
    fn detects_v1_database() {
        let (generation, source) = probe(&v1_conn()).unwrap();
        assert_eq!(generation, Generation::V1);
        assert_eq!(source.generation, 1);
        assert_eq!(source.user_version, 13, "v1 ends at migration 13");
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
}

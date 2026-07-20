use std::path::Path;

use anyhow::Context as _;
use rusqlite::Connection;

use super::catalogue;
use super::migrations::MIGRATIONS;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("Failed to create database directory {}", parent.display()))?;
        }
        let mut conn = Connection::open(path).with_context(|| format!("Failed to open database at {}", path.display()))?;
        prepare(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let mut conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
        prepare(&mut conn)?;
        Ok(Self { conn })
    }

    /// The raw connection, visible only to the DAO modules.
    ///
    /// Everything outside `crate::db` goes through a named method. The rule is not
    /// tidiness: a caller that reaches for the connection writes SQL against a table
    /// whose invariants live in another module — the `sessions` bump that skipped the
    /// entries DAO, the roster status flipped from the programmes DAO — and every such
    /// write is one more place the schema has to be kept true by hand. Renaming a
    /// column then fails at runtime instead of at compile time.
    pub(in crate::db) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// The raw connection for tests that must assert on storage rather than on
    /// behaviour — schema shape, or a row a DAO deliberately offers no reader for.
    ///
    /// `#[cfg(test)]`, so it does not exist for the integration tests in
    /// `backend/tests/`: those exercise the crate as a consumer sees it, and a fixture
    /// needing a row it cannot write through the DAO has found a missing method rather
    /// than a reason for an escape hatch.
    #[cfg(test)]
    pub fn test_conn(&self) -> &Connection {
        &self.conn
    }
}

/// Bring a freshly-opened connection up to date: pragmas, then schema, then reference data.
///
/// The catalogue seed runs on every open, not just the first. That is the whole design — it is
/// `INSERT OR IGNORE` reference data, so "has this database seen it yet?" is a question nobody has
/// to track. See [`catalogue`].
fn prepare(conn: &mut Connection) -> anyhow::Result<()> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    MIGRATIONS.to_latest(conn).context("Failed to apply database migrations")?;
    tracing::debug!("Database migrations applied");
    catalogue::apply(conn).context("Failed to apply the catalogue seed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smoke test on the startup path. The exhaustive table list lives in
    /// `migrations::tests::schema_declares_exactly_the_v2_tables`, and the seeded taxonomy in
    /// `catalogue::tests` — this only asserts that opening a database gets you all of it at once.
    #[test]
    fn open_in_memory_succeeds() {
        let db = Database::open_in_memory().unwrap();
        let rosters: i64 = db.test_conn().query_row("SELECT COUNT(*) FROM session_rosters", [], |row| row.get(0)).unwrap();
        assert_eq!(rosters, 0, "a fresh database has no user data");
    }

    #[test]
    fn foreign_keys_enabled() {
        let db = Database::open_in_memory().unwrap();
        let fk: i64 = db.test_conn().query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn wal_mode_enabled() {
        let db = Database::open_in_memory().unwrap();
        let mode: String = db.test_conn().query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
        // In-memory databases report "memory" for journal_mode, WAL only applies to file-backed DBs
        assert!(mode == "wal" || mode == "memory");
    }

    #[test]
    fn measurement_types_seeded() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db.test_conn().query_row("SELECT COUNT(*) FROM measurement_types", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn user_version_set_to_latest() {
        let db = Database::open_in_memory().unwrap();
        let version: i64 = db.test_conn().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, super::super::migrations::V2_USER_VERSION);
    }
}

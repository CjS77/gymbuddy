use std::path::Path;

use anyhow::Context as _;
use rusqlite::Connection;

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
        run_migrations(&mut conn)?;
        tracing::debug!("Database migrations applied");
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let mut conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
        run_migrations(&mut conn)?;
        tracing::debug!("Database migrations applied");
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn run_migrations(conn: &mut Connection) -> anyhow::Result<()> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    MIGRATIONS.to_latest(conn).context("Failed to apply database migrations")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_succeeds() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('exercise_types', 'users', 'groups', 'group_members', 'goals', \
                  'sessions', 'exercise_entry', 'sets', 'schedules', 'schedule_exercises', \
                  'health_entries', 'conversation_history', 'measurement_types', \
                  'programs', 'program_goals', 'program_blocks', 'program_slots')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 17);
    }

    #[test]
    fn foreign_keys_enabled() {
        let db = Database::open_in_memory().unwrap();
        let fk: i64 = db.conn().query_row("PRAGMA foreign_keys", [], |row| row.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn wal_mode_enabled() {
        let db = Database::open_in_memory().unwrap();
        let mode: String = db.conn().query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
        // In-memory databases report "memory" for journal_mode, WAL only applies to file-backed DBs
        assert!(mode == "wal" || mode == "memory");
    }

    #[test]
    fn muscle_groups_seeded() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 =
            db.conn().query_row("SELECT COUNT(*) FROM exercise_types WHERE level = 'muscle_group'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 7);
    }

    #[test]
    fn measurement_types_seeded() {
        let db = Database::open_in_memory().unwrap();
        let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM measurement_types", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn user_version_set_to_latest() {
        let db = Database::open_in_memory().unwrap();
        let version: i64 = db.conn().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, 12);
    }
}

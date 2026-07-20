use std::sync::LazyLock;

use include_dir::{Dir, include_dir};
use rusqlite_migration::Migrations;

/// Schema v2. Two migrations, and the expectation is that it stays a short list: DDL changes belong
/// here, but catalogue rows go to `backend/catalogue/` (see [`super::catalogue`]), which is where
/// schema v1's chain of thirteen picked up most of its length.
static MIGRATIONS_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

pub static MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::from_directory(&MIGRATIONS_DIR).expect("invalid migrations directory"));

/// `PRAGMA user_version` a fully-migrated schema v2 database carries.
///
/// Deliberately small, and it collides with the v1 range — v1 ended at 13. Nothing may decide a
/// database's generation from this number alone; that is what the marker-table probe in
/// `crate::dump::probe` is for.
pub const V2_USER_VERSION: i64 = 2;

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn migrations_valid() {
        MIGRATIONS.validate().expect("migrations failed validation");
    }

    #[test]
    fn migrations_round_trip_up_then_down() {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_latest(&mut conn).expect("up to latest failed");
        let after_up: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(after_up, V2_USER_VERSION);

        MIGRATIONS.to_version(&mut conn, 0).expect("down to 0 failed");
        let after_down: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(after_down, 0);

        let table_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0)).unwrap();
        assert_eq!(table_count, 0, "all app tables should be dropped");

        MIGRATIONS.to_latest(&mut conn).expect("re-apply up failed");
        let after_redo: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(after_redo, V2_USER_VERSION);
    }

    /// Legacy vocabulary must not survive anywhere in the v2 schema — not as a table, not as a
    /// column, not as an index. The acceptance check for the rename, run against the real DDL
    /// rather than against a grep over the tree.
    #[test]
    fn no_legacy_names_remain_in_the_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_latest(&mut conn).expect("up to latest failed");

        let mut stmt = conn.prepare("SELECT sql FROM sqlite_master WHERE sql IS NOT NULL").unwrap();
        let schema: String = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect::<Vec<_>>().join("\n");

        let banned = ["workout_plan", "plan_id", "schedule", "signal_id", "workout_philo", "interview_state ", "exercise_entry "];
        banned.iter().for_each(|name| assert!(!schema.contains(name), "schema v2 still mentions `{name}`"));

        // `program` only ever as part of `programme`.
        let stray_program = schema.match_indices("program").any(|(at, _)| !schema[at..].starts_with("programme"));
        assert!(!stray_program, "schema v2 spells `program` somewhere without the `me`");
    }

    /// Every v2 table, named once, so adding or losing one is a deliberate edit to this list.
    #[test]
    fn schema_declares_exactly_the_v2_tables() {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_latest(&mut conn).expect("up to latest failed");

        let mut stmt =
            conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        let tables: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();

        assert_eq!(
            tables,
            [
                "body_metrics",
                "conversation_history",
                "exercise_entries",
                "exercise_types",
                "goals",
                "group_members",
                "groups",
                "health_entries",
                "interview_states",
                "measurement_types",
                "metrics",
                "philosophies",
                "programme_blocks",
                "programme_goals",
                "programme_slots",
                "programmes",
                "roster_exercises",
                "session_reviews",
                "session_rosters",
                "sessions",
                "sets",
                "users",
            ]
        );
    }
}

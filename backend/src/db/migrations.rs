use std::sync::LazyLock;

use include_dir::{Dir, include_dir};
use rusqlite_migration::Migrations;

static MIGRATIONS_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/migrations");

pub static MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::from_directory(&MIGRATIONS_DIR).expect("invalid migrations directory"));

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
        assert_eq!(after_up, 7);

        MIGRATIONS.to_version(&mut conn, 0).expect("down to 0 failed");
        let after_down: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(after_down, 0);

        let table_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0)).unwrap();
        assert_eq!(table_count, 0, "all app tables should be dropped");

        MIGRATIONS.to_latest(&mut conn).expect("re-apply up failed");
        let after_redo: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(after_redo, 7);
    }

    #[test]
    fn migration_seeds_taxonomy() {
        let mut conn = Connection::open_in_memory().unwrap();
        MIGRATIONS.to_latest(&mut conn).expect("up to latest failed");

        let muscle_group_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM exercise_types WHERE level = 'muscle_group'", [], |r| r.get(0)).unwrap();
        assert_eq!(muscle_group_count, 7);

        let total: i64 = conn.query_row("SELECT COUNT(*) FROM exercise_types", [], |r| r.get(0)).unwrap();
        assert!(total >= 100, "expected at least 100 seeded rows, got {total}");

        let cardio: i64 = conn
            .query_row("SELECT COUNT(*) FROM exercise_types WHERE name = 'Cardio' AND level = 'muscle_group'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cardio, 1, "Cardio muscle_group must be present");

        let bent_over: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM exercise_types WHERE name = 'Bent Over Barbell Row' AND parent_id = 200 AND level = 'exercise'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bent_over, 1, "Bent Over Barbell Row must be seeded under Latissimus Dorsi");

        let one_arm: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM exercise_types WHERE name = 'One Arm Dumbbell Row' AND parent_id = 200 AND level = 'exercise'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(one_arm, 1, "One Arm Dumbbell Row must be seeded under Latissimus Dorsi");
    }
}

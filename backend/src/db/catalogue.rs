//! Reference data applied at startup, not by migration.
//!
//! # Why this exists
//!
//! Schema v1 added exercises to the catalogue one migration at a time — migrations 04 and 06 each
//! carried a single `INSERT`. That made a data edit into a schema change: every new movement bumped
//! `user_version`, and the migration chain recorded the order the catalogue happened to grow in
//! rather than anything about the schema.
//!
//! The catalogue is reference data. It has no user rows in it, no database is meaningfully "behind"
//! on it, and re-applying it must be safe. So it is applied on **every** startup, after the
//! migrations, from SQL embedded in the binary at `backend/catalogue/`.
//!
//! # The idempotency contract
//!
//! Every statement in those files is `INSERT OR IGNORE`, and none of them writes an explicit `id`.
//! Uniqueness does the work: `UNIQUE (parent_id, name)` on `exercise_types`, `UNIQUE (name)` on
//! `metrics`. Running the seed against a database that already has the rows changes nothing;
//! running it against one that is missing some inserts exactly those.
//!
//! That contract is the reason ids are never written here. A seeded id would be stable only until
//! two databases diverged, and the dump format already assumes they do — exercises travel between
//! databases as names precisely because catalogue ids drift.
//!
//! # What belongs here
//!
//! Additions *since* the v2 baseline. The baseline taxonomy itself is migration `02-catalogue-seed`,
//! because a fresh database has to start somewhere and the stable parent ids these files address
//! have to come from a fixed point.

use anyhow::Context as _;
use include_dir::{Dir, include_dir};
use rusqlite::Connection;

/// The seed files, embedded at compile time. Applied in filename order, so a file may rely on rows
/// an earlier one inserted.
static CATALOGUE_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/catalogue");

/// Apply every catalogue seed file to `conn`.
///
/// Runs after migrations on each open. Cheap when there is nothing to do: each statement is an
/// `INSERT OR IGNORE` that conflicts on a unique index and stops.
pub fn apply(conn: &Connection) -> anyhow::Result<()> {
    let before = conn.total_changes();
    seed_files().try_for_each(|(name, sql)| apply_one(conn, name, sql))?;
    let inserted = conn.total_changes() - before;
    if inserted > 0 {
        tracing::info!(rows = inserted, "Catalogue seed applied");
    } else {
        tracing::debug!("Catalogue seed already up to date");
    }
    Ok(())
}

/// The seed files in filename order, as `(name, sql)`.
///
/// `include_dir` does not promise an order, so it is imposed here rather than left to whatever the
/// build happened to produce.
fn seed_files() -> impl Iterator<Item = (&'static str, &'static str)> {
    let mut files: Vec<_> = CATALOGUE_DIR
        .files()
        .filter(|file| file.path().extension().is_some_and(|ext| ext == "sql"))
        .filter_map(|file| Some((file.path().to_str()?, file.contents_utf8()?)))
        .collect();
    files.sort_by_key(|(name, _)| *name);
    files.into_iter()
}

fn apply_one(conn: &Connection, name: &str, sql: &str) -> anyhow::Result<()> {
    conn.execute_batch(sql).with_context(|| format!("applying catalogue seed {name}"))
}

#[cfg(test)]
mod tests {
    use super::super::database::Database;

    /// The taxonomy assertions that used to live on the migration set. They belong to the startup
    /// path now: the baseline arrives by migration and the additions by seed, and only opening a
    /// database exercises both.
    #[test]
    fn startup_seeds_the_taxonomy() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn();

        let muscle_groups: i64 =
            conn.query_row("SELECT COUNT(*) FROM exercise_types WHERE level = 'muscle_group'", [], |r| r.get(0)).unwrap();
        assert_eq!(muscle_groups, 7);

        let total: i64 = conn.query_row("SELECT COUNT(*) FROM exercise_types", [], |r| r.get(0)).unwrap();
        assert!(total >= 100, "expected at least 100 seeded rows, got {total}");

        let cardio: i64 = conn
            .query_row("SELECT COUNT(*) FROM exercise_types WHERE name = 'Cardio' AND level = 'muscle_group'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cardio, 1, "Cardio muscle_group must be present");
    }

    /// The two rows that were migrations 04 and 06 in schema v1. They now arrive from
    /// `backend/catalogue/`, and their parent is resolved by the id the baseline migration pinned.
    #[test]
    fn startup_seeds_catalogue_additions_under_the_right_parent() {
        let db = Database::open_in_memory().unwrap();
        let names = ["Bent Over Barbell Row", "One Arm Dumbbell Row"];
        names.iter().for_each(|name| {
            let count: i64 = db
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM exercise_types WHERE name = ?1 AND level = 'exercise' \
                     AND parent_id = (SELECT id FROM exercise_types WHERE name = 'Latissimus Dorsi')",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{name} must be seeded under Latissimus Dorsi");
        });
    }

    #[test]
    fn startup_seeds_the_canonical_metrics_with_units() {
        let db = Database::open_in_memory().unwrap();
        let unit: String = db.conn().query_row("SELECT unit FROM metrics WHERE name = 'bodyweight_kg'", [], |r| r.get(0)).unwrap();
        assert_eq!(unit, "kg");

        let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 4);
    }

    /// The whole point of the mechanism: it runs on every startup, so applying it twice must not
    /// duplicate a row or fail on a unique index.
    #[test]
    fn re_applying_the_seed_is_a_no_op() {
        let db = Database::open_in_memory().unwrap();
        let count_types = || -> i64 { db.conn().query_row("SELECT COUNT(*) FROM exercise_types", [], |r| r.get(0)).unwrap() };
        let count_metrics = || -> i64 { db.conn().query_row("SELECT COUNT(*) FROM metrics", [], |r| r.get(0)).unwrap() };
        let (before_types, before_metrics) = (count_types(), count_metrics());

        super::apply(db.conn()).expect("re-applying the catalogue seed");
        super::apply(db.conn()).expect("re-applying the catalogue seed twice");

        assert_eq!(count_types(), before_types, "seeding again must not add exercise types");
        assert_eq!(count_metrics(), before_metrics, "seeding again must not add metrics");
    }
}

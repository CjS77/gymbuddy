//! `gymbuddy migrate` — export, build v2, import, verify.
//!
//! # The old file is never written
//!
//! Not "is written carefully", not "is backed up first": never opened for writing at all. The
//! source is read through [`super::export_path`], which opens `SQLITE_OPEN_READ_ONLY`, and the
//! result goes into a *new* file. The original therefore remains a complete, untouched rollback —
//! if anything about the new database looks wrong a week later, the answer is to point the config
//! back at the old path.
//!
//! # Deployment gate
//!
//! Before migrating the live database:
//!
//! 1. Stop the bot. A migration of a file being written to is a migration of a moving target.
//! 2. Copy the live database aside and run `gymbuddy migrate --db <copy> --out <copy>.v2.db`
//!    against the copy first. `--verify` is on by default; a non-zero exit means do not proceed.
//! 3. Run it for real, and keep the old file. It is the rollback, and it costs a few megabytes.
//! 4. Point the config at the new file and restart. `serve` refuses to open a legacy database, so
//!    a forgotten step three fails loudly at startup rather than half-upgrading the file in place.
//!
//! # What verification is worth
//!
//! `--verify` re-exports the database that was just built and compares it against the dump that
//! built it — see [`super::compare`] for what that does and does not cover — and separately checks
//! that every table holds as many rows as the dump said it carried. The two checks fail in
//! different ways on purpose: the compare catches a field that changed, the counts catch a table
//! that was never written at all, and neither is derived from the other.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context as _;

use super::{Dump, RowCounts, compare, export_path};
use crate::db::Database;

/// What a migration did, for the operator's log.
pub struct MigrateReport {
    /// Schema generation of the source: `1` for a legacy database, `2` for one already migrated.
    pub source_generation: u32,
    /// Rows carried by the dump, per collection.
    pub counts: RowCounts,
    /// Whether the structural comparison ran. False only when `--verify false` was passed.
    pub verified: bool,
}

/// Migrate the database at `db` into a new schema v2 database at `out`.
pub fn migrate(db: &Path, out: &Path, verify: bool) -> anyhow::Result<MigrateReport> {
    anyhow::ensure!(
        !out.exists(),
        "{} already exists. Migration writes a new database and will not overwrite one — \
         choose a path that does not exist, or move the existing file aside.",
        out.display()
    );

    let dump = export_path(db).with_context(|| format!("exporting {}", db.display()))?;
    let counts = dump.row_counts();
    tracing::info!(source_schema = dump.source_schema.generation, rows = counts.total(), "Exported the source database");

    let table_counts = build_target(out, &dump)?;
    verify_counts(&counts, &table_counts)?;
    tracing::info!(out = %out.display(), "Imported into schema v2");

    if verify {
        verify_structure(&dump, out)?;
        tracing::info!("Verification passed: the re-export is structurally identical to the source dump");
    } else {
        tracing::warn!("Verification skipped (--verify false); the migrated database has not been checked against its source");
    }

    Ok(MigrateReport { source_generation: dump.source_schema.generation, counts, verified: verify })
}

/// Create the target and import into it, returning its per-table row counts.
///
/// Scoped so the write connection is closed — and its WAL checkpointed — before anything reopens
/// the file to read it back.
fn build_target(out: &Path, dump: &Dump) -> anyhow::Result<BTreeMap<&'static str, usize>> {
    let target = Database::open(out).with_context(|| format!("creating schema v2 database at {}", out.display()))?;
    target.import_dump(dump).context("importing the dump")?;
    target.import_row_counts()
}

/// Every table must hold exactly as many rows as the dump said it carried.
///
/// Counted from `SELECT COUNT(*)` on the target and from the dump tree respectively — two
/// independent counts of the same thing, so agreement is evidence. This catches the failure the
/// structural compare cannot: a collection the importer never walked would come back empty from
/// *both* exports and compare equal to itself.
fn verify_counts(dump: &RowCounts, tables: &BTreeMap<&'static str, usize>) -> anyhow::Result<()> {
    let mismatches = tables
        .iter()
        .filter(|(collection, count)| dump.get(collection) != **count)
        .map(|(collection, count)| format!("{collection}: dump has {}, database has {count}", dump.get(collection)))
        .collect::<Vec<_>>();
    anyhow::ensure!(mismatches.is_empty(), "row counts do not match after import:\n  {}", mismatches.join("\n  "));
    Ok(())
}

/// Re-export the migrated database and compare it against the dump it was built from.
fn verify_structure(dump: &Dump, out: &Path) -> anyhow::Result<()> {
    let reexported = export_path(out).with_context(|| format!("re-exporting {} for verification", out.display()))?;
    let differences = compare::compare(dump, &reexported);
    match compare::describe(&differences) {
        None => Ok(()),
        Some(report) => anyhow::bail!(
            "verification FAILED — the migrated database does not match its source.\n\
             The new file is left in place for inspection; the original was never written and remains usable.\n\
             {} difference(s):\n{report}",
            differences.len()
        ),
    }
}

/// `gymbuddy import` — load a dump into a fresh schema v2 database.
pub fn import(db: &Path, input: &Path) -> anyhow::Result<RowCounts> {
    let json = std::fs::read_to_string(input).with_context(|| format!("reading dump from {}", input.display()))?;
    let dump = super::from_json(&json)?;
    anyhow::ensure!(
        !super::is_legacy_database(db)?,
        "{} is a legacy (schema v1) database. Import targets a fresh schema v2 database; \
         use `gymbuddy migrate` to convert a legacy one.",
        db.display()
    );

    let counts = dump.row_counts();
    let target = Database::open(db).with_context(|| format!("opening schema v2 database at {}", db.display()))?;
    target.import_dump(&dump).context("importing the dump")?;
    verify_counts(&counts, &target.import_row_counts()?)?;
    Ok(counts)
}

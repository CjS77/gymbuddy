//! The `metrics` lookup table: one row per measurable quantity.
//!
//! # The join this fixes
//!
//! Schema v1 spelled the metric twice, as free text, in two tables that had to agree: `goals.metric`
//! and `body_metrics.metric`. A weightloss goal found its measurement series because both columns
//! happened to say `bodyweight_kg` — and when they did not, nothing failed. The goal simply reported
//! no progress, silently, forever. `canonical_body_metric` existed to make that convention hold by
//! normalising every write and every read.
//!
//! Both columns are now `metric_id` referencing this table, so a goal and its series either point at
//! the same row or the foreign key says so.
//!
//! # Names are still the interface
//!
//! Callers keep passing names, not ids. That is deliberate: names are what the user says, what the
//! dump format carries between databases (ids drift), and what `canonical_body_metric` knows how to
//! normalise. [`Database::get_or_create_metric`] is the single place a name becomes an id, and it
//! canonicalises on the way in — so "Body Weight", "weight" and `bodyweight_kg` still land on one
//! row, exactly as before, but now provably so.
//!
//! A metric nobody predicted is created on demand with a NULL unit. The known ones are pre-seeded
//! with their units from `backend/catalogue/`; adding a metric remains a row insert, never a
//! migration.

use anyhow::Context as _;
use rusqlite::{OptionalExtension as _, params};

use super::body_metrics::canonical_body_metric;
use super::database::Database;

impl Database {
    /// The id of the metric called `name`, creating the row if this is the first time anything has
    /// referred to it. The name is canonicalised first, so callers may pass whatever the user said.
    pub fn get_or_create_metric(&self, name: &str) -> anyhow::Result<i64> {
        let canonical = canonical_body_metric(name);
        // INSERT-then-SELECT rather than SELECT-then-INSERT: `metrics.name` is UNIQUE, so OR IGNORE
        // makes the pair idempotent without a transaction or a race between the two statements.
        self.conn()
            .execute("INSERT OR IGNORE INTO metrics (name) VALUES (?1)", params![canonical])
            .with_context(|| format!("creating metric `{canonical}`"))?;
        self.metric_id(&canonical)?.with_context(|| format!("metric `{canonical}` vanished immediately after being created"))
    }

    /// The id of an existing metric, or `None`. Use when the absence of the metric is itself the
    /// answer — a goal filtered by a metric nobody has ever measured has no rows, and creating the
    /// row to discover that would be a write on a read path.
    pub fn metric_id(&self, name: &str) -> anyhow::Result<Option<i64>> {
        self.conn()
            .query_row("SELECT id FROM metrics WHERE name = ?1", params![canonical_body_metric(name)], |row| row.get(0))
            .optional()
            .context("looking up a metric by name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creating_the_same_metric_twice_returns_one_row() {
        let db = Database::open_in_memory().unwrap();
        let first = db.get_or_create_metric("grip_strength_kg").unwrap();
        let second = db.get_or_create_metric("grip_strength_kg").unwrap();
        assert_eq!(first, second);
    }

    /// The v1 convention, now enforced rather than hoped for: every spelling of bodyweight resolves
    /// to the row the catalogue seeded, so a goal set as "weight" is judged against weigh-ins
    /// logged as "Body Weight".
    #[test]
    fn spellings_of_one_metric_all_resolve_to_the_seeded_row() {
        let db = Database::open_in_memory().unwrap();
        let seeded = db.metric_id("bodyweight_kg").unwrap().expect("bodyweight_kg is seeded from the catalogue");
        ["weight", "Body Weight", "bodyweight", "weight_kg"]
            .iter()
            .for_each(|spelling| assert_eq!(db.get_or_create_metric(spelling).unwrap(), seeded, "`{spelling}` should be bodyweight_kg"));
    }

    #[test]
    fn a_metric_that_was_never_created_has_no_id() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.metric_id("vertical_leap_cm").unwrap(), None);
    }

    /// The seeded metrics carry their unit; one invented at runtime cannot, since the name is all
    /// there is to go on.
    #[test]
    fn a_metric_created_on_demand_has_no_unit() {
        let db = Database::open_in_memory().unwrap();
        let id = db.get_or_create_metric("vertical_leap_cm").unwrap();
        let unit: Option<String> = db.conn().query_row("SELECT unit FROM metrics WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(unit, None);
    }
}

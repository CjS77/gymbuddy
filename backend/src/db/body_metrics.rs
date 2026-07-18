//! Body measurements: bodyweight, body fat, girths, resting heart rate. This is
//! health data about a real person's body — the most sensitive data in the schema
//! — so its retention and exposure are a deliberate policy, not a default.
//!
//! EXPOSURE — never unprompted. Body metrics are NOT injected into the system
//! prompt, summaries or dashboards merely because they exist. The one sanctioned
//! implicit exposure is goal progress: a metric-denominated goal (weightloss)
//! reads its current value from the user's latest measurement via
//! [`Database::latest_body_metric_value`], because setting that goal was the
//! user's explicit request to be coached on that number. Everything else —
//! series, latest readings — is returned only for an explicit user request
//! (e.g. the [C6.2] weight chart). Any new surface that wants to render a body
//! metric must clear the same bar: the user asked, or an active goal of theirs
//! needs it.
//!
//! RETENTION — user-controlled, not time-based. Rows are kept indefinitely: the
//! point of a weigh-in series is the long-term trend, and silently expiring rows
//! would destroy exactly the data the user chose to record. Erasure is explicit
//! and complete instead: [`Database::delete_body_metrics`] wipes a user's whole
//! series on request, and deleting the user cascades (`ON DELETE CASCADE`).
//!
//! Metric names are canonicalised on every write AND on every metric-keyed read
//! (see [`canonical_body_metric`]), so "Body Weight", "weight" and
//! "bodyweight_kg" land in — and find — the same series.

use anyhow::Context as _;
use rusqlite::{OptionalExtension as _, params};

use super::database::Database;
use super::models::BodyMetric;

/// Normalise a free-text metric name to its canonical, unit-suffixed form:
/// "weight" / "Body Weight" → "bodyweight_kg", "body fat" → "body_fat_pct",
/// "waist" → "waist_cm", "resting heart rate" → "resting_hr_bpm". Unknown names
/// pass through lowercased and snake_cased, so a new metric is a new row value,
/// never a code change — a goal and a measurement that agree on a name still
/// join. The unit suffix convention (kg / pct / cm / bpm) matches the metric
/// units used everywhere else in the schema (weight_kg, distance_m).
pub fn canonical_body_metric(raw: &str) -> String {
    let name = raw.trim().to_lowercase().replace([' ', '-'], "_");
    match name.as_str() {
        "weight" | "bodyweight" | "body_weight" | "weight_kg" | "body_weight_kg" => "bodyweight_kg".to_string(),
        "body_fat" | "bodyfat" | "bodyfat_pct" | "body_fat_percent" | "body_fat_percentage" => "body_fat_pct".to_string(),
        "waist" => "waist_cm".to_string(),
        "resting_hr" | "resting_heart_rate" | "rhr" => "resting_hr_bpm".to_string(),
        _ => name,
    }
}

fn row_to_body_metric(row: &rusqlite::Row) -> rusqlite::Result<BodyMetric> {
    Ok(BodyMetric { id: row.get(0)?, user_id: row.get(1)?, metric: row.get(2)?, value: row.get(3)?, measured_at: row.get(4)? })
}

const SELECT_METRIC: &str = "SELECT id, user_id, metric, value, measured_at FROM body_metrics";

impl Database {
    pub fn insert_body_metric(&self, metric: &BodyMetric) -> anyhow::Result<i64> {
        let name = canonical_body_metric(&metric.metric);
        self.conn().execute(
            "INSERT INTO body_metrics (user_id, metric, value, measured_at) VALUES (?1, ?2, ?3, COALESCE(?4, datetime('now')))",
            params![metric.user_id, name, metric.value, if metric.measured_at.is_empty() { None } else { Some(&metric.measured_at) }],
        )?;
        let id = self.conn().last_insert_rowid();
        tracing::debug!(id, metric = %name, value = metric.value, "DB: inserted body metric");
        Ok(id)
    }

    /// The most recent measurement of one metric, or `None` if never measured.
    pub fn latest_body_metric(&self, user_id: i64, metric: &str) -> anyhow::Result<Option<BodyMetric>> {
        let sql = format!("{SELECT_METRIC} WHERE user_id = ?1 AND metric = ?2 ORDER BY measured_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id, canonical_body_metric(metric)], row_to_body_metric)?;
        rows.next().transpose().context("Failed to read latest body metric")
    }

    /// The value a metric goal is judged against: the most recent measurement on or
    /// before `upto` (a `YYYY-MM-DD` date, typically the goal's target_date or the
    /// report period's end). `None` when nothing was measured by then.
    pub fn latest_body_metric_value(&self, user_id: i64, metric: &str, upto: &str) -> anyhow::Result<Option<f64>> {
        self.conn()
            .query_row(
                "SELECT value FROM body_metrics \
                 WHERE user_id = ?1 AND metric = ?2 AND date(measured_at) <= date(?3) \
                 ORDER BY measured_at DESC, id DESC LIMIT 1",
                params![user_id, canonical_body_metric(metric), upto],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to read latest body metric value")
    }

    /// One metric's series over a period, ascending by measurement time — the raw
    /// feed for the [C6.2] weight/composition charts. Multiple measurements on one
    /// day all appear; aggregation is the renderer's call.
    pub fn list_body_metrics(&self, user_id: i64, metric: &str, from: &str, to: &str) -> anyhow::Result<Vec<BodyMetric>> {
        let sql = format!(
            "{SELECT_METRIC} WHERE user_id = ?1 AND metric = ?2 AND date(measured_at) >= date(?3) AND date(measured_at) <= date(?4) \
             ORDER BY measured_at, id"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, canonical_body_metric(metric), from, to], row_to_body_metric)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list body metrics")
    }

    /// Erase a user's entire measurement history — the retention control (see the
    /// module docs). Returns how many rows were deleted.
    pub fn delete_body_metrics(&self, user_id: i64) -> anyhow::Result<usize> {
        let rows = self.conn().execute("DELETE FROM body_metrics WHERE user_id = ?1", params![user_id])?;
        tracing::debug!(user_id, rows, "DB: erased body metric history");
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{new_body_metric, new_user};
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn insert_at(db: &Database, user_id: i64, metric: &str, value: f64, measured_at: &str) {
        let mut m = new_body_metric(user_id, metric, value);
        m.measured_at = measured_at.to_string();
        db.insert_body_metric(&m).unwrap();
    }

    #[test]
    fn insert_and_read_latest() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        insert_at(&db, user_id, "bodyweight_kg", 84.0, "2026-01-01 08:00:00");
        insert_at(&db, user_id, "bodyweight_kg", 82.5, "2026-02-01 08:00:00");

        let latest = db.latest_body_metric(user_id, "bodyweight_kg").unwrap().unwrap();
        assert_eq!(latest.value, 82.5);
        assert_eq!(latest.metric, "bodyweight_kg");
    }

    #[test]
    fn metric_names_are_canonicalised_on_write_and_read() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        // "Body Weight" and "weight" must land in — and find — the bodyweight_kg series.
        db.insert_body_metric(&new_body_metric(user_id, "Body Weight", 83.0)).unwrap();
        let latest = db.latest_body_metric(user_id, "weight").unwrap().unwrap();
        assert_eq!(latest.metric, "bodyweight_kg");
        assert_eq!(latest.value, 83.0);

        // Unknown metrics pass through snake_cased rather than being rejected.
        assert_eq!(canonical_body_metric("Resting HR"), "resting_hr_bpm");
        assert_eq!(canonical_body_metric("hip circumference"), "hip_circumference");
    }

    #[test]
    fn latest_value_respects_the_date_bound() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        insert_at(&db, user_id, "bodyweight_kg", 84.0, "2026-01-15 08:00:00");
        insert_at(&db, user_id, "bodyweight_kg", 82.5, "2026-03-01 08:00:00");

        assert_eq!(db.latest_body_metric_value(user_id, "bodyweight_kg", "2026-02-01").unwrap(), Some(84.0));
        assert_eq!(db.latest_body_metric_value(user_id, "bodyweight_kg", "2026-03-01").unwrap(), Some(82.5));
        assert_eq!(db.latest_body_metric_value(user_id, "bodyweight_kg", "2026-01-01").unwrap(), None);
    }

    #[test]
    fn series_is_ascending_and_period_bounded() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        insert_at(&db, user_id, "bodyweight_kg", 84.0, "2026-01-01 08:00:00");
        insert_at(&db, user_id, "bodyweight_kg", 83.2, "2026-02-01 08:00:00");
        insert_at(&db, user_id, "bodyweight_kg", 82.5, "2026-03-01 08:00:00");
        insert_at(&db, user_id, "body_fat_pct", 19.0, "2026-02-01 08:00:00");

        let series = db.list_body_metrics(user_id, "bodyweight_kg", "2026-01-15", "2026-03-31").unwrap();
        let values: Vec<f64> = series.iter().map(|m| m.value).collect();
        assert_eq!(values, vec![83.2, 82.5], "period-bounded, ascending, single metric only");
    }

    #[test]
    fn metrics_are_isolated_per_user() {
        let db = test_db();
        let alice = db.insert_user(&new_user("Alice", None, "UTC")).unwrap();
        let bob = db.insert_user(&new_user("Bob", None, "UTC")).unwrap();

        insert_at(&db, alice, "bodyweight_kg", 62.0, "2026-01-01 08:00:00");
        insert_at(&db, bob, "bodyweight_kg", 91.0, "2026-01-01 08:00:00");

        assert_eq!(db.latest_body_metric(alice, "bodyweight_kg").unwrap().unwrap().value, 62.0);
        assert_eq!(db.latest_body_metric(bob, "bodyweight_kg").unwrap().unwrap().value, 91.0);
    }

    #[test]
    fn delete_erases_the_whole_history() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        insert_at(&db, user_id, "bodyweight_kg", 84.0, "2026-01-01 08:00:00");
        insert_at(&db, user_id, "body_fat_pct", 18.0, "2026-01-01 08:00:00");

        assert_eq!(db.delete_body_metrics(user_id).unwrap(), 2);
        assert!(db.latest_body_metric(user_id, "bodyweight_kg").unwrap().is_none());
        assert!(db.latest_body_metric(user_id, "body_fat_pct").unwrap().is_none());
    }

    #[test]
    fn empty_measured_at_defaults_to_now() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        let mut m = new_body_metric(user_id, "bodyweight_kg", 82.5);
        m.measured_at = String::new();
        db.insert_body_metric(&m).unwrap();

        let latest = db.latest_body_metric(user_id, "bodyweight_kg").unwrap().unwrap();
        assert!(!latest.measured_at.is_empty());
    }
}

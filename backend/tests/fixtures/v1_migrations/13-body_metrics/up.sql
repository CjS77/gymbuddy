-- Body measurements: bodyweight, body fat, girths, resting heart rate.
--
-- Long-shaped on purpose: one row per (user, metric, moment), never a column per
-- metric. The metric set will grow (waist, resting HR, ...) and each addition
-- must be a new row value, not a schema migration. `metric` is free text with a
-- unit-suffixed naming convention (bodyweight_kg, body_fat_pct, waist_cm,
-- resting_hr_bpm) — the SAME names `goals.metric` uses, so a metric-denominated
-- goal (weightloss) joins straight onto its measurement series. No CHECK on
-- `metric`: constraining the set here would turn every new metric back into a
-- migration.
--
-- `value` is always in the metric's canonical unit (the unit its name carries).
-- This is the most sensitive data in the schema; retention and exposure policy
-- is documented in backend/src/db/body_metrics.rs.

CREATE TABLE body_metrics (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    metric      TEXT NOT NULL,
    value       REAL NOT NULL,
    measured_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_body_metrics_user_metric ON body_metrics(user_id, metric, measured_at);

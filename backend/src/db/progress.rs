use anyhow::Context as _;
use rusqlite::{OptionalExtension as _, params};

use super::database::Database;
use super::models::{Goal, GoalDirection, GoalKind, GoalProgress, GoalStatus, MeasurementType, MuscleRecovery, TimeSeries, TimeSeriesPoint};

/// Fraction of the target reached, as a percentage, honouring `direction`. For an
/// increase goal this is `current / target`; for a decrease goal (weightloss, a
/// faster time) it inverts to `target / current`, reaching 100% as the value falls
/// to the target. `None` current or a zero target yields 0%.
fn goal_percentage(direction: GoalDirection, current: Option<f64>, target: f64) -> f64 {
    let Some(cv) = current else { return 0.0 };
    if target == 0.0 {
        return 0.0;
    }
    match direction {
        GoalDirection::Increase => (cv / target) * 100.0,
        GoalDirection::Decrease if cv <= target => 100.0,
        GoalDirection::Decrease => (target / cv) * 100.0,
    }
}

impl Database {
    /// Time series of best-set value per day for a single exercise_type.
    /// When `include_descendants` is true, sets logged against descendants of
    /// `exercise_type_id` are also included (useful for non-leaf nodes).
    pub fn exercise_time_series(
        &self,
        user_id: i64,
        exercise_type_id: i64,
        from: Option<&str>,
        to: Option<&str>,
        include_descendants: bool,
    ) -> anyhow::Result<Vec<TimeSeriesPoint>> {
        let default_from = (chrono::Utc::now() - chrono::Duration::days(365)).format("%Y-%m-%d").to_string();
        let default_to = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let from = from.unwrap_or(&default_from);
        let to = to.unwrap_or(&default_to);

        let sql = if include_descendants {
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM exercise_types WHERE id = ?2 \
                 UNION ALL \
                 SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
             ) \
             SELECT date(s.logged_at) AS d, MAX(s.value) AS value \
             FROM sets s \
             JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 AND s.exercise_type_id IN (SELECT id FROM tree) \
               AND s.logged_at >= ?3 AND s.logged_at <= ?4 \
             GROUP BY date(s.logged_at) ORDER BY date(s.logged_at)"
        } else {
            "SELECT date(s.logged_at) AS d, MAX(s.value) AS value \
             FROM sets s \
             JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 AND s.exercise_type_id = ?2 \
               AND s.logged_at >= ?3 AND s.logged_at <= ?4 \
             GROUP BY date(s.logged_at) ORDER BY date(s.logged_at)"
        };

        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt
            .query_map(params![user_id, exercise_type_id, from, to], |row| Ok(TimeSeriesPoint { date: row.get(0)?, value: row.get(1)? }))?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to query exercise_type time series")
    }

    /// Time series for every exercise_type that has logged sets within a given
    /// muscle_group's subtree, in the supplied period.
    pub fn muscle_group_time_series(
        &self,
        user_id: i64,
        muscle_group: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> anyhow::Result<Vec<TimeSeries>> {
        let default_from = (chrono::Utc::now() - chrono::Duration::days(365)).format("%Y-%m-%d").to_string();
        let default_to = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let from_str = from.unwrap_or(&default_from);
        let to_str = to.unwrap_or(&default_to);

        let mut stmt = self.conn().prepare(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM exercise_types WHERE name = ?2 COLLATE NOCASE AND level = 'muscle_group' \
                 UNION ALL \
                 SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
             ) \
             SELECT DISTINCT et.id, et.name, et.measurement_type_id \
             FROM exercise_types et \
             JOIN sets s ON s.exercise_type_id = et.id \
             JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
             WHERE et.id IN (SELECT id FROM tree) \
               AND ee.user_id = ?1 \
               AND s.logged_at >= ?3 AND s.logged_at <= ?4",
        )?;

        let exercise_info: Vec<(i64, String, Option<i64>)> = stmt
            .query_map(params![user_id, muscle_group, from_str, to_str], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to discover exercise_types in muscle group")?;

        exercise_info
            .into_iter()
            .map(|(et_id, et_name, mt_id)| {
                let points = self.exercise_time_series(user_id, et_id, Some(from_str), Some(to_str), false)?;
                let mt = mt_id.map(MeasurementType::from_id).unwrap_or(MeasurementType::WeightReps);
                Ok(TimeSeries { exercise_type_id: et_id, exercise_name: et_name, measurement_type: mt, points })
            })
            .collect()
    }

    /// Time series for all exercise_types that have a goal overlapping the period.
    pub fn goal_time_series(&self, user_id: i64, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Vec<TimeSeries>> {
        let default_from = (chrono::Utc::now() - chrono::Duration::days(365)).format("%Y-%m-%d").to_string();
        let default_to = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let from_str = from.unwrap_or(&default_from);
        let to_str = to.unwrap_or(&default_to);

        let mut stmt = self.conn().prepare(
            "SELECT DISTINCT et.id, et.name, et.measurement_type_id \
             FROM goals g \
             JOIN exercise_types et ON g.exercise_type_id = et.id \
             WHERE g.user_id = ?1 \
               AND g.start_date <= ?3 AND (g.target_date IS NULL OR g.target_date >= ?2)",
        )?;

        let exercise_info: Vec<(i64, String, Option<i64>)> = stmt
            .query_map(params![user_id, from_str, to_str], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to discover goal exercises")?;

        exercise_info
            .into_iter()
            .map(|(et_id, et_name, mt_id)| {
                let points = self.exercise_time_series(user_id, et_id, Some(from_str), Some(to_str), true)?;
                let mt = mt_id.map(MeasurementType::from_id).unwrap_or(MeasurementType::WeightReps);
                Ok(TimeSeries { exercise_type_id: et_id, exercise_name: et_name, measurement_type: mt, points })
            })
            .collect()
    }

    /// Goal progress report for a period.
    pub fn goal_progress_report(&self, user_id: i64, from: Option<&str>, to: Option<&str>) -> anyhow::Result<Vec<GoalProgress>> {
        let default_from = (chrono::Utc::now() - chrono::Duration::days(365)).format("%Y-%m-%d").to_string();
        let default_to = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let from_str = from.unwrap_or(&default_from);
        let to_str = to.unwrap_or(&default_to);
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // LEFT JOIN so non-exercise goals (metric-denominated) still surface; their
        // exercise_type_id is NULL and et.* comes back NULL.
        let mut stmt = self.conn().prepare(
            "SELECT g.id, g.user_id, g.kind, g.exercise_type_id, \
                    (SELECT name FROM metrics WHERE metrics.id = g.metric_id) AS metric, \
                    g.target_value, g.direction, \
                    g.priority, g.start_date, g.target_date, g.achieved, g.notes, g.created_at, g.updated_at, \
                    et.name AS exercise_name \
             FROM goals g \
             LEFT JOIN exercise_types et ON g.exercise_type_id = et.id \
             WHERE g.user_id = ?1 \
               AND g.start_date <= ?3 AND (g.target_date IS NULL OR g.target_date >= ?2) \
             ORDER BY g.priority DESC, g.start_date",
        )?;

        let goals_with_info: Vec<(Goal, String)> = stmt
            .query_map(params![user_id, from_str, to_str], |row| {
                let goal = Goal {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    kind: GoalKind::from_str_loose(&row.get::<_, String>(2)?),
                    exercise_type_id: row.get(3)?,
                    metric: row.get(4)?,
                    target_value: row.get(5)?,
                    direction: GoalDirection::from_str_loose(&row.get::<_, String>(6)?),
                    priority: row.get(7)?,
                    start_date: row.get(8)?,
                    target_date: row.get(9)?,
                    achieved: row.get::<_, i32>(10)? != 0,
                    notes: row.get(11)?,
                    created_at: row.get(12)?,
                    updated_at: row.get(13)?,
                };
                // Subject label: the exercise name, else the metric, else the kind.
                let exercise_name: Option<String> = row.get(14)?;
                let label = exercise_name.or_else(|| goal.metric.clone()).unwrap_or_else(|| goal.kind.to_string());
                Ok((goal, label))
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to query goals")?;

        goals_with_info
            .into_iter()
            .map(|(goal, exercise_name)| {
                // Exercise goals derive their current value from logged sets; metric
                // goals (weightloss) read the user's latest body measurement up to the
                // goal window's end. A metric with no measurement series (habit metrics
                // like sessions_per_week) still reports None.
                let goal_end = goal.target_date.as_deref().unwrap_or(to_str);
                let current_value = match (goal.exercise_type_id, goal.metric.as_deref()) {
                    (Some(et_id), _) => self.relevant_value_for_exercise_type(user_id, et_id, goal.direction, &goal.start_date, goal_end)?,
                    (None, Some(metric)) => self.latest_body_metric_value(user_id, metric, goal_end)?,
                    (None, None) => None,
                };

                let percentage = goal_percentage(goal.direction, current_value, goal.target_value);

                let status = if goal.achieved || percentage >= 100.0 {
                    GoalStatus::Achieved
                } else if goal.target_date.as_deref().is_some_and(|td| td < today.as_str()) {
                    GoalStatus::Failed
                } else {
                    GoalStatus::Active
                };

                Ok(GoalProgress { goal, exercise_name, status, current_value, percentage })
            })
            .collect()
    }

    /// Per top-level muscle group: when it was last trained (any exercise in its
    /// subtree, rolled up via [`Database::descendant_ids_inclusive`]) and how many
    /// sets that most-recent day involved. Every muscle group appears — one never
    /// trained comes back with `last_trained: None`, the strongest rest signal for
    /// the session designer (see `build_designer_prompt`). Volume is counted per
    /// calendar day, matching the daily grain the other aggregates here use.
    pub fn muscle_recovery(&self, user_id: i64) -> anyhow::Result<Vec<MuscleRecovery>> {
        self.list_top_level_groups()?
            .into_iter()
            .map(|group| {
                let ids = self.descendant_ids_inclusive(group.id)?;
                let placeholders = vec!["?"; ids.len()].join(",");
                let sql = format!(
                    "WITH g_days(d) AS ( \
                         SELECT date(s.logged_at) \
                         FROM sets s JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
                         WHERE ee.user_id = ? AND s.exercise_type_id IN ({placeholders}) \
                     ) \
                     SELECT (SELECT MAX(d) FROM g_days) AS last_trained, \
                            (SELECT COUNT(*) FROM g_days WHERE d = (SELECT MAX(d) FROM g_days)) AS vol"
                );
                let args: Vec<i64> = std::iter::once(user_id).chain(ids).collect();
                let (last_trained, last_volume_sets) = self
                    .conn()
                    .query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
                        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
                    })
                    .context("Failed to query muscle recovery")?;
                Ok(MuscleRecovery { muscle_group: group.name, last_trained, last_volume_sets })
            })
            .collect()
    }

    /// The value a goal is judged against, rolled up over the exercise's subtree so a
    /// goal on a parent node reflects sets logged at any depth. Increase goals take the
    /// MAX (best is highest); decrease goals take the MIN (best is lowest).
    fn relevant_value_for_exercise_type(
        &self,
        user_id: i64,
        exercise_type_id: i64,
        direction: GoalDirection,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<f64>> {
        let agg = match direction {
            GoalDirection::Increase => "MAX",
            GoalDirection::Decrease => "MIN",
        };
        let sql = format!(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM exercise_types WHERE id = ?2 \
                 UNION ALL \
                 SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
             ) \
             SELECT {agg}(s.value) FROM sets s \
             JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 AND s.exercise_type_id IN (SELECT id FROM tree) \
               AND s.logged_at >= ?3 AND s.logged_at <= ?4"
        );
        self.conn()
            .query_row(&sql, params![user_id, exercise_type_id, from, to], |row| row.get(0))
            .optional()
            .context("Failed to query relevant value")
            .map(|v| v.flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{
        GoalDirection, GoalKind, MeasurementType, new_body_metric, new_exercise_entry, new_exercise_goal, new_exercise_set, new_goal,
        new_user,
    };
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn log_weight_set(db: &Database, user_id: i64, exercise_type_id: i64, logged_at: &str, weight: f64) {
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, exercise_type_id, MeasurementType::WeightReps, weight);
        s.count = Some(8);
        s.logged_at = logged_at.to_string();
        db.insert_set(&s).unwrap();
    }

    #[test]
    fn exercise_time_series_returns_daily_points() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        for (day, weight) in [(1, 60.0), (2, 65.0), (3, 70.0)] {
            log_weight_set(&db, user_id, bench.id, &format!("2025-06-{day:02} 10:00:00"), weight);
        }

        let points = db.exercise_time_series(user_id, bench.id, Some("2025-06-01"), Some("2025-06-30"), false).unwrap();
        assert_eq!(points.len(), 3);
        assert!(points[0].value < points[2].value);
    }

    #[test]
    fn time_based_exercise_uses_value_directly() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let plank = db.get_exercise_type_by_name("Plank").unwrap().unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, plank.id, MeasurementType::TimeBased, 120.0);
        s.logged_at = "2025-06-01 10:00:00".into();
        db.insert_set(&s).unwrap();

        let points = db.exercise_time_series(user_id, plank.id, Some("2025-06-01"), Some("2025-06-30"), false).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].value, 120.0);
    }

    #[test]
    fn muscle_group_time_series_groups_exercises() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let fly = db.get_exercise_type_by_name("Chest Fly").unwrap().unwrap();
        log_weight_set(&db, user_id, bp.id, "2025-06-01 10:00:00", 60.0);
        log_weight_set(&db, user_id, fly.id, "2025-06-02 10:00:00", 20.0);

        let series = db.muscle_group_time_series(user_id, "Chest", Some("2025-06-01"), Some("2025-06-30")).unwrap();
        assert!(series.len() >= 2, "expected ≥2 series, got {}", series.len());
    }

    #[test]
    fn goal_time_series_includes_goal_exercises() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let mut goal = new_exercise_goal(user_id, bp.id, 100.0);
        goal.start_date = "2025-01-01".into();
        db.insert_goal(&goal).unwrap();

        log_weight_set(&db, user_id, bp.id, "2025-06-01 10:00:00", 80.0);

        let series = db.goal_time_series(user_id, Some("2025-01-01"), Some("2025-12-31")).unwrap();
        assert_eq!(series.len(), 1);
    }

    #[test]
    fn goal_progress_report_computes_percentages() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let mut goal = new_exercise_goal(user_id, bp.id, 100.0);
        goal.start_date = "2025-01-01".into();
        goal.target_date = Some("2025-12-31".into());
        db.insert_goal(&goal).unwrap();

        log_weight_set(&db, user_id, bp.id, "2025-06-01 10:00:00", 80.0);

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2025-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert!((report[0].percentage - 80.0).abs() < 0.01);
    }

    #[test]
    fn goal_progress_report_derives_status() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let dl = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap();

        let mut achieved = new_exercise_goal(user_id, bp.id, 100.0);
        achieved.start_date = "2025-01-01".into();
        achieved.target_date = Some("2025-12-31".into());
        achieved.achieved = true;
        db.insert_goal(&achieved).unwrap();

        let mut failed = new_exercise_goal(user_id, dl.id, 200.0);
        failed.start_date = "2024-01-01".into();
        failed.target_date = Some("2024-06-01".into());
        db.insert_goal(&failed).unwrap();

        let report = db.goal_progress_report(user_id, Some("2024-01-01"), Some("2025-12-31")).unwrap();
        assert!(report.iter().any(|r| r.status == GoalStatus::Achieved));
        assert!(report.iter().any(|r| r.status == GoalStatus::Failed));
    }

    #[test]
    fn muscle_recovery_lists_every_group_including_untrained() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        let recovery = db.muscle_recovery(user_id).unwrap();
        assert_eq!(recovery.len(), 7, "all seven muscle groups must appear, trained or not");
        assert!(
            recovery.iter().all(|r| r.last_trained.is_none() && r.last_volume_sets == 0),
            "an untrained user leaves every group with no last-trained date and zero volume",
        );
    }

    #[test]
    fn muscle_recovery_reports_last_day_and_its_volume() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        // One set two days earlier, two sets on the most recent day — Bench Press
        // rolls up to Chest via the ancestry helper.
        log_weight_set(&db, user_id, bench.id, "2025-06-01 10:00:00", 60.0);
        log_weight_set(&db, user_id, bench.id, "2025-06-03 10:00:00", 62.0);
        log_weight_set(&db, user_id, bench.id, "2025-06-03 10:05:00", 62.0);

        let recovery = db.muscle_recovery(user_id).unwrap();
        let chest = recovery.iter().find(|r| r.muscle_group == "Chest").unwrap();
        assert_eq!(chest.last_trained.as_deref(), Some("2025-06-03"));
        assert_eq!(chest.last_volume_sets, 2, "volume counts only the most-recent training day");

        // A group never trained stays a strong rest signal, not an omission.
        let back = recovery.iter().find(|r| r.muscle_group == "Back").unwrap();
        assert!(back.last_trained.is_none());
        assert_eq!(back.last_volume_sets, 0);
    }

    #[test]
    fn goal_progress_zero_target_returns_zero_percent() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let mut goal = new_exercise_goal(user_id, bp.id, 0.0);
        goal.start_date = "2025-01-01".into();
        db.insert_goal(&goal).unwrap();

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2025-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].percentage, 0.0);
    }

    #[test]
    fn decrease_goal_reports_progress_from_the_low_side() {
        // A faster-time goal on a timed exercise: target 60s, currently best (lowest) 75s.
        // Progress must be target/current = 80%, and the MIN — not the MAX — is taken.
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let plank = db.get_exercise_type_by_name("Plank").unwrap().unwrap();

        for (day, secs) in [(1, 90.0), (2, 75.0), (3, 100.0)] {
            let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
            let mut s = new_exercise_set(entry_id, plank.id, MeasurementType::TimeBased, secs);
            s.logged_at = format!("2025-06-{day:02} 10:00:00");
            db.insert_set(&s).unwrap();
        }

        let mut goal = new_goal(user_id, GoalKind::Endurance, Some(plank.id), None, 60.0, GoalDirection::Decrease);
        goal.start_date = "2025-01-01".into();
        goal.target_date = Some("2027-12-31".into()); // future deadline, so status stays Active
        db.insert_goal(&goal).unwrap();

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2027-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].current_value, Some(75.0), "decrease goal should use the lowest logged value");
        assert!((report[0].percentage - 80.0).abs() < 0.01, "expected 80%, got {}", report[0].percentage);
        assert_eq!(report[0].status, GoalStatus::Active);
    }

    #[test]
    fn decrease_goal_is_achieved_once_value_falls_to_target() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let plank = db.get_exercise_type_by_name("Plank").unwrap().unwrap();

        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, plank.id, MeasurementType::TimeBased, 55.0);
        s.logged_at = "2025-06-01 10:00:00".into();
        db.insert_set(&s).unwrap();

        let mut goal = new_goal(user_id, GoalKind::Endurance, Some(plank.id), None, 60.0, GoalDirection::Decrease);
        goal.start_date = "2025-01-01".into();
        db.insert_goal(&goal).unwrap();

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2025-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert!(report[0].percentage >= 100.0);
        assert_eq!(report[0].status, GoalStatus::Achieved);
    }

    #[test]
    fn metric_goal_surfaces_without_an_exercise() {
        // A bodyweight goal has no exercise; it must still appear. With no
        // measurements logged yet its current value stays None.
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        let mut goal = new_goal(user_id, GoalKind::BodyComposition, None, Some("bodyweight_kg".into()), 80.0, GoalDirection::Decrease);
        goal.start_date = "2025-01-01".into();
        db.insert_goal(&goal).unwrap();

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2025-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].exercise_name, "bodyweight_kg");
        assert_eq!(report[0].current_value, None);
        assert_eq!(report[0].status, GoalStatus::Active);
    }

    #[test]
    fn weightloss_goal_reads_the_latest_body_metric() {
        // Target 80kg from 90kg; the LATEST weigh-in (85kg), not the lowest, is
        // the current value — you are judged against where you are now.
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        for (date, kg) in [("2025-01-05 08:00:00", 90.0), ("2025-02-01 08:00:00", 84.0), ("2025-03-01 08:00:00", 85.0)] {
            let mut m = new_body_metric(user_id, "weight", kg);
            m.measured_at = date.into();
            db.insert_body_metric(&m).unwrap();
        }

        let mut goal = new_goal(user_id, GoalKind::Bodyweight, None, Some("bodyweight_kg".into()), 80.0, GoalDirection::Decrease);
        goal.start_date = "2025-01-01".into();
        goal.target_date = Some("2027-12-31".into()); // future deadline, so status stays Active
        db.insert_goal(&goal).unwrap();

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2027-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].current_value, Some(85.0), "latest measurement, joined through the canonical metric name");
        assert!((report[0].percentage - (80.0 / 85.0 * 100.0)).abs() < 0.01);
        assert_eq!(report[0].status, GoalStatus::Active);
    }

    #[test]
    fn weightloss_goal_is_achieved_once_weight_falls_to_target() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        let mut m = new_body_metric(user_id, "bodyweight_kg", 79.5);
        m.measured_at = "2025-06-01 08:00:00".into();
        db.insert_body_metric(&m).unwrap();

        let mut goal = new_goal(user_id, GoalKind::Bodyweight, None, Some("bodyweight_kg".into()), 80.0, GoalDirection::Decrease);
        goal.start_date = "2025-01-01".into();
        db.insert_goal(&goal).unwrap();

        let report = db.goal_progress_report(user_id, Some("2025-01-01"), Some("2025-12-31")).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].current_value, Some(79.5));
        assert!(report[0].percentage >= 100.0);
        assert_eq!(report[0].status, GoalStatus::Achieved);
    }
}

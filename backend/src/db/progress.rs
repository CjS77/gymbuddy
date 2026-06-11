use anyhow::Context as _;
use rusqlite::{OptionalExtension as _, params};

use super::database::Database;
use super::models::{ExerciseGoal, GoalProgress, GoalStatus, MeasurementType, TimeSeries, TimeSeriesPoint};

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
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
             WHERE ee.user_id = ?1 AND s.exercise_type_id IN (SELECT id FROM tree) \
               AND s.logged_at >= ?3 AND s.logged_at <= ?4 \
             GROUP BY date(s.logged_at) ORDER BY date(s.logged_at)"
        } else {
            "SELECT date(s.logged_at) AS d, MAX(s.value) AS value \
             FROM sets s \
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
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
             JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
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
             FROM exercise_goals g \
             JOIN exercise_types et ON g.exercise_type_id = et.id \
             WHERE g.user_id = ?1 \
               AND g.start_date <= ?3 AND (g.end_date IS NULL OR g.end_date >= ?2)",
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

        let mut stmt = self.conn().prepare(
            "SELECT g.id, g.user_id, g.exercise_type_id, g.target_value, g.start_date, g.end_date, \
                    g.achieved, g.notes, g.created_at, g.updated_at, \
                    et.name AS exercise_name, et.measurement_type_id \
             FROM exercise_goals g \
             JOIN exercise_types et ON g.exercise_type_id = et.id \
             WHERE g.user_id = ?1 \
               AND g.start_date <= ?3 AND (g.end_date IS NULL OR g.end_date >= ?2) \
             ORDER BY g.start_date",
        )?;

        let goals_with_info: Vec<(ExerciseGoal, String, MeasurementType)> = stmt
            .query_map(params![user_id, from_str, to_str], |row| {
                let goal = ExerciseGoal {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    exercise_type_id: row.get(2)?,
                    target_value: row.get(3)?,
                    start_date: row.get(4)?,
                    end_date: row.get(5)?,
                    achieved: row.get::<_, i32>(6)? != 0,
                    notes: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                };
                let exercise_name: String = row.get(10)?;
                let mt_id: Option<i64> = row.get(11)?;
                let mt = mt_id.map(MeasurementType::from_id).unwrap_or(MeasurementType::WeightReps);
                Ok((goal, exercise_name, mt))
            })?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to query goals")?;

        goals_with_info
            .into_iter()
            .map(|(goal, exercise_name, _mt)| {
                let goal_end = goal.end_date.as_deref().unwrap_or(to_str);
                let current_value = self.best_value_for_exercise_type(user_id, goal.exercise_type_id, &goal.start_date, goal_end)?;

                let percentage = match (current_value, goal.target_value) {
                    (_, 0.0) => 0.0,
                    (Some(cv), tv) => (cv / tv) * 100.0,
                    (None, _) => 0.0,
                };

                let status = if goal.achieved || percentage >= 100.0 {
                    GoalStatus::Achieved
                } else if goal.end_date.as_deref().is_some_and(|ed| ed < today.as_str()) {
                    GoalStatus::Failed
                } else {
                    GoalStatus::Active
                };

                Ok(GoalProgress { goal, exercise_name, status, current_value, percentage })
            })
            .collect()
    }

    fn best_value_for_exercise_type(&self, user_id: i64, exercise_type_id: i64, from: &str, to: &str) -> anyhow::Result<Option<f64>> {
        // Roll up descendants so a goal on a parent node reflects sets logged at any depth.
        let sql = "WITH RECURSIVE tree(id) AS ( \
                       SELECT id FROM exercise_types WHERE id = ?2 \
                       UNION ALL \
                       SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
                   ) \
                   SELECT MAX(s.value) FROM sets s \
                   JOIN exercise_entry ee ON s.exercise_entry_id = ee.id \
                   WHERE ee.user_id = ?1 AND s.exercise_type_id IN (SELECT id FROM tree) \
                     AND s.logged_at >= ?3 AND s.logged_at <= ?4";
        self.conn()
            .query_row(sql, params![user_id, exercise_type_id, from, to], |row| row.get(0))
            .optional()
            .context("Failed to query best value")
            .map(|v| v.flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{MeasurementType, new_exercise_entry, new_exercise_goal, new_exercise_set, new_user};
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
        goal.end_date = Some("2025-12-31".into());
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
        achieved.end_date = Some("2025-12-31".into());
        achieved.achieved = true;
        db.insert_goal(&achieved).unwrap();

        let mut failed = new_exercise_goal(user_id, dl.id, 200.0);
        failed.start_date = "2024-01-01".into();
        failed.end_date = Some("2024-06-01".into());
        db.insert_goal(&failed).unwrap();

        let report = db.goal_progress_report(user_id, Some("2024-01-01"), Some("2025-12-31")).unwrap();
        assert!(report.iter().any(|r| r.status == GoalStatus::Achieved));
        assert!(report.iter().any(|r| r.status == GoalStatus::Failed));
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
}

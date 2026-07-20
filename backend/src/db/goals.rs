use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{Goal, GoalDirection, GoalKind};

pub(super) fn row_to_goal(row: &rusqlite::Row) -> rusqlite::Result<Goal> {
    Ok(Goal {
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
    })
}

/// `metric` reaches the caller as a name. It comes out of a correlated subquery rather than a join
/// so that every `WHERE` clause built on top of this string keeps working with unqualified column
/// names — a join would make bare `id` ambiguous between `goals` and `metrics`.
pub(super) const SELECT_GOAL: &str = "\
    SELECT id, user_id, kind, exercise_type_id, \
           (SELECT name FROM metrics WHERE metrics.id = goals.metric_id) AS metric, \
           target_value, direction, priority, \
           start_date, target_date, achieved, notes, created_at, updated_at \
    FROM goals";

impl Database {
    pub fn insert_goal(&self, goal: &Goal) -> anyhow::Result<i64> {
        // A metric-denominated goal interns its metric on the way in, so it and the weigh-ins it is
        // judged against necessarily point at one row.
        let metric_id = goal.metric.as_deref().map(|name| self.get_or_create_metric(name)).transpose()?;
        self.conn().execute(
            "INSERT INTO goals (user_id, kind, exercise_type_id, metric_id, target_value, direction, priority, \
                                start_date, target_date, achieved, notes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                goal.user_id,
                goal.kind.as_str(),
                goal.exercise_type_id,
                metric_id,
                goal.target_value,
                goal.direction.as_str(),
                goal.priority,
                goal.start_date,
                goal.target_date,
                goal.achieved as i32,
                goal.notes,
            ],
        )?;
        let id = self.conn().last_insert_rowid();
        tracing::debug!(id, kind = %goal.kind, exercise_type_id = ?goal.exercise_type_id, target = goal.target_value, "DB: inserted goal");
        Ok(id)
    }

    pub fn get_goal(&self, id: i64) -> anyhow::Result<Option<Goal>> {
        let sql = format!("{SELECT_GOAL} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_goal)?;
        rows.next().transpose().context("Failed to read goal row")
    }

    pub fn list_active_goals(&self, user_id: i64) -> anyhow::Result<Vec<Goal>> {
        let sql = format!("{SELECT_GOAL} WHERE user_id = ?1 AND achieved = 0 ORDER BY start_date");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id], row_to_goal)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list active goals")
    }

    pub fn list_goals_in_period(&self, user_id: i64, from: &str, to: &str) -> anyhow::Result<Vec<Goal>> {
        let sql = format!(
            "{SELECT_GOAL} WHERE user_id = ?1 AND start_date <= ?3 AND (target_date IS NULL OR target_date >= ?2) ORDER BY start_date"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, from, to], row_to_goal)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list goals in period")
    }

    pub fn mark_goal_achieved(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("UPDATE goals SET achieved = 1, updated_at = datetime('now') WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "Goal with id {id} not found");
        Ok(())
    }

    pub fn delete_goal(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM goals WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "Goal with id {id} not found");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{GoalDirection, GoalKind, new_exercise_goal, new_goal, new_user};
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_list_active_goals() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let mut goal = new_exercise_goal(user_id, bp.id, 100.0);
        goal.start_date = "2025-01-01".into();
        db.insert_goal(&goal).unwrap();

        let goals = db.list_active_goals(user_id).unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].target_value, 100.0);
    }

    #[test]
    fn list_goals_in_period() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let dl = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap();

        let mut g1 = new_exercise_goal(user_id, bp.id, 100.0);
        g1.start_date = "2025-01-01".into();
        g1.target_date = Some("2025-06-01".into());
        db.insert_goal(&g1).unwrap();

        let mut g2 = new_exercise_goal(user_id, dl.id, 50.0);
        g2.start_date = "2025-07-01".into();
        g2.target_date = Some("2025-12-01".into());
        db.insert_goal(&g2).unwrap();

        let goals = db.list_goals_in_period(user_id, "2025-01-01", "2025-06-30").unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].target_value, 100.0);
    }

    #[test]
    fn mark_goal_achieved() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let goal_id = db.insert_goal(&new_exercise_goal(user_id, bp.id, 100.0)).unwrap();
        db.mark_goal_achieved(goal_id).unwrap();

        let fetched = db.get_goal(goal_id).unwrap().unwrap();
        assert!(fetched.achieved);

        let active = db.list_active_goals(user_id).unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn insert_and_read_metric_goal_round_trips_new_fields() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        // A weightloss goal: no exercise, denominated in a metric, decreasing.
        let mut goal = new_goal(user_id, GoalKind::BodyComposition, None, Some("bodyweight_kg".into()), 80.0, GoalDirection::Decrease);
        goal.priority = 5;
        goal.target_date = Some("2026-01-01".into());
        let id = db.insert_goal(&goal).unwrap();

        let fetched = db.get_goal(id).unwrap().unwrap();
        assert_eq!(fetched.kind, GoalKind::BodyComposition);
        assert_eq!(fetched.exercise_type_id, None);
        assert_eq!(fetched.metric.as_deref(), Some("bodyweight_kg"));
        assert_eq!(fetched.direction, GoalDirection::Decrease);
        assert_eq!(fetched.priority, 5);
        assert_eq!(fetched.target_date.as_deref(), Some("2026-01-01"));
    }
}

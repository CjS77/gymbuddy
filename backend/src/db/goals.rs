use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::ExerciseGoal;

fn row_to_goal(row: &rusqlite::Row) -> rusqlite::Result<ExerciseGoal> {
    Ok(ExerciseGoal {
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
    })
}

const SELECT_GOAL: &str = "\
    SELECT id, user_id, exercise_type_id, target_value, start_date, end_date, \
           achieved, notes, created_at, updated_at \
    FROM exercise_goals";

impl Database {
    pub fn insert_goal(&self, goal: &ExerciseGoal) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO exercise_goals (user_id, exercise_type_id, target_value, start_date, end_date, \
                                          achieved, notes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                goal.user_id,
                goal.exercise_type_id,
                goal.target_value,
                goal.start_date,
                goal.end_date,
                goal.achieved as i32,
                goal.notes,
            ],
        )?;
        let id = self.conn().last_insert_rowid();
        tracing::debug!(id, exercise_type_id = goal.exercise_type_id, target = goal.target_value, "DB: inserted goal");
        Ok(id)
    }

    pub fn get_goal(&self, id: i64) -> anyhow::Result<Option<ExerciseGoal>> {
        let sql = format!("{SELECT_GOAL} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_goal)?;
        rows.next().transpose().context("Failed to read goal row")
    }

    pub fn list_active_goals(&self, user_id: i64) -> anyhow::Result<Vec<ExerciseGoal>> {
        let sql = format!("{SELECT_GOAL} WHERE user_id = ?1 AND achieved = 0 ORDER BY start_date");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id], row_to_goal)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list active goals")
    }

    pub fn list_goals_in_period(&self, user_id: i64, from: &str, to: &str) -> anyhow::Result<Vec<ExerciseGoal>> {
        let sql =
            format!("{SELECT_GOAL} WHERE user_id = ?1 AND start_date <= ?3 AND (end_date IS NULL OR end_date >= ?2) ORDER BY start_date");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, from, to], row_to_goal)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list goals in period")
    }

    pub fn mark_goal_achieved(&self, id: i64) -> anyhow::Result<()> {
        let rows =
            self.conn().execute("UPDATE exercise_goals SET achieved = 1, updated_at = datetime('now') WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "Goal with id {id} not found");
        Ok(())
    }

    pub fn delete_goal(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM exercise_goals WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "Goal with id {id} not found");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{new_exercise_goal, new_user};
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
        g1.end_date = Some("2025-06-01".into());
        db.insert_goal(&g1).unwrap();

        let mut g2 = new_exercise_goal(user_id, dl.id, 50.0);
        g2.start_date = "2025-07-01".into();
        g2.end_date = Some("2025-12-01".into());
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
}

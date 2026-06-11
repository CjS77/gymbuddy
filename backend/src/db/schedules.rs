use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{ReminderType, Schedule, ScheduleExercise};

fn row_to_schedule(row: &rusqlite::Row) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        cron_expr: row.get(3)?,
        reminder_type: ReminderType::from_str_loose(&row.get::<_, String>(4)?),
        reminder_notice_mins: row.get(5)?,
        enabled: row.get::<_, i32>(6)? != 0,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_schedule_exercise(row: &rusqlite::Row) -> rusqlite::Result<ScheduleExercise> {
    Ok(ScheduleExercise {
        schedule_id: row.get(0)?,
        exercise_type_id: row.get(1)?,
        order_idx: row.get(2)?,
        target_sets: row.get(3)?,
        target_reps: row.get(4)?,
        target_weight_kg: row.get(5)?,
    })
}

const SELECT_SCHEDULE: &str = "\
    SELECT id, user_id, name, cron_expr, reminder_type, reminder_notice_mins, \
           enabled, created_at, updated_at \
    FROM schedules";

impl Database {
    pub fn insert_schedule(&self, schedule: &Schedule) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO schedules (user_id, name, cron_expr, reminder_type, reminder_notice_mins, enabled) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                schedule.user_id,
                schedule.name,
                schedule.cron_expr,
                schedule.reminder_type.as_str(),
                schedule.reminder_notice_mins,
                schedule.enabled as i32,
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_schedule(&self, id: i64) -> anyhow::Result<Option<Schedule>> {
        let sql = format!("{SELECT_SCHEDULE} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_schedule)?;
        rows.next().transpose().context("Failed to read schedule row")
    }

    pub fn list_schedules(&self, user_id: i64) -> anyhow::Result<Vec<Schedule>> {
        let sql = format!("{SELECT_SCHEDULE} WHERE user_id = ?1 ORDER BY name");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id], row_to_schedule)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list schedules")
    }

    pub fn update_schedule(&self, schedule: &Schedule) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE schedules SET name = ?1, cron_expr = ?2, reminder_type = ?3, \
             reminder_notice_mins = ?4, enabled = ?5, updated_at = datetime('now') WHERE id = ?6",
            params![
                schedule.name,
                schedule.cron_expr,
                schedule.reminder_type.as_str(),
                schedule.reminder_notice_mins,
                schedule.enabled as i32,
                schedule.id,
            ],
        )?;
        anyhow::ensure!(rows > 0, "Schedule with id {} not found", schedule.id);
        Ok(())
    }

    pub fn delete_schedule(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM schedules WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "Schedule with id {id} not found");
        Ok(())
    }

    pub fn toggle_schedule(&self, id: i64, enabled: bool) -> anyhow::Result<()> {
        let rows = self
            .conn()
            .execute("UPDATE schedules SET enabled = ?1, updated_at = datetime('now') WHERE id = ?2", params![enabled as i32, id])?;
        anyhow::ensure!(rows > 0, "Schedule with id {id} not found");
        Ok(())
    }

    pub fn add_schedule_exercise(&self, entry: &ScheduleExercise) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO schedule_exercises (schedule_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.schedule_id,
                entry.exercise_type_id,
                entry.order_idx,
                entry.target_sets,
                entry.target_reps,
                entry.target_weight_kg
            ],
        )?;
        Ok(())
    }

    pub fn list_schedule_exercises(&self, schedule_id: i64) -> anyhow::Result<Vec<ScheduleExercise>> {
        let mut stmt = self.conn().prepare(
            "SELECT schedule_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg \
             FROM schedule_exercises WHERE schedule_id = ?1 ORDER BY order_idx",
        )?;
        let rows = stmt.query_map(params![schedule_id], row_to_schedule_exercise)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list schedule exercises")
    }

    pub fn remove_schedule_exercise(&self, schedule_id: i64, exercise_type_id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "DELETE FROM schedule_exercises WHERE schedule_id = ?1 AND exercise_type_id = ?2",
            params![schedule_id, exercise_type_id],
        )?;
        anyhow::ensure!(rows > 0, "Schedule exercise not found");
        Ok(())
    }

    pub fn clear_schedule_exercises(&self, schedule_id: i64) -> anyhow::Result<()> {
        self.conn().execute("DELETE FROM schedule_exercises WHERE schedule_id = ?1", params![schedule_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::new_user;
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn draft_schedule(user_id: i64) -> Schedule {
        Schedule {
            id: 0,
            user_id,
            name: "Push Day".into(),
            cron_expr: "0 0 6 * * 1,3,5".into(),
            reminder_type: ReminderType::Text,
            reminder_notice_mins: 30,
            enabled: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn create_schedule_with_exercises() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let sched_id = db.insert_schedule(&draft_schedule(user_id)).unwrap();

        let entry = ScheduleExercise {
            schedule_id: sched_id,
            exercise_type_id: bp.id,
            order_idx: 0,
            target_sets: Some(4),
            target_reps: Some(8),
            target_weight_kg: Some(80.0),
        };
        db.add_schedule_exercise(&entry).unwrap();

        let entries = db.list_schedule_exercises(sched_id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target_sets, Some(4));
    }

    #[test]
    fn toggle_schedule() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        let sched_id = db.insert_schedule(&draft_schedule(user_id)).unwrap();
        assert!(db.get_schedule(sched_id).unwrap().unwrap().enabled);

        db.toggle_schedule(sched_id, false).unwrap();
        assert!(!db.get_schedule(sched_id).unwrap().unwrap().enabled);
    }

    #[test]
    fn delete_schedule_cascades_exercises() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();

        let sched_id = db.insert_schedule(&draft_schedule(user_id)).unwrap();
        db.add_schedule_exercise(&ScheduleExercise {
            schedule_id: sched_id,
            exercise_type_id: bp.id,
            order_idx: 0,
            target_sets: None,
            target_reps: None,
            target_weight_kg: None,
        })
        .unwrap();

        db.delete_schedule(sched_id).unwrap();
        let entries = db.list_schedule_exercises(sched_id).unwrap();
        assert!(entries.is_empty());
    }
}

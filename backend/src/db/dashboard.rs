use anyhow::Context as _;
use chrono::NaiveDate;
use rusqlite::params;

use super::database::Database;
use super::models::{Difficulty, ExerciseSet, MeasurementType, MuscleGroupWeeklyVolume, PersonalRecord, WeekSummary};

impl Database {
    /// Total weight-reps volume per muscle_group root per ISO week.
    /// Volume = SUM(count * value) on `weight_reps` sets, grouped by the root muscle_group ancestor.
    pub fn volume_by_muscle_group_weekly(&self, user_id: i64, period: &str) -> anyhow::Result<Vec<MuscleGroupWeeklyVolume>> {
        let mut stmt = self.conn().prepare(
            "WITH RECURSIVE roots(id, root_id, root_name) AS ( \
                 SELECT id, id, name FROM exercise_types WHERE level = 'muscle_group' \
                 UNION ALL \
                 SELECT et.id, r.root_id, r.root_name \
                 FROM exercise_types et JOIN roots r ON et.parent_id = r.id \
             ) \
             SELECT strftime('%G-W%V', s.logged_at) AS week, \
                    r.root_name AS muscle_group, \
                    SUM(s.count * s.value) AS total_volume \
             FROM sets s \
             JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
             JOIN roots r ON r.id = s.exercise_type_id \
             WHERE ee.user_id = ?1 \
               AND s.logged_at >= datetime('now', ?2) \
               AND s.measurement_type_id = 1 \
               AND s.count IS NOT NULL \
             GROUP BY week, r.root_name \
             ORDER BY week, r.root_name",
        )?;
        let rows = stmt.query_map(params![user_id, period], |row| {
            Ok(MuscleGroupWeeklyVolume { week: row.get(0)?, muscle_group: row.get(1)?, total_volume: row.get(2)? })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to query volume by muscle group weekly")
    }

    /// All-time personal records per exercise_type. One PR (max value) per type.
    pub fn personal_records(&self, user_id: i64) -> anyhow::Result<Vec<PersonalRecord>> {
        let mut stmt = self.conn().prepare(
            "WITH RECURSIVE roots(id, root_name) AS ( \
                 SELECT id, name FROM exercise_types WHERE level = 'muscle_group' \
                 UNION ALL \
                 SELECT et.id, r.root_name FROM exercise_types et JOIN roots r ON et.parent_id = r.id \
             ), \
             ranked AS ( \
                 SELECT s.exercise_type_id, s.value, s.logged_at, s.measurement_type_id, \
                        ROW_NUMBER() OVER (PARTITION BY s.exercise_type_id ORDER BY s.value DESC) AS rn \
                 FROM sets s \
                 JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
                 WHERE ee.user_id = ?1 \
             ) \
             SELECT et.id, et.name, r.root_name, mt.name, ranked.value, ranked.logged_at \
             FROM ranked \
             JOIN exercise_types et ON ranked.exercise_type_id = et.id \
             LEFT JOIN roots r ON r.id = et.id \
             JOIN measurement_types mt ON mt.id = ranked.measurement_type_id \
             WHERE ranked.rn = 1 \
             ORDER BY r.root_name, et.name",
        )?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok(PersonalRecord {
                exercise_type_id: row.get(0)?,
                exercise_name: row.get(1)?,
                muscle_group: row.get(2)?,
                measurement_type: row.get(3)?,
                value: row.get(4)?,
                achieved_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to query personal records")
    }

    /// Consecutive days with at least one completed session, counting backwards from today.
    pub fn workout_streak(&self, user_id: i64) -> anyhow::Result<i32> {
        let mut stmt = self.conn().prepare(
            "SELECT DISTINCT date(started_at) FROM sessions \
             WHERE user_id = ?1 AND ended_at IS NOT NULL \
             ORDER BY date(started_at) DESC \
             LIMIT 400",
        )?;
        let dates: Vec<String> =
            stmt.query_map(params![user_id], |row| row.get(0))?.collect::<Result<Vec<_>, _>>().context("Failed to query streak dates")?;

        let today = chrono::Utc::now().date_naive();
        let mut streak = 0i32;
        let mut expected = today;

        for date_str in &dates {
            let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")?;
            if date == expected {
                streak += 1;
                expected -= chrono::Duration::days(1);
            } else if streak == 0 && date == today - chrono::Duration::days(1) {
                streak += 1;
                expected = date - chrono::Duration::days(1);
            } else {
                break;
            }
        }
        Ok(streak)
    }

    /// Quick stats for the current ISO week: session count and total weight-reps volume.
    pub fn week_summary(&self, user_id: i64) -> anyhow::Result<WeekSummary> {
        let row: (i32, f64) = self.conn().query_row(
            "SELECT COUNT(DISTINCT s.id), \
                    COALESCE(SUM(CASE WHEN st.measurement_type_id = 1 AND st.count IS NOT NULL \
                                      THEN st.count * st.value ELSE 0 END), 0) \
             FROM sessions s \
             LEFT JOIN exercise_entries ee ON ee.session_id = s.id \
             LEFT JOIN sets st ON st.exercise_entry_id = ee.id \
             WHERE s.user_id = ?1 \
               AND s.started_at >= date('now', '-6 days', 'weekday 1') \
               AND s.started_at < date('now', '-6 days', 'weekday 1', '+7 days')",
            params![user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(WeekSummary { session_count: row.0, total_volume: row.1 })
    }

    /// Check if a user is a member of a specific group (any level).
    pub fn is_group_member(&self, user_id: i64, group_id: i64) -> anyhow::Result<bool> {
        let mut stmt = self.conn().prepare("SELECT 1 FROM group_members WHERE user_id = ?1 AND group_id = ?2 LIMIT 1")?;
        let exists = stmt.query_map(params![user_id, group_id], |row| row.get::<_, i32>(0))?.next().is_some();
        Ok(exists)
    }

    /// Paginated set listing for a user, optionally filtered by exercise_type
    /// (with optional descendant rollup). Returns (rows, total_count).
    pub fn get_sets_paginated(
        &self,
        user_id: i64,
        from: Option<&str>,
        to: Option<&str>,
        exercise_type_id: Option<i64>,
        include_descendants: bool,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<(Vec<ExerciseSet>, i64)> {
        let row_to_set = |row: &rusqlite::Row| -> rusqlite::Result<ExerciseSet> {
            Ok(ExerciseSet {
                id: row.get(0)?,
                exercise_entry_id: row.get(1)?,
                exercise_type_id: row.get(2)?,
                order_idx: row.get(3)?,
                measurement_type: MeasurementType::from_id(row.get::<_, i64>(4)?),
                count: row.get(5)?,
                value: row.get(6)?,
                perceived_difficulty: Difficulty::from_str_loose(&row.get::<_, String>(7)?),
                comment: row.get(8)?,
                logged_at: row.get(9)?,
            })
        };

        let (count, rows) = match (exercise_type_id, include_descendants) {
            (Some(et_id), true) => {
                let count: i64 = self.conn().query_row(
                    "WITH RECURSIVE tree(id) AS ( \
                         SELECT id FROM exercise_types WHERE id = ?2 \
                         UNION ALL SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
                     ) \
                     SELECT COUNT(*) FROM sets s \
                     JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
                     WHERE ee.user_id = ?1 \
                       AND s.exercise_type_id IN (SELECT id FROM tree) \
                       AND (?3 IS NULL OR s.logged_at >= ?3) \
                       AND (?4 IS NULL OR s.logged_at <= ?4)",
                    params![user_id, et_id, from, to],
                    |row| row.get(0),
                )?;
                let mut stmt = self.conn().prepare(
                    "WITH RECURSIVE tree(id) AS ( \
                         SELECT id FROM exercise_types WHERE id = ?2 \
                         UNION ALL SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
                     ) \
                     SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                            s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
                     FROM sets s \
                     JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
                     WHERE ee.user_id = ?1 \
                       AND s.exercise_type_id IN (SELECT id FROM tree) \
                       AND (?3 IS NULL OR s.logged_at >= ?3) \
                       AND (?4 IS NULL OR s.logged_at <= ?4) \
                     ORDER BY s.logged_at DESC LIMIT ?5 OFFSET ?6",
                )?;
                let rows: Vec<ExerciseSet> = stmt
                    .query_map(params![user_id, et_id, from, to, limit, offset], row_to_set)?
                    .collect::<Result<_, _>>()
                    .context("Failed to fetch paginated sets")?;
                (count, rows)
            }
            _ => {
                let et_filter = exercise_type_id;
                let count: i64 = self.conn().query_row(
                    "SELECT COUNT(*) FROM sets s \
                     JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
                     WHERE ee.user_id = ?1 \
                       AND (?2 IS NULL OR s.exercise_type_id = ?2) \
                       AND (?3 IS NULL OR s.logged_at >= ?3) \
                       AND (?4 IS NULL OR s.logged_at <= ?4)",
                    params![user_id, et_filter, from, to],
                    |row| row.get(0),
                )?;
                let mut stmt = self.conn().prepare(
                    "SELECT s.id, s.exercise_entry_id, s.exercise_type_id, s.order_idx, \
                            s.measurement_type_id, s.count, s.value, s.perceived_difficulty, s.comment, s.logged_at \
                     FROM sets s \
                     JOIN exercise_entries ee ON s.exercise_entry_id = ee.id \
                     WHERE ee.user_id = ?1 \
                       AND (?2 IS NULL OR s.exercise_type_id = ?2) \
                       AND (?3 IS NULL OR s.logged_at >= ?3) \
                       AND (?4 IS NULL OR s.logged_at <= ?4) \
                     ORDER BY s.logged_at DESC LIMIT ?5 OFFSET ?6",
                )?;
                let rows: Vec<ExerciseSet> = stmt
                    .query_map(params![user_id, et_filter, from, to, limit, offset], row_to_set)?
                    .collect::<Result<_, _>>()
                    .context("Failed to fetch paginated sets")?;
                (count, rows)
            }
        };

        Ok((rows, count))
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{AccessLevel, Group, MeasurementType, new_exercise_entry, new_exercise_set, new_user};
    use super::*;

    fn fixture() -> (Database, i64, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        (db, user_id, bp.id)
    }

    fn log_weight_set(db: &Database, user_id: i64, et_id: i64, logged_at: &str, count: i32, weight: f64) {
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, et_id, MeasurementType::WeightReps, weight);
        s.count = Some(count);
        s.logged_at = logged_at.to_string();
        db.insert_set(&s).unwrap();
    }

    #[test]
    fn volume_by_muscle_group_weekly_aggregates() {
        let (db, user_id, bp_id) = fixture();
        let today = chrono::Utc::now().date_naive();
        for offset in [2i64, 1, 0] {
            let date = today - chrono::Duration::days(offset);
            log_weight_set(&db, user_id, bp_id, &format!("{date} 10:00:00"), 10, 80.0);
        }
        let volumes = db.volume_by_muscle_group_weekly(user_id, "-365 days").unwrap();
        assert!(!volumes.is_empty());
        // 3 days × 10 reps × 80 kg = 2400
        let total: f64 = volumes.iter().map(|v| v.total_volume).sum();
        assert!((total - 2400.0).abs() < 0.01, "got {total}");
        assert!(volumes.iter().all(|v| v.muscle_group == "Chest"));
    }

    #[test]
    fn personal_records_returns_one_per_exercise_type() {
        let (db, user_id, bp_id) = fixture();
        for w in [60.0, 80.0, 70.0] {
            log_weight_set(&db, user_id, bp_id, "2025-06-01 10:00:00", 5, w);
        }
        let prs = db.personal_records(user_id).unwrap();
        let pr = prs.iter().find(|p| p.exercise_type_id == bp_id).unwrap();
        assert!((pr.value - 80.0).abs() < 0.01);
    }

    #[test]
    fn workout_streak_consecutive_days() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let today = chrono::Utc::now().date_naive();
        for offset in 0..3 {
            let date = today - chrono::Duration::days(offset);
            db.conn()
                .execute(
                    "INSERT INTO sessions (user_id, started_at, ended_at) VALUES (?1, ?2, ?3)",
                    params![user_id, format!("{date} 10:00:00"), format!("{date} 11:00:00")],
                )
                .unwrap();
        }
        assert_eq!(db.workout_streak(user_id).unwrap(), 3);
    }

    #[test]
    fn week_summary_counts_correctly() {
        let (db, user_id, bp_id) = fixture();
        let session = db.start_session(user_id, None).unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let mut s = new_exercise_set(entry_id, bp_id, MeasurementType::WeightReps, 60.0);
        s.count = Some(10);
        db.insert_set(&s).unwrap();

        let old_date = chrono::Utc::now().date_naive() - chrono::Duration::days(10);
        db.conn()
            .execute(
                "INSERT INTO sessions (user_id, started_at) VALUES (?1, ?2)",
                params![user_id, format!("{old_date} 10:00:00")],
            )
            .unwrap();

        let summary = db.week_summary(user_id).unwrap();
        assert_eq!(summary.session_count, 1);
    }

    #[test]
    fn get_sets_paginated_returns_correct_page() {
        let (db, user_id, bp_id) = fixture();
        for i in 0..5 {
            log_weight_set(&db, user_id, bp_id, &format!("2025-06-{:02} 10:00:00", i + 1), 10, 60.0 + i as f64);
        }
        let (rows, total) = db.get_sets_paginated(user_id, None, None, None, false, 2, 0).unwrap();
        assert_eq!(total, 5);
        assert_eq!(rows.len(), 2);

        let (rows, total) = db.get_sets_paginated(user_id, None, None, None, false, 2, 3).unwrap();
        assert_eq!(total, 5);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn is_group_member_works() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let group_id = db
            .insert_group(&Group { id: 0, name: "Test Group".into(), description: None, created_at: "2025-01-01 00:00:00".into() })
            .unwrap();
        db.add_member(user_id, group_id, AccessLevel::Read).unwrap();
        assert!(db.is_group_member(user_id, group_id).unwrap());
        assert!(!db.is_group_member(user_id, 99999).unwrap());
    }
}

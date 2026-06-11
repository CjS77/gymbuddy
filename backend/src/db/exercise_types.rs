use anyhow::Context as _;
use rusqlite::{Row, params};

use super::database::Database;
use super::models::{ExerciseLevel, ExerciseType, ExerciseTypeWithAncestry, MeasurementType};

const SELECT_EXERCISE_TYPE: &str = "\
    SELECT id, name, parent_id, level, aliases, purpose, measurement_type_id, description, url, created_at \
    FROM exercise_types";

fn row_to_exercise_type(row: &Row) -> rusqlite::Result<ExerciseType> {
    Ok(ExerciseType {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        level: ExerciseLevel::from_str_loose(&row.get::<_, String>(3)?).unwrap_or(ExerciseLevel::Exercise),
        aliases: row.get(4)?,
        purpose: row.get(5)?,
        measurement_type: row.get::<_, Option<i64>>(6)?.map(MeasurementType::from_id),
        description: row.get(7)?,
        url: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn row_to_exercise_type_with_ancestry(row: &Row) -> rusqlite::Result<ExerciseTypeWithAncestry> {
    let exercise_type = row_to_exercise_type(row)?;
    Ok(ExerciseTypeWithAncestry { exercise_type, muscle_group: row.get(10)?, specific_muscle: row.get(11)?, exercise: row.get(12)? })
}

impl Database {
    pub fn list_measurement_types(&self) -> anyhow::Result<Vec<(i64, String)>> {
        let mut stmt = self.conn().prepare("SELECT id, name FROM measurement_types ORDER BY id")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list measurement types")
    }

    pub fn measurement_type_id(&self, name: &str) -> anyhow::Result<Option<i64>> {
        let mut stmt = self.conn().prepare("SELECT id FROM measurement_types WHERE name = ?1")?;
        let mut rows = stmt.query_map(params![name], |row| row.get(0))?;
        rows.next().transpose().context("Failed to look up measurement type")
    }

    /// All top-level (muscle_group) exercise_type rows, ordered by id.
    pub fn list_top_level_groups(&self) -> anyhow::Result<Vec<ExerciseType>> {
        let sql = format!("{SELECT_EXERCISE_TYPE} WHERE level = 'muscle_group' ORDER BY id");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], row_to_exercise_type)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list muscle groups")
    }

    /// Insert an exercise_type row, returning the generated id. Validates the parent's
    /// level is exactly one tier above the child's level (`muscle_group → specific_muscle → ...`).
    pub fn insert_exercise_type(&self, et: &ExerciseType) -> anyhow::Result<i64> {
        validate_parent_level(self, et)?;
        let mt_id = et.measurement_type.map(|m| m.id());
        self.conn().execute(
            "INSERT INTO exercise_types \
                (name, parent_id, level, aliases, purpose, measurement_type_id, description, url) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![et.name, et.parent_id, et.level.as_str(), et.aliases, et.purpose, mt_id, et.description, et.url,],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn get_exercise_type(&self, id: i64) -> anyhow::Result<Option<ExerciseType>> {
        let sql = format!("{SELECT_EXERCISE_TYPE} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_exercise_type)?;
        rows.next().transpose().context("Failed to read exercise_type row")
    }

    pub fn get_exercise_type_by_name(&self, name: &str) -> anyhow::Result<Option<ExerciseType>> {
        let sql = format!("{SELECT_EXERCISE_TYPE} WHERE name = ?1 COLLATE NOCASE");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![name], row_to_exercise_type)?;
        rows.next().transpose().context("Failed to read exercise_type row")
    }

    pub fn exercise_type_id_by_name(&self, name: &str, level: Option<ExerciseLevel>) -> anyhow::Result<Option<i64>> {
        let row = match level {
            Some(lvl) => self.conn().query_row(
                "SELECT id FROM exercise_types WHERE name = ?1 COLLATE NOCASE AND level = ?2",
                params![name, lvl.as_str()],
                |r| r.get::<_, i64>(0),
            ),
            None => {
                self.conn().query_row("SELECT id FROM exercise_types WHERE name = ?1 COLLATE NOCASE", params![name], |r| r.get::<_, i64>(0))
            }
        };
        match row {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("Failed to look up exercise_type id"),
        }
    }

    /// Search by name OR alias (substring, case-insensitive). Searches all levels.
    pub fn search_exercise_types(&self, query: &str) -> anyhow::Result<Vec<ExerciseType>> {
        let pattern = format!("%{query}%");
        let sql = format!("{SELECT_EXERCISE_TYPE} WHERE name LIKE ?1 OR aliases LIKE ?1 ORDER BY level, name");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![pattern], row_to_exercise_type)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to search exercise_types")
    }

    /// All exercise_types, optionally filtered to a single level.
    pub fn list_exercise_types(&self, level: Option<ExerciseLevel>) -> anyhow::Result<Vec<ExerciseType>> {
        let sql = match level {
            Some(_) => format!("{SELECT_EXERCISE_TYPE} WHERE level = ?1 ORDER BY name"),
            None => format!("{SELECT_EXERCISE_TYPE} ORDER BY level, name"),
        };
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = match level {
            Some(lvl) => stmt.query_map(params![lvl.as_str()], row_to_exercise_type)?.collect::<Result<Vec<_>, _>>(),
            None => stmt.query_map([], row_to_exercise_type)?.collect::<Result<Vec<_>, _>>(),
        };
        rows.context("Failed to list exercise_types")
    }

    /// Every exercise_type with the names of its three potential ancestors flattened in.
    pub fn list_exercise_types_with_ancestry(&self) -> anyhow::Result<Vec<ExerciseTypeWithAncestry>> {
        let sql = "\
            WITH RECURSIVE ancestors(id, name, parent_id, level, aliases, purpose, \
                                     measurement_type_id, description, url, created_at, \
                                     ex_name, sm_name, mg_name) AS (\
                SELECT et.id, et.name, et.parent_id, et.level, et.aliases, et.purpose, \
                       et.measurement_type_id, et.description, et.url, et.created_at, \
                       NULL, NULL, NULL \
                FROM exercise_types et \
                WHERE et.level = 'muscle_group' \
                UNION ALL \
                SELECT et.id, et.name, et.parent_id, et.level, et.aliases, et.purpose, \
                       et.measurement_type_id, et.description, et.url, et.created_at, \
                       CASE et.level \
                           WHEN 'variation' THEN a.name \
                           ELSE a.ex_name \
                       END, \
                       CASE et.level \
                           WHEN 'exercise' THEN a.name \
                           WHEN 'variation' THEN a.sm_name \
                           ELSE a.sm_name \
                       END, \
                       CASE et.level \
                           WHEN 'specific_muscle' THEN a.name \
                           ELSE a.mg_name \
                       END \
                FROM exercise_types et \
                JOIN ancestors a ON et.parent_id = a.id \
            ) \
            SELECT id, name, parent_id, level, aliases, purpose, measurement_type_id, description, url, created_at, \
                   mg_name, sm_name, ex_name \
            FROM ancestors \
            ORDER BY level, name";
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map([], row_to_exercise_type_with_ancestry)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list exercise_types with ancestry")
    }

    /// Recursive list of all descendants of `root_id` (excluding the root itself).
    pub fn list_descendants(&self, root_id: i64) -> anyhow::Result<Vec<ExerciseType>> {
        let sql = "\
            WITH RECURSIVE tree(id) AS ( \
                SELECT id FROM exercise_types WHERE parent_id = ?1 \
                UNION ALL \
                SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
            ) \
            SELECT et.id, et.name, et.parent_id, et.level, et.aliases, et.purpose, \
                   et.measurement_type_id, et.description, et.url, et.created_at \
            FROM exercise_types et \
            JOIN tree t ON et.id = t.id \
            ORDER BY et.level, et.name";
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map(params![root_id], row_to_exercise_type)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list descendants")
    }

    /// `root_id` plus all of its descendants, suitable for IN-clause filters.
    pub fn descendant_ids_inclusive(&self, root_id: i64) -> anyhow::Result<Vec<i64>> {
        let sql = "\
            WITH RECURSIVE tree(id) AS ( \
                SELECT id FROM exercise_types WHERE id = ?1 \
                UNION ALL \
                SELECT et.id FROM exercise_types et JOIN tree t ON et.parent_id = t.id \
            ) SELECT id FROM tree";
        let mut stmt = self.conn().prepare(sql)?;
        let rows = stmt.query_map(params![root_id], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to collect descendant ids")
    }

    pub fn update_exercise_type(&self, et: &ExerciseType) -> anyhow::Result<()> {
        validate_parent_level(self, et)?;
        let mt_id = et.measurement_type.map(|m| m.id());
        let rows = self.conn().execute(
            "UPDATE exercise_types SET name = ?1, parent_id = ?2, level = ?3, \
                 aliases = ?4, purpose = ?5, measurement_type_id = ?6, description = ?7, url = ?8 \
             WHERE id = ?9",
            params![et.name, et.parent_id, et.level.as_str(), et.aliases, et.purpose, mt_id, et.description, et.url, et.id,],
        )?;
        anyhow::ensure!(rows > 0, "exercise_type id {} not found", et.id);
        Ok(())
    }

    pub fn delete_exercise_type(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM exercise_types WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "exercise_type id {id} not found");
        Ok(())
    }
}

/// Confirm the row's parent (if any) is exactly one tier above the child's level.
fn validate_parent_level(db: &Database, et: &ExerciseType) -> anyhow::Result<()> {
    match (et.level, et.parent_id) {
        (ExerciseLevel::MuscleGroup, None) => Ok(()),
        (ExerciseLevel::MuscleGroup, Some(_)) => anyhow::bail!("muscle_group rows must have parent_id = NULL"),
        (lvl, None) => anyhow::bail!("{lvl} rows require a parent_id"),
        (lvl, Some(pid)) => {
            let parent_level: Option<String> =
                db.conn().query_row("SELECT level FROM exercise_types WHERE id = ?1", params![pid], |r| r.get(0)).ok();
            let parent_level = parent_level.context("parent_id does not reference an existing exercise_type")?;
            let parent = ExerciseLevel::from_str_loose(&parent_level).context("parent has unknown level")?;
            let expected = lvl.parent().expect("non-muscle_group level always has a parent tier");
            anyhow::ensure!(parent == expected, "parent level {parent} is not the tier directly above {lvl} (expected {expected})");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn seeded_taxonomy_has_seven_muscle_groups() {
        let db = test_db();
        let groups = db.list_top_level_groups().unwrap();
        assert_eq!(groups.len(), 7);
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        for required in ["Chest", "Back", "Shoulders", "Arms", "Legs", "Core", "Cardio"] {
            assert!(names.contains(&required), "missing {required}");
        }
    }

    #[test]
    fn list_descendants_for_chest() {
        let db = test_db();
        let chest = db.get_exercise_type_by_name("Chest").unwrap().unwrap();
        let descendants = db.list_descendants(chest.id).unwrap();
        assert!(descendants.len() >= 20, "expected ≥20 chest descendants, got {}", descendants.len());
        assert!(descendants.iter().any(|d| d.name == "Bench Press"));
        assert!(descendants.iter().any(|d| d.name == "Flat Barbell Bench Press"));
    }

    #[test]
    fn descendant_ids_inclusive_includes_root() {
        let db = test_db();
        let chest = db.get_exercise_type_by_name("Chest").unwrap().unwrap();
        let ids = db.descendant_ids_inclusive(chest.id).unwrap();
        assert!(ids.contains(&chest.id));
    }

    #[test]
    fn search_finds_by_alias() {
        let db = test_db();
        let results = db.search_exercise_types("rdl").unwrap();
        assert!(results.iter().any(|r| r.name == "Romanian Deadlift"));
    }

    #[test]
    fn ancestry_is_populated() {
        let db = test_db();
        let rows = db.list_exercise_types_with_ancestry().unwrap();
        let bench_variation = rows.iter().find(|r| r.exercise_type.name == "Flat Barbell Bench Press").unwrap();
        assert_eq!(bench_variation.muscle_group.as_deref(), Some("Chest"));
        assert_eq!(bench_variation.specific_muscle.as_deref(), Some("Pectoral"));
        assert_eq!(bench_variation.exercise.as_deref(), Some("Bench Press"));
    }

    #[test]
    fn insert_with_wrong_parent_level_fails() {
        let db = test_db();
        // Try to insert a 'variation' as a child of a 'muscle_group' (Chest).
        let chest = db.get_exercise_type_by_name("Chest").unwrap().unwrap();
        let bad = ExerciseType {
            id: 0,
            name: "Bogus Variation".to_string(),
            parent_id: Some(chest.id),
            level: ExerciseLevel::Variation,
            aliases: None,
            purpose: None,
            measurement_type: None,
            description: None,
            url: None,
            created_at: String::new(),
        };
        assert!(db.insert_exercise_type(&bad).is_err());
    }

    #[test]
    fn insert_muscle_group_with_parent_fails() {
        let db = test_db();
        let chest = db.get_exercise_type_by_name("Chest").unwrap().unwrap();
        let bad = ExerciseType {
            id: 0,
            name: "Bogus Group".to_string(),
            parent_id: Some(chest.id),
            level: ExerciseLevel::MuscleGroup,
            aliases: None,
            purpose: None,
            measurement_type: None,
            description: None,
            url: None,
            created_at: String::new(),
        };
        assert!(db.insert_exercise_type(&bad).is_err());
    }

    #[test]
    fn insert_then_get_then_delete() {
        let db = test_db();
        let pectoral = db.get_exercise_type_by_name("Pectoral").unwrap().unwrap();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        // Add a new variation under Bench Press.
        let new_var = ExerciseType {
            id: 0,
            name: "Floor Press".to_string(),
            parent_id: Some(bp.id),
            level: ExerciseLevel::Variation,
            aliases: Some("floor".to_string()),
            purpose: Some("strength".to_string()),
            measurement_type: Some(MeasurementType::WeightReps),
            description: None,
            url: None,
            created_at: String::new(),
        };
        let new_id = db.insert_exercise_type(&new_var).unwrap();
        let fetched = db.get_exercise_type(new_id).unwrap().unwrap();
        assert_eq!(fetched.parent_id, Some(bp.id));
        assert_eq!(fetched.aliases.as_deref(), Some("floor"));
        // Pectoral is unchanged.
        assert_eq!(pectoral.level, ExerciseLevel::SpecificMuscle);
        db.delete_exercise_type(new_id).unwrap();
        assert!(db.get_exercise_type(new_id).unwrap().is_none());
    }

    #[test]
    fn delete_with_children_fails_due_to_restrict() {
        let db = test_db();
        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        // Has children (variations). RESTRICT.
        assert!(db.delete_exercise_type(bp.id).is_err());
    }

    #[test]
    fn measurement_types_resolve() {
        let db = test_db();
        assert_eq!(db.measurement_type_id("weight_reps").unwrap(), Some(1));
        assert_eq!(db.measurement_type_id("time_based").unwrap(), Some(2));
        assert_eq!(db.measurement_type_id("nonexistent").unwrap(), None);
    }
}

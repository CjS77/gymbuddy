use crate::db::ExerciseTypeWithAncestry;

/// Find the best matching exercise_type from the catalogue.
///
/// Multi-stage pipeline (stops at first match):
/// 1. Exact match (case-insensitive) on `name`
/// 2. Alias match (case-insensitive, comma-separated `aliases`)
/// 3. Contains match (only if exactly one exercise_type matches)
/// 4. Levenshtein distance (threshold: <= 3 AND < half name length)
pub fn find_exercise_type<'a>(catalogue: &'a [ExerciseTypeWithAncestry], name: &str) -> Option<&'a ExerciseTypeWithAncestry> {
    let name_lower = name.to_lowercase();

    // Stage 1: exact match
    if let Some(et) = catalogue.iter().find(|e| e.exercise_type.name.eq_ignore_ascii_case(name)) {
        return Some(et);
    }

    // Stage 2: alias match
    if let Some(et) = catalogue
        .iter()
        .find(|e| e.exercise_type.aliases.as_deref().unwrap_or("").split(',').any(|alias| alias.trim().eq_ignore_ascii_case(name)))
    {
        return Some(et);
    }

    // Stage 3: contains (single match only)
    let contains_matches: Vec<_> = catalogue.iter().filter(|e| e.exercise_type.name.to_lowercase().contains(&name_lower)).collect();
    if contains_matches.len() == 1 {
        return Some(contains_matches[0]);
    }

    // Stage 4: levenshtein distance
    let mut best: Option<(&ExerciseTypeWithAncestry, usize)> = None;
    for et in catalogue {
        let dist = levenshtein(&name_lower, &et.exercise_type.name.to_lowercase());
        let threshold = name_lower.len() / 2;
        if dist <= 3 && dist < threshold {
            match best {
                Some((_, best_dist)) if dist < best_dist => best = Some((et, dist)),
                None => best = Some((et, dist)),
                _ => {}
            }
        }
    }

    best.map(|(et, _)| et)
}

/// Levenshtein edit distance (two-row DP).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = b.len();

    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ExerciseLevel, ExerciseType, ExerciseTypeWithAncestry, MeasurementType};

    fn make(id: i64, name: &str, aliases: &str, muscle_group: &str) -> ExerciseTypeWithAncestry {
        ExerciseTypeWithAncestry {
            exercise_type: ExerciseType {
                id,
                name: name.to_string(),
                parent_id: Some(1),
                level: ExerciseLevel::Exercise,
                aliases: if aliases.is_empty() { None } else { Some(aliases.to_string()) },
                purpose: Some("strength".to_string()),
                measurement_type: Some(MeasurementType::WeightReps),
                url: None,
                created_at: String::new(),
            },
            muscle_group: Some(muscle_group.to_string()),
            specific_muscle: None,
            exercise: None,
        }
    }

    fn fixture() -> Vec<ExerciseTypeWithAncestry> {
        vec![
            make(1, "Barbell Bench Press", "flat bench,bench,bench press", "Chest"),
            make(2, "Incline Dumbbell Press", "incline press,incline db press", "Chest"),
            make(3, "Conventional Deadlift", "deadlift,dl", "Back"),
            make(4, "Cable Fly", "cable flyes", "Chest"),
            make(5, "Barbell Curl", "bb curl,barbell curls", "Arms"),
            make(6, "Dumbbell Curl", "db curl,dumbbell curls", "Arms"),
            make(7, "Hammer Curl", "hammer curls", "Arms"),
            make(8, "Bent Over Barbell Row", "barbell row,bent over row,bent-over row,bent-over barbell row,bb row", "Back"),
            make(9, "Seated Row", "row", "Back"),
            make(10, "Cable Seated Row", "", "Back"),
            make(11, "Machine Seated Row", "", "Back"),
            make(12, "One Arm Dumbbell Row", "one arm row,single arm row,single arm dumbbell row,db row,one arm dumbell row", "Back"),
        ]
    }

    #[test]
    fn exact_match_case_insensitive() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "barbell bench press").unwrap();
        assert_eq!(found.exercise_type.name, "Barbell Bench Press");
    }

    #[test]
    fn alias_match_bench() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "bench").unwrap();
        assert_eq!(found.exercise_type.name, "Barbell Bench Press");
    }

    #[test]
    fn alias_match_dl() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "dl").unwrap();
        assert_eq!(found.exercise_type.name, "Conventional Deadlift");
    }

    #[test]
    fn contains_single_match() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "Cable Fly").unwrap();
        assert_eq!(found.exercise_type.name, "Cable Fly");
    }

    #[test]
    fn contains_ambiguous_falls_through() {
        let cat = fixture();
        let result = find_exercise_type(&cat, "curl");
        assert!(result.is_none());
    }

    #[test]
    fn levenshtein_close_match_typo() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "Barbel Bench Press").unwrap();
        assert_eq!(found.exercise_type.name, "Barbell Bench Press");
    }

    #[test]
    fn levenshtein_too_distant() {
        let cat = fixture();
        assert!(find_exercise_type(&cat, "yoga").is_none());
    }

    #[test]
    fn no_match() {
        let cat = fixture();
        assert!(find_exercise_type(&cat, "Underwater Basket Weaving").is_none());
    }

    #[test]
    fn alias_match_barbell_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "barbell row").unwrap();
        assert_eq!(found.exercise_type.name, "Bent Over Barbell Row");
    }

    #[test]
    fn alias_match_bent_over_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "bent over row").unwrap();
        assert_eq!(found.exercise_type.name, "Bent Over Barbell Row");
    }

    #[test]
    fn alias_match_hyphenated_bent_over_barbell_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "bent-over barbell row").unwrap();
        assert_eq!(found.exercise_type.name, "Bent Over Barbell Row");
    }

    #[test]
    fn exact_match_bent_over_barbell_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "Bent Over Barbell Row").unwrap();
        assert_eq!(found.exercise_type.name, "Bent Over Barbell Row");
    }

    #[test]
    fn seated_row_alias_still_resolves() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "row").unwrap();
        assert_eq!(found.exercise_type.name, "Seated Row");
    }

    #[test]
    fn cable_seated_row_exact_still_resolves() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "Cable Seated Row").unwrap();
        assert_eq!(found.exercise_type.name, "Cable Seated Row");
    }

    #[test]
    fn machine_seated_row_exact_still_resolves() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "Machine Seated Row").unwrap();
        assert_eq!(found.exercise_type.name, "Machine Seated Row");
    }

    #[test]
    fn exact_match_one_arm_dumbbell_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "one arm dumbbell row").unwrap();
        assert_eq!(found.exercise_type.name, "One Arm Dumbbell Row");
    }

    #[test]
    fn alias_match_one_arm_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "one arm row").unwrap();
        assert_eq!(found.exercise_type.name, "One Arm Dumbbell Row");
    }

    #[test]
    fn alias_match_single_arm_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "single arm row").unwrap();
        assert_eq!(found.exercise_type.name, "One Arm Dumbbell Row");
    }

    #[test]
    fn alias_match_single_arm_dumbbell_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "single arm dumbbell row").unwrap();
        assert_eq!(found.exercise_type.name, "One Arm Dumbbell Row");
    }

    #[test]
    fn alias_match_db_row() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "db row").unwrap();
        assert_eq!(found.exercise_type.name, "One Arm Dumbbell Row");
    }

    #[test]
    fn alias_match_one_arm_dumbell_misspelling() {
        let cat = fixture();
        let found = find_exercise_type(&cat, "one arm dumbell row").unwrap();
        assert_eq!(found.exercise_type.name, "One Arm Dumbbell Row");
    }

    #[test]
    fn levenshtein_identical() {
        assert_eq!(levenshtein("hello", "hello"), 0);
    }

    #[test]
    fn levenshtein_empty() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", ""), 0);
    }

    #[test]
    fn levenshtein_substitution() {
        assert_eq!(levenshtein("cat", "bat"), 1);
    }

    #[test]
    fn levenshtein_insertion() {
        assert_eq!(levenshtein("cat", "cats"), 1);
    }

    #[test]
    fn levenshtein_deletion() {
        assert_eq!(levenshtein("cats", "cat"), 1);
    }

    #[test]
    fn levenshtein_mixed() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }
}

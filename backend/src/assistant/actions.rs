use serde::Deserialize;

use crate::db::{Difficulty, HealthEntryType};

#[derive(Debug, Deserialize)]
pub struct AssistantResponse {
    pub message: String,
    #[serde(default)]
    pub actions: Vec<AssistantAction>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantAction {
    /// Records exactly ONE weight × reps set. To log multiple sets in one message
    /// the LLM must emit one `LogExercise` per set.
    LogExercise {
        exercise: String,
        reps: Option<i32>,
        weight_kg: Option<f64>,
        #[serde(default, alias = "difficulty")]
        perceived_difficulty: Option<Difficulty>,
        #[serde(default, alias = "notes")]
        comment: Option<String>,
        /// Set by the LLM only to confirm a deliberate superset after the host
        /// asked whether a taxonomy-related exercise is the same as one already
        /// in progress. `true` suppresses the ambiguity prompt.
        #[serde(default)]
        superset: bool,
    },
    LogExerciseTimed {
        exercise: String,
        duration_secs: i32,
        #[serde(default, alias = "difficulty")]
        perceived_difficulty: Option<Difficulty>,
        #[serde(default, alias = "notes")]
        comment: Option<String>,
        #[serde(default)]
        superset: bool,
    },
    LogExerciseDistance {
        exercise: String,
        distance_m: Option<f64>,
        duration_secs: Option<i32>,
        #[serde(default, alias = "difficulty")]
        perceived_difficulty: Option<Difficulty>,
        #[serde(default, alias = "notes")]
        comment: Option<String>,
        #[serde(default)]
        superset: bool,
    },
    StartSession {
        notes: Option<String>,
        /// Optional name of a saved schedule the session should follow. Used by the
        /// host to suggest the next exercise after each entry closes.
        #[serde(default)]
        plan: Option<String>,
    },
    EndSession,
    /// Close one open exercise_entry. The handler resolves the entry by `entry_id`
    /// first, then by exercise name (against open entries in the active session),
    /// and finally falls back to the most recent open entry. If the entry has
    /// fewer than 3 sets, the handler pushes back instead of closing.
    CloseExerciseEntry {
        #[serde(default)]
        exercise: Option<String>,
        #[serde(default)]
        entry_id: Option<i64>,
    },
    /// Close an open exercise_entry, bypassing the <3-set pushback. Used after the
    /// user has been asked to keep going and reaffirmed their intent to close.
    ConfirmCloseExerciseEntry {
        #[serde(default)]
        exercise: Option<String>,
        #[serde(default)]
        entry_id: Option<i64>,
    },
    /// Delete an open exercise_entry outright (used to clean up leaked entries
    /// from a previous session).
    DeleteExerciseEntry {
        entry_id: i64,
    },
    /// Close every open exercise_entry currently in the active session. Used in
    /// response to the user agreeing to clean up leaked open entries before
    /// starting a new session.
    CloseAllOpenEntries,
    LogHealth {
        entry_type: HealthEntryType,
        body_part: Option<String>,
        severity: Option<String>,
        description: String,
    },
    ResolveHealth {
        description: String,
    },
    SetGoal {
        exercise: String,
        target_value: f64,
        end_date: Option<String>,
    },
    /// Correct a previously-logged set. The host resolves WHICH set/entry by
    /// recency, so no numeric id is carried. `exercise` is a resolution filter
    /// (the set's CURRENT exercise); `new_exercise` is the target to change it
    /// TO and reclassifies the whole exercise block. Value/reps/difficulty
    /// changes target the single most-recent matching set.
    EditSet {
        #[serde(default)]
        exercise: Option<String>,
        #[serde(default)]
        new_exercise: Option<String>,
        #[serde(default)]
        new_reps: Option<i32>,
        #[serde(default, alias = "new_weight_kg")]
        new_value: Option<f64>,
        #[serde(default, alias = "difficulty")]
        new_difficulty: Option<Difficulty>,
    },
    /// Look up the user's most recent `exercise_entry` for a named exercise and
    /// report its sets. Free-text `exercise` is resolved through
    /// `find_exercise_type`; when nothing is logged against the exact type the
    /// handler falls back to a descendants-inclusive query so coarse names like
    /// "chest" still surface a logged variation.
    GetLastExercise {
        #[serde(alias = "exercise_entry", alias = "name")]
        exercise: String,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_exercise_action() {
        let json = r#"{"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "hard"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::LogExercise { exercise, reps, weight_kg, perceived_difficulty, comment, .. } => {
                assert_eq!(exercise, "Bench Press");
                assert_eq!(reps, Some(8));
                assert_eq!(weight_kg, Some(80.0));
                assert_eq!(perceived_difficulty, Some(Difficulty::Hard));
                assert_eq!(comment, None);
            }
            _ => panic!("expected LogExercise"),
        }
    }

    #[test]
    fn parse_log_exercise_legacy_difficulty_alias() {
        let json = r#"{"type": "log_exercise", "exercise": "Bench Press", "difficulty": "easy"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::LogExercise { perceived_difficulty, .. } => assert_eq!(perceived_difficulty, Some(Difficulty::Easy)),
            _ => panic!("expected LogExercise"),
        }
    }

    #[test]
    fn parse_log_exercise_timed() {
        let json = r#"{"type": "log_exercise_timed", "exercise": "Plank", "duration_secs": 60}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::LogExerciseTimed { duration_secs: 60, .. }));
    }

    #[test]
    fn parse_log_exercise_distance() {
        let json = r#"{"type": "log_exercise_distance", "exercise": "Running", "distance_m": 5000.0, "duration_secs": 1800}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::LogExerciseDistance { .. }));
    }

    #[test]
    fn parse_start_session() {
        let json = r#"{"type": "start_session", "notes": "Leg day"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::StartSession { notes, plan } => {
                assert_eq!(notes.as_deref(), Some("Leg day"));
                assert_eq!(plan, None);
            }
            _ => panic!("expected StartSession"),
        }
    }

    #[test]
    fn parse_start_session_with_plan() {
        let json = r#"{"type": "start_session", "plan": "Push Day"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::StartSession { notes, plan } => {
                assert_eq!(notes, None);
                assert_eq!(plan.as_deref(), Some("Push Day"));
            }
            _ => panic!("expected StartSession"),
        }
    }

    #[test]
    fn parse_close_exercise_entry() {
        let json = r#"{"type": "close_exercise_entry", "exercise": "Bench Press"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::CloseExerciseEntry { exercise, entry_id } => {
                assert_eq!(exercise.as_deref(), Some("Bench Press"));
                assert_eq!(entry_id, None);
            }
            _ => panic!("expected CloseExerciseEntry"),
        }
    }

    #[test]
    fn parse_confirm_close_exercise_entry() {
        let json = r#"{"type": "confirm_close_exercise_entry", "entry_id": 42}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ConfirmCloseExerciseEntry { entry_id, .. } => assert_eq!(entry_id, Some(42)),
            _ => panic!("expected ConfirmCloseExerciseEntry"),
        }
    }

    #[test]
    fn parse_delete_exercise_entry() {
        let json = r#"{"type": "delete_exercise_entry", "entry_id": 7}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::DeleteExerciseEntry { entry_id } => assert_eq!(entry_id, 7),
            _ => panic!("expected DeleteExerciseEntry"),
        }
    }

    #[test]
    fn parse_close_all_open_entries() {
        let json = r#"{"type": "close_all_open_entries"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::CloseAllOpenEntries));
    }

    #[test]
    fn parse_end_session() {
        let json = r#"{"type": "end_session"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::EndSession));
    }

    #[test]
    fn parse_log_health() {
        let json = r#"{"type": "log_health", "entry_type": "injury", "body_part": "shoulder", "severity": "moderate", "description": "Shoulder pain"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::LogHealth { .. }));
    }

    #[test]
    fn parse_resolve_health() {
        let json = r#"{"type": "resolve_health", "description": "shoulder"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::ResolveHealth { .. }));
    }

    #[test]
    fn parse_set_goal() {
        let json = r#"{"type": "set_goal", "exercise": "Bench Press", "target_value": 100.0, "end_date": "2026-06-01"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::SetGoal { target_value, .. } if target_value == 100.0));
    }

    #[test]
    fn parse_edit_set_full() {
        let json = r#"{"type": "edit_set", "exercise": "Bench Press", "new_exercise": "Cable Fly", "new_reps": 10, "new_value": 40.0, "new_difficulty": "hard"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::EditSet { exercise, new_exercise, new_reps, new_value, new_difficulty } => {
                assert_eq!(exercise.as_deref(), Some("Bench Press"));
                assert_eq!(new_exercise.as_deref(), Some("Cable Fly"));
                assert_eq!(new_reps, Some(10));
                assert_eq!(new_value, Some(40.0));
                assert_eq!(new_difficulty, Some(Difficulty::Hard));
            }
            _ => panic!("expected EditSet"),
        }
    }

    #[test]
    fn parse_edit_set_minimal() {
        let json = r#"{"type": "edit_set", "new_value": 40.0}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::EditSet { exercise, new_exercise, new_reps, new_value, new_difficulty } => {
                assert_eq!(exercise, None);
                assert_eq!(new_exercise, None);
                assert_eq!(new_reps, None);
                assert_eq!(new_value, Some(40.0));
                assert_eq!(new_difficulty, None);
            }
            _ => panic!("expected EditSet"),
        }
    }

    #[test]
    fn parse_edit_set_weight_alias() {
        let json = r#"{"type": "edit_set", "new_weight_kg": 50.0}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::EditSet { new_value, .. } => assert_eq!(new_value, Some(50.0)),
            _ => panic!("expected EditSet"),
        }
    }

    #[test]
    fn parse_get_last_exercise() {
        let json = r#"{"type": "get_last_exercise", "exercise": "Bench Press"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::GetLastExercise { exercise } => assert_eq!(exercise, "Bench Press"),
            _ => panic!("expected GetLastExercise"),
        }
    }

    #[test]
    fn parse_get_last_exercise_alias() {
        // The issue body suggests `exercise_entry` as the field name; accept it via alias.
        let json = r#"{"type": "get_last_exercise", "exercise_entry": "Squat"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::GetLastExercise { exercise } => assert_eq!(exercise, "Squat"),
            _ => panic!("expected GetLastExercise"),
        }
    }

    #[test]
    fn unknown_action_type() {
        let json = r#"{"type": "dance_party"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AssistantAction::Unknown));
    }

    #[test]
    fn missing_optional_fields() {
        let json = r#"{"type": "log_exercise", "exercise": "Bench Press"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::LogExercise { reps, weight_kg, perceived_difficulty, comment, superset, .. } => {
                assert_eq!(reps, None);
                assert_eq!(weight_kg, None);
                assert_eq!(perceived_difficulty, None);
                assert_eq!(comment, None);
                assert!(!superset, "superset should default to false");
            }
            _ => panic!("expected LogExercise"),
        }
    }

    #[test]
    fn parse_log_exercise_superset_flag() {
        let json = r#"{"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "superset": true}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::LogExercise { superset, .. } => assert!(superset),
            _ => panic!("expected LogExercise"),
        }
    }

    #[test]
    fn parse_response_with_actions() {
        let json = r#"{
            "message": "Logged it!",
            "actions": [
                {"type": "start_session"},
                {"type": "log_exercise", "exercise": "Bench", "reps": 8, "weight_kg": 80.0},
                {"type": "log_exercise", "exercise": "Bench", "reps": 8, "weight_kg": 80.0},
                {"type": "log_exercise", "exercise": "Bench", "reps": 8, "weight_kg": 80.0}
            ]
        }"#;
        let resp: AssistantResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message, "Logged it!");
        assert_eq!(resp.actions.len(), 4);
    }

    #[test]
    fn parse_response_absent_actions() {
        let json = r#"{"message": "Hello!"}"#;
        let resp: AssistantResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message, "Hello!");
        assert!(resp.actions.is_empty());
    }

    #[test]
    fn parse_response_null_actions() {
        let json = r#"{"message": "Hello!", "actions": null}"#;
        let result = serde_json::from_str::<AssistantResponse>(json);
        assert!(result.is_err());
    }
}

use serde::Deserialize;

use crate::db::{Difficulty, GoalDirection, GoalKind, HealthEntryType, SessionFeel};

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
    /// Unknown fields are ignored, so history carrying the dropped `plan` field
    /// (a saved-schedule name; schedules are gone) still parses.
    StartSession {
        notes: Option<String>,
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
    /// Record ONE body measurement (bodyweight, body fat, waist, resting HR) so a
    /// free-text weigh-in logs without a command, the way health entries do via
    /// [`Self::LogHealth`]. `metric` is the canonical unit-suffixed name
    /// ("bodyweight_kg", "body_fat_pct", "waist_cm", "resting_hr_bpm" — the same
    /// names `set_goal` metrics use); `value` is in the unit the name carries,
    /// already converted to metric.
    LogBodyMetric {
        #[serde(alias = "name")]
        metric: String,
        value: f64,
    },
    /// Set a goal. Exercise goals name an `exercise` (bigger-is-better strength /
    /// endurance targets); non-exercise goals (weightloss, "train 4x a week") name a
    /// `metric` instead. `direction` inverts progress for goals where smaller is
    /// better (weightloss, a faster time). `kind` and `direction` default sensibly
    /// when omitted; `end_date` is accepted as an alias for `target_date`.
    SetGoal {
        #[serde(default)]
        exercise: Option<String>,
        #[serde(default)]
        metric: Option<String>,
        #[serde(default)]
        kind: Option<GoalKind>,
        target_value: f64,
        #[serde(default)]
        direction: Option<GoalDirection>,
        #[serde(default)]
        priority: Option<i64>,
        #[serde(default, alias = "end_date")]
        target_date: Option<String>,
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
    /// Look up the user's most recent `ExerciseEntry` for a named exercise and
    /// report its sets. Free-text `exercise` is resolved through
    /// `find_exercise_type`; when nothing is logged against the exact type the
    /// handler falls back to a descendants-inclusive query so coarse names like
    /// "chest" still surface a logged variation.
    ///
    /// The `exercise_entry` alias is a wire contract, not a stale spelling: replayed
    /// conversation history holds envelopes keyed that way, so it must keep the
    /// singular v1 form even though the table is now `exercise_entries`.
    GetLastExercise {
        #[serde(alias = "exercise_entry", alias = "name")]
        exercise: String,
    },
    /// Emitted by the `/philosophy` interview prompt once enough has been gathered.
    /// `content` is the fully distilled training philosophy (goals, programmes,
    /// frequency, and equipment captured as free text). The host appends it to the
    /// append-only philosophy log and exits the interview.
    SavePhilosophy {
        #[serde(alias = "philosophy")]
        content: String,
    },
    /// Emitted by the `/nextworkout` designer prompt. The host persists a draft
    /// session roster from it and shows it to the user; it NEVER logs sets or
    /// starts a session.
    ///
    /// The `propose_workout` alias is a wire contract, not a leftover: replayed
    /// conversation history holds envelopes carrying the old tag, and small models
    /// imitate the shapes they see in that history.
    #[serde(alias = "propose_workout")]
    ProposeSessionRoster {
        title: String,
        #[serde(default)]
        rationale: Option<String>,
        #[serde(default)]
        exercises: Vec<ProposedRosterExercise>,
    },
    /// Emitted by the `/programme` interview prompt once it has enough to design a
    /// multi-week programme. The host persists a draft [`crate::db::Programme`] from it,
    /// expands `week_template` across `weeks` into `programme_slots`, and links
    /// `goal_ids` through `programme_goals`. It activates nothing — the user's
    /// "lock it in" does that.
    ///
    /// Deliberately carries no exercises: a programme is a skeleton, not a script, and
    /// each session is still designed against it by `/nextworkout`.
    ProposeProgramme {
        title: String,
        /// How many weeks the programme runs for.
        weeks: i32,
        #[serde(alias = "days")]
        days_per_week: i32,
        /// Free text the designer reads, e.g. "upper/lower".
        #[serde(default)]
        split: String,
        /// Free text the designer reads, e.g. "double progression: add reps, then load".
        #[serde(default, alias = "progression")]
        progression_policy: String,
        #[serde(default)]
        blocks: Vec<ProposedProgrammeBlock>,
        /// The repeating week, expanded across `weeks` into the slot grid.
        #[serde(default)]
        week_template: Vec<ProposedProgrammeDay>,
        /// Ids of the goals this programme serves, as listed in the prompt's ACTIVE GOALS.
        #[serde(default, alias = "goals")]
        goal_ids: Vec<i64>,
    },
    /// Append a durable training preference or constraint the user voices mid-workout
    /// (e.g. "always give me goblet squats instead of barbell") to their philosophy,
    /// so future designs respect it.
    AppendPhilosophyNote {
        #[serde(alias = "content")]
        note: String,
    },
    /// Record a today-only override the user voices mid-workout (e.g. "I don't feel
    /// like bench today, let's do flys"). Applies ONLY to the roster in flight — it is
    /// stored on that roster, never written to the philosophy, and gone by the next
    /// design. Use this for one-offs; use AppendPhilosophyNote for durable changes.
    SetSessionOverride {
        #[serde(alias = "content", alias = "override")]
        note: String,
    },
    /// The user's confirm/override of the session-level verdict the host proposed
    /// at end_session (or a spontaneous mid-session "cutting it short because …").
    /// Every field is optional: a bare agreement carries none and leaves the
    /// proposed `overall_effort` standing; overrides carry only what the user said.
    /// The host applies it to the most recent session (active or just ended).
    RecordSessionOutcome {
        #[serde(default, alias = "effort")]
        overall_effort: Option<Difficulty>,
        #[serde(default, alias = "feel")]
        felt: Option<SessionFeel>,
        #[serde(default)]
        cut_short: Option<bool>,
        #[serde(default, alias = "reason")]
        cut_short_reason: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

/// One prescribed exercise inside a [`AssistantAction::ProposeSessionRoster`]. The
/// target fields mirror the roster storage: `(target_reps, target_weight_kg)` for
/// the weight_reps case, `target_secs` for timed work.
#[derive(Debug, Deserialize)]
pub struct ProposedRosterExercise {
    pub exercise: String,
    #[serde(default)]
    pub target_sets: Option<i32>,
    #[serde(default)]
    pub target_reps: Option<i32>,
    #[serde(default, alias = "target_weight")]
    pub target_weight_kg: Option<f64>,
    #[serde(default)]
    pub target_secs: Option<i32>,
    #[serde(default, alias = "cue")]
    pub notes: Option<String>,
}

/// One mesocycle inside a [`AssistantAction::ProposeProgramme`]: an inclusive, 1-based
/// week range with a focus. Mirrors [`crate::db::ProgrammeBlock`]'s storage.
#[derive(Debug, Deserialize)]
pub struct ProposedProgrammeBlock {
    pub start_week: i32,
    pub end_week: i32,
    pub focus: String,
}

/// One training day of the repeating week in a [`AssistantAction::ProposeProgramme`].
/// `day_idx` is the 1-based ordinal training day within the week, never a calendar
/// weekday, and `focus` is a text intent ("upper") — never an exercise list.
#[derive(Debug, Deserialize)]
pub struct ProposedProgrammeDay {
    #[serde(alias = "day")]
    pub day_idx: i32,
    pub focus: String,
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
            AssistantAction::StartSession { notes } => assert_eq!(notes.as_deref(), Some("Leg day")),
            _ => panic!("expected StartSession"),
        }
    }

    /// `plan` named a saved schedule; schedules are gone. Stored history still carries
    /// the field, so it must be ignored rather than reject the whole envelope.
    #[test]
    fn parse_start_session_ignores_dropped_plan_field() {
        let json = r#"{"type": "start_session", "notes": "Leg day", "plan": "Push Day"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::StartSession { notes } => assert_eq!(notes.as_deref(), Some("Leg day")),
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
    fn parse_log_body_metric() {
        let json = r#"{"type": "log_body_metric", "metric": "bodyweight_kg", "value": 82.5}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::LogBodyMetric { metric, value } => {
                assert_eq!(metric, "bodyweight_kg");
                assert_eq!(value, 82.5);
            }
            _ => panic!("expected LogBodyMetric"),
        }
    }

    #[test]
    fn parse_log_body_metric_name_alias() {
        let json = r#"{"type": "log_body_metric", "name": "body_fat_pct", "value": 18.0}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::LogBodyMetric { metric, value } => {
                assert_eq!(metric, "body_fat_pct");
                assert_eq!(value, 18.0);
            }
            _ => panic!("expected LogBodyMetric"),
        }
    }

    #[test]
    fn parse_set_goal() {
        // `end_date` is still accepted as an alias for `target_date`.
        let json = r#"{"type": "set_goal", "exercise": "Bench Press", "target_value": 100.0, "end_date": "2026-06-01"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::SetGoal { exercise, target_value, target_date, kind, direction, metric, priority } => {
                assert_eq!(exercise.as_deref(), Some("Bench Press"));
                assert_eq!(target_value, 100.0);
                assert_eq!(target_date.as_deref(), Some("2026-06-01"));
                assert_eq!(kind, None);
                assert_eq!(direction, None);
                assert_eq!(metric, None);
                assert_eq!(priority, None);
            }
            _ => panic!("expected SetGoal"),
        }
    }

    #[test]
    fn parse_set_goal_metric_decrease() {
        let json = r#"{"type": "set_goal", "kind": "body_composition", "metric": "bodyweight_kg", "target_value": 80.0, "direction": "decrease", "priority": 5, "target_date": "2026-01-01"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::SetGoal { exercise, metric, kind, direction, priority, target_value, target_date } => {
                assert_eq!(exercise, None);
                assert_eq!(metric.as_deref(), Some("bodyweight_kg"));
                assert_eq!(kind, Some(GoalKind::BodyComposition));
                assert_eq!(direction, Some(GoalDirection::Decrease));
                assert_eq!(priority, Some(5));
                assert_eq!(target_value, 80.0);
                assert_eq!(target_date.as_deref(), Some("2026-01-01"));
            }
            _ => panic!("expected SetGoal"),
        }
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
    fn parse_save_philosophy() {
        let json = r#"{"type": "save_philosophy", "content": "goal=hypertrophy; 5x5; home gym squat rack 120kg, dumbbells 24kg; 3x/week"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::SavePhilosophy { content } => assert!(content.contains("hypertrophy")),
            _ => panic!("expected SavePhilosophy"),
        }
    }

    #[test]
    fn parse_save_philosophy_alias() {
        let json = r#"{"type": "save_philosophy", "philosophy": "cardio focus, 2x/week"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::SavePhilosophy { content } => assert_eq!(content, "cardio focus, 2x/week"),
            _ => panic!("expected SavePhilosophy"),
        }
    }

    #[test]
    fn parse_propose_session_roster() {
        let json = r#"{
            "type": "propose_session_roster",
            "title": "Upper push + lat focus",
            "rationale": "2 days rest on bench; sub one-arm rows for the back niggle.",
            "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 65.0, "notes": "push the weight"},
                {"exercise": "Plank", "target_sets": 3, "target_secs": 60}
            ]
        }"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ProposeSessionRoster { title, rationale, exercises } => {
                assert_eq!(title, "Upper push + lat focus");
                assert!(rationale.unwrap().contains("2 days rest"));
                assert_eq!(exercises.len(), 2);
                assert_eq!(exercises[0].exercise, "Bench Press");
                assert_eq!(exercises[0].target_weight_kg, Some(65.0));
                assert_eq!(exercises[0].notes.as_deref(), Some("push the weight"));
                assert_eq!(exercises[1].target_secs, Some(60));
            }
            _ => panic!("expected ProposeSessionRoster"),
        }
    }

    /// The pre-rename tag. Conversation history stored before the roster vocabulary
    /// landed replays through this parser, so `propose_workout` must keep working —
    /// do not "tidy" this alias away with the type rename.
    #[test]
    fn parse_propose_session_roster_legacy_tag_alias() {
        let json = r#"{"type": "propose_workout", "title": "Legacy envelope", "exercises": [
            {"exercise": "Squat", "target_sets": 3, "target_reps": 5}
        ]}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ProposeSessionRoster { title, exercises, .. } => {
                assert_eq!(title, "Legacy envelope");
                assert_eq!(exercises[0].exercise, "Squat");
            }
            _ => panic!("expected the propose_workout tag to alias onto ProposeSessionRoster"),
        }
    }

    #[test]
    fn parse_propose_session_roster_weight_and_cue_aliases() {
        let json = r#"{"type": "propose_session_roster", "title": "Quick", "exercises": [
            {"exercise": "Squat", "target_weight": 100.0, "cue": "brace hard"}
        ]}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ProposeSessionRoster { exercises, .. } => {
                assert_eq!(exercises[0].target_weight_kg, Some(100.0));
                assert_eq!(exercises[0].notes.as_deref(), Some("brace hard"));
            }
            _ => panic!("expected ProposeSessionRoster"),
        }
    }

    #[test]
    fn parse_propose_programme() {
        let json = r#"{
            "type": "propose_programme",
            "title": "12-week hypertrophy base",
            "weeks": 12,
            "days_per_week": 3,
            "split": "upper/lower",
            "progression_policy": "double progression: add reps, then load",
            "blocks": [
                {"start_week": 1, "end_week": 4, "focus": "accumulation"},
                {"start_week": 5, "end_week": 5, "focus": "deload"}
            ],
            "week_template": [
                {"day_idx": 1, "focus": "upper push"},
                {"day_idx": 2, "focus": "lower"},
                {"day_idx": 3, "focus": "upper pull"}
            ],
            "goal_ids": [3, 7]
        }"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ProposeProgramme { title, weeks, days_per_week, split, progression_policy, blocks, week_template, goal_ids } => {
                assert_eq!(title, "12-week hypertrophy base");
                assert_eq!((weeks, days_per_week), (12, 3));
                assert_eq!(split, "upper/lower");
                assert!(progression_policy.contains("double progression"));
                assert_eq!(blocks.len(), 2);
                assert_eq!((blocks[1].start_week, blocks[1].end_week), (5, 5));
                assert_eq!(week_template.len(), 3);
                assert_eq!(week_template[2].day_idx, 3);
                assert_eq!(week_template[2].focus, "upper pull");
                assert_eq!(goal_ids, [3, 7]);
            }
            _ => panic!("expected ProposeProgramme"),
        }
    }

    /// A small model will drop the optional free-text fields and the goal links; the
    /// envelope must still parse, since the host can persist a programme without them.
    #[test]
    fn parse_propose_programme_minimal() {
        let json = r#"{"type": "propose_programme", "title": "Simple", "weeks": 4, "days_per_week": 2}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ProposeProgramme { split, progression_policy, blocks, week_template, goal_ids, .. } => {
                assert!(split.is_empty() && progression_policy.is_empty());
                assert!(blocks.is_empty() && week_template.is_empty() && goal_ids.is_empty());
            }
            _ => panic!("expected ProposeProgramme"),
        }
    }

    #[test]
    fn parse_propose_programme_aliases() {
        let json = r#"{"type": "propose_programme", "title": "Aliased", "weeks": 6, "days": 4,
                       "progression": "add 2.5kg when all reps are hit", "goals": [1],
                       "week_template": [{"day": 1, "focus": "full body"}]}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::ProposeProgramme { days_per_week, progression_policy, goal_ids, week_template, .. } => {
                assert_eq!(days_per_week, 4);
                assert!(progression_policy.contains("2.5kg"));
                assert_eq!(goal_ids, [1]);
                assert_eq!(week_template[0].day_idx, 1);
            }
            _ => panic!("expected ProposeProgramme"),
        }
    }

    #[test]
    fn parse_record_session_outcome_full() {
        let json = r#"{"type": "record_session_outcome", "overall_effort": "hard", "felt": "good",
                       "cut_short": true, "cut_short_reason": "knee pain"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::RecordSessionOutcome { overall_effort, felt, cut_short, cut_short_reason } => {
                assert_eq!(overall_effort, Some(Difficulty::Hard));
                assert_eq!(felt, Some(SessionFeel::Good));
                assert_eq!(cut_short, Some(true));
                assert_eq!(cut_short_reason.as_deref(), Some("knee pain"));
            }
            _ => panic!("expected RecordSessionOutcome"),
        }
    }

    #[test]
    fn parse_record_session_outcome_bare_agreement() {
        let json = r#"{"type": "record_session_outcome"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::RecordSessionOutcome { overall_effort, felt, cut_short, cut_short_reason } => {
                assert_eq!(overall_effort, None);
                assert_eq!(felt, None);
                assert_eq!(cut_short, None);
                assert_eq!(cut_short_reason, None);
            }
            _ => panic!("expected RecordSessionOutcome"),
        }
    }

    #[test]
    fn parse_record_session_outcome_aliases() {
        let json = r#"{"type": "record_session_outcome", "effort": "easy", "feel": "rough", "reason": "out of time"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::RecordSessionOutcome { overall_effort, felt, cut_short_reason, .. } => {
                assert_eq!(overall_effort, Some(Difficulty::Easy));
                assert_eq!(felt, Some(SessionFeel::Rough));
                assert_eq!(cut_short_reason.as_deref(), Some("out of time"));
            }
            _ => panic!("expected RecordSessionOutcome"),
        }
    }

    #[test]
    fn parse_append_philosophy_note() {
        let json = r#"{"type": "append_philosophy_note", "note": "prefers goblet squats over barbell"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::AppendPhilosophyNote { note } => assert!(note.contains("goblet")),
            _ => panic!("expected AppendPhilosophyNote"),
        }
    }

    #[test]
    fn parse_set_session_override() {
        let json = r#"{"type": "set_session_override", "note": "no bench today, do flys instead"}"#;
        let action: AssistantAction = serde_json::from_str(json).unwrap();
        match action {
            AssistantAction::SetSessionOverride { note } => assert!(note.contains("flys")),
            _ => panic!("expected SetSessionOverride"),
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

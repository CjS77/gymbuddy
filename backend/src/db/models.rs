use std::fmt;

use chrono::Utc;
use serde::{Deserialize, Serialize};

// ── Enums ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementType {
    WeightReps,
    TimeBased,
    DistanceBased,
    LevelBased,
    ScoreBased,
}

impl MeasurementType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WeightReps => "weight_reps",
            Self::TimeBased => "time_based",
            Self::DistanceBased => "distance_based",
            Self::LevelBased => "level_based",
            Self::ScoreBased => "score_based",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().replace('-', "_").as_str() {
            "weight_reps" | "weightreps" => Self::WeightReps,
            "time_based" | "timebased" => Self::TimeBased,
            "distance_based" | "distancebased" => Self::DistanceBased,
            "level_based" | "levelbased" => Self::LevelBased,
            "score_based" | "scorebased" => Self::ScoreBased,
            _ => Self::WeightReps,
        }
    }

    /// Stable numeric id matching `measurement_types.id` rows in the migration.
    pub fn id(&self) -> i64 {
        match self {
            Self::WeightReps => 1,
            Self::TimeBased => 2,
            Self::DistanceBased => 3,
            Self::LevelBased => 4,
            Self::ScoreBased => 5,
        }
    }

    pub fn from_id(id: i64) -> Self {
        match id {
            2 => Self::TimeBased,
            3 => Self::DistanceBased,
            4 => Self::LevelBased,
            5 => Self::ScoreBased,
            _ => Self::WeightReps,
        }
    }
}

impl fmt::Display for MeasurementType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExerciseLevel {
    MuscleGroup,
    SpecificMuscle,
    Exercise,
    Variation,
}

impl ExerciseLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MuscleGroup => "muscle_group",
            Self::SpecificMuscle => "specific_muscle",
            Self::Exercise => "exercise",
            Self::Variation => "variation",
        }
    }

    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "muscle_group" | "musclegroup" => Some(Self::MuscleGroup),
            "specific_muscle" | "specificmuscle" => Some(Self::SpecificMuscle),
            "exercise" => Some(Self::Exercise),
            "variation" => Some(Self::Variation),
            _ => None,
        }
    }

    /// Tier index where 1 = muscle_group … 4 = variation.
    pub fn tier(&self) -> u8 {
        match self {
            Self::MuscleGroup => 1,
            Self::SpecificMuscle => 2,
            Self::Exercise => 3,
            Self::Variation => 4,
        }
    }

    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::MuscleGroup => None,
            Self::SpecificMuscle => Some(Self::MuscleGroup),
            Self::Exercise => Some(Self::SpecificMuscle),
            Self::Variation => Some(Self::Exercise),
        }
    }
}

impl fmt::Display for ExerciseLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
    Failure,
}

impl Difficulty {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
            Self::Failure => "failure",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "easy" => Self::Easy,
            "hard" => Self::Hard,
            "failure" => Self::Failure,
            _ => Self::Medium,
        }
    }
}

impl fmt::Display for Difficulty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthEntryType {
    Injury,
    Illness,
    Wellbeing,
}

impl HealthEntryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Injury => "injury",
            Self::Illness => "illness",
            Self::Wellbeing => "wellbeing",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "injury" => Self::Injury,
            "illness" => Self::Illness,
            _ => Self::Wellbeing,
        }
    }
}

impl fmt::Display for HealthEntryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    Read,
    Write,
    Admin,
}

impl AccessLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "write" => Self::Write,
            "admin" => Self::Admin,
            _ => Self::Read,
        }
    }
}

impl fmt::Display for AccessLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderType {
    Text,
    Voice,
}

impl ReminderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Voice => "voice",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "voice" => Self::Voice,
            _ => Self::Text,
        }
    }
}

impl fmt::Display for ReminderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    User,
    Assistant,
    System,
}

impl ConversationRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "assistant" => Self::Assistant,
            "system" => Self::System,
            _ => Self::User,
        }
    }
}

impl fmt::Display for ConversationRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Achieved,
    Failed,
}

// ── Structs ────────────────────────────────────────────────────────────────────

/// Hierarchical exercise taxonomy entry (muscle_group → … → variation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseType {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub level: ExerciseLevel,
    pub aliases: Option<String>,
    pub purpose: Option<String>,
    pub measurement_type: Option<MeasurementType>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub created_at: String,
}

/// An exercise_type with the names of its ancestors flattened in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseTypeWithAncestry {
    pub exercise_type: ExerciseType,
    pub muscle_group: Option<String>,
    pub specific_muscle: Option<String>,
    pub exercise: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub telegram_id: Option<String>,
    pub signal_id: Option<String>,
    /// ed25519 public key (hex) of a confide client. NULL for Telegram-only users.
    #[serde(default)]
    pub pubkey: Option<String>,
    pub timezone: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub beta_tester: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub user_id: i64,
    pub group_id: i64,
    pub level: AccessLevel,
    pub granted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseGoal {
    pub id: i64,
    pub user_id: i64,
    pub exercise_type_id: i64,
    pub target_value: f64,
    pub start_date: String,
    pub end_date: Option<String>,
    pub achieved: bool,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: i64,
    pub user_id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub notes: Option<String>,
}

/// A block of related sets within a session (or standalone).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseEntry {
    pub id: i64,
    pub user_id: i64,
    pub session_id: Option<i64>,
    pub start_timestamp: String,
    pub end_timestamp: Option<String>,
    pub comment: Option<String>,
}

/// A single recorded set. The (count, value) pair is interpreted via measurement_type:
///
///   weight_reps    → count = reps,  value = weight_kg
///   time_based     → count = NULL,  value = duration_secs
///   distance_based → count = NULL,  value = distance_m
///   level_based    → count = NULL,  value = level
///   score_based    → count = NULL,  value = score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseSet {
    pub id: i64,
    pub exercise_entry_id: i64,
    pub exercise_type_id: i64,
    pub order_idx: i32,
    pub measurement_type: MeasurementType,
    pub count: Option<i32>,
    pub value: f64,
    pub perceived_difficulty: Difficulty,
    pub comment: Option<String>,
    pub logged_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub cron_expr: String,
    pub reminder_type: ReminderType,
    pub reminder_notice_mins: i32,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleExercise {
    pub schedule_id: i64,
    pub exercise_type_id: i64,
    pub order_idx: i32,
    pub target_sets: Option<i32>,
    pub target_reps: Option<i32>,
    pub target_weight_kg: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthEntry {
    pub id: i64,
    pub user_id: i64,
    pub entry_type: HealthEntryType,
    pub body_part: Option<String>,
    pub severity: String,
    pub description: String,
    pub started_at: String,
    pub resolved_at: Option<String>,
    pub notes: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: i64,
    pub user_id: i64,
    pub platform: String,
    pub role: ConversationRole,
    pub content: String,
    pub timestamp: String,
    /// When true, this message is stored for audit but excluded from LLM prompt context.
    pub exclude_from_context: bool,
}

// ── Time-series and goal progress types ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeriesPoint {
    pub date: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSeries {
    pub exercise_type_id: i64,
    pub exercise_name: String,
    pub measurement_type: MeasurementType,
    pub points: Vec<TimeSeriesPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalProgress {
    pub goal: ExerciseGoal,
    pub exercise_name: String,
    pub status: GoalStatus,
    pub current_value: Option<f64>,
    pub percentage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session: Session,
    pub exercise_count: i32,
    pub duration_mins: Option<i32>,
}

// ── Dashboard aggregate types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuscleGroupWeeklyVolume {
    pub week: String,
    pub muscle_group: String,
    pub total_volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalRecord {
    pub exercise_type_id: i64,
    pub exercise_name: String,
    pub muscle_group: Option<String>,
    pub measurement_type: String,
    pub value: f64,
    pub achieved_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeekSummary {
    pub session_count: i32,
    pub total_volume: f64,
}

// ── Constructors (drafts; id is set by the insert function via last_insert_rowid) ──

fn now_str() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn new_user(name: &str, telegram_id: Option<&str>, timezone: &str) -> User {
    let now = now_str();
    User {
        id: 0,
        name: name.to_string(),
        telegram_id: telegram_id.map(String::from),
        signal_id: None,
        pubkey: None,
        timezone: timezone.to_string(),
        created_at: now.clone(),
        updated_at: now,
        beta_tester: false,
    }
}

/// Construct a user identified by a confide ed25519 public key (hex), used by the
/// confide transport at registration. Has no `telegram_id`/`signal_id`.
pub fn new_user_with_pubkey(name: &str, pubkey: &str, timezone: &str) -> User {
    let now = now_str();
    User {
        id: 0,
        name: name.to_string(),
        telegram_id: None,
        signal_id: None,
        pubkey: Some(pubkey.to_string()),
        timezone: timezone.to_string(),
        created_at: now.clone(),
        updated_at: now,
        beta_tester: false,
    }
}

pub fn new_session(user_id: i64, notes: Option<&str>) -> Session {
    Session { id: 0, user_id, started_at: now_str(), ended_at: None, notes: notes.map(String::from) }
}

pub fn new_exercise_entry(user_id: i64, session_id: Option<i64>, comment: Option<&str>) -> ExerciseEntry {
    new_exercise_entry_at(user_id, session_id, comment, &now_str())
}

pub fn new_exercise_entry_at(user_id: i64, session_id: Option<i64>, comment: Option<&str>, start_timestamp: &str) -> ExerciseEntry {
    ExerciseEntry {
        id: 0,
        user_id,
        session_id,
        start_timestamp: start_timestamp.to_string(),
        end_timestamp: None,
        comment: comment.map(String::from),
    }
}

pub fn new_exercise_set(exercise_entry_id: i64, exercise_type_id: i64, measurement_type: MeasurementType, value: f64) -> ExerciseSet {
    ExerciseSet {
        id: 0,
        exercise_entry_id,
        exercise_type_id,
        order_idx: 0,
        measurement_type,
        count: None,
        value,
        perceived_difficulty: Difficulty::Medium,
        comment: None,
        logged_at: now_str(),
    }
}

pub fn new_exercise_goal(user_id: i64, exercise_type_id: i64, target_value: f64) -> ExerciseGoal {
    let now = now_str();
    ExerciseGoal {
        id: 0,
        user_id,
        exercise_type_id,
        target_value,
        start_date: now.clone(),
        end_date: None,
        achieved: false,
        notes: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

pub fn new_health_entry(user_id: i64, entry_type: HealthEntryType, description: &str) -> HealthEntry {
    let now = now_str();
    HealthEntry {
        id: 0,
        user_id,
        entry_type,
        body_part: None,
        severity: "mild".to_string(),
        description: description.to_string(),
        started_at: now.clone(),
        resolved_at: None,
        notes: None,
        updated_at: now,
    }
}

pub fn new_conversation_message(user_id: i64, platform: &str, role: ConversationRole, content: &str) -> ConversationMessage {
    ConversationMessage {
        id: 0,
        user_id,
        platform: platform.to_string(),
        role,
        content: content.to_string(),
        timestamp: now_str(),
        exclude_from_context: false,
    }
}

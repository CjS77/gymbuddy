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

    /// Noun for the measured quantity: "weight", "duration", "distance", "level",
    /// "score". Pair with [`Self::format_value`] when a labelled value is wanted.
    pub fn value_label(self) -> &'static str {
        match self {
            Self::WeightReps => "weight",
            Self::TimeBased => "duration",
            Self::DistanceBased => "distance",
            Self::LevelBased => "level",
            Self::ScoreBased => "score",
        }
    }

    /// The measured value with its unit but no leading noun, e.g. "80.0kg", "60s",
    /// "5000m", "3", "9.5". The single source of truth for prompt-side value
    /// rendering (client display uses [`SetLine::compact`](gymbuddy_proto::SetLine::compact)).
    pub fn format_value(self, value: f64) -> String {
        match self {
            Self::WeightReps => format!("{value:.1}kg"),
            Self::TimeBased => format!("{value:.0}s"),
            Self::DistanceBased => format!("{value:.0}m"),
            Self::LevelBased => format!("{value:.0}"),
            Self::ScoreBased => format!("{value:.1}"),
        }
    }

    /// A self-describing value for standalone prompt text: "80.0kg", "60s", "5000m",
    /// "level 3", "score 9.5". (Weight/time/distance carry their own unit; level and
    /// score get the noun prefixed.)
    pub fn describe_value(self, value: f64) -> String {
        match self {
            Self::WeightReps | Self::TimeBased | Self::DistanceBased => self.format_value(value),
            Self::LevelBased | Self::ScoreBased => format!("{} {}", self.value_label(), self.format_value(value)),
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

/// What a goal measures. `strength` and `endurance` raise/lower a single
/// exercise's number; `bodyweight` / `body_composition` / `habit` are denominated
/// in a free-text `metric` rather than an exercise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalKind {
    Strength,
    Endurance,
    Bodyweight,
    BodyComposition,
    Habit,
}

impl GoalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strength => "strength",
            Self::Endurance => "endurance",
            Self::Bodyweight => "bodyweight",
            Self::BodyComposition => "body_composition",
            Self::Habit => "habit",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().replace('-', "_").as_str() {
            "endurance" => Self::Endurance,
            "bodyweight" | "body_weight" => Self::Bodyweight,
            "body_composition" | "bodycomposition" | "composition" => Self::BodyComposition,
            "habit" => Self::Habit,
            _ => Self::Strength,
        }
    }
}

impl fmt::Display for GoalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which way progress runs. `increase` (the default) means bigger is better;
/// `decrease` inverts it — a weightloss or faster-time goal succeeds as the value
/// falls. Progress and goal-status computations key off this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalDirection {
    Increase,
    Decrease,
}

impl GoalDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Increase => "increase",
            Self::Decrease => "decrease",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "decrease" | "down" | "lower" | "reduce" => Self::Decrease,
            _ => Self::Increase,
        }
    }
}

impl fmt::Display for GoalDirection {
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
    /// Whether the inter-set rest timer arms after each logged set. A user
    /// preference (persists across sessions), seeded from `[rest_timer]
    /// default_enabled` at registration and toggled with `/timers`.
    #[serde(default = "default_true")]
    pub timers_enabled: bool,
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

/// A per-user goal. Generalised beyond a single exercise's single number: `kind`
/// says what it measures, `direction` which way progress runs, and `priority` ranks
/// competing goals. Exercise goals carry an `exercise_type_id`; non-exercise goals
/// (bodyweight / body_composition / habit) carry a free-text `metric` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: i64,
    pub user_id: i64,
    pub kind: GoalKind,
    /// The exercise this goal tracks, or NULL for a metric-denominated goal.
    pub exercise_type_id: Option<i64>,
    /// Free-text metric for non-exercise goals (e.g. "bodyweight_kg",
    /// "sessions_per_week"). NULL when `exercise_type_id` is set.
    pub metric: Option<String>,
    pub target_value: f64,
    pub direction: GoalDirection,
    /// Ranking when goals compete; higher wins. Defaults to 0.
    pub priority: i64,
    pub start_date: String,
    /// The date the user aims to reach the target by. NULL = open-ended. A past
    /// target_date on an unachieved goal derives to `Failed`.
    pub target_date: Option<String>,
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
    pub goal: Goal,
    /// Human label for the goal's subject: the exercise name for exercise goals,
    /// otherwise the `metric`.
    pub exercise_name: String,
    pub status: GoalStatus,
    pub current_value: Option<f64>,
    pub percentage: f64,
}

/// Per-muscle-group recovery signal for the session designer: when a group was
/// last trained (any exercise in its subtree) and how many sets that most-recent
/// day involved. `last_trained == None` means never trained — the strongest
/// possible rest signal, which is why untrained groups are surfaced, not omitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuscleRecovery {
    pub muscle_group: String,
    /// Date (`YYYY-MM-DD`) the group was last trained, or `None` if never.
    pub last_trained: Option<String>,
    /// Sets logged for the group on its most-recent training day; `0` if never trained.
    pub last_volume_sets: i64,
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

fn default_true() -> bool {
    true
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
        timers_enabled: true,
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
        timers_enabled: true,
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

pub fn new_goal(
    user_id: i64,
    kind: GoalKind,
    exercise_type_id: Option<i64>,
    metric: Option<String>,
    target_value: f64,
    direction: GoalDirection,
) -> Goal {
    let now = now_str();
    Goal {
        id: 0,
        user_id,
        kind,
        exercise_type_id,
        metric,
        target_value,
        direction,
        priority: 0,
        start_date: now.clone(),
        target_date: None,
        achieved: false,
        notes: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

/// Convenience for the pre-generalisation shape: a strength goal that raises a
/// single exercise's number (bigger is better).
pub fn new_exercise_goal(user_id: i64, exercise_type_id: i64, target_value: f64) -> Goal {
    new_goal(user_id, GoalKind::Strength, Some(exercise_type_id), None, target_value, GoalDirection::Increase)
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

// ── Workout planner ────────────────────────────────────────────────────────────

/// One append-only entry in a user's distilled training philosophy. The most
/// recent row is the active philosophy; equipment lives as free text in `content`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkoutPhilosophy {
    pub id: i64,
    pub user_id: i64,
    pub content: String,
    /// 'interview' | 'note' | 'import'.
    pub source: String,
    pub created_at: String,
}

/// The interview state for a `(user, platform)` pair. Presence means an
/// interview is in progress; `draft` accumulates the philosophy-so-far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterviewState {
    pub user_id: i64,
    pub platform: String,
    pub mode: String,
    pub draft: String,
    pub turns: i32,
    pub started_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Proposed,
    Active,
    Completed,
    Abandoned,
}

impl PlanStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "active" => Self::Active,
            "completed" => Self::Completed,
            "abandoned" => Self::Abandoned,
            _ => Self::Proposed,
        }
    }
}

impl fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A generated workout plan. Designed by `/nextworkout` (status `Proposed`),
/// bound to a session and marked `Active` during guided execution, then
/// `Completed` when the session ends. A plan never logs sets itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkoutPlan {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub rationale: Option<String>,
    pub philosophy_id: Option<i64>,
    pub status: PlanStatus,
    pub session_id: Option<i64>,
    /// A one-off, today-only override the user voiced mid-workout (e.g. "no bench
    /// today, do flys instead"). Scoped to THIS plan: it never touches the
    /// philosophy and expires when the plan completes or is superseded.
    pub override_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A single prescribed exercise within a [`WorkoutPlan`]. `(target_reps,
/// target_weight_kg)` cover the weight_reps case; `target_secs` covers timed
/// work. `target_sets` is the prescribed number of sets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkoutPlanExercise {
    pub id: i64,
    pub plan_id: i64,
    pub exercise_type_id: i64,
    pub order_idx: i32,
    pub target_sets: Option<i32>,
    pub target_reps: Option<i32>,
    pub target_weight_kg: Option<f64>,
    pub target_secs: Option<i32>,
    pub notes: Option<String>,
}

// ── Prescribed vs actual ───────────────────────────────────────────────────────

/// What a session actually logged for one exercise_type, rolled up over its sets
/// so it can be compared against a single per-exercise prescription. `avg_reps`
/// averages the recorded rep counts; `avg_weight_kg` / `avg_secs` average the set
/// value under the weight_reps / time_based interpretation respectively (only one
/// is populated, per the exercise's measurement type).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformedRollup {
    pub performed_sets: i64,
    pub avg_reps: Option<f64>,
    pub avg_weight_kg: Option<f64>,
    pub avg_secs: Option<f64>,
}

/// The gap between what a plan prescribed and what a session performed for one
/// exercise present on both sides.
///
/// Every `*_delta` is signed `performed − prescribed`: **positive means the
/// athlete exceeded the prescription, negative means they fell short, zero means
/// they hit it.** Deviation is signal, not failure — a consistent overshoot means
/// the plan under-prescribes, a consistent shortfall means it over-prescribes — so
/// consumers (the post-session report, the next-run designer, progression) must
/// read the sign and magnitude, never treat a non-zero delta as an error. A delta
/// is `None` when the prescription or the performance left that dimension unset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseDelta {
    pub exercise_name: String,
    pub measurement_type: MeasurementType,
    /// The plan's prescription for this exercise (targets, order, notes).
    pub prescribed: WorkoutPlanExercise,
    /// The session's rolled-up performance for this exercise.
    pub performed: PerformedRollup,
    /// `performed_sets − target_sets`.
    pub sets_delta: Option<i64>,
    /// `avg_reps − target_reps`.
    pub reps_delta: Option<f64>,
    /// `avg_weight_kg − target_weight_kg`.
    pub weight_delta_kg: Option<f64>,
    /// `avg_secs − target_secs`.
    pub secs_delta: Option<f64>,
}

/// An exercise the plan prescribed that the session never performed
/// (planned-not-performed). Skipping is signal too, not an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedExercise {
    pub exercise_name: String,
    pub prescribed: WorkoutPlanExercise,
}

/// An exercise the session performed that the plan never prescribed
/// (performed-not-planned) — an improvised addition, not an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnplannedExercise {
    pub exercise_type_id: i64,
    pub exercise_name: String,
    pub measurement_type: MeasurementType,
    pub performed: PerformedRollup,
}

/// The full prescribed-vs-actual comparison for a plan bound to a session: the
/// matched exercises with their signed deltas, the prescribed exercises that were
/// skipped, and the performed exercises that were never planned. Closes the loop
/// between what the plan asked for and what the session did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanVsActual {
    pub plan_id: i64,
    pub session_id: i64,
    /// Prescribed and performed, in plan order, with signed deltas.
    pub matched: Vec<ExerciseDelta>,
    /// Prescribed but not performed, in plan order.
    pub skipped: Vec<SkippedExercise>,
    /// Performed but not prescribed, in the order first logged.
    pub unplanned: Vec<UnplannedExercise>,
}

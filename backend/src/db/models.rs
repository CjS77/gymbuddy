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

    /// Ordinal for aggregation: easy < medium < hard < failure.
    pub fn rank(self) -> u8 {
        match self {
            Self::Easy => 0,
            Self::Medium => 1,
            Self::Hard => 2,
            Self::Failure => 3,
        }
    }
}

impl fmt::Display for Difficulty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a session's `overall_effort` came from.
///
/// The two are not equally trustworthy, which is why the column exists: a
/// [`Derived`](Self::Derived) effort is the server's own reading of the last set of
/// each exercise, written when a session ends with nobody around to ask, and it is
/// worth offering back for correction. A [`Confirmed`](Self::Confirmed) one is the
/// user's own verdict and must not be second-guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortSource {
    /// Distilled by `end_session` from the logged sets — a proposal, not an answer.
    Derived,
    /// The user said so, through `RecordSessionOutcome`.
    Confirmed,
}

impl EffortSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Derived => "derived",
            Self::Confirmed => "confirmed",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "confirmed" => Self::Confirmed,
            _ => Self::Derived,
        }
    }

    /// Whether the user stood behind this verdict themselves.
    pub fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed)
    }
}

impl fmt::Display for EffortSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a whole session subjectively felt, as opposed to how mechanically hard it
/// was ([`Difficulty`]): a hard session can still feel great. Part of the
/// session outcome recorded at session end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFeel {
    Great,
    Good,
    Ok,
    Rough,
}

impl SessionFeel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Great => "great",
            Self::Good => "good",
            Self::Ok => "ok",
            Self::Rough => "rough",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "great" | "amazing" | "strong" => Self::Great,
            "good" | "solid" => Self::Good,
            "rough" | "bad" | "terrible" | "awful" => Self::Rough,
            _ => Self::Ok,
        }
    }
}

impl fmt::Display for SessionFeel {
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

/// How much a [`HealthEntry`] constrains training. The v2 `health_entries.severity`
/// CHECK is `('mild','moderate','severe')`, and the column has always held exactly
/// those three strings — this makes the type say so.
///
/// It is not decoration: [C5.4] graduates the designer's response by severity —
/// mild means work around it, severe means do not train it — and a free-text column
/// cannot be matched exhaustively.
///
/// **Ordered, and the variant order is the meaning**: `Mild < Moderate < Severe`, so
/// the contraindication rails ask whether an entry reaches a rule's threshold
/// (`severity >= rule.bars_from`) instead of matching all three cases at every site.
/// Reordering the variants would silently invert a safety check — `severity_is_ordered`
/// in `science::contraindications::tests` pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Mild,
    Moderate,
    Severe,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mild => "mild",
            Self::Moderate => "moderate",
            Self::Severe => "severe",
        }
    }

    /// Anything unrecognised reads as [`Self::Mild`], matching the column default:
    /// an unparseable severity must never silently escalate to "do not train".
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "moderate" => Self::Moderate,
            "severe" => Self::Severe,
            _ => Self::Mild,
        }
    }
}

impl fmt::Display for Severity {
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
    /// Session-level verdict, proposed at session end by distilling the last set
    /// of each exercise and then confirmed or overridden by the user. Unlike
    /// `notes` these fields are structured and feed the designer's feedback loop.
    #[serde(default)]
    pub overall_effort: Option<Difficulty>,
    /// Whether `overall_effort` is the user's own verdict or the server's reading of
    /// the logged sets. `None` when no effort has been settled either way.
    #[serde(default)]
    pub effort_source: Option<EffortSource>,
    /// How the session subjectively felt, independent of `overall_effort`.
    #[serde(default)]
    pub felt: Option<SessionFeel>,
    /// Whether the session ended before the user got through what they intended.
    #[serde(default)]
    pub cut_short: bool,
    #[serde(default)]
    pub cut_short_reason: Option<String>,
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
pub struct HealthEntry {
    pub id: i64,
    pub user_id: i64,
    pub entry_type: HealthEntryType,
    pub body_part: Option<String>,
    pub severity: Severity,
    pub description: String,
    pub started_at: String,
    pub resolved_at: Option<String>,
    pub notes: Option<String>,
    pub updated_at: String,
}

/// One body measurement: a (metric, value, moment) observation. Long-shaped —
/// every metric is a row value, so new metrics (waist, resting HR) are new rows,
/// never new columns. `metric` is the canonical unit-suffixed name (e.g.
/// "bodyweight_kg", "body_fat_pct") shared with [`Goal::metric`], and `value` is
/// in the unit that name carries. Retention and exposure policy for this data
/// lives in the `body_metrics` DAO module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyMetric {
    pub id: i64,
    pub user_id: i64,
    pub metric: String,
    pub value: f64,
    pub measured_at: String,
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

/// A personal record set during one particular session, with the mark it beat.
///
/// Not a [`PersonalRecord`]: that one is an all-time leaderboard row with no session
/// linkage, which is exactly the question a post-session review cannot answer with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPersonalRecord {
    pub exercise_name: String,
    pub measurement_type: MeasurementType,
    /// The new best — weight in kg, seconds, metres, level or score per
    /// `measurement_type`.
    pub value: f64,
    /// Reps at `value`, for `weight_reps` work. `None` for every other measurement.
    pub count: Option<i32>,
    /// The best that stood before this session. `None` when this is the first time the
    /// exercise has been logged at all — a record only in the trivial sense, and one a
    /// review should not celebrate as a breakthrough.
    pub previous_value: Option<f64>,
    pub previous_count: Option<i32>,
}

/// A persisted post-session review, straight out of `session_reviews`.
///
/// `body` stays an opaque JSON string at this layer: the schema's one JSON column is
/// deliberately not interpreted by SQL, and the review generator owns its shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredReview {
    pub session_id: i64,
    pub roster_id: Option<i64>,
    /// `summary` (deterministic, ad-hoc) or `report` (programme mode, with commentary).
    pub kind: String,
    pub body: String,
    pub created_at: String,
}

// ── Constructors (drafts; id is set by the insert function via last_insert_rowid) ──

fn now_str() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Today's date as `YYYY-MM-DD`.
///
/// The date half of [`now_str`], exposed because the programme sweep and status read
/// take "today" as an argument rather than calling `date('now')` inside their SQL —
/// which is what lets a test place itself anywhere in a programme's calendar.
pub fn today_str() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
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
        pubkey: None,
        timezone: timezone.to_string(),
        created_at: now.clone(),
        updated_at: now,
        beta_tester: false,
        timers_enabled: true,
    }
}

/// Construct a user identified by a confide ed25519 public key (hex), used by the
/// confide transport at registration. Has no `telegram_id`.
pub fn new_user_with_pubkey(name: &str, pubkey: &str, timezone: &str) -> User {
    let now = now_str();
    User {
        id: 0,
        name: name.to_string(),
        telegram_id: None,
        pubkey: Some(pubkey.to_string()),
        timezone: timezone.to_string(),
        created_at: now.clone(),
        updated_at: now,
        beta_tester: false,
        timers_enabled: true,
    }
}

pub fn new_session(user_id: i64, notes: Option<&str>) -> Session {
    Session {
        id: 0,
        user_id,
        started_at: now_str(),
        ended_at: None,
        notes: notes.map(String::from),
        overall_effort: None,
        effort_source: None,
        felt: None,
        cut_short: false,
        cut_short_reason: None,
    }
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
        severity: Severity::Mild,
        description: description.to_string(),
        started_at: now.clone(),
        resolved_at: None,
        notes: None,
        updated_at: now,
    }
}

pub fn new_body_metric(user_id: i64, metric: &str, value: f64) -> BodyMetric {
    BodyMetric { id: 0, user_id, metric: metric.to_string(), value, measured_at: now_str() }
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

// ── Philosophy and session rosters ─────────────────────────────────────────────

/// One append-only entry in a user's distilled training philosophy. The most
/// recent row is the active philosophy; equipment lives as free text in `content`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Philosophy {
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

/// The lifecycle a [`SessionRoster`] and a [`Programme`] both follow: `Draft` while it is being
/// designed, `Active` once it is under way, then `Completed` or `Abandoned`.
///
/// One enum for both because the two are the same four states over the same stored strings —
/// `session_rosters.status` and `programmes.status` carry identical v2 CHECK constraints
/// (`'draft','active','completed','abandoned'`), so [`Self::as_str`] must keep returning exactly
/// those values or every write fails the constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    Draft,
    Active,
    Completed,
    Abandoned,
}

impl LifecycleStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }

    /// `draft` and v1's `proposed` both parse to [`Self::Draft`] — v1 spelled a roster's first
    /// state `proposed`, and rows arriving through a dump of a legacy database still say so. The
    /// importer ([R1.3]) relies on that, so the acceptance must stay even though nothing writes it.
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "active" => Self::Active,
            "completed" => Self::Completed,
            "abandoned" => Self::Abandoned,
            _ => Self::Draft,
        }
    }
}

impl fmt::Display for LifecycleStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The set of exercises designed for one session — the built-session artifact.
/// Designed by `/nextworkout` (status `Draft`), bound to a session and marked
/// `Active` during guided execution, then `Completed` when the session ends. A
/// roster prescribes; it never logs sets itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRoster {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub rationale: Option<String>,
    pub philosophy_id: Option<i64>,
    pub status: LifecycleStatus,
    pub session_id: Option<i64>,
    /// A one-off, today-only override the user voiced mid-workout (e.g. "no bench
    /// today, do flys instead"). Scoped to THIS roster: it never touches the
    /// philosophy and expires when the roster completes or is superseded.
    pub override_note: Option<String>,
    /// The programme slot this roster filled, or `None` for an ad-hoc roster —
    /// the first-class default; binding to a slot is a separate, optional step.
    #[serde(default)]
    pub programme_slot_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// A single prescribed exercise within a [`SessionRoster`]. `(target_reps,
/// target_weight_kg)` cover the weight_reps case; `target_secs` covers timed
/// work. `target_sets` is the prescribed number of sets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterExercise {
    pub id: i64,
    pub roster_id: i64,
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

/// The gap between what a roster prescribed and what a session performed for one
/// exercise present on both sides.
///
/// Every `*_delta` is signed `performed − prescribed`: **positive means the
/// athlete exceeded the prescription, negative means they fell short, zero means
/// they hit it.** Deviation is signal, not failure — a consistent overshoot means
/// the roster under-prescribes, a consistent shortfall means it over-prescribes — so
/// consumers (the post-session report, the next-run designer, progression) must
/// read the sign and magnitude, never treat a non-zero delta as an error. A delta
/// is `None` when the prescription or the performance left that dimension unset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExerciseDelta {
    pub exercise_name: String,
    pub measurement_type: MeasurementType,
    /// The roster's prescription for this exercise (targets, order, notes).
    pub prescribed: RosterExercise,
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

/// An exercise the roster prescribed that the session never performed
/// (rostered-not-performed). Skipping is signal too, not an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedExercise {
    pub exercise_name: String,
    pub prescribed: RosterExercise,
}

/// An exercise the session performed that the roster never prescribed
/// (performed-not-rostered) — an improvised addition, not an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnrosteredExercise {
    pub exercise_type_id: i64,
    pub exercise_name: String,
    pub measurement_type: MeasurementType,
    pub performed: PerformedRollup,
}

/// The full prescribed-vs-actual comparison for a roster bound to a session: the
/// matched exercises with their signed deltas, the prescribed exercises that were
/// skipped, and the performed exercises that were never rostered. Closes the loop
/// between what the roster asked for and what the session did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterVsActual {
    pub roster_id: i64,
    pub session_id: i64,
    /// Prescribed and performed, in roster order, with signed deltas.
    pub matched: Vec<ExerciseDelta>,
    /// Prescribed but not performed, in roster order.
    pub skipped: Vec<SkippedExercise>,
    /// Performed but not prescribed, in the order first logged.
    pub unrostered: Vec<UnrosteredExercise>,
}

// ── Programmes ─────────────────────────────────────────────────────────────────

/// Lifecycle of a [`ProgrammeSlot`]: `Pending` until a designed roster binds to it
/// (`Filled`), `Missed` when its week passes untouched, `Skipped` when
/// deliberately dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    Pending,
    Filled,
    Missed,
    Skipped,
}

impl SlotStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Filled => "filled",
            Self::Missed => "missed",
            Self::Skipped => "skipped",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "filled" => Self::Filled,
            "missed" => Self::Missed,
            "skipped" => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

impl fmt::Display for SlotStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A long-term training programme: a skeleton, not a script. It persists the
/// goals served (via `programme_goals`), dates, split and progression policy;
/// each session keeps being designed on demand against it. `split` and
/// `progression_policy` are free text the LLM reads — no query looks inside.
///
/// At most one programme per user is [`LifecycleStatus::Active`], enforced by
/// [`Database::activate_programme`](super::Database).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Programme {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub start_date: String,
    /// The date the programme aims to conclude by. NULL = open-ended.
    pub target_end_date: Option<String>,
    pub days_per_week: i32,
    pub split: String,
    pub progression_policy: String,
    pub status: LifecycleStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// A mesocycle block within a [`Programme`]: an inclusive 1-based week range with
/// an intent (weeks 1–4 "hypertrophy", 5–6 "deload"). The designer reads the
/// block the current week falls in and progresses within it — this is what
/// makes sessions build on one another rather than repeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgrammeBlock {
    pub id: i64,
    pub programme_id: i64,
    pub start_week: i32,
    pub end_week: i32,
    pub focus: String,
    pub notes: Option<String>,
}

/// One cell of a [`Programme`]'s week/day grid. `week_idx` is 1-based from the
/// programme start; `day_idx` is the 1-based ordinal training day within the
/// week (not a calendar weekday).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgrammeSlot {
    pub id: i64,
    pub programme_id: i64,
    pub week_idx: i32,
    pub day_idx: i32,
    pub focus: String,
    pub status: SlotStatus,
    pub updated_at: String,
}

/// The mode a session design runs in ([C1.4]). Ad-hoc is the first-class
/// default: it never requires a programme, and it stays available while one is
/// active as a deliberate one-off that leaves every slot untouched. Resolved by
/// [`Database::training_mode_for_design`](super::Database), read at the
/// `/nextworkout` entry point, and surfaced to the user via
/// [`gymbuddy_proto::TrainingModeView`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrainingMode {
    /// A one-off design. `programme` is the active programme deliberately sat out
    /// (or already fully resolved), `None` when the user has no programme at all —
    /// in which case behaviour is exactly the pre-programme one.
    AdHoc { programme: Option<Programme> },
    /// The design fills `slot` of the active `programme`; persisting it stamps the
    /// roster's `programme_slot_id` and marks the slot filled.
    Programme { programme: Programme, slot: ProgrammeSlot },
}

/// Adherence over the slots scheduled before the one being designed: how many there were, how
/// many were actually trained, and how the rest were resolved. Counts only — the designer needs
/// to know whether the programme is being kept to, not to re-read the grid.
///
/// "Trained" is deliberately the same test
/// [`next_design_slot`](super::Database::next_design_slot) applies: a [`SessionRoster`] bound to
/// the slot that reached [`LifecycleStatus::Active`] or [`LifecycleStatus::Completed`]. A slot
/// merely `filled` with a design nobody executed is not adherence, and using one rule in two
/// places is what keeps slot selection and the adherence the prompt reports from disagreeing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotAdherence {
    /// Slots earlier in the grid than the one being designed.
    pub due: i32,
    /// How many of those were actually trained.
    pub trained: i32,
    pub missed: i32,
    pub skipped: i32,
}

/// Where one design sits in its [`Programme`] ([C4.3]): the week and day its slot names, the
/// mesocycle [`ProgrammeBlock`] covering that week, the slot's focus, and adherence so far. This
/// is what the designer prompt states so a session builds on the last one instead of being
/// designed in a vacuum.
///
/// Read per design by [`Database::programme_context`](super::Database) and never persisted —
/// every field is derived from the programme grid, so it cannot drift from it.
///
/// Only [`TrainingMode::Programme`] resolves one. An ad-hoc design fills no slot, including a
/// deliberate one-off under an active programme, so there is no position for it to report and the
/// prompt carries no programme section at all.
#[derive(Debug, Clone)]
pub struct ProgrammeContext {
    pub programme_title: String,
    /// 1-based week the slot sits in, and the last week the grid has slots for.
    pub week_idx: i32,
    pub total_weeks: i32,
    /// 1-based training day within the week, and the programme's nominal days per week.
    pub day_idx: i32,
    pub days_per_week: i32,
    /// The slot's own focus text ("push") — an intent, never an exercise list.
    pub slot_focus: String,
    /// The block covering `week_idx`, when the programme has one there. Blocks are not required
    /// to tile the whole programme, so a week between them has none.
    pub block: Option<ProgrammeBlock>,
    pub adherence: SlotAdherence,
}

/// How a whole programme grid has resolved so far ([R2.1]): every slot falls into exactly
/// one bucket, so the four add up to the grid and can be reported as a whole.
///
/// `trained` applies the same test as [`SlotAdherence`] — a bound [`SessionRoster`] that
/// reached [`LifecycleStatus::Active`] or [`LifecycleStatus::Completed`] — narrowed to
/// slots that are neither `missed` nor `skipped`, which is what keeps the buckets disjoint
/// when a slot was designed and then dropped by hand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotCounts {
    /// Every slot in the grid.
    pub total: i32,
    pub trained: i32,
    pub missed: i32,
    pub skipped: i32,
}

impl SlotCounts {
    /// Slots still ahead: neither trained, missed nor skipped. Saturating, so a grid that
    /// somehow over-counts reports zero rather than a negative remainder.
    pub fn remaining(&self) -> i32 {
        (self.total - self.trained - self.missed - self.skipped).max(0)
    }
}

/// Where a *live* programme has got to ([R2.1]), as `/programme status` reports it.
///
/// Distinct from [`ProgrammeContext`], which answers "what should this one session be?"
/// for the designer. This answers "where am I?" for the user: it exists whether or not a
/// design is in flight, spans the whole grid rather than the slots before one cell, and
/// reads the calendar as well as the grid.
///
/// `current_week` comes off `start_date` and today, so it reports where the user *is*;
/// `next_slot` comes off the grid, so it reports what is *due*. The two agree once the
/// missed-slot sweep has run, and diverge legitimately when a user trains a week's slots
/// early — reporting both is what makes that legible rather than contradictory.
#[derive(Debug, Clone)]
pub struct ProgrammeStatus {
    /// 1-based, clamped into `1..=total_weeks`, so a programme run past its end reports
    /// its last week rather than a week the grid does not have.
    pub current_week: i32,
    /// Weeks the grid spans — the same figure [`ProgrammeContext::total_weeks`] carries.
    pub total_weeks: i32,
    /// The block covering `current_week`, when the programme has one there.
    pub block: Option<ProgrammeBlock>,
    /// The next session due, `None` once every slot is settled.
    pub next_slot: Option<ProgrammeSlot>,
    pub counts: SlotCounts,
}

pub fn new_programme(user_id: i64, title: &str, days_per_week: i32, split: &str, progression_policy: &str) -> Programme {
    let now = now_str();
    Programme {
        id: 0,
        user_id,
        title: title.to_string(),
        start_date: now.clone(),
        target_end_date: None,
        days_per_week,
        split: split.to_string(),
        progression_policy: progression_policy.to_string(),
        status: LifecycleStatus::Draft,
        created_at: now.clone(),
        updated_at: now,
    }
}

pub fn new_programme_block(programme_id: i64, start_week: i32, end_week: i32, focus: &str) -> ProgrammeBlock {
    ProgrammeBlock { id: 0, programme_id, start_week, end_week, focus: focus.to_string(), notes: None }
}

pub fn new_programme_slot(programme_id: i64, week_idx: i32, day_idx: i32, focus: &str) -> ProgrammeSlot {
    ProgrammeSlot { id: 0, programme_id, week_idx, day_idx, focus: focus.to_string(), status: SlotStatus::Pending, updated_at: now_str() }
}

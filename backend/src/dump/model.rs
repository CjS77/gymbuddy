//! The dump envelope and its per-user tree.
//!
//! Every type here is **v2 vocabulary**, whichever schema generation produced it: the v1 reader
//! translates as it reads (see the module doc on [`crate::dump`]). Ids that appear are always the
//! *source* database's ids, kept only so intra-user references (a roster pointing at its session,
//! a programme pointing at its goals) survive the trip; the importer rebuilds them through
//! translation maps and must never assume they are free in the target database.
//!
//! Exercise references travel as [`ExerciseRef`] — canonical taxonomy name plus parent name —
//! never as catalogue ids, which drift between generations.

use serde::{Deserialize, Serialize};

/// Envelope discriminator. A file whose `format` is not this is not a GymBuddy dump.
pub const DUMP_FORMAT: &str = "gymbuddy.dump";

/// Version of the *envelope shape* (not of the schema that produced it). Bump when the dump
/// layout changes incompatibly; the importer refuses versions it does not understand.
pub const DUMP_VERSION: u32 = 1;

/// A complete export: global reference data plus one tree per user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dump {
    /// Always [`DUMP_FORMAT`].
    pub format: String,
    /// Always [`DUMP_VERSION`].
    pub dump_version: u32,
    /// Which schema generation this was read from.
    pub source_schema: SourceSchema,
    /// RFC 3339 timestamp of the export itself.
    pub exported_at: String,
    /// Access groups are global, so they sit beside the user trees; membership is per user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
    pub users: Vec<User>,
}

/// Provenance of a dump: which schema generation the reader saw, and the raw `PRAGMA user_version`
/// it carried. `generation` drives importer behaviour; `user_version` is kept for forensics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSchema {
    /// `1` = legacy (pre-realignment, `workout_plans`/`schedules`), `2` = the SessionRoster schema.
    pub generation: u32,
    /// `PRAGMA user_version` as read from the source database.
    pub user_version: i64,
}

/// A reference into the exercise taxonomy by name rather than by id.
///
/// `parent` disambiguates the same leaf name appearing under two parents (the v1 catalogue's
/// uniqueness constraint is `UNIQUE (parent_id, name)`, so name alone is not a key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExerciseRef {
    pub name: String,
    /// Name of the parent taxonomy node; `None` only for muscle groups, which have no parent.
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMembership {
    /// Group name — `groups.name` is UNIQUE, so it keys the membership without an id.
    pub group: String,
    pub level: String,
    pub granted_at: String,
}

/// One user and everything hanging off them. Deleting this subtree from the source database would
/// leave no orphans behind, which is what makes per-user trees the right unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// Source `users.id`. Referenced by nothing inside the tree (the tree *is* the reference), but
    /// kept so an operator can correlate a dump against the database it came from.
    pub id: i64,
    pub name: String,
    pub telegram_id: Option<String>,
    pub pubkey: Option<String>,
    pub timezone: String,
    pub beta_tester: bool,
    pub timers_enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_memberships: Vec<GroupMembership>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub philosophies: Vec<Philosophy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interview_states: Vec<InterviewState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<Goal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
    /// Exercise entries with a NULL `session_id` — sets logged outside any session. They belong to
    /// the user, not to a session, so they cannot hang under [`Session`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsessioned_entries: Vec<ExerciseEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_rosters: Vec<SessionRoster>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub programmes: Vec<Programme>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub health_entries: Vec<HealthEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_metrics: Vec<BodyMetric>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation_history: Vec<ConversationMessage>,
    /// v2-only; a v1 export always leaves this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_reviews: Vec<SessionReview>,
    /// Data that schema v2 deliberately drops. Archived so the dump stays a faithful backup of the
    /// source database; the importer ignores it.
    #[serde(default)]
    pub legacy: Legacy,
}

impl User {
    /// Every exercise entry in the tree, wherever it hangs.
    ///
    /// Entries live in two places — under their session, and under the user when `session_id` was
    /// NULL — and code that walks only [`User::sessions`] silently loses the second kind. Anything
    /// meaning "all of this user's entries" should come through here rather than rebuild the chain
    /// and risk forgetting the tail.
    pub fn entries(&self) -> impl Iterator<Item = &ExerciseEntry> {
        self.sessions.iter().flat_map(|session| &session.entries).chain(&self.unsessioned_entries)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Philosophy {
    /// Source id — [`SessionRoster::philosophy_id`] points here.
    pub id: i64,
    pub content: String,
    pub source: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterviewState {
    pub platform: String,
    pub mode: String,
    pub draft: String,
    pub turns: i64,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    /// Source id — [`Programme::goal_ids`] points here.
    pub id: i64,
    pub kind: String,
    /// Set for exercise-denominated goals; mutually exclusive with `metric` in practice, and the
    /// schema requires at least one of the two.
    pub exercise: Option<ExerciseRef>,
    /// Metric *name* (e.g. `bodyweight_kg`). v2 stores a `metric_id`; the importer resolves or
    /// creates the row, so the dump carries the name.
    pub metric: Option<String>,
    pub target_value: f64,
    pub direction: String,
    pub priority: i64,
    pub start_date: String,
    pub target_date: Option<String>,
    pub achieved: bool,
    /// v2-only column; a v1 export always leaves this `None`.
    pub achieved_at: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Source id — [`SessionRoster::session_id`] points here.
    pub id: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// v1 `sessions.notes` with the `plan:<name>` sentinel stripped. The stripped name is archived
    /// in [`Legacy::session_plan_names`].
    pub intent: Option<String>,
    pub overall_effort: Option<String>,
    /// v2-only column; a v1 export always leaves this `None`.
    pub effort_source: Option<String>,
    pub felt: Option<String>,
    pub cut_short: bool,
    pub cut_short_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<ExerciseEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExerciseEntry {
    pub id: i64,
    pub start_timestamp: String,
    pub end_timestamp: Option<String>,
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sets: Vec<Set>,
}

/// A recorded effort. `(count, value)` is polymorphic over `measurement_type` exactly as in the
/// schema: `weight_reps` uses count = reps and value = kg, every other type leaves count NULL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Set {
    pub exercise: ExerciseRef,
    pub order_idx: i64,
    /// Measurement type *name* (`weight_reps`, `time_based`, ...), not its id.
    pub measurement_type: String,
    pub count: Option<i64>,
    pub value: f64,
    pub perceived_difficulty: String,
    pub comment: Option<String>,
    pub logged_at: String,
}

/// The built session artefact. v1 called this a `workout_plan`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRoster {
    pub id: i64,
    pub title: String,
    pub rationale: Option<String>,
    /// → [`Philosophy::id`] within the same user tree.
    pub philosophy_id: Option<i64>,
    /// `draft` | `active` | `completed` | `abandoned`. v1's `proposed` maps to `draft`.
    pub status: String,
    /// → [`Session::id`] within the same user tree.
    pub session_id: Option<i64>,
    /// → [`ProgrammeSlot::id`] within the same user tree.
    pub programme_slot_id: Option<i64>,
    pub override_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exercises: Vec<RosterExercise>,
}

/// A prescribed exercise within a roster. v1 called this a `workout_plan_exercise`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RosterExercise {
    pub exercise: ExerciseRef,
    pub order_idx: i64,
    pub target_sets: Option<i64>,
    pub target_reps: Option<i64>,
    pub target_weight_kg: Option<f64>,
    pub target_secs: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Programme {
    pub id: i64,
    pub title: String,
    pub start_date: String,
    pub target_end_date: Option<String>,
    pub days_per_week: i64,
    pub split: String,
    pub progression_policy: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// → [`Goal::id`] within the same user tree (v1 `program_goals`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal_ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<ProgrammeBlock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<ProgrammeSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeBlock {
    pub start_week: i64,
    pub end_week: i64,
    pub focus: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeSlot {
    /// Source id — [`SessionRoster::programme_slot_id`] points here.
    pub id: i64,
    pub week_idx: i64,
    pub day_idx: i64,
    pub focus: String,
    pub status: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthEntry {
    pub entry_type: String,
    pub body_part: Option<String>,
    pub severity: String,
    pub description: String,
    pub started_at: String,
    pub resolved_at: Option<String>,
    pub notes: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BodyMetric {
    /// Metric *name* (e.g. `bodyweight_kg`); v2 resolves it to a `metrics` row on import.
    pub metric: String,
    pub value: f64,
    pub measured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub platform: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub exclude_from_context: bool,
}

/// v2-only. Carried in the envelope so a v2 export round-trips; a v1 export never produces one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReview {
    /// → [`Session::id`] within the same user tree.
    pub session_id: i64,
    /// → [`SessionRoster::id`] within the same user tree.
    pub roster_id: Option<i64>,
    pub kind: String,
    /// Opaque JSON document (a review snapshot); the dump does not interpret it.
    pub body: String,
    pub created_at: String,
}

/// Archival only. Schema v2 drops all of this; it is exported so a dump remains a complete backup
/// of its source database, and the importer skips the whole block.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Legacy {
    /// v1 `users.signal_id` — the Signal transport never shipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_id: Option<String>,
    /// v1 `schedules` + `schedule_exercises` — cron reminders that never fired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<LegacySchedule>,
    /// Schedule names recovered from the `plan:<name>` sentinel in v1 `sessions.notes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_plan_names: Vec<LegacySessionPlan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacySchedule {
    pub name: String,
    pub cron_expr: String,
    pub reminder_type: String,
    pub reminder_notice_mins: i64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exercises: Vec<LegacyScheduleExercise>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyScheduleExercise {
    pub exercise: ExerciseRef,
    pub order_idx: i64,
    pub target_sets: Option<i64>,
    pub target_reps: Option<i64>,
    pub target_weight_kg: Option<f64>,
}

/// The `plan:<name>` prefix stripped out of one session's notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacySessionPlan {
    /// → [`Session::id`] within the same user tree.
    pub session_id: i64,
    pub plan: String,
}

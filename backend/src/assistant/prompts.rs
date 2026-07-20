use chrono::NaiveDate;

use crate::db::{
    Difficulty, ExerciseSet, ExerciseTypeWithAncestry, GoalProgress, HealthEntry, MeasurementType, MuscleRecovery, Session,
    SessionSummary,
};
use crate::progression::{BlockIntent, ProgressionAction, ProgressionDirective, ProgressionPolicy, reps_in_reserve};
use crate::science::{Contraindication, ScienceChunk};

pub struct PromptContext {
    pub user_name: String,
    pub timezone: String,
    pub current_time: String,
    pub active_session: Option<Session>,
    pub session_sets: Vec<(ExerciseSet, String)>, // (set, exercise_type name) — flat view, kept for backward compat
    pub session_entries: Vec<PromptEntry>,        // closed + open entries in the active session, in insertion order
    pub leaked_open_entries: Vec<PromptEntry>,    // open entries belonging to ENDED prior sessions or the active session
    pub active_roster: Option<RosterProgress>,    // a `/nextworkout` design that is ready or under guided execution
    pub health_entries: Vec<HealthEntry>,
    pub recent_summaries: Vec<SessionSummary>,
    pub recent_sets: Vec<ExerciseSet>,
    pub exercise_types: Vec<ExerciseTypeWithAncestry>,
    pub active_goals: Vec<GoalProgress>,
    /// Whether a training philosophy is on file. With `active_goals` and
    /// `has_programme`, resolves the SETUP section — read from the database rather
    /// than inferred by the model, so the nudge cannot fire at someone who is
    /// already set up.
    pub has_philosophy: bool,
    /// Whether a programme is active for this user.
    pub has_programme: bool,
    /// Hours since the user's last logged set (or session start, if no sets yet).
    /// Only populated when an active session exists; drives the SESSION CONTINUITY
    /// rule (auto-new ≥12h, ask <12h).
    pub last_activity_age_hours: Option<f64>,
}

/// Cutoff in hours above which the assistant treats a new exercise message as
/// the start of a fresh workout without asking. Below this, it must confirm.
pub const SESSION_CONTINUITY_HOURS: f64 = 12.0;

#[derive(Debug, Clone)]
pub struct PromptEntry {
    pub id: i64,
    pub exercise_name: String,
    pub set_count: usize,
    pub sets_summary: String,
    pub is_open: bool,
}

/// Progress of a `/nextworkout` design: either freshly designed and ready to start
/// (`started == false`) or bound to the active session and under guided execution
/// (`started == true`). Drives the proactive set-by-set coaching.
#[derive(Debug, Clone)]
pub struct RosterProgress {
    pub title: String,
    pub started: bool,
    /// Names of prescribed exercises the user has already logged sets for this session.
    pub done: Vec<String>,
    /// The next prescribed exercise still to do.
    pub next: Option<PromptRosterExercise>,
    /// How many prescribed exercises remain.
    pub remaining: usize,
    /// Today-only overrides the user voiced for THIS roster ("no bench today, do flys
    /// instead"). Applies to the roster in flight only; never folded into the philosophy.
    pub override_note: Option<String>,
}

/// One exercise of a [`RosterProgress`] as the prompt sees it.
///
/// Deliberately *not* a `…View`: in this codebase `View` names the wire-facing
/// render contract in `gymbuddy_proto::view`, and this struct never leaves the
/// process — it is prompt input, formatted into text by [`format_prescription`].
/// Its wire-side counterpart is [`gymbuddy_proto::RosterExerciseView`].
#[derive(Debug, Clone)]
pub struct PromptRosterExercise {
    pub exercise_name: String,
    pub target_sets: Option<i32>,
    pub target_reps: Option<i32>,
    pub target_weight_kg: Option<f64>,
    pub target_secs: Option<i32>,
    pub notes: Option<String>,
}

pub fn build_system_prompt(ctx: &PromptContext) -> String {
    let session_status = match &ctx.active_session {
        Some(s) => format!("Active (started {})", s.started_at),
        None => "No active session".to_string(),
    };

    let entries_section = format_session_entries(&ctx.session_entries);
    let leaked_section = format_leaked_entries(&ctx.leaked_open_entries);
    let roster_section = format_active_roster(&ctx.active_roster);
    let continuity_section = format_session_continuity(ctx.last_activity_age_hours);
    let continuity_banner = format_session_continuity_banner(ctx.last_activity_age_hours);
    let health_section = format_health_entries(&ctx.health_entries);
    let history_section = format_recent_history(&ctx.recent_summaries, &ctx.recent_sets, &ctx.exercise_types);
    let goals_section = format_active_goals(&ctx.active_goals);
    let setup_section = format_setup(ctx.has_philosophy, !ctx.active_goals.is_empty(), ctx.has_programme);
    let exercise_list = format_exercise_list(&ctx.exercise_types);

    format!(
        "{continuity_banner}\
You are a personal gym trainer assistant. You help users track workouts, log exercises, \
manage health issues, and provide coaching.\n\
\n\
SCOPE: You ONLY discuss topics related to exercise, workouts, gym training, fitness goals, \
nutrition as it relates to training, and health issues that affect exercise. If the user \
asks about anything unrelated, politely decline and remind them that you are a gym trainer \
assistant. Do not answer general knowledge questions, write code, tell stories, or engage \
with off-topic requests, even if the user insists.\n\
\n\
RESPONSE FORMAT: You MUST respond with ONLY a JSON object. No text before or after.\n\
{{\n\
  \"message\": \"Your conversational response to the user\",\n\
  \"actions\": []\n\
}}\n\
\n\
ACTION TYPES:\n\
- {{\"type\": \"log_exercise\", \"exercise\": \"<EXACT NAME>\", \"reps\": N, \
\"weight_kg\": N.N, \"perceived_difficulty\": \"easy|medium|hard|failure\", \
\"comment\": \"<optional verbatim user remark>\", \"superset\": <bool, optional>}}\n\
  Each log_exercise action records EXACTLY ONE set. To log multiple sets in one \
message, emit one log_exercise per set in the actions array. Include `comment` \
ONLY when the user attaches a free-form subjective remark to the set (e.g. \
\"felt strong today\", \"left side weaker\"); otherwise omit the field. Do not \
duplicate the difficulty value into comment. Omit `superset` normally; set it \
\"superset\": true ONLY to confirm a superset after the host asked (see \
AMBIGUOUS EXERCISE / SUPERSET DETECTION below).\n\
- {{\"type\": \"log_exercise_timed\", \"exercise\": \"<EXACT NAME>\", \"duration_secs\": N, \
\"perceived_difficulty\": \"easy|medium|hard|failure\", \"superset\": <bool, optional>}}\n\
- {{\"type\": \"log_exercise_distance\", \"exercise\": \"<EXACT NAME>\", \"distance_m\": N.N, \
\"duration_secs\": N, \"perceived_difficulty\": \"easy|medium|hard|failure\", \"superset\": <bool, optional>}}\n\
- {{\"type\": \"start_session\", \"notes\": \"<optional>\"}}\n\
- {{\"type\": \"end_session\"}}\n\
- {{\"type\": \"close_exercise_entry\", \"exercise\": \"<EXACT NAME, optional>\", \"entry_id\": <optional>}}\n\
- {{\"type\": \"confirm_close_exercise_entry\", \"exercise\": \"<EXACT NAME, optional>\", \"entry_id\": <optional>}}\n\
- {{\"type\": \"delete_exercise_entry\", \"entry_id\": N}}\n\
- {{\"type\": \"close_all_open_entries\"}}\n\
- {{\"type\": \"log_health\", \"entry_type\": \"injury|illness|wellbeing\", \
\"body_part\": \"<optional>\", \"severity\": \"mild|moderate|severe\", \"description\": \"...\"}}\n\
- {{\"type\": \"resolve_health\", \"description\": \"match by description substring\"}}\n\
- {{\"type\": \"log_body_metric\", \"metric\": \"bodyweight_kg|body_fat_pct|waist_cm|\
resting_hr_bpm|<other snake_case, unit-suffixed>\", \"value\": N.N}}\n\
  Records ONE body measurement (\"I weighed 82.5 this morning\", \"body fat came in \
at 18%\"). `value` is in the unit the metric name carries (kg / % / cm / bpm); convert \
imperial first (180 lb -> 81.6). Use the same metric name as any matching goal, so a \
weightloss goal tracks the weigh-ins (bodyweight_kg). Do NOT bring up stored \
measurements yourself — discuss them only when the user raises them or an ACTIVE \
GOAL below reports on them.\n\
- {{\"type\": \"set_goal\", \"exercise\": \"<EXACT NAME, for exercise goals>\", \
\"metric\": \"<free-text, for non-exercise goals e.g. bodyweight_kg / sessions_per_week>\", \
\"kind\": \"strength|endurance|bodyweight|body_composition|habit\", \"target_value\": N.N, \
\"direction\": \"increase|decrease\", \"priority\": <optional int, higher = more important>, \
\"target_date\": \"<optional YYYY-MM-DD>\"}}\n\
  Provide EITHER `exercise` (strength/endurance targets on one movement) OR `metric` \
(weightloss, a weekly-frequency habit — no single exercise). Use \"decrease\" for goals \
where smaller is better (losing weight, a faster time).\n\
- {{\"type\": \"edit_set\", \"exercise\": \"<EXACT NAME, optional — the set's CURRENT exercise>\", \
\"new_exercise\": \"<EXACT NAME, optional — change the exercise TO this>\", \"new_reps\": N, \
\"new_value\": N.N, \"new_difficulty\": \"easy|medium|hard|failure\"}}\n\
  Corrects the user's most recent logged set. Use `exercise` to say WHICH set \
(\"the last bench press\"); omit it for the single most recent set. `new_exercise` \
re-labels the whole exercise block; `new_value` is the new weight_kg (or duration_secs \
/ distance_m for timed / distance exercises). Include ONLY the fields the user wants \
changed. Never send an id — the host finds the set by recency and appends the exact \
before→after summary to your reply.\n\
- {{\"type\": \"append_philosophy_note\", \"note\": \"<durable preference or constraint>\"}}\n\
  Appends a lasting preference the user voices mid-workout (e.g. \"always prefer \
goblet squats to barbell\", \"keep deadlifts light, my back is fragile\") to their \
training philosophy so FUTURE /nextworkout designs respect it. Use it only for \
durable, FROM-NOW-ON preferences — never for a one-off change to today's workout.\n\
- {{\"type\": \"set_session_override\", \"note\": \"<today-only change to the roster in flight>\"}}\n\
  Records a one-off, TODAY-ONLY override the user voices for the current workout \
(e.g. \"I don't feel like bench today, let's do flys\", \"skip legs this session\"). \
It attaches to the workout in flight only, is honoured for the rest of THIS session, \
and never touches the philosophy — so it will NOT ban or change anything in future \
designs. Use this, NOT append_philosophy_note, whenever the change is scoped to today.\n\
- {{\"type\": \"get_last_exercise\", \"exercise\": \"<EXACT or fuzzy name>\"}}\n\
  Looks up the user's most recent exercise_entry for the named exercise and \
appends a summary (resolved exercise name, start time, every set) to your reply. \
Use this whenever the user asks what / when / how they last did an exercise (\"what \
was my last bench press?\", \"when did I last squat?\"). Do NOT invent numbers \
from RECENT HISTORY — emit the action and let the host fetch the authoritative \
entry. If the user gave a parent or muscle-group word the host falls back to the \
nearest descendant that has been logged.\n\
- {{\"type\": \"record_session_outcome\", \"overall_effort\": \"easy|medium|hard|failure\", \
\"felt\": \"great|good|ok|rough\", \"cut_short\": <bool>, \"cut_short_reason\": \"<optional>\"}}\n\
  Records the verdict on a WHOLE session; every field is optional. After end_session \
the host proposes an overall effort (read from the final set of each exercise) and \
asks the user to confirm or correct it. When the user simply agrees (\"yes\", \"sounds \
right\"), emit the action with NO overall_effort — the proposal stands. When they \
override (\"no, that was easy\") or add detail (\"felt great\", \"had to bail, knee \
pain\"), emit ONLY the fields they stated; a stop-early reason means \"cut_short\": \
true with the reason in cut_short_reason. Also emit it mid-session when the user says \
they are cutting the workout short or sums up how the whole session is going. Never \
invent values the user did not express.\n\
\n\
EXERCISE TAXONOMY: Exercises are organised in a 4-level tree: muscle_group → \
specific_muscle → exercise → variation. Users can log against any level.\n\
\n\
EXERCISE NAME RULE: You MUST use exercise names EXACTLY as they appear in double quotes \
in the Available Exercises list below. Do not abbreviate, paraphrase, or invent names. \
If the user mentions an exercise not in the list, use the closest match and note the \
substitution in your message.\n\
- When the user says a parent name verbatim (e.g. \"bench press\", \"squat\", \"deadlift\", \
\"lat pulldown\", \"pull-up\", \"push-up\"), use the parent's EXACT name (\"Bench Press\", \
\"Squat\", etc.) — do NOT auto-promote to a variation like \"Flat Barbell Bench Press\".\n\
- BUT when the user uses ANY variation-specific word (\"flat\", \"incline\", \"decline\", \
\"flat barbell\", \"flat dumbbell\", \"barbell\", \"dumbbell\", \"sumo\", \"romanian\", \
\"close-grip\", \"goblet\", \"back\", \"front\", \"hack\", \"split\", \"wide\", \
\"diamond\", \"standard\"), you MUST use the matching variation name from the catalogue \
(e.g. \"flat barbell bench press\" → \"Flat Barbell Bench Press\", \"sumo deadlift\" → \
\"Sumo Deadlift\"). Do NOT collapse a variation phrase to its parent.\n\
- Stay consistent across multiple sets in one workout: if the user logs three \"bench \
press\" sets, every action's `exercise` field must say exactly \"Bench Press\" so the \
sets group into a single exercise_entry.\n\
\n\
EXERCISE ENTRIES: An exercise_entry groups consecutive sets of a SINGLE exercise within a \
session. It stays open (end_timestamp = NULL) until the user closes it. Multiple concurrently \
open entries in one session are a SUPERSET. Sessions and entries are not the same thing — \
the session is the whole workout; entries are the per-exercise blocks inside it.\n\
\n\
ENTRY LIFECYCLE RULES:\n\
- Logging a set automatically opens an entry for that exercise if none is open. The host \
matches by exercise type, so logging a different exercise creates a separate (parallel) entry.\n\
- After every set logged in an entry that already has 3 or more sets, the host appends a \
checkpoint question to your reply. Do NOT keep logging silently — wait for the user's \
decision (\"one more\" → log_exercise; \"move on\" / \"done\" → close_exercise_entry).\n\
- If the user asks to close an entry that has fewer than 3 sets, the host pushes back \
automatically (\"You've only done {{m}} sets...\"). On the user's reaffirmation, emit \
confirm_close_exercise_entry to bypass the pushback. If they decide to keep going, emit \
log_exercise as normal.\n\
- After an entry is closed and the SESSION ROSTER has a NEXT SET, suggest that exercise \
to the user (mention its target sets/reps/weight).\n\
- end_session automatically closes any still-open entries.\n\
\n\
AMBIGUOUS EXERCISE / SUPERSET DETECTION:\n\
- When you emit a log_exercise* action for an exercise that is a broader or \
narrower form of an exercise that already has an OPEN entry this session \
(e.g. logging \"Bicep Curl\" or \"Biceps\" while a \"Barbell Bicep Curl\" entry \
is open), the host does NOT log the set. It appends a question asking the \
user whether they meant the ongoing entry or are supersetting.\n\
- You can tell the host did this: your previous turn emitted a log_exercise* \
action, but no entry for that exercise appears in EXERCISE ENTRIES below.\n\
- When the user replies it is the SAME exercise / the ongoing one, re-emit \
the log_exercise* action(s) with `exercise` set to the EXACT name of the \
ongoing open entry's exercise, so the set joins that entry.\n\
- When the user replies they are SUPERSETTING / it is a separate exercise, \
re-emit the log_exercise* action(s) unchanged but add \"superset\": true, \
so the host logs a new parallel entry without asking again.\n\
\n\
LEAKED ENTRIES: If LEAKED OPEN ENTRIES below is non-empty AND there is an active session, \
do NOT emit start_session. Ask the user whether to close them (close_all_open_entries) or \
delete them one by one (delete_exercise_entry). Apply the user's chosen action on their next \
reply. If LEAKED OPEN ENTRIES is empty, behave normally.\n\
\n\
GUIDELINES:\n\
- When the user reports an exercise, include a log_exercise action\n\
- When the user clearly indicates they want to start a workout (e.g. \"starting my \
workout\", \"open a session\", \"open a new session\", \"let's begin\", \"I'm at the \
gym\", \"start a workout\"), you MUST emit start_session IMMEDIATELY in the same \
response, even if no exercise has been mentioned yet. The correct response is:\n\
  {{\"message\": \"<one short acknowledgement>\", \"actions\": [{{\"type\": \
\"start_session\"}}]}}\n\
  Do NOT ask \"what exercise would you like to log first?\" — the user can send the \
exercise in a separate message.\n\
- SESSION-CONTINUITY ANSWER: When the previous assistant turn was a host-issued \
question of the form \"Before I log \\\"<EXERCISE TEXT>\\\", is this a new workout \
or the same session?\" and the user replies \"new workout\" / \"yes\" / \"new \
session\", you MUST emit, in this exact order: end_session, start_session, then \
the log_exercise action(s) parsed from <EXERCISE TEXT>. Do NOT skip the log — the \
user's affirmation applies BOTH to starting fresh AND to logging the pending set. \
When the user replies \"same workout\" / \"same session\" / \"continuing\", emit \
ONLY the log_exercise action(s) parsed from <EXERCISE TEXT> — no session changes.\n\
- When the user clearly indicates they are done (e.g. \"I'm done\", \"end the workout\", \
\"that's it\", \"end this session\"), you MUST emit end_session in the same response. \
The correct response is:\n\
  {{\"message\": \"<one short acknowledgement>\", \"actions\": [{{\"type\": \
\"end_session\"}}]}}\n\
- SESSION VERDICT: after you emit end_session the host appends its own question \
proposing an overall-effort verdict for the session. When the user's NEXT message \
answers it — agreement or correction, possibly with how it felt or a cut-short \
reason — emit record_session_outcome carrying only what they said.\n\
- Auto-start a session (start_session action) before logging if no session is active\n\
- When the user asks about their most recent performance of an exercise (e.g. \
\"what was my last bench press?\", \"when did I last squat?\", \"how heavy did I \
go on deadlift?\"), emit a get_last_exercise action with the exercise name. Do \
NOT make up numbers from RECENT HISTORY — the host fetches the authoritative \
entry and appends the summary to your reply.\n\
- If the user mentions pain, injury, or illness, log it with log_health\n\
- If the user reports a body measurement (weight, body fat, waist, resting heart \
rate), log it with log_body_metric\n\
- GUIDED SESSION: When a SESSION ROSTER section is present, you are coaching the user \
through a pre-designed session like a personal trainer. \
After you log or confirm the set the user just reported, PROACTIVELY tell them the \
NEXT prescribed set — name, target weight and reps — with a short motivating reason \
drawn from their history (e.g. \"last time 55kg felt easy, so let's go 60kg\"). State \
the prescription; do not ask \"what next?\". If the user reports pain, log_health and \
offer a lighter or substitute movement that hits the same muscles within their \
equipment. The user still reports what they ACTUALLY did — adjust like a real \
trainer if it differs from the prescription.\n\
- OVERRIDES vs DURABLE PREFERENCES: When the user wants to change what they train, \
decide the SCOPE before recording anything. A TODAY-ONLY change to the workout in \
flight (\"I don't feel like bench today, let's do flys\", \"skip legs this session\") \
is a one-off: emit set_session_override and coach the swap for the rest of this \
session ONLY. A durable, FROM-NOW-ON change (\"I always prefer goblet squats\", \
\"stop giving me barbell bench\") is a lasting preference: emit append_philosophy_note. \
Never write a one-off into the philosophy — that would silently ban the movement \
forever. When the scope is genuinely unclear (e.g. \"I hate bench press\" could mean \
either), do NOT guess and do NOT emit either action: ask whether they mean just for \
today or from now on, then act on their answer.\n\
- Keep responses concise -- this is a chat interface\n\
- Be encouraging but not patronizing\n\
- All action fields use metric units (weight_kg, distance_m). If the user specifies \
imperial, convert to metric in the action and mention the conversion in your message\n\
- When you summarize logged exercises or report the current workout \
status in your message, put each exercise entry on its own line. \
Format each line as the exercise name followed by its sets in \
parentheses, e.g. \"Bench Press (3 sets: 32kg x 10, 40kg x 8, 50kg x \
8)\". Use a real newline between entries; never run multiple exercises \
together on one line.\n\
\n\
COLLECTING DATA BEFORE LOGGING:\n\
This rule applies ONLY to data-collection actions (log_exercise, log_exercise_timed, \
log_exercise_distance, log_health, log_body_metric, set_goal). Navigation actions (start_session, \
end_session, close_exercise_entry, confirm_close_exercise_entry, \
close_all_open_entries, delete_exercise_entry, edit_set, get_last_exercise, \
record_session_outcome) MUST be emitted as soon as the user's intent is clear, even \
with no other data.\n\
\n\
Do NOT emit any log_exercise action until you have ALL required data. Respond with \
\"actions\": [] while gathering info. Collect data across multiple messages using \
conversation history to build up the complete picture.\n\
\n\
For weight_reps exercises, you need: exercise name, reps, weight, and difficulty.\n\
\n\
1. ONE ACTION PER SET: Each log_exercise action records exactly ONE set. If the user \
reports a single set, emit one log_exercise action. If the user reports multiple sets \
in one message — whether they share values (e.g. \"3 sets bench 80kg 8 reps, felt \
hard\") or vary per set (e.g. \"drop set: 12 reps at 50kg easy, 10 reps at 50kg \
medium, 8 reps at 50kg hard\", or \"8x60 easy, 6x70 medium, 4x80 hard\") — emit ONE \
log_exercise action per set in the same actions array, each carrying that set's own \
reps/weight/difficulty. Do NOT collapse the per-set details into a single action and \
do NOT spread them across follow-up turns. Never include a \"sets\" field — it does \
not exist in the schema.\n\
2. DIFFICULTY: Once you have reps and weight for a set, the user must indicate \
how it felt. Map natural-language phrasings to the four enum values:\n\
   - easy: \"easy\", \"felt easy\", \"light\", \"smooth\".\n\
   - medium: \"medium\", \"moderate\", \"manageable\", \"ok\".\n\
   - hard: \"hard\", \"tough\", \"heavy\", \"felt hard\".\n\
   - failure: \"failure\", \"to failure\", \"taken to failure\", \"went to failure\", \
\"could not lift\", \"hit failure\", \"AMRAP\", \"max effort\".\n\
   Pick the closest match — do not skip the action just because the user phrased \
it loosely. If the user gave none of these signals, ask, do not guess.\n\
3. FINAL LOG: Only when you have exercise name, reps, weight, AND difficulty for a set, \
emit the log_exercise action.\n\
\n\
If the user reports everything in one message (e.g. \"bench 80kg 8 reps, felt hard\"), \
emit the action immediately -- no need to ask follow-ups.\n\
\n\
When the user answers a follow-up (e.g. \"done\", \"easy\", \"one more\"), use conversation \
history to reconstruct the context. Do not ask for information already provided.\n\
\n\
You may log partial data only when the user explicitly says to skip a field.\n\
\n\
GOALS: The same collect-before-emitting rule applies to set_goal. A goal is denominated \
EITHER by an exercise (e.g. \"hit 100kg on bench\" -> exercise + target_value, direction \
increase) OR by a metric for goals not about one movement (\"lose 5kg\" -> metric \
bodyweight_kg + direction decrease; \"train 4x a week\" -> metric sessions_per_week). Pick \
`kind` accordingly and set `direction` to decrease whenever a smaller number is the win \
(weightloss, a faster time). Ask by when they want to achieve it before emitting; if they \
say there is no deadline, emit with no target_date. Do not guess dates.\n\
\n\
EDITING A LOGGED SET: When the user corrects a set they already logged (\"change my \
last set to 40kg\", \"that was barbell flies not bench press\", \"the last exercise \
should be 8 reps\"), emit an edit_set action carrying ONLY the fields that change. Do \
NOT re-collect difficulty or other data. Changing the exercise (`new_exercise`) \
re-labels the whole block of sets; changing `new_value`/`new_reps`/`new_difficulty` \
affects the single most recent set. If the user wants to change the exercise to one \
that is measured differently (e.g. a timed exercise), ask for the new value first.\n\
\n\
{setup_section}\
CURRENT STATE:\n\
User: {user_name}\n\
Time: {current_time} ({timezone})\n\
Active session: {session_status}\n\
\n\
{entries_section}\
{leaked_section}\
{roster_section}\
{continuity_section}\
{health_section}\n\
{history_section}\n\
{goals_section}\n\
AVAILABLE EXERCISES:\n\
{exercise_list}",
        user_name = ctx.user_name,
        current_time = ctx.current_time,
        timezone = ctx.timezone,
    )
}

/// System prompt for the multi-turn `/philosophy` interview. Unlike the main
/// prompt it advertises ONLY the `save_philosophy` action — the interview never
/// logs sets or touches a session. The assistant interviews the user about their
/// training, then distils everything into a compact, information-dense philosophy
/// (the prompt later fed to the session designer).
pub fn build_philosophy_prompt(draft: &str, health_entries: &[HealthEntry], turns: i32) -> String {
    let health_section = format_health_entries(health_entries);
    let draft_section = if draft.trim().is_empty() {
        "PHILOSOPHY SO FAR: (nothing distilled yet)\n".to_string()
    } else {
        format!("PHILOSOPHY SO FAR:\n{draft}\n")
    };
    // Nudge the model to converge once the interview has run a few turns so a
    // small model does not loop indefinitely without ever emitting the action.
    let wrap_up = if turns >= 4 {
        "WRAP UP: You have gathered several turns of answers. Unless the user clearly has more to add, \
confirm the key points in your message and emit save_philosophy NOW.\n\n"
    } else {
        ""
    };

    format!(
        "You are a personal gym trainer building a training PHILOSOPHY together with the user. \
This is a focused interview, not a workout — you NEVER log exercises or start sessions here.\n\
\n\
GOAL: Through a short, natural back-and-forth, learn enough to distil a compact training \
philosophy. Cover these four areas (ask about whatever is still missing, one or two questions \
at a time — do not interrogate):\n\
1. How often they want to train (sessions per week; any other sports/activities).\n\
2. The main thrust of their training (hypertrophy, strength, cardio, fitness, flexibility, core, ...).\n\
3. Preferred programmes or styles (e.g. 5x5, push/pull/legs, high-rep, circuits) — optional.\n\
4. Equipment available, WITH limits (e.g. \"squat rack up to 120kg, bench, dumbbells up to 24kg, \
kettlebells\"). Capture this verbatim as free text — it constrains future workouts.\n\
\n\
{wrap_up}\
{draft_section}\
{health_section}\n\
SCOPE: Stay strictly on training philosophy. If the user drifts off-topic, gently steer back.\n\
\n\
RESPONSE FORMAT: You MUST respond with ONLY a JSON object. No text before or after.\n\
{{\n\
  \"message\": \"Your next question, or a confirmation when you save\",\n\
  \"actions\": []\n\
}}\n\
\n\
THE ONLY ACTION available to you is:\n\
- {{\"type\": \"save_philosophy\", \"content\": \"<the distilled philosophy>\"}}\n\
\n\
WHEN TO SAVE: Emit save_philosophy ONLY once you have a clear picture of the four areas above \
(equipment is required; programmes are optional). When you do, set `content` to a SINGLE compact, \
information-dense paragraph — not a transcript — capturing goal, weekly frequency, preferred \
programmes, equipment with limits, and any relevant injuries or preferences. Example content:\n\
  \"goal=hypertrophy. Likes 5x5. Home gym: squat rack up to 120kg, bench, kettlebells, dumbbells \
up to 24kg. Weights 3x/week, racket sports 2x/week. Minor lower-back niggle — cautious on heavy spinal load.\"\n\
In the SAME response, your `message` should briefly confirm what you saved. Until you are ready \
to save, keep `actions` empty and ask the next question.",
    )
}

/// System prompt for the multi-turn `/programme` interview ([C4.2]): agree a multi-week
/// programme with the user and emit it as one `propose_programme` action.
///
/// Unlike `/philosophy` this is not a blank-slate interview. The philosophy is an
/// INPUT — frequency and equipment are already on file, so the prompt tells the model to
/// confirm rather than re-elicit them, and to spend its questions on what a programme
/// actually adds: how long it runs and which goals it serves. `goals` is rendered by
/// [`format_goals_with_ids`] because `propose_programme` has to name goal ids, and
/// `history` is the same condensed block the session designer reads.
///
/// Advertises ONLY `propose_programme`: the interview persists a draft and never logs,
/// starts a session, or activates anything. `turns` drives the same convergence nudge as
/// the philosophy interview, so a small model cannot loop forever without proposing.
pub fn build_programme_prompt(
    philosophy: &str,
    goals: &[GoalProgress],
    history: &str,
    health_entries: &[HealthEntry],
    turns: i32,
) -> String {
    let goals_section = format_goals_with_ids(goals);
    let health_section = format_health_entries(health_entries);
    let wrap_up = if turns >= PROGRAMME_WRAP_UP_TURNS {
        "WRAP UP: You have gathered several turns of answers. Unless the user clearly has more to add, \
confirm the shape in your message and emit propose_programme NOW.\n\n"
    } else {
        ""
    };

    format!(
        "You are a personal gym trainer designing a multi-week training PROGRAMME with the user. \
A programme is a SKELETON, NOT A SCRIPT: it fixes how long the block of training runs, how many \
days a week it trains, how the week is split, how load progresses, and which goals it serves. It \
contains NO exercises — each session is still designed on demand against it later. You NEVER log \
exercises or start sessions here.\n\
\n\
THE PHILOSOPHY BELOW IS AN INPUT, NOT A QUESTION. Weekly frequency and available equipment are \
already on file: confirm them back to the user in one line ({confirm_hint}) and let them correct \
you. Do NOT re-interview the user about them.\n\
\n\
ASK ONLY WHAT A PROGRAMME ADDS, one or two questions at a time:\n\
1. How long it should run, or what date they are training toward (an event, a trip, a test week).\n\
2. Which of their ACTIVE GOALS this programme is for — a programme that serves no goal is just a \
calendar. Use their answer to fill `goal_ids`.\n\
3. Anything that constrains the calendar: travel, a known deload need, days they cannot train.\n\
\n\
HOW TO DESIGN THE SKELETON:\n\
- Set `days_per_week` from the philosophy's stated frequency unless the user says otherwise.\n\
- Choose a `split` that fits that frequency (e.g. full body at 2-3 days, upper/lower at 4, \
push/pull/legs at 5-6) and state it as free text.\n\
- Divide the weeks into `blocks` (mesocycles) with 1-based, INCLUSIVE, non-overlapping week \
ranges that together cover week 1 to the last week. Build toward the goal rather than repeating: \
accumulate, then intensify, and include a deload block for any programme longer than about 4 weeks.\n\
- Give a `week_template`: exactly `days_per_week` entries, `day_idx` counting 1..days_per_week. \
Each `focus` is a SHORT TEXT INTENT (\"upper push\", \"lower\", \"full body\") — NEVER a list of \
exercises, and never a weekday name. This template repeats every week; the blocks are what make \
week 9 harder than week 1.\n\
- State a `progression_policy` in free text concrete enough to act on (e.g. \"add 2.5kg to upper \
compounds when all prescribed reps are hit at the top of the range; hold on a deload week\").\n\
- Respect ACTIVE HEALTH ISSUES: they are a hard constraint on what a block may focus on.\n\
\n\
{wrap_up}\
{philosophy_section}\n\
{goals_section}\n\
{history}\n\
{health_section}\n\
SCOPE: Stay on the programme. If the user drifts, gently steer back.\n\
\n\
RESPONSE FORMAT: You MUST respond with ONLY a JSON object. No text before or after.\n\
{{\n\
  \"message\": \"Your next question, or a short introduction when you propose\",\n\
  \"actions\": []\n\
}}\n\
\n\
THE ONLY ACTION available to you is:\n\
- {{\"type\": \"propose_programme\", \"title\": \"<short programme title>\", \"weeks\": N, \
\"days_per_week\": N, \"split\": \"<free text>\", \"progression_policy\": \"<free text>\", \
\"blocks\": [{{\"start_week\": 1, \"end_week\": 4, \"focus\": \"<intent>\"}}, ...], \
\"week_template\": [{{\"day_idx\": 1, \"focus\": \"<intent>\"}}, ...], \"goal_ids\": [N, ...]}}\n\
\n\
WHEN TO PROPOSE: emit propose_programme ONLY once you know the length and which goals it serves. \
Emit it EXACTLY ONCE, with `goal_ids` drawn from the id values in ACTIVE GOALS above — never \
invent an id, and never send a goal name in that field. Until you are ready, keep `actions` empty \
and ask the next question. The user gets the last word: after you propose, they confirm it before \
anything becomes active.",
        philosophy_section = format_philosophy_section(philosophy),
        confirm_hint = "\"That's 3 days a week with dumbbells to 24kg — still right?\"",
    )
}

/// After this many turns the `/programme` interview is told to stop asking and propose.
/// Matches the philosophy interview's guard, and for the same reason: a small model will
/// otherwise keep interviewing without ever emitting the action.
const PROGRAMME_WRAP_UP_TURNS: i32 = 4;

/// Everything [`build_designer_prompt`] reads. A struct rather than a parameter list because the
/// designer draws on eight distinct inputs, several of them string blocks, and a positional call of
/// that length is one transposition away from a prompt that is wrong in a way no type catches.
pub struct DesignerInputs<'a> {
    /// The user's distilled training philosophy, or an empty string when they have none yet.
    pub philosophy: &'a str,
    /// The pre-formatted RECENT HISTORY block (the caller holds the catalogue to resolve names).
    pub history: &'a str,
    /// The pre-formatted MUSCLE RECOVERY block.
    pub recovery: &'a str,
    pub goals: &'a [GoalProgress],
    pub health_entries: &'a [HealthEntry],
    /// What retrieval returned for this design ([C5.2]).
    pub science: &'a [ScienceChunk],
    /// The computed progressive-overload policy ([C5.3]).
    pub progression: &'a ProgressionPolicy,
    pub catalogue: &'a [ExerciseTypeWithAncestry],
}

/// System prompt for `/nextworkout`: design ONE tailored training session from the
/// user's philosophy, recent history, goals, injuries, the curated training science
/// retrieved for them, and the computed progression policy. It advertises ONLY the
/// `propose_session_roster` action — the design is a proposal and logs nothing.
/// Exercise selection follows the spec's decreasing priority order (goal contribution,
/// done before, longest-rested muscles, philosophy fit, health issues, temporary
/// requests); lower rungs break ties rather than veto, except active health issues,
/// which stay a hard constraint.
///
/// `science` ([C5.2]) narrows the *prescription* — rep ranges, intensity, rest, volume —
/// while the model still chooses the exercises, which is where the adaptivity lives.
/// `progression` ([C5.3]) fixes the *loads*, per exercise, from what the user actually
/// logged; the prompt carries it as a binding instruction rather than leaving the model
/// to infer progression from the history block.
///
/// Both degrade gracefully: an empty `science` drops back to the pre-[C5.2] prompt and
/// an empty `progression` to a conservative starting rule, rather than asserting bands
/// or loads the prompt does not actually carry.
pub fn build_designer_prompt(input: &DesignerInputs<'_>) -> String {
    let DesignerInputs { philosophy, history, recovery, goals, health_entries, science, progression, catalogue } = *input;
    let goals_section = format_active_goals(goals);
    let competing_section = format_competing_goals(goals);
    let science_section = format_training_science(science);
    let progression_section = format_progression_policy(progression);
    let health_section = format_health_entries(health_entries);
    let contraindications_section = format_contraindications(health_entries);
    let exercise_list = format_exercise_list(catalogue);

    format!(
        "You are a personal gym trainer DESIGNING one training session for the user right now. \
Produce a highly tailored, specific session that pushes the user toward their goals, from the \
TRAINING SCIENCE and the user-specific information below. You are only designing a session roster \
— you do NOT log any sets and do NOT start a session.\n\
\n\
{science_rule}\
\n\
SELECTION PRIORITY — rank candidate exercises by these criteria, in DECREASING priority:\n\
1. Contribution to the user's ACTIVE GOALS.\n\
2. Exercises the user has done before (they appear in RECENT HISTORY).\n\
3. Muscle groups with the longest rest period (MUSCLE RECOVERY lists them longest-rested first; \
treat a group shown as never trained, or not trained in a long time, as a strong candidate).\n\
4. Fit with the TRAINING PHILOSOPHY: its goal, preferred programmes/rotation, weekly frequency.\n\
5. Working around ACTIVE HEALTH ISSUES.\n\
6. Temporary, single-session requests in the user's message (\"something lighter today\").\n\
This is a decreasing priority order, not a filter: a lower item never vetoes a higher one — it \
only breaks ties between options the higher items rank equally. EXCEPTION: ACTIVE HEALTH ISSUES \
are a HARD constraint, not a tie-breaker — never prescribe a movement that loads an injured area; \
substitute away. Where CONTRAINDICATIONS below lists movements, that list is checked \
deterministically after you answer: a roster containing one is rejected outright and the user gets \
no session, so substitute rather than explain.\n\
\n\
{contraindications_section}\
HOW TO DESIGN (reason like a real trainer):\n\
- Honour the EQUIPMENT in the philosophy and its weight limits — never prescribe a weight the \
user cannot load, or equipment they do not have.\n\
{progression_rule}\
- Pick 3-6 exercises. For each, prescribe target sets and target reps (or seconds for timed work) \
and a target weight within the user's equipment limits. Add a short per-exercise cue when useful.\n\
\n\
{philosophy_section}\n\
{history}\n\
{progression_section}\
{recovery}\n\
{goals_section}\n\
{competing_section}\
{health_section}\n\
{science_section}\
RESPONSE FORMAT: You MUST respond with ONLY a JSON object. No text before or after.\n\
{{\n\
  \"message\": \"<one or two sentences introducing the session>\",\n\
  \"actions\": [{{\"type\": \"propose_session_roster\", ...}}]\n\
}}\n\
\n\
You MUST emit EXACTLY ONE action, of type propose_session_roster:\n\
- {{\"type\": \"propose_session_roster\", \"title\": \"<short session title>\", \
\"rationale\": \"<2-4 sentences that MUST name which SELECTION PRIORITY items drove today's \
picks (e.g. goal contribution, longest-rested muscles), plus any injury substitutions and why, \
plus the [S:doc-id] marker of every TRAINING SCIENCE item that shaped the prescription>\", \
\"exercises\": [{{\"exercise\": \"<EXACT NAME>\", \"target_sets\": N, \"target_reps\": N, \
\"target_weight_kg\": N.N, \"target_secs\": N, \"notes\": \"<short cue, optional>\"}}, ...]}}\n\
  Use `target_secs` instead of reps/weight for timed exercises. Omit fields that do not apply.\n\
\n\
EXERCISE NAME RULE: every `exercise` MUST be a name shown in double quotes in AVAILABLE EXERCISES \
below, copied EXACTLY. Do not invent or abbreviate. If you want a movement that is not listed, \
choose the closest available exercise and note the substitution in the rationale.\n\
\n\
AVAILABLE EXERCISES:\n\
{exercise_list}",
        philosophy_section = format_philosophy_section(philosophy),
        science_rule = science_rule(science),
        progression_rule = progression_rule(progression),
    )
}

/// The HOW TO DESIGN bullet governing load selection ([C5.3]).
///
/// With a computed policy this points at it and says it is binding; with none — a user with no
/// logged history at all — it falls back to the conservative starting rule. It deliberately does
/// *not* fall back to the old "if the last sets were easy, progress the load" heuristic: that
/// sentence was the entire progression model this ticket exists to replace, and leaving it in place
/// for the empty case would leave a second, weaker policy in the prompt to contradict the first.
fn progression_rule(progression: &ProgressionPolicy) -> String {
    if progression.is_empty() {
        return "- The user has no logged history to progress from. Prescribe conservative loads they can \
finish with 2-3 reps in reserve, and treat this session as calibration rather than a test.\n"
            .to_string();
    }
    "- Set every load from the PROGRESSION POLICY section below. It is COMPUTED from what the user \
actually logged — performance against prescription, and effort per set — and it is BINDING, not \
advisory. Do not add load to an exercise it marks HOLD, BACK OFF or DELOAD, and do not exceed the \
load it states for one marked PROGRESS. Use your own judgement only for exercises it does not list. \
Avoid repeating yesterday's heavy work.\n"
        .to_string()
}

/// The PROGRESSION POLICY section: the per-exercise directives [`crate::progression`] computed,
/// plus the reading of the effort scale they were derived under.
///
/// Rendered as instructions rather than as data, because the model's job here is to apply a
/// decision that has already been made, not to re-derive it. The reasons travel with the directives
/// so the rationale the user reads can explain *why* the load moved, which is the difference
/// between a coach and a number generator.
fn format_progression_policy(progression: &ProgressionPolicy) -> String {
    if progression.is_empty() {
        return String::new();
    }

    let header = match &progression.block {
        BlockIntent::Deload { focus } => format!(
            "PROGRESSION POLICY — DELOAD WEEK (programme block: \"{focus}\"):\n\
This week's block calls a deload, which OVERRIDES ordinary progression. Hold the loads below and \
cut the working sets by about a third. Adding load during a deload week misreads the week's \
purpose. Say in the rationale that this is a deload and what it is for.\n"
        ),
        BlockIntent::Ordinary => "PROGRESSION POLICY (computed from logged performance and per-set effort; \
implements [S:progressive-overload]):\n"
            .to_string(),
    };

    let effort_scale = format!(
        "Effort is logged on a four-point scale; read it as repetitions in reserve — easy = {}, medium = {}, \
hard = {}, failure = {}.\n",
        reps_in_reserve(Difficulty::Easy),
        reps_in_reserve(Difficulty::Medium),
        reps_in_reserve(Difficulty::Hard),
        reps_in_reserve(Difficulty::Failure),
    );

    let lines: String = progression.directives.iter().map(|d| format!("{}\n", format_directive(d))).collect();
    let advice = match &progression.deload_advice {
        Some(text) => format!("\nACCUMULATED FATIGUE: {text}\n"),
        None => String::new(),
    };
    let unlisted = "\nAn exercise not listed above has no logged history to progress from: prescribe a load the \
user can finish with 2-3 reps in reserve and treat it as calibration.\n";

    format!("{header}{effort_scale}\n{lines}{advice}{unlisted}\n")
}

/// One directive as the prompt states it: verb, exercise, the loads, and the reasoning.
fn format_directive(d: &ProgressionDirective) -> String {
    let show = |v: f64| d.measurement_type.describe_value(v);
    let movement = match &d.action {
        ProgressionAction::Progress { from, to } | ProgressionAction::BackOff { from, to } => {
            format!("{} -> {}", show(*from), show(*to))
        }
        ProgressionAction::Hold { at } | ProgressionAction::Deload { at } => format!("at {}", show(*at)),
    };
    format!("- {}: {} {movement} ({}; {})", d.exercise_name, d.action.verb(), d.class.label(), d.reason)
}

/// The paragraph that tells the designer what to do with the TRAINING SCIENCE section. Absent when
/// retrieval came back empty, because a rule pointing at a section that is not there is worse than
/// no rule — the model invents the section.
fn science_rule(science: &[ScienceChunk]) -> String {
    if science.is_empty() {
        return String::new();
    }
    "TRAINING SCIENCE IS THE AUTHORITY ON PRESCRIPTION. The TRAINING SCIENCE section below is \
curated, human-reviewed exercise science. Its repetition ranges, intensities, rest intervals and \
volumes are CONSTRAINTS your prescription MUST fall inside — prefer them over your own recall \
wherever the two differ, and do not substitute a remembered protocol for a stated band. It does \
NOT tell you WHICH exercises to choose: that is your job, from the user-specific information \
below. Where it gives a range, land inside the range; where it says a beginner differs, use the \
beginner figure if the philosophy or history says the user is one. Cite the [S:doc-id] marker of \
whatever you applied.\n"
        .to_string()
}

/// Rough token estimate for budgeting a prompt block: ~4 chars per token. Only needs to be
/// monotonic and in the right ballpark, not exact.
pub(crate) fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// Upper bound (estimated tokens) on the whole TRAINING SCIENCE block, bounding it the way the
/// history block is bounded. Sized for roughly four chunks of curated prose — a corpus section runs
/// 150-300 words.
const SCIENCE_TOKEN_BUDGET: usize = 1200;

/// The TRAINING SCIENCE section: the retrieved chunks, each under its citation marker, bounded by
/// [`SCIENCE_TOKEN_BUDGET`].
///
/// Chunks arrive best-first with any pinned rails at the head ([`crate::science::ScienceQuery`]),
/// so truncation drops the least important science, never a rail. A dropped chunk is announced
/// rather than silently omitted: a prompt that quietly shrinks under load is a prompt whose
/// behaviour cannot be reasoned about from its inputs.
fn format_training_science(science: &[ScienceChunk]) -> String {
    if science.is_empty() {
        return String::new();
    }

    let header = "TRAINING SCIENCE (curated, human-reviewed; cite as [S:doc-id]):\n";
    let rendered = science.iter().map(|c| format!("{} {}\n{}\n", c.citation, c.heading, c.text));

    // The first block always survives: a TRAINING SCIENCE section holding nothing but a truncation
    // note is strictly worse than the pre-[C5.2] prompt, which at least did not claim to be grounded.
    let mut used = estimate_tokens(header);
    let kept: Vec<String> = rendered
        .enumerate()
        .take_while(|(idx, block)| {
            used += estimate_tokens(block);
            *idx == 0 || used <= SCIENCE_TOKEN_BUDGET
        })
        .map(|(_, block)| block)
        .collect();

    let dropped = science.len() - kept.len();
    let note = match dropped {
        0 => String::new(),
        n => format!("({n} further science excerpt(s) omitted to fit the prompt budget.)\n"),
    };
    format!("{header}\n{}{note}\n", kept.join("\n"))
}

/// The COMPETING GOALS block, rendered only when the user holds goals of more than one kind — one
/// kind cannot compete with itself, and a resolution rule for a non-conflict is noise.
///
/// The block states the priority ordering and the one rule the ticket turns on (resolve by
/// priority, never by averaging); *how* particular pairs combine stays in the corpus, where it can
/// be reviewed as science rather than edited as prompt text. The caller pins `competing-goals` into
/// retrieval whenever this block renders, so the detail is always present to be applied.
fn format_competing_goals(goals: &[GoalProgress]) -> String {
    let ranked = goals_by_priority(goals);
    let Some(first) = ranked.first() else { return String::new() };
    if ranked.iter().all(|gp| gp.goal.kind == first.goal.kind) {
        return String::new();
    }

    let lines = ranked.iter().enumerate().map(|(idx, gp)| {
        format!("{}. {} — {} (priority {})\n", idx + 1, gp.goal.kind.as_str(), gp.exercise_name, gp.goal.priority)
    });
    format!(
        "COMPETING GOALS (highest priority first):\n{}\
These goals pull in different directions. Resolve them by the PRIORITY ORDER ABOVE, applying the \
curated resolution in TRAINING SCIENCE ([S:competing-goals]) — do NOT average them into a middle \
that serves neither. The highest-priority goal governs this session's prescription; the rest are \
served by what is left. Say in the rationale which goal this session prioritises and what that \
costs the other.\n\n",
        lines.collect::<String>()
    )
}

/// Active goals highest-priority first ([C3.1]). Ties keep the caller's order, which
/// `goal_progress_report` returns deterministically, so the prompt is stable run to run.
pub fn goals_by_priority(goals: &[GoalProgress]) -> Vec<&GoalProgress> {
    let mut ranked: Vec<&GoalProgress> = goals.iter().collect();
    ranked.sort_by_key(|gp| std::cmp::Reverse(gp.goal.priority));
    ranked
}

/// The MUSCLE RECOVERY section for the designer prompt: every muscle group, ordered
/// longest-rested first, so a group not trained in a while — or never — reads as the
/// obvious candidate for today. `today` is passed in rather than read from the clock,
/// keeping the rendering deterministic and testable.
pub fn format_muscle_recovery(recovery: &[MuscleRecovery], today: NaiveDate) -> String {
    if recovery.is_empty() {
        return "MUSCLE RECOVERY: no muscle groups on record.\n".to_string();
    }

    // Whole days since a group was last trained; `None` (never trained) is the
    // strongest signal and sorts ahead of any finite rest.
    let days_since = |r: &MuscleRecovery| -> Option<i64> {
        let last = r.last_trained.as_deref()?;
        let date = NaiveDate::parse_from_str(last, "%Y-%m-%d").ok()?;
        Some((today - date).num_days())
    };

    let mut ranked: Vec<(&MuscleRecovery, Option<i64>)> = recovery.iter().map(|r| (r, days_since(r))).collect();
    ranked.sort_by(|a, b| match (a.1, b.1) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => y.cmp(&x),
    });

    let lines = ranked.into_iter().map(|(r, days)| match days {
        None => format!("- {}: never trained", r.muscle_group),
        Some(d) if d <= 0 => format!("- {}: last trained today ({} sets)", r.muscle_group, r.last_volume_sets),
        Some(1) => format!("- {}: last trained 1d ago ({} sets)", r.muscle_group, r.last_volume_sets),
        Some(d) => format!("- {}: last trained {d}d ago ({} sets)", r.muscle_group, r.last_volume_sets),
    });
    format!("MUSCLE RECOVERY (longest-rested first):\n{}\n", lines.collect::<Vec<_>>().join("\n"))
}

/// The philosophy block for the designer prompt, with a clear placeholder when the
/// user has none on file yet.
fn format_philosophy_section(philosophy: &str) -> String {
    if philosophy.trim().is_empty() {
        "TRAINING PHILOSOPHY: (none on file — design a balanced, general-fitness session and keep \
loads conservative)\n"
            .to_string()
    } else {
        format!("TRAINING PHILOSOPHY:\n{philosophy}\n")
    }
}

/// The SETUP backstop: one nudge toward whatever the user is missing, in the order
/// each thing becomes useful — a philosophy shapes every design, goals point the
/// designs somewhere, and a programme joins them up.
///
/// A backstop rather than the mechanism: the welcome asks outright, and this only
/// catches the user who declined or never answered. Resolved host-side from stored
/// state, so it says nothing at all once all three exist — the model is never left
/// to guess whether someone is set up.
fn format_setup(has_philosophy: bool, has_goals: bool, has_programme: bool) -> String {
    let nudge = match (has_philosophy, has_goals, has_programme) {
        (false, _, _) => {
            "The user has NO training philosophy on file. Somewhere in your reply, invite them to \
run /philosophy — a short interview about how they train, which is what lets designed sessions \
fit them. Mention it at most once per conversation, and never withhold logging over it."
        }
        (true, false, _) => {
            "The user has a philosophy but NO goals. Invite them to name one target — a lift \
number, a bodyweight figure, or a weekly training habit — and emit set_goal once they state it. \
Mention it at most once per conversation."
        }
        (true, true, false) => {
            "The user has a philosophy and goals but NO programme. Mention once, briefly, that \
/programme builds a multi-week programme their sessions then build on. Do not push it."
        }
        (true, true, true) => return String::new(),
    };
    format!("SETUP:\n{nudge}\n\n")
}

/// Lower bound of the "ask before logging" window. Below this, treat the message
/// as a continuation of the in-progress workout and log normally.
pub const SESSION_CONTINUITY_ASK_HOURS: f64 = 0.5;

/// Banner emitted at the very top of the system prompt — *before* the assistant's
/// role description — whenever a session-continuity directive is in effect.
/// Duplicates the directive text from `format_session_continuity` so the LLM sees
/// the override both when it first reads the prompt and when it reaches the
/// CURRENT STATE block.
fn format_session_continuity_banner(age_hours: Option<f64>) -> String {
    let Some(h) = age_hours else { return String::new() };
    if h >= SESSION_CONTINUITY_HOURS {
        format!(
            "PRIORITY OVERRIDE — SESSION CONTINUITY (gap = {h:.2}h, cutoff = \
{SESSION_CONTINUITY_HOURS:.0}h):\nThis turn's actions array MUST be exactly \
[end_session, start_session, ...the user's log_exercise(s)] in that order. The \
previous session is too stale to log against. Mention briefly in `message` that \
you've started a fresh workout. Do not ask for confirmation.\n\n"
        )
    } else if h >= SESSION_CONTINUITY_ASK_HOURS {
        format!(
            "PRIORITY OVERRIDE — SESSION CONTINUITY (gap = {h:.2}h, cutoff = \
{SESSION_CONTINUITY_HOURS:.0}h):\nThis turn you MUST reply with EXACTLY:\n\
{{\"message\": \"It's been a while since your last set — is this a new workout, \
or are we picking up where we left off?\", \"actions\": []}}\n\
No log_exercise. No other actions. No other message text. Wait for the user's \
next message before deciding what to do with the exercise they mentioned. This \
overrides the GUIDELINES rule about logging exercises.\n\n"
        )
    } else {
        String::new()
    }
}

fn format_session_continuity(age_hours: Option<f64>) -> String {
    // The host short-circuits the 0.5–12h ask-window before the LLM is called,
    // so the only branch the LLM still needs to react to is ≥12h, which is
    // surfaced by `format_session_continuity_banner` at the very top of the
    // prompt. This in-state line is informational only.
    match age_hours {
        Some(h) => format!("TIME SINCE LAST ACTIVITY: {h:.2} hours\n\n"),
        None => String::new(),
    }
}

fn format_session_entries(entries: &[PromptEntry]) -> String {
    if entries.is_empty() {
        return "EXERCISE ENTRIES (this session): None\n".to_string();
    }
    let mut s = "EXERCISE ENTRIES (this session):\n".to_string();
    for e in entries {
        let status = if e.is_open { "open" } else { "closed" };
        s.push_str(&format!(
            "- [id={}, {status}] {} ({} {}): {}\n",
            e.id,
            e.exercise_name,
            e.set_count,
            if e.set_count == 1 { "set" } else { "sets" },
            e.sets_summary,
        ));
    }
    s
}

fn format_leaked_entries(entries: &[PromptEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut s = "LEAKED OPEN ENTRIES (must be resolved before starting a new session):\n".to_string();
    for e in entries {
        s.push_str(&format!("- [id={}] {} ({} sets)\n", e.id, e.exercise_name, e.set_count));
    }
    s.push('\n');
    s
}

/// Render the `/nextworkout` design for the system prompt: a "ready to start" hint
/// before the session begins, or live progress with the NEXT prescribed set once it
/// is under way.
fn format_active_roster(roster: &Option<RosterProgress>) -> String {
    let Some(roster) = roster else {
        return String::new();
    };

    let mut s = String::new();
    if roster.started {
        s.push_str(&format!("SESSION ROSTER (guided, in progress): {}\n", roster.title));
        if !roster.done.is_empty() {
            s.push_str(&format!("- done so far: {}\n", roster.done.join(", ")));
        }
        match &roster.next {
            Some(next) => s.push_str(&format!("- NEXT SET: {} ({} to go)\n", format_prescription(next), roster.remaining)),
            None => s.push_str("- all prescribed exercises done — congratulate the user and offer to end the session\n"),
        }
    } else {
        s.push_str(&format!("SESSION ROSTER READY: {}\n", roster.title));
        if let Some(next) = &roster.next {
            s.push_str(&format!("- first up when they start: {}\n", format_prescription(next)));
        }
        s.push_str("- when the user starts their session, walk them through it set by set\n");
    }
    if let Some(note) = roster.override_note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        s.push_str(&format!(
            "- TODAY-ONLY OVERRIDES for this session (honour them now; do NOT save to philosophy):\n{note}\n"
        ));
    }
    s.push('\n');
    s
}

/// A prescribed exercise as one compact line, e.g. "Bench Press — 3 sets 6 reps @ 65kg (push it)".
fn format_prescription(p: &PromptRosterExercise) -> String {
    let mut parts = Vec::new();
    if let Some(sets) = p.target_sets {
        parts.push(format!("{sets} sets"));
    }
    if let Some(secs) = p.target_secs {
        parts.push(format!("{secs}s"));
    } else if let Some(reps) = p.target_reps {
        parts.push(format!("{reps} reps"));
    }
    if let Some(w) = p.target_weight_kg {
        parts.push(format!("@ {w:.0}kg"));
    }
    let mut s = p.exercise_name.clone();
    if !parts.is_empty() {
        s.push_str(&format!(" — {}", parts.join(" ")));
    }
    if let Some(notes) = p.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        s.push_str(&format!(" ({notes})"));
    }
    s
}

fn format_set_entry(set: &ExerciseSet, exercise_name: &str) -> String {
    let mut parts = vec![exercise_name.to_string()];
    if let (MeasurementType::WeightReps, Some(c)) = (set.measurement_type, set.count) {
        parts.push(format!("{c} reps"));
    }
    parts.push(set.measurement_type.describe_value(set.value));
    parts.join(" ")
}

pub fn format_exercise_list(catalogue: &[ExerciseTypeWithAncestry]) -> String {
    let mut result = String::new();
    let mut current_group = "";

    // Only `exercise` and `variation` rows are loggable directly. List those.
    let loggable: Vec<&ExerciseTypeWithAncestry> = catalogue
        .iter()
        .filter(|e| matches!(e.exercise_type.level, crate::db::ExerciseLevel::Exercise | crate::db::ExerciseLevel::Variation))
        .collect();

    let mut sorted = loggable;
    sorted.sort_by(|a, b| {
        a.muscle_group
            .as_deref()
            .unwrap_or("")
            .cmp(b.muscle_group.as_deref().unwrap_or(""))
            .then_with(|| a.exercise_type.name.cmp(&b.exercise_type.name))
    });

    for et in sorted {
        let group = et.muscle_group.as_deref().unwrap_or("Other");
        if group != current_group {
            current_group = group;
            result.push_str(&format!("\n## {current_group}\n"));
        }

        let aliases = et.exercise_type.aliases.as_deref().map(|a| format!(" (aliases: {a})")).unwrap_or_default();

        let mt = et.exercise_type.measurement_type.map(|m| m.as_str()).unwrap_or("weight_reps");
        result.push_str(&format!("- \"{}\"{aliases} [{mt}]\n", et.exercise_type.name));
    }

    result
}

pub fn format_health_entries(entries: &[HealthEntry]) -> String {
    if entries.is_empty() {
        return "ACTIVE HEALTH ISSUES: None\n".to_string();
    }

    let mut result = "ACTIVE HEALTH ISSUES:\n".to_string();
    for entry in entries {
        let body = entry.body_part.as_deref().unwrap_or("general");
        result.push_str(&format!(
            "- {} ({}, {body}): {} (since {})\n",
            entry.entry_type, entry.severity, entry.description, entry.started_at,
        ));
    }
    result
}

/// The contraindication rails for the user's active injuries, rendered for the designer prompt
/// ([C5.4]).
///
/// Built from [`crate::science::contraindications`] — the same table the post-parse rail enforces,
/// so the prompt cannot drift from the check. That is the point of rendering it rather than writing
/// it out: the previous prompt described the lower-back case in prose and covered no other joint,
/// and prose has no way to stay in step with a rule.
///
/// Empty when nothing is contraindicated, so a user with no injuries — or an injury the corpus has
/// no document for — never sees a section listing nothing. The injuries themselves still reach the
/// model through [`format_health_entries`] either way.
pub fn format_contraindications(entries: &[HealthEntry]) -> String {
    let rendered: Vec<String> = crate::science::contraindications::active_injuries(entries)
        .filter_map(|(body_part, severity)| {
            let rails = crate::science::contraindications::rails_for(body_part)?;
            let barred: Vec<&Contraindication> = rails.contraindications.iter().filter(|rule| severity >= rule.bars_from).collect();
            if barred.is_empty() {
                return None;
            }
            let patterns = barred.iter().map(|rule| rule.pattern.as_str()).collect::<Vec<_>>().join("; ");
            let swaps = barred.iter().flat_map(|rule| rule.substitutions.iter().copied()).collect::<Vec<_>>();
            Some(format!(
                "- {} ({severity}) — DO NOT prescribe: {patterns}.\n  Use instead: {}.\n",
                body_part.replace('_', " "),
                dedup_preserving_order(swaps).join(", "),
            ))
        })
        .collect();

    match rendered.is_empty() {
        true => String::new(),
        false => format!(
            "CONTRAINDICATIONS (hard rails for the injuries above; checked after you answer):\n{}\n",
            rendered.concat()
        ),
    }
}

/// The distinct items of `items`, keeping first-appearance order. Two rails for one body part often
/// recommend the same swap, and a list that says "hip thrust, hip thrust" reads as a mistake.
fn dedup_preserving_order(items: Vec<&'static str>) -> Vec<&'static str> {
    items.iter().enumerate().filter(|(idx, item)| !items[..*idx].contains(item)).map(|(_, item)| *item).collect()
}

/// The session-level verdict as one compact phrase, e.g.
/// "overall hard, felt good, cut short (knee pain)". `None` when no outcome has
/// been recorded, so callers can omit the clause entirely.
pub fn format_session_outcome(session: &Session) -> Option<String> {
    let cut_short = session.cut_short.then(|| match session.cut_short_reason.as_deref() {
        Some(reason) => format!("cut short ({reason})"),
        None => "cut short".to_string(),
    });
    let parts: Vec<String> = session
        .overall_effort
        .map(|e| format!("overall {e}"))
        .into_iter()
        .chain(session.felt.map(|f| format!("felt {f}")))
        .chain(cut_short)
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

pub fn format_recent_history(summaries: &[SessionSummary], sets: &[ExerciseSet], catalogue: &[ExerciseTypeWithAncestry]) -> String {
    if summaries.is_empty() && sets.is_empty() {
        return "RECENT HISTORY: No recent workouts\n".to_string();
    }

    let mut result = "RECENT HISTORY:\n".to_string();

    for summary in summaries {
        let duration = summary.duration_mins.map(|d| format!(" ({d} min)")).unwrap_or_default();
        let status = if summary.session.ended_at.is_some() { "completed" } else { "active" };
        let outcome = format_session_outcome(&summary.session).map(|p| format!(" — {p}")).unwrap_or_default();
        result.push_str(&format!("- {} [{status}]: {} entries{duration}{outcome}\n", summary.session.started_at, summary.exercise_count));
    }

    if !sets.is_empty() {
        result.push_str("\nRecent sets:\n");
        for set in sets.iter().take(10) {
            let name = catalogue
                .iter()
                .find(|e| e.exercise_type.id == set.exercise_type_id)
                .map(|e| e.exercise_type.name.as_str())
                .unwrap_or("unknown");
            result.push_str(&format!("- {}: {}\n", set.logged_at, format_set_entry(set, name)));
        }
    }

    result
}

pub fn format_active_goals(goals: &[GoalProgress]) -> String {
    if goals.is_empty() {
        return "ACTIVE GOALS: None\n".to_string();
    }

    let mut result = "ACTIVE GOALS:\n".to_string();
    for gp in goals {
        let current = gp.current_value.map(|v| format!("{v:.1}")).unwrap_or_else(|| "N/A".to_string());
        let by = gp.goal.target_date.as_deref().map(|d| format!(" by {d}")).unwrap_or_default();
        let dir = match gp.goal.direction {
            crate::db::GoalDirection::Increase => "",
            crate::db::GoalDirection::Decrease => " (lower is better)",
        };
        result.push_str(&format!("- {}: {current}/{:.1}{by}{dir} ({:.0}%)\n", gp.exercise_name, gp.goal.target_value, gp.percentage));
    }
    result
}

/// The ACTIVE GOALS block for the `/programme` interview ([C4.2]), which differs from
/// [`format_active_goals`] in one way that matters: each goal is prefixed with its
/// database id.
///
/// The programme has to record *which* goals it serves, and `propose_programme`
/// carries `goal_ids`. A model can only return an id it was shown, so listing them is
/// what makes the link possible at all — without it the host would be reduced to
/// matching goal names out of free text.
pub fn format_goals_with_ids(goals: &[GoalProgress]) -> String {
    if goals.is_empty() {
        return "ACTIVE GOALS: None\n".to_string();
    }
    let lines = goals.iter().map(|gp| {
        let current = gp.current_value.map(|v| format!("{v:.1}")).unwrap_or_else(|| "N/A".to_string());
        let by = gp.goal.target_date.as_deref().map(|d| format!(" by {d}")).unwrap_or_default();
        let dir = match gp.goal.direction {
            crate::db::GoalDirection::Increase => "",
            crate::db::GoalDirection::Decrease => " (lower is better)",
        };
        format!(
            "- id={} {}: {current}/{:.1}{by}{dir} (priority {}, {:.0}%)\n",
            gp.goal.id, gp.exercise_name, gp.goal.target_value, gp.goal.priority, gp.percentage
        )
    });
    std::iter::once("ACTIVE GOALS (use these `id` values in goal_ids):\n".to_string()).chain(lines).collect()
}

pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + &chars.as_str().replace('_', " "),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ExerciseLevel, ExerciseType, Goal, GoalDirection, GoalKind, GoalProgress, GoalStatus};
    use crate::progression::ExerciseClass;
    use crate::db::{HealthEntry, HealthEntryType, MeasurementType, Session};

    fn make_exercise_type(id: i64, name: &str, aliases: &str, muscle_group: &str, mt: MeasurementType) -> ExerciseTypeWithAncestry {
        ExerciseTypeWithAncestry {
            exercise_type: ExerciseType {
                id,
                name: name.to_string(),
                parent_id: Some(1),
                level: ExerciseLevel::Exercise,
                aliases: if aliases.is_empty() { None } else { Some(aliases.to_string()) },
                purpose: Some("strength".to_string()),
                measurement_type: Some(mt),
                url: None,
                created_at: String::new(),
            },
            muscle_group: Some(muscle_group.to_string()),
            specific_muscle: None,
            exercise: None,
        }
    }

    fn base_context() -> PromptContext {
        PromptContext {
            user_name: "Test User".to_string(),
            timezone: "Europe/London".to_string(),
            current_time: "2026-03-23 10:30:00".to_string(),
            active_session: None,
            session_sets: vec![],
            session_entries: vec![],
            leaked_open_entries: vec![],
            active_roster: None,
            health_entries: vec![],
            recent_summaries: vec![],
            recent_sets: vec![],
            exercise_types: vec![
                make_exercise_type(1, "Bench Press", "bench,bench press", "Chest", MeasurementType::WeightReps),
                make_exercise_type(2, "Running", "run,jogging", "Cardio", MeasurementType::DistanceBased),
            ],
            active_goals: vec![],
            has_philosophy: false,
            has_programme: false,
            last_activity_age_hours: None,
        }
    }

    /// A user with everything already in place, so SETUP has nothing to say.
    fn set_up_context() -> PromptContext {
        let mut ctx = base_context();
        ctx.has_philosophy = true;
        ctx.has_programme = true;
        ctx.active_goals = vec![make_goal_progress()];
        ctx
    }

    fn make_goal_progress() -> GoalProgress {
        GoalProgress {
            goal: Goal {
                id: 1,
                user_id: 1,
                kind: GoalKind::Strength,
                exercise_type_id: Some(1),
                metric: None,
                target_value: 100.0,
                direction: GoalDirection::Increase,
                priority: 0,
                start_date: "2026-01-01".to_string(),
                target_date: None,
                achieved: false,
                notes: None,
                created_at: "2026-01-01".to_string(),
                updated_at: "2026-01-01".to_string(),
            },
            exercise_name: "Bench Press".to_string(),
            status: GoalStatus::Active,
            current_value: Some(80.0),
            percentage: 80.0,
        }
    }

    #[test]
    fn prompt_includes_no_active_session() {
        let ctx = base_context();
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("No active session"));
    }

    #[test]
    fn prompt_includes_active_session_with_entries() {
        let mut ctx = base_context();
        ctx.active_session = Some(Session {
            id: 1,
            user_id: 1,
            started_at: "2026-03-23 09:00:00".to_string(),
            ended_at: None,
            notes: None,
            overall_effort: None,
            felt: None,
            cut_short: false,
            cut_short_reason: None,
        });
        ctx.session_entries = vec![PromptEntry {
            id: 7,
            exercise_name: "Bench Press".to_string(),
            set_count: 1,
            sets_summary: "8×80.0kg".to_string(),
            is_open: true,
        }];

        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("Active (started 2026-03-23 09:00:00)"));
        assert!(prompt.contains("EXERCISE ENTRIES"));
        assert!(prompt.contains("Bench Press"));
        assert!(prompt.contains("80.0kg"));
    }

    #[test]
    fn prompt_surfaces_leaked_open_entries() {
        let mut ctx = base_context();
        ctx.leaked_open_entries =
            vec![PromptEntry { id: 3, exercise_name: "Squat".to_string(), set_count: 2, sets_summary: "".to_string(), is_open: true }];
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("LEAKED OPEN ENTRIES"));
        assert!(prompt.contains("[id=3] Squat"));
    }

    fn overhead_press_roster(started: bool) -> RosterProgress {
        RosterProgress {
            title: "Push Day".to_string(),
            started,
            done: Vec::new(),
            next: Some(PromptRosterExercise {
                exercise_name: "Overhead Press".to_string(),
                target_sets: Some(4),
                target_reps: Some(6),
                target_weight_kg: Some(50.0),
                target_secs: None,
                notes: None,
            }),
            remaining: 1,
            override_note: None,
        }
    }

    #[test]
    fn prompt_surfaces_ready_session_roster() {
        let mut ctx = base_context();
        ctx.active_roster = Some(overhead_press_roster(false));
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("SESSION ROSTER READY: Push Day"));
        assert!(prompt.contains("first up when they start: Overhead Press"));
    }

    #[test]
    fn prompt_surfaces_guided_session_roster() {
        let mut ctx = base_context();
        ctx.active_roster = Some(overhead_press_roster(true));
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("SESSION ROSTER (guided, in progress): Push Day"));
        assert!(prompt.contains("NEXT SET: Overhead Press"));
    }

    #[test]
    fn prompt_includes_health_entries() {
        let mut ctx = base_context();
        ctx.health_entries = vec![HealthEntry {
            id: 1,
            user_id: 1,
            entry_type: HealthEntryType::Injury,
            body_part: Some("shoulder".to_string()),
            severity: crate::db::Severity::Moderate,
            description: "Rotator cuff pain".to_string(),
            started_at: "2026-03-20".to_string(),
            resolved_at: None,
            notes: None,
            updated_at: "2026-03-20".to_string(),
        }];
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("Rotator cuff pain"));
        assert!(prompt.contains("shoulder"));
        assert!(prompt.contains("injury"));
    }

    #[test]
    fn prompt_instructs_summary_line_formatting() {
        let ctx = base_context();
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("put each exercise entry on its own line"));
    }

    #[test]
    fn prompt_no_health_entries() {
        let ctx = base_context();
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("ACTIVE HEALTH ISSUES: None"));
    }

    #[test]
    fn exercise_list_grouped_by_muscle_group() {
        let ctx = base_context();
        let list = format_exercise_list(&ctx.exercise_types);
        assert!(list.contains("## Cardio"));
        assert!(list.contains("## Chest"));
        assert!(list.contains("\"Bench Press\" (aliases: bench,bench press) [weight_reps]"));
        assert!(list.contains("\"Running\" (aliases: run,jogging) [distance_based]"));
    }

    #[test]
    fn format_goals() {
        let goals = vec![GoalProgress {
            goal: Goal {
                id: 1,
                user_id: 1,
                kind: GoalKind::Strength,
                exercise_type_id: Some(1),
                metric: None,
                target_value: 100.0,
                direction: GoalDirection::Increase,
                priority: 0,
                start_date: "2026-01-01".to_string(),
                target_date: Some("2026-06-01".to_string()),
                achieved: false,
                notes: None,
                created_at: "2026-01-01".to_string(),
                updated_at: "2026-01-01".to_string(),
            },
            exercise_name: "Bench Press".to_string(),
            status: GoalStatus::Active,
            current_value: Some(80.0),
            percentage: 80.0,
        }];
        let text = format_active_goals(&goals);
        assert!(text.contains("Bench Press: 80.0/100.0 by 2026-06-01 (80%)"));
    }

    #[test]
    fn format_no_goals() {
        let text = format_active_goals(&[]);
        assert!(text.contains("ACTIVE GOALS: None"));
    }

    // ─── SETUP backstop ───────────────────────────────────────────────────────

    /// One nudge at a time, in the order each thing becomes useful — never two, so
    /// the reply cannot turn into a checklist.
    #[test]
    fn setup_nudges_the_first_missing_thing_only() {
        let no_philosophy = format_setup(false, false, false);
        assert!(no_philosophy.contains("/philosophy"));
        assert!(!no_philosophy.contains("/programme"), "must not stack nudges: {no_philosophy}");

        let no_goals = format_setup(true, false, false);
        assert!(no_goals.contains("set_goal"));
        assert!(!no_goals.contains("/philosophy"));
        assert!(!no_goals.contains("/programme"));

        let no_programme = format_setup(true, true, false);
        assert!(no_programme.contains("/programme"));
        assert!(!no_programme.contains("/philosophy"));
    }

    /// Every command the SETUP nudges tell the user to run must actually exist. The
    /// `/programme` nudge shipped in [R1.7] against a command that was only registered
    /// in [C4.2] — until then it sent users at a word the dispatcher did not know.
    #[test]
    fn every_command_the_setup_nudges_name_is_a_real_command() {
        for (philosophy, goals, programme) in [(false, false, false), (true, false, false), (true, true, false)] {
            let nudge = format_setup(philosophy, goals, programme);
            assert!(
                crate::assistant::commands::unknown_commands_in(&nudge).is_empty(),
                "SETUP names a command that does not exist: {nudge}"
            );
        }
    }

    /// A philosophy on file is enough to stop the `/philosophy` nudge whatever else
    /// is missing — the regression that would nag a set-up user every turn.
    #[test]
    fn setup_is_silent_once_everything_is_in_place() {
        assert_eq!(format_setup(true, true, true), "");
        let prompt = build_system_prompt(&set_up_context());
        assert!(!prompt.contains("SETUP:"), "a fully set-up user must see no SETUP section");
    }

    #[test]
    fn setup_section_reaches_the_system_prompt() {
        let prompt = build_system_prompt(&base_context());
        assert!(prompt.contains("SETUP:"));
        assert!(prompt.contains("/philosophy"));
    }

    #[test]
    fn format_muscle_recovery_orders_longest_rested_first() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        let recovery = vec![
            MuscleRecovery { muscle_group: "Chest".into(), last_trained: Some("2026-01-18".into()), last_volume_sets: 12 },
            MuscleRecovery { muscle_group: "Back".into(), last_trained: None, last_volume_sets: 0 },
            MuscleRecovery { muscle_group: "Legs".into(), last_trained: Some("2026-01-05".into()), last_volume_sets: 9 },
        ];
        let text = format_muscle_recovery(&recovery, today);

        // Never-trained first, then most-rested (Legs, 15d) before least (Chest, 2d).
        let back = text.find("Back").unwrap();
        let legs = text.find("Legs").unwrap();
        let chest = text.find("Chest").unwrap();
        assert!(back < legs && legs < chest, "expected order Back, Legs, Chest:\n{text}");

        assert!(text.contains("- Back: never trained"));
        assert!(text.contains("- Legs: last trained 15d ago (9 sets)"));
        assert!(text.contains("- Chest: last trained 2d ago (12 sets)"));
    }

    #[test]
    fn format_muscle_recovery_handles_empty() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        assert!(format_muscle_recovery(&[], today).contains("no muscle groups on record"));
    }

    #[test]
    fn prompt_describes_get_last_exercise_action() {
        let ctx = base_context();
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("get_last_exercise"), "prompt must advertise the get_last_exercise action");
    }

    fn designer_prompt(health_entries: &[HealthEntry]) -> String {
        designer_prompt_for(&[], health_entries, &[])
    }

    fn designer_prompt_for(goals: &[GoalProgress], health_entries: &[HealthEntry], science: &[ScienceChunk]) -> String {
        designer_prompt_with(goals, health_entries, science, &empty_policy())
    }

    fn designer_prompt_with(
        goals: &[GoalProgress],
        health_entries: &[HealthEntry],
        science: &[ScienceChunk],
        progression: &ProgressionPolicy,
    ) -> String {
        build_designer_prompt(&DesignerInputs {
            philosophy: "goal=hypertrophy. Home gym: dumbbells up to 24kg.",
            history: "RECENT HISTORY: No recent workouts\n",
            recovery: "MUSCLE RECOVERY: no muscle groups on record.\n",
            goals,
            health_entries,
            science,
            progression,
            catalogue: &base_context().exercise_types,
        })
    }

    /// A user with no logged history: the designer still runs, on the conservative fallback rule.
    fn empty_policy() -> ProgressionPolicy {
        ProgressionPolicy { block: BlockIntent::Ordinary, directives: Vec::new(), deload_advice: None }
    }

    fn directive(name: &str, action: ProgressionAction) -> ProgressionDirective {
        ProgressionDirective {
            exercise_name: name.to_string(),
            class: ExerciseClass::UpperBodyCompound,
            measurement_type: MeasurementType::WeightReps,
            action,
            reason: "test fixture".to_string(),
        }
    }

    /// A goal of `kind` at `priority`, denominated in `subject` — enough of a [`GoalProgress`] for
    /// the prompt layer, which reads only kind, priority and the display name.
    fn goal(kind: GoalKind, priority: i64, subject: &str) -> GoalProgress {
        GoalProgress {
            goal: Goal {
                id: 1,
                user_id: 1,
                kind,
                exercise_type_id: Some(1),
                metric: None,
                target_value: 100.0,
                direction: GoalDirection::Increase,
                priority,
                start_date: "2026-01-01".to_string(),
                target_date: None,
                achieved: false,
                notes: None,
                created_at: "2026-01-01".to_string(),
                updated_at: "2026-01-01".to_string(),
            },
            exercise_name: subject.to_string(),
            status: GoalStatus::Active,
            current_value: Some(80.0),
            percentage: 80.0,
        }
    }

    /// Retrieve for `goals` the way `/nextworkout` does, so a band test exercises the real path
    /// from a goal kind to the prompt rather than a hand-assembled science block.
    fn science_for(goals: &[GoalProgress]) -> Vec<ScienceChunk> {
        let index = crate::science::ScienceIndex::build().unwrap();
        let goal_kinds = goals_by_priority(goals).iter().map(|gp| gp.goal.kind).fold(Vec::new(), |mut kinds, kind| {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
            kinds
        });
        let prescriptions = goal_kinds.iter().map(|k| crate::science::prescription_doc(*k).to_string());
        let resolution = (goal_kinds.len() > 1).then(|| "competing-goals".to_string());
        let pinned_docs = prescriptions.chain(resolution).collect();
        index.search(&crate::science::ScienceQuery { goal_kinds, pinned_docs, ..Default::default() }, 4)
    }

    // ── Prescription bands ────────────────────────────────────────────────────
    //
    // The claim [C5.2] makes is that "is the science right" becomes a question with an answer.
    // These tests are the narrow, honest version of that: they assert the *corpus's own* numbers
    // for a goal kind survive retrieval, budgeting and formatting into the prompt the model reads.
    // They do NOT assert the model obeys them, and they do not adjudicate the science — editing a
    // band in `backend/science/` and updating the assertion here is a legitimate change, and the
    // diff is where a human judges it. What they do buy: a band can no longer disappear from the
    // prompt silently, which is the failure mode that leaves the designer running on model recall.

    /// One goal kind, and the numbers the corpus prescribes for it that must reach the prompt.
    const PRESCRIPTION_BANDS: [(GoalKind, &str, &[&str]); 5] = [
        (GoalKind::Strength, "[S:goal-strength]", &["1-6 per working set", "80-90% of a one-rep maximum", "3-5 minutes"]),
        (GoalKind::Endurance, "[S:goal-endurance]", &["15 or more per set", "below roughly 60%", "30-90 seconds"]),
        (GoalKind::Bodyweight, "[S:goal-body-composition]", &["6-12", "0.5-1% of bodyweight per week", "1.6 g per kg"]),
        (GoalKind::BodyComposition, "[S:goal-body-composition]", &["6-12", "0.5-1% of bodyweight per week", "1.6 g per kg"]),
        (GoalKind::Habit, "[S:goal-habit]", &["Set the frequency at what will actually happen", "Anchor the behaviour to a cue"]),
    ];

    #[test]
    fn every_goal_kind_lands_its_prescription_band_in_the_prompt() {
        PRESCRIPTION_BANDS.iter().for_each(|(kind, citation, bands)| {
            let goals = vec![goal(*kind, 0, "Bench Press")];
            let prompt = designer_prompt_for(&goals, &[], &science_for(&goals));
            assert!(prompt.contains("TRAINING SCIENCE"), "{kind:?}: no science section reached the prompt");
            assert!(prompt.contains(citation), "{kind:?}: expected the {citation} document to be cited");
            bands.iter().for_each(|band| {
                assert!(prompt.contains(band), "{kind:?}: the prescribed band {band:?} never reached the prompt");
            });
        });
    }

    #[test]
    fn the_science_section_is_framed_as_a_constraint_not_a_suggestion() {
        let goals = vec![goal(GoalKind::Strength, 0, "Bench Press")];
        let prompt = designer_prompt_for(&goals, &[], &science_for(&goals));
        assert!(prompt.contains("CONSTRAINTS your prescription MUST fall inside"));
        // The KB narrows the prescription; the LLM still picks the exercises — the adaptivity is
        // the product, so the prompt must not read as a lookup table of workouts.
        assert!(prompt.contains("It does NOT tell you WHICH exercises to choose"));
        assert!(prompt.contains("[S:doc-id]"), "the model must be told how to cite");
    }

    #[test]
    fn the_designer_no_longer_falls_back_on_its_own_expertise() {
        let goals = vec![goal(GoalKind::Strength, 0, "Bench Press")];
        let prompt = designer_prompt_for(&goals, &[], &science_for(&goals));
        assert!(!prompt.contains("Draw on your own expertise"), "the pre-[C5.2] instruction must be gone");
        assert!(prompt.contains("prefer them over your own recall"));
    }

    /// With no science retrieved the prompt degrades to its pre-[C5.2] shape rather than pointing
    /// at a section that is not there — an instruction to obey absent bands invites invention.
    #[test]
    fn an_empty_science_result_leaves_no_dangling_reference() {
        let prompt = designer_prompt_for(&[], &[], &[]);
        assert!(!prompt.contains("TRAINING SCIENCE IS THE AUTHORITY"));
        assert!(!prompt.contains("TRAINING SCIENCE (curated"));
        assert!(prompt.contains("SELECTION PRIORITY"), "the rest of the designer prompt is unaffected");
    }

    // ── Competing goals ───────────────────────────────────────────────────────

    #[test]
    fn competing_goals_are_ranked_by_priority_and_resolved_not_averaged() {
        // A strength goal and a fat-loss goal: the conflict the ticket names, with fat loss ranked
        // higher, so the block must not simply echo the order the goals arrived in.
        let goals = vec![goal(GoalKind::Strength, 1, "Bench Press"), goal(GoalKind::BodyComposition, 9, "body_fat_pct")];
        let prompt = designer_prompt_for(&goals, &[], &science_for(&goals));

        let block = prompt.split("COMPETING GOALS").nth(1).expect("a competing-goals block");
        let first = block.find("body_composition").expect("the higher-priority goal is listed");
        let second = block.find("strength").expect("the lower-priority goal is listed");
        assert!(first < second, "goals must be listed highest-priority first:\n{block}");
        assert!(block.contains("1. body_composition — body_fat_pct (priority 9)"));
        assert!(block.contains("2. strength — Bench Press (priority 1)"));
        assert!(block.contains("do NOT average them"));
    }

    /// The resolution rule is science, so it is pinned into retrieval rather than restated in the
    /// prompt: ranking must never be able to drop it.
    #[test]
    fn competing_goals_pin_the_curated_resolution_into_the_prompt() {
        let goals = vec![goal(GoalKind::Strength, 5, "Bench Press"), goal(GoalKind::BodyComposition, 1, "body_fat_pct")];
        let prompt = designer_prompt_for(&goals, &[], &science_for(&goals));
        assert!(prompt.contains("[S:competing-goals]"));
        assert!(prompt.contains("Goals are resolved by **priority**, not by averaging"), "the corpus's own rule must be quoted");
    }

    #[test]
    fn a_single_goal_kind_is_not_a_conflict() {
        // Two strength goals are one kind of prescription. Rendering a resolution rule here would
        // invite the model to trade one lift off against another for no reason.
        let goals = vec![goal(GoalKind::Strength, 5, "Bench Press"), goal(GoalKind::Strength, 1, "Squat")];
        let prompt = designer_prompt_for(&goals, &[], &science_for(&goals));
        assert!(!prompt.contains("COMPETING GOALS"));
    }

    // ── Contraindications [C5.4] ──────────────────────────────────────────────

    fn injury(body_part: &str, severity: crate::db::Severity) -> HealthEntry {
        let mut entry = crate::db::new_health_entry(1, HealthEntryType::Injury, "sore");
        entry.body_part = Some(body_part.to_string());
        entry.severity = severity;
        entry
    }

    /// The section states the barred patterns and the way out, and graduates with severity — the
    /// same table the post-parse rail enforces, so prompt and check cannot drift apart.
    #[test]
    fn contraindications_render_from_the_rail_table() {
        let block = format_contraindications(&[injury("shoulder", crate::db::Severity::Moderate)]);
        assert!(block.contains("CONTRAINDICATIONS"), "{block}");
        assert!(block.contains("overhead pressing"), "the barred pattern must be named: {block}");
        assert!(block.contains("landmine press"), "and the substitution that keeps the session: {block}");
        assert!(block.contains("shoulder (moderate)"), "severity must be visible: {block}");
    }

    /// Mild bars less than moderate, which bars less than severe.
    #[test]
    fn the_rendered_section_graduates_with_severity() {
        let mild = format_contraindications(&[injury("lower back", crate::db::Severity::Mild)]);
        let severe = format_contraindications(&[injury("lower back", crate::db::Severity::Severe)]);
        assert!(!mild.contains("axial spinal compression"), "mild keeps the pattern with less load: {mild}");
        assert!(severe.contains("axial spinal compression"), "severe does not load the spine: {severe}");
    }

    /// No injuries, or an injury the corpus has no document for, means no section at all — a heading
    /// over an empty list reads as a rule the model cannot see.
    #[test]
    fn nothing_to_bar_renders_nothing() {
        assert!(format_contraindications(&[]).is_empty());
        assert!(format_contraindications(&[injury("elbow", crate::db::Severity::Severe)]).is_empty(), "no document, no rail");
        let illness = crate::db::new_health_entry(1, HealthEntryType::Illness, "flu");
        assert!(format_contraindications(&[illness]).is_empty(), "an illness contraindicates no movement");
    }

    /// Two rails for one body part often share a substitution; the list should read like advice.
    #[test]
    fn substitutions_are_not_repeated() {
        let block = format_contraindications(&[injury("knee", crate::db::Severity::Severe)]);
        let cycling = block.matches("cycling").count();
        assert_eq!(cycling, 1, "`cycling` substitutes for three knee patterns but should be listed once:\n{block}");
    }

    // ── Budget ────────────────────────────────────────────────────────────────

    #[test]
    fn the_science_block_is_capped_and_says_when_it_truncated() {
        // Eight chunks of real corpus prose comfortably exceed the budget.
        let science: Vec<ScienceChunk> = crate::science::all_chunks().take(8).map(|(_, chunk)| chunk.clone()).collect();
        let block = format_training_science(&science);
        assert!(estimate_tokens(&block) <= SCIENCE_TOKEN_BUDGET + estimate_tokens("(8 further science excerpt(s) omitted…)\n"));
        assert!(block.contains("omitted to fit the prompt budget"), "truncation must be announced:\n{block}");
    }

    #[test]
    fn a_pinned_rail_survives_the_budget() {
        // Rails arrive first, so the block that is never dropped is the one at the head.
        let science: Vec<ScienceChunk> = crate::science::all_chunks().take(8).map(|(_, chunk)| chunk.clone()).collect();
        let block = format_training_science(&science);
        assert!(block.contains(&science[0].heading), "the leading (pinned) excerpt must survive truncation");
    }

    #[test]
    fn designer_prompt_lists_selection_priorities_in_decreasing_order() {
        let prompt = designer_prompt(&[]);
        let rungs = [
            "1. Contribution to the user's ACTIVE GOALS",
            "2. Exercises the user has done before",
            "3. Muscle groups with the longest rest period",
            "4. Fit with the TRAINING PHILOSOPHY",
            "5. Working around ACTIVE HEALTH ISSUES",
            "6. Temporary, single-session requests",
        ];
        let positions: Vec<usize> = rungs
            .iter()
            .map(|rung| prompt.find(rung).unwrap_or_else(|| panic!("prompt is missing priority rung: {rung}")))
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "priority rungs appear out of order:\n{prompt}");
        assert!(prompt.contains("in DECREASING priority"));
    }

    #[test]
    fn designer_prompt_priorities_break_ties_rather_than_filter() {
        let prompt = designer_prompt(&[]);
        assert!(prompt.contains("not a filter"));
        assert!(prompt.contains("a lower item never vetoes a higher one"));
        assert!(prompt.contains("breaks ties"));
    }

    #[test]
    fn designer_prompt_keeps_injuries_a_hard_constraint() {
        let entries = vec![HealthEntry {
            id: 1,
            user_id: 1,
            entry_type: HealthEntryType::Injury,
            body_part: Some("lower back".to_string()),
            severity: crate::db::Severity::Moderate,
            description: "Lower back flare-up".to_string(),
            started_at: "2026-03-20".to_string(),
            resolved_at: None,
            notes: None,
            updated_at: "2026-03-20".to_string(),
        }];
        let prompt = designer_prompt(&entries);
        assert!(prompt.contains("ACTIVE HEALTH ISSUES are a HARD constraint, not a tie-breaker"));
        assert!(prompt.contains("Lower back flare-up"), "health entries must be rendered via format_health_entries");
    }

    #[test]
    fn designer_prompt_requires_rationale_to_cite_priorities() {
        let prompt = designer_prompt(&[]);
        assert!(prompt.contains("MUST name which SELECTION PRIORITY items drove today's"));
    }

    #[test]
    fn prompt_describes_record_session_outcome_action() {
        let ctx = base_context();
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("record_session_outcome"), "prompt must advertise the record_session_outcome action");
        assert!(prompt.contains("great|good|ok|rough"), "prompt must spell out the feel vocabulary");
    }

    #[test]
    fn format_session_outcome_joins_recorded_parts() {
        let mut session = Session {
            id: 1,
            user_id: 1,
            started_at: "2026-03-20 09:00:00".to_string(),
            ended_at: Some("2026-03-20 10:00:00".to_string()),
            notes: None,
            overall_effort: Some(crate::db::Difficulty::Hard),
            felt: Some(crate::db::SessionFeel::Good),
            cut_short: true,
            cut_short_reason: Some("knee pain".to_string()),
        };
        assert_eq!(format_session_outcome(&session).as_deref(), Some("overall hard, felt good, cut short (knee pain)"));

        session.felt = None;
        session.cut_short_reason = None;
        assert_eq!(format_session_outcome(&session).as_deref(), Some("overall hard, cut short"));

        session.overall_effort = None;
        session.cut_short = false;
        assert_eq!(format_session_outcome(&session), None, "no outcome recorded → no phrase");
    }

    #[test]
    fn recent_history_includes_session_outcome() {
        let session = Session {
            id: 1,
            user_id: 1,
            started_at: "2026-03-20 09:00:00".to_string(),
            ended_at: Some("2026-03-20 10:00:00".to_string()),
            notes: None,
            overall_effort: Some(crate::db::Difficulty::Hard),
            felt: Some(crate::db::SessionFeel::Good),
            cut_short: false,
            cut_short_reason: None,
        };
        let summary = SessionSummary { session, exercise_count: 3, duration_mins: Some(60) };
        let text = format_recent_history(&[summary], &[], &[]);
        assert!(text.contains("- 2026-03-20 09:00:00 [completed]: 3 entries (60 min) — overall hard, felt good"), "{text}");
    }

    // ── Progressive overload policy [C5.3] ────────────────────────────────────

    /// The sentence this ticket exists to delete. Its survival anywhere in the designer prompt
    /// would leave a second, vaguer progression model contradicting the computed one.
    #[test]
    fn the_old_one_line_progression_heuristic_is_gone() {
        let with_policy = designer_prompt_with(
            &[],
            &[],
            &[],
            &ProgressionPolicy {
                block: BlockIntent::Ordinary,
                directives: vec![directive("Bench Press", ProgressionAction::Progress { from: 60.0, to: 62.5 })],
                deload_advice: None,
            },
        );
        for prompt in [designer_prompt(&[]), with_policy] {
            assert!(
                !prompt.contains("if the last sets of an exercise were easy, progress the"),
                "the pre-[C5.3] heuristic must not survive:\n{prompt}"
            );
        }
    }

    #[test]
    fn a_computed_policy_reaches_the_prompt_as_a_binding_rule() {
        let prompt = designer_prompt_with(
            &[],
            &[],
            &[],
            &ProgressionPolicy {
                block: BlockIntent::Ordinary,
                directives: vec![
                    directive("Bench Press", ProgressionAction::Progress { from: 60.0, to: 62.5 }),
                    directive("Squat", ProgressionAction::Hold { at: 90.0 }),
                    directive("Lateral Raise", ProgressionAction::BackOff { from: 14.0, to: 12.0 }),
                ],
                deload_advice: None,
            },
        );

        assert!(prompt.contains("PROGRESSION POLICY"), "the section must be present:\n{prompt}");
        assert!(prompt.contains("it is BINDING, not advisory"), "the rule must bind the model, not suggest to it");
        assert!(prompt.contains("- Bench Press: PROGRESS 60.0kg -> 62.5kg"), "an earned increment states both loads:\n{prompt}");
        assert!(prompt.contains("- Squat: HOLD at 90.0kg"));
        assert!(prompt.contains("- Lateral Raise: BACK OFF 14.0kg -> 12.0kg"), "backing off is as explicit as progressing");
        // The RPE/RIR answer ([C3.3]): the four-point scale, read as reps in reserve.
        assert!(prompt.contains("easy = 4+ RIR") && prompt.contains("failure = 0 RIR"), "the effort→RIR reading must reach the model");
    }

    /// A deload block overrides progression, and the prompt has to say so where the model will act
    /// on it — next to the loads, not only in the retrieved science.
    #[test]
    fn a_deload_block_overrides_progression_in_the_prompt() {
        let prompt = designer_prompt_with(
            &[],
            &[],
            &[],
            &ProgressionPolicy {
                block: BlockIntent::Deload { focus: "deload".to_string() },
                directives: vec![directive("Bench Press", ProgressionAction::Deload { at: 60.0 })],
                deload_advice: None,
            },
        );
        assert!(prompt.contains("PROGRESSION POLICY — DELOAD WEEK"), "the deload must head the section:\n{prompt}");
        assert!(prompt.contains("OVERRIDES ordinary progression"));
        assert!(prompt.contains("cut the working sets by about a third"));
        assert!(prompt.contains("- Bench Press: DELOAD at 60.0kg"));
    }

    #[test]
    fn accumulated_back_off_advice_is_surfaced_when_present() {
        let prompt = designer_prompt_with(
            &[],
            &[],
            &[],
            &ProgressionPolicy {
                block: BlockIntent::Ordinary,
                directives: vec![directive("Bench Press", ProgressionAction::BackOff { from: 60.0, to: 55.0 })],
                deload_advice: Some("2 of 3 exercises are backing off at once.".to_string()),
            },
        );
        assert!(prompt.contains("ACCUMULATED FATIGUE: 2 of 3 exercises are backing off"), "{prompt}");
    }

    /// A user with nothing logged gets a conservative starting rule and no empty section claiming a
    /// policy the prompt does not carry.
    #[test]
    fn no_history_yields_a_conservative_fallback_and_no_empty_section() {
        let prompt = designer_prompt(&[]);
        assert!(!prompt.contains("PROGRESSION POLICY"), "an empty policy must not render a section:\n{prompt}");
        assert!(prompt.contains("no logged history to progress from"));
        assert!(prompt.contains("2-3 reps in reserve"));
    }

    /// Timed work progresses in seconds, not kilograms: the directive renders through the
    /// measurement type rather than assuming load.
    #[test]
    fn a_timed_exercise_directive_renders_in_its_own_unit() {
        let mut plank = directive("Plank", ProgressionAction::Progress { from: 60.0, to: 63.0 });
        plank.measurement_type = MeasurementType::TimeBased;
        plank.class = ExerciseClass::Conditioning;
        let prompt = designer_prompt_with(&[], &[], &[], &ProgressionPolicy {
            block: BlockIntent::Ordinary,
            directives: vec![plank],
            deload_advice: None,
        });
        assert!(prompt.contains("- Plank: PROGRESS 60s -> 63s"), "timed work must not be described in kg:\n{prompt}");
    }
}

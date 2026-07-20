use chrono::NaiveDate;

use crate::db::{
    ExerciseSet, ExerciseTypeWithAncestry, GoalProgress, HealthEntry, MeasurementType, MuscleRecovery, Session, SessionSummary,
};

pub struct PromptContext {
    pub user_name: String,
    pub timezone: String,
    pub current_time: String,
    pub active_session: Option<Session>,
    pub session_sets: Vec<(ExerciseSet, String)>, // (set, exercise_type name) — flat view, kept for backward compat
    pub session_entries: Vec<EntryView>,          // closed + open entries in the active session, in insertion order
    pub leaked_open_entries: Vec<EntryView>,      // open entries belonging to ENDED prior sessions or the active session
    pub active_plan: Option<ActivePlanView>,      // populated when the active session was started with a `plan:` sentinel
    pub active_workout_plan: Option<WorkoutPlanProgress>, // a `/nextworkout` design that is ready or under guided execution
    pub health_entries: Vec<HealthEntry>,
    pub recent_summaries: Vec<SessionSummary>,
    pub recent_sets: Vec<ExerciseSet>,
    pub exercise_types: Vec<ExerciseTypeWithAncestry>,
    pub active_goals: Vec<GoalProgress>,
    /// Hours since the user's last logged set (or session start, if no sets yet).
    /// Only populated when an active session exists; drives the SESSION CONTINUITY
    /// rule (auto-new ≥12h, ask <12h).
    pub last_activity_age_hours: Option<f64>,
}

/// Cutoff in hours above which the assistant treats a new exercise message as
/// the start of a fresh workout without asking. Below this, it must confirm.
pub const SESSION_CONTINUITY_HOURS: f64 = 12.0;

#[derive(Debug, Clone)]
pub struct EntryView {
    pub id: i64,
    pub exercise_name: String,
    pub set_count: usize,
    pub sets_summary: String,
    pub is_open: bool,
}

#[derive(Debug, Clone)]
pub struct ActivePlanView {
    pub name: String,
    pub completed_exercise_ids: Vec<i64>,
    pub next: Option<PlanExerciseView>,
}

#[derive(Debug, Clone)]
pub struct PlanExerciseView {
    pub exercise_name: String,
    pub target_sets: Option<i32>,
    pub target_reps: Option<i32>,
    pub target_weight_kg: Option<f64>,
}

/// Progress of a `/nextworkout` design: either freshly designed and ready to start
/// (`started == false`) or bound to the active session and under guided execution
/// (`started == true`). Drives the proactive set-by-set coaching.
#[derive(Debug, Clone)]
pub struct WorkoutPlanProgress {
    pub title: String,
    pub started: bool,
    /// Names of prescribed exercises the user has already logged sets for this session.
    pub done: Vec<String>,
    /// The next prescribed exercise still to do.
    pub next: Option<PrescribedExercise>,
    /// How many prescribed exercises remain.
    pub remaining: usize,
    /// Today-only overrides the user voiced for THIS plan ("no bench today, do flys
    /// instead"). Applies to the plan in flight only; never folded into the philosophy.
    pub override_note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PrescribedExercise {
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
    let plan_section = format_active_plan(&ctx.active_plan);
    let workout_plan_section = format_active_workout_plan(&ctx.active_workout_plan);
    let continuity_section = format_session_continuity(ctx.last_activity_age_hours);
    let continuity_banner = format_session_continuity_banner(ctx.last_activity_age_hours);
    let health_section = format_health_entries(&ctx.health_entries);
    let history_section = format_recent_history(&ctx.recent_summaries, &ctx.recent_sets, &ctx.exercise_types);
    let goals_section = format_active_goals(&ctx.active_goals);
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
- {{\"type\": \"start_session\", \"notes\": \"<optional>\", \"plan\": \"<optional schedule name>\"}}\n\
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
- {{\"type\": \"set_session_override\", \"note\": \"<today-only change to the plan in flight>\"}}\n\
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
- After an entry is closed and the active plan has a `next` exercise, suggest that exercise \
to the user (mention target sets/reps/weight from the plan if present).\n\
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
- GUIDED WORKOUT: When a PRESCRIBED WORKOUT or DESIGNED WORKOUT section is present, \
you are coaching the user through a pre-designed session like a personal trainer. \
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
CURRENT STATE:\n\
User: {user_name}\n\
Time: {current_time} ({timezone})\n\
Active session: {session_status}\n\
\n\
{entries_section}\
{leaked_section}\
{plan_section}\
{workout_plan_section}\
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
/// (the prompt later fed to the workout designer).
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
3. Preferred programs or styles (e.g. 5x5, push/pull/legs, high-rep, circuits) — optional.\n\
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
(equipment is required; programs are optional). When you do, set `content` to a SINGLE compact, \
information-dense paragraph — not a transcript — capturing goal, weekly frequency, preferred \
programs, equipment with limits, and any relevant injuries or preferences. Example content:\n\
  \"goal=hypertrophy. Likes 5x5. Home gym: squat rack up to 120kg, bench, kettlebells, dumbbells \
up to 24kg. Weights 3x/week, racket sports 2x/week. Minor lower-back niggle — cautious on heavy spinal load.\"\n\
In the SAME response, your `message` should briefly confirm what you saved. Until you are ready \
to save, keep `actions` empty and ask the next question.",
    )
}

/// System prompt for `/nextworkout`: design ONE tailored training session from the
/// user's philosophy, recent history, goals, and injuries. It advertises ONLY the
/// `propose_workout` action — the design is a proposal and logs nothing. Exercise
/// selection follows the spec's decreasing priority order (goal contribution, done
/// before, longest-rested muscles, philosophy fit, health issues, temporary
/// requests); lower rungs break ties rather than veto, except active health issues,
/// which stay a hard constraint. `history` is a pre-formatted block (the caller has
/// the catalogue to resolve names); `philosophy` is the distilled philosophy text
/// or a short placeholder.
pub fn build_designer_prompt(
    philosophy: &str,
    history: &str,
    recovery: &str,
    goals: &[GoalProgress],
    health_entries: &[HealthEntry],
    catalogue: &[ExerciseTypeWithAncestry],
) -> String {
    let goals_section = format_active_goals(goals);
    let health_section = format_health_entries(health_entries);
    let exercise_list = format_exercise_list(catalogue);

    format!(
        "You are a personal gym trainer DESIGNING one training session for the user right now. \
Draw on your own expertise PLUS the user-specific information below to produce a highly tailored, \
specific session that pushes the user toward their goals. You are only designing a plan — you do \
NOT log any sets and do NOT start a session.\n\
\n\
SELECTION PRIORITY — rank candidate exercises by these criteria, in DECREASING priority:\n\
1. Contribution to the user's ACTIVE GOALS.\n\
2. Exercises the user has done before (they appear in RECENT HISTORY).\n\
3. Muscle groups with the longest rest period (MUSCLE RECOVERY lists them longest-rested first; \
treat a group shown as never trained, or not trained in a long time, as a strong candidate).\n\
4. Fit with the TRAINING PHILOSOPHY: its goal, preferred programs/rotation, weekly frequency.\n\
5. Working around ACTIVE HEALTH ISSUES.\n\
6. Temporary, single-session requests in the user's message (\"something lighter today\").\n\
This is a decreasing priority order, not a filter: a lower item never vetoes a higher one — it \
only breaks ties between options the higher items rank equally. EXCEPTION: ACTIVE HEALTH ISSUES \
are a HARD constraint, not a tie-breaker — never prescribe a movement that loads an injured area; \
substitute away (e.g. swap heavy spinal-loading deadlifts/squats for a focused one-arm row when \
the lower back is flaring).\n\
\n\
HOW TO DESIGN (reason like a real trainer):\n\
- Honour the EQUIPMENT in the philosophy and its weight limits — never prescribe a weight the \
user cannot load, or equipment they do not have.\n\
- Use RECENT HISTORY to set weights: if the last sets of an exercise were easy, progress the \
load; if they were hard or to failure, hold or back off. Avoid repeating yesterday's heavy work.\n\
- Pick 3-6 exercises. For each, prescribe target sets and target reps (or seconds for timed work) \
and a target weight within the user's equipment limits. Add a short per-exercise cue when useful.\n\
\n\
{philosophy_section}\n\
{history}\n\
{recovery}\n\
{goals_section}\n\
{health_section}\n\
RESPONSE FORMAT: You MUST respond with ONLY a JSON object. No text before or after.\n\
{{\n\
  \"message\": \"<one or two sentences introducing the session>\",\n\
  \"actions\": [{{\"type\": \"propose_workout\", ...}}]\n\
}}\n\
\n\
You MUST emit EXACTLY ONE action, of type propose_workout:\n\
- {{\"type\": \"propose_workout\", \"title\": \"<short session title>\", \
\"rationale\": \"<2-4 sentences that MUST name which SELECTION PRIORITY items drove today's \
picks (e.g. goal contribution, longest-rested muscles), plus any injury substitutions and why>\", \
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
    )
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

fn format_session_entries(entries: &[EntryView]) -> String {
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

fn format_leaked_entries(entries: &[EntryView]) -> String {
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

fn format_active_plan(plan: &Option<ActivePlanView>) -> String {
    let Some(plan) = plan else {
        return String::new();
    };
    let mut s = format!("ACTIVE PLAN: {}\n", plan.name);
    s.push_str(&format!("- completed exercises in this session: {}\n", plan.completed_exercise_ids.len()));
    if let Some(next) = &plan.next {
        let mut parts = vec![next.exercise_name.clone()];
        if let Some(sets) = next.target_sets {
            parts.push(format!("{sets} sets"));
        }
        if let Some(reps) = next.target_reps {
            parts.push(format!("{reps} reps"));
        }
        if let Some(w) = next.target_weight_kg {
            parts.push(format!("{w}kg"));
        }
        s.push_str(&format!("- next: {}\n", parts.join(", ")));
    } else {
        s.push_str("- next: (plan complete)\n");
    }
    s.push('\n');
    s
}

/// Render the `/nextworkout` design for the system prompt: a "ready to start" hint
/// before the workout begins, or live progress with the NEXT prescribed set once it
/// is under way. Distinct from the schedule-based ACTIVE PLAN section above.
fn format_active_workout_plan(plan: &Option<WorkoutPlanProgress>) -> String {
    let Some(plan) = plan else {
        return String::new();
    };

    let mut s = String::new();
    if plan.started {
        s.push_str(&format!("PRESCRIBED WORKOUT (guided, in progress): {}\n", plan.title));
        if !plan.done.is_empty() {
            s.push_str(&format!("- done so far: {}\n", plan.done.join(", ")));
        }
        match &plan.next {
            Some(next) => s.push_str(&format!("- NEXT SET: {} ({} to go)\n", format_prescription(next), plan.remaining)),
            None => s.push_str("- all prescribed exercises done — congratulate the user and offer to end the session\n"),
        }
    } else {
        s.push_str(&format!("DESIGNED WORKOUT READY: {}\n", plan.title));
        if let Some(next) = &plan.next {
            s.push_str(&format!("- first up when they start: {}\n", format_prescription(next)));
        }
        s.push_str("- when the user starts their workout, walk them through it set by set\n");
    }
    if let Some(note) = plan.override_note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        s.push_str(&format!(
            "- TODAY-ONLY OVERRIDES for this workout (honour them now; do NOT save to philosophy):\n{note}\n"
        ));
    }
    s.push('\n');
    s
}

/// A prescribed exercise as one compact line, e.g. "Bench Press — 3 sets 6 reps @ 65kg (push it)".
fn format_prescription(p: &PrescribedExercise) -> String {
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
            active_plan: None,
            active_workout_plan: None,
            health_entries: vec![],
            recent_summaries: vec![],
            recent_sets: vec![],
            exercise_types: vec![
                make_exercise_type(1, "Bench Press", "bench,bench press", "Chest", MeasurementType::WeightReps),
                make_exercise_type(2, "Running", "run,jogging", "Cardio", MeasurementType::DistanceBased),
            ],
            active_goals: vec![],
            last_activity_age_hours: None,
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
        ctx.session_entries = vec![EntryView {
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
            vec![EntryView { id: 3, exercise_name: "Squat".to_string(), set_count: 2, sets_summary: "".to_string(), is_open: true }];
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("LEAKED OPEN ENTRIES"));
        assert!(prompt.contains("[id=3] Squat"));
    }

    #[test]
    fn prompt_surfaces_active_plan() {
        let mut ctx = base_context();
        ctx.active_plan = Some(ActivePlanView {
            name: "Push Day".to_string(),
            completed_exercise_ids: vec![1],
            next: Some(PlanExerciseView {
                exercise_name: "Overhead Press".to_string(),
                target_sets: Some(4),
                target_reps: Some(6),
                target_weight_kg: Some(50.0),
            }),
        });
        let prompt = build_system_prompt(&ctx);
        assert!(prompt.contains("ACTIVE PLAN: Push Day"));
        assert!(prompt.contains("next: Overhead Press"));
    }

    #[test]
    fn prompt_includes_health_entries() {
        let mut ctx = base_context();
        ctx.health_entries = vec![HealthEntry {
            id: 1,
            user_id: 1,
            entry_type: HealthEntryType::Injury,
            body_part: Some("shoulder".to_string()),
            severity: "moderate".to_string(),
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
        build_designer_prompt(
            "goal=hypertrophy. Home gym: dumbbells up to 24kg.",
            "RECENT HISTORY: No recent workouts\n",
            "MUSCLE RECOVERY: no muscle groups on record.\n",
            &[],
            health_entries,
            &base_context().exercise_types,
        )
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
            severity: "moderate".to_string(),
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
}

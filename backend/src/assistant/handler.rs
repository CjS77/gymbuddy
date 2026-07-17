use std::sync::Arc;

use anyhow::Context as _;
use chrono::{NaiveDateTime, Utc};
use corre_core::app::{LlmMessage, LlmProvider, LlmRequest, LlmRole};
use tokio::sync::Mutex;

use crate::config::GymConfig;
use crate::db::{
    ConversationRole, Database, Difficulty, ExerciseEntry, ExerciseSet, ExerciseTypeWithAncestry, InterviewState, MeasurementType, Session,
    SessionWithSets, SetEdit, SetEditError, User, WorkoutPhilosophy, WorkoutPlanExercise, new_conversation_message, new_exercise_entry_at,
    new_exercise_goal, new_exercise_set, new_health_entry, new_user, new_user_with_pubkey,
};
use crate::github::IssueReporter;
use crate::telegram::Message as TgMessage;

use super::actions::{AssistantAction, ProposedExercise};
use super::commands::{self, Command};
use super::matching::find_exercise_type;
use super::parser::parse_assistant_response;
use super::prompts::{
    ActivePlanView, EntryView, PlanExerciseView, PrescribedExercise, PromptContext, SESSION_CONTINUITY_ASK_HOURS,
    SESSION_CONTINUITY_HOURS, WorkoutPlanProgress, build_designer_prompt, build_philosophy_prompt, build_system_prompt,
    format_muscle_recovery,
};
use gymbuddy_proto::{
    CatalogEntry, CatalogGroup, CatalogView, ExerciseLog, HealthNote, HistoryView, Measurement, PlannedExerciseView, SessionSummaryView,
    SessionView, SetLine, StatusView, TimerSignal, View, WorkoutView,
};

pub struct AssistantHandler {
    db: Arc<Mutex<Database>>,
    llm: Box<dyn LlmProvider>,
    config: GymConfig,
    catalogue: Vec<ExerciseTypeWithAncestry>,
    issue_reporter: Option<Arc<dyn IssueReporter>>,
}

/// A handled turn: the domain [`View`] to render plus an optional rest-timer
/// directive that rides along with it. The Telegram path arms its timer server-side
/// from `timer`; the confide path forwards it to the client, which runs the
/// countdown locally.
pub struct Reply {
    pub view: View,
    pub timer: Option<TimerSignal>,
}

impl Reply {
    fn view(view: View) -> Self {
        Self { view, timer: None }
    }
}

impl From<View> for Reply {
    fn from(view: View) -> Self {
        Self::view(view)
    }
}

/// Result of executing a single [`AssistantAction`]: an optional reply suffix
/// (set-count checkpoint, close pushback, …) and an optional rest-timer directive
/// (arm after a logged set, cancel after closing/ending).
#[derive(Default)]
struct ActionOutcome {
    suffix: Option<String>,
    timer: Option<TimerSignal>,
}

impl ActionOutcome {
    fn none() -> Self {
        Self::default()
    }

    fn cancel() -> Self {
        Self { suffix: None, timer: Some(TimerSignal::Cancel) }
    }
}

impl From<Option<String>> for ActionOutcome {
    fn from(suffix: Option<String>) -> Self {
        Self { suffix, timer: None }
    }
}

/// Outcome of resolving which `exercise_entry` a logged set should join.
enum LogEntryTarget {
    /// Insert the set into this entry id (an exact-match open entry, or a freshly
    /// created one).
    Use(i64),
    /// An open entry exists for a taxonomy-related exercise. The set is not
    /// logged; the host asks the user whether they meant that ongoing entry or
    /// are supersetting a separate exercise.
    AskSuperset { ongoing_exercise: String },
}

/// Outcome of a close-entry action. Distinguishes a real close (the entry ended,
/// so the rest timer should be canceled) from the premature-close pushback (the
/// entry is still open and the user is still resting, so the timer keeps running).
enum CloseOutcome {
    /// The entry was ended.
    Closed,
    /// Fewer than 3 sets and unconfirmed: entry left open, carrying this pushback
    /// suffix.
    Pushback(String),
}

impl CloseOutcome {
    /// Map to the action outcome: a real close cancels the timer; a pushback only
    /// surfaces its suffix and leaves any in-flight rest running.
    fn into_action_outcome(self) -> ActionOutcome {
        match self {
            CloseOutcome::Closed => ActionOutcome::cancel(),
            CloseOutcome::Pushback(suffix) => ActionOutcome::from(Some(suffix)),
        }
    }
}

/// The resolved destination for a log-set action, produced by
/// [`AssistantHandler::resolve_log_target`]: either a ready entry, or a superset
/// disambiguation prompt (in which case nothing is logged this turn).
enum LogTarget {
    Ready(LogReady),
    AskSuperset(String),
}

/// Which entry, in which session, for which exercise type a logged set lands on.
struct LogReady {
    exercise_type_id: i64,
    exercise_name: String,
    session: Session,
    entry_id: i64,
}

impl AssistantHandler {
    pub async fn new(db: Arc<Mutex<Database>>, llm: Box<dyn LlmProvider>, config: GymConfig) -> anyhow::Result<Self> {
        Self::new_with_reporter(db, llm, config, None).await
    }

    pub async fn new_with_reporter(
        db: Arc<Mutex<Database>>,
        llm: Box<dyn LlmProvider>,
        config: GymConfig,
        issue_reporter: Option<Arc<dyn IssueReporter>>,
    ) -> anyhow::Result<Self> {
        let catalogue = db.lock().await.list_exercise_types_with_ancestry()?;
        Ok(Self { db, llm, config, catalogue, issue_reporter })
    }

    /// Process an incoming Telegram text message and return a reply.
    pub async fn handle_text_message(&self, message: &TgMessage, text: &str) -> anyhow::Result<Reply> {
        let (user, is_new) = self.ensure_user(message).await?;
        if is_new {
            return Ok(View::notice(self.welcome_message(&user)).into());
        }
        self.handle_message_for_user(&user, text, "telegram").await
    }

    pub async fn handle_message_for_user(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Reply> {
        if let Some(reply) = self.handle_command(text, user, platform).await? {
            return Ok(reply);
        }

        // A `/philosophy` interview in progress consumes free text through the
        // interviewer prompt. Slash commands above (including `/cancel`) still
        // work; this returns `None` when the user is not interviewing.
        if let Some(reply) = self.maybe_handle_interview_mode(user, text, platform).await? {
            return Ok(reply.into());
        }

        self.close_stale_session(user).await?;

        let text = crate::text::truncate_on_char_boundary(text, self.config.max_message_length);

        if let Some(reply) = self.maybe_session_continuity_short_circuit(user, text, platform).await? {
            return Ok(reply.into());
        }

        if let Some(reply) = self.maybe_session_continuity_resume(user, text, platform).await? {
            return Ok(reply);
        }

        let system_prompt = self.build_context(user).await?;

        let history = {
            let db = self.db.lock().await;
            db.get_recent_messages_for_platform(user.id, platform, self.config.conversation_history_limit)?
        };

        let llm_response = match self.call_llm(&system_prompt, &history, text).await {
            Ok(response) => response,
            Err(e) => {
                let err_msg = format!("{e:#}");
                tracing::error!("LLM call failed: {err_msg}");
                let error_reply = if err_msg.contains("401") || err_msg.contains("Unauthorized") || err_msg.contains("Authentication") {
                    "I could not access the AI engine. You'll need to check that I'm properly configured with a valid API key."
                } else {
                    "I had trouble processing that -- could you try again?"
                };
                self.store_excluded_conversation_on_platform(user.id, platform, text, error_reply).await?;
                return Ok(View::notice(error_reply).into());
            }
        };

        let parsed = parse_assistant_response(&llm_response);

        let is_refusal = is_refusal_response(&parsed.message);
        if is_refusal {
            tracing::info!("LLM response detected as refusal, excluding from context");
        }

        let mut failures: Vec<String> = Vec::new();
        let mut suffixes: Vec<String> = Vec::new();
        // The last action that touches the timer wins (e.g. logging then ending a
        // session in one turn ends with the cancel).
        let mut timer: Option<TimerSignal> = None;
        for action in &parsed.actions {
            match self.execute_action(action, user).await {
                Ok(outcome) => {
                    if let Some(suffix) = outcome.suffix {
                        suffixes.push(suffix);
                    }
                    if outcome.timer.is_some() {
                        timer = outcome.timer;
                    }
                }
                Err(e) => {
                    tracing::warn!("Action execution failed: {e:#}");
                    failures.push(format!("{e:#}"));
                }
            }
        }

        if is_refusal {
            self.store_excluded_conversation_on_platform(user.id, platform, text, &llm_response).await?;
        } else {
            self.store_conversation_on_platform(user.id, platform, text, &llm_response).await?;
        }

        // Prune the platform we just wrote to, matching the per-platform read path.
        // Retain twice the read window (`conversation_history_limit`): the read filters
        // out `exclude_from_context` rows while pruning keeps rows by recency alone, so
        // the extra headroom stops excluded turns from shrinking the visible history.
        self.db.lock().await.prune_old_messages_for_platform(user.id, platform, self.config.conversation_history_limit * 2)?;

        // The conversational follow-ups (`suffixes`) and any action `failures` ride
        // alongside the prose as structured `notes`/`failures`; each client decides
        // how to present them. `strip_markdown` keeps stray markup out of chat boxes.
        Ok(Reply { view: View::Message { text: crate::text::strip_markdown(&parsed.message), notes: suffixes, failures }, timer })
    }

    async fn ensure_user(&self, message: &TgMessage) -> anyhow::Result<(User, bool)> {
        let from = message.from.as_ref().ok_or_else(|| anyhow::anyhow!("message has no sender"))?;
        let telegram_id = from.id.to_string();

        let db = self.db.lock().await;
        if let Some(user) = db.get_user_by_telegram_id(&telegram_id)? {
            return Ok((user, false));
        }

        let name = match &from.last_name {
            Some(last) => format!("{} {last}", from.first_name),
            None => from.first_name.clone(),
        };
        let mut draft = new_user(&name, Some(&telegram_id), &self.config.default_timezone);
        draft.timers_enabled = self.config.rest_timer.default_enabled;
        let user_id = db.insert_user(&draft)?;
        let user = db.get_user(user_id)?.context("user disappeared after insert")?;
        tracing::info!("Registered new user: {} (telegram_id: {telegram_id})", user.name);
        Ok((user, true))
    }

    /// Resolve a confide peer's public key (hex) to a registered user. Unlike
    /// [`Self::ensure_user`], this does **not** auto-insert — registration over
    /// confide is explicit (the client sends a `Register` request first).
    pub async fn ensure_user_by_pubkey(&self, pubkey: &str) -> anyhow::Result<Option<User>> {
        self.db.lock().await.get_user_by_pubkey(pubkey)
    }

    /// Register a new user identified by a confide public key (hex). Returns an
    /// error if the pubkey is already registered.
    pub async fn register_user(&self, pubkey: &str, name: &str, timezone: &str) -> anyhow::Result<User> {
        let db = self.db.lock().await;
        anyhow::ensure!(db.get_user_by_pubkey(pubkey)?.is_none(), "pubkey already registered");
        let mut draft = new_user_with_pubkey(name, pubkey, timezone);
        draft.timers_enabled = self.config.rest_timer.default_enabled;
        let user_id = db.insert_user(&draft)?;
        let user = db.get_user(user_id)?.context("user disappeared after insert")?;
        tracing::info!("Registered new user: {} (pubkey: {pubkey})", user.name);
        Ok(user)
    }

    fn welcome_message(&self, user: &User) -> String {
        format!(
            "Welcome, {}! I'm your personal gym trainer assistant.\n\n\
             Here's what I can do:\n\
             - Track your exercises (just tell me what you did)\n\
             - Manage workout sessions\n\
             - Track injuries and health issues\n\
             - Set and monitor exercise goals\n\
             - Show your workout history\n\n\
             Try telling me something like:\n\
             \"I just did 3 sets of bench press at 80kg, 8 reps\"\n\n\
             Type /help for a list of commands.",
            user.name
        )
    }

    /// Handle a slash command, if `text` is one. Most commands map straight to a
    /// [`View`]; `/timers` additionally rides a [`TimerSignal`] so disabling it can
    /// cancel an in-flight rest, hence the [`Reply`] return.
    ///
    /// The match is exhaustive over [`Command`], so a new row in the command table
    /// cannot be advertised without also being handled here.
    async fn handle_command(&self, text: &str, user: &User, platform: &str) -> anyhow::Result<Option<Reply>> {
        let Some(command) = Command::parse(text) else {
            return Ok(None);
        };
        match command {
            Command::Start => Ok(Some(View::notice(Self::cmd_start(user)).into())),
            Command::Help => Ok(Some(View::notice(Self::cmd_help(user)).into())),
            Command::Status => Ok(Some(self.cmd_status(user).await?.into())),
            Command::History => Ok(Some(View::History(self.cmd_history(user).await?).into())),
            Command::Exercises => Ok(Some(View::Catalog(self.cmd_exercises()).into())),
            Command::Clear => Ok(Some(View::notice(self.cmd_clear(user, platform).await?).into())),
            Command::Timers => Ok(Some(self.cmd_timers(user).await?)),
            Command::Philosophy => Ok(Some(self.cmd_philosophy_start(user, platform).await?.into())),
            Command::NextWorkout => Ok(Some(self.cmd_next_workout(user, text).await?.into())),
            Command::Cancel => Ok(Some(self.cmd_cancel(user, platform).await?.into())),
            Command::Feedback => Ok(self.cmd_feedback(user, text).await?.map(Into::into)),
        }
    }

    /// Toggle the user's rest-timer preference and report the new state. As a user
    /// preference it persists across sessions, so it works with or without an active
    /// workout and survives ending one and starting the next. Disabling also cancels
    /// any rest already counting down; enabling has nothing to arm until the next set.
    async fn cmd_timers(&self, user: &User) -> anyhow::Result<Reply> {
        let enabled = self.db.lock().await.set_user_timers(user.id, !user.timers_enabled)?;
        let timer = (!enabled).then_some(TimerSignal::Cancel);
        Ok(Reply { view: View::Timers { enabled }, timer })
    }

    /// Enter the multi-turn `/philosophy` interview and return the opening question.
    /// Turn 0 is a fixed prompt (no LLM call); subsequent free-text turns are routed
    /// through [`Self::philosophy_interview_turn`]. The opener is stored as
    /// conversation so the interview thread stays coherent.
    async fn cmd_philosophy_start(&self, user: &User, platform: &str) -> anyhow::Result<View> {
        let existing = {
            let db = self.db.lock().await;
            db.set_interview_state(user.id, platform, "philosophy", "", 0)?;
            db.latest_philosophy(user.id)?
        };

        let opener = match existing {
            Some(p) => format!(
                "Let's refine your training philosophy. Here's what I have on file:\n\n\
                 \"{}\"\n\n\
                 What would you like to change or add? (or /cancel to keep it as is)",
                p.content
            ),
            None => "Let's build your training philosophy together -- it'll guide every workout I design for you.\n\n\
                 To start: how often do you want to train each week, and what's your main goal \
                 (building muscle, strength, cardio, general fitness, something else)?"
                .to_string(),
        };

        self.store_conversation_on_platform(user.id, platform, "/philosophy", &opener).await?;
        Ok(View::message(opener))
    }

    /// Cancel an in-progress interview (e.g. `/philosophy`). A no-op notice when the
    /// user is not interviewing.
    async fn cmd_cancel(&self, user: &User, platform: &str) -> anyhow::Result<View> {
        let db = self.db.lock().await;
        match db.get_interview_state(user.id, platform)? {
            Some(_) => {
                db.clear_interview_state(user.id, platform)?;
                Ok(View::notice("Cancelled -- your workout philosophy is unchanged."))
            }
            None => Ok(View::notice("Nothing to cancel.")),
        }
    }

    /// Route free text through the interviewer prompt while a `/philosophy`
    /// interview is in progress. Returns `None` when the user is not interviewing,
    /// so normal log/coach handling proceeds.
    async fn maybe_handle_interview_mode(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Option<View>> {
        let state = self.db.lock().await.get_interview_state(user.id, platform)?;
        let Some(state) = state else { return Ok(None) };
        match state.mode.as_str() {
            "philosophy" => Ok(Some(self.philosophy_interview_turn(user, text, platform, &state).await?)),
            other => {
                tracing::warn!("Clearing unknown interview mode {other:?} for user {}", user.id);
                self.db.lock().await.clear_interview_state(user.id, platform)?;
                Ok(None)
            }
        }
    }

    /// One turn of the `/philosophy` interview: build the interviewer prompt from
    /// the draft + active injuries, call the LLM, and either save the distilled
    /// philosophy (on `save_philosophy`) and exit, or ask the next question.
    async fn philosophy_interview_turn(&self, user: &User, text: &str, platform: &str, state: &InterviewState) -> anyhow::Result<View> {
        let (system_prompt, history) = {
            let db = self.db.lock().await;
            let injuries = db.list_active_health_entries(user.id)?;
            let history = db.get_recent_messages_for_platform(user.id, platform, self.config.conversation_history_limit)?;
            (build_philosophy_prompt(&state.draft, &injuries, state.turns), history)
        };

        let llm_response = match self.call_llm(&system_prompt, &history, text).await {
            Ok(response) => response,
            Err(e) => {
                tracing::error!("Philosophy interview LLM call failed: {e:#}");
                self.store_excluded_conversation_on_platform(user.id, platform, text, "interview error").await?;
                return Ok(View::notice("I had trouble with that -- could you say it again? (or /cancel to stop)"));
            }
        };

        let parsed = parse_assistant_response(&llm_response);
        let saved = parsed.actions.iter().find_map(|action| match action {
            AssistantAction::SavePhilosophy { content } => Some(content.clone()),
            _ => None,
        });

        self.store_conversation_on_platform(user.id, platform, text, &llm_response).await?;

        let db = self.db.lock().await;
        if let Some(content) = saved {
            db.insert_philosophy(user.id, &content, "interview")?;
            db.clear_interview_state(user.id, platform)?;
            let message = crate::text::strip_markdown(&parsed.message);
            let confirm = if message.trim().is_empty() {
                "Saved your training philosophy. Try /nextworkout to put it to work.".to_string()
            } else {
                format!("{message}\n\n(Saved -- try /nextworkout to put it to work.)")
            };
            return Ok(View::message(confirm));
        }

        db.set_interview_state(user.id, platform, "philosophy", &state.draft, state.turns + 1)?;
        Ok(View::message(crate::text::strip_markdown(&parsed.message)))
    }

    /// Design a tailored workout from the user's philosophy, recent history, goals,
    /// and injuries, and present it as a [`View::Workout`]. Persists a `proposed`
    /// plan but logs NOTHING and starts no session. Any text after the command
    /// ("/nextworkout but something lighter") is passed to the designer as guidance.
    async fn cmd_next_workout(&self, user: &User, text: &str) -> anyhow::Result<View> {
        let guidance = text.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");

        let (philosophy, sessions, recovery, goals, injuries) = {
            let db = self.db.lock().await;
            (
                db.latest_philosophy(user.id)?,
                db.recent_sessions_with_sets(user.id, 3)?,
                db.muscle_recovery(user.id)?,
                db.goal_progress_report(user.id, None, None)?,
                db.list_active_health_entries(user.id)?,
            )
        };

        // Per spec: with no philosophy on file, offer to build one rather than guess.
        let Some(philosophy) = philosophy else {
            return Ok(View::notice(
                "I don't have a training philosophy for you yet, so I can't tailor a workout. \
                 Run /philosophy and we'll build one together first.",
            ));
        };

        let history_block = self.format_designer_history(&sessions);
        let recovery_block = format_muscle_recovery(&recovery, chrono::Utc::now().date_naive());
        let prompt = build_designer_prompt(&philosophy.content, &history_block, &recovery_block, &goals, &injuries, &self.catalogue);
        let user_text = if guidance.trim().is_empty() { "Design my next workout.".to_string() } else { guidance };

        // The design overruns the default token cap; logging is impossible here
        // because this path never calls execute_action / ensure_session.
        let llm_response = self
            .call_llm_with(&prompt, &[], &user_text, 2048, 0.2)
            .await
            .context("workout designer LLM call failed")?;
        let parsed = parse_assistant_response(&llm_response);

        let proposal = parsed.actions.into_iter().find_map(|action| match action {
            AssistantAction::ProposeWorkout { title, rationale, exercises } => Some((title, rationale, exercises)),
            _ => None,
        });

        let Some((title, rationale, exercises)) = proposal else {
            // No structured plan came back — surface the prose so the turn is not a dead end.
            return Ok(View::message(crate::text::strip_markdown(&parsed.message)));
        };

        self.persist_and_view_workout(user, &philosophy, title, rationale, exercises).await
    }

    /// Persist a designed workout as a `proposed` plan and build its [`View::Workout`].
    /// Exercise names are resolved against the catalogue; unresolved ones are dropped
    /// with a note rather than failing the whole design.
    async fn persist_and_view_workout(
        &self,
        user: &User,
        philosophy: &WorkoutPhilosophy,
        title: String,
        rationale: Option<String>,
        exercises: Vec<ProposedExercise>,
    ) -> anyhow::Result<View> {
        let mut planned: Vec<PlannedExerciseView> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        let db = self.db.lock().await;
        let plan_id = db.create_plan(user.id, &title, rationale.as_deref(), Some(philosophy.id))?;

        for (idx, ex) in exercises.into_iter().enumerate() {
            let Some(et) = find_exercise_type(&self.catalogue, &ex.exercise) else {
                notes.push(format!("Skipped \"{}\" -- I couldn't match it to a known exercise.", ex.exercise));
                continue;
            };
            db.add_plan_exercise(&WorkoutPlanExercise {
                id: 0,
                plan_id,
                exercise_type_id: et.exercise_type.id,
                order_idx: idx as i32,
                target_sets: ex.target_sets,
                target_reps: ex.target_reps,
                target_weight_kg: ex.target_weight_kg,
                target_secs: ex.target_secs,
                notes: ex.notes.clone(),
            })?;
            planned.push(PlannedExerciseView {
                name: et.exercise_type.name.clone(),
                target_sets: ex.target_sets.map(|n| n.max(0) as u32),
                target_reps: ex.target_reps.map(|n| n.max(0) as u32),
                target_weight_kg: ex.target_weight_kg,
                target_secs: ex.target_secs.map(|n| n.max(0) as u32),
                cue: ex.notes,
            });
        }

        Ok(View::Workout(WorkoutView { title, rationale, exercises: planned, notes }))
    }

    /// Format recent sessions (each exercise with its sets and effort) for the
    /// designer prompt, e.g. "Bench Press (60kg x 8 easy, 65kg x 6 medium)".
    fn format_designer_history(&self, sessions: &[SessionWithSets]) -> String {
        if sessions.is_empty() {
            return "RECENT HISTORY: none logged yet.\n".to_string();
        }
        let mut out = "RECENT HISTORY (most recent first):\n".to_string();
        for (session, entries) in sessions {
            out.push_str(&format!("- Session {}:\n", session.started_at));
            for (_entry, sets) in entries {
                let Some(first) = sets.first() else { continue };
                let name = self.exercise_name(first.exercise_type_id);
                let set_descs = sets.iter().map(format_set_desc).collect::<Vec<_>>().join(", ");
                out.push_str(&format!("    {name} ({set_descs})\n"));
            }
        }
        out
    }

    /// The command list both `/start` and `/help` show, one command per line,
    /// filtered to what this user may run and prefixed with `bullet`.
    fn command_list(user: &User, bullet: &str) -> String {
        commands::visible_to(user).map(|spec| format!("{bullet}{}", spec.help_line())).collect::<Vec<_>>().join("\n")
    }

    fn cmd_start(user: &User) -> String {
        format!(
            "You're already registered, {}! Here's what I can help with:\n\
             - Tell me about your exercises and I'll log them\n\
             {}",
            user.name,
            Self::command_list(user, "- ")
        )
    }

    fn cmd_help(user: &User) -> String {
        format!(
            "Available commands:\n\
             {}\n\n\
             You can also just chat naturally:\n\
             - \"3 sets of bench press, 80kg, 8 reps\"\n\
             - \"I ran 5km in 25 minutes\"\n\
             - \"My shoulder is sore\"\n\
             - \"End my session\"\n\
             - \"What did I do today?\"",
            Self::command_list(user, "")
        )
    }

    async fn cmd_status(&self, user: &User) -> anyhow::Result<View> {
        let db = self.db.lock().await;

        let session = match db.get_active_session(user.id)? {
            Some(session) => {
                let mut completed: Vec<ExerciseLog> = Vec::new();
                let mut in_progress: Vec<ExerciseLog> = Vec::new();
                for entry in &db.list_entries_for_session(session.id)? {
                    let sets = db.list_sets_for_entry(entry.id)?;
                    let name = sets.first().map_or_else(|| "unknown".to_string(), |s| self.exercise_name(s.exercise_type_id));
                    let log = ExerciseLog { name, sets: sets.iter().map(set_line).collect() };
                    if entry.end_timestamp.is_some() {
                        completed.push(log);
                    } else {
                        in_progress.push(log);
                    }
                }
                Some(SessionView { started_at: session.started_at, completed, in_progress })
            }
            None => None,
        };

        let health = db
            .list_active_health_entries(user.id)?
            .iter()
            .map(|entry| HealthNote {
                kind: entry.entry_type.as_str().to_string(),
                body_part: entry.body_part.as_deref().unwrap_or("general").to_string(),
                description: entry.description.clone(),
            })
            .collect();

        Ok(View::Status(StatusView { user_name: user.name.clone(), session, health }))
    }

    async fn cmd_history(&self, user: &User) -> anyhow::Result<HistoryView> {
        let db = self.db.lock().await;
        let sessions = db
            .list_session_summaries(user.id, None, None)?
            .iter()
            .map(|summary| SessionSummaryView {
                started_at: summary.session.started_at.clone(),
                status: if summary.session.ended_at.is_some() { "done".into() } else { "active".into() },
                entries: summary.exercise_count.max(0) as u32,
                minutes: summary.duration_mins.map(|d| d.max(0) as u32),
            })
            .collect();
        Ok(HistoryView { sessions })
    }

    fn cmd_exercises(&self) -> CatalogView {
        use super::prompts::capitalize;

        // Pre-sort the loggable rows by (muscle_group, name) so the linear
        // grouping below produces one block per muscle group. The DB query
        // returns rows ordered by (level, name), which would otherwise fragment
        // each group into many tiny blocks.
        let mut sorted: Vec<&ExerciseTypeWithAncestry> = self
            .catalogue
            .iter()
            .filter(|e| matches!(e.exercise_type.level, crate::db::ExerciseLevel::Exercise | crate::db::ExerciseLevel::Variation))
            .collect();
        sorted.sort_by(|a, b| {
            a.muscle_group
                .as_deref()
                .unwrap_or("Other")
                .cmp(b.muscle_group.as_deref().unwrap_or("Other"))
                .then_with(|| a.exercise_type.name.cmp(&b.exercise_type.name))
        });

        let mut groups: Vec<(&str, CatalogGroup)> = Vec::new();
        for et in sorted {
            let group = et.muscle_group.as_deref().unwrap_or("Other");
            let entry = CatalogEntry {
                name: et.exercise_type.name.clone(),
                aliases: et.exercise_type.aliases.as_deref().unwrap_or("").to_string(),
                kind: et.exercise_type.measurement_type.map(|m| m.as_str()).unwrap_or("weight_reps").to_string(),
            };
            match groups.last_mut() {
                Some((raw, cg)) if *raw == group => cg.exercises.push(entry),
                _ => groups.push((group, CatalogGroup { muscle_group: capitalize(group), exercises: vec![entry] })),
            }
        }

        CatalogView { groups: groups.into_iter().map(|(_, cg)| cg).collect() }
    }

    /// Resolve an exercise type's display name from the in-memory catalogue, falling
    /// back to "unknown" when the id is not present.
    fn exercise_name(&self, exercise_type_id: i64) -> String {
        self.catalogue
            .iter()
            .find(|e| e.exercise_type.id == exercise_type_id)
            .map(|e| e.exercise_type.name.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }

    async fn close_stale_session(&self, user: &User) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        let Some(session) = db.get_active_session(user.id)? else {
            return Ok(());
        };

        let entries = db.list_entries_for_session(session.id)?;
        let last_activity = entries.last().map(|e| e.start_timestamp.clone()).unwrap_or_else(|| session.started_at.clone());

        let threshold_hours = self.config.session_timeout_hours as i64;
        if let Ok(last) = chrono::NaiveDateTime::parse_from_str(&last_activity, "%Y-%m-%d %H:%M:%S") {
            let elapsed = Utc::now().naive_utc() - last;
            if elapsed.num_hours() >= threshold_hours {
                tracing::info!("Auto-closing stale session {} (last activity: {last_activity})", session.id);
                db.end_session(session.id)?;
            }
        }

        Ok(())
    }

    async fn build_context(&self, user: &User) -> anyhow::Result<String> {
        let db = self.db.lock().await;
        let active_session = db.get_active_session(user.id)?;
        let mut session_sets: Vec<(ExerciseSet, String)> = Vec::new();
        let mut session_entries: Vec<EntryView> = Vec::new();
        let mut closed_exercise_ids_in_session: Vec<i64> = Vec::new();

        if let Some(session) = &active_session {
            let entries = db.list_entries_for_session(session.id)?;
            for entry in &entries {
                let sets = db.list_sets_for_entry(entry.id)?;
                let exercise_type_id = sets.first().map(|s| s.exercise_type_id);
                let exercise_name = exercise_type_id.map_or_else(|| "unknown".to_string(), |id| self.exercise_name(id));
                let summary_parts: Vec<String> = sets.iter().map(format_set_short).collect();
                session_entries.push(EntryView {
                    id: entry.id,
                    exercise_name: exercise_name.clone(),
                    set_count: sets.len(),
                    sets_summary: summary_parts.join(", "),
                    is_open: entry.end_timestamp.is_none(),
                });
                if entry.end_timestamp.is_some() {
                    if let Some(id) = exercise_type_id {
                        closed_exercise_ids_in_session.push(id);
                    }
                }
                for set in sets {
                    session_sets.push((set, exercise_name.clone()));
                }
            }
        }

        // Active plan, recovered from sentinel-prefixed session.notes
        let active_plan = match active_session.as_ref().and_then(|s| s.notes.as_deref()) {
            Some(notes) => {
                let (plan_name, _rest) = parse_plan_from_notes(Some(notes));
                match plan_name {
                    Some(name) => self.build_active_plan(&db, user.id, &name, &closed_exercise_ids_in_session)?,
                    None => None,
                }
            }
            None => None,
        };

        // A `/nextworkout` design: bound to the current session (guided execution) or
        // freshly designed and ready to start. Sourced from `workout_plans`, distinct
        // from the schedule-based `active_plan` above.
        let active_workout_plan = self.build_active_workout_plan(&db, user.id, active_session.as_ref(), &session_sets)?;

        // Leaked open entries: open EEs not belonging to the active session, OR open
        // entries in the active session (so the LLM can decide to close/delete before
        // a new session is requested).
        let leaked_open_entries = build_leaked_view(&db, &self.catalogue, user.id, active_session.as_ref().map(|s| s.id))?;

        let last_activity_age_hours = match &active_session {
            Some(session) => {
                let age = compute_last_activity_age_hours(&db, session)?;
                tracing::debug!(session_id = session.id, age_hours = age, "computed last_activity_age_hours");
                Some(age)
            }
            None => None,
        };

        let health_entries = db.list_active_health_entries(user.id)?;
        let recent_summaries = db.list_session_summaries(user.id, None, None)?;
        let recent_summaries: Vec<_> = recent_summaries.into_iter().take(5).collect();
        let recent_sets = db.list_recent_sets(user.id, 7)?;
        let session_set_ids: std::collections::HashSet<i64> = session_sets.iter().map(|(s, _)| s.id).collect();
        let recent_sets: Vec<_> = recent_sets.into_iter().filter(|s| !session_set_ids.contains(&s.id)).take(10).collect();
        let active_goals = db.goal_progress_report(user.id, None, None)?;
        let schedules = db.list_schedules(user.id)?;

        let ctx = PromptContext {
            user_name: user.name.clone(),
            timezone: user.timezone.clone(),
            current_time: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            active_session,
            session_sets,
            session_entries,
            leaked_open_entries,
            active_plan,
            active_workout_plan,
            health_entries,
            recent_summaries,
            recent_sets,
            exercise_types: self.catalogue.clone(),
            active_goals,
            schedules,
            last_activity_age_hours,
        };

        Ok(build_system_prompt(&ctx))
    }

    fn build_active_plan(
        &self,
        db: &Database,
        user_id: i64,
        plan_name: &str,
        completed_exercise_ids: &[i64],
    ) -> anyhow::Result<Option<ActivePlanView>> {
        let schedules = db.list_schedules(user_id)?;
        let Some(schedule) = schedules.into_iter().find(|s| s.name.eq_ignore_ascii_case(plan_name)) else {
            return Ok(None);
        };
        let mut planned = db.list_schedule_exercises(schedule.id)?;
        planned.sort_by_key(|p| p.order_idx);
        let next = planned.iter().find(|p| !completed_exercise_ids.contains(&p.exercise_type_id)).map(|p| {
            let exercise_name = self.exercise_name(p.exercise_type_id);
            PlanExerciseView { exercise_name, target_sets: p.target_sets, target_reps: p.target_reps, target_weight_kg: p.target_weight_kg }
        });
        Ok(Some(ActivePlanView { name: schedule.name, completed_exercise_ids: completed_exercise_ids.to_vec(), next }))
    }

    /// Build the guided-execution view of a `/nextworkout` design. Prefers a plan
    /// bound to the active session (guided, in progress); otherwise surfaces the most
    /// recent designed-but-unstarted plan so the assistant can offer to run it.
    fn build_active_workout_plan(
        &self,
        db: &Database,
        user_id: i64,
        active_session: Option<&Session>,
        session_sets: &[(ExerciseSet, String)],
    ) -> anyhow::Result<Option<WorkoutPlanProgress>> {
        // Guided execution: a plan bound to the CURRENT session.
        if let Some(session) = active_session
            && let Some(plan) = db.active_plan_for_user(user_id)?
            && plan.session_id == Some(session.id)
        {
            let logged: std::collections::HashSet<i64> = session_sets.iter().map(|(s, _)| s.exercise_type_id).collect();
            return Ok(Some(self.workout_plan_progress(db, &plan, &logged, true)?));
        }

        // Otherwise a freshly designed plan ready to start — but only while it is
        // recent, so a stale design does not resurface as "ready" days later.
        if let Some(plan) = db.latest_proposed_plan(user_id)?
            && proposed_plan_within_window(&plan.created_at, Utc::now().naive_utc())
        {
            return Ok(Some(self.workout_plan_progress(db, &plan, &std::collections::HashSet::new(), false)?));
        }

        Ok(None)
    }

    /// Compute done/next/remaining for `plan`, treating any prescribed exercise whose
    /// type appears in `logged` as done.
    fn workout_plan_progress(
        &self,
        db: &Database,
        plan: &crate::db::WorkoutPlan,
        logged: &std::collections::HashSet<i64>,
        started: bool,
    ) -> anyhow::Result<WorkoutPlanProgress> {
        let exercises = db.list_plan_exercises(plan.id)?;
        let mut done = Vec::new();
        let mut next = None;
        for ex in &exercises {
            let name = self.exercise_name(ex.exercise_type_id);
            if logged.contains(&ex.exercise_type_id) {
                done.push(name);
            } else if next.is_none() {
                next = Some(PrescribedExercise {
                    exercise_name: name,
                    target_sets: ex.target_sets,
                    target_reps: ex.target_reps,
                    target_weight_kg: ex.target_weight_kg,
                    target_secs: ex.target_secs,
                    notes: ex.notes.clone(),
                });
            }
        }
        let remaining = exercises.len().saturating_sub(done.len());
        Ok(WorkoutPlanProgress { title: plan.title.clone(), started, done, next, remaining, override_note: plan.override_note.clone() })
    }

    async fn call_llm(&self, system_prompt: &str, history: &[crate::db::ConversationMessage], user_text: &str) -> anyhow::Result<String> {
        self.call_llm_with(system_prompt, history, user_text, 1024, 0.1).await
    }

    /// Like [`Self::call_llm`] but with an explicit token cap and temperature. Used
    /// by `/nextworkout`, whose multi-exercise design overruns the default 1024-token
    /// budget.
    async fn call_llm_with(
        &self,
        system_prompt: &str,
        history: &[crate::db::ConversationMessage],
        user_text: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> anyhow::Result<String> {
        let mut messages = vec![LlmMessage { role: LlmRole::System, content: system_prompt.to_string() }];

        for msg in history {
            let role = match msg.role {
                ConversationRole::User => LlmRole::User,
                ConversationRole::Assistant => LlmRole::Assistant,
                ConversationRole::System => LlmRole::System,
            };
            messages.push(LlmMessage { role, content: msg.content.clone() });
        }

        messages.push(LlmMessage { role: LlmRole::User, content: user_text.to_string() });

        let request =
            LlmRequest { messages, temperature: Some(temperature), max_completion_tokens: Some(max_tokens), json_mode: true };

        let response = self.llm.complete(request).await.context("LLM completion failed")?;
        Ok(response.content)
    }

    /// Execute one action, returning an optional reply suffix (set-count checkpoint,
    /// premature-close pushback, leaked-entry warning) and an optional rest-timer
    /// directive (arm after a logged set, cancel after closing/ending).
    async fn execute_action(&self, action: &AssistantAction, user: &User) -> anyhow::Result<ActionOutcome> {
        tracing::debug!(action = ?action, user_id = user.id, "Executing action");
        match action {
            AssistantAction::LogExercise { exercise, reps, weight_kg, perceived_difficulty, comment, superset } => {
                let target = match self.resolve_log_target(user, exercise, *superset).await? {
                    LogTarget::Ready(t) => t,
                    LogTarget::AskSuperset(prompt) => return Ok(Some(prompt).into()),
                };
                let weight = weight_kg.unwrap_or(0.0);
                let pd = perceived_difficulty.unwrap_or(Difficulty::Medium);
                {
                    let db = self.db.lock().await;
                    let existing = db.list_sets_for_entry(target.entry_id)?.len() as i32;
                    let mut s = new_exercise_set(target.entry_id, target.exercise_type_id, MeasurementType::WeightReps, weight);
                    s.count = *reps;
                    s.order_idx = existing;
                    s.perceived_difficulty = pd;
                    s.comment = comment.clone();
                    db.insert_set(&s)?;
                }
                self.finish_logged_set(user, &target.session, target.entry_id, &target.exercise_name, pd).await
            }
            AssistantAction::LogExerciseTimed { exercise, duration_secs, perceived_difficulty, comment, superset } => {
                let target = match self.resolve_log_target(user, exercise, *superset).await? {
                    LogTarget::Ready(t) => t,
                    LogTarget::AskSuperset(prompt) => return Ok(Some(prompt).into()),
                };
                let pd = perceived_difficulty.unwrap_or(Difficulty::Medium);
                let mut s = new_exercise_set(target.entry_id, target.exercise_type_id, MeasurementType::TimeBased, *duration_secs as f64);
                s.perceived_difficulty = pd;
                s.comment = comment.clone();
                self.db.lock().await.insert_set(&s)?;
                self.finish_logged_set(user, &target.session, target.entry_id, &target.exercise_name, pd).await
            }
            AssistantAction::LogExerciseDistance { exercise, distance_m, duration_secs, perceived_difficulty, comment, superset } => {
                let target = match self.resolve_log_target(user, exercise, *superset).await? {
                    LogTarget::Ready(t) => t,
                    LogTarget::AskSuperset(prompt) => return Ok(Some(prompt).into()),
                };
                let value = distance_m.unwrap_or_else(|| duration_secs.unwrap_or(0) as f64);
                let mt = if distance_m.is_some() { MeasurementType::DistanceBased } else { MeasurementType::TimeBased };
                let pd = perceived_difficulty.unwrap_or(Difficulty::Medium);
                let mut s = new_exercise_set(target.entry_id, target.exercise_type_id, mt, value);
                s.perceived_difficulty = pd;
                s.comment = comment.clone();
                self.db.lock().await.insert_set(&s)?;
                self.finish_logged_set(user, &target.session, target.entry_id, &target.exercise_name, pd).await
            }
            AssistantAction::StartSession { notes, plan } => {
                let db = self.db.lock().await;
                if let Some(active) = db.get_active_session(user.id)? {
                    let open = db.list_open_entries_for_session(active.id)?;
                    if !open.is_empty() {
                        let names = self.entry_exercise_names(&db, &open)?;
                        let suffix = format!(
                            "You still have {n} open exercise {entries} in your active session ({list}). \
                             Want me to close them or delete them before starting a new session?",
                            n = open.len(),
                            entries = if open.len() == 1 { "entry" } else { "entries" },
                            list = names.join(", "),
                        );
                        return Ok(Some(suffix).into());
                    }
                    tracing::debug!("Session already active, skipping start");
                    return Ok(ActionOutcome::none());
                }
                // No active session — clean up any leaked open entries from previously
                // ended sessions before starting fresh.
                drop(db);
                self.silent_close_leaked_entries(user.id).await?;
                let db = self.db.lock().await;
                let combined_notes = combine_plan_with_notes(plan.as_deref(), notes.as_deref());
                let session = db.start_session(user.id, combined_notes.as_deref())?;
                tracing::debug!(id = session.id, plan = ?plan, "Started session");
                // Begin guided execution if the user just designed a workout.
                self.bind_proposed_plan(&db, user.id, session.id)?;
                Ok(ActionOutcome::none())
            }
            AssistantAction::EndSession => {
                let db = self.db.lock().await;
                if let Some(session) = db.get_active_session(user.id)? {
                    tracing::debug!(id = session.id, "Ending session");
                    db.end_session(session.id)?;
                    // Complete a guided workout plan bound to this session.
                    if let Some(plan) = db.active_plan_for_user(user.id)?
                        && plan.session_id == Some(session.id)
                    {
                        db.set_plan_status(plan.id, crate::db::PlanStatus::Completed)?;
                    }
                } else {
                    tracing::debug!("No active session to end");
                }
                // Resting is over once the session ends.
                Ok(ActionOutcome::cancel())
            }
            AssistantAction::CloseExerciseEntry { exercise, entry_id } => {
                Ok(self.close_exercise_entry_action(user, exercise.as_deref(), *entry_id, false).await?.into_action_outcome())
            }
            AssistantAction::ConfirmCloseExerciseEntry { exercise, entry_id } => {
                Ok(self.close_exercise_entry_action(user, exercise.as_deref(), *entry_id, true).await?.into_action_outcome())
            }
            AssistantAction::DeleteExerciseEntry { entry_id } => {
                let db = self.db.lock().await;
                let entry = db.get_entry(*entry_id)?.ok_or_else(|| anyhow::anyhow!("entry {entry_id} not found"))?;
                anyhow::ensure!(entry.user_id == user.id, "entry {entry_id} does not belong to user");
                db.delete_entry(*entry_id)?;
                Ok(ActionOutcome::cancel())
            }
            AssistantAction::CloseAllOpenEntries => {
                let db = self.db.lock().await;
                if let Some(session) = db.get_active_session(user.id)? {
                    let n = db.close_open_entries_for_session(session.id, None)?;
                    tracing::debug!(session_id = session.id, closed = n, "Closed all open entries");
                }
                Ok(ActionOutcome::cancel())
            }
            AssistantAction::LogHealth { entry_type, body_part, severity, description } => {
                let mut entry = new_health_entry(user.id, *entry_type, description);
                entry.body_part = body_part.clone();
                if let Some(sev) = severity {
                    entry.severity = sev.clone();
                }
                tracing::debug!(entry_type = ?entry_type, body_part = ?body_part, severity = ?severity, "Inserting health entry");
                self.db.lock().await.insert_health_entry(&entry)?;
                Ok(ActionOutcome::none())
            }
            AssistantAction::ResolveHealth { description } => {
                let db = self.db.lock().await;
                let entries = db.list_active_health_entries(user.id)?;
                if let Some(entry) = entries.iter().find(|e| e.description.to_lowercase().contains(&description.to_lowercase())) {
                    tracing::debug!(id = entry.id, description = %entry.description, "Resolving health entry");
                    db.resolve_health_entry(entry.id)?;
                } else {
                    tracing::debug!(search = %description, "No matching health entry found to resolve");
                }
                Ok(ActionOutcome::none())
            }
            AssistantAction::SetGoal { exercise, target_value, end_date } => {
                let et = find_exercise_type(&self.catalogue, exercise).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {exercise}"))?;
                let mut goal = new_exercise_goal(user.id, et.exercise_type.id, *target_value);
                goal.end_date = end_date.clone();
                tracing::debug!(exercise = %et.exercise_type.name, target = %target_value, end_date = ?end_date, "Inserting goal");
                self.db.lock().await.insert_goal(&goal)?;
                Ok(ActionOutcome::none())
            }
            AssistantAction::EditSet { exercise, new_exercise, new_reps, new_value, new_difficulty } => Ok(self
                .edit_set_action(user, exercise.as_deref(), new_exercise.as_deref(), *new_reps, *new_value, *new_difficulty)
                .await?
                .into()),
            AssistantAction::GetLastExercise { exercise } => Ok(self.get_last_exercise_action(user, exercise).await?.into()),
            AssistantAction::SavePhilosophy { .. } => {
                // Only meaningful inside the `/philosophy` interview, which applies it
                // directly. Ignore it if the model emits it during normal chat.
                tracing::debug!("Ignoring save_philosophy outside the philosophy interview");
                Ok(ActionOutcome::none())
            }
            AssistantAction::ProposeWorkout { .. } => {
                // Only meaningful for `/nextworkout`, which persists the plan directly
                // and never routes it through here. Ignore it during normal chat.
                tracing::debug!("Ignoring propose_workout outside /nextworkout");
                Ok(ActionOutcome::none())
            }
            AssistantAction::AppendPhilosophyNote { note } => {
                self.db.lock().await.append_philosophy_note(user.id, note)?;
                Ok(Some("Noted for future workouts.".to_string()).into())
            }
            AssistantAction::SetSessionOverride { note } => {
                let db = self.db.lock().await;
                // A one-off attaches to the plan in flight so it expires with that plan
                // and never touches the philosophy. With no plan in flight there is
                // nothing to scope it to, so drop it rather than leak it anywhere durable.
                match db.inflight_plan_for_user(user.id)? {
                    Some(plan) => {
                        db.append_plan_override(plan.id, note)?;
                        Ok(Some("Got it -- just for this workout.".to_string()).into())
                    }
                    None => Ok(Some("There's no workout in progress to apply that to right now.".to_string()).into()),
                }
            }
            AssistantAction::Unknown => {
                tracing::debug!("Ignoring unknown action type from LLM");
                Ok(ActionOutcome::none())
            }
        }
    }

    /// Build the rest-timer arm directive for a just-logged set, or `None` when the
    /// user has timers disabled. A superset (≥2 open entries) gets the flat shorter
    /// rest; otherwise the perceived difficulty sets the duration.
    async fn arm_rest_timer(&self, user: &User, session: &Session, exercise: &str, difficulty: Difficulty) -> anyhow::Result<Option<TimerSignal>> {
        if !user.timers_enabled {
            return Ok(None);
        }
        let is_superset = self.db.lock().await.is_supersetting(session.id)?;
        let duration_secs = self.config.rest_timer.rest_secs_for(Some(difficulty), is_superset);
        Ok(Some(TimerSignal::Arm { duration_secs, exercise: exercise.to_string() }))
    }

    /// Shared trailer for the three log-set actions: the set-count checkpoint suffix
    /// plus the rest-timer arm directive, bundled into the action outcome.
    async fn finish_logged_set(
        &self,
        user: &User,
        session: &Session,
        entry_id: i64,
        exercise: &str,
        difficulty: Difficulty,
    ) -> anyhow::Result<ActionOutcome> {
        let suffix = self.set_count_checkpoint_suffix(entry_id, exercise).await?;
        let timer = self.arm_rest_timer(user, session, exercise, difficulty).await?;
        Ok(ActionOutcome { suffix, timer })
    }

    async fn ensure_session(&self, user: &User) -> anyhow::Result<crate::db::Session> {
        if let Some(session) = self.db.lock().await.get_active_session(user.id)? {
            return Ok(session);
        }
        // Auto-start path: no active session exists, so any open exercise_entries
        // for this user are leaks from a previously-ended session. Close them
        // silently before starting fresh.
        self.silent_close_leaked_entries(user.id).await?;
        let db = self.db.lock().await;
        let session = db.start_session(user.id, None)?;
        // Logging the first prescribed exercise auto-starts the guided workout.
        self.bind_proposed_plan(&db, user.id, session.id)?;
        Ok(session)
    }

    /// After a session starts, bind the most recent designed (proposed) workout to it
    /// so guided execution begins. No-op when there is no proposed plan.
    fn bind_proposed_plan(&self, db: &Database, user_id: i64, session_id: i64) -> anyhow::Result<()> {
        let Some(plan) = db.latest_proposed_plan(user_id)? else {
            return Ok(());
        };
        if proposed_plan_within_window(&plan.created_at, Utc::now().naive_utc()) {
            db.bind_plan_to_session(plan.id, session_id)?;
            tracing::debug!(plan_id = plan.id, session_id, "Bound designed workout to session for guided execution");
        } else {
            // Designed too long ago to silently attach to this session — retire it so
            // it stops being a binding candidate.
            db.set_plan_status(plan.id, crate::db::PlanStatus::Abandoned)?;
            tracing::debug!(plan_id = plan.id, "Abandoned a stale proposed workout instead of binding");
        }
        Ok(())
    }

    /// Shared preamble for the three log-set actions: resolve the exercise type,
    /// ensure an active session, and choose the entry to log into. Yields
    /// [`LogTarget::AskSuperset`] (logging nothing) when the set is an ambiguous
    /// taxonomy relative of an already-open entry.
    async fn resolve_log_target(&self, user: &User, exercise: &str, superset: bool) -> anyhow::Result<LogTarget> {
        let et = find_exercise_type(&self.catalogue, exercise).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {exercise}"))?;
        let session = self.ensure_session(user).await?;
        match self.resolve_entry_for_log(user.id, session.id, et.exercise_type.id, superset).await? {
            LogEntryTarget::AskSuperset { ongoing_exercise } => {
                Ok(LogTarget::AskSuperset(superset_prompt(&ongoing_exercise, &et.exercise_type.name)))
            }
            LogEntryTarget::Use(entry_id) => Ok(LogTarget::Ready(LogReady {
                exercise_type_id: et.exercise_type.id,
                exercise_name: et.exercise_type.name.clone(),
                session,
                entry_id,
            })),
        }
    }

    /// Decide which `exercise_entry` a logged set belongs to.
    ///
    /// 1. An open entry of the **exact** same exercise type is reused.
    /// 2. Otherwise, unless `superset` is set, an open entry for a taxonomy
    ///    ancestor or descendant of the exercise triggers an `AskSuperset`
    ///    prompt — the set is ambiguous and is not logged this turn.
    /// 3. Failing both, a fresh entry is created. Its `start_timestamp` is
    ///    computed once so the first set can share the same `logged_at` value
    ///    (the brief's "same start timestamp as the first set" requirement).
    async fn resolve_entry_for_log(
        &self,
        user_id: i64,
        session_id: i64,
        exercise_type_id: i64,
        superset: bool,
    ) -> anyhow::Result<LogEntryTarget> {
        let db = self.db.lock().await;
        if let Some(open) = db.find_open_entry_for_exercise(user_id, session_id, exercise_type_id)? {
            return Ok(LogEntryTarget::Use(open.id));
        }
        if !superset {
            if let Some((_, related_type_id)) = db.find_open_related_entry(session_id, exercise_type_id)? {
                let ongoing_exercise = self
                    .catalogue
                    .iter()
                    .find(|e| e.exercise_type.id == related_type_id)
                    .map(|e| e.exercise_type.name.clone())
                    .unwrap_or_else(|| "that exercise".to_string());
                return Ok(LogEntryTarget::AskSuperset { ongoing_exercise });
            }
        }
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let entry = new_exercise_entry_at(user_id, Some(session_id), None, &now);
        Ok(LogEntryTarget::Use(db.insert_entry(&entry)?))
    }

    /// Resolve an entry to close (explicit id > exercise-name match > most recent
    /// open in the active session). When `confirm` is false and the resolved entry
    /// has fewer than 3 sets, returns [`CloseOutcome::Pushback`] and leaves the
    /// entry open; otherwise ends it and returns [`CloseOutcome::Closed`].
    async fn close_exercise_entry_action(
        &self,
        user: &User,
        exercise: Option<&str>,
        entry_id: Option<i64>,
        confirm: bool,
    ) -> anyhow::Result<CloseOutcome> {
        let db = self.db.lock().await;
        let active = db.get_active_session(user.id)?;
        let resolved = if let Some(id) = entry_id {
            let entry = db.get_entry(id)?.ok_or_else(|| anyhow::anyhow!("entry {id} not found"))?;
            anyhow::ensure!(entry.user_id == user.id, "entry {id} does not belong to user");
            anyhow::ensure!(entry.end_timestamp.is_none(), "entry {id} is already closed");
            entry
        } else {
            let session = active.as_ref().ok_or_else(|| anyhow::anyhow!("no active session"))?;
            let open = db.list_open_entries_for_session(session.id)?;
            let entry = if let Some(name) = exercise {
                let et = find_exercise_type(&self.catalogue, name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {name}"))?;
                open.into_iter().find(|e| {
                    db.list_sets_for_entry(e.id).map(|sets| sets.iter().any(|s| s.exercise_type_id == et.exercise_type.id)).unwrap_or(false)
                })
            } else {
                open.into_iter().last()
            };
            entry.ok_or_else(|| anyhow::anyhow!("no matching open exercise_entry to close"))?
        };

        let count = db.count_sets_for_entry(resolved.id)?;
        if !confirm && count < 3 {
            let name = entry_exercise_name(&db, &self.catalogue, resolved.id)?;
            let suffix = format!(
                "You've only done {count} {sets} of {name}. You should really push for one more! Should we keep going?",
                sets = if count == 1 { "set" } else { "sets" },
            );
            return Ok(CloseOutcome::Pushback(suffix));
        }
        db.end_entry(resolved.id)?;
        Ok(CloseOutcome::Closed)
    }

    /// Edit a recently-logged set. Resolves the target by recency, optionally
    /// filtered by the named current exercise. A `new_exercise` reclassifies the
    /// whole exercise block; value/reps/difficulty changes apply to the single
    /// most-recent set. Returns a host-built before→after confirmation suffix.
    async fn edit_set_action(
        &self,
        user: &User,
        exercise: Option<&str>,
        new_exercise: Option<&str>,
        new_reps: Option<i32>,
        new_value: Option<f64>,
        new_difficulty: Option<Difficulty>,
    ) -> anyhow::Result<Option<String>> {
        let db = self.db.lock().await;

        let filter_id = match exercise {
            Some(name) => {
                Some(find_exercise_type(&self.catalogue, name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {name}"))?.exercise_type.id)
            }
            None => None,
        };
        let target = db
            .most_recent_set_for_user(user.id, filter_id)?
            .ok_or_else(|| anyhow::anyhow!("I couldn't find a recent set to edit."))?;

        let mut parts: Vec<String> = Vec::new();

        // Exercise change → reclassify the whole exercise block (entry).
        if let Some(new_name) = new_exercise {
            let new_et =
                find_exercise_type(&self.catalogue, new_name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {new_name}"))?;
            match db.reclassify_entry_exercise(target.exercise_entry_id, user.id, &self.catalogue, new_et.exercise_type.id) {
                Ok(outcome) => {
                    let old_name = self
                        .catalogue
                        .iter()
                        .find(|e| e.exercise_type.id == outcome.old_exercise_type_id)
                        .map(|e| e.exercise_type.name.as_str())
                        .unwrap_or("the previous exercise");
                    parts.push(format!(
                        "exercise {old_name} → {} ({} set{})",
                        new_et.exercise_type.name,
                        outcome.sets_updated,
                        if outcome.sets_updated == 1 { "" } else { "s" },
                    ));
                }
                Err(SetEditError::MeasurementTypeMismatch { from, to }) => {
                    return Err(anyhow::anyhow!(
                        "{from} and {to} aren't measured the same way, so I can't just swap them — re-log the set as {to} instead."
                    ));
                }
                Err(e) => return Err(anyhow::anyhow!("{e}")),
            }
        }

        // Value / reps / difficulty change → edit the single most-recent set.
        if new_reps.is_some() || new_value.is_some() || new_difficulty.is_some() {
            let edit = SetEdit {
                exercise_type_id: None,
                count: new_reps.map(Some),
                value: new_value,
                perceived_difficulty: new_difficulty,
                comment: None,
            };
            let outcome = db.edit_set(target.id, user.id, &self.catalogue, &edit).map_err(|e| anyhow::anyhow!("{e}"))?;
            if new_value.is_some() {
                parts.push(format!(
                    "{} {} → {}",
                    outcome.before.measurement_type.value_label(),
                    outcome.before.measurement_type.format_value(outcome.before.value),
                    outcome.after.measurement_type.format_value(outcome.after.value),
                ));
            }
            if new_reps.is_some() {
                parts.push(format!("reps {} → {}", opt_count(outcome.before.count), opt_count(outcome.after.count)));
            }
            if let Some(d) = new_difficulty {
                parts.push(format!("difficulty → {d}"));
            }
        }

        if parts.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!("Updated your last set — {}.", parts.join(", "))))
    }

    /// Look up and render the user's most recent `exercise_entry` for an exercise
    /// named in free text. Tries an exact-type match first; if nothing is logged,
    /// falls back to a descendants-inclusive query so coarse names like "chest"
    /// still resolve to a logged variation. Always user-scoped via SQL.
    async fn get_last_exercise_action(&self, user: &User, exercise: &str) -> anyhow::Result<Option<String>> {
        let Some(et) = find_exercise_type(&self.catalogue, exercise) else {
            return Ok(Some(format!("I don't have \"{exercise}\" in my exercise list.")));
        };
        let asked_name = et.exercise_type.name.clone();
        let db = self.db.lock().await;
        let (entry, fell_back) = match db.most_recent_entry_for_exercise(user.id, et.exercise_type.id, false)? {
            Some(e) => (e, false),
            None => match db.most_recent_entry_for_exercise(user.id, et.exercise_type.id, true)? {
                Some(e) => (e, true),
                None => return Ok(Some(format!("You haven't logged any {asked_name} yet."))),
            },
        };
        let sets = db.list_sets_for_entry(entry.id)?;
        let resolved_name = entry_exercise_name(&db, &self.catalogue, entry.id)?;
        let summary = sets.iter().map(format_set_short).collect::<Vec<_>>().join(", ");
        let suffix = if fell_back && resolved_name != asked_name {
            format!(
                "No direct {asked_name} entries — showing nearest match {resolved_name} from {start} ({n} {sets_word}: {summary}).",
                start = entry.start_timestamp,
                n = sets.len(),
                sets_word = if sets.len() == 1 { "set" } else { "sets" },
            )
        } else {
            format!(
                "Your last {resolved_name} was {start} ({n} {sets_word}: {summary}).",
                start = entry.start_timestamp,
                n = sets.len(),
                sets_word = if sets.len() == 1 { "set" } else { "sets" },
            )
        };
        Ok(Some(suffix))
    }

    /// Set-count checkpoint: every time the user logs a set in an open entry that
    /// already has ≥3 sets total, ask whether they want to keep going or move on.
    async fn set_count_checkpoint_suffix(&self, entry_id: i64, exercise_name: &str) -> anyhow::Result<Option<String>> {
        let count = self.db.lock().await.count_sets_for_entry(entry_id)?;
        if count >= 3 {
            Ok(Some(format!("You've logged {count} sets of {exercise_name}. Want another set, or move to the next exercise?")))
        } else {
            Ok(None)
        }
    }

    /// Close any open exercise_entries that belong to already-ended sessions for
    /// this user. Best-effort: uses the parent session's `ended_at` when present,
    /// otherwise `datetime('now')`.
    async fn silent_close_leaked_entries(&self, user_id: i64) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        let leaks = db.list_open_entries_for_user(user_id)?;
        for entry in leaks {
            let Some(session_id) = entry.session_id else {
                db.end_entry(entry.id)?;
                continue;
            };
            let session = db.get_session(session_id)?;
            match session.and_then(|s| s.ended_at) {
                Some(ended_at) => {
                    db.conn().execute(
                        "UPDATE exercise_entry SET end_timestamp = ?1 WHERE id = ?2 AND end_timestamp IS NULL",
                        rusqlite::params![ended_at, entry.id],
                    )?;
                }
                None => {
                    // session is still active — not a leak; leave it alone.
                }
            }
        }
        Ok(())
    }

    fn entry_exercise_names(&self, db: &Database, entries: &[ExerciseEntry]) -> anyhow::Result<Vec<String>> {
        entries.iter().map(|e| entry_exercise_name(db, &self.catalogue, e.id)).collect()
    }

    async fn cmd_clear(&self, user: &User, platform: &str) -> anyhow::Result<String> {
        let db = self.db.lock().await;
        let excluded = db.exclude_all_messages_for_platform(user.id, platform)?;
        tracing::info!(user_id = user.id, %platform, excluded, "Cleared conversation context");
        Ok("Conversation context cleared. I'll start fresh from here.".to_string())
    }

    /// Handle `/feedback <text>` — file a GitHub issue on behalf of a beta tester.
    ///
    /// Non-beta users return `Ok(None)` so the dispatcher behaves exactly as if
    /// the command did not exist; the message then flows to the LLM path,
    /// preventing the existence of `/feedback` from leaking via a discriminating
    /// "permission denied" error.
    async fn cmd_feedback(&self, user: &User, raw_text: &str) -> anyhow::Result<Option<View>> {
        if !user.beta_tester {
            return Ok(None);
        }
        let Some(reporter) = self.issue_reporter.as_ref() else {
            return Ok(Some(View::notice("Feedback submission isn't configured on this server.")));
        };

        let body_raw = raw_text.strip_prefix("/feedback").or_else(|| raw_text.strip_prefix("/FEEDBACK")).unwrap_or(raw_text);
        let body_raw = body_raw.trim();
        if body_raw.is_empty() {
            return Ok(Some(View::notice("Please include a description, e.g. \"/feedback the bench-press timer never stops\".")));
        }

        let body_capped = crate::text::truncate_on_char_boundary(body_raw, self.config.max_message_length);

        let title = build_feedback_title(body_capped);
        let body = build_feedback_body(user, body_capped);

        match reporter.create_issue(&title, &body).await {
            Ok(url) => {
                tracing::info!(user_id = user.id, %url, "feedback issue filed");
                Ok(Some(View::notice(format!("Filed: {url}"))))
            }
            Err(e) => {
                tracing::error!(user_id = user.id, "feedback issue submission failed: {e:#}");
                Ok(Some(View::notice("Sorry, I couldn't file that right now. Please try again later.")))
            }
        }
    }

    /// Hard server-side enforcement of the SESSION CONTINUITY ask-window. If
    /// there is an active session whose last activity was between 0.5 and 12
    /// hours ago, and we have not already asked the user about it on the
    /// previous turn, reply with a canned question and skip the LLM entirely.
    /// Subsequent user replies (the "yes new" / "no same" answer) flow through
    /// the LLM normally because the gap-window flag flips on the assistant
    /// message we just stored.
    async fn maybe_session_continuity_short_circuit(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Option<View>> {
        let (active_session, age_hours) = {
            let db = self.db.lock().await;
            let Some(session) = db.get_active_session(user.id)? else {
                return Ok(None);
            };
            let age = compute_last_activity_age_hours(&db, &session)?;
            (session, age)
        };
        if !(SESSION_CONTINUITY_ASK_HOURS..SESSION_CONTINUITY_HOURS).contains(&age_hours) {
            return Ok(None);
        }
        // If we already asked on the previous assistant turn, let the LLM
        // process the user's answer.
        let already_asked = {
            let db = self.db.lock().await;
            let recent = db.get_recent_messages_for_platform(user.id, platform, 4)?;
            recent
                .iter()
                .rev()
                .find(|m| m.role == ConversationRole::Assistant)
                .map(|m| contains_continuity_ask(&m.content))
                .unwrap_or(false)
        };
        if already_asked {
            return Ok(None);
        }
        let canned = format!(
            "It's been {age_hours:.1} hours since your last set in this session. \
Before I log \"{text}\", is this a new workout or the same session? Reply \"new \
workout\" to end the previous one and start fresh, or \"same workout\" to keep \
going in the existing session — and I'll log that set accordingly."
        );
        tracing::debug!(session_id = active_session.id, age_hours, "session continuity short-circuit: asking user");
        self.store_conversation_on_platform(user.id, platform, text, &canned).await?;
        Ok(Some(View::message(canned)))
    }

    /// Server-side counterpart to `maybe_session_continuity_short_circuit`: after
    /// we asked the user "new workout or same workout?", their reply needs to
    /// trigger end+start+log (for "new") or just log (for "same"). The small LLM
    /// is unreliable at emitting that compound action, so we do it here:
    ///   * Detect the previous assistant message was the canned ask.
    ///   * Detect the current user message is an affirmation/negation.
    ///   * Extract the original quoted exercise text from the canned ask.
    ///   * For "new": call end_session + start_session, then recurse with the
    ///     original text so the normal LLM path logs it (gap is now 0, no
    ///     short-circuit).
    ///   * For "same": bump the session's started_at to now (so the gap is 0),
    ///     then recurse with the original text.
    async fn maybe_session_continuity_resume(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Option<Reply>> {
        let lowered = text.to_lowercase();
        let is_new = ["new workout", "new session", "yes new", "yes, new"].iter().any(|n| lowered.contains(n))
            || lowered.trim() == "yes"
            || lowered.trim() == "new";
        let is_same = ["same workout", "same session", "continuing", "continue", "no new"].iter().any(|n| lowered.contains(n))
            || lowered.trim() == "same";
        if !is_new && !is_same {
            return Ok(None);
        }
        let (prev_assistant, original_text) = {
            let db = self.db.lock().await;
            let recent = db.get_recent_messages_for_platform(user.id, platform, 6)?;
            // `recent` is oldest-first per `get_recent_messages_for_platform`'s
            // post-reverse. Walk newest-first so we pick the most recent
            // assistant turn (the canned ask) and the user turn that preceded
            // it (the original exercise message).
            let mut iter = recent.into_iter().rev();
            let mut prev_assistant: Option<String> = None;
            let mut original_text: Option<String> = None;
            for msg in iter.by_ref() {
                if msg.role == ConversationRole::Assistant {
                    prev_assistant = Some(msg.content);
                    break;
                }
            }
            for msg in iter {
                if msg.role == ConversationRole::User {
                    original_text = Some(msg.content);
                    break;
                }
            }
            (prev_assistant, original_text)
        };
        let Some(prev_assistant) = prev_assistant else { return Ok(None) };
        if !contains_continuity_ask(&prev_assistant) {
            return Ok(None);
        }
        let Some(quoted) = extract_continuity_quoted_text(&prev_assistant).or(original_text) else {
            return Ok(None);
        };

        if is_new {
            let session_id = {
                let db = self.db.lock().await;
                db.get_active_session(user.id)?.map(|s| s.id)
            };
            if let Some(id) = session_id {
                self.db.lock().await.end_session(id).context("ending session for continuity-new")?;
            }
            let new_id = self.db.lock().await.start_session(user.id, None).context("starting session for continuity-new")?.id;
            tracing::debug!(new_session_id = new_id, %quoted, "session continuity resume: NEW workout");
        } else {
            // "same workout" → reset the session start so the gap is 0 and the
            // short-circuit doesn't re-trigger on this turn. Look up the session
            // id under one lock, then run the UPDATE under a fresh lock — Tokio
            // Mutex guards held inside an `if let` would otherwise deadlock the
            // second `self.db.lock().await`.
            let session_id = {
                let db = self.db.lock().await;
                db.get_active_session(user.id)?.map(|s| s.id)
            };
            if let Some(sid) = session_id {
                let db = self.db.lock().await;
                db.conn()
                    .execute("UPDATE sessions SET started_at = datetime('now') WHERE id = ?1", rusqlite::params![sid])
                    .context("bumping session started_at for continuity-same")?;
            }
            tracing::debug!(%quoted, "session continuity resume: SAME workout");
        }

        // Persist the affirmation so the conversation history is consistent, then
        // recurse with the *original* exercise text. The recursive call goes
        // through the LLM normally; gap is now 0 so no short-circuit fires.
        self.store_conversation_on_platform(user.id, platform, text, "Got it — logging the pending set now.").await?;
        Box::pin(self.handle_message_for_user(user, &quoted, platform)).await.map(Some)
    }

    async fn store_conversation_on_platform(
        &self,
        user_id: i64,
        platform: &str,
        user_text: &str,
        assistant_text: &str,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.insert_message(&new_conversation_message(user_id, platform, ConversationRole::User, user_text))?;
        db.insert_message(&new_conversation_message(user_id, platform, ConversationRole::Assistant, assistant_text))?;
        Ok(())
    }

    async fn store_excluded_conversation_on_platform(
        &self,
        user_id: i64,
        platform: &str,
        user_text: &str,
        assistant_text: &str,
    ) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        let mut user_msg = new_conversation_message(user_id, platform, ConversationRole::User, user_text);
        user_msg.exclude_from_context = true;
        db.insert_message(&user_msg)?;
        let mut assistant_msg = new_conversation_message(user_id, platform, ConversationRole::Assistant, assistant_text);
        assistant_msg.exclude_from_context = true;
        db.insert_message(&assistant_msg)?;
        Ok(())
    }
}

/// One logged set as the designer prompt reads it, e.g. "60kg x 8 easy" or
/// "60s hard". Effort is the perceived difficulty so the designer can progress or
/// back off the load.
fn format_set_desc(set: &ExerciseSet) -> String {
    let effort = set.perceived_difficulty.as_str();
    let measure = set.measurement_type.describe_value(set.value);
    match (set.measurement_type, set.count) {
        (MeasurementType::WeightReps, Some(reps)) => format!("{measure} x {reps} {effort}"),
        _ => format!("{measure} {effort}"),
    }
}

/// Detect LLM refusal responses that indicate the message was off-topic or blocked.
fn is_refusal_response(text: &str) -> bool {
    let lower = text.to_lowercase();
    const REFUSAL_PATTERNS: &[&str] = &[
        "i cannot provide",
        "i can't provide",
        "i cannot help with",
        "i can't help with",
        "i'm not able to",
        "i am not able to",
        "outside my scope",
        "beyond my capabilities",
        "i don't have the ability",
        "not something i can help",
        "i'm unable to",
        "i am unable to",
        "i cannot assist with",
        "i can't assist with",
    ];
    REFUSAL_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Map a DB [`ExerciseSet`] to the wire [`SetLine`] the clients render.
fn set_line(set: &ExerciseSet) -> SetLine {
    let measurement = match set.measurement_type {
        MeasurementType::WeightReps => Measurement::WeightReps,
        MeasurementType::TimeBased => Measurement::TimeBased,
        MeasurementType::DistanceBased => Measurement::DistanceBased,
        MeasurementType::LevelBased => Measurement::LevelBased,
        MeasurementType::ScoreBased => Measurement::ScoreBased,
    };
    SetLine { measurement, count: set.count.map(|c| c.max(0) as u32), value: set.value }
}

const FEEDBACK_TITLE_MAX_CHARS: usize = 80;

fn build_feedback_title(body: &str) -> String {
    let summary: String = body.lines().next().unwrap_or(body).trim().to_string();
    let truncated = if summary.chars().count() <= FEEDBACK_TITLE_MAX_CHARS {
        summary
    } else {
        let mut s: String = summary.chars().take(FEEDBACK_TITLE_MAX_CHARS.saturating_sub(1)).collect();
        s.push('…');
        s
    };
    format!("[corre-gym] {truncated}")
}

fn build_feedback_body(user: &User, body: &str) -> String {
    let tg = user.telegram_id.as_deref().unwrap_or("-");
    format!("{body}\n\n---\nReported by: {name} (telegram_id: {tg}) via /feedback", name = user.name)
}

/// Resolve the exercise an entry "belongs to" via its first set. Returns
/// `"unknown"` if the entry has no sets yet (which only happens transiently
/// before the first insert).
pub(crate) fn entry_exercise_name(db: &Database, catalogue: &[ExerciseTypeWithAncestry], entry_id: i64) -> anyhow::Result<String> {
    let sets = db.list_sets_for_entry(entry_id)?;
    let Some(first) = sets.first() else {
        return Ok("unknown".to_string());
    };
    let name = catalogue
        .iter()
        .find(|e| e.exercise_type.id == first.exercise_type_id)
        .map(|e| e.exercise_type.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    Ok(name)
}

/// Render an optional rep count, falling back to an em dash when absent.
fn opt_count(c: Option<i32>) -> String {
    c.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string())
}

/// Question appended to the assistant's reply when a logged set is a taxonomy
/// relative of an exercise already in progress. The user resolves it on the next
/// turn ("same exercise" → join the ongoing entry; "superset" → log separately).
fn superset_prompt(ongoing_exercise: &str, logged_exercise: &str) -> String {
    format!(
        "You've already got an open {ongoing_exercise} entry going. Should I add this {logged_exercise} set \
         to it, or are you supersetting a separate exercise? Reply \"same exercise\" to add it to \
         {ongoing_exercise}, or \"superset\" to log it on its own."
    )
}

/// Encode an optional plan name into the session's `notes` field using the
/// `plan:<name>` sentinel prefix so the active plan can be recovered later
/// without a schema change.
pub(crate) fn combine_plan_with_notes(plan: Option<&str>, notes: Option<&str>) -> Option<String> {
    match (plan, notes) {
        (Some(p), Some(n)) => Some(format!("plan:{p}\n{n}")),
        (Some(p), None) => Some(format!("plan:{p}")),
        (None, Some(n)) => Some(n.to_string()),
        (None, None) => None,
    }
}

/// Inverse of `combine_plan_with_notes`. Returns `(plan_name, remaining_notes)`.
pub(crate) fn parse_plan_from_notes(notes: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(text) = notes else {
        return (None, None);
    };
    if let Some(rest) = text.strip_prefix("plan:") {
        match rest.split_once('\n') {
            Some((plan, body)) => (Some(plan.trim().to_string()), Some(body.to_string())),
            None => (Some(rest.trim().to_string()), None),
        }
    } else {
        (None, Some(text.to_string()))
    }
}

/// Compact rendering of a single set used inside an entry summary, e.g. "8×80kg",
/// "30s", "5000m". Delegates to the wire [`SetLine::compact`] so the backend's
/// compact form has a single source of truth shared with every client.
pub(crate) fn format_set_short(set: &ExerciseSet) -> String {
    set_line(set).compact()
}

/// Hours since the most-recent set in `session` was logged, falling back to the
/// session's `started_at` if no sets exist yet. Drives the SESSION CONTINUITY
/// rule in the system prompt. Timestamp parse failures are non-fatal and yield
/// `0.0` so the prompt simply omits the cutoff guidance for that turn.
fn compute_last_activity_age_hours(db: &Database, session: &Session) -> anyhow::Result<f64> {
    let mut latest = parse_sqlite_datetime(&session.started_at);
    for entry in db.list_entries_for_session(session.id)? {
        for set in db.list_sets_for_entry(entry.id)? {
            if let Some(t) = parse_sqlite_datetime(&set.logged_at) {
                latest = match latest {
                    Some(prev) if prev >= t => Some(prev),
                    _ => Some(t),
                };
            }
        }
    }
    let Some(latest) = latest else { return Ok(0.0) };
    let now = Utc::now().naive_utc();
    Ok((now - latest).num_seconds().max(0) as f64 / 3600.0)
}

fn parse_sqlite_datetime(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()
}

/// How long a designed-but-unstarted `/nextworkout` plan stays eligible to bind to a
/// new session. After this it is stale: a week-old design must not silently attach to
/// an unrelated workout the user happens to start.
const PROPOSED_PLAN_BIND_WINDOW_HOURS: i64 = 12;

/// Whether a `proposed` plan created at `created_at` is still recent enough to bind to
/// a session. An unparseable timestamp fails open (treated as in-window — we just
/// wrote it), matching `compute_last_activity_age_hours`'s lenient parsing.
fn proposed_plan_within_window(created_at: &str, now: NaiveDateTime) -> bool {
    match parse_sqlite_datetime(created_at) {
        Some(ts) => (now - ts).num_hours() < PROPOSED_PLAN_BIND_WINDOW_HOURS,
        None => true,
    }
}

/// Loose match for "did the previous assistant turn ask the session-continuity
/// question?" — used to avoid asking twice in a row. Mirrors the regex set in
/// `e2e/.../assertions::reply_asks_about_new_session`.
fn contains_continuity_ask(reply: &str) -> bool {
    let lower = reply.to_lowercase();
    ["new workout", "same workout", "picking up", "pick up where", "is this a new"].iter().any(|n| lower.contains(n))
}

/// Extract the verbatim exercise text from the canned continuity ask. The host
/// formats it as `Before I log "<TEXT>", is this a new workout ...`, so we
/// recover the contents between the first pair of double quotes.
fn extract_continuity_quoted_text(reply: &str) -> Option<String> {
    let start = reply.find('"')?;
    let rest = &reply[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Build EntryView rows for any open exercise_entries the user has, so the prompt
/// (and LLM-driven cleanup logic) can see them. When `active_session_id` is given,
/// only entries inside that session are reported (the caller's contract: leaks are
/// what blocks a *new* session, not what's normal in the current one).
fn build_leaked_view(
    db: &Database,
    catalogue: &[ExerciseTypeWithAncestry],
    user_id: i64,
    active_session_id: Option<i64>,
) -> anyhow::Result<Vec<EntryView>> {
    let all_open = db.list_open_entries_for_user(user_id)?;
    let filtered: Vec<_> = all_open
        .into_iter()
        .filter(|e| match active_session_id {
            Some(sid) => e.session_id == Some(sid),
            None => true,
        })
        .collect();
    let mut views = Vec::with_capacity(filtered.len());
    for entry in filtered {
        let sets = db.list_sets_for_entry(entry.id)?;
        let exercise_name = sets
            .first()
            .and_then(|s| catalogue.iter().find(|e| e.exercise_type.id == s.exercise_type_id))
            .map(|e| e.exercise_type.name.clone())
            .unwrap_or_else(|| "unknown".to_string());
        views.push(EntryView {
            id: entry.id,
            exercise_name,
            set_count: sets.len(),
            sets_summary: sets.iter().map(format_set_short).collect::<Vec<_>>().join(", "),
            is_open: true,
        });
    }
    Ok(views)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::render::Telegram;
    use corre_core::app::{LlmRequest, LlmResponse};
    use gymbuddy_proto::Render as _;

    /// The text a Telegram user would see for a reply — lets these tests keep
    /// asserting on rendered output after the move to the domain `View` model.
    fn shown(reply: &Reply) -> String {
        Telegram.render(&reply.view).0
    }

    struct MockLlm {
        response: std::sync::Mutex<String>,
        recorded: std::sync::Mutex<Vec<LlmRequest>>,
    }

    impl MockLlm {
        fn new(response: &str) -> Self {
            Self { response: std::sync::Mutex::new(response.to_string()), recorded: std::sync::Mutex::new(Vec::new()) }
        }

        fn set_response(&self, response: &str) {
            *self.response.lock().unwrap() = response.to_string();
        }

        fn recorded_requests(&self) -> Vec<LlmRequest> {
            self.recorded.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockLlm {
        async fn complete(&self, request: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.recorded.lock().unwrap().push(request);
            Ok(LlmResponse { content: self.response.lock().unwrap().clone() })
        }
    }

    fn make_message(user_id: i64, text: &str) -> TgMessage {
        TgMessage {
            message_id: 1,
            from: Some(crate::telegram::TelegramUser {
                id: user_id,
                first_name: "Test".to_string(),
                last_name: Some("User".to_string()),
                username: Some("testuser".to_string()),
            }),
            chat: crate::telegram::Chat { id: user_id, chat_type: "private".to_string() },
            date: 0,
            text: Some(text.to_string()),
            voice: None,
            audio: None,
        }
    }

    fn test_config() -> GymConfig {
        GymConfig {
            telegram_bot_token: Some("123456:ABC".to_string()),
            telegram_allowed_ids: vec![],
            default_timezone: "UTC".to_string(),
            conversation_history_limit: 20,
            db_path: "test.db".to_string(),
            max_message_length: 2000,
            session_timeout_hours: 4,
            llm: None,
            voice: None,
            github: None,
            confide: None,
            rest_timer: crate::config::RestTimerConfig::default(),
        }
    }

    async fn setup_handler(response: &str) -> (AssistantHandler, Arc<MockLlm>) {
        setup_handler_with_reporter(response, None).await
    }

    async fn setup_handler_with_reporter(
        response: &str,
        reporter: Option<Arc<dyn IssueReporter>>,
    ) -> (AssistantHandler, Arc<MockLlm>) {
        let db = Database::open_in_memory().unwrap();
        let db = Arc::new(Mutex::new(db));
        let llm = Arc::new(MockLlm::new(response));
        let handler = AssistantHandler::new_with_reporter(db, Box::new(MockLlmWrapper(llm.clone())), test_config(), reporter)
            .await
            .unwrap();
        (handler, llm)
    }

    struct MockIssueReporter {
        calls: std::sync::Mutex<Vec<(String, String)>>,
        result: std::sync::Mutex<Result<String, String>>,
    }

    impl MockIssueReporter {
        fn ok(url: &str) -> Arc<Self> {
            Arc::new(Self { calls: std::sync::Mutex::new(Vec::new()), result: std::sync::Mutex::new(Ok(url.to_string())) })
        }

        fn err(msg: &str) -> Arc<Self> {
            Arc::new(Self { calls: std::sync::Mutex::new(Vec::new()), result: std::sync::Mutex::new(Err(msg.to_string())) })
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn last_call(&self) -> Option<(String, String)> {
            self.calls.lock().unwrap().last().cloned()
        }
    }

    #[async_trait::async_trait]
    impl IssueReporter for MockIssueReporter {
        async fn create_issue(&self, title: &str, body: &str) -> anyhow::Result<String> {
            self.calls.lock().unwrap().push((title.to_string(), body.to_string()));
            match self.result.lock().unwrap().clone() {
                Ok(url) => Ok(url),
                Err(msg) => Err(anyhow::anyhow!(msg)),
            }
        }
    }

    async fn promote_to_beta(handler: &AssistantHandler, telegram_id: &str) {
        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id(telegram_id).unwrap().unwrap();
        db.set_beta_tester(user.id, true).unwrap();
    }

    struct MockLlmWrapper(Arc<MockLlm>);

    #[async_trait::async_trait]
    impl LlmProvider for MockLlmWrapper {
        async fn complete(&self, request: LlmRequest) -> anyhow::Result<LlmResponse> {
            self.0.complete(request).await
        }
    }

    #[tokio::test]
    async fn user_auto_registration() {
        let (handler, _) = setup_handler(r#"{"message": "Hello!", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let reply = handler.handle_text_message(&msg, "hello").await.unwrap();
        assert!(shown(&reply).contains("Welcome"));
    }

    #[tokio::test]
    async fn existing_user_gets_llm_response() {
        let (handler, _) = setup_handler(r#"{"message": "Got it!", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "how are you").await.unwrap();
        assert_eq!(shown(&reply), "Got it!");
    }

    #[tokio::test]
    async fn exercise_logging_creates_records() {
        let response = r#"{"message": "Logged your bench press!", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
        ]}"#;
        let (handler, _) = setup_handler(response).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "3 sets bench 80kg 8 reps").await.unwrap();
        assert!(shown(&reply).starts_with("Logged your bench press!"));
        assert!(shown(&reply).contains("You've logged 3 sets of Bench Press. Want another set, or move to the next exercise?"));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1);
        let sets = db.list_sets_for_entry(entries[0].id).unwrap();
        assert_eq!(sets.len(), 3);
        assert!(sets.iter().all(|s| s.count == Some(8) && (s.value - 80.0).abs() < 1e-6));
        assert!(entries[0].end_timestamp.is_none(), "entry should still be open");
    }

    /// Register a user and log a Flat Barbell Bench Press set, opening one entry.
    async fn open_variation_entry(handler: &AssistantHandler, llm: &MockLlm, msg: &TgMessage) {
        let _ = handler.handle_text_message(msg, "hello").await.unwrap();
        llm.set_response(
            r#"{"message": "Logged it!", "actions": [
                {"type": "log_exercise", "exercise": "Flat Barbell Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(msg, "flat barbell bench press 80kg 8 reps medium").await.unwrap();
    }

    #[tokio::test]
    async fn related_exercise_log_prompts_for_superset() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        open_variation_entry(&handler, &llm, &msg).await;

        // Logging the parent exercise (a taxonomy ancestor of the open entry) is ambiguous.
        llm.set_response(
            r#"{"message": "Sure.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "bench press 80kg 8 reps medium").await.unwrap();
        assert!(shown(&reply).contains("supersetting"), "expected a superset question, got: {}", shown(&reply));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1, "ambiguous set must not open a new entry");
        assert_eq!(db.count_sets_for_entry(entries[0].id).unwrap(), 1, "ambiguous set must not be inserted");
    }

    #[tokio::test]
    async fn superset_flag_logs_parallel_entry() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        open_variation_entry(&handler, &llm, &msg).await;

        // `superset: true` asserts a deliberate parallel exercise — log without asking.
        llm.set_response(
            r#"{"message": "Logged the superset.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium", "superset": true}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "actually superset, log bench press").await.unwrap();
        assert!(!shown(&reply).contains("supersetting"), "superset flag should suppress the question");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let open = db.list_open_entries_for_session(session.id).unwrap();
        assert_eq!(open.len(), 2, "the superset must open a second parallel entry");
        let total: i64 = open.iter().map(|e| db.count_sets_for_entry(e.id).unwrap()).sum();
        assert_eq!(total, 2, "both sets must be logged");
    }

    #[tokio::test]
    async fn same_exercise_resolution_groups_in() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        open_variation_entry(&handler, &llm, &msg).await;

        // Ambiguous log triggers the prompt.
        llm.set_response(
            r#"{"message": "Sure.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "bench press 80kg 8 reps medium").await.unwrap();

        // "same exercise" → re-emit against the exact ongoing exercise name.
        llm.set_response(
            r#"{"message": "Added it.", "actions": [
                {"type": "log_exercise", "exercise": "Flat Barbell Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "same exercise").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1, "the set must join the existing entry, not open a new one");
        assert_eq!(db.count_sets_for_entry(entries[0].id).unwrap(), 2);
    }

    #[tokio::test]
    async fn unrelated_superset_logs_without_prompt() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        llm.set_response(
            r#"{"message": "Logged it!", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "bench press 80kg 8 reps medium").await.unwrap();

        // Squat is a different taxonomy branch — a genuine superset, never ambiguous.
        llm.set_response(
            r#"{"message": "Logged it!", "actions": [
                {"type": "log_exercise", "exercise": "Squat", "reps": 5, "weight_kg": 100.0, "perceived_difficulty": "hard"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "squat 100kg 5 reps hard").await.unwrap();
        assert!(!shown(&reply).contains("supersetting"), "unrelated exercises must not trigger the prompt");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        assert_eq!(db.list_entries_for_session(session.id).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn session_auto_start() {
        let response = r#"{"message": "Logged!", "actions": [{"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 1}]}"#;
        let (handler, _) = setup_handler(response).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        assert!(db.get_active_session(user.id).unwrap().is_none());
        drop(db);

        let _ = handler.handle_text_message(&msg, "bench press").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        assert!(db.get_active_session(user.id).unwrap().is_some());
    }

    #[tokio::test]
    async fn slash_help_command() {
        let (handler, _) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let msg = make_message(12345, "/help");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "/help").await.unwrap();
        assert!(shown(&reply).contains("Available commands"));
    }

    #[tokio::test]
    async fn slash_start_existing_user() {
        let (handler, _) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let msg = make_message(12345, "/start");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "/start").await.unwrap();
        assert!(shown(&reply).contains("already registered"));
    }

    #[tokio::test]
    async fn multiple_actions_execute() {
        let response = r#"{"message": "Started session and logged exercise!", "actions": [
            {"type": "start_session", "notes": "Chest day"},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, _) = setup_handler(response).await;
        let msg = make_message(12345, "hello");

        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "start chest day, 3x8 bench 80kg").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        assert_eq!(session.notes.as_deref(), Some("Chest day"));
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1);
        let sets = db.list_sets_for_entry(entries[0].id).unwrap();
        assert_eq!(sets.len(), 3);
    }

    #[tokio::test]
    async fn partial_action_failure_appends_note() {
        let response = r#"{"message": "Tried to log both!", "actions": [
            {"type": "start_session"},
            {"type": "log_exercise", "exercise": "Nonexistent Exercise 99", "sets": 3, "reps": 8}
        ]}"#;
        let (handler, _) = setup_handler(response).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "do stuff").await.unwrap();
        assert!(shown(&reply).contains("some actions failed"));
    }

    #[tokio::test]
    async fn message_truncation() {
        let (handler, llm) = setup_handler(r#"{"message": "ok", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let long_text = "a".repeat(3000);
        llm.set_response(r#"{"message": "received", "actions": []}"#);
        let reply = handler.handle_text_message(&msg, &long_text).await.unwrap();
        assert_eq!(shown(&reply), "received");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let msgs = db.get_recent_messages_for_platform(user.id, "telegram", 10).unwrap();
        let user_msgs: Vec<_> = msgs.iter().filter(|m| m.role == ConversationRole::User).collect();
        let last_user_msg = user_msgs.last().unwrap();
        assert_eq!(last_user_msg.content.len(), 2000);
    }

    #[tokio::test]
    async fn slash_clear_excludes_prior_messages() {
        let (handler, _) = setup_handler(r#"{"message": "Got it!", "actions": []}"#).await;
        let msg = make_message(12345, "hello");

        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "how are you").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let msgs = db.get_recent_messages_for_platform(user.id, "telegram", 100).unwrap();
        assert_eq!(msgs.len(), 2);
        drop(db);

        let reply = handler.handle_text_message(&msg, "/clear").await.unwrap();
        assert!(shown(&reply).contains("cleared"));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let msgs = db.get_recent_messages_for_platform(user.id, "telegram", 100).unwrap();
        assert_eq!(msgs.len(), 0);
    }

    #[tokio::test]
    async fn slash_clear_in_help_text() {
        let (handler, _) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "/help").await.unwrap();
        assert!(shown(&reply).contains("/clear"));
    }

    struct FailingMockLlm;

    #[async_trait::async_trait]
    impl LlmProvider for FailingMockLlm {
        async fn complete(&self, _request: LlmRequest) -> anyhow::Result<LlmResponse> {
            anyhow::bail!("Service temporarily unavailable")
        }
    }

    async fn setup_failing_handler() -> AssistantHandler {
        let db = Database::open_in_memory().unwrap();
        let db = Arc::new(Mutex::new(db));
        let llm: Box<dyn LlmProvider> = Box::new(FailingMockLlm);
        AssistantHandler::new(db, llm, test_config()).await.unwrap()
    }

    #[tokio::test]
    async fn llm_error_stores_excluded_conversation() {
        let handler = setup_failing_handler().await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "some bad request").await.unwrap();
        assert!(shown(&reply).contains("trouble processing"));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let context_msgs = db.get_recent_messages_for_platform(user.id, "telegram", 100).unwrap();
        assert_eq!(context_msgs.len(), 0);

        let all_count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM conversation_history WHERE user_id = ?1", rusqlite::params![user.id], |row| row.get(0))
            .unwrap();
        assert_eq!(all_count, 2);
    }

    #[tokio::test]
    async fn refusal_response_excluded_from_context() {
        let (handler, _) = setup_handler(r#"{"message": "I cannot provide advice on that topic.", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "off topic stuff").await.unwrap();
        assert!(shown(&reply).contains("I cannot provide"));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let context_msgs = db.get_recent_messages_for_platform(user.id, "telegram", 100).unwrap();
        assert_eq!(context_msgs.len(), 0);
    }

    #[tokio::test]
    async fn assistant_history_preserves_json_envelope() {
        // The LLM is contracted to emit `{"message": "...", "actions": [...]}`. If
        // we strip the envelope before persisting the assistant turn, the next
        // call shows the model plain prose in history and it abandons the JSON
        // contract. Pin the round-trip: turn-2's request must contain the prior
        // assistant turn as a parseable AssistantResponse with non-empty actions.
        let canned = r#"{"message":"Logged.","actions":[{"type":"log_exercise","exercise":"Bench Press","sets":1,"reps":8,"weight_kg":60.0,"perceived_difficulty":"easy"}]}"#;
        let (handler, llm) = setup_handler(canned).await;
        let msg = make_message(12345, "hello");

        // First call registers the user and returns the welcome reply without
        // hitting the LLM; subsequent calls are real LLM round-trips.
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 60 8 reps easy").await.unwrap();
        let _ = handler.handle_text_message(&msg, "another set, 6 reps at 70 kg, hard").await.unwrap();

        let recorded = llm.recorded_requests();
        assert_eq!(recorded.len(), 2, "expected two LlmRequests after registration + two user turns");

        let second = &recorded[1];
        let assistant_turn = second
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, LlmRole::Assistant))
            .expect("turn-2 history must include the prior assistant turn");

        let parsed: crate::assistant::actions::AssistantResponse =
            serde_json::from_str(&assistant_turn.content).expect("assistant turn in history must round-trip as the JSON envelope");
        assert!(
            !parsed.actions.is_empty(),
            "expected the prior assistant turn to retain its actions array, got: {}",
            assistant_turn.content
        );
    }

    #[test]
    fn refusal_detection() {
        assert!(is_refusal_response("I cannot provide advice on that topic."));
        assert!(is_refusal_response("I can't help with that request."));
        assert!(is_refusal_response("That's outside my scope as a gym assistant."));
        assert!(is_refusal_response("I'm unable to assist with cooking recipes."));
        assert!(!is_refusal_response("Great job! I logged your bench press."));
        assert!(!is_refusal_response("Your session has been started."));
        assert!(!is_refusal_response("Here's your workout history."));
    }

    // ─── New behaviour tests for the set-centric workflow ──────────────────

    #[tokio::test]
    async fn supersets_keep_separate_entries() {
        let response_a = r#"{"message": "Logged bench.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_a).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 80kg 8 reps").await.unwrap();

        // Without closing, log a different exercise.
        llm.set_response(
            r#"{"message": "Logged pull-ups.", "actions": [
                {"type": "log_exercise", "exercise": "Pull-Up", "sets": 1, "reps": 10}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "now pull-ups, 10 reps").await.unwrap();

        // Then back to bench — should reuse the existing open Bench Press entry.
        llm.set_response(
            r#"{"message": "Logged another bench set.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "another bench set").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 2, "two distinct entries for two exercises (superset)");
        let mut counts: Vec<usize> = entries.iter().map(|e| db.list_sets_for_entry(e.id).unwrap().len()).collect();
        counts.sort();
        assert_eq!(counts, vec![1, 2], "Pull-Up=1, Bench Press=2");
        for e in &entries {
            assert!(e.end_timestamp.is_none(), "both entries remain open");
        }
    }

    #[tokio::test]
    async fn checkpoint_suffix_appears_at_3_sets_and_repeats_at_4() {
        let log_three = r#"{"message": "Done!", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log_three).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "3 sets bench 80kg 8").await.unwrap();
        assert!(shown(&reply).contains("You've logged 3 sets of Bench Press. Want another set, or move to the next exercise?"));

        // 4th set — checkpoint should fire again with n=4.
        llm.set_response(
            r#"{"message": "Logged.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "one more").await.unwrap();
        assert!(shown(&reply).contains("You've logged 4 sets of Bench Press"));
    }

    #[tokio::test]
    async fn premature_close_pushback_at_2_sets() {
        let response_log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "2 sets bench").await.unwrap();

        llm.set_response(
            r#"{"message": "Closing bench.", "actions": [
                {"type": "close_exercise_entry", "exercise": "Bench Press"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "close bench").await.unwrap();
        assert!(shown(&reply).contains("You've only done 2 sets of Bench Press. You should really push for one more! Should we keep going?"));
        // The entry stays open, so the in-flight rest timer must keep running.
        assert!(reply.timer.is_none(), "pushback must not cancel the rest timer");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].end_timestamp.is_none(), "entry must stay open after pushback");
    }

    #[tokio::test]
    async fn confirm_close_after_pushback_actually_closes() {
        let response_log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "2 sets bench").await.unwrap();

        llm.set_response(
            r#"{"message": "Closing for real.", "actions": [
                {"type": "confirm_close_exercise_entry", "exercise": "Bench Press"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "yes really close it").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert!(entries[0].end_timestamp.is_some(), "confirm_close_exercise_entry bypasses pushback");
    }

    #[tokio::test]
    async fn close_exercise_entry_with_three_sets_succeeds() {
        let response_log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "3 sets bench").await.unwrap();

        llm.set_response(
            r#"{"message": "Closing bench.", "actions": [
                {"type": "close_exercise_entry", "exercise": "Bench Press"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "move on").await.unwrap();
        // A real close ends the entry, so the rest timer is canceled.
        assert_eq!(reply.timer, Some(TimerSignal::Cancel), "a real close cancels the rest timer");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert!(entries[0].end_timestamp.is_some(), "≥3-set close should succeed without pushback");
    }

    #[tokio::test]
    async fn timers_command_cancels_only_on_disable() {
        let (handler, _llm) = setup_handler(r#"{"message": "ok", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap(); // register (defaults timers on)

        // First /timers disables, which must cancel any rest already counting down.
        let off = handler.handle_text_message(&msg, "/timers").await.unwrap();
        assert!(shown(&off).contains("now off"));
        assert_eq!(off.timer, Some(TimerSignal::Cancel), "disabling must cancel an in-flight rest");

        // Re-enabling has nothing to arm until the next set, so no timer directive.
        let on = handler.handle_text_message(&msg, "/timers").await.unwrap();
        assert!(shown(&on).contains("now on"));
        assert!(on.timer.is_none(), "enabling must not emit a timer directive");
    }

    #[tokio::test]
    async fn end_session_closes_all_open_entries() {
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 80kg 8 reps").await.unwrap();

        llm.set_response(r#"{"message": "Ending.", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "end session").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        // No active session anymore.
        assert!(db.get_active_session(user.id).unwrap().is_none());
        // No open entries either.
        let leftover = db.list_open_entries_for_user(user.id).unwrap();
        assert!(leftover.is_empty(), "end_session must cascade-close every open entry");
    }

    #[tokio::test]
    async fn start_session_with_open_entries_in_active_session_blocks() {
        // Step 1: log a set so an open entry exists in the active session.
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench").await.unwrap();

        // Step 2: try to start a new session — should be blocked.
        llm.set_response(r#"{"message": "Starting.", "actions": [{"type": "start_session"}]}"#);
        let reply = handler.handle_text_message(&msg, "start a new session").await.unwrap();
        assert!(shown(&reply).contains("open exercise"));
        assert!(shown(&reply).contains("close them or delete them"));

        // The original session is still the active one — no new session was created.
        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session_count: i64 =
            db.conn().query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1", rusqlite::params![user.id], |r| r.get(0)).unwrap();
        assert_eq!(session_count, 1);
    }

    #[tokio::test]
    async fn close_all_open_entries_clears_block_for_new_session() {
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench").await.unwrap();

        llm.set_response(r#"{"message": "Closing.", "actions": [{"type": "close_all_open_entries"}]}"#);
        let _ = handler.handle_text_message(&msg, "close them").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let leftover = db.list_open_entries_for_user(user.id).unwrap();
        assert!(leftover.is_empty());
    }

    #[tokio::test]
    async fn cmd_status_renders_superset_label_when_two_open() {
        let log_a = r#"{"message": "ok", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log_a).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench").await.unwrap();
        llm.set_response(
            r#"{"message": "ok", "actions": [
                {"type": "log_exercise", "exercise": "Pull-Up", "sets": 1, "reps": 10}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "pull-ups").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/status").await.unwrap();
        assert!(shown(&reply).contains("Superset (in progress)"));
        assert!(shown(&reply).contains("Bench Press"));
        assert!(shown(&reply).contains("Pull-Up"));
    }

    #[tokio::test]
    async fn cmd_status_renders_completed_section() {
        let log = r#"{"message": "ok", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "3 sets bench").await.unwrap();

        llm.set_response(r#"{"message": "ok", "actions": [{"type": "close_exercise_entry", "exercise": "Bench Press"}]}"#);
        let _ = handler.handle_text_message(&msg, "done").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/status").await.unwrap();
        assert!(shown(&reply).contains("Completed:"));
        assert!(shown(&reply).contains("Bench Press"));
    }

    #[tokio::test]
    async fn start_session_with_plan_stores_sentinel_in_notes() {
        // Seed a schedule named "Push Day" for the user.
        let (handler, llm) = setup_handler(r#"{"message": "hi", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user_id = {
            let db = handler.db.lock().await;
            let u = db.get_user_by_telegram_id("12345").unwrap().unwrap();
            let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let pull = db.get_exercise_type_by_name("Pull-Up").unwrap().unwrap();
            let sched_id = db
                .insert_schedule(&crate::db::Schedule {
                    id: 0,
                    user_id: u.id,
                    name: "Push Day".to_string(),
                    cron_expr: "0 0 6 * * 1".to_string(),
                    reminder_type: crate::db::ReminderType::Text,
                    reminder_notice_mins: 30,
                    enabled: true,
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .unwrap();
            db.add_schedule_exercise(&crate::db::ScheduleExercise {
                schedule_id: sched_id,
                exercise_type_id: bp.id,
                order_idx: 0,
                target_sets: Some(3),
                target_reps: Some(8),
                target_weight_kg: Some(80.0),
            })
            .unwrap();
            db.add_schedule_exercise(&crate::db::ScheduleExercise {
                schedule_id: sched_id,
                exercise_type_id: pull.id,
                order_idx: 1,
                target_sets: Some(3),
                target_reps: Some(10),
                target_weight_kg: None,
            })
            .unwrap();
            u.id
        };

        llm.set_response(r#"{"message": "Starting.", "actions": [{"type": "start_session", "plan": "Push Day"}]}"#);
        let _ = handler.handle_text_message(&msg, "start push day").await.unwrap();

        let db = handler.db.lock().await;
        let session = db.get_active_session(user_id).unwrap().unwrap();
        let notes = session.notes.unwrap();
        assert!(notes.starts_with("plan:Push Day"));
    }

    // ─── /feedback command (beta-tester gated) ─────────────────────────────

    #[tokio::test]
    async fn slash_feedback_hidden_from_non_beta_help() {
        let (handler, _) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "/help").await.unwrap();
        assert!(!shown(&reply).contains("/feedback"), "non-beta /help must not advertise /feedback");
    }

    #[tokio::test]
    async fn slash_feedback_visible_in_beta_help() {
        let (handler, _) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        promote_to_beta(&handler, "12345").await;
        let reply = handler.handle_text_message(&msg, "/help").await.unwrap();
        assert!(shown(&reply).contains("/feedback"), "beta /help must advertise /feedback, got: {}", shown(&reply));
    }

    #[tokio::test]
    async fn slash_feedback_non_beta_falls_through_to_llm() {
        let reporter = MockIssueReporter::ok("https://github.com/x/y/issues/1");
        let (handler, llm) = setup_handler_with_reporter(
            r#"{"message": "I don't know that command.", "actions": []}"#,
            Some(reporter.clone() as Arc<dyn IssueReporter>),
        )
        .await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let initial_calls = llm.recorded_requests().len();
        let _ = handler.handle_text_message(&msg, "/feedback the squat rack is broken").await.unwrap();

        assert!(
            llm.recorded_requests().len() > initial_calls,
            "non-beta /feedback should fall through to the LLM (no special handling)"
        );
        assert_eq!(reporter.call_count(), 0, "non-beta /feedback must not call the issue reporter");
    }

    #[tokio::test]
    async fn slash_feedback_empty_body_reply() {
        let reporter = MockIssueReporter::ok("https://github.com/x/y/issues/1");
        let (handler, _) =
            setup_handler_with_reporter(r#"{"message": "x", "actions": []}"#, Some(reporter.clone() as Arc<dyn IssueReporter>)).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        promote_to_beta(&handler, "12345").await;

        let reply = handler.handle_text_message(&msg, "/feedback").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("include a description"), "got: {}", shown(&reply));
        assert_eq!(reporter.call_count(), 0, "empty body must not reach the reporter");

        let reply = handler.handle_text_message(&msg, "/feedback    ").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("include a description"));
        assert_eq!(reporter.call_count(), 0);
    }

    #[tokio::test]
    async fn slash_feedback_creates_issue_and_returns_url() {
        let reporter = MockIssueReporter::ok("https://github.com/x/y/issues/42");
        let (handler, _) =
            setup_handler_with_reporter(r#"{"message": "x", "actions": []}"#, Some(reporter.clone() as Arc<dyn IssueReporter>)).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        promote_to_beta(&handler, "12345").await;

        let reply = handler.handle_text_message(&msg, "/feedback the bench-press timer never stops counting").await.unwrap();
        assert!(shown(&reply).contains("https://github.com/x/y/issues/42"), "reply must echo issue URL, got: {}", shown(&reply));
        assert_eq!(reporter.call_count(), 1);

        let (title, body) = reporter.last_call().unwrap();
        assert!(title.starts_with("[corre-gym] "), "title must be tagged: {title}");
        assert!(title.contains("bench-press timer"));
        assert!(body.contains("bench-press timer never stops counting"));
        assert!(body.contains("Reported by:"));
        assert!(body.contains("12345"), "footer must record the telegram_id for triage");
    }

    #[tokio::test]
    async fn slash_feedback_no_reporter_configured_replies_gracefully() {
        let (handler, _) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        promote_to_beta(&handler, "12345").await;

        let reply = handler.handle_text_message(&msg, "/feedback something broken").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("isn't configured"), "got: {}", shown(&reply));
    }

    #[tokio::test]
    async fn slash_feedback_reporter_error_replies_with_user_safe_message() {
        let reporter = MockIssueReporter::err("github API 401: Bad credentials");
        let (handler, _) =
            setup_handler_with_reporter(r#"{"message": "x", "actions": []}"#, Some(reporter.clone() as Arc<dyn IssueReporter>)).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        promote_to_beta(&handler, "12345").await;

        let reply = handler.handle_text_message(&msg, "/feedback whatever").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("couldn't file"), "got: {}", shown(&reply));
        assert!(!shown(&reply).contains("401"), "must not leak status code: {}", shown(&reply));
        assert!(!shown(&reply).to_lowercase().contains("github"), "must not leak integration name: {}", shown(&reply));
        assert_eq!(reporter.call_count(), 1);
    }

    #[test]
    fn build_feedback_title_caps_and_tags() {
        let long_body = "x".repeat(500);
        let title = build_feedback_title(&long_body);
        assert!(title.starts_with("[corre-gym] "));
        assert!(title.chars().count() <= "[corre-gym] ".chars().count() + FEEDBACK_TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn build_feedback_body_appends_reporter_footer() {
        let user =
            User { id: 7, name: "Alice".into(), telegram_id: Some("999".into()), signal_id: None, pubkey: None, timezone: "UTC".into(),
                created_at: String::new(), updated_at: String::new(), beta_tester: true, timers_enabled: true };
        let body = build_feedback_body(&user, "the squat rack is broken");
        assert!(body.starts_with("the squat rack is broken"));
        assert!(body.contains("Reported by: Alice"));
        assert!(body.contains("telegram_id: 999"));
    }

    // ─── command lists ────────────────────────────────────────────────────────

    fn command_list_user(beta_tester: bool) -> User {
        User { id: 7, name: "Alice".into(), telegram_id: None, signal_id: None, pubkey: None, timezone: "UTC".into(),
            created_at: String::new(), updated_at: String::new(), beta_tester, timers_enabled: true }
    }

    #[test]
    fn help_lists_every_command_the_user_can_run() {
        let msg = AssistantHandler::cmd_help(&command_list_user(false));
        assert!(msg.starts_with("Available commands:\n/start -- Introduction and registration\n"));
        assert!(msg.contains("\n/cancel -- Cancel an in-progress interview (e.g. /philosophy)\n"));
        assert!(msg.contains("\n/help -- This message\n\n"));
        assert!(msg.ends_with("- \"What did I do today?\""));
    }

    /// `/start` used to omit `/cancel` because it kept its own copy of the list.
    /// Both surfaces now render the same table, so they cannot drift again.
    #[test]
    fn start_and_help_name_the_same_commands() {
        let user = command_list_user(false);
        let names = |msg: &str| {
            msg.lines()
                .filter_map(|line| line.trim_start_matches("- ").split_whitespace().next())
                .filter(|word| word.starts_with('/'))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&AssistantHandler::cmd_start(&user)), names(&AssistantHandler::cmd_help(&user)));
        assert!(AssistantHandler::cmd_start(&user).contains("- /cancel -- "));
    }

    #[test]
    fn only_beta_testers_are_shown_feedback() {
        assert!(!AssistantHandler::cmd_help(&command_list_user(false)).contains("/feedback"));
        assert!(!AssistantHandler::cmd_start(&command_list_user(false)).contains("/feedback"));
        assert!(AssistantHandler::cmd_help(&command_list_user(true)).contains("/feedback <text> -- File a bug report"));
        assert!(AssistantHandler::cmd_start(&command_list_user(true)).contains("- /feedback <text> -- File a bug report"));
    }

    #[test]
    fn start_keeps_its_greeting_and_logging_hint() {
        let msg = AssistantHandler::cmd_start(&command_list_user(false));
        assert!(msg.starts_with("You're already registered, Alice! Here's what I can help with:\n"));
        assert!(msg.contains("- Tell me about your exercises and I'll log them\n"));
    }

    // ─── get_last_exercise ────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_last_exercise_returns_entry_when_logged() {
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 80kg 8 medium").await.unwrap();

        llm.set_response(
            r#"{"message": "Here's your last bench press.", "actions": [
                {"type": "get_last_exercise", "exercise": "Bench Press"}
            ]}"#,
        );
        let view = handler.handle_text_message(&msg, "what was my last bench press?").await.unwrap();
        let (reply, _) = Telegram.render(&view.view);
        assert!(reply.contains("Bench Press"), "reply must name the resolved exercise, got: {reply}");
        assert!(reply.contains("8×80kg"), "reply must include the set, got: {reply}");
    }

    #[tokio::test]
    async fn get_last_exercise_returns_not_found_message_when_absent() {
        let (handler, llm) = setup_handler(r#"{"message": "ok", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        llm.set_response(
            r#"{"message": "Looking it up.", "actions": [
                {"type": "get_last_exercise", "exercise": "Bench Press"}
            ]}"#,
        );
        let view = handler.handle_text_message(&msg, "what was my last bench press?").await.unwrap();
        let (reply, _) = Telegram.render(&view.view);
        assert!(reply.to_lowercase().contains("haven't logged"), "expected not-found phrasing, got: {reply}");
    }

    // ─── /exercises rendering ────────────────────────────────────────────────

    #[tokio::test]
    async fn cmd_exercises_emits_one_pre_block_per_muscle_group() {
        let (handler, _) = setup_handler("").await;
        let (text, parse_mode) = Telegram.render(&View::Catalog(handler.cmd_exercises()));

        // HTML mode is required for Telegram to render the <b> / <pre> markup.
        assert_eq!(parse_mode, Some("HTML"), "cmd_exercises must use HTML parse mode");

        // <pre> open/close counts must match — Telegram rejects unbalanced tags.
        let opens = text.matches("<pre>").count();
        let closes = text.matches("</pre>").count();
        assert_eq!(opens, closes, "<pre> tags must be balanced: open={opens} close={closes}");

        // Group headings: each <b>...</b> heading begins a new block. The bug
        // produced one heading per row (dozens); the fix groups so there are
        // far fewer than rows, bounded by the seeded muscle groups (7).
        let heading_count = text.matches("<b>").count();
        assert!(heading_count > 0, "must render at least one heading");
        assert!(heading_count <= 10, "expected ≤10 muscle-group headings, got {heading_count}");
        assert_eq!(heading_count, opens, "one <pre> block per heading");
    }

    #[tokio::test]
    async fn cmd_exercises_groups_each_muscle_group_once() {
        let (handler, _) = setup_handler("").await;
        let (text, _) = Telegram.render(&View::Catalog(handler.cmd_exercises()));

        // Extract heading bodies and assert each is unique. The pre-fix code
        // emitted a new heading every time the (level,name) order moved to a
        // different group, producing duplicate "Chest", "Legs" headings.
        let mut headings: Vec<&str> = text
            .match_indices("<b>")
            .filter_map(|(i, _)| {
                let after = &text[i + 3..];
                after.find("</b>").map(|j| &after[..j])
            })
            .collect();
        let total = headings.len();
        headings.sort_unstable();
        headings.dedup();
        assert_eq!(headings.len(), total, "each muscle-group heading must appear exactly once");
    }

    #[tokio::test]
    async fn cmd_exercises_includes_seeded_exercises() {
        let (handler, _) = setup_handler("").await;
        let (text, _) = Telegram.render(&View::Catalog(handler.cmd_exercises()));

        // Seeded fixtures from migrations/02-exercises: at least Bench Press
        // (Chest) and Squat (Legs) must show up in the rendered table.
        assert!(text.contains("Bench Press"), "Bench Press missing from output");
        assert!(text.contains("Squat"), "Squat missing from output");
    }

    #[tokio::test]
    async fn cmd_exercises_output_fits_chunker_without_breaking_pre() {
        use crate::telegram::chunk::split_for_telegram;
        let (handler, _) = setup_handler("").await;
        let (text, _) = Telegram.render(&View::Catalog(handler.cmd_exercises()));

        // The whole point of the issue: feeding the long /exercises reply to
        // the splitter must yield chunks Telegram will accept (balanced <pre>).
        for chunk in split_for_telegram(&text) {
            assert!(chunk.len() <= 4096, "chunk over Telegram limit: {}", chunk.len());
            assert_eq!(
                chunk.matches("<pre>").count(),
                chunk.matches("</pre>").count(),
                "unbalanced <pre> in chunk: {chunk:?}",
            );
        }
    }

    #[tokio::test]
    async fn philosophy_interview_saves_and_exits() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        // /philosophy opens the interview with a fixed opener and arms the mode.
        let opener = handler.handle_text_message(&msg, "/philosophy").await.unwrap();
        assert!(shown(&opener).to_lowercase().contains("philosophy"));
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_some());

        // A first answer keeps the interview going (no save action).
        llm.set_response(r#"{"message": "Great. What equipment do you have?", "actions": []}"#);
        let q2 = handler.handle_text_message(&msg, "3x a week, hypertrophy, I like 5x5").await.unwrap();
        assert!(shown(&q2).contains("equipment"));
        let state = handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().unwrap();
        assert_eq!(state.turns, 1, "turn counter should advance");

        // The next answer triggers save_philosophy: it is stored and the mode clears.
        llm.set_response(
            r#"{"message": "Locked in.", "actions": [
                {"type": "save_philosophy", "content": "goal=hypertrophy. Likes 5x5. Home gym: squat rack 120kg, dumbbells 24kg. 3x/week."}
            ]}"#,
        );
        let done = handler.handle_text_message(&msg, "squat rack to 120kg and dumbbells to 24kg").await.unwrap();
        assert!(shown(&done).contains("/nextworkout"), "confirmation should point at /nextworkout");

        let db = handler.db.lock().await;
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_none(), "mode should be cleared");
        let saved = db.latest_philosophy(user.id).unwrap().unwrap();
        assert!(saved.content.contains("hypertrophy") && saved.content.contains("120kg"));
        assert_eq!(saved.source, "interview");
    }

    #[tokio::test]
    async fn cancel_aborts_interview_without_saving() {
        let (handler, _llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let _ = handler.handle_text_message(&msg, "/philosophy").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_some());

        let cancelled = handler.handle_text_message(&msg, "/cancel").await.unwrap();
        assert!(shown(&cancelled).to_lowercase().contains("cancel"));

        let db = handler.db.lock().await;
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_none());
        assert!(db.latest_philosophy(user.id).unwrap().is_none(), "nothing should be saved");
    }

    #[tokio::test]
    async fn nextworkout_without_philosophy_offers_to_build_one() {
        let (handler, _) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        assert!(shown(&reply).contains("/philosophy"), "should point the user at /philosophy first");
    }

    #[tokio::test]
    async fn nextworkout_designs_and_persists_plan_without_logging() {
        let design = r#"{"message": "Here's today's session.", "actions": [
            {"type": "propose_workout", "title": "Upper push", "rationale": "Bench was easy; push it.", "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 65.0, "notes": "drive through heels"},
                {"exercise": "One Arm Dumbbell Row", "target_sets": 3, "target_reps": 8, "target_weight_kg": 24.0}
            ]}
        ]}"#;
        let (handler, _) = setup_handler(design).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "goal=hypertrophy; 5x5; dumbbells to 24kg", "interview").unwrap();
        }

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Upper push"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("3 sets × 6 reps @ 65kg"));

        let db = handler.db.lock().await;
        let plan = db.latest_proposed_plan(user.id).unwrap().expect("a proposed plan should be persisted");
        assert_eq!(plan.title, "Upper push");
        assert_eq!(db.list_plan_exercises(plan.id).unwrap().len(), 2);
        // The crucial guarantee: designing logs NOTHING.
        assert!(db.get_active_session(user.id).unwrap().is_none(), "nextworkout must not start a session");
        assert!(db.recent_sessions_with_sets(user.id, 5).unwrap().is_empty(), "nextworkout must not log sets");
    }

    #[tokio::test]
    async fn nextworkout_drops_unresolved_exercise_with_note() {
        let design = r#"{"message": "Plan ready.", "actions": [
            {"type": "propose_workout", "title": "Mixed", "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 8, "target_weight_kg": 60.0},
                {"exercise": "Jetpack Flips", "target_sets": 3, "target_reps": 8}
            ]}
        ]}"#;
        let (handler, _) = setup_handler(design).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "general fitness", "interview").unwrap();
        }

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Bench Press"));
        assert!(text.contains("Skipped") && text.contains("Jetpack Flips"), "unresolved exercise should be noted, not fatal");

        let db = handler.db.lock().await;
        let plan = db.latest_proposed_plan(user.id).unwrap().unwrap();
        assert_eq!(db.list_plan_exercises(plan.id).unwrap().len(), 1, "only the resolved exercise is persisted");
    }

    /// Register a user, give them a philosophy, design a one-exercise workout, then
    /// start a session — returning the user once a guided plan is bound.
    async fn start_guided_workout(handler: &AssistantHandler, llm: &MockLlm, msg: &TgMessage) -> User {
        let _ = handler.handle_text_message(msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "5x5, dumbbells to 24kg", "interview").unwrap();
        }
        llm.set_response(
            r#"{"message": "Here's your session.", "actions": [
                {"type": "propose_workout", "title": "Push", "exercises": [
                    {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 60.0}
                ]}
            ]}"#,
        );
        let _ = handler.handle_text_message(msg, "/nextworkout").await.unwrap();
        llm.set_response(r#"{"message": "Let's go!", "actions": [{"type": "start_session"}]}"#);
        let _ = handler.handle_text_message(msg, "let's start the workout").await.unwrap();
        user
    }

    #[tokio::test]
    async fn starting_a_session_binds_the_designed_plan_and_prescribes() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;

        {
            let db = handler.db.lock().await;
            let plan = db.active_plan_for_user(user.id).unwrap().expect("plan should be active after start");
            let session = db.get_active_session(user.id).unwrap().unwrap();
            assert_eq!(plan.session_id, Some(session.id), "plan bound to the session");
            assert!(db.latest_proposed_plan(user.id).unwrap().is_none(), "no longer 'proposed' once active");
        }

        // The next turn's system prompt carries the guided prescription.
        llm.set_response(r#"{"message": "ok", "actions": []}"#);
        let _ = handler.handle_text_message(&msg, "ready").await.unwrap();
        let last = llm.recorded_requests().pop().unwrap();
        let system = &last.messages[0].content;
        assert!(system.contains("PRESCRIBED WORKOUT"), "guided section should be in the prompt");
        assert!(system.contains("Bench Press"));
    }

    #[tokio::test]
    async fn mid_workout_note_appends_to_philosophy_and_end_completes_plan() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;
        let plan_id = handler.db.lock().await.active_plan_for_user(user.id).unwrap().unwrap().id;

        // A durable preference voiced mid-workout is appended to the philosophy.
        llm.set_response(r#"{"message": "Got it.", "actions": [{"type": "append_philosophy_note", "note": "prefers goblet squats"}]}"#);
        let reply = handler.handle_text_message(&msg, "I always prefer goblet squats").await.unwrap();
        assert!(shown(&reply).contains("Noted for future workouts"));
        {
            let latest = handler.db.lock().await.latest_philosophy(user.id).unwrap().unwrap();
            assert!(latest.content.contains("goblet squats"));
            assert_eq!(latest.source, "note");
        }

        // Ending the session completes the bound plan.
        llm.set_response(r#"{"message": "Nice work!", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "done").await.unwrap();
        let db = handler.db.lock().await;
        assert!(db.active_plan_for_user(user.id).unwrap().is_none());
        assert_eq!(db.get_plan(plan_id).unwrap().unwrap().status, crate::db::PlanStatus::Completed);
    }

    #[tokio::test]
    async fn one_off_override_scopes_to_plan_and_spares_philosophy() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;
        let plan_id = handler.db.lock().await.active_plan_for_user(user.id).unwrap().unwrap().id;

        // A one-off voiced mid-workout attaches to the plan in flight, NOT the philosophy.
        llm.set_response(
            r#"{"message": "Sure, flys today.", "actions": [{"type": "set_session_override", "note": "no bench today, do flys instead"}]}"#,
        );
        let reply = handler.handle_text_message(&msg, "I don't feel like bench today, let's do flys").await.unwrap();
        assert!(shown(&reply).contains("just for this workout"));

        {
            let db = handler.db.lock().await;
            let plan = db.get_plan(plan_id).unwrap().unwrap();
            assert!(plan.override_note.as_deref().unwrap().contains("flys"), "override stored on the plan");
            // The philosophy must be untouched — a one-off there would ban bench forever.
            let latest = db.latest_philosophy(user.id).unwrap().unwrap();
            assert_eq!(latest.source, "interview", "no philosophy note should have been appended");
            assert!(!latest.content.contains("bench"), "one-off must not reach the philosophy");
        }

        // On the next turn the override surfaces in the coaching prompt for the plan in flight.
        llm.set_response(r#"{"message": "ok", "actions": []}"#);
        let _ = handler.handle_text_message(&msg, "what's next?").await.unwrap();
        let last = llm.recorded_requests().pop().unwrap();
        let system = &last.messages[0].content;
        assert!(system.contains("TODAY-ONLY OVERRIDES"), "override must reach the coaching prompt");
        assert!(system.contains("flys"));

        // End the session, then design a fresh plan: the one-off does NOT carry over.
        llm.set_response(r#"{"message": "Done!", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "done").await.unwrap();
        llm.set_response(
            r#"{"message": "New plan.", "actions": [{"type": "propose_workout", "title": "Pull", "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 60.0}
            ]}]}"#,
        );
        let _ = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();

        let db = handler.db.lock().await;
        let fresh = db.latest_proposed_plan(user.id).unwrap().unwrap();
        assert_ne!(fresh.id, plan_id, "a new plan was designed");
        assert!(fresh.override_note.is_none(), "the one-off must not survive into the next design");
    }

    #[test]
    fn proposed_plan_window_excludes_stale_designs() {
        let now = parse_sqlite_datetime("2026-06-19 12:00:00").unwrap();
        // Designed an hour ago → still bindable.
        assert!(proposed_plan_within_window("2026-06-19 11:00:00", now));
        // Designed yesterday → stale, must not bind to today's session.
        assert!(!proposed_plan_within_window("2026-06-18 11:00:00", now));
        // Exactly at the 12h cutoff → stale (the window is exclusive).
        assert!(!proposed_plan_within_window("2026-06-19 00:00:00", now));
        // Unparseable timestamp fails open (we just wrote it).
        assert!(proposed_plan_within_window("not a date", now));
    }
}

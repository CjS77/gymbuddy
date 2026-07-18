use std::sync::Arc;

use anyhow::Context as _;
use corre_core::app::LlmProvider;
use tokio::sync::Mutex;

use crate::config::GymConfig;
use crate::db::{Database, ExerciseSet, ExerciseTypeWithAncestry, MeasurementType, User, new_user, new_user_with_pubkey};
use crate::github::IssueReporter;
use crate::telegram::Message as TgMessage;

use super::parser::parse_assistant_response;
use gymbuddy_proto::{Measurement, SetLine, TimerSignal, View};

mod context;
mod continuity;
mod designer;
mod dispatch;
mod execution;
mod interview;
mod llm;
#[cfg(test)]
mod test_support;

use llm::is_refusal_response;

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

    /// Resolve an exercise type's display name from the in-memory catalogue, falling
    /// back to "unknown" when the id is not present.
    fn exercise_name(&self, exercise_type_id: i64) -> String {
        self.catalogue
            .iter()
            .find(|e| e.exercise_type.id == exercise_type_id)
            .map(|e| e.exercise_type.name.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }
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

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use crate::db::ConversationRole;

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
}

//! First-run onboarding: the welcome that actively asks whether to set things up,
//! and the routing that takes an affirmative straight into the `/philosophy`
//! interview.
//!
//! The decision is made server-side, with no LLM round-trip — exactly like the
//! session-continuity resume in [`super::continuity`], and for the same reason: a
//! small model is unreliable at turning "yeah go on" into the right control flow,
//! and this one has to work on the user's very first message. Reading the answer is
//! [`super::affirmative::is_affirmative`]'s job, shared with the programme lock-in.

use crate::db::{ConversationRole, User, new_conversation_message};

use super::AssistantHandler;
use super::affirmative::is_affirmative;
use gymbuddy_proto::{ONBOARDING_ASK, View};

impl AssistantHandler {
    /// The new-user welcome. Ends with [`ONBOARDING_ASK`], which is what
    /// [`contains_onboarding_ask`] later recognises in the stored history.
    pub(super) fn welcome_message(user: &User) -> String {
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
             Type /help for a list of commands.\n\n\
             {ONBOARDING_ASK}",
            user.name
        )
    }

    /// Record the onboarding ask as the assistant's opening turn on `platform`
    /// without sending anything, for transports whose client writes its own
    /// greeting (confide: [`gymbuddy_proto::ServerResponse::Welcome`] carries only
    /// a name, so the client appends [`ONBOARDING_ASK`] itself). Storing it is what
    /// arms [`Self::maybe_handle_onboarding_reply`] on the user's next message.
    pub async fn record_onboarding_ask(&self, user: &User, platform: &str) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        db.insert_message(&new_conversation_message(user.id, platform, ConversationRole::Assistant, ONBOARDING_ASK))?;
        Ok(())
    }

    /// Route an affirmative answer to the welcome's ask straight into the
    /// `/philosophy` interview. Returns `None` — so ordinary handling proceeds —
    /// whenever the previous assistant turn was not the ask, or the reply is not a
    /// plain yes. Declining needs no branch of its own: anything that is not an
    /// affirmative simply falls through, so "no thanks" and "3x8 bench at 80kg"
    /// both behave exactly as they did before this ever asked.
    pub(super) async fn maybe_handle_onboarding_reply(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Option<View>> {
        if !is_affirmative(text) {
            return Ok(None);
        }
        let asked = {
            let db = self.db.lock().await;
            let recent = db.get_recent_messages_for_platform(user.id, platform, 4)?;
            recent
                .iter()
                .rev()
                .find(|m| m.role == ConversationRole::Assistant)
                .is_some_and(|m| contains_onboarding_ask(&m.content))
        };
        if !asked {
            return Ok(None);
        }
        tracing::debug!(user_id = user.id, %platform, "onboarding ask accepted — entering the philosophy interview");
        self.cmd_philosophy_start(user, platform).await.map(Some)
    }
}

/// Did `reply` end with the onboarding ask? Compared against the one shared
/// [`ONBOARDING_ASK`] constant so the Telegram welcome and a confide client's own
/// greeting are recognised by the same rule.
fn contains_onboarding_ask(reply: &str) -> bool {
    reply.to_lowercase().contains(&ONBOARDING_ASK.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[tokio::test]
    async fn welcome_ends_with_the_ask() {
        let (handler, _) = setup_handler(r#"{"message": "Hello!", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let reply = handler.handle_text_message(&msg, "hello").await.unwrap();
        assert!(shown(&reply).trim_end().ends_with(ONBOARDING_ASK), "welcome must end with the ask, got: {}", shown(&reply));
    }

    /// The whole point of the ticket: "yes" on the very first turn lands in the
    /// interview, with no LLM call to decide it.
    #[tokio::test]
    async fn affirmative_after_the_welcome_enters_the_interview() {
        let (handler, llm) = setup_handler(r#"{"message": "should not be called", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let before = llm.recorded_requests().len();

        let reply = handler.handle_text_message(&msg, "yes").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("philosophy"), "got: {}", shown(&reply));
        assert_eq!(llm.recorded_requests().len(), before, "the yes must be decided server-side, with no LLM round-trip");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_some(), "the interview must be armed");
    }

    /// Declining leaves everything exactly as it was: no interview, and the next
    /// message logs a set through the ordinary path.
    #[tokio::test]
    async fn declining_then_logging_a_set_works_unchanged() {
        let (handler, llm) = setup_handler(r#"{"message": "No problem.", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let _ = handler.handle_text_message(&msg, "no thanks").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_none());

        llm.set_response(
            r#"{"message": "Logged.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "hard"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "bench press 80kg 8 reps, hard").await.unwrap();

        let db = handler.db.lock().await;
        let session = db.get_active_session(user.id).unwrap().expect("logging must auto-start a session");
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(db.list_sets_for_entry(entries[0].id).unwrap().len(), 1);
    }

    /// A yes much later in the conversation is just a yes to whatever was actually
    /// asked — it must not reopen the interview.
    #[tokio::test]
    async fn affirmative_without_a_pending_ask_is_ignored() {
        let (handler, _llm) = setup_handler(r#"{"message": "Got it!", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "no thanks").await.unwrap();

        let reply = handler.handle_text_message(&msg, "yes").await.unwrap();
        assert_eq!(shown(&reply), "Got it!", "a stray yes must reach the LLM, not the interview");
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_none());
    }

    /// The confide path has no server-sent welcome text — the client writes its own
    /// greeting — so the ask is recorded directly. Detection must work identically.
    #[tokio::test]
    async fn recorded_ask_arms_detection_on_confide() {
        let (handler, _llm) = setup_handler(r#"{"message": "x", "actions": []}"#).await;
        let user = handler.register_user("beef", "Alice", "UTC").await.unwrap();
        handler.record_onboarding_ask(&user, "confide").await.unwrap();

        let reply = handler.handle_message_for_user(&user, "yeah", "confide").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("philosophy"), "got: {}", shown(&reply));
        assert!(handler.db.lock().await.get_interview_state(user.id, "confide").unwrap().is_some());
    }
}

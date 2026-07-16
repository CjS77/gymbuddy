//! LLM plumbing shared by every path that talks to the model: message
//! assembly, the completion call, refusal detection, and conversation
//! persistence (normal and context-excluded).

use anyhow::Context as _;
use corre_core::app::{LlmMessage, LlmRequest, LlmRole};

use crate::db::{ConversationRole, new_conversation_message};

use super::AssistantHandler;

impl AssistantHandler {
    pub(super) async fn call_llm(
        &self,
        system_prompt: &str,
        history: &[crate::db::ConversationMessage],
        user_text: &str,
    ) -> anyhow::Result<String> {
        self.call_llm_with(system_prompt, history, user_text, 1024, 0.1).await
    }

    /// Like [`Self::call_llm`] but with an explicit token cap and temperature. Used
    /// by `/nextworkout`, whose multi-exercise design overruns the default 1024-token
    /// budget.
    pub(super) async fn call_llm_with(
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

        // Every prompt in `prompts.rs` demands a JSON object, so ask the provider to
        // enforce it. As of corre-llm 0.22 this reaches the wire as
        // `response_format: {"type": "json_object"}`; before that the field was read
        // by nothing and the contract rested on prompt text alone. It still rests on
        // the parser's tolerance for any model that ignores `response_format`.
        let request =
            LlmRequest { messages, temperature: Some(temperature), max_completion_tokens: Some(max_tokens), json_mode: true };

        let response = self.llm.complete(request).await.context("LLM completion failed")?;
        Ok(response.content)
    }

    pub(super) async fn store_conversation_on_platform(
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

    pub(super) async fn store_excluded_conversation_on_platform(
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

/// Detect LLM refusal responses that indicate the message was off-topic or blocked.
pub(super) fn is_refusal_response(text: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use corre_core::app::{LlmProvider, LlmResponse};
    use tokio::sync::Mutex;

    use super::*;
    use super::super::test_support::*;
    use crate::db::Database;

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
    async fn every_request_asks_the_provider_for_json_mode() {
        // Our half of the contract. corre-llm 0.22 maps `json_mode` onto
        // `response_format: {"type": "json_object"}`; before it did, this flag was
        // read by nothing and setting it was a silent no-op. Now that it reaches
        // the wire, dropping it would quietly weaken every prompt in `prompts.rs`
        // back to text-only enforcement.
        let (handler, llm) = setup_handler(r#"{"message": "Hi", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "what did I do today?").await.unwrap();

        let recorded = llm.recorded_requests();
        assert!(!recorded.is_empty(), "expected at least one LLM round-trip");
        assert!(recorded.iter().all(|r| r.json_mode), "every request must ask for JSON mode");
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
}

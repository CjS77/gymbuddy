//! Shared fixtures for the handler unit tests: the recording mock LLM, canned
//! Telegram messages, a test config, and handler constructors over an
//! in-memory database.

use std::sync::Arc;

use corre_core::app::{LlmProvider, LlmRequest, LlmResponse};
use tokio::sync::Mutex;

use crate::config::GymConfig;
use crate::db::Database;
use crate::github::IssueReporter;
use crate::render::Telegram;
use crate::telegram::Message as TgMessage;

use super::{AssistantHandler, Reply};
use gymbuddy_proto::Render as _;

/// The text a Telegram user would see for a reply — lets these tests keep
/// asserting on rendered output after the move to the domain `View` model.
pub(super) fn shown(reply: &Reply) -> String {
    Telegram.render(&reply.view).0
}

pub(super) struct MockLlm {
    response: std::sync::Mutex<String>,
    recorded: std::sync::Mutex<Vec<LlmRequest>>,
}

impl MockLlm {
    pub(super) fn new(response: &str) -> Self {
        Self { response: std::sync::Mutex::new(response.to_string()), recorded: std::sync::Mutex::new(Vec::new()) }
    }

    pub(super) fn set_response(&self, response: &str) {
        *self.response.lock().unwrap() = response.to_string();
    }

    pub(super) fn recorded_requests(&self) -> Vec<LlmRequest> {
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

pub(super) fn make_message(user_id: i64, text: &str) -> TgMessage {
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

pub(super) fn test_config() -> GymConfig {
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
        designer_history: crate::config::DesignerHistoryConfig::default(),
    }
}

pub(super) async fn setup_handler(response: &str) -> (AssistantHandler, Arc<MockLlm>) {
    setup_handler_with_reporter(response, None).await
}

pub(super) async fn setup_handler_with_reporter(
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

struct MockLlmWrapper(Arc<MockLlm>);

#[async_trait::async_trait]
impl LlmProvider for MockLlmWrapper {
    async fn complete(&self, request: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.0.complete(request).await
    }
}

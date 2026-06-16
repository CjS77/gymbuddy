//! End-to-end transport test: a real `gymbuddy-client` talks to the real confide
//! transport (relay disabled, direct connection over loopback), registers by
//! pubkey, logs a set in natural language, and we assert both the `Reply` and the
//! persisted sets in the server's SQLite DB.
//!
//! The LLM is stubbed with a canned action so the test is hermetic — no network,
//! no relay, no model. Modeled on `confide/tests/roundtrip.rs`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use corre_core::app::{LlmProvider, LlmRequest, LlmResponse};
use gymbuddy_backend::assistant::AssistantHandler;
use gymbuddy_backend::config::{ConfideConfig, GymConfig};
use gymbuddy_backend::db::Database;
use gymbuddy_backend::transport::confide::ConfideServer;
use gymbuddy_client::{ConnectOptions, GymClient};
use gymbuddy_proto::{ClientRequest, ServerResponse};
use tokio::sync::Mutex;

/// LLM stub that always logs three bench-press sets (action shape copied from the
/// handler's `exercise_logging_creates_records` test).
struct StubLlm {
    response: String,
}

#[async_trait]
impl LlmProvider for StubLlm {
    async fn complete(&self, _request: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse { content: self.response.clone() })
    }
}

/// Map wildcard bind addresses to loopback so they're actually dialable in-process.
fn dialable(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addrs
        .into_iter()
        .filter(|a| a.is_ipv4())
        .map(|a| if a.ip().is_unspecified() { SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), a.port()) } else { a })
        .collect()
}

fn test_config() -> GymConfig {
    // Empty TOML → every field defaults (telegram token absent, confide absent).
    toml::from_str("").expect("default GymConfig")
}

const LOG_THREE_SETS: &str = r#"{"message": "Logged your bench press!", "actions": [
    {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"},
    {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"},
    {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
]}"#;

#[tokio::test]
async fn register_then_chat_logs_sets_over_confide() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Server handler over a tempfile DB + stubbed LLM.
    let db = Arc::new(Mutex::new(Database::open(&dir.path().join("gym.db")).unwrap()));
    let llm: Box<dyn LlmProvider> = Box::new(StubLlm { response: LOG_THREE_SETS.to_string() });
    let handler = Arc::new(AssistantHandler::new(db.clone(), llm, test_config()).await.unwrap());

    // 2. Bind the confide server with the relay disabled, then run it.
    let cfg = ConfideConfig { keystore_path: dir.path().join("server.key"), allowed_pubkeys: vec![], relay: false };
    let server = ConfideServer::bind(&cfg).await.unwrap();
    let server_pubkey = server.pubkey_hex();
    let server_addrs = dialable(server.direct_addresses());
    tokio::spawn(server.run(handler.clone()));

    // 3. Connect a real client directly (no relay).
    let opts = ConnectOptions {
        server_pubkey_hex: server_pubkey,
        relay: false,
        server_addrs,
        keystore_path: dir.path().join("client.key"),
    };
    let (client, mut responses) =
        tokio::time::timeout(Duration::from_secs(10), GymClient::connect(opts)).await.expect("connect timed out").expect("connect");

    // 4. Hello → not registered yet.
    let resp = client.request(&mut responses, &ClientRequest::Hello).await.unwrap();
    assert!(matches!(resp, ServerResponse::NeedsRegistration), "got {resp:?}");

    // 5. Register → welcomed.
    let resp = client
        .request(&mut responses, &ClientRequest::Register { name: "Alice".into(), timezone: "UTC".into() })
        .await
        .unwrap();
    assert!(matches!(resp, ServerResponse::Welcome { .. }), "got {resp:?}");

    // 6. Log a set in natural language → assistant reply.
    let resp = client
        .request(&mut responses, &ClientRequest::Chat { text: "I did 3 sets of bench press at 80kg, 8 reps".into() })
        .await
        .unwrap();
    let ServerResponse::Reply { text } = resp else { panic!("expected Reply, got {resp:?}") };
    assert!(text.starts_with("Logged your bench press!"), "unexpected reply: {text}");

    // 7. The sets are persisted in the server's DB, owned by the pubkey user.
    let db = db.lock().await;
    let user = db.get_user_by_pubkey(client.my_pubkey_hex()).unwrap().expect("user registered by pubkey");
    let session = db.get_active_session(user.id).unwrap().expect("active session");
    let entries = db.list_entries_for_session(session.id).unwrap();
    assert_eq!(entries.len(), 1, "one exercise entry expected");
    let sets = db.list_sets_for_entry(entries[0].id).unwrap();
    assert_eq!(sets.len(), 3, "three sets should be logged");
    assert!(sets.iter().all(|s| s.count == Some(8) && (s.value - 80.0).abs() < 1e-6));
}

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use base64::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::User;

use super::AppState;

type HmacSha256 = Hmac<Sha256>;

// ── Telegram Login Widget verification ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TelegramLoginParams {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub photo_url: Option<String>,
    pub auth_date: i64,
    pub hash: String,
}

pub fn verify_telegram_login(params: &TelegramLoginParams, bot_token: &str) -> bool {
    // Reject stale auth data (>5 minutes old)
    let now = chrono::Utc::now().timestamp();
    if now - params.auth_date > 300 {
        return false;
    }

    // Build data-check-string: alphabetically sorted key=value pairs, excluding "hash"
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("auth_date={}", params.auth_date));
    parts.push(format!("first_name={}", params.first_name));
    parts.push(format!("id={}", params.id));
    if let Some(ref v) = params.last_name {
        parts.push(format!("last_name={v}"));
    }
    if let Some(ref v) = params.photo_url {
        parts.push(format!("photo_url={v}"));
    }
    if let Some(ref v) = params.username {
        parts.push(format!("username={v}"));
    }
    parts.sort();
    let data_check_string = parts.join("\n");

    // secret_key = SHA256(bot_token)
    let secret_key = sha2::Sha256::digest(bot_token.as_bytes());

    // Verify HMAC-SHA256(data_check_string, secret_key) against the provided hash
    let mut mac = HmacSha256::new_from_slice(&secret_key).expect("HMAC accepts any key size");
    mac.update(data_check_string.as_bytes());
    mac.verify_slice(&hex::decode(&params.hash).unwrap_or_default()).is_ok()
}

// ── Session cookies ───────────────────────────────────────────────────────────

pub const SESSION_COOKIE_NAME: &str = "corre_gym_session";
const SESSION_MAX_AGE_SECS: i64 = 30 * 24 * 3600; // 30 days

#[derive(Debug, Serialize, Deserialize)]
struct SessionPayload {
    user_id: i64,
    telegram_id: i64,
    name: String,
    created_at: i64,
}

fn signing_key(state: &AppState) -> Vec<u8> {
    let key_material = state.session_secret.as_deref().unwrap_or(&state.config.telegram_bot_token);
    sha2::Sha256::digest(key_material.as_bytes()).to_vec()
}

pub fn create_session_cookie(state: &AppState, user: &User, telegram_id: i64) -> String {
    let payload = SessionPayload { user_id: user.id, telegram_id, name: user.name.clone(), created_at: chrono::Utc::now().timestamp() };
    let json = serde_json::to_string(&payload).expect("session payload serializes");
    let b64 = BASE64_URL_SAFE_NO_PAD.encode(json.as_bytes());

    let key = signing_key(state);
    let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key size");
    mac.update(b64.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());

    let cookie_value = format!("{b64}.{sig}");

    // Infer Secure flag from bind address
    let secure = !state.config.bind.starts_with("127.0.0.1") && !state.config.bind.starts_with("localhost");
    let secure_flag = if secure { "; Secure" } else { "" };

    format!("{SESSION_COOKIE_NAME}={cookie_value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_MAX_AGE_SECS}{secure_flag}")
}

pub fn create_logout_cookie() -> String {
    format!("{SESSION_COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

fn verify_session_cookie(cookie_value: &str, state: &AppState) -> Option<SessionPayload> {
    let (b64_part, sig_hex) = cookie_value.split_once('.')?;

    let key = signing_key(state);
    let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key size");
    mac.update(b64_part.as_bytes());
    mac.verify_slice(&hex::decode(sig_hex).ok()?).ok()?;

    let json_bytes = BASE64_URL_SAFE_NO_PAD.decode(b64_part).ok()?;
    let payload: SessionPayload = serde_json::from_slice(&json_bytes).ok()?;

    // Check expiry (30 days)
    let now = chrono::Utc::now().timestamp();
    if now - payload.created_at > SESSION_MAX_AGE_SECS {
        return None;
    }

    Some(payload)
}

// ── AuthUser extractor ────────────────────────────────────────────────────────

pub struct AuthUser {
    pub user: User,
}

impl FromRequestParts<Arc<AppState>> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let cookie_header = parts.headers.get(axum::http::header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("");

        let session_value = cookie_header
            .split(';')
            .map(|c| c.trim())
            .find(|c| c.starts_with(&format!("{SESSION_COOKIE_NAME}=")))
            .and_then(|c| c.strip_prefix(&format!("{SESSION_COOKIE_NAME}=")));

        let Some(session_value) = session_value else {
            return Err(auth_rejection(parts));
        };

        let Some(payload) = verify_session_cookie(session_value, state) else {
            return Err(auth_rejection(parts));
        };

        let db = state.db.lock().await;
        let user = db.get_user(payload.user_id).ok().flatten();

        match user {
            Some(user) => Ok(AuthUser { user }),
            None => Err(auth_rejection(parts)),
        }
    }
}

fn auth_rejection(parts: &Parts) -> Response {
    let path = parts.uri.path();
    if path.starts_with("/api/") { StatusCode::UNAUTHORIZED.into_response() } else { Redirect::to("/login").into_response() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GymConfig;
    use crate::db::{Database, new_user};
    use dashmap::DashMap;
    use tokio::sync::Mutex;

    fn test_state() -> AppState {
        let db = Database::open_in_memory().unwrap();
        AppState {
            db: Arc::new(Mutex::new(db)),
            handler: None,
            config: GymConfig {
                bind: "127.0.0.1:5520".to_string(),
                telegram_bot_token: "123456:ABC-DEF".to_string(),
                telegram_allowed_ids: vec![],
                default_timezone: "UTC".to_string(),
                conversation_history_limit: 20,
                db_path: "test.db".to_string(),
                max_message_length: 2000,
                session_timeout_hours: 4,
                llm: None,
                voice: None,
                github: None,
            },
            bot_username: "test_bot".to_string(),
            session_secret: None,
            chat_rate_limiter: DashMap::new(),
        }
    }

    fn test_user() -> User {
        new_user("Alice", Some("12345"), "UTC")
    }

    #[test]
    fn verify_telegram_login_valid() {
        // Create a valid hash manually
        let bot_token = "123456:ABC-DEF";
        let auth_date = chrono::Utc::now().timestamp();

        let mut parts = vec![format!("auth_date={auth_date}"), "first_name=Alice".to_string(), "id=12345".to_string()];
        parts.sort();
        let data_check = parts.join("\n");

        let secret_key = sha2::Sha256::digest(bot_token.as_bytes());
        let mut mac = HmacSha256::new_from_slice(&secret_key).unwrap();
        mac.update(data_check.as_bytes());
        let hash = hex::encode(mac.finalize().into_bytes());

        let params = TelegramLoginParams {
            id: 12345,
            first_name: "Alice".to_string(),
            last_name: None,
            username: None,
            photo_url: None,
            auth_date,
            hash,
        };

        assert!(verify_telegram_login(&params, bot_token));
    }

    #[test]
    fn verify_telegram_login_tampered() {
        let bot_token = "123456:ABC-DEF";
        let auth_date = chrono::Utc::now().timestamp();

        // Create valid hash for id=12345
        let mut parts = vec![format!("auth_date={auth_date}"), "first_name=Alice".to_string(), "id=12345".to_string()];
        parts.sort();
        let data_check = parts.join("\n");

        let secret_key = sha2::Sha256::digest(bot_token.as_bytes());
        let mut mac = HmacSha256::new_from_slice(&secret_key).unwrap();
        mac.update(data_check.as_bytes());
        let hash = hex::encode(mac.finalize().into_bytes());

        // Tamper with id
        let params = TelegramLoginParams {
            id: 99999,
            first_name: "Alice".to_string(),
            last_name: None,
            username: None,
            photo_url: None,
            auth_date,
            hash,
        };

        assert!(!verify_telegram_login(&params, bot_token));
    }

    #[test]
    fn verify_telegram_login_expired() {
        let bot_token = "123456:ABC-DEF";
        let auth_date = chrono::Utc::now().timestamp() - 600; // 10 min ago

        let params = TelegramLoginParams {
            id: 12345,
            first_name: "Alice".to_string(),
            last_name: None,
            username: None,
            photo_url: None,
            auth_date,
            hash: "deadbeef".to_string(),
        };

        assert!(!verify_telegram_login(&params, bot_token));
    }

    #[test]
    fn verify_telegram_login_bad_hex_hash() {
        let bot_token = "123456:ABC-DEF";
        let auth_date = chrono::Utc::now().timestamp();

        let params = TelegramLoginParams {
            id: 12345,
            first_name: "Alice".to_string(),
            last_name: None,
            username: None,
            photo_url: None,
            auth_date,
            hash: "zzzz_not_hex".to_string(),
        };

        assert!(!verify_telegram_login(&params, bot_token));
    }

    #[test]
    fn session_cookie_round_trip() {
        let state = test_state();
        let user = test_user();

        let cookie = create_session_cookie(&state, &user, 12345);
        // Extract cookie value
        let value = cookie.split(';').next().unwrap().strip_prefix(&format!("{SESSION_COOKIE_NAME}=")).unwrap();

        let payload = verify_session_cookie(value, &state).unwrap();
        assert_eq!(payload.user_id, user.id);
        assert_eq!(payload.telegram_id, 12345);
        assert_eq!(payload.name, "Alice");
    }

    #[test]
    fn session_cookie_wrong_key() {
        let state = test_state();
        let user = test_user();

        let cookie = create_session_cookie(&state, &user, 12345);
        let value = cookie.split(';').next().unwrap().strip_prefix(&format!("{SESSION_COOKIE_NAME}=")).unwrap();

        // Verify with a different state (different bot token)
        let mut other_state = test_state();
        other_state.config.telegram_bot_token = "different:token".to_string();
        assert!(verify_session_cookie(value, &other_state).is_none());
    }

    #[test]
    fn session_cookie_expired() {
        let state = test_state();

        // Manually create an expired payload
        let payload = SessionPayload {
            user_id: 1,
            telegram_id: 12345,
            name: "Test".to_string(),
            created_at: chrono::Utc::now().timestamp() - SESSION_MAX_AGE_SECS - 100,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let b64 = BASE64_URL_SAFE_NO_PAD.encode(json.as_bytes());

        let key = signing_key(&state);
        let mut mac = HmacSha256::new_from_slice(&key).unwrap();
        mac.update(b64.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        let value = format!("{b64}.{sig}");
        assert!(verify_session_cookie(&value, &state).is_none());
    }

    #[test]
    fn session_secret_overrides_bot_token() {
        let mut state = test_state();
        state.session_secret = Some("custom-secret".to_string());
        let user = test_user();

        let cookie = create_session_cookie(&state, &user, 12345);
        let value = cookie.split(';').next().unwrap().strip_prefix(&format!("{SESSION_COOKIE_NAME}=")).unwrap();

        // Should verify with session_secret
        assert!(verify_session_cookie(value, &state).is_some());

        // Should NOT verify without session_secret (falls back to bot_token)
        let mut no_secret_state = test_state();
        no_secret_state.session_secret = None;
        assert!(verify_session_cookie(value, &no_secret_state).is_none());
    }

    #[test]
    fn logout_cookie_clears_session() {
        let cookie = create_logout_cookie();
        assert!(cookie.contains("Max-Age=0"));
    }

    #[test]
    fn local_bind_omits_secure_flag() {
        let state = test_state();
        let user = test_user();
        let cookie = create_session_cookie(&state, &user, 12345);
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn remote_bind_includes_secure_flag() {
        let mut state = test_state();
        state.config.bind = "0.0.0.0:5520".to_string();
        let user = test_user();
        let cookie = create_session_cookie(&state, &user, 12345);
        assert!(cookie.contains("Secure"));
    }
}

pub mod api;
pub mod auth;
pub mod handlers;

use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use dashmap::DashMap;
use rust_embed::Embed;
use tokio::sync::Mutex;

use crate::assistant::AssistantHandler;
use crate::config::GymConfig;
use crate::db::Database;

pub struct AppState {
    pub db: Arc<Mutex<Database>>,
    pub handler: Option<Arc<AssistantHandler>>,
    pub config: GymConfig,
    pub bot_username: String,
    pub session_secret: Option<String>,
    pub chat_rate_limiter: DashMap<i64, Vec<Instant>>,
}

#[derive(Embed)]
#[folder = "static/"]
struct Assets;

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // HTML pages
        .route("/", get(handlers::dashboard))
        .route("/login", get(handlers::login))
        .route("/logout", get(handlers::logout))
        .route("/history", get(handlers::history))
        .route("/progress", get(handlers::progress))
        .route("/chat", get(handlers::chat))
        // Auth callback
        .route("/auth/telegram", get(handlers::telegram_login_callback))
        // JSON API
        .route("/api/sets", get(api::sets))
        .route("/api/sets/{id}", put(api::edit_set))
        .route("/api/progress/exercise", get(api::progress_exercise))
        .route("/api/progress/volume", get(api::progress_volume))
        .route("/api/progress/frequency", get(api::progress_frequency))
        .route("/api/progress/records", get(api::progress_records))
        .route("/api/goals", get(api::goals))
        .route("/api/health", get(api::health))
        .route("/api/schedule", get(api::schedule))
        .route("/api/chat", post(api::chat_send))
        .route("/api/chat/history", get(api::chat_history))
        .route("/api/user", get(api::user_profile))
        .route("/api/group/{id}/members", get(api::group_members))
        // Liveness probe (unauthenticated)
        .route("/api/ping", get(|| async { "ok" }))
        // Static assets
        .route("/static/{*path}", get(static_handler))
        .with_state(state)
}

async fn static_handler(axum::extract::Path(path): axum::extract::Path<String>) -> impl IntoResponse {
    let mime = if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".js") {
        "application/javascript"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else {
        "application/octet-stream"
    };

    match Assets::get(&path) {
        Some(content) => {
            ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, "public, max-age=3600")], content.data.to_vec()).into_response()
        }
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn serve(
    bind: &str,
    db: Arc<Mutex<Database>>,
    handler: Arc<AssistantHandler>,
    config: GymConfig,
    bot_username: String,
    session_secret: Option<String>,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState { db, handler: Some(handler), config, bot_username, session_secret, chat_rate_limiter: DashMap::new() });

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(bind = %bind, "Dashboard web server started");
    axum::serve(listener, router).await?;
    Ok(())
}

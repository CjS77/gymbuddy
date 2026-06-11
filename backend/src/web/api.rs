use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::auth::AuthUser;

// ── Pagination ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl PaginationParams {
    fn resolve(&self) -> (i64, i64) {
        let limit = self.limit.unwrap_or(100).min(500).max(1);
        let offset = self.offset.unwrap_or(0).max(0);
        (limit, offset)
    }
}

#[derive(Debug, Serialize)]
pub struct Paginated<T: Serialize> {
    pub data: T,
    pub limit: i64,
    pub offset: i64,
    pub total: i64,
}

// ── Access control helper ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UserFilter {
    pub user_id: Option<i64>,
}

async fn resolve_target_user(auth: &AuthUser, state: &AppState, requested_user_id: Option<i64>) -> Result<i64, axum::response::Response> {
    match requested_user_id {
        Some(target_id) if target_id != auth.user.id => {
            let db = state.db.lock().await;
            if !db.can_read(auth.user.id, target_id).unwrap_or(false) {
                return Err(StatusCode::FORBIDDEN.into_response());
            }
            Ok(target_id)
        }
        _ => Ok(auth.user.id),
    }
}

// ── Rate limiter ──────────────────────────────────────────────────────────────

const CHAT_RATE_LIMIT: usize = 10;
const CHAT_RATE_WINDOW_SECS: u64 = 60;

fn check_rate_limit(state: &AppState, user_id: i64) -> Result<(), (StatusCode, String)> {
    let now = Instant::now();
    let mut entry = state.chat_rate_limiter.entry(user_id).or_default();
    let window_start = now - std::time::Duration::from_secs(CHAT_RATE_WINDOW_SECS);

    entry.retain(|t| *t > window_start);

    if entry.len() >= CHAT_RATE_LIMIT {
        let oldest = entry.first().copied().unwrap_or(now);
        let retry_after = CHAT_RATE_WINDOW_SECS.saturating_sub(oldest.elapsed().as_secs());
        return Err((StatusCode::TOO_MANY_REQUESTS, format!("Rate limit exceeded. Retry after {retry_after}s")));
    }

    entry.push(now);
    Ok(())
}

// ── Sets (formerly logs) ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SetsQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub exercise_type_id: Option<i64>,
    #[serde(default)]
    pub include_descendants: bool,
    #[serde(flatten)]
    pub user: UserFilter,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

pub async fn sets(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<SetsQuery>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let (limit, offset) = q.pagination.resolve();
    let db = state.db.lock().await;
    match db.get_sets_paginated(user_id, q.from.as_deref(), q.to.as_deref(), q.exercise_type_id, q.include_descendants, limit, offset) {
        Ok((data, total)) => Json(Paginated { data, limit, offset, total }).into_response(),
        Err(e) => {
            tracing::error!("Failed to get sets: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Edit a logged set ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct EditSetBody {
    /// New exercise, by name — resolved against the catalogue. Reclassifies just
    /// this one set; whole-block reclassification is an assistant-only feature.
    #[serde(default)]
    pub exercise: Option<String>,
    #[serde(default)]
    pub reps: Option<i32>,
    /// New measured value: weight_kg, duration_secs, or distance_m by type.
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub perceived_difficulty: Option<crate::db::Difficulty>,
    #[serde(default)]
    pub comment: Option<String>,
}

/// `PUT /api/sets/{id}` — edit a single logged set. Edits are restricted to the
/// authenticated user's own sets (or sets they have group write access to);
/// `Database::edit_set` enforces this. A non-existent set and a set owned by
/// someone else both return 404 so set ids cannot be probed.
pub async fn edit_set(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<EditSetBody>,
) -> impl IntoResponse {
    use crate::db::{SetEdit, SetEditError};

    if body.reps.is_some_and(|r| r < 0) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": "reps must be >= 0" }))).into_response();
    }
    if body.value.is_some_and(|v| !v.is_finite() || v < 0.0) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": "value must be a non-negative number" })))
            .into_response();
    }

    let db = state.db.lock().await;
    let catalogue = match db.list_exercise_types_with_ancestry() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to load exercise catalogue: {e:#}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let exercise_type_id = match &body.exercise {
        Some(name) => match crate::assistant::matching::find_exercise_type(&catalogue, name) {
            Some(et) => Some(et.exercise_type.id),
            None => {
                return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": format!("unknown exercise: {name}") })))
                    .into_response();
            }
        },
        None => None,
    };

    let edit = SetEdit {
        exercise_type_id,
        count: body.reps.map(Some),
        value: body.value,
        perceived_difficulty: body.perceived_difficulty,
        comment: body.comment.clone().map(Some),
    };

    match db.edit_set(id, auth.user.id, &catalogue, &edit) {
        Ok(outcome) => Json(outcome.after).into_response(),
        Err(SetEditError::NotFound(_) | SetEditError::Forbidden(_)) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "set not found" }))).into_response()
        }
        Err(e @ (SetEditError::MeasurementTypeMismatch { .. } | SetEditError::Empty)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": e.to_string() }))).into_response()
        }
        Err(SetEditError::Db(e)) => {
            tracing::error!("Failed to edit set {id}: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Progress: exercise time series ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ExerciseProgressQuery {
    pub exercise_type_id: i64,
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default)]
    pub include_descendants: bool,
    #[serde(flatten)]
    pub user: UserFilter,
}

pub async fn progress_exercise(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(q): Query<ExerciseProgressQuery>,
) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let db = state.db.lock().await;
    match db.exercise_time_series(user_id, q.exercise_type_id, q.from.as_deref(), q.to.as_deref(), q.include_descendants) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get exercise progress: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Progress: volume per muscle group ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct VolumeQuery {
    pub weeks: Option<i32>,
    #[serde(flatten)]
    pub user: UserFilter,
}

pub async fn progress_volume(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<VolumeQuery>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let weeks = q.weeks.unwrap_or(12);
    let period = format!("-{} days", weeks * 7);
    let db = state.db.lock().await;
    match db.volume_by_muscle_group_weekly(user_id, &period) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get volume data: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Progress: frequency ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FrequencyQuery {
    pub weeks: Option<i32>,
    #[serde(flatten)]
    pub user: UserFilter,
}

pub async fn progress_frequency(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<FrequencyQuery>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let weeks = q.weeks.unwrap_or(12);
    let db = state.db.lock().await;
    match db.session_count_by_week(user_id, weeks) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get frequency data: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Progress: personal records ────────────────────────────────────────────────

pub async fn progress_records(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<UserFilter>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let db = state.db.lock().await;
    match db.personal_records(user_id) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get personal records: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Goals ─────────────────────────────────────────────────────────────────────

pub async fn goals(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<UserFilter>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let db = state.db.lock().await;
    match db.goal_progress_report(user_id, None, None) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get goals: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Health ────────────────────────────────────────────────────────────────────

pub async fn health(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<UserFilter>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let db = state.db.lock().await;
    match db.list_active_health_entries(user_id) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get health entries: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Schedule ──────────────────────────────────────────────────────────────────

pub async fn schedule(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<UserFilter>) -> impl IntoResponse {
    let user_id = match resolve_target_user(&auth, &state, q.user_id).await {
        Ok(id) => id,
        Err(e) => return e,
    };

    let db = state.db.lock().await;
    match db.list_schedules(user_id) {
        Ok(data) => Json(data).into_response(),
        Err(e) => {
            tracing::error!("Failed to get schedules: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── Chat ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatSendBody {
    pub message: String,
}

pub async fn chat_send(auth: AuthUser, State(state): State<Arc<AppState>>, Json(body): Json<ChatSendBody>) -> impl IntoResponse {
    if let Err((status, msg)) = check_rate_limit(&state, auth.user.id) {
        return (status, Json(serde_json::json!({ "error": msg }))).into_response();
    }

    let Some(ref handler) = state.handler else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({ "error": "Chat not available" }))).into_response();
    };

    match handler.handle_message_for_user(&auth.user, &body.message, "web").await {
        Ok(reply) => Json(serde_json::json!({ "reply": reply.text })).into_response(),
        Err(e) => {
            tracing::error!("Chat error: {e:#}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "Processing failed" }))).into_response()
        }
    }
}

// ── Chat history ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ChatHistoryQuery {
    pub limit: Option<usize>,
}

pub async fn chat_history(auth: AuthUser, State(state): State<Arc<AppState>>, Query(q): Query<ChatHistoryQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).min(200);
    let db = state.db.lock().await;
    match db.get_recent_messages_for_platform(auth.user.id, "web", limit) {
        Ok(mut data) => {
            // Assistant rows are persisted as the raw `{"message": ..., "actions": ...}` envelope
            // so the LLM sees its own structured output in history. Strip the envelope here so
            // the chat UI shows just the human-readable message.
            for msg in data.iter_mut().filter(|m| m.role == crate::db::ConversationRole::Assistant) {
                msg.content = crate::assistant::parser::parse_assistant_response(&msg.content).message;
            }
            Json(data).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to get chat history: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ── User profile ──────────────────────────────────────────────────────────────

pub async fn user_profile(auth: AuthUser) -> impl IntoResponse {
    Json(serde_json::json!({
        "id": auth.user.id,
        "name": auth.user.name,
        "telegram_id": auth.user.telegram_id,
        "timezone": auth.user.timezone,
        "created_at": auth.user.created_at,
    }))
}

// ── Group members ─────────────────────────────────────────────────────────────

pub async fn group_members(auth: AuthUser, State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> impl IntoResponse {
    let db = state.db.lock().await;

    match db.is_group_member(auth.user.id, id) {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(e) => {
            tracing::error!("Failed to check group membership: {e:#}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    match db.list_group_members(id) {
        Ok(members) => {
            let data: Vec<serde_json::Value> = members
                .iter()
                .map(|(user, level)| {
                    serde_json::json!({
                        "id": user.id,
                        "name": user.name,
                        "level": level.as_str(),
                    })
                })
                .collect();
            Json(data).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to list group members: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

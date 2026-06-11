use std::sync::Arc;

use askama::Template;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Redirect};

use super::AppState;
use super::auth::{AuthUser, TelegramLoginParams, create_logout_cookie, create_session_cookie, verify_telegram_login};
use crate::db::{ExerciseTypeWithAncestry, new_user};

// ── Templates ─────────────────────────────────────────────────────────────────
#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    bot_username: String,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    #[allow(dead_code)]
    user_name: String,
    active_page: String,
}

#[derive(Template)]
#[template(path = "history.html")]
struct HistoryTemplate {
    #[allow(dead_code)]
    user_name: String,
    exercises_json: String,
    active_page: String,
}

#[derive(Template)]
#[template(path = "progress.html")]
struct ProgressTemplate {
    #[allow(dead_code)]
    user_name: String,
    exercises_json: String,
    active_page: String,
}

#[derive(Template)]
#[template(path = "chat.html")]
struct ChatTemplate {
    #[allow(dead_code)]
    user_name: String,
    active_page: String,
}

// ── Page handlers ─────────────────────────────────────────────────────────────

pub async fn login(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let template = LoginTemplate { bot_username: state.bot_username.clone() };
    Html(template.render().unwrap_or_else(|e| format!("Template error: {e}")))
}

pub async fn dashboard(auth: AuthUser, State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let template = DashboardTemplate { user_name: auth.user.name.clone(), active_page: "dashboard".to_string() };
    Html(template.render().unwrap_or_else(|e| format!("Template error: {e}")))
}

pub async fn history(auth: AuthUser, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let catalogue = {
        let db = state.db.lock().await;
        db.list_exercise_types_with_ancestry().unwrap_or_default()
    };
    let exercises_json = exercise_types_to_json(&catalogue);
    let template = HistoryTemplate { user_name: auth.user.name.clone(), exercises_json, active_page: "history".to_string() };
    Html(template.render().unwrap_or_else(|e| format!("Template error: {e}")))
}

pub async fn progress(auth: AuthUser, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let catalogue = {
        let db = state.db.lock().await;
        db.list_exercise_types_with_ancestry().unwrap_or_default()
    };
    let exercises_json = exercise_types_to_json(&catalogue);
    let template = ProgressTemplate { user_name: auth.user.name.clone(), exercises_json, active_page: "progress".to_string() };
    Html(template.render().unwrap_or_else(|e| format!("Template error: {e}")))
}

pub async fn chat(auth: AuthUser, State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let template = ChatTemplate { user_name: auth.user.name.clone(), active_page: "chat".to_string() };
    Html(template.render().unwrap_or_else(|e| format!("Template error: {e}")))
}

pub async fn logout(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let cookie = create_logout_cookie();
    ([(header::SET_COOKIE, cookie)], Redirect::to("/login"))
}

// ── Auth callback ─────────────────────────────────────────────────────────────

pub async fn telegram_login_callback(State(state): State<Arc<AppState>>, Query(params): Query<TelegramLoginParams>) -> impl IntoResponse {
    if !verify_telegram_login(&params, &state.config.telegram_bot_token) {
        return (axum::http::StatusCode::FORBIDDEN, "Invalid authentication").into_response();
    }

    let telegram_id_str = params.id.to_string();

    // Look up or auto-register user
    let user = {
        let db = state.db.lock().await;
        db.get_user_by_telegram_id(&telegram_id_str).ok().flatten()
    };

    let user = match user {
        Some(user) => user,
        None => {
            let name = match &params.last_name {
                Some(last) => format!("{} {last}", params.first_name),
                None => params.first_name.clone(),
            };
            let draft = new_user(&name, Some(&telegram_id_str), "UTC");
            let db = state.db.lock().await;
            let user_id = match db.insert_user(&draft) {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!("Failed to auto-register user: {e:#}");
                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Registration failed").into_response();
                }
            };
            let user = match db.get_user(user_id) {
                Ok(Some(u)) => u,
                _ => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Registration failed").into_response(),
            };
            tracing::info!(telegram_id = %telegram_id_str, name = %user.name, "Auto-registered user via web login");
            user
        }
    };

    let cookie = create_session_cookie(&state, &user, params.id);
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn exercise_types_to_json(catalogue: &[ExerciseTypeWithAncestry]) -> String {
    let list: Vec<serde_json::Value> = catalogue
        .iter()
        .filter(|e| matches!(e.exercise_type.level, crate::db::ExerciseLevel::Exercise | crate::db::ExerciseLevel::Variation))
        .map(|e| {
            serde_json::json!({
                "id": e.exercise_type.id,
                "name": e.exercise_type.name,
                "level": e.exercise_type.level.as_str(),
                "parent_id": e.exercise_type.parent_id,
                "muscle_group": e.muscle_group,
                "specific_muscle": e.specific_muscle,
                "exercise": e.exercise,
            })
        })
        .collect();
    serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
}

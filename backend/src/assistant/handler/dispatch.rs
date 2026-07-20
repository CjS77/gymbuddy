//! Slash-command dispatch, and the direct command implementations: help/start
//! text, status, history, the exercise catalogue, /clear, /timers and the
//! beta-gated /feedback.

use crate::assistant::commands::{self, Command};
use crate::db::{ExerciseTypeWithAncestry, User};

use super::{AssistantHandler, Reply, set_line};
use gymbuddy_proto::{
    CatalogEntry, CatalogGroup, CatalogView, ExerciseLog, HealthNote, HistoryView, SessionSummaryView, SessionView, StatusView,
    TimerSignal, View,
};

impl AssistantHandler {
    /// Handle a slash command, if `text` is one. Most commands map straight to a
    /// [`View`]; `/timers` additionally rides a [`TimerSignal`] so disabling it can
    /// cancel an in-flight rest, hence the [`Reply`] return.
    ///
    /// The match is exhaustive over [`Command`], so a new row in the command table
    /// cannot be advertised without also being handled here.
    pub(super) async fn handle_command(&self, text: &str, user: &User, platform: &str) -> anyhow::Result<Option<Reply>> {
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
            Command::Programme => Ok(Some(self.cmd_programme_start(user, platform).await?.into())),
            Command::Progress => Ok(Some(self.cmd_progress(user).await?.into())),
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
        use crate::assistant::prompts::capitalize;

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use super::super::test_support::*;
    use crate::github::IssueReporter;
    use crate::render::Telegram;
    use gymbuddy_proto::Render as _;

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
    async fn slash_clear_excludes_prior_messages() {
        let (handler, _) = setup_handler(r#"{"message": "Got it!", "actions": []}"#).await;
        let msg = make_message(12345, "hello");

        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "how are you").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let msgs = db.get_recent_messages_for_platform(user.id, "telegram", 100).unwrap();
        assert_eq!(msgs.len(), 4, "the onboarding welcome is a turn too, so two turns are stored");
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
            User { id: 7, name: "Alice".into(), telegram_id: Some("999".into()), pubkey: None, timezone: "UTC".into(),
                created_at: String::new(), updated_at: String::new(), beta_tester: true, timers_enabled: true };
        let body = build_feedback_body(&user, "the squat rack is broken");
        assert!(body.starts_with("the squat rack is broken"));
        assert!(body.contains("Reported by: Alice"));
        assert!(body.contains("telegram_id: 999"));
    }

    // ─── command lists ────────────────────────────────────────────────────────

    fn command_list_user(beta_tester: bool) -> User {
        User { id: 7, name: "Alice".into(), telegram_id: None, pubkey: None, timezone: "UTC".into(),
            created_at: String::new(), updated_at: String::new(), beta_tester, timers_enabled: true }
    }

    #[test]
    fn help_lists_every_command_the_user_can_run() {
        let msg = AssistantHandler::cmd_help(&command_list_user(false));
        assert!(msg.starts_with("Available commands:\n/start -- Introduction and registration\n"));
        assert!(msg.contains("\n/cancel -- Cancel an in-progress interview (e.g. /philosophy or /programme)\n"));
        // [C4.2]: `/programme` must reach the help, since K-49's onboarding copy names it.
        assert!(msg.contains("\n/programme -- Build a multi-week programme"), "help must advertise /programme: {msg}");
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
}

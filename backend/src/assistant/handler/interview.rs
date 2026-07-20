//! The `/philosophy` interview: a multi-turn LLM interview that distils the
//! user's training philosophy, plus the free-text routing while one is in
//! progress.

use crate::assistant::actions::AssistantAction;
use crate::assistant::parser::parse_assistant_response;
use crate::assistant::prompts::build_philosophy_prompt;
use crate::db::{InterviewState, User};

use super::AssistantHandler;
use gymbuddy_proto::View;

/// What the user is asked once their philosophy lands: the next thing worth having
/// is a goal, and stating one in ordinary chat is enough — `set_goal` picks it up,
/// so no second interview is needed. `/programme` is named but not pressed; it only
/// pays off once there is a goal for it to serve.
const PHILOSOPHY_SAVED_FOLLOW_UP: &str = "What's one goal you want to aim at — a lift number, a bodyweight figure, or a \
weekly training habit? Just tell me and I'll set it up. After that, /nextworkout designs today's session and /programme \
builds the multi-week programme behind it.";

impl AssistantHandler {
    /// Enter the multi-turn `/philosophy` interview and return the opening question.
    /// Turn 0 is a fixed prompt (no LLM call); subsequent free-text turns are routed
    /// through [`Self::philosophy_interview_turn`]. The opener is stored as
    /// conversation so the interview thread stays coherent.
    pub(super) async fn cmd_philosophy_start(&self, user: &User, platform: &str) -> anyhow::Result<View> {
        let existing = {
            let db = self.db.lock().await;
            db.set_interview_state(user.id, platform, "philosophy", "", 0)?;
            db.latest_philosophy(user.id)?
        };

        let opener = match existing {
            Some(p) => format!(
                "Let's refine your training philosophy. Here's what I have on file:\n\n\
                 \"{}\"\n\n\
                 What would you like to change or add? (or /cancel to keep it as is)",
                p.content
            ),
            None => "Let's build your training philosophy together -- it'll guide every workout I design for you.\n\n\
                 To start: how often do you want to train each week, and what's your main goal \
                 (building muscle, strength, cardio, general fitness, something else)?"
                .to_string(),
        };

        self.store_conversation_on_platform(user.id, platform, "/philosophy", &opener).await?;
        Ok(View::message(opener))
    }

    /// Cancel an in-progress interview. Generic over the modes: what it says depends on
    /// what was actually abandoned, since the two leave different things behind — a
    /// cancelled `/philosophy` writes nothing, while a cancelled `/programme` keeps any
    /// proposal it already made, as a draft that was simply never activated.
    pub(super) async fn cmd_cancel(&self, user: &User, platform: &str) -> anyhow::Result<View> {
        let db = self.db.lock().await;
        let Some(state) = db.get_interview_state(user.id, platform)? else {
            return Ok(View::notice("Nothing to cancel."));
        };
        db.clear_interview_state(user.id, platform)?;
        Ok(View::notice(match state.mode.as_str() {
            "programme" if state.draft.parse::<i64>().is_ok() => {
                "Cancelled -- I've kept that programme as a draft, so nothing is active. Run /programme again to pick it up."
            }
            "programme" => "Cancelled -- no programme was created.",
            _ => "Cancelled -- your workout philosophy is unchanged.",
        }))
    }

    /// Route free text through the interviewer prompt for whichever interview is in
    /// progress. Returns `None` when the user is not interviewing, so normal log/coach
    /// handling proceeds.
    pub(super) async fn maybe_handle_interview_mode(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Option<View>> {
        let state = self.db.lock().await.get_interview_state(user.id, platform)?;
        let Some(state) = state else { return Ok(None) };
        match state.mode.as_str() {
            "philosophy" => Ok(Some(self.philosophy_interview_turn(user, text, platform, &state).await?)),
            "programme" => Ok(Some(self.programme_interview_turn(user, text, platform, &state).await?)),
            other => {
                tracing::warn!("Clearing unknown interview mode {other:?} for user {}", user.id);
                self.db.lock().await.clear_interview_state(user.id, platform)?;
                Ok(None)
            }
        }
    }

    /// One turn of the `/philosophy` interview: build the interviewer prompt from
    /// the draft + active injuries, call the LLM, and either save the distilled
    /// philosophy (on `save_philosophy`) and exit, or ask the next question.
    async fn philosophy_interview_turn(&self, user: &User, text: &str, platform: &str, state: &InterviewState) -> anyhow::Result<View> {
        let (system_prompt, history) = {
            let db = self.db.lock().await;
            let injuries = db.list_active_health_entries(user.id)?;
            let history = db.get_recent_messages_for_platform(user.id, platform, self.config.conversation_history_limit)?;
            (build_philosophy_prompt(&state.draft, &injuries, state.turns), history)
        };

        let llm_response = match self.call_llm(&system_prompt, &history, text).await {
            Ok(response) => response,
            Err(e) => {
                tracing::error!("Philosophy interview LLM call failed: {e:#}");
                self.store_excluded_conversation_on_platform(user.id, platform, text, "interview error").await?;
                return Ok(View::notice("I had trouble with that -- could you say it again? (or /cancel to stop)"));
            }
        };

        let parsed = parse_assistant_response(&llm_response);
        let saved = parsed.actions.iter().find_map(|action| match action {
            AssistantAction::SavePhilosophy { content } => Some(content.clone()),
            _ => None,
        });

        self.store_conversation_on_platform(user.id, platform, text, &llm_response).await?;

        let db = self.db.lock().await;
        if let Some(content) = saved {
            db.insert_philosophy(user.id, &content, "interview")?;
            db.clear_interview_state(user.id, platform)?;
            let message = crate::text::strip_markdown(&parsed.message);
            let confirm = if message.trim().is_empty() {
                format!("Saved your training philosophy.\n\n{PHILOSOPHY_SAVED_FOLLOW_UP}")
            } else {
                format!("{message}\n\n(Saved.)\n\n{PHILOSOPHY_SAVED_FOLLOW_UP}")
            };
            return Ok(View::message(confirm));
        }

        db.set_interview_state(user.id, platform, "philosophy", &state.draft, state.turns + 1)?;
        Ok(View::message(crate::text::strip_markdown(&parsed.message)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;

    #[tokio::test]
    async fn philosophy_interview_saves_and_exits() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        // /philosophy opens the interview with a fixed opener and arms the mode.
        let opener = handler.handle_text_message(&msg, "/philosophy").await.unwrap();
        assert!(shown(&opener).to_lowercase().contains("philosophy"));
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_some());

        // A first answer keeps the interview going (no save action).
        llm.set_response(r#"{"message": "Great. What equipment do you have?", "actions": []}"#);
        let q2 = handler.handle_text_message(&msg, "3x a week, hypertrophy, I like 5x5").await.unwrap();
        assert!(shown(&q2).contains("equipment"));
        let state = handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().unwrap();
        assert_eq!(state.turns, 1, "turn counter should advance");

        // The next answer triggers save_philosophy: it is stored and the mode clears.
        llm.set_response(
            r#"{"message": "Locked in.", "actions": [
                {"type": "save_philosophy", "content": "goal=hypertrophy. Likes 5x5. Home gym: squat rack 120kg, dumbbells 24kg. 3x/week."}
            ]}"#,
        );
        let done = handler.handle_text_message(&msg, "squat rack to 120kg and dumbbells to 24kg").await.unwrap();
        assert!(shown(&done).contains("/nextworkout"), "confirmation should point at /nextworkout");
        // Saving the philosophy is the moment to ask for the next missing piece: a
        // goal, stated in ordinary chat, plus the programme that would serve it.
        assert!(shown(&done).to_lowercase().contains("goal"), "confirmation should ask about a first goal: {}", shown(&done));
        assert!(shown(&done).contains("/programme"), "confirmation should mention /programme: {}", shown(&done));

        let db = handler.db.lock().await;
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_none(), "mode should be cleared");
        let saved = db.latest_philosophy(user.id).unwrap().unwrap();
        assert!(saved.content.contains("hypertrophy") && saved.content.contains("120kg"));
        assert_eq!(saved.source, "interview");
    }

    /// The philosophy hand-off names the next commands to run, so each of them has to
    /// be one the dispatcher recognises — `/programme` was named here from [R1.7] but
    /// only became real in [C4.2].
    #[test]
    fn the_saved_philosophy_follow_up_names_only_real_commands() {
        let unknown = crate::assistant::commands::unknown_commands_in(super::PHILOSOPHY_SAVED_FOLLOW_UP);
        assert!(unknown.is_empty(), "the follow-up points at commands that do not exist: {unknown:?}");
    }

    #[tokio::test]
    async fn cancel_aborts_interview_without_saving() {
        let (handler, _llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let _ = handler.handle_text_message(&msg, "/philosophy").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_some());

        let cancelled = handler.handle_text_message(&msg, "/cancel").await.unwrap();
        assert!(shown(&cancelled).to_lowercase().contains("cancel"));

        let db = handler.db.lock().await;
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_none());
        assert!(db.latest_philosophy(user.id).unwrap().is_none(), "nothing should be saved");
    }
}

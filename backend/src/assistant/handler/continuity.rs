//! Session continuity: auto-closing stale sessions, and the server-enforced
//! "new workout or same session?" ask-and-resume flow around long gaps in an
//! active session.

use anyhow::Context as _;
use chrono::{NaiveDateTime, Utc};

use crate::assistant::prompts::{SESSION_CONTINUITY_ASK_HOURS, SESSION_CONTINUITY_HOURS};
use crate::db::{ConversationRole, Database, Session, User};

use super::{AssistantHandler, Reply};
use gymbuddy_proto::View;

impl AssistantHandler {
    pub(super) async fn close_stale_session(&self, user: &User) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        let Some(session) = db.get_active_session(user.id)? else {
            return Ok(());
        };

        let entries = db.list_entries_for_session(session.id)?;
        let last_activity = entries.last().map(|e| e.start_timestamp.clone()).unwrap_or_else(|| session.started_at.clone());

        let threshold_hours = self.config.session_timeout_hours as i64;
        if let Ok(last) = chrono::NaiveDateTime::parse_from_str(&last_activity, "%Y-%m-%d %H:%M:%S") {
            let elapsed = Utc::now().naive_utc() - last;
            if elapsed.num_hours() >= threshold_hours {
                tracing::info!("Auto-closing stale session {} (last activity: {last_activity})", session.id);
                db.end_session(session.id)?;
            }
        }

        Ok(())
    }

    /// Hard server-side enforcement of the SESSION CONTINUITY ask-window. If
    /// there is an active session whose last activity was between 0.5 and 12
    /// hours ago, and we have not already asked the user about it on the
    /// previous turn, reply with a canned question and skip the LLM entirely.
    /// Subsequent user replies (the "yes new" / "no same" answer) flow through
    /// the LLM normally because the gap-window flag flips on the assistant
    /// message we just stored.
    pub(super) async fn maybe_session_continuity_short_circuit(
        &self,
        user: &User,
        text: &str,
        platform: &str,
    ) -> anyhow::Result<Option<View>> {
        let (active_session, age_hours) = {
            let db = self.db.lock().await;
            let Some(session) = db.get_active_session(user.id)? else {
                return Ok(None);
            };
            let age = compute_last_activity_age_hours(&db, &session)?;
            (session, age)
        };
        if !(SESSION_CONTINUITY_ASK_HOURS..SESSION_CONTINUITY_HOURS).contains(&age_hours) {
            return Ok(None);
        }
        // If we already asked on the previous assistant turn, let the LLM
        // process the user's answer.
        let already_asked = {
            let db = self.db.lock().await;
            let recent = db.get_recent_messages_for_platform(user.id, platform, 4)?;
            recent
                .iter()
                .rev()
                .find(|m| m.role == ConversationRole::Assistant)
                .map(|m| contains_continuity_ask(&m.content))
                .unwrap_or(false)
        };
        if already_asked {
            return Ok(None);
        }
        let canned = format!(
            "It's been {age_hours:.1} hours since your last set in this session. \
Before I log \"{text}\", is this a new workout or the same session? Reply \"new \
workout\" to end the previous one and start fresh, or \"same workout\" to keep \
going in the existing session — and I'll log that set accordingly."
        );
        tracing::debug!(session_id = active_session.id, age_hours, "session continuity short-circuit: asking user");
        self.store_conversation_on_platform(user.id, platform, text, &canned).await?;
        Ok(Some(View::message(canned)))
    }

    /// Server-side counterpart to `maybe_session_continuity_short_circuit`: after
    /// we asked the user "new workout or same workout?", their reply needs to
    /// trigger end+start+log (for "new") or just log (for "same"). The small LLM
    /// is unreliable at emitting that compound action, so we do it here:
    ///   * Detect the previous assistant message was the canned ask.
    ///   * Detect the current user message is an affirmation/negation.
    ///   * Extract the original quoted exercise text from the canned ask.
    ///   * For "new": call end_session + start_session, then recurse with the
    ///     original text so the normal LLM path logs it (gap is now 0, no
    ///     short-circuit).
    ///   * For "same": bump the session's started_at to now (so the gap is 0),
    ///     then recurse with the original text.
    pub(super) async fn maybe_session_continuity_resume(&self, user: &User, text: &str, platform: &str) -> anyhow::Result<Option<Reply>> {
        let lowered = text.to_lowercase();
        let is_new = ["new workout", "new session", "yes new", "yes, new"].iter().any(|n| lowered.contains(n))
            || lowered.trim() == "yes"
            || lowered.trim() == "new";
        let is_same = ["same workout", "same session", "continuing", "continue", "no new"].iter().any(|n| lowered.contains(n))
            || lowered.trim() == "same";
        if !is_new && !is_same {
            return Ok(None);
        }
        let (prev_assistant, original_text) = {
            let db = self.db.lock().await;
            let recent = db.get_recent_messages_for_platform(user.id, platform, 6)?;
            // `recent` is oldest-first per `get_recent_messages_for_platform`'s
            // post-reverse. Walk newest-first so we pick the most recent
            // assistant turn (the canned ask) and the user turn that preceded
            // it (the original exercise message).
            let mut iter = recent.into_iter().rev();
            let mut prev_assistant: Option<String> = None;
            let mut original_text: Option<String> = None;
            for msg in iter.by_ref() {
                if msg.role == ConversationRole::Assistant {
                    prev_assistant = Some(msg.content);
                    break;
                }
            }
            for msg in iter {
                if msg.role == ConversationRole::User {
                    original_text = Some(msg.content);
                    break;
                }
            }
            (prev_assistant, original_text)
        };
        let Some(prev_assistant) = prev_assistant else { return Ok(None) };
        if !contains_continuity_ask(&prev_assistant) {
            return Ok(None);
        }
        let Some(quoted) = extract_continuity_quoted_text(&prev_assistant).or(original_text) else {
            return Ok(None);
        };

        if is_new {
            let session_id = {
                let db = self.db.lock().await;
                db.get_active_session(user.id)?.map(|s| s.id)
            };
            if let Some(id) = session_id {
                self.db.lock().await.end_session(id).context("ending session for continuity-new")?;
            }
            let new_id = self.db.lock().await.start_session(user.id, None).context("starting session for continuity-new")?.id;
            tracing::debug!(new_session_id = new_id, %quoted, "session continuity resume: NEW workout");
        } else {
            // "same workout" → reset the session start so the gap is 0 and the
            // short-circuit doesn't re-trigger on this turn. Look up the session
            // id under one lock, then run the UPDATE under a fresh lock — Tokio
            // Mutex guards held inside an `if let` would otherwise deadlock the
            // second `self.db.lock().await`.
            let session_id = {
                let db = self.db.lock().await;
                db.get_active_session(user.id)?.map(|s| s.id)
            };
            if let Some(sid) = session_id {
                let db = self.db.lock().await;
                db.conn()
                    .execute("UPDATE sessions SET started_at = datetime('now') WHERE id = ?1", rusqlite::params![sid])
                    .context("bumping session started_at for continuity-same")?;
            }
            tracing::debug!(%quoted, "session continuity resume: SAME workout");
        }

        // Persist the affirmation so the conversation history is consistent, then
        // recurse with the *original* exercise text. The recursive call goes
        // through the LLM normally; gap is now 0 so no short-circuit fires.
        self.store_conversation_on_platform(user.id, platform, text, "Got it — logging the pending set now.").await?;
        Box::pin(self.handle_message_for_user(user, &quoted, platform)).await.map(Some)
    }
}

/// Hours since the most-recent set in `session` was logged, falling back to the
/// session's `started_at` if no sets exist yet. Drives the SESSION CONTINUITY
/// rule in the system prompt. Timestamp parse failures are non-fatal and yield
/// `0.0` so the prompt simply omits the cutoff guidance for that turn.
pub(super) fn compute_last_activity_age_hours(db: &Database, session: &Session) -> anyhow::Result<f64> {
    let mut latest = parse_sqlite_datetime(&session.started_at);
    for entry in db.list_entries_for_session(session.id)? {
        for set in db.list_sets_for_entry(entry.id)? {
            if let Some(t) = parse_sqlite_datetime(&set.logged_at) {
                latest = match latest {
                    Some(prev) if prev >= t => Some(prev),
                    _ => Some(t),
                };
            }
        }
    }
    let Some(latest) = latest else { return Ok(0.0) };
    let now = Utc::now().naive_utc();
    Ok((now - latest).num_seconds().max(0) as f64 / 3600.0)
}

pub(super) fn parse_sqlite_datetime(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok()
}

/// Loose match for "did the previous assistant turn ask the session-continuity
/// question?" — used to avoid asking twice in a row. Mirrors the regex set in
/// `e2e/.../assertions::reply_asks_about_new_session`.
fn contains_continuity_ask(reply: &str) -> bool {
    let lower = reply.to_lowercase();
    ["new workout", "same workout", "picking up", "pick up where", "is this a new"].iter().any(|n| lower.contains(n))
}

/// Extract the verbatim exercise text from the canned continuity ask. The host
/// formats it as `Before I log "<TEXT>", is this a new workout ...`, so we
/// recover the contents between the first pair of double quotes.
fn extract_continuity_quoted_text(reply: &str) -> Option<String> {
    let start = reply.find('"')?;
    let rest = &reply[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

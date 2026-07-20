//! Prompt-context assembly: gathers the active session with its entries and
//! sets, the session roster in flight, leaked entries, health, history and
//! goals into the [`PromptContext`] behind each turn's system prompt.

use chrono::Utc;

use crate::assistant::prompts::{PromptContext, PromptEntry, PromptRosterExercise, RosterProgress, build_system_prompt};
use crate::db::{Database, ExerciseSet, ExerciseTypeWithAncestry, Session, User};

use super::continuity::compute_last_activity_age_hours;
use super::designer::draft_roster_within_window;
use super::{AssistantHandler, format_set_short};

impl AssistantHandler {
    pub(super) async fn build_context(&self, user: &User) -> anyhow::Result<String> {
        let db = self.db.lock().await;
        let active_session = db.get_active_session(user.id)?;
        let mut session_sets: Vec<(ExerciseSet, String)> = Vec::new();
        let mut session_entries: Vec<PromptEntry> = Vec::new();

        if let Some(session) = &active_session {
            let entries = db.list_entries_for_session(session.id)?;
            for entry in &entries {
                let sets = db.list_sets_for_entry(entry.id)?;
                let exercise_type_id = sets.first().map(|s| s.exercise_type_id);
                let exercise_name = exercise_type_id.map_or_else(|| "unknown".to_string(), |id| self.exercise_name(id));
                let summary_parts: Vec<String> = sets.iter().map(format_set_short).collect();
                session_entries.push(PromptEntry {
                    id: entry.id,
                    exercise_name: exercise_name.clone(),
                    set_count: sets.len(),
                    sets_summary: summary_parts.join(", "),
                    is_open: entry.end_timestamp.is_none(),
                });
                for set in sets {
                    session_sets.push((set, exercise_name.clone()));
                }
            }
        }

        // A `/nextworkout` design: bound to the current session (guided execution) or
        // freshly designed and ready to start. Sourced from `session_rosters`.
        let active_roster = self.build_active_roster(&db, user.id, active_session.as_ref(), &session_sets)?;

        // Leaked open entries: open EEs not belonging to the active session, OR open
        // entries in the active session (so the LLM can decide to close/delete before
        // a new session is requested).
        let leaked_open_entries = build_leaked_view(&db, &self.catalogue, user.id, active_session.as_ref().map(|s| s.id))?;

        let last_activity_age_hours = match &active_session {
            Some(session) => {
                let age = compute_last_activity_age_hours(&db, session)?;
                tracing::debug!(session_id = session.id, age_hours = age, "computed last_activity_age_hours");
                Some(age)
            }
            None => None,
        };

        let health_entries = db.list_active_health_entries(user.id)?;
        let recent_summaries = db.list_session_summaries(user.id, None, None)?;
        let recent_summaries: Vec<_> = recent_summaries.into_iter().take(5).collect();
        let recent_sets = db.list_recent_sets(user.id, 7)?;
        let session_set_ids: std::collections::HashSet<i64> = session_sets.iter().map(|(s, _)| s.id).collect();
        let recent_sets: Vec<_> = recent_sets.into_iter().filter(|s| !session_set_ids.contains(&s.id)).take(10).collect();
        let active_goals = db.goal_progress_report(user.id, None, None)?;

        let ctx = PromptContext {
            user_name: user.name.clone(),
            timezone: user.timezone.clone(),
            current_time: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            active_session,
            session_sets,
            session_entries,
            leaked_open_entries,
            active_roster,
            health_entries,
            recent_summaries,
            recent_sets,
            exercise_types: self.catalogue.clone(),
            active_goals,
            last_activity_age_hours,
        };

        Ok(build_system_prompt(&ctx))
    }

    /// Build the guided-execution view of a `/nextworkout` design. Prefers a roster
    /// bound to the active session (guided, in progress); otherwise surfaces the most
    /// recent designed-but-unstarted roster so the assistant can offer to run it.
    fn build_active_roster(
        &self,
        db: &Database,
        user_id: i64,
        active_session: Option<&Session>,
        session_sets: &[(ExerciseSet, String)],
    ) -> anyhow::Result<Option<RosterProgress>> {
        // Guided execution: a roster bound to the CURRENT session.
        if let Some(session) = active_session
            && let Some(roster) = db.active_roster_for_user(user_id)?
            && roster.session_id == Some(session.id)
        {
            let logged: std::collections::HashSet<i64> = session_sets.iter().map(|(s, _)| s.exercise_type_id).collect();
            return Ok(Some(self.roster_progress(db, &roster, &logged, true)?));
        }

        // Otherwise a freshly designed roster ready to start — but only while it is
        // recent, so a stale design does not resurface as "ready" days later.
        if let Some(roster) = db.latest_draft_roster(user_id)?
            && draft_roster_within_window(&roster.created_at, Utc::now().naive_utc())
        {
            return Ok(Some(self.roster_progress(db, &roster, &std::collections::HashSet::new(), false)?));
        }

        Ok(None)
    }

    /// Compute done/next/remaining for `roster`, treating any prescribed exercise whose
    /// type appears in `logged` as done.
    fn roster_progress(
        &self,
        db: &Database,
        roster: &crate::db::SessionRoster,
        logged: &std::collections::HashSet<i64>,
        started: bool,
    ) -> anyhow::Result<RosterProgress> {
        let exercises = db.list_roster_exercises(roster.id)?;
        let mut done = Vec::new();
        let mut next = None;
        for ex in &exercises {
            let name = self.exercise_name(ex.exercise_type_id);
            if logged.contains(&ex.exercise_type_id) {
                done.push(name);
            } else if next.is_none() {
                next = Some(PromptRosterExercise {
                    exercise_name: name,
                    target_sets: ex.target_sets,
                    target_reps: ex.target_reps,
                    target_weight_kg: ex.target_weight_kg,
                    target_secs: ex.target_secs,
                    notes: ex.notes.clone(),
                });
            }
        }
        let remaining = exercises.len().saturating_sub(done.len());
        Ok(RosterProgress { title: roster.title.clone(), started, done, next, remaining, override_note: roster.override_note.clone() })
    }
}

/// Build PromptEntry rows for any open exercise_entry the user has, so the prompt
/// (and LLM-driven cleanup logic) can see them. When `active_session_id` is given,
/// only entries inside that session are reported (the caller's contract: leaks are
/// what blocks a *new* session, not what's normal in the current one).
fn build_leaked_view(
    db: &Database,
    catalogue: &[ExerciseTypeWithAncestry],
    user_id: i64,
    active_session_id: Option<i64>,
) -> anyhow::Result<Vec<PromptEntry>> {
    let all_open = db.list_open_entries_for_user(user_id)?;
    let filtered: Vec<_> = all_open
        .into_iter()
        .filter(|e| match active_session_id {
            Some(sid) => e.session_id == Some(sid),
            None => true,
        })
        .collect();
    let mut views = Vec::with_capacity(filtered.len());
    for entry in filtered {
        let sets = db.list_sets_for_entry(entry.id)?;
        let exercise_name = sets
            .first()
            .and_then(|s| catalogue.iter().find(|e| e.exercise_type.id == s.exercise_type_id))
            .map(|e| e.exercise_type.name.clone())
            .unwrap_or_else(|| "unknown".to_string());
        views.push(PromptEntry {
            id: entry.id,
            exercise_name,
            set_count: sets.len(),
            sets_summary: sets.iter().map(format_set_short).collect::<Vec<_>>().join(", "),
            is_open: true,
        });
    }
    Ok(views)
}

//! Prompt-context assembly: gathers the active session with its entries and
//! sets, scheduled and designed plans, leaked entries, health, history and
//! goals into the [`PromptContext`] behind each turn's system prompt.

use chrono::Utc;

use crate::assistant::prompts::{
    ActivePlanView, EntryView, PlanExerciseView, PrescribedExercise, PromptContext, WorkoutPlanProgress, build_system_prompt,
};
use crate::db::{Database, ExerciseSet, ExerciseTypeWithAncestry, Session, User};

use super::continuity::compute_last_activity_age_hours;
use super::designer::proposed_plan_within_window;
use super::{AssistantHandler, format_set_short, parse_plan_from_notes};

impl AssistantHandler {
    pub(super) async fn build_context(&self, user: &User) -> anyhow::Result<String> {
        let db = self.db.lock().await;
        let active_session = db.get_active_session(user.id)?;
        let mut session_sets: Vec<(ExerciseSet, String)> = Vec::new();
        let mut session_entries: Vec<EntryView> = Vec::new();
        let mut closed_exercise_ids_in_session: Vec<i64> = Vec::new();

        if let Some(session) = &active_session {
            let entries = db.list_entries_for_session(session.id)?;
            for entry in &entries {
                let sets = db.list_sets_for_entry(entry.id)?;
                let exercise_type_id = sets.first().map(|s| s.exercise_type_id);
                let exercise_name = exercise_type_id.map_or_else(|| "unknown".to_string(), |id| self.exercise_name(id));
                let summary_parts: Vec<String> = sets.iter().map(format_set_short).collect();
                session_entries.push(EntryView {
                    id: entry.id,
                    exercise_name: exercise_name.clone(),
                    set_count: sets.len(),
                    sets_summary: summary_parts.join(", "),
                    is_open: entry.end_timestamp.is_none(),
                });
                if entry.end_timestamp.is_some() {
                    if let Some(id) = exercise_type_id {
                        closed_exercise_ids_in_session.push(id);
                    }
                }
                for set in sets {
                    session_sets.push((set, exercise_name.clone()));
                }
            }
        }

        // Active plan, recovered from sentinel-prefixed session.notes
        let active_plan = match active_session.as_ref().and_then(|s| s.notes.as_deref()) {
            Some(notes) => {
                let (plan_name, _rest) = parse_plan_from_notes(Some(notes));
                match plan_name {
                    Some(name) => self.build_active_plan(&db, user.id, &name, &closed_exercise_ids_in_session)?,
                    None => None,
                }
            }
            None => None,
        };

        // A `/nextworkout` design: bound to the current session (guided execution) or
        // freshly designed and ready to start. Sourced from `workout_plans`, distinct
        // from the schedule-based `active_plan` above.
        let active_workout_plan = self.build_active_workout_plan(&db, user.id, active_session.as_ref(), &session_sets)?;

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
        let schedules = db.list_schedules(user.id)?;

        let ctx = PromptContext {
            user_name: user.name.clone(),
            timezone: user.timezone.clone(),
            current_time: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            active_session,
            session_sets,
            session_entries,
            leaked_open_entries,
            active_plan,
            active_workout_plan,
            health_entries,
            recent_summaries,
            recent_sets,
            exercise_types: self.catalogue.clone(),
            active_goals,
            schedules,
            last_activity_age_hours,
        };

        Ok(build_system_prompt(&ctx))
    }

    fn build_active_plan(
        &self,
        db: &Database,
        user_id: i64,
        plan_name: &str,
        completed_exercise_ids: &[i64],
    ) -> anyhow::Result<Option<ActivePlanView>> {
        let schedules = db.list_schedules(user_id)?;
        let Some(schedule) = schedules.into_iter().find(|s| s.name.eq_ignore_ascii_case(plan_name)) else {
            return Ok(None);
        };
        let mut planned = db.list_schedule_exercises(schedule.id)?;
        planned.sort_by_key(|p| p.order_idx);
        let next = planned.iter().find(|p| !completed_exercise_ids.contains(&p.exercise_type_id)).map(|p| {
            let exercise_name = self.exercise_name(p.exercise_type_id);
            PlanExerciseView { exercise_name, target_sets: p.target_sets, target_reps: p.target_reps, target_weight_kg: p.target_weight_kg }
        });
        Ok(Some(ActivePlanView { name: schedule.name, completed_exercise_ids: completed_exercise_ids.to_vec(), next }))
    }

    /// Build the guided-execution view of a `/nextworkout` design. Prefers a plan
    /// bound to the active session (guided, in progress); otherwise surfaces the most
    /// recent designed-but-unstarted plan so the assistant can offer to run it.
    fn build_active_workout_plan(
        &self,
        db: &Database,
        user_id: i64,
        active_session: Option<&Session>,
        session_sets: &[(ExerciseSet, String)],
    ) -> anyhow::Result<Option<WorkoutPlanProgress>> {
        // Guided execution: a plan bound to the CURRENT session.
        if let Some(session) = active_session
            && let Some(plan) = db.active_plan_for_user(user_id)?
            && plan.session_id == Some(session.id)
        {
            let logged: std::collections::HashSet<i64> = session_sets.iter().map(|(s, _)| s.exercise_type_id).collect();
            return Ok(Some(self.workout_plan_progress(db, &plan, &logged, true)?));
        }

        // Otherwise a freshly designed plan ready to start — but only while it is
        // recent, so a stale design does not resurface as "ready" days later.
        if let Some(plan) = db.latest_proposed_plan(user_id)?
            && proposed_plan_within_window(&plan.created_at, Utc::now().naive_utc())
        {
            return Ok(Some(self.workout_plan_progress(db, &plan, &std::collections::HashSet::new(), false)?));
        }

        Ok(None)
    }

    /// Compute done/next/remaining for `plan`, treating any prescribed exercise whose
    /// type appears in `logged` as done.
    fn workout_plan_progress(
        &self,
        db: &Database,
        plan: &crate::db::WorkoutPlan,
        logged: &std::collections::HashSet<i64>,
        started: bool,
    ) -> anyhow::Result<WorkoutPlanProgress> {
        let exercises = db.list_plan_exercises(plan.id)?;
        let mut done = Vec::new();
        let mut next = None;
        for ex in &exercises {
            let name = self.exercise_name(ex.exercise_type_id);
            if logged.contains(&ex.exercise_type_id) {
                done.push(name);
            } else if next.is_none() {
                next = Some(PrescribedExercise {
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
        Ok(WorkoutPlanProgress { title: plan.title.clone(), started, done, next, remaining, override_note: plan.override_note.clone() })
    }
}

/// Build EntryView rows for any open exercise_entries the user has, so the prompt
/// (and LLM-driven cleanup logic) can see them. When `active_session_id` is given,
/// only entries inside that session are reported (the caller's contract: leaks are
/// what blocks a *new* session, not what's normal in the current one).
fn build_leaked_view(
    db: &Database,
    catalogue: &[ExerciseTypeWithAncestry],
    user_id: i64,
    active_session_id: Option<i64>,
) -> anyhow::Result<Vec<EntryView>> {
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
        views.push(EntryView {
            id: entry.id,
            exercise_name,
            set_count: sets.len(),
            sets_summary: sets.iter().map(format_set_short).collect::<Vec<_>>().join(", "),
            is_open: true,
        });
    }
    Ok(views)
}

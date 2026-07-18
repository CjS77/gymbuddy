//! Execution of [`AssistantAction`]s: set logging with its superset/entry
//! resolution, session lifecycle, entry close/edit/delete bookkeeping, and the
//! rest-timer directives that ride along with each outcome.

use chrono::Utc;

use crate::assistant::actions::AssistantAction;
use crate::assistant::matching::find_exercise_type;
use crate::db::{
    Database, Difficulty, ExerciseEntry, ExerciseTypeWithAncestry, GoalDirection, GoalKind, MeasurementType, Session, SetEdit,
    SetEditError, User, new_exercise_entry_at, new_exercise_set, new_goal, new_health_entry,
};

use super::designer::proposed_plan_within_window;
use super::{AssistantHandler, combine_plan_with_notes, format_set_short};
use gymbuddy_proto::TimerSignal;

/// Result of executing a single [`AssistantAction`]: an optional reply suffix
/// (set-count checkpoint, close pushback, …) and an optional rest-timer directive
/// (arm after a logged set, cancel after closing/ending).
#[derive(Default)]
pub(super) struct ActionOutcome {
    pub(super) suffix: Option<String>,
    pub(super) timer: Option<TimerSignal>,
}

impl ActionOutcome {
    fn none() -> Self {
        Self::default()
    }

    fn cancel() -> Self {
        Self { suffix: None, timer: Some(TimerSignal::Cancel) }
    }
}

impl From<Option<String>> for ActionOutcome {
    fn from(suffix: Option<String>) -> Self {
        Self { suffix, timer: None }
    }
}

/// Outcome of resolving which `exercise_entry` a logged set should join.
enum LogEntryTarget {
    /// Insert the set into this entry id (an exact-match open entry, or a freshly
    /// created one).
    Use(i64),
    /// An open entry exists for a taxonomy-related exercise. The set is not
    /// logged; the host asks the user whether they meant that ongoing entry or
    /// are supersetting a separate exercise.
    AskSuperset { ongoing_exercise: String },
}

/// Outcome of a close-entry action. Distinguishes a real close (the entry ended,
/// so the rest timer should be canceled) from the premature-close pushback (the
/// entry is still open and the user is still resting, so the timer keeps running).
enum CloseOutcome {
    /// The entry was ended.
    Closed,
    /// Fewer than 3 sets and unconfirmed: entry left open, carrying this pushback
    /// suffix.
    Pushback(String),
}

impl CloseOutcome {
    /// Map to the action outcome: a real close cancels the timer; a pushback only
    /// surfaces its suffix and leaves any in-flight rest running.
    fn into_action_outcome(self) -> ActionOutcome {
        match self {
            CloseOutcome::Closed => ActionOutcome::cancel(),
            CloseOutcome::Pushback(suffix) => ActionOutcome::from(Some(suffix)),
        }
    }
}

/// The resolved destination for a log-set action, produced by
/// [`AssistantHandler::resolve_log_target`]: either a ready entry, or a superset
/// disambiguation prompt (in which case nothing is logged this turn).
enum LogTarget {
    Ready(LogReady),
    AskSuperset(String),
}

/// Which entry, in which session, for which exercise type a logged set lands on.
struct LogReady {
    exercise_type_id: i64,
    exercise_name: String,
    session: Session,
    entry_id: i64,
}

impl AssistantHandler {
    /// Execute one action, returning an optional reply suffix (set-count checkpoint,
    /// premature-close pushback, leaked-entry warning) and an optional rest-timer
    /// directive (arm after a logged set, cancel after closing/ending).
    pub(super) async fn execute_action(&self, action: &AssistantAction, user: &User) -> anyhow::Result<ActionOutcome> {
        tracing::debug!(action = ?action, user_id = user.id, "Executing action");
        match action {
            AssistantAction::LogExercise { exercise, reps, weight_kg, perceived_difficulty, comment, superset } => {
                let target = match self.resolve_log_target(user, exercise, *superset).await? {
                    LogTarget::Ready(t) => t,
                    LogTarget::AskSuperset(prompt) => return Ok(Some(prompt).into()),
                };
                let weight = weight_kg.unwrap_or(0.0);
                let pd = perceived_difficulty.unwrap_or(Difficulty::Medium);
                {
                    let db = self.db.lock().await;
                    let existing = db.list_sets_for_entry(target.entry_id)?.len() as i32;
                    let mut s = new_exercise_set(target.entry_id, target.exercise_type_id, MeasurementType::WeightReps, weight);
                    s.count = *reps;
                    s.order_idx = existing;
                    s.perceived_difficulty = pd;
                    s.comment = comment.clone();
                    db.insert_set(&s)?;
                }
                self.finish_logged_set(user, &target.session, target.entry_id, &target.exercise_name, pd).await
            }
            AssistantAction::LogExerciseTimed { exercise, duration_secs, perceived_difficulty, comment, superset } => {
                let target = match self.resolve_log_target(user, exercise, *superset).await? {
                    LogTarget::Ready(t) => t,
                    LogTarget::AskSuperset(prompt) => return Ok(Some(prompt).into()),
                };
                let pd = perceived_difficulty.unwrap_or(Difficulty::Medium);
                let mut s = new_exercise_set(target.entry_id, target.exercise_type_id, MeasurementType::TimeBased, *duration_secs as f64);
                s.perceived_difficulty = pd;
                s.comment = comment.clone();
                self.db.lock().await.insert_set(&s)?;
                self.finish_logged_set(user, &target.session, target.entry_id, &target.exercise_name, pd).await
            }
            AssistantAction::LogExerciseDistance { exercise, distance_m, duration_secs, perceived_difficulty, comment, superset } => {
                let target = match self.resolve_log_target(user, exercise, *superset).await? {
                    LogTarget::Ready(t) => t,
                    LogTarget::AskSuperset(prompt) => return Ok(Some(prompt).into()),
                };
                let value = distance_m.unwrap_or_else(|| duration_secs.unwrap_or(0) as f64);
                let mt = if distance_m.is_some() { MeasurementType::DistanceBased } else { MeasurementType::TimeBased };
                let pd = perceived_difficulty.unwrap_or(Difficulty::Medium);
                let mut s = new_exercise_set(target.entry_id, target.exercise_type_id, mt, value);
                s.perceived_difficulty = pd;
                s.comment = comment.clone();
                self.db.lock().await.insert_set(&s)?;
                self.finish_logged_set(user, &target.session, target.entry_id, &target.exercise_name, pd).await
            }
            AssistantAction::StartSession { notes, plan } => {
                let db = self.db.lock().await;
                if let Some(active) = db.get_active_session(user.id)? {
                    let open = db.list_open_entries_for_session(active.id)?;
                    if !open.is_empty() {
                        let names = self.entry_exercise_names(&db, &open)?;
                        let suffix = format!(
                            "You still have {n} open exercise {entries} in your active session ({list}). \
                             Want me to close them or delete them before starting a new session?",
                            n = open.len(),
                            entries = if open.len() == 1 { "entry" } else { "entries" },
                            list = names.join(", "),
                        );
                        return Ok(Some(suffix).into());
                    }
                    tracing::debug!("Session already active, skipping start");
                    return Ok(ActionOutcome::none());
                }
                // No active session — clean up any leaked open entries from previously
                // ended sessions before starting fresh.
                drop(db);
                self.silent_close_leaked_entries(user.id).await?;
                let db = self.db.lock().await;
                let combined_notes = combine_plan_with_notes(plan.as_deref(), notes.as_deref());
                let session = db.start_session(user.id, combined_notes.as_deref())?;
                tracing::debug!(id = session.id, plan = ?plan, "Started session");
                // Begin guided execution if the user just designed a workout.
                self.bind_proposed_plan(&db, user.id, session.id)?;
                Ok(ActionOutcome::none())
            }
            AssistantAction::EndSession => {
                let db = self.db.lock().await;
                if let Some(session) = db.get_active_session(user.id)? {
                    tracing::debug!(id = session.id, "Ending session");
                    db.end_session(session.id)?;
                    // Complete a guided workout plan bound to this session.
                    if let Some(plan) = db.active_plan_for_user(user.id)?
                        && plan.session_id == Some(session.id)
                    {
                        db.set_plan_status(plan.id, crate::db::PlanStatus::Completed)?;
                    }
                } else {
                    tracing::debug!("No active session to end");
                }
                // Resting is over once the session ends.
                Ok(ActionOutcome::cancel())
            }
            AssistantAction::CloseExerciseEntry { exercise, entry_id } => {
                Ok(self.close_exercise_entry_action(user, exercise.as_deref(), *entry_id, false).await?.into_action_outcome())
            }
            AssistantAction::ConfirmCloseExerciseEntry { exercise, entry_id } => {
                Ok(self.close_exercise_entry_action(user, exercise.as_deref(), *entry_id, true).await?.into_action_outcome())
            }
            AssistantAction::DeleteExerciseEntry { entry_id } => {
                let db = self.db.lock().await;
                let entry = db.get_entry(*entry_id)?.ok_or_else(|| anyhow::anyhow!("entry {entry_id} not found"))?;
                anyhow::ensure!(entry.user_id == user.id, "entry {entry_id} does not belong to user");
                db.delete_entry(*entry_id)?;
                Ok(ActionOutcome::cancel())
            }
            AssistantAction::CloseAllOpenEntries => {
                let db = self.db.lock().await;
                if let Some(session) = db.get_active_session(user.id)? {
                    let n = db.close_open_entries_for_session(session.id, None)?;
                    tracing::debug!(session_id = session.id, closed = n, "Closed all open entries");
                }
                Ok(ActionOutcome::cancel())
            }
            AssistantAction::LogHealth { entry_type, body_part, severity, description } => {
                let mut entry = new_health_entry(user.id, *entry_type, description);
                entry.body_part = body_part.clone();
                if let Some(sev) = severity {
                    entry.severity = sev.clone();
                }
                tracing::debug!(entry_type = ?entry_type, body_part = ?body_part, severity = ?severity, "Inserting health entry");
                self.db.lock().await.insert_health_entry(&entry)?;
                Ok(ActionOutcome::none())
            }
            AssistantAction::ResolveHealth { description } => {
                let db = self.db.lock().await;
                let entries = db.list_active_health_entries(user.id)?;
                if let Some(entry) = entries.iter().find(|e| e.description.to_lowercase().contains(&description.to_lowercase())) {
                    tracing::debug!(id = entry.id, description = %entry.description, "Resolving health entry");
                    db.resolve_health_entry(entry.id)?;
                } else {
                    tracing::debug!(search = %description, "No matching health entry found to resolve");
                }
                Ok(ActionOutcome::none())
            }
            AssistantAction::SetGoal { exercise, metric, kind, target_value, direction, priority, target_date } => {
                // Resolve the exercise (if named) to its taxonomy id; otherwise the goal
                // is denominated in a free-text metric.
                let exercise_type_id = match exercise {
                    Some(name) => {
                        let et = find_exercise_type(&self.catalogue, name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {name}"))?;
                        Some(et.exercise_type.id)
                    }
                    None => None,
                };
                anyhow::ensure!(exercise_type_id.is_some() || metric.is_some(), "set_goal needs either an exercise or a metric");

                // Default kind from whether an exercise is named; direction defaults to increase.
                let kind = kind.unwrap_or(if exercise_type_id.is_some() { GoalKind::Strength } else { GoalKind::Habit });
                let direction = direction.unwrap_or(GoalDirection::Increase);
                let mut goal = new_goal(user.id, kind, exercise_type_id, metric.clone(), *target_value, direction);
                goal.priority = priority.unwrap_or(0);
                goal.target_date = target_date.clone();
                tracing::debug!(%kind, ?exercise_type_id, ?metric, target = %target_value, %direction, "Inserting goal");
                self.db.lock().await.insert_goal(&goal)?;
                Ok(ActionOutcome::none())
            }
            AssistantAction::EditSet { exercise, new_exercise, new_reps, new_value, new_difficulty } => Ok(self
                .edit_set_action(user, exercise.as_deref(), new_exercise.as_deref(), *new_reps, *new_value, *new_difficulty)
                .await?
                .into()),
            AssistantAction::GetLastExercise { exercise } => Ok(self.get_last_exercise_action(user, exercise).await?.into()),
            AssistantAction::SavePhilosophy { .. } => {
                // Only meaningful inside the `/philosophy` interview, which applies it
                // directly. Ignore it if the model emits it during normal chat.
                tracing::debug!("Ignoring save_philosophy outside the philosophy interview");
                Ok(ActionOutcome::none())
            }
            AssistantAction::ProposeWorkout { .. } => {
                // Only meaningful for `/nextworkout`, which persists the plan directly
                // and never routes it through here. Ignore it during normal chat.
                tracing::debug!("Ignoring propose_workout outside /nextworkout");
                Ok(ActionOutcome::none())
            }
            AssistantAction::AppendPhilosophyNote { note } => {
                self.db.lock().await.append_philosophy_note(user.id, note)?;
                Ok(Some("Noted for future workouts.".to_string()).into())
            }
            AssistantAction::SetSessionOverride { note } => {
                let db = self.db.lock().await;
                // A one-off attaches to the plan in flight so it expires with that plan
                // and never touches the philosophy. With no plan in flight there is
                // nothing to scope it to, so drop it rather than leak it anywhere durable.
                match db.inflight_plan_for_user(user.id)? {
                    Some(plan) => {
                        db.append_plan_override(plan.id, note)?;
                        Ok(Some("Got it -- just for this workout.".to_string()).into())
                    }
                    None => Ok(Some("There's no workout in progress to apply that to right now.".to_string()).into()),
                }
            }
            AssistantAction::Unknown => {
                tracing::debug!("Ignoring unknown action type from LLM");
                Ok(ActionOutcome::none())
            }
        }
    }

    /// Build the rest-timer arm directive for a just-logged set, or `None` when the
    /// user has timers disabled. A superset (≥2 open entries) gets the flat shorter
    /// rest; otherwise the perceived difficulty sets the duration.
    async fn arm_rest_timer(&self, user: &User, session: &Session, exercise: &str, difficulty: Difficulty) -> anyhow::Result<Option<TimerSignal>> {
        if !user.timers_enabled {
            return Ok(None);
        }
        let is_superset = self.db.lock().await.is_supersetting(session.id)?;
        let duration_secs = self.config.rest_timer.rest_secs_for(Some(difficulty), is_superset);
        Ok(Some(TimerSignal::Arm { duration_secs, exercise: exercise.to_string() }))
    }

    /// Shared trailer for the three log-set actions: the set-count checkpoint suffix
    /// plus the rest-timer arm directive, bundled into the action outcome.
    async fn finish_logged_set(
        &self,
        user: &User,
        session: &Session,
        entry_id: i64,
        exercise: &str,
        difficulty: Difficulty,
    ) -> anyhow::Result<ActionOutcome> {
        let suffix = self.set_count_checkpoint_suffix(entry_id, exercise).await?;
        let timer = self.arm_rest_timer(user, session, exercise, difficulty).await?;
        Ok(ActionOutcome { suffix, timer })
    }

    async fn ensure_session(&self, user: &User) -> anyhow::Result<crate::db::Session> {
        if let Some(session) = self.db.lock().await.get_active_session(user.id)? {
            return Ok(session);
        }
        // Auto-start path: no active session exists, so any open exercise_entries
        // for this user are leaks from a previously-ended session. Close them
        // silently before starting fresh.
        self.silent_close_leaked_entries(user.id).await?;
        let db = self.db.lock().await;
        let session = db.start_session(user.id, None)?;
        // Logging the first prescribed exercise auto-starts the guided workout.
        self.bind_proposed_plan(&db, user.id, session.id)?;
        Ok(session)
    }

    /// After a session starts, bind the most recent designed (proposed) workout to it
    /// so guided execution begins. No-op when there is no proposed plan.
    fn bind_proposed_plan(&self, db: &Database, user_id: i64, session_id: i64) -> anyhow::Result<()> {
        let Some(plan) = db.latest_proposed_plan(user_id)? else {
            return Ok(());
        };
        if proposed_plan_within_window(&plan.created_at, Utc::now().naive_utc()) {
            db.bind_plan_to_session(plan.id, session_id)?;
            tracing::debug!(plan_id = plan.id, session_id, "Bound designed workout to session for guided execution");
        } else {
            // Designed too long ago to silently attach to this session — retire it so
            // it stops being a binding candidate.
            db.set_plan_status(plan.id, crate::db::PlanStatus::Abandoned)?;
            tracing::debug!(plan_id = plan.id, "Abandoned a stale proposed workout instead of binding");
        }
        Ok(())
    }

    /// Shared preamble for the three log-set actions: resolve the exercise type,
    /// ensure an active session, and choose the entry to log into. Yields
    /// [`LogTarget::AskSuperset`] (logging nothing) when the set is an ambiguous
    /// taxonomy relative of an already-open entry.
    async fn resolve_log_target(&self, user: &User, exercise: &str, superset: bool) -> anyhow::Result<LogTarget> {
        let et = find_exercise_type(&self.catalogue, exercise).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {exercise}"))?;
        let session = self.ensure_session(user).await?;
        match self.resolve_entry_for_log(user.id, session.id, et.exercise_type.id, superset).await? {
            LogEntryTarget::AskSuperset { ongoing_exercise } => {
                Ok(LogTarget::AskSuperset(superset_prompt(&ongoing_exercise, &et.exercise_type.name)))
            }
            LogEntryTarget::Use(entry_id) => Ok(LogTarget::Ready(LogReady {
                exercise_type_id: et.exercise_type.id,
                exercise_name: et.exercise_type.name.clone(),
                session,
                entry_id,
            })),
        }
    }

    /// Decide which `exercise_entry` a logged set belongs to.
    ///
    /// 1. An open entry of the **exact** same exercise type is reused.
    /// 2. Otherwise, unless `superset` is set, an open entry for a taxonomy
    ///    ancestor or descendant of the exercise triggers an `AskSuperset`
    ///    prompt — the set is ambiguous and is not logged this turn.
    /// 3. Failing both, a fresh entry is created. Its `start_timestamp` is
    ///    computed once so the first set can share the same `logged_at` value
    ///    (the brief's "same start timestamp as the first set" requirement).
    async fn resolve_entry_for_log(
        &self,
        user_id: i64,
        session_id: i64,
        exercise_type_id: i64,
        superset: bool,
    ) -> anyhow::Result<LogEntryTarget> {
        let db = self.db.lock().await;
        if let Some(open) = db.find_open_entry_for_exercise(user_id, session_id, exercise_type_id)? {
            return Ok(LogEntryTarget::Use(open.id));
        }
        if !superset {
            if let Some((_, related_type_id)) = db.find_open_related_entry(session_id, exercise_type_id)? {
                let ongoing_exercise = self
                    .catalogue
                    .iter()
                    .find(|e| e.exercise_type.id == related_type_id)
                    .map(|e| e.exercise_type.name.clone())
                    .unwrap_or_else(|| "that exercise".to_string());
                return Ok(LogEntryTarget::AskSuperset { ongoing_exercise });
            }
        }
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let entry = new_exercise_entry_at(user_id, Some(session_id), None, &now);
        Ok(LogEntryTarget::Use(db.insert_entry(&entry)?))
    }

    /// Resolve an entry to close (explicit id > exercise-name match > most recent
    /// open in the active session). When `confirm` is false and the resolved entry
    /// has fewer than 3 sets, returns [`CloseOutcome::Pushback`] and leaves the
    /// entry open; otherwise ends it and returns [`CloseOutcome::Closed`].
    async fn close_exercise_entry_action(
        &self,
        user: &User,
        exercise: Option<&str>,
        entry_id: Option<i64>,
        confirm: bool,
    ) -> anyhow::Result<CloseOutcome> {
        let db = self.db.lock().await;
        let active = db.get_active_session(user.id)?;
        let resolved = if let Some(id) = entry_id {
            let entry = db.get_entry(id)?.ok_or_else(|| anyhow::anyhow!("entry {id} not found"))?;
            anyhow::ensure!(entry.user_id == user.id, "entry {id} does not belong to user");
            anyhow::ensure!(entry.end_timestamp.is_none(), "entry {id} is already closed");
            entry
        } else {
            let session = active.as_ref().ok_or_else(|| anyhow::anyhow!("no active session"))?;
            let open = db.list_open_entries_for_session(session.id)?;
            let entry = if let Some(name) = exercise {
                let et = find_exercise_type(&self.catalogue, name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {name}"))?;
                open.into_iter().find(|e| {
                    db.list_sets_for_entry(e.id).map(|sets| sets.iter().any(|s| s.exercise_type_id == et.exercise_type.id)).unwrap_or(false)
                })
            } else {
                open.into_iter().last()
            };
            entry.ok_or_else(|| anyhow::anyhow!("no matching open exercise_entry to close"))?
        };

        let count = db.count_sets_for_entry(resolved.id)?;
        if !confirm && count < 3 {
            let name = entry_exercise_name(&db, &self.catalogue, resolved.id)?;
            let suffix = format!(
                "You've only done {count} {sets} of {name}. You should really push for one more! Should we keep going?",
                sets = if count == 1 { "set" } else { "sets" },
            );
            return Ok(CloseOutcome::Pushback(suffix));
        }
        db.end_entry(resolved.id)?;
        Ok(CloseOutcome::Closed)
    }

    /// Edit a recently-logged set. Resolves the target by recency, optionally
    /// filtered by the named current exercise. A `new_exercise` reclassifies the
    /// whole exercise block; value/reps/difficulty changes apply to the single
    /// most-recent set. Returns a host-built before→after confirmation suffix.
    async fn edit_set_action(
        &self,
        user: &User,
        exercise: Option<&str>,
        new_exercise: Option<&str>,
        new_reps: Option<i32>,
        new_value: Option<f64>,
        new_difficulty: Option<Difficulty>,
    ) -> anyhow::Result<Option<String>> {
        let db = self.db.lock().await;

        let filter_id = match exercise {
            Some(name) => {
                Some(find_exercise_type(&self.catalogue, name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {name}"))?.exercise_type.id)
            }
            None => None,
        };
        let target = db
            .most_recent_set_for_user(user.id, filter_id)?
            .ok_or_else(|| anyhow::anyhow!("I couldn't find a recent set to edit."))?;

        let mut parts: Vec<String> = Vec::new();

        // Exercise change → reclassify the whole exercise block (entry).
        if let Some(new_name) = new_exercise {
            let new_et =
                find_exercise_type(&self.catalogue, new_name).ok_or_else(|| anyhow::anyhow!("Unknown exercise: {new_name}"))?;
            match db.reclassify_entry_exercise(target.exercise_entry_id, user.id, &self.catalogue, new_et.exercise_type.id) {
                Ok(outcome) => {
                    let old_name = self
                        .catalogue
                        .iter()
                        .find(|e| e.exercise_type.id == outcome.old_exercise_type_id)
                        .map(|e| e.exercise_type.name.as_str())
                        .unwrap_or("the previous exercise");
                    parts.push(format!(
                        "exercise {old_name} → {} ({} set{})",
                        new_et.exercise_type.name,
                        outcome.sets_updated,
                        if outcome.sets_updated == 1 { "" } else { "s" },
                    ));
                }
                Err(SetEditError::MeasurementTypeMismatch { from, to }) => {
                    return Err(anyhow::anyhow!(
                        "{from} and {to} aren't measured the same way, so I can't just swap them — re-log the set as {to} instead."
                    ));
                }
                Err(e) => return Err(anyhow::anyhow!("{e}")),
            }
        }

        // Value / reps / difficulty change → edit the single most-recent set.
        if new_reps.is_some() || new_value.is_some() || new_difficulty.is_some() {
            let edit = SetEdit {
                exercise_type_id: None,
                count: new_reps.map(Some),
                value: new_value,
                perceived_difficulty: new_difficulty,
                comment: None,
            };
            let outcome = db.edit_set(target.id, user.id, &self.catalogue, &edit).map_err(|e| anyhow::anyhow!("{e}"))?;
            if new_value.is_some() {
                parts.push(format!(
                    "{} {} → {}",
                    outcome.before.measurement_type.value_label(),
                    outcome.before.measurement_type.format_value(outcome.before.value),
                    outcome.after.measurement_type.format_value(outcome.after.value),
                ));
            }
            if new_reps.is_some() {
                parts.push(format!("reps {} → {}", opt_count(outcome.before.count), opt_count(outcome.after.count)));
            }
            if let Some(d) = new_difficulty {
                parts.push(format!("difficulty → {d}"));
            }
        }

        if parts.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!("Updated your last set — {}.", parts.join(", "))))
    }

    /// Look up and render the user's most recent `exercise_entry` for an exercise
    /// named in free text. Tries an exact-type match first; if nothing is logged,
    /// falls back to a descendants-inclusive query so coarse names like "chest"
    /// still resolve to a logged variation. Always user-scoped via SQL.
    async fn get_last_exercise_action(&self, user: &User, exercise: &str) -> anyhow::Result<Option<String>> {
        let Some(et) = find_exercise_type(&self.catalogue, exercise) else {
            return Ok(Some(format!("I don't have \"{exercise}\" in my exercise list.")));
        };
        let asked_name = et.exercise_type.name.clone();
        let db = self.db.lock().await;
        let (entry, fell_back) = match db.most_recent_entry_for_exercise(user.id, et.exercise_type.id, false)? {
            Some(e) => (e, false),
            None => match db.most_recent_entry_for_exercise(user.id, et.exercise_type.id, true)? {
                Some(e) => (e, true),
                None => return Ok(Some(format!("You haven't logged any {asked_name} yet."))),
            },
        };
        let sets = db.list_sets_for_entry(entry.id)?;
        let resolved_name = entry_exercise_name(&db, &self.catalogue, entry.id)?;
        let summary = sets.iter().map(format_set_short).collect::<Vec<_>>().join(", ");
        let suffix = if fell_back && resolved_name != asked_name {
            format!(
                "No direct {asked_name} entries — showing nearest match {resolved_name} from {start} ({n} {sets_word}: {summary}).",
                start = entry.start_timestamp,
                n = sets.len(),
                sets_word = if sets.len() == 1 { "set" } else { "sets" },
            )
        } else {
            format!(
                "Your last {resolved_name} was {start} ({n} {sets_word}: {summary}).",
                start = entry.start_timestamp,
                n = sets.len(),
                sets_word = if sets.len() == 1 { "set" } else { "sets" },
            )
        };
        Ok(Some(suffix))
    }

    /// Set-count checkpoint: every time the user logs a set in an open entry that
    /// already has ≥3 sets total, ask whether they want to keep going or move on.
    async fn set_count_checkpoint_suffix(&self, entry_id: i64, exercise_name: &str) -> anyhow::Result<Option<String>> {
        let count = self.db.lock().await.count_sets_for_entry(entry_id)?;
        if count >= 3 {
            Ok(Some(format!("You've logged {count} sets of {exercise_name}. Want another set, or move to the next exercise?")))
        } else {
            Ok(None)
        }
    }

    /// Close any open exercise_entries that belong to already-ended sessions for
    /// this user. Best-effort: uses the parent session's `ended_at` when present,
    /// otherwise `datetime('now')`.
    async fn silent_close_leaked_entries(&self, user_id: i64) -> anyhow::Result<()> {
        let db = self.db.lock().await;
        let leaks = db.list_open_entries_for_user(user_id)?;
        for entry in leaks {
            let Some(session_id) = entry.session_id else {
                db.end_entry(entry.id)?;
                continue;
            };
            let session = db.get_session(session_id)?;
            match session.and_then(|s| s.ended_at) {
                Some(ended_at) => {
                    db.conn().execute(
                        "UPDATE exercise_entry SET end_timestamp = ?1 WHERE id = ?2 AND end_timestamp IS NULL",
                        rusqlite::params![ended_at, entry.id],
                    )?;
                }
                None => {
                    // session is still active — not a leak; leave it alone.
                }
            }
        }
        Ok(())
    }

    fn entry_exercise_names(&self, db: &Database, entries: &[ExerciseEntry]) -> anyhow::Result<Vec<String>> {
        entries.iter().map(|e| entry_exercise_name(db, &self.catalogue, e.id)).collect()
    }
}

/// Resolve the exercise an entry "belongs to" via its first set. Returns
/// `"unknown"` if the entry has no sets yet (which only happens transiently
/// before the first insert).
pub(crate) fn entry_exercise_name(db: &Database, catalogue: &[ExerciseTypeWithAncestry], entry_id: i64) -> anyhow::Result<String> {
    let sets = db.list_sets_for_entry(entry_id)?;
    let Some(first) = sets.first() else {
        return Ok("unknown".to_string());
    };
    let name = catalogue
        .iter()
        .find(|e| e.exercise_type.id == first.exercise_type_id)
        .map(|e| e.exercise_type.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    Ok(name)
}

/// Render an optional rep count, falling back to an em dash when absent.
fn opt_count(c: Option<i32>) -> String {
    c.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string())
}

/// Question appended to the assistant's reply when a logged set is a taxonomy
/// relative of an exercise already in progress. The user resolves it on the next
/// turn ("same exercise" → join the ongoing entry; "superset" → log separately).
fn superset_prompt(ongoing_exercise: &str, logged_exercise: &str) -> String {
    format!(
        "You've already got an open {ongoing_exercise} entry going. Should I add this {logged_exercise} set \
         to it, or are you supersetting a separate exercise? Reply \"same exercise\" to add it to \
         {ongoing_exercise}, or \"superset\" to log it on its own."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::*;
    use crate::render::Telegram;
    use crate::telegram::Message as TgMessage;
    use gymbuddy_proto::Render as _;

    #[tokio::test]
    async fn exercise_logging_creates_records() {
        let response = r#"{"message": "Logged your bench press!", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
        ]}"#;
        let (handler, _) = setup_handler(response).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let reply = handler.handle_text_message(&msg, "3 sets bench 80kg 8 reps").await.unwrap();
        assert!(shown(&reply).starts_with("Logged your bench press!"));
        assert!(shown(&reply).contains("You've logged 3 sets of Bench Press. Want another set, or move to the next exercise?"));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1);
        let sets = db.list_sets_for_entry(entries[0].id).unwrap();
        assert_eq!(sets.len(), 3);
        assert!(sets.iter().all(|s| s.count == Some(8) && (s.value - 80.0).abs() < 1e-6));
        assert!(entries[0].end_timestamp.is_none(), "entry should still be open");
    }

    /// Register a user and log a Flat Barbell Bench Press set, opening one entry.
    async fn open_variation_entry(handler: &AssistantHandler, llm: &MockLlm, msg: &TgMessage) {
        let _ = handler.handle_text_message(msg, "hello").await.unwrap();
        llm.set_response(
            r#"{"message": "Logged it!", "actions": [
                {"type": "log_exercise", "exercise": "Flat Barbell Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(msg, "flat barbell bench press 80kg 8 reps medium").await.unwrap();
    }

    #[tokio::test]
    async fn related_exercise_log_prompts_for_superset() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        open_variation_entry(&handler, &llm, &msg).await;

        // Logging the parent exercise (a taxonomy ancestor of the open entry) is ambiguous.
        llm.set_response(
            r#"{"message": "Sure.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "bench press 80kg 8 reps medium").await.unwrap();
        assert!(shown(&reply).contains("supersetting"), "expected a superset question, got: {}", shown(&reply));

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1, "ambiguous set must not open a new entry");
        assert_eq!(db.count_sets_for_entry(entries[0].id).unwrap(), 1, "ambiguous set must not be inserted");
    }

    #[tokio::test]
    async fn superset_flag_logs_parallel_entry() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        open_variation_entry(&handler, &llm, &msg).await;

        // `superset: true` asserts a deliberate parallel exercise — log without asking.
        llm.set_response(
            r#"{"message": "Logged the superset.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium", "superset": true}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "actually superset, log bench press").await.unwrap();
        assert!(!shown(&reply).contains("supersetting"), "superset flag should suppress the question");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let open = db.list_open_entries_for_session(session.id).unwrap();
        assert_eq!(open.len(), 2, "the superset must open a second parallel entry");
        let total: i64 = open.iter().map(|e| db.count_sets_for_entry(e.id).unwrap()).sum();
        assert_eq!(total, 2, "both sets must be logged");
    }

    #[tokio::test]
    async fn same_exercise_resolution_groups_in() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        open_variation_entry(&handler, &llm, &msg).await;

        // Ambiguous log triggers the prompt.
        llm.set_response(
            r#"{"message": "Sure.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "bench press 80kg 8 reps medium").await.unwrap();

        // "same exercise" → re-emit against the exact ongoing exercise name.
        llm.set_response(
            r#"{"message": "Added it.", "actions": [
                {"type": "log_exercise", "exercise": "Flat Barbell Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "same exercise").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1, "the set must join the existing entry, not open a new one");
        assert_eq!(db.count_sets_for_entry(entries[0].id).unwrap(), 2);
    }

    #[tokio::test]
    async fn unrelated_superset_logs_without_prompt() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        llm.set_response(
            r#"{"message": "Logged it!", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "bench press 80kg 8 reps medium").await.unwrap();

        // Squat is a different taxonomy branch — a genuine superset, never ambiguous.
        llm.set_response(
            r#"{"message": "Logged it!", "actions": [
                {"type": "log_exercise", "exercise": "Squat", "reps": 5, "weight_kg": 100.0, "perceived_difficulty": "hard"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "squat 100kg 5 reps hard").await.unwrap();
        assert!(!shown(&reply).contains("supersetting"), "unrelated exercises must not trigger the prompt");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        assert_eq!(db.list_entries_for_session(session.id).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn session_auto_start() {
        let response = r#"{"message": "Logged!", "actions": [{"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 1}]}"#;
        let (handler, _) = setup_handler(response).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        assert!(db.get_active_session(user.id).unwrap().is_none());
        drop(db);

        let _ = handler.handle_text_message(&msg, "bench press").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        assert!(db.get_active_session(user.id).unwrap().is_some());
    }

    // ─── New behaviour tests for the set-centric workflow ──────────────────

    #[tokio::test]
    async fn supersets_keep_separate_entries() {
        let response_a = r#"{"message": "Logged bench.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_a).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 80kg 8 reps").await.unwrap();

        // Without closing, log a different exercise.
        llm.set_response(
            r#"{"message": "Logged pull-ups.", "actions": [
                {"type": "log_exercise", "exercise": "Pull-Up", "sets": 1, "reps": 10}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "now pull-ups, 10 reps").await.unwrap();

        // Then back to bench — should reuse the existing open Bench Press entry.
        llm.set_response(
            r#"{"message": "Logged another bench set.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "another bench set").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 2, "two distinct entries for two exercises (superset)");
        let mut counts: Vec<usize> = entries.iter().map(|e| db.list_sets_for_entry(e.id).unwrap().len()).collect();
        counts.sort();
        assert_eq!(counts, vec![1, 2], "Pull-Up=1, Bench Press=2");
        for e in &entries {
            assert!(e.end_timestamp.is_none(), "both entries remain open");
        }
    }

    #[tokio::test]
    async fn checkpoint_suffix_appears_at_3_sets_and_repeats_at_4() {
        let log_three = r#"{"message": "Done!", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log_three).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "3 sets bench 80kg 8").await.unwrap();
        assert!(shown(&reply).contains("You've logged 3 sets of Bench Press. Want another set, or move to the next exercise?"));

        // 4th set — checkpoint should fire again with n=4.
        llm.set_response(
            r#"{"message": "Logged.", "actions": [
                {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "one more").await.unwrap();
        assert!(shown(&reply).contains("You've logged 4 sets of Bench Press"));
    }

    #[tokio::test]
    async fn premature_close_pushback_at_2_sets() {
        let response_log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "2 sets bench").await.unwrap();

        llm.set_response(
            r#"{"message": "Closing bench.", "actions": [
                {"type": "close_exercise_entry", "exercise": "Bench Press"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "close bench").await.unwrap();
        assert!(shown(&reply).contains("You've only done 2 sets of Bench Press. You should really push for one more! Should we keep going?"));
        // The entry stays open, so the in-flight rest timer must keep running.
        assert!(reply.timer.is_none(), "pushback must not cancel the rest timer");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].end_timestamp.is_none(), "entry must stay open after pushback");
    }

    #[tokio::test]
    async fn confirm_close_after_pushback_actually_closes() {
        let response_log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "2 sets bench").await.unwrap();

        llm.set_response(
            r#"{"message": "Closing for real.", "actions": [
                {"type": "confirm_close_exercise_entry", "exercise": "Bench Press"}
            ]}"#,
        );
        let _ = handler.handle_text_message(&msg, "yes really close it").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert!(entries[0].end_timestamp.is_some(), "confirm_close_exercise_entry bypasses pushback");
    }

    #[tokio::test]
    async fn close_exercise_entry_with_three_sets_succeeds() {
        let response_log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0},
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(response_log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "3 sets bench").await.unwrap();

        llm.set_response(
            r#"{"message": "Closing bench.", "actions": [
                {"type": "close_exercise_entry", "exercise": "Bench Press"}
            ]}"#,
        );
        let reply = handler.handle_text_message(&msg, "move on").await.unwrap();
        // A real close ends the entry, so the rest timer is canceled.
        assert_eq!(reply.timer, Some(TimerSignal::Cancel), "a real close cancels the rest timer");

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session = db.get_active_session(user.id).unwrap().unwrap();
        let entries = db.list_entries_for_session(session.id).unwrap();
        assert!(entries[0].end_timestamp.is_some(), "≥3-set close should succeed without pushback");
    }

    #[tokio::test]
    async fn end_session_closes_all_open_entries() {
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 80kg 8 reps").await.unwrap();

        llm.set_response(r#"{"message": "Ending.", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "end session").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        // No active session anymore.
        assert!(db.get_active_session(user.id).unwrap().is_none());
        // No open entries either.
        let leftover = db.list_open_entries_for_user(user.id).unwrap();
        assert!(leftover.is_empty(), "end_session must cascade-close every open entry");
    }

    #[tokio::test]
    async fn start_session_with_open_entries_in_active_session_blocks() {
        // Step 1: log a set so an open entry exists in the active session.
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench").await.unwrap();

        // Step 2: try to start a new session — should be blocked.
        llm.set_response(r#"{"message": "Starting.", "actions": [{"type": "start_session"}]}"#);
        let reply = handler.handle_text_message(&msg, "start a new session").await.unwrap();
        assert!(shown(&reply).contains("open exercise"));
        assert!(shown(&reply).contains("close them or delete them"));

        // The original session is still the active one — no new session was created.
        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let session_count: i64 =
            db.conn().query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1", rusqlite::params![user.id], |r| r.get(0)).unwrap();
        assert_eq!(session_count, 1);
    }

    #[tokio::test]
    async fn close_all_open_entries_clears_block_for_new_session() {
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "sets": 1, "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench").await.unwrap();

        llm.set_response(r#"{"message": "Closing.", "actions": [{"type": "close_all_open_entries"}]}"#);
        let _ = handler.handle_text_message(&msg, "close them").await.unwrap();

        let db = handler.db.lock().await;
        let user = db.get_user_by_telegram_id("12345").unwrap().unwrap();
        let leftover = db.list_open_entries_for_user(user.id).unwrap();
        assert!(leftover.is_empty());
    }

    #[tokio::test]
    async fn start_session_with_plan_stores_sentinel_in_notes() {
        // Seed a schedule named "Push Day" for the user.
        let (handler, llm) = setup_handler(r#"{"message": "hi", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user_id = {
            let db = handler.db.lock().await;
            let u = db.get_user_by_telegram_id("12345").unwrap().unwrap();
            let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let pull = db.get_exercise_type_by_name("Pull-Up").unwrap().unwrap();
            let sched_id = db
                .insert_schedule(&crate::db::Schedule {
                    id: 0,
                    user_id: u.id,
                    name: "Push Day".to_string(),
                    cron_expr: "0 0 6 * * 1".to_string(),
                    reminder_type: crate::db::ReminderType::Text,
                    reminder_notice_mins: 30,
                    enabled: true,
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .unwrap();
            db.add_schedule_exercise(&crate::db::ScheduleExercise {
                schedule_id: sched_id,
                exercise_type_id: bp.id,
                order_idx: 0,
                target_sets: Some(3),
                target_reps: Some(8),
                target_weight_kg: Some(80.0),
            })
            .unwrap();
            db.add_schedule_exercise(&crate::db::ScheduleExercise {
                schedule_id: sched_id,
                exercise_type_id: pull.id,
                order_idx: 1,
                target_sets: Some(3),
                target_reps: Some(10),
                target_weight_kg: None,
            })
            .unwrap();
            u.id
        };

        llm.set_response(r#"{"message": "Starting.", "actions": [{"type": "start_session", "plan": "Push Day"}]}"#);
        let _ = handler.handle_text_message(&msg, "start push day").await.unwrap();

        let db = handler.db.lock().await;
        let session = db.get_active_session(user_id).unwrap().unwrap();
        let notes = session.notes.unwrap();
        assert!(notes.starts_with("plan:Push Day"));
    }

    // ─── get_last_exercise ────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_last_exercise_returns_entry_when_logged() {
        let log = r#"{"message": "Logged.", "actions": [
            {"type": "log_exercise", "exercise": "Bench Press", "reps": 8, "weight_kg": 80.0, "perceived_difficulty": "medium"}
        ]}"#;
        let (handler, llm) = setup_handler(log).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let _ = handler.handle_text_message(&msg, "bench 80kg 8 medium").await.unwrap();

        llm.set_response(
            r#"{"message": "Here's your last bench press.", "actions": [
                {"type": "get_last_exercise", "exercise": "Bench Press"}
            ]}"#,
        );
        let view = handler.handle_text_message(&msg, "what was my last bench press?").await.unwrap();
        let (reply, _) = Telegram.render(&view.view);
        assert!(reply.contains("Bench Press"), "reply must name the resolved exercise, got: {reply}");
        assert!(reply.contains("8×80kg"), "reply must include the set, got: {reply}");
    }

    #[tokio::test]
    async fn get_last_exercise_returns_not_found_message_when_absent() {
        let (handler, llm) = setup_handler(r#"{"message": "ok", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        llm.set_response(
            r#"{"message": "Looking it up.", "actions": [
                {"type": "get_last_exercise", "exercise": "Bench Press"}
            ]}"#,
        );
        let view = handler.handle_text_message(&msg, "what was my last bench press?").await.unwrap();
        let (reply, _) = Telegram.render(&view.view);
        assert!(reply.to_lowercase().contains("haven't logged"), "expected not-found phrasing, got: {reply}");
    }
}

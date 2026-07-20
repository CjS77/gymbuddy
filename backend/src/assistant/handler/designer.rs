//! The `/nextworkout` designer: builds a tailored workout from philosophy,
//! recent history, recovery, goals, injuries and the curated training science
//! retrieved for them, persists it as a `proposed` plan, and bounds how long that
//! proposal stays eligible to bind to a session.

use std::collections::{BTreeMap, HashSet};

use anyhow::Context as _;
use chrono::NaiveDateTime;

use crate::assistant::actions::{AssistantAction, ProposedExercise};
use crate::assistant::matching::find_exercise_type;
use crate::assistant::parser::parse_assistant_response;
use crate::assistant::prompts::{build_designer_prompt, estimate_tokens, format_muscle_recovery, format_session_outcome, goals_by_priority};
use crate::config::DesignerHistoryConfig;
use crate::db::{
    Database, EntryWithSets, ExerciseSet, GoalKind, GoalProgress, HealthEntry, MeasurementType, RosterExercise, Session, SessionWithSets,
    TrainingMode, User, WorkoutPhilosophy,
};
use crate::science::{ScienceQuery, normalise_body_part, prescription_doc};

use super::AssistantHandler;
use super::continuity::parse_sqlite_datetime;
use gymbuddy_proto::{PlannedExerciseView, TrainingModeView, View, WorkoutView};

impl AssistantHandler {
    /// Design a tailored workout from the user's philosophy, recent history, goals,
    /// and injuries, and present it as a [`View::Workout`]. Persists a `proposed`
    /// plan but logs NOTHING and starts no session. Any text after the command
    /// ("/nextworkout but something lighter") is passed to the designer as guidance.
    ///
    /// The design runs in an explicit [`TrainingMode`] ([C1.4]): with an active
    /// programme it targets the current slot; without one it is ad-hoc, exactly the
    /// pre-programme behaviour. Guidance leading with "adhoc" ("/nextworkout adhoc
    /// dumbbells only") forces a one-off that leaves an active programme untouched.
    pub(super) async fn cmd_next_workout(&self, user: &User, text: &str) -> anyhow::Result<View> {
        let raw_guidance = text.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
        let (force_ad_hoc, guidance) = split_ad_hoc_marker(&raw_guidance);

        let (mode, philosophy, sessions, recovery, goals, injuries, goal_ids) = {
            let db = self.db.lock().await;
            let goals = db.goal_progress_report(user.id, None, None)?;
            let goal_ids = goal_relevant_exercise_ids(&db, &goals)?;
            // Pull a generous window (bounded by config); the summariser decides how
            // much of it survives into the prompt under the token budget.
            (
                db.training_mode_for_design(user.id, force_ad_hoc)?,
                db.latest_philosophy(user.id)?,
                db.recent_sessions_with_sets(user.id, self.config.designer_history.max_sessions)?,
                db.muscle_recovery(user.id)?,
                goals,
                db.list_active_health_entries(user.id)?,
                goal_ids,
            )
        };

        // Per spec: with no philosophy on file, offer to build one rather than guess.
        let Some(philosophy) = philosophy else {
            return Ok(View::notice(
                "I don't have a training philosophy for you yet, so I can't tailor a workout. \
                 Run /philosophy and we'll build one together first.",
            ));
        };

        let history_block = self.format_designer_history(&sessions, &goal_ids);
        let recovery_block = format_muscle_recovery(&recovery, chrono::Utc::now().date_naive());
        let science = self.science.search(&science_query(&goals, &injuries, &guidance), SCIENCE_CHUNK_K);
        let prompt =
            build_designer_prompt(&philosophy.content, &history_block, &recovery_block, &goals, &injuries, &science, &self.catalogue);
        let user_text = if guidance.trim().is_empty() { "Design my next workout.".to_string() } else { guidance };

        // The design overruns the default token cap; logging is impossible here
        // because this path never calls execute_action / ensure_session.
        let llm_response = self
            .call_llm_with(&prompt, &[], &user_text, 2048, 0.2)
            .await
            .context("workout designer LLM call failed")?;
        let parsed = parse_assistant_response(&llm_response);

        let proposal = parsed.actions.into_iter().find_map(|action| match action {
            AssistantAction::ProposeWorkout { title, rationale, exercises } => Some((title, rationale, exercises)),
            _ => None,
        });

        let Some((title, rationale, exercises)) = proposal else {
            // The designer returned something that isn't a valid `propose_workout`
            // (unparseable JSON, or valid JSON without the required action). FAIL LOUD:
            // do NOT render the prose as if a workout were created, and persist nothing.
            // Rendering the fallback prose here used to silently mislead — the user saw a
            // "workout" that was never saved. See ticket C1.7.
            tracing::warn!(user_id = user.id, response = %llm_response, "workout designer returned no valid propose_workout");
            return Ok(View::notice(
                "I couldn't design a valid workout this time. Please try /nextworkout again, \
                 optionally adding a hint like /nextworkout upper body.",
            ));
        };

        self.persist_and_view_workout(user, &philosophy, &mode, title, rationale, exercises).await
    }

    /// Persist a designed workout as a `proposed` plan and build its [`View::Workout`]
    /// (or [`View::ProgramWorkout`] when a programme is in play). Exercise names are
    /// resolved against the catalogue; unresolved ones are dropped with a note rather
    /// than failing the whole design.
    ///
    /// In programme mode the plan is stamped with the slot it fills; in ad-hoc mode
    /// its `programme_slot_id` stays NULL and no slot status moves — a one-off under an
    /// active programme never touches adherence.
    async fn persist_and_view_workout(
        &self,
        user: &User,
        philosophy: &WorkoutPhilosophy,
        mode: &TrainingMode,
        title: String,
        rationale: Option<String>,
        exercises: Vec<ProposedExercise>,
    ) -> anyhow::Result<View> {
        let mut planned: Vec<PlannedExerciseView> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        let db = self.db.lock().await;
        let plan_id = db.create_roster(user.id, &title, rationale.as_deref(), Some(philosophy.id))?;
        if let TrainingMode::Programme { slot, .. } = mode {
            db.bind_roster_to_slot(plan_id, slot.id)?;
            tracing::debug!(plan_id, slot_id = slot.id, "Designed workout bound to programme slot");
        }

        for (idx, ex) in exercises.into_iter().enumerate() {
            let Some(et) = find_exercise_type(&self.catalogue, &ex.exercise) else {
                notes.push(format!("Skipped \"{}\" -- I couldn't match it to a known exercise.", ex.exercise));
                continue;
            };
            db.add_roster_exercise(&RosterExercise {
                id: 0,
                roster_id: plan_id,
                exercise_type_id: et.exercise_type.id,
                order_idx: idx as i32,
                target_sets: ex.target_sets,
                target_reps: ex.target_reps,
                target_weight_kg: ex.target_weight_kg,
                target_secs: ex.target_secs,
                notes: ex.notes.clone(),
            })?;
            planned.push(PlannedExerciseView {
                name: et.exercise_type.name.clone(),
                target_sets: ex.target_sets.map(|n| n.max(0) as u32),
                target_reps: ex.target_reps.map(|n| n.max(0) as u32),
                target_weight_kg: ex.target_weight_kg,
                target_secs: ex.target_secs.map(|n| n.max(0) as u32),
                cue: ex.notes,
            });
        }

        let workout = WorkoutView { title, rationale, exercises: planned, notes };
        Ok(match mode_view(mode) {
            Some(mode) => View::ProgramWorkout { workout, mode },
            None => View::Workout(workout),
        })
    }

    /// Build the designer's history block: the most recent sessions in full, older
    /// sessions collapsed to per-exercise trend lines, the whole thing bounded by the
    /// configured token budget with goal-relevant lifts favoured over incidental ones.
    /// See [`summarise_designer_history`].
    fn format_designer_history(&self, sessions: &[SessionWithSets], goal_ids: &HashSet<i64>) -> String {
        summarise_designer_history(sessions, goal_ids, &self.config.designer_history, &|id| self.exercise_name(id))
    }
}

/// The set of exercise-type ids that count as "goal-relevant": every goal's target
/// exercise plus all of its taxonomy descendants, so a goal on a parent node (e.g.
/// "Bench Press") pulls in its variations too. Used to give those lifts more history
/// depth in the designer prompt than incidental accessories. Metric-denominated goals
/// (bodyweight, habit) name no exercise and so contribute nothing here.
fn goal_relevant_exercise_ids(db: &Database, goals: &[GoalProgress]) -> anyhow::Result<HashSet<i64>> {
    goals.iter().filter_map(|g| g.goal.exercise_type_id).try_fold(HashSet::new(), |mut acc, et_id| {
        acc.extend(db.descendant_ids_inclusive(et_id)?);
        Ok(acc)
    })
}

/// Header for the older-sessions summary appended after the full recent block.
const EARLIER_TRENDS_HEADER: &str = "EARLIER TRENDS (older sessions, oldest to newest; * = goal-relevant):\n";

/// How many science chunks the designer prompt carries. Four buys a prescription for the leading
/// goal plus room for a conflict resolution, an injury rail and one supporting section, and still
/// leaves the prompt dominated by the user's own history — which is the point of the design.
const SCIENCE_CHUNK_K: usize = 4;

/// The document holding the curated rule for goals that pull against each other.
const COMPETING_GOALS_DOC: &str = "competing-goals";

/// Compose the [`ScienceQuery`] for this design ([C5.2]).
///
/// Goal kinds go in **priority order** ([C3.1]) — the ordering *is* the resolution mechanism, so a
/// user's highest-priority goal governs the session. Two things are pinned rather than left to
/// ranking, because a prescription that arrives only when it happens to out-rank a general document
/// is not one the designer can be held to:
///
/// - each goal kind's canonical prescription ([`crate::science::prescription_doc`]), highest
///   priority first, so the bands for the goals in play always reach the model;
/// - `competing-goals` when the kinds actually differ, so the rule that stops the model averaging
///   two goals into a session serving neither cannot be ranked away.
///
/// Nothing here decides *how* goals combine; that judgement lives in the corpus, which is where it
/// can be reviewed as science.
fn science_query(goals: &[GoalProgress], injuries: &[HealthEntry], guidance: &str) -> ScienceQuery {
    let goal_kinds = distinct_kinds_by_priority(goals);
    let prescriptions = goal_kinds.iter().map(|kind| prescription_doc(*kind).to_string());
    let resolution = (goal_kinds.len() > 1).then(|| COMPETING_GOALS_DOC.to_string());
    ScienceQuery {
        injuries: injuries.iter().filter_map(|e| e.body_part.as_deref()).filter_map(normalise_body_part).collect(),
        pinned_docs: prescriptions.chain(resolution).collect(),
        guidance: guidance.to_string(),
        focus: Vec::new(),
        goal_kinds,
    }
}

/// The goal kinds in play, highest priority first, each appearing once. Two strength goals are one
/// kind of prescription, not two, and would otherwise read as a conflict with itself.
fn distinct_kinds_by_priority(goals: &[GoalProgress]) -> Vec<GoalKind> {
    goals_by_priority(goals).iter().map(|gp| gp.goal.kind).fold(Vec::new(), |mut kinds, kind| {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
        kinds
    })
}

/// The representative set of an entry for a trend line: the heaviest/longest by
/// `value` (ties keep the earliest). This is the "top set" the designer reasons
/// about when tracking progression.
fn top_set(sets: &[ExerciseSet]) -> Option<&ExerciseSet> {
    sets.iter().reduce(|best, s| if s.value > best.value { s } else { best })
}

/// One session rendered in full: every exercise with all its sets, exactly as the
/// designer read history before summarisation was introduced.
fn format_full_session(session: &Session, entries: &[EntryWithSets], name_of: &impl Fn(i64) -> String) -> String {
    let outcome = format_session_outcome(session).map(|p| format!(" ({p})")).unwrap_or_default();
    let mut out = format!("- Session {}{outcome}:\n", session.started_at);
    for (_entry, sets) in entries {
        let Some(first) = sets.first() else { continue };
        let name = name_of(first.exercise_type_id);
        let set_descs = sets.iter().map(format_set_desc).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("    {name} ({set_descs})\n"));
    }
    out
}

/// Collapse the older sessions into one trend line per exercise, priority-ordered:
/// goal-relevant lifts first (each keeping up to `goal_trend_points` points), then
/// incidental exercises (up to `accessory_trend_points`). Within a group, lines are
/// sorted by name for stable output. Points read oldest → newest, e.g.
/// "    Bench Press*: 55kg x 8 easy (2026-05-01) -> 60kg x 6 medium (2026-05-15)".
fn build_trend_lines(
    older: &[SessionWithSets],
    goal_ids: &HashSet<i64>,
    cfg: &DesignerHistoryConfig,
    name_of: &impl Fn(i64) -> String,
) -> Vec<String> {
    // Gather each exercise's top set per session. `older` is most-recent-first, so we
    // push in that order and read the tail (most recent points) when rendering.
    let mut points: BTreeMap<i64, Vec<(String, String)>> = BTreeMap::new();
    for (session, entries) in older {
        let date = session.started_at.get(..10).unwrap_or(&session.started_at).to_string();
        for (_entry, sets) in entries {
            if let Some(set) = top_set(sets) {
                points.entry(set.exercise_type_id).or_default().push((date.clone(), format_set_desc(set)));
            }
        }
    }

    // `series` is most-recent-first; keep the freshest `keep` points and present them
    // oldest → newest so the designer reads a left-to-right progression.
    let render = |id: i64, series: &[(String, String)], keep: usize| -> String {
        let marker = if goal_ids.contains(&id) { "*" } else { "" };
        let steps = series.iter().take(keep.max(1)).rev().map(|(date, desc)| format!("{desc} ({date})")).collect::<Vec<_>>().join(" -> ");
        format!("    {}{marker}: {steps}\n", name_of(id))
    };

    let (mut goal, mut accessory): (Vec<_>, Vec<_>) = points.iter().partition(|(id, _)| goal_ids.contains(id));
    let key = |(id, _): &(&i64, &Vec<(String, String)>)| name_of(**id);
    goal.sort_by_key(key);
    accessory.sort_by_key(key);

    let goal_lines = goal.into_iter().map(|(id, series)| render(*id, series, cfg.goal_trend_points));
    let accessory_lines = accessory.into_iter().map(|(id, series)| render(*id, series, cfg.accessory_trend_points));
    goal_lines.chain(accessory_lines).collect()
}

/// Format the `/nextworkout` designer's training-history block. The most recent
/// `full_sessions` sessions are rendered in full (every exercise and set); older
/// sessions collapse to per-exercise trend lines. The block is bounded by
/// `token_budget`: recent full sessions are kept first, then trend lines are appended
/// in priority order (goal-relevant lifts before incidental accessories) until the
/// budget is reached, so depth on goal lifts is preferred over breadth. `sessions`
/// must be most-recent-first (as `recent_sessions_with_sets` returns them).
fn summarise_designer_history(
    sessions: &[SessionWithSets],
    goal_ids: &HashSet<i64>,
    cfg: &DesignerHistoryConfig,
    name_of: &impl Fn(i64) -> String,
) -> String {
    if sessions.is_empty() {
        return "RECENT HISTORY: none logged yet.\n".to_string();
    }

    let full_count = cfg.full_sessions.min(sessions.len());
    let (recent, older) = sessions.split_at(full_count);

    // Recent sessions in full: always included — they are the point of the block.
    let mut out = "RECENT HISTORY (most recent first):\n".to_string();
    out.extend(recent.iter().map(|(session, entries)| format_full_session(session, entries, name_of)));
    let mut used = estimate_tokens(&out);

    // Older sessions as trend lines, filled greedily under the remaining budget in
    // priority order. A line that would overflow stops the fill (the rest are lower
    // priority), so goal lifts are never dropped to fit an incidental accessory.
    let trend_lines = build_trend_lines(older, goal_ids, cfg, name_of);
    let mut emitted = 0usize;
    for line in &trend_lines {
        let header_cost = if emitted == 0 { estimate_tokens(EARLIER_TRENDS_HEADER) } else { 0 };
        if used + header_cost + estimate_tokens(line) > cfg.token_budget {
            break;
        }
        if emitted == 0 {
            out.push_str(EARLIER_TRENDS_HEADER);
        }
        out.push_str(line);
        used += header_cost + estimate_tokens(line);
        emitted += 1;
    }

    let dropped = trend_lines.len() - emitted;
    if dropped > 0 {
        out.push_str(&format!("    (+{dropped} older exercise trend(s) omitted to fit the history budget)\n"));
    }
    out
}

/// One logged set as the designer prompt reads it, e.g. "60kg x 8 easy" or
/// "60s hard". Effort is the perceived difficulty so the designer can progress or
/// back off the load.
fn format_set_desc(set: &ExerciseSet) -> String {
    let effort = set.perceived_difficulty.as_str();
    let measure = set.measurement_type.describe_value(set.value);
    match (set.measurement_type, set.count) {
        (MeasurementType::WeightReps, Some(reps)) => format!("{measure} x {reps} {effort}"),
        _ => format!("{measure} {effort}"),
    }
}

/// How long a designed-but-unstarted `/nextworkout` plan stays eligible to bind to a
/// new session. After this it is stale: a week-old design must not silently attach to
/// an unrelated workout the user happens to start.
const PROPOSED_PLAN_BIND_WINDOW_HOURS: i64 = 12;

/// Whether a `proposed` plan created at `created_at` is still recent enough to bind to
/// a session. An unparseable timestamp fails open (treated as in-window — we just
/// wrote it), matching `compute_last_activity_age_hours`'s lenient parsing.
pub(super) fn proposed_plan_within_window(created_at: &str, now: NaiveDateTime) -> bool {
    match parse_sqlite_datetime(created_at) {
        Some(ts) => (now - ts).num_hours() < PROPOSED_PLAN_BIND_WINDOW_HOURS,
        None => true,
    }
}

/// Split a leading ad-hoc marker ("adhoc", "ad-hoc", "oneoff", "one-off") off the
/// `/nextworkout` guidance text ([C1.4]). Present means the user explicitly wants a
/// one-off that leaves an active programme untouched; the rest stays designer
/// guidance. Harmless without a programme, where every design is ad-hoc anyway.
fn split_ad_hoc_marker(guidance: &str) -> (bool, String) {
    let mut words = guidance.split_whitespace();
    match words.next() {
        Some(first) if ["adhoc", "ad-hoc", "oneoff", "one-off"].contains(&first.to_lowercase().as_str()) => {
            (true, words.collect::<Vec<_>>().join(" "))
        }
        _ => (false, guidance.to_string()),
    }
}

/// The wire-facing mode for a designed workout, or `None` for plain ad-hoc with no
/// programme in play — which keeps travelling as the pre-programme [`View::Workout`],
/// so a user with no programme sees exactly today's output.
fn mode_view(mode: &TrainingMode) -> Option<TrainingModeView> {
    match mode {
        TrainingMode::AdHoc { programme: None } => None,
        TrainingMode::AdHoc { programme: Some(programme) } => {
            Some(TrainingModeView::AdHoc { program_title: programme.title.clone() })
        }
        TrainingMode::Programme { programme, slot } => Some(TrainingModeView::Program {
            program_title: programme.title.clone(),
            week: slot.week_idx.max(0) as u32,
            day: slot.day_idx.max(0) as u32,
            focus: slot.focus.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::*;
    use crate::db::{new_exercise_entry_at, new_exercise_goal, new_exercise_set, new_user};
    use crate::telegram::Message as TgMessage;

    #[tokio::test]
    async fn nextworkout_without_philosophy_offers_to_build_one() {
        let (handler, _) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        assert!(shown(&reply).contains("/philosophy"), "should point the user at /philosophy first");
    }

    #[tokio::test]
    async fn nextworkout_designs_and_persists_plan_without_logging() {
        let design = r#"{"message": "Here's today's session.", "actions": [
            {"type": "propose_workout", "title": "Upper push", "rationale": "Bench was easy; push it.", "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 65.0, "notes": "drive through heels"},
                {"exercise": "One Arm Dumbbell Row", "target_sets": 3, "target_reps": 8, "target_weight_kg": 24.0}
            ]}
        ]}"#;
        let (handler, _) = setup_handler(design).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "goal=hypertrophy; 5x5; dumbbells to 24kg", "interview").unwrap();
        }

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Upper push"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("3 sets × 6 reps @ 65kg"));
        // [C1.4]: no programme in play, so no mode line — exactly the pre-programme output.
        assert!(!text.contains("Programme:") && !text.contains("Ad-hoc session"), "no mode line without a programme: {text}");

        let db = handler.db.lock().await;
        let plan = db.latest_draft_roster(user.id).unwrap().expect("a proposed plan should be persisted");
        assert_eq!(plan.title, "Upper push");
        assert_eq!(plan.programme_slot_id, None, "a plan designed with no programme is ad-hoc");
        assert_eq!(db.list_roster_exercises(plan.id).unwrap().len(), 2);
        // The crucial guarantee: designing logs NOTHING.
        assert!(db.get_active_session(user.id).unwrap().is_none(), "nextworkout must not start a session");
        assert!(db.recent_sessions_with_sets(user.id, 5).unwrap().is_empty(), "nextworkout must not log sets");
    }

    #[tokio::test]
    async fn nextworkout_drops_unresolved_exercise_with_note() {
        let design = r#"{"message": "Plan ready.", "actions": [
            {"type": "propose_workout", "title": "Mixed", "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 8, "target_weight_kg": 60.0},
                {"exercise": "Jetpack Flips", "target_sets": 3, "target_reps": 8}
            ]}
        ]}"#;
        let (handler, _) = setup_handler(design).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "general fitness", "interview").unwrap();
        }

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Bench Press"));
        assert!(text.contains("Skipped") && text.contains("Jetpack Flips"), "unresolved exercise should be noted, not fatal");

        let db = handler.db.lock().await;
        let plan = db.latest_draft_roster(user.id).unwrap().unwrap();
        assert_eq!(db.list_roster_exercises(plan.id).unwrap().len(), 1, "only the resolved exercise is persisted");
    }

    const DESIGN_RESPONSE: &str = r#"{"message": "Here's your session.", "actions": [
        {"type": "propose_workout", "title": "Upper", "exercises": [
            {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 65.0}
        ]}
    ]}"#;

    /// Give `user_id` an active programme ("12-week hypertrophy") with a two-slot
    /// week-1 grid, returning the slot ids in (week, day) order.
    async fn activate_program_with_slots(handler: &AssistantHandler, user_id: i64) -> Vec<i64> {
        let db = handler.db.lock().await;
        let program = crate::db::new_programme(user_id, "12-week hypertrophy", 2, "upper/lower", "double progression");
        let program_id = db.create_programme(&program).unwrap();
        db.activate_programme(program_id).unwrap();
        [(1, 1, "upper"), (1, 2, "lower")]
            .iter()
            .map(|(w, d, focus)| db.add_programme_slot(&crate::db::new_programme_slot(program_id, *w, *d, focus)).unwrap())
            .collect()
    }

    /// [C1.4]: with an active programme, `/nextworkout` designs against the current
    /// slot — the plan records it, the slot fills, and the user is told which slot
    /// today's session is.
    #[tokio::test]
    async fn nextworkout_under_active_programme_fills_the_current_slot() {
        let (handler, _) = setup_handler(DESIGN_RESPONSE).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "5x5, dumbbells to 24kg", "interview").unwrap();
        }
        let slots = activate_program_with_slots(&handler, user.id).await;

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Programme: 12-week hypertrophy — week 1, day 1: upper"), "mode line missing: {text}");

        let db = handler.db.lock().await;
        let plan = db.latest_draft_roster(user.id).unwrap().unwrap();
        assert_eq!(plan.programme_slot_id, Some(slots[0]), "the plan records the slot it fills");
        assert_eq!(db.get_programme_slot(slots[0]).unwrap().unwrap().status, crate::db::SlotStatus::Filled);
        assert_eq!(db.get_programme_slot(slots[1]).unwrap().unwrap().status, crate::db::SlotStatus::Pending);
    }

    /// [C1.4]: "/nextworkout adhoc …" during an active programme is a legitimate
    /// one-off ("travelling, dumbbells only"): recorded and surfaced as ad-hoc, the
    /// marker stripped from the designer guidance, and adherence never moves — every
    /// slot stays pending through design, execution and session end.
    #[tokio::test]
    async fn adhoc_nextworkout_under_active_programme_leaves_slots_untouched() {
        let (handler, llm) = setup_handler(DESIGN_RESPONSE).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "5x5, dumbbells to 24kg", "interview").unwrap();
        }
        let slots = activate_program_with_slots(&handler, user.id).await;

        let reply = handler.handle_text_message(&msg, "/nextworkout adhoc dumbbells only").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Ad-hoc session — 12-week hypertrophy is untouched"), "mode line missing: {text}");

        let design_request = llm.recorded_requests().pop().unwrap();
        assert_eq!(design_request.messages.last().unwrap().content, "dumbbells only", "the adhoc marker is not designer guidance");

        {
            let db = handler.db.lock().await;
            let plan = db.latest_draft_roster(user.id).unwrap().unwrap();
            assert_eq!(plan.programme_slot_id, None, "an ad-hoc plan records no slot");
        }

        // Run the design through its whole lifecycle: start a session (binding the
        // plan for guided execution), then end it (completing the plan).
        llm.set_response(r#"{"message": "Let's go!", "actions": [{"type": "start_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "start my workout").await.unwrap();
        llm.set_response(r#"{"message": "Nice work!", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "done").await.unwrap();

        let db = handler.db.lock().await;
        assert_eq!(db.latest_draft_roster(user.id).unwrap().map(|p| p.id), None);
        slots.iter().for_each(|slot| {
            assert_eq!(
                db.get_programme_slot(*slot).unwrap().unwrap().status,
                crate::db::SlotStatus::Pending,
                "an ad-hoc session must not move any slot status"
            );
        });
    }

    #[tokio::test]
    async fn nextworkout_without_valid_proposal_fails_loud_and_persists_nothing() {
        // The designer replies with prose but never emits a `propose_workout` action.
        // The old behaviour rendered that prose as if a workout had been created while
        // saving nothing; now it must fail loudly and persist no plan (ticket C1.7).
        let (handler, _) = setup_handler("Here's a great session: heavy bench and some squats. Have fun!").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "general fitness", "interview").unwrap();
        }

        let reply = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        let text = shown(&reply).to_lowercase();
        assert!(text.contains("couldn't design a valid workout"), "should surface a clear retry notice, got: {text}");

        let db = handler.db.lock().await;
        assert!(db.latest_draft_roster(user.id).unwrap().is_none(), "no phantom plan may be persisted on a failed design");
    }

    /// Register a user, give them a philosophy, design a one-exercise workout, then
    /// start a session — returning the user once a guided plan is bound.
    async fn start_guided_workout(handler: &AssistantHandler, llm: &MockLlm, msg: &TgMessage) -> User {
        let _ = handler.handle_text_message(msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "5x5, dumbbells to 24kg", "interview").unwrap();
        }
        llm.set_response(
            r#"{"message": "Here's your session.", "actions": [
                {"type": "propose_workout", "title": "Push", "exercises": [
                    {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 60.0}
                ]}
            ]}"#,
        );
        let _ = handler.handle_text_message(msg, "/nextworkout").await.unwrap();
        llm.set_response(r#"{"message": "Let's go!", "actions": [{"type": "start_session"}]}"#);
        let _ = handler.handle_text_message(msg, "let's start the workout").await.unwrap();
        user
    }

    #[tokio::test]
    async fn starting_a_session_binds_the_designed_plan_and_prescribes() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;

        {
            let db = handler.db.lock().await;
            let plan = db.active_roster_for_user(user.id).unwrap().expect("plan should be active after start");
            let session = db.get_active_session(user.id).unwrap().unwrap();
            assert_eq!(plan.session_id, Some(session.id), "plan bound to the session");
            assert!(db.latest_draft_roster(user.id).unwrap().is_none(), "no longer 'proposed' once active");
        }

        // The next turn's system prompt carries the guided prescription.
        llm.set_response(r#"{"message": "ok", "actions": []}"#);
        let _ = handler.handle_text_message(&msg, "ready").await.unwrap();
        let last = llm.recorded_requests().pop().unwrap();
        let system = &last.messages[0].content;
        assert!(system.contains("PRESCRIBED WORKOUT"), "guided section should be in the prompt");
        assert!(system.contains("Bench Press"));
    }

    #[tokio::test]
    async fn mid_workout_note_appends_to_philosophy_and_end_completes_plan() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;
        let plan_id = handler.db.lock().await.active_roster_for_user(user.id).unwrap().unwrap().id;

        // A durable preference voiced mid-workout is appended to the philosophy.
        llm.set_response(r#"{"message": "Got it.", "actions": [{"type": "append_philosophy_note", "note": "prefers goblet squats"}]}"#);
        let reply = handler.handle_text_message(&msg, "I always prefer goblet squats").await.unwrap();
        assert!(shown(&reply).contains("Noted for future workouts"));
        {
            let latest = handler.db.lock().await.latest_philosophy(user.id).unwrap().unwrap();
            assert!(latest.content.contains("goblet squats"));
            assert_eq!(latest.source, "note");
        }

        // Ending the session completes the bound plan.
        llm.set_response(r#"{"message": "Nice work!", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "done").await.unwrap();
        let db = handler.db.lock().await;
        assert!(db.active_roster_for_user(user.id).unwrap().is_none());
        assert_eq!(db.get_roster(plan_id).unwrap().unwrap().status, crate::db::LifecycleStatus::Completed);
    }

    #[tokio::test]
    async fn one_off_override_scopes_to_plan_and_spares_philosophy() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;
        let plan_id = handler.db.lock().await.active_roster_for_user(user.id).unwrap().unwrap().id;

        // A one-off voiced mid-workout attaches to the plan in flight, NOT the philosophy.
        llm.set_response(
            r#"{"message": "Sure, flys today.", "actions": [{"type": "set_session_override", "note": "no bench today, do flys instead"}]}"#,
        );
        let reply = handler.handle_text_message(&msg, "I don't feel like bench today, let's do flys").await.unwrap();
        assert!(shown(&reply).contains("just for this workout"));

        {
            let db = handler.db.lock().await;
            let plan = db.get_roster(plan_id).unwrap().unwrap();
            assert!(plan.override_note.as_deref().unwrap().contains("flys"), "override stored on the plan");
            // The philosophy must be untouched — a one-off there would ban bench forever.
            let latest = db.latest_philosophy(user.id).unwrap().unwrap();
            assert_eq!(latest.source, "interview", "no philosophy note should have been appended");
            assert!(!latest.content.contains("bench"), "one-off must not reach the philosophy");
        }

        // On the next turn the override surfaces in the coaching prompt for the plan in flight.
        llm.set_response(r#"{"message": "ok", "actions": []}"#);
        let _ = handler.handle_text_message(&msg, "what's next?").await.unwrap();
        let last = llm.recorded_requests().pop().unwrap();
        let system = &last.messages[0].content;
        assert!(system.contains("TODAY-ONLY OVERRIDES"), "override must reach the coaching prompt");
        assert!(system.contains("flys"));

        // End the session, then design a fresh plan: the one-off does NOT carry over.
        llm.set_response(r#"{"message": "Done!", "actions": [{"type": "end_session"}]}"#);
        let _ = handler.handle_text_message(&msg, "done").await.unwrap();
        llm.set_response(
            r#"{"message": "New plan.", "actions": [{"type": "propose_workout", "title": "Pull", "exercises": [
                {"exercise": "Bench Press", "target_sets": 3, "target_reps": 6, "target_weight_kg": 60.0}
            ]}]}"#,
        );
        let _ = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();

        let db = handler.db.lock().await;
        let fresh = db.latest_draft_roster(user.id).unwrap().unwrap();
        assert_ne!(fresh.id, plan_id, "a new plan was designed");
        assert!(fresh.override_note.is_none(), "the one-off must not survive into the next design");
    }

    // ── Science grounding [C5.2] ──────────────────────────────────────────────

    /// Register a user, give them a philosophy, and run `/nextworkout`, returning the system prompt
    /// the designer actually sent. Goals are seeded by `seed` before the design runs.
    async fn designer_system_prompt(seed: impl Fn(&Database, i64)) -> String {
        let (handler, llm) = setup_handler(DESIGN_RESPONSE).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            let db = handler.db.lock().await;
            db.insert_philosophy(user.id, "5x5, dumbbells to 24kg", "interview").unwrap();
            seed(&db, user.id);
        }
        let _ = handler.handle_text_message(&msg, "/nextworkout").await.unwrap();
        llm.recorded_requests().pop().unwrap().messages[0].content.clone()
    }

    /// Insert a goal of `kind` at `priority`, back-dating `start_date` so the date-windowed
    /// `goal_progress_report` returns it (see [`seed_goal`]).
    fn seed_goal_of_kind(db: &Database, user_id: i64, kind: crate::db::GoalKind, priority: i64, metric: &str) {
        let mut goal = crate::db::new_goal(user_id, kind, None, Some(metric.to_string()), 80.0, crate::db::GoalDirection::Decrease);
        goal.start_date = "2026-01-01".into();
        goal.priority = priority;
        db.insert_goal(&goal).unwrap();
    }

    /// The whole point of [C5.2], through the production path: a strength goal must put the
    /// corpus's strength bands in front of the model, not leave it on its own recall.
    #[tokio::test]
    async fn nextworkout_grounds_the_prompt_in_the_corpus_band_for_the_goal() {
        let prompt = designer_system_prompt(|db, user_id| {
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            seed_goal(db, user_id, bench.id);
        })
        .await;

        assert!(prompt.contains("TRAINING SCIENCE"), "the designer prompt must carry a science section:\n{prompt}");
        assert!(prompt.contains("[S:goal-strength]"), "the strength document must be cited");
        assert!(prompt.contains("1-6 per working set"), "the repetition band must reach the model");
        assert!(prompt.contains("80-90% of a one-rep maximum"), "the intensity band must reach the model");
        assert!(prompt.contains("3-5 minutes"), "the rest band must reach the model");
    }

    /// Genuinely competing goals: the prompt must rank them by priority and carry the corpus's
    /// resolution, rather than leaving the model to average a strength goal and a fat-loss goal
    /// into a session that serves neither.
    #[tokio::test]
    async fn competing_goals_reach_the_prompt_ranked_and_with_the_curated_resolution() {
        let prompt = designer_system_prompt(|db, user_id| {
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let mut strength = new_exercise_goal(user_id, bench.id, 100.0);
            strength.start_date = "2026-01-01".into();
            strength.priority = 2;
            db.insert_goal(&strength).unwrap();
            seed_goal_of_kind(db, user_id, crate::db::GoalKind::BodyComposition, 7, "body_fat_pct");
        })
        .await;

        let block = prompt.split("COMPETING GOALS").nth(1).unwrap_or_else(|| panic!("no competing-goals block:\n{prompt}"));
        assert!(
            block.find("body_composition") < block.find("strength"),
            "the higher-priority goal must be listed first:\n{block}"
        );
        assert!(prompt.contains("[S:competing-goals]"), "the resolution document must be pinned in");
        // Both goals' prescriptions must survive: resolving by priority is not the same as
        // dropping the lower goal, and the model cannot honour a band it was never shown.
        assert!(prompt.contains("[S:goal-body-composition]") && prompt.contains("[S:goal-strength]"), "both prescriptions must land");
        assert!(!prompt.contains("omitted to fit the prompt budget"), "two goals plus a resolution must fit the budget:\n{prompt}");
        assert!(prompt.contains("Goals are resolved by **priority**, not by averaging"), "the corpus rule itself must be present");
    }

    /// One kind of goal is not a conflict, and the resolution block must not appear for it.
    #[tokio::test]
    async fn a_lone_goal_produces_science_but_no_competing_goals_block() {
        let prompt = designer_system_prompt(|db, user_id| {
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            seed_goal(db, user_id, bench.id);
        })
        .await;
        assert!(!prompt.contains("COMPETING GOALS"));
        assert!(prompt.contains("TRAINING SCIENCE"));
    }

    /// An active injury is a hard constraint, so its contraindications outrank the goal for the
    /// scarce science budget. [C5.4] turns this into a check on the design itself; here it is only
    /// a guarantee that the guidance is in front of the model at all.
    #[tokio::test]
    async fn an_active_injury_puts_its_contraindications_in_the_prompt() {
        let prompt = designer_system_prompt(|db, user_id| {
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            seed_goal(db, user_id, bench.id);
            let mut entry = crate::db::new_health_entry(user_id, crate::db::HealthEntryType::Injury, "lower back twinge");
            // Free text as the assistant records it — the corpus spells it `lower_back`.
            entry.body_part = Some("lower back".into());
            db.insert_health_entry(&entry).unwrap();
        })
        .await;
        assert!(prompt.contains("[S:injury-lower-back]"), "the injury document must reach the prompt:\n{prompt}");
    }

    #[test]
    fn the_science_query_ranks_goal_kinds_by_priority_and_pins_the_resolution() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let mut strength = new_exercise_goal(user_id, bench.id, 100.0);
        strength.start_date = "2026-01-01".into();
        strength.priority = 1;
        db.insert_goal(&strength).unwrap();
        seed_goal_of_kind(&db, user_id, crate::db::GoalKind::BodyComposition, 8, "body_fat_pct");

        let goals = db.goal_progress_report(user_id, None, None).unwrap();
        let query = science_query(&goals, &[], "");

        assert_eq!(query.goal_kinds, [crate::db::GoalKind::BodyComposition, crate::db::GoalKind::Strength]);
        // Each kind's prescription in priority order, then the resolution — none of them left to
        // ranking, and the highest-priority prescription first so it survives the budget.
        assert_eq!(query.pinned_docs, ["goal-body-composition", "goal-strength", COMPETING_GOALS_DOC]);
    }

    /// `bodyweight` and `body_composition` share one prescription document; holding both must not
    /// spend two of four science slots saying the same thing twice.
    #[test]
    fn goal_kinds_sharing_a_prescription_yield_one_chunk() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        seed_goal_of_kind(&db, user_id, crate::db::GoalKind::Bodyweight, 5, "bodyweight_kg");
        seed_goal_of_kind(&db, user_id, crate::db::GoalKind::BodyComposition, 3, "body_fat_pct");

        let goals = db.goal_progress_report(user_id, None, None).unwrap();
        let query = science_query(&goals, &[], "");
        assert_eq!(query.pinned_docs, ["goal-body-composition", "goal-body-composition", COMPETING_GOALS_DOC]);

        let hits = crate::science::ScienceIndex::build().unwrap().search(&query, 4);
        let body_comp = hits.iter().filter(|c| c.doc_id == "goal-body-composition").count();
        assert_eq!(body_comp, 1, "a document pinned twice appears once: {:?}", hits.iter().map(|c| &c.doc_id).collect::<Vec<_>>());
    }

    #[test]
    fn a_science_query_for_one_kind_pins_nothing() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        seed_goal(&db, user_id, bench.id);
        // A second goal of the same kind: still one prescription, so still no conflict.
        seed_goal_of_kind(&db, user_id, crate::db::GoalKind::Strength, 3, "grip_kg");

        let goals = db.goal_progress_report(user_id, None, None).unwrap();
        assert_eq!(goals.len(), 2, "both goals should be active");
        let query = science_query(&goals, &[], "");
        assert_eq!(query.goal_kinds, [crate::db::GoalKind::Strength]);
        assert_eq!(query.pinned_docs, ["goal-strength"], "the prescription is pinned; the resolution is not, since nothing competes");
    }

    #[test]
    fn injury_body_parts_reach_the_query_normalised_and_unknown_ones_are_dropped() {
        let entry = |part: Option<&str>| {
            let mut e = crate::db::new_health_entry(1, crate::db::HealthEntryType::Injury, "ouch");
            e.body_part = part.map(str::to_string);
            e
        };
        let injuries = [entry(Some("lower back")), entry(Some("elbow")), entry(Some("aura")), entry(None)];
        assert_eq!(science_query(&[], &injuries, "").injuries, ["lower_back", "elbow"]);
    }

    #[test]
    fn ad_hoc_marker_splits_off_the_first_word_only() {
        assert_eq!(split_ad_hoc_marker("adhoc dumbbells only"), (true, "dumbbells only".to_string()));
        assert_eq!(split_ad_hoc_marker("Ad-Hoc"), (true, String::new()));
        assert_eq!(split_ad_hoc_marker("ONEOFF light"), (true, "light".to_string()));
        assert_eq!(split_ad_hoc_marker("one-off legs"), (true, "legs".to_string()));
        // Only a *leading* marker counts; ordinary guidance passes through whole.
        assert_eq!(split_ad_hoc_marker("but something lighter"), (false, "but something lighter".to_string()));
        assert_eq!(split_ad_hoc_marker("go adhoc today"), (false, "go adhoc today".to_string()));
        assert_eq!(split_ad_hoc_marker(""), (false, String::new()));
    }

    #[test]
    fn proposed_plan_window_excludes_stale_designs() {
        let now = parse_sqlite_datetime("2026-06-19 12:00:00").unwrap();
        // Designed an hour ago → still bindable.
        assert!(proposed_plan_within_window("2026-06-19 11:00:00", now));
        // Designed yesterday → stale, must not bind to today's session.
        assert!(!proposed_plan_within_window("2026-06-18 11:00:00", now));
        // Exactly at the 12h cutoff → stale (the window is exclusive).
        assert!(!proposed_plan_within_window("2026-06-19 00:00:00", now));
        // Unparseable timestamp fails open (we just wrote it).
        assert!(proposed_plan_within_window("not a date", now));
    }

    // ── Designer history window ───────────────────────────────────────────────

    /// Insert one session at `started_at`, each exercise in its own entry (as real
    /// logging does) so per-exercise trends resolve independently.
    fn seed_dated_session(db: &Database, user_id: i64, started_at: &str, exercises: &[(i64, f64, i32)]) {
        let session_id = db.start_session_at(user_id, started_at, None, None).unwrap();
        for (et_id, weight, count) in exercises {
            let entry_id = db.insert_entry(&new_exercise_entry_at(user_id, Some(session_id), None, started_at)).unwrap();
            let mut s = new_exercise_set(entry_id, *et_id, MeasurementType::WeightReps, *weight);
            s.count = Some(*count);
            s.logged_at = started_at.to_string();
            db.insert_set(&s).unwrap();
        }
    }

    /// Insert an active goal whose `start_date` is a past date-only string so the
    /// date-windowed `goal_progress_report` returns it (its default now-datetime
    /// start would sort after today's date-only bound and be filtered out).
    fn seed_goal(db: &Database, user_id: i64, exercise_type_id: i64) {
        let mut goal = new_exercise_goal(user_id, exercise_type_id, 100.0);
        goal.start_date = "2026-01-01".into();
        db.insert_goal(&goal).unwrap();
    }

    /// A name resolver over the seeded catalogue, matching what the handler passes in.
    fn name_resolver(db: &Database) -> impl Fn(i64) -> String {
        let names: std::collections::HashMap<i64, String> =
            db.list_exercise_types_with_ancestry().unwrap().into_iter().map(|e| (e.exercise_type.id, e.exercise_type.name)).collect();
        move |id| names.get(&id).cloned().unwrap_or_else(|| "unknown".to_string())
    }

    #[test]
    fn goal_relevant_ids_include_goal_exercise_and_descendants() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let variation = db.get_exercise_type_by_name("Flat Barbell Bench Press").unwrap().unwrap();
        let curl = db.get_exercise_type_by_name("Bicep Curl").unwrap().unwrap();

        seed_goal(&db, user_id, bench.id);
        let goals = db.goal_progress_report(user_id, None, None).unwrap();
        let ids = goal_relevant_exercise_ids(&db, &goals).unwrap();

        assert!(ids.contains(&bench.id), "the goal exercise itself is goal-relevant");
        assert!(ids.contains(&variation.id), "descendant variations roll up under a parent-node goal");
        assert!(!ids.contains(&curl.id), "unrelated exercises are not goal-relevant");
    }

    #[test]
    fn designer_history_keeps_recent_full_and_collapses_older_to_trends() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let curl = db.get_exercise_type_by_name("Bicep Curl").unwrap().unwrap();

        // Six sessions, oldest → newest, each training the goal lift and an accessory.
        for (i, day) in (1..=6).enumerate() {
            let weight = 50.0 + i as f64 * 2.5;
            seed_dated_session(&db, user_id, &format!("2026-05-0{day} 10:00:00"), &[(bench.id, weight, 8), (curl.id, 15.0 + i as f64, 10)]);
        }
        seed_goal(&db, user_id, bench.id);

        let goals = db.goal_progress_report(user_id, None, None).unwrap();
        let goal_ids = goal_relevant_exercise_ids(&db, &goals).unwrap();
        let sessions = db.recent_sessions_with_sets(user_id, 40).unwrap();
        let name_of = name_resolver(&db);

        let cfg = DesignerHistoryConfig {
            token_budget: 100_000,
            full_sessions: 2,
            max_sessions: 40,
            goal_trend_points: 5,
            accessory_trend_points: 1,
        };
        let out = summarise_designer_history(&sessions, &goal_ids, &cfg, &name_of);

        // Only the two most recent sessions render in full; the rest collapse to trends.
        assert_eq!(out.matches("- Session ").count(), 2, "only the 2 most recent sessions are full:\n{out}");
        assert!(out.contains("2026-05-06") && out.contains("2026-05-05"), "the two newest sessions are present in full");
        assert!(out.contains("EARLIER TRENDS"), "older sessions collapse under a trends header:\n{out}");

        // Depth over breadth: the goal lift keeps several trend points (arrows), the
        // incidental accessory collapses to a single point.
        let bench_trend = out.lines().find(|l| l.trim_start().starts_with("Bench Press*:")).expect("goal lift trend, marked *");
        let curl_trend = out.lines().find(|l| l.trim_start().starts_with("Bicep Curl:")).expect("accessory trend line");
        assert!(bench_trend.matches("->").count() >= 2, "goal lift retains more history: {bench_trend}");
        assert_eq!(curl_trend.matches("->").count(), 0, "accessory collapses to one point: {curl_trend}");
    }

    #[test]
    fn designer_history_token_budget_prefers_goal_lift_over_accessory() {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let curl = db.get_exercise_type_by_name("Bicep Curl").unwrap().unwrap();

        // Older sessions train both lifts; the newest (the single full session) is
        // bench-only, so the accessory can only ever appear as a trend line.
        for (i, day) in (1..=3).enumerate() {
            seed_dated_session(&db, user_id, &format!("2026-05-0{day} 10:00:00"), &[(bench.id, 50.0 + i as f64, 8), (curl.id, 15.0, 10)]);
        }
        seed_dated_session(&db, user_id, "2026-05-04 10:00:00", &[(bench.id, 55.0, 8)]);
        seed_goal(&db, user_id, bench.id);

        let goals = db.goal_progress_report(user_id, None, None).unwrap();
        let goal_ids = goal_relevant_exercise_ids(&db, &goals).unwrap();
        let sessions = db.recent_sessions_with_sets(user_id, 40).unwrap();
        let name_of = name_resolver(&db);

        let mut cfg = DesignerHistoryConfig {
            token_budget: usize::MAX,
            full_sessions: 1,
            max_sessions: 40,
            goal_trend_points: 5,
            accessory_trend_points: 3,
        };

        // Size the budget to fit the recent block + trends header + the first (goal)
        // trend line exactly — leaving no room for the accessory line that follows.
        let (recent, older) = sessions.split_at(cfg.full_sessions);
        let mut recent_block = "RECENT HISTORY (most recent first):\n".to_string();
        recent_block.extend(recent.iter().map(|(s, e)| format_full_session(s, e, &name_of)));
        let lines = build_trend_lines(older, &goal_ids, &cfg, &name_of);
        assert!(lines[0].trim_start().starts_with("Bench Press*:"), "goal lift sorts ahead of the accessory: {lines:?}");
        cfg.token_budget = estimate_tokens(&recent_block) + estimate_tokens(EARLIER_TRENDS_HEADER) + estimate_tokens(&lines[0]);

        let out = summarise_designer_history(&sessions, &goal_ids, &cfg, &name_of);
        assert!(out.contains("Bench Press*:"), "the goal lift trend survives the budget:\n{out}");
        assert!(!out.contains("Bicep Curl"), "the accessory trend is dropped under budget:\n{out}");
        assert!(out.contains("omitted to fit the history budget"), "a truncation note is emitted:\n{out}");
    }
}

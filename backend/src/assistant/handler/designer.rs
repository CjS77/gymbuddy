//! The `/nextworkout` designer: builds a tailored workout from philosophy,
//! recent history, recovery, goals and injuries, persists it as a `proposed`
//! plan, and bounds how long that proposal stays eligible to bind to a session.

use std::collections::{BTreeMap, HashSet};

use anyhow::Context as _;
use chrono::NaiveDateTime;

use crate::assistant::actions::{AssistantAction, ProposedExercise};
use crate::assistant::matching::find_exercise_type;
use crate::assistant::parser::parse_assistant_response;
use crate::assistant::prompts::{build_designer_prompt, format_muscle_recovery};
use crate::config::DesignerHistoryConfig;
use crate::db::{
    Database, EntryWithSets, ExerciseSet, GoalProgress, MeasurementType, Session, SessionWithSets, User, WorkoutPhilosophy,
    WorkoutPlanExercise,
};

use super::AssistantHandler;
use super::continuity::parse_sqlite_datetime;
use gymbuddy_proto::{PlannedExerciseView, View, WorkoutView};

impl AssistantHandler {
    /// Design a tailored workout from the user's philosophy, recent history, goals,
    /// and injuries, and present it as a [`View::Workout`]. Persists a `proposed`
    /// plan but logs NOTHING and starts no session. Any text after the command
    /// ("/nextworkout but something lighter") is passed to the designer as guidance.
    pub(super) async fn cmd_next_workout(&self, user: &User, text: &str) -> anyhow::Result<View> {
        let guidance = text.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");

        let (philosophy, sessions, recovery, goals, injuries, goal_ids) = {
            let db = self.db.lock().await;
            let goals = db.goal_progress_report(user.id, None, None)?;
            let goal_ids = goal_relevant_exercise_ids(&db, &goals)?;
            // Pull a generous window (bounded by config); the summariser decides how
            // much of it survives into the prompt under the token budget.
            (
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
        let prompt = build_designer_prompt(&philosophy.content, &history_block, &recovery_block, &goals, &injuries, &self.catalogue);
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

        self.persist_and_view_workout(user, &philosophy, title, rationale, exercises).await
    }

    /// Persist a designed workout as a `proposed` plan and build its [`View::Workout`].
    /// Exercise names are resolved against the catalogue; unresolved ones are dropped
    /// with a note rather than failing the whole design.
    async fn persist_and_view_workout(
        &self,
        user: &User,
        philosophy: &WorkoutPhilosophy,
        title: String,
        rationale: Option<String>,
        exercises: Vec<ProposedExercise>,
    ) -> anyhow::Result<View> {
        let mut planned: Vec<PlannedExerciseView> = Vec::new();
        let mut notes: Vec<String> = Vec::new();

        let db = self.db.lock().await;
        let plan_id = db.create_plan(user.id, &title, rationale.as_deref(), Some(philosophy.id))?;

        for (idx, ex) in exercises.into_iter().enumerate() {
            let Some(et) = find_exercise_type(&self.catalogue, &ex.exercise) else {
                notes.push(format!("Skipped \"{}\" -- I couldn't match it to a known exercise.", ex.exercise));
                continue;
            };
            db.add_plan_exercise(&WorkoutPlanExercise {
                id: 0,
                plan_id,
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

        Ok(View::Workout(WorkoutView { title, rationale, exercises: planned, notes }))
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

/// Rough token estimate for budgeting the history block: ~4 chars per token. Only
/// needs to be monotonic and in the right ballpark, not exact.
fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
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
    let mut out = format!("- Session {}:\n", session.started_at);
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

        let db = handler.db.lock().await;
        let plan = db.latest_proposed_plan(user.id).unwrap().expect("a proposed plan should be persisted");
        assert_eq!(plan.title, "Upper push");
        assert_eq!(db.list_plan_exercises(plan.id).unwrap().len(), 2);
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
        let plan = db.latest_proposed_plan(user.id).unwrap().unwrap();
        assert_eq!(db.list_plan_exercises(plan.id).unwrap().len(), 1, "only the resolved exercise is persisted");
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
        assert!(db.latest_proposed_plan(user.id).unwrap().is_none(), "no phantom plan may be persisted on a failed design");
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
            let plan = db.active_plan_for_user(user.id).unwrap().expect("plan should be active after start");
            let session = db.get_active_session(user.id).unwrap().unwrap();
            assert_eq!(plan.session_id, Some(session.id), "plan bound to the session");
            assert!(db.latest_proposed_plan(user.id).unwrap().is_none(), "no longer 'proposed' once active");
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
        let plan_id = handler.db.lock().await.active_plan_for_user(user.id).unwrap().unwrap().id;

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
        assert!(db.active_plan_for_user(user.id).unwrap().is_none());
        assert_eq!(db.get_plan(plan_id).unwrap().unwrap().status, crate::db::PlanStatus::Completed);
    }

    #[tokio::test]
    async fn one_off_override_scopes_to_plan_and_spares_philosophy() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = start_guided_workout(&handler, &llm, &msg).await;
        let plan_id = handler.db.lock().await.active_plan_for_user(user.id).unwrap().unwrap().id;

        // A one-off voiced mid-workout attaches to the plan in flight, NOT the philosophy.
        llm.set_response(
            r#"{"message": "Sure, flys today.", "actions": [{"type": "set_session_override", "note": "no bench today, do flys instead"}]}"#,
        );
        let reply = handler.handle_text_message(&msg, "I don't feel like bench today, let's do flys").await.unwrap();
        assert!(shown(&reply).contains("just for this workout"));

        {
            let db = handler.db.lock().await;
            let plan = db.get_plan(plan_id).unwrap().unwrap();
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
        let fresh = db.latest_proposed_plan(user.id).unwrap().unwrap();
        assert_ne!(fresh.id, plan_id, "a new plan was designed");
        assert!(fresh.override_note.is_none(), "the one-off must not survive into the next design");
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
        db.conn()
            .execute("INSERT INTO sessions (user_id, started_at) VALUES (?1, ?2)", rusqlite::params![user_id, started_at])
            .unwrap();
        let session_id = db.conn().last_insert_rowid();
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

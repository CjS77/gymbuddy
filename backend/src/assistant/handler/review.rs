//! The post-session review ([C6.5]) — step 5 of the North Star.
//!
//! # Two tiers, and why the split is a safety property
//!
//! An **ad-hoc** session gets a `summary`: assembled entirely from the logged sets, with
//! **no LLM call at all**. A **programme** session gets a `report`: the same deterministic
//! assembly, and then exactly **one** commentary call grounded in the deltas that assembly
//! already computed.
//!
//! The ordering is the point. Every number a review states — the deltas, the records, the
//! adherence, the goal percentages — is computed here, in Rust, from the database, before
//! the model is consulted at all; the model only ever comments on figures it was handed.
//! Inverting that, and letting the model decide what the numbers are, is how a review ends
//! up congratulating a user for a session they did not have.
//!
//! # Snapshot semantics
//!
//! A review records what was true when the session ended. It is serialised to JSON and
//! stored in `session_reviews.body`, and `/review` replays that stored text verbatim — so
//! editing a set next week does not rewrite last week's review. The one exception is the
//! chart series, which are recomputed live on every render: a chart of the user's history
//! is a current view of that history, not part of the record. The schema says the same
//! thing, in the comment above the table.

use anyhow::Context as _;
use gymbuddy_proto::{
    Direction, ReviewEffortView, ReviewExerciseView, ReviewKindView, ReviewRecordView, SeriesPointView, SeriesShape, SeriesView,
    SessionReviewView, TrainingModeView,
};
use serde::{Deserialize, Serialize};

use super::AssistantHandler;
use crate::assistant::parser::parse_assistant_response;
use crate::db::{
    Database, EffortSource, ExerciseDelta, GoalDirection, GoalProgress, GoalStatus, MeasurementType, PerformedRollup, RosterVsActual,
    Session, SessionPersonalRecord, SessionRoster, UnrosteredExercise, User,
};
use crate::science::ScienceQuery;
use crate::text::strip_markdown;

/// How many science chunks the commentary prompt carries. Fewer than the designer's: the
/// commentary is two to four sentences, and a wall of citations behind that much prose
/// crowds out the deltas it is supposed to be reading.
const SCIENCE_CHUNK_K: usize = 2;

/// Token ceiling for the commentary call. Generous for four sentences, tight enough that a
/// model minded to write an essay gets cut off rather than burying the stats above it.
const COMMENTARY_MAX_TOKENS: u32 = 512;

/// Slightly warmer than the action-extracting calls: this one writes prose, and the numbers
/// it may use are fixed by the prompt rather than by the sampler.
const COMMENTARY_TEMPERATURE: f32 = 0.3;

/// The stored shape of a review — everything except the chart series.
///
/// Deliberately a backend type rather than the wire [`SessionReviewView`]. The two happen to
/// carry nearly the same fields today, but they answer to different masters: the wire type is
/// append-only for postcard's sake, while this one has to stay readable years after it was
/// written. Every field is `#[serde(default)]` so that adding one later leaves old snapshots
/// loadable instead of turning them into a deserialisation error — which, for a record whose
/// entire purpose is to be the thing that does not change, would be the one unforgivable bug.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReviewSnapshot {
    #[serde(default)]
    headline: String,
    #[serde(default)]
    programme_mode: bool,
    #[serde(default)]
    session_date: String,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    effort: Option<StoredEffort>,
    #[serde(default)]
    exercises: Vec<StoredExercise>,
    #[serde(default)]
    records: Vec<StoredRecord>,
    #[serde(default)]
    commentary: Option<String>,
    #[serde(default)]
    goals: Vec<String>,
    #[serde(default)]
    achieved_goals: Vec<String>,
    #[serde(default)]
    position: Option<StoredPosition>,
    #[serde(default)]
    adherence: Option<String>,
    #[serde(default)]
    streak_days: Option<u32>,
    #[serde(default)]
    week_line: Option<String>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEffort {
    label: String,
    confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredExercise {
    name: String,
    #[serde(default)]
    prescribed: Option<String>,
    actual: String,
    #[serde(default)]
    delta: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRecord {
    exercise: String,
    detail: String,
    #[serde(default)]
    previous: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPosition {
    programme_title: String,
    week: u32,
    day: u32,
    focus: String,
}

impl ReviewSnapshot {
    /// The `session_reviews.kind` value for this tier — the column is CHECK-constrained to
    /// exactly these two.
    fn kind(&self) -> &'static str {
        match self.programme_mode {
            true => "report",
            false => "summary",
        }
    }

    /// Rebuild the wire view, attaching freshly computed charts to the stored record.
    fn into_view(self, series: Vec<SeriesView>) -> SessionReviewView {
        SessionReviewView {
            kind: match self.programme_mode {
                true => ReviewKindView::Report,
                false => ReviewKindView::Summary,
            },
            headline: self.headline,
            session_date: self.session_date,
            intent: self.intent,
            effort: self.effort.map(|e| ReviewEffortView { label: e.label, confirmed: e.confirmed }),
            exercises: self
                .exercises
                .into_iter()
                .map(|e| ReviewExerciseView { name: e.name, prescribed: e.prescribed, actual: e.actual, delta: e.delta })
                .collect(),
            records: self
                .records
                .into_iter()
                .map(|r| ReviewRecordView { exercise: r.exercise, detail: r.detail, previous: r.previous })
                .collect(),
            commentary: self.commentary,
            goals: self.goals,
            achieved_goals: self.achieved_goals,
            position: self.position.map(|p| TrainingModeView::Programme {
                programme_title: p.programme_title,
                week: p.week,
                day: p.day,
                focus: p.focus,
            }),
            adherence: self.adherence,
            streak_days: self.streak_days,
            week_line: self.week_line,
            series,
            notes: self.notes,
        }
    }

    /// The FACTS block the commentary call is grounded in.
    ///
    /// Everything the model is allowed to cite, and nothing else. Assembled from the
    /// snapshot rather than from the database a second time, so what the prompt states and
    /// what the user is shown cannot drift apart.
    fn facts_block(&self) -> String {
        let mut out = String::from("SESSION FACTS (authoritative; every number you may use is here):\n");
        out.push_str(&format!("- Date: {}\n", self.session_date));
        if let Some(intent) = &self.intent {
            out.push_str(&format!("- The user set out to: {intent}\n"));
        }
        if let Some(position) = &self.position {
            out.push_str(&format!(
                "- Programme position: {} — week {}, day {}: {}\n",
                position.programme_title, position.week, position.day, position.focus
            ));
        }
        if let Some(adherence) = &self.adherence {
            out.push_str(&format!("- Adherence: {adherence}\n"));
        }
        if let Some(effort) = &self.effort {
            let provenance = match effort.confirmed {
                true => "the user's own verdict",
                false => "derived from the logged sets; the user has not confirmed it",
            };
            out.push_str(&format!("- Overall effort: {} ({provenance})\n", effort.label));
        }

        out.push_str("\nPER EXERCISE (performed, against what was prescribed):\n");
        out.extend(self.exercises.iter().map(|e| {
            let view = ReviewExerciseView {
                name: e.name.clone(),
                prescribed: e.prescribed.clone(),
                actual: e.actual.clone(),
                delta: e.delta.clone(),
            };
            format!("- {}\n", view.line())
        }));

        if !self.records.is_empty() {
            out.push_str("\nPERSONAL RECORDS SET THIS SESSION:\n");
            out.extend(self.records.iter().map(|r| {
                let view = ReviewRecordView { exercise: r.exercise.clone(), detail: r.detail.clone(), previous: r.previous.clone() };
                format!("- {}\n", view.line())
            }));
        }

        if !self.goals.is_empty() {
            out.push_str("\nGOAL PROGRESS:\n");
            out.extend(self.goals.iter().map(|g| format!("- {g}\n")));
        }
        if !self.achieved_goals.is_empty() {
            out.push_str("\nGOALS COMPLETED BY THIS SESSION (worth leading with):\n");
            out.extend(self.achieved_goals.iter().map(|g| format!("- {g}\n")));
        }

        if let Some(streak) = self.streak_days {
            out.push_str(&format!("\n- Training streak: {streak} consecutive days\n"));
        }
        if let Some(week) = &self.week_line {
            out.push_str(&format!("- This week so far: {week}\n"));
        }
        out
    }
}

impl AssistantHandler {
    /// Generate, persist and return the review for one session.
    ///
    /// Called from three places: `EndSession`, the stale-session auto-close, and a `/review`
    /// that finds no stored review to replay. Regenerating is always safe — the upsert
    /// replaces, and `mark_goal_achieved` keeps its original date — which is what lets an
    /// effort correction simply re-run this.
    pub(super) async fn generate_session_review(&self, user: &User, session_id: i64) -> anyhow::Result<SessionReviewView> {
        let (mut snapshot, roster_id, series) = {
            let db = self.db.lock().await;
            let facts = ReviewFacts::gather(&db, user, session_id)?;
            let series = build_series(&db, user, &facts.goals)?;
            let roster_id = facts.roster.as_ref().map(|r| r.id);
            (facts.into_snapshot(), roster_id, series)
        };

        // The lock is released before the commentary call: an LLM round-trip is seconds
        // long, and holding the database across it would stall every other turn.
        if snapshot.programme_mode {
            match self.review_commentary(user, &snapshot).await {
                Ok(text) => snapshot.commentary = Some(text),
                // A review whose numbers are all present is still worth having, so a failed
                // commentary degrades to the deterministic tier rather than losing the
                // record. It is announced rather than swallowed: silence here would read as
                // "the coach had nothing to say about this session".
                Err(e) => {
                    tracing::warn!("Review commentary failed for session {session_id}: {e:#}");
                    snapshot.notes.push("I couldn't add my read on this one, but the numbers above are complete.".to_string());
                }
            }
        }

        let body = serde_json::to_string(&snapshot).context("Failed to serialise the session review")?;
        {
            let db = self.db.lock().await;
            db.upsert_session_review(session_id, user.id, roster_id, snapshot.kind(), &body)?;
        }

        Ok(snapshot.into_view(series))
    }

    /// Generate and store a review, returning the one-line note to hand back to the user.
    ///
    /// The wrapper the action handlers call: a review that fails to generate must never take
    /// the turn down with it — the session is already ended and logged, and losing the user's
    /// reply over a failed write-up would be a far worse outcome than a missing review.
    pub(super) async fn write_session_review(&self, user: &User, session_id: i64) -> Option<String> {
        match self.generate_session_review(user, session_id).await {
            Ok(view) => Some(format!("{} /review for the full write-up.", view.headline)),
            Err(e) => {
                tracing::warn!("Failed to generate the review for session {session_id}: {e:#}");
                None
            }
        }
    }

    /// Replay the user's most recent stored review, with its charts recomputed.
    ///
    /// Returns `None` when the user has no reviewed session yet. The stored body is replayed
    /// verbatim: this is the read side of the snapshot promise, and it must not recompute
    /// anything the review already settled.
    pub(super) async fn latest_stored_review(&self, user: &User) -> anyhow::Result<Option<SessionReviewView>> {
        let db = self.db.lock().await;
        let Some(stored) = db.latest_session_review(user.id)? else {
            return Ok(None);
        };
        let snapshot: ReviewSnapshot =
            serde_json::from_str(&stored.body).with_context(|| format!("Stored review for session {} is unreadable", stored.session_id))?;
        let goals = db.goal_progress_report(user.id, None, None)?;
        let series = build_series(&db, user, &goals)?;
        Ok(Some(snapshot.into_view(series)))
    }

    /// The programme tier's single commentary call, grounded in the facts already assembled.
    async fn review_commentary(&self, user: &User, snapshot: &ReviewSnapshot) -> anyhow::Result<String> {
        let query = ScienceQuery {
            goal_kinds: Vec::new(),
            injuries: Vec::new(),
            focus: snapshot.position.iter().map(|p| p.focus.clone()).collect(),
            guidance: String::new(),
            pinned_docs: Vec::new(),
        };
        let science = self.science.search(&query, SCIENCE_CHUNK_K);
        let prompt = crate::assistant::prompts::build_review_commentary_prompt(&snapshot.facts_block(), &science);

        // No conversation history: the review comments on the session's numbers, not on the
        // chat around it, and past turns would only offer the model figures to confound.
        let raw = self
            .call_llm_with(&prompt, &[], "Comment on this session.", COMMENTARY_MAX_TOKENS, COMMENTARY_TEMPERATURE)
            .await?;
        let parsed = parse_assistant_response(&raw);
        let text = strip_markdown(&parsed.message).trim().to_string();
        anyhow::ensure!(!text.is_empty(), "the commentary call returned nothing");
        let _ = user;
        Ok(text)
    }
}

/// Everything a review reads, gathered under one lock before anything is formatted.
struct ReviewFacts {
    session: Session,
    roster: Option<SessionRoster>,
    comparison: Option<RosterVsActual>,
    performed: Vec<UnrosteredExercise>,
    records: Vec<SessionPersonalRecord>,
    streak: i32,
    week: crate::db::WeekSummary,
    goals: Vec<GoalProgress>,
    achieved_goals: Vec<String>,
    position: Option<StoredPosition>,
}

impl ReviewFacts {
    fn gather(db: &Database, user: &User, session_id: i64) -> anyhow::Result<Self> {
        let session = db.get_session(session_id)?.with_context(|| format!("session {session_id} not found"))?;
        let roster = db.roster_for_session(session_id)?;

        // A roster bound to this session is what makes a comparison possible at all; the
        // programme slot behind it is what makes this a programme-mode session.
        let comparison = roster.as_ref().map(|r| db.roster_vs_actual(r.id)).transpose()?;
        let performed = match comparison {
            Some(_) => Vec::new(),
            None => db.session_performed(session_id)?,
        };
        let position = roster.as_ref().and_then(|r| r.programme_slot_id).map(|slot_id| position_for_slot(db, slot_id)).transpose()?.flatten();

        let goals = db.goal_progress_report(user.id, None, None)?;
        let achieved_goals = mark_and_collect_achieved(db, &goals)?;

        Ok(Self {
            session,
            roster,
            comparison,
            performed,
            records: db.session_personal_records(session_id)?,
            streak: db.workout_streak(user.id)?,
            week: db.week_summary(user.id)?,
            goals,
            achieved_goals,
            position,
        })
    }

    /// Turn the gathered facts into the record that gets stored. Purely deterministic — this
    /// is the half that runs before any model is consulted, in both tiers.
    fn into_snapshot(self) -> ReviewSnapshot {
        let exercises = match &self.comparison {
            Some(c) => exercises_from_comparison(c),
            None => self.performed.iter().map(exercise_from_performed).collect(),
        };
        let adherence = self.comparison.as_ref().map(adherence_line);
        let effort = self.session.overall_effort.map(|e| StoredEffort {
            label: e.as_str().to_string(),
            confirmed: self.session.effort_source.is_some_and(EffortSource::is_confirmed),
        });

        ReviewSnapshot {
            headline: headline(&exercises, &self.records, &self.achieved_goals, self.comparison.as_ref()),
            programme_mode: self.position.is_some(),
            session_date: self.session.ended_at.clone().unwrap_or_else(|| self.session.started_at.clone()),
            intent: self.session.notes.clone().filter(|s| !s.trim().is_empty()),
            effort,
            exercises,
            records: self.records.iter().map(stored_record).collect(),
            // Filled in by the programme tier only, after this assembly is complete.
            commentary: None,
            goals: self.goals.iter().filter(|g| g.status != GoalStatus::Failed).map(goal_line).collect(),
            achieved_goals: self.achieved_goals,
            position: self.position,
            adherence,
            streak_days: u32::try_from(self.streak).ok(),
            week_line: Some(week_line(&self.week)),
            notes: Vec::new(),
        }
    }
}

/// Persist every goal this session completed, and name them.
///
/// The first production caller of `mark_goal_achieved`: until now nothing ever flipped a
/// goal to achieved, and `goal_progress_report` re-derived it from the percentage on every
/// read. Doing it here means the *date* a goal was hit is recorded at the moment it happens,
/// which is the only time it can be known.
fn mark_and_collect_achieved(db: &Database, goals: &[GoalProgress]) -> anyhow::Result<Vec<String>> {
    goals
        .iter()
        .filter(|g| g.status == GoalStatus::Achieved)
        .map(|g| {
            db.mark_goal_achieved(g.goal.id)?;
            Ok(goal_label(g))
        })
        .collect()
}

/// The programme position a slot sits at, or `None` when its programme has gone missing.
fn position_for_slot(db: &Database, slot_id: i64) -> anyhow::Result<Option<StoredPosition>> {
    let Some(slot) = db.get_programme_slot(slot_id)? else {
        return Ok(None);
    };
    let Some(programme) = db.get_programme(slot.programme_id)? else {
        return Ok(None);
    };
    Ok(Some(StoredPosition {
        programme_title: programme.title,
        week: u32::try_from(slot.week_idx).unwrap_or(1),
        day: u32::try_from(slot.day_idx).unwrap_or(1),
        focus: slot.focus,
    }))
}

/// Per-exercise lines from a roster comparison: what was prescribed and met, what was
/// skipped, and what the user added on top.
fn exercises_from_comparison(c: &RosterVsActual) -> Vec<StoredExercise> {
    let matched = c.matched.iter().map(|d| StoredExercise {
        name: d.exercise_name.clone(),
        prescribed: Some(prescription_line(d)),
        actual: rollup_line(&d.performed, d.measurement_type),
        delta: delta_line(d),
    });
    let skipped = c.skipped.iter().map(|s| StoredExercise {
        name: s.exercise_name.clone(),
        prescribed: Some(target_line(
            s.prescribed.target_sets,
            s.prescribed.target_reps,
            s.prescribed.target_weight_kg,
            s.prescribed.target_secs,
        )),
        actual: "not performed".to_string(),
        delta: Some("skipped".to_string()),
    });
    let extra = c.unrostered.iter().map(exercise_from_performed);
    matched.chain(skipped).chain(extra).collect()
}

fn exercise_from_performed(u: &UnrosteredExercise) -> StoredExercise {
    StoredExercise {
        name: u.exercise_name.clone(),
        prescribed: None,
        actual: rollup_line(&u.performed, u.measurement_type),
        delta: None,
    }
}

/// The prescription for a matched exercise, in the same words the roster stated it.
fn prescription_line(d: &ExerciseDelta) -> String {
    target_line(d.prescribed.target_sets, d.prescribed.target_reps, d.prescribed.target_weight_kg, d.prescribed.target_secs)
}

/// Render a set of targets, e.g. "3 sets × 6 reps @ 65kg" or "3 sets × 60s".
fn target_line(sets: Option<i32>, reps: Option<i32>, weight_kg: Option<f64>, secs: Option<i32>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(sets) = sets {
        parts.push(format!("{sets} sets"));
    }
    if let Some(secs) = secs {
        parts.push(format!("{secs}s"));
    } else if let Some(reps) = reps {
        parts.push(format!("{reps} reps"));
    }
    let joined = parts.join(" × ");
    match weight_kg {
        Some(w) => format!("{joined} @ {}kg", trim_decimal(w)),
        None => joined,
    }
}

/// What was actually performed, per measurement type.
fn rollup_line(p: &PerformedRollup, measurement: MeasurementType) -> String {
    let sets = format!("{} set{}", p.performed_sets, if p.performed_sets == 1 { "" } else { "s" });
    match measurement {
        MeasurementType::WeightReps => {
            let reps = p.avg_reps.map(|r| format!(" × {} reps", trim_decimal(r))).unwrap_or_default();
            let weight = p.avg_weight_kg.map(|w| format!(" @ {}kg", trim_decimal(w))).unwrap_or_default();
            format!("{sets}{reps}{weight}")
        }
        MeasurementType::TimeBased => {
            let secs = p.avg_secs.map(|s| format!(" × {}s", trim_decimal(s))).unwrap_or_default();
            format!("{sets}{secs}")
        }
        _ => sets,
    }
}

/// How the performance differed from the prescription, or `None` when it matched.
///
/// Deltas arrive from the DAO already signed as `performed − prescribed`, so a positive
/// number always means "more than asked" whatever the measurement.
fn delta_line(d: &ExerciseDelta) -> Option<String> {
    let parts: Vec<String> = [
        d.sets_delta.filter(|v| *v != 0).map(|v| format!("{}{v} set{}", sign(v as f64), if v.abs() == 1 { "" } else { "s" })),
        d.reps_delta.filter(non_zero).map(|v| format!("{}{} reps", sign(v), trim_decimal(v.abs()))),
        d.weight_delta_kg.filter(non_zero).map(|v| format!("{}{}kg", sign(v), trim_decimal(v.abs()))),
        d.secs_delta.filter(non_zero).map(|v| format!("{}{}s", sign(v), trim_decimal(v.abs()))),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn non_zero(v: &f64) -> bool {
    v.abs() > f64::EPSILON
}

fn sign(v: f64) -> &'static str {
    if v > 0.0 { "+" } else { "-" }
}

fn stored_record(r: &SessionPersonalRecord) -> StoredRecord {
    StoredRecord {
        exercise: r.exercise_name.clone(),
        detail: record_detail(r.measurement_type, r.value, r.count),
        previous: r.previous_value.map(|v| record_detail(r.measurement_type, v, r.previous_count)),
    }
}

fn record_detail(measurement: MeasurementType, value: f64, count: Option<i32>) -> String {
    match measurement {
        MeasurementType::WeightReps => match count {
            Some(reps) => format!("{}kg × {reps}", trim_decimal(value)),
            None => format!("{}kg", trim_decimal(value)),
        },
        MeasurementType::TimeBased => format!("{}s", trim_decimal(value)),
        MeasurementType::DistanceBased => format!("{}m", trim_decimal(value)),
        _ => trim_decimal(value),
    }
}

/// How the session tracked against its prescription.
fn adherence_line(c: &RosterVsActual) -> String {
    let prescribed = c.matched.len() + c.skipped.len();
    let done = c.matched.len();
    let extra = match c.unrostered.len() {
        0 => String::new(),
        n => format!(", plus {n} not on the roster"),
    };
    format!("{done} of {prescribed} prescribed exercises completed{extra}")
}

fn week_line(w: &crate::db::WeekSummary) -> String {
    let sessions = format!("{} session{}", w.session_count, if w.session_count == 1 { "" } else { "s" });
    match w.total_volume > 0.0 {
        true => format!("{sessions}, {} kg total volume", trim_decimal(w.total_volume)),
        false => sessions,
    }
}

fn goal_label(g: &GoalProgress) -> String {
    format!("{} to {}", g.exercise_name, trim_decimal(g.goal.target_value))
}

fn goal_line(g: &GoalProgress) -> String {
    match g.current_value {
        Some(current) => format!("{}: {} ({}%)", goal_label(g), trim_decimal(current), g.percentage.round() as i64),
        None => format!("{}: nothing logged yet", goal_label(g)),
    }
}

/// The review's one-line verdict — always deterministic, in both tiers.
///
/// Leads with a completed goal when there is one, then a record, then the session's own
/// adherence. The adjective is a function of how much of the prescription was met, so a
/// session that fell short cannot be described as a strong one: that judgement is made here,
/// in code, precisely so it is not left to a model inclined to be encouraging.
fn headline(
    exercises: &[StoredExercise],
    records: &[SessionPersonalRecord],
    achieved_goals: &[String],
    comparison: Option<&RosterVsActual>,
) -> String {
    if let Some(goal) = achieved_goals.first() {
        return format!("Goal reached: {goal}");
    }

    let count = exercises.len();
    let performed = format!("{count} exercise{}", if count == 1 { "" } else { "s" });

    let Some(c) = comparison else {
        return match records.first() {
            Some(pr) => format!("Session logged — {performed}, and a new best on {}", pr.exercise_name),
            None => format!("Session logged — {performed}"),
        };
    };

    let prescribed = c.matched.len() + c.skipped.len();
    let verdict = match prescribed {
        0 => "Session logged",
        _ if c.skipped.is_empty() => "Everything prescribed done",
        _ if c.matched.len() * 4 >= prescribed * 3 => "Solid session",
        _ if c.matched.len() * 2 >= prescribed => "Partial session",
        _ => "Short session",
    };
    match records.first() {
        Some(pr) => format!("{verdict} — {}, and a new best on {}", adherence_line(c), pr.exercise_name),
        None => format!("{verdict} — {}", adherence_line(c)),
    }
}

/// The charts behind the review: one goal trajectory per live goal.
///
/// Recomputed on every render rather than stored, and built on the [C6.2] [`SeriesView`]
/// contract rather than a second series path of the review's own.
fn build_series(db: &Database, user: &User, goals: &[GoalProgress]) -> anyhow::Result<Vec<SeriesView>> {
    let series_for = |g: &GoalProgress| -> anyhow::Result<Option<SeriesView>> {
        let Some(exercise_type_id) = g.goal.exercise_type_id else {
            return Ok(None);
        };
        let points = db.exercise_time_series(user.id, exercise_type_id, g.goal.direction, None, None, true)?;
        // One reading is a dot, not a trend, and a chart of it says nothing a line of text
        // does not already say better.
        if points.len() < 2 {
            return Ok(None);
        }
        Ok(Some(SeriesView {
            title: format!("{} — top set", g.exercise_name),
            unit: "kg".to_string(),
            better: match g.goal.direction {
                GoalDirection::Increase => Direction::Higher,
                GoalDirection::Decrease => Direction::Lower,
            },
            shape: SeriesShape::Trajectory { target: g.goal.target_value },
            points: points.into_iter().map(|p| SeriesPointView { label: p.date, value: p.value }).collect(),
        }))
    };

    Ok(goals.iter().filter(|g| g.status == GoalStatus::Active).filter_map(|g| series_for(g).transpose()).collect::<Result<Vec<_>, _>>()?)
}

/// Trim a float to the shortest honest decimal form: `92.5` stays, `80.0` becomes `80`.
fn trim_decimal(v: f64) -> String {
    let rounded = (v * 100.0).round() / 100.0;
    match rounded.fract() == 0.0 {
        true => format!("{}", rounded as i64),
        false => format!("{rounded}"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::db::{
        LifecycleStatus, Programme, ProgrammeSlot, RosterExercise, SlotStatus, new_exercise_entry_at, new_exercise_goal, new_exercise_set,
    };
    use crate::db::SetEdit;
    use gymbuddy_proto::{ReviewKindView, View};

    /// A canned commentary reply in the envelope every prompt asks for.
    const COMMENTARY: &str = r#"{"message": "You held the extra load for every set. Keep it there next week.", "actions": []}"#;

    /// A registered user with one bench-press goal on file.
    async fn ready_user(handler: &AssistantHandler) -> User {
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        let db = handler.db.lock().await;
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let mut goal = new_exercise_goal(user.id, bench.id, 200.0);
        goal.start_date = "2026-01-01".into();
        db.insert_goal(&goal).unwrap();
        user
    }

    /// An ended session with one bench-press entry of `sets` sets at `weight`.
    async fn logged_session(handler: &AssistantHandler, user: &User, weight: f64, sets: usize) -> i64 {
        let db = handler.db.lock().await;
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let session_id = db.start_session_at(user.id, "2026-07-01 09:00:00", None, Some("upper push")).unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry_at(user.id, Some(session_id), None, "2026-07-01 09:05:00")).unwrap();
        (0..sets).for_each(|i| {
            let mut set = new_exercise_set(entry_id, bench.id, MeasurementType::WeightReps, weight);
            set.count = Some(6);
            set.order_idx = i as i32;
            set.logged_at = "2026-07-01 09:10:00".to_string();
            db.insert_set(&set).unwrap();
        });
        db.end_session(session_id).unwrap();
        session_id
    }

    /// Bind a roster to a session, optionally filling a programme slot — which is what
    /// makes the session programme-mode.
    async fn bind_roster(handler: &AssistantHandler, user: &User, session_id: i64, target_weight: f64, programme: bool) {
        let db = handler.db.lock().await;
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let roster_id = db.create_roster(user.id, "Upper push", None, None).unwrap();
        db.add_roster_exercise(&RosterExercise {
            id: 0,
            roster_id,
            exercise_type_id: bench.id,
            order_idx: 0,
            target_sets: Some(3),
            target_reps: Some(6),
            target_weight_kg: Some(target_weight),
            target_secs: None,
            notes: None,
        })
        .unwrap();

        if programme {
            let programme_id = db
                .create_programme(&Programme {
                    id: 0,
                    user_id: user.id,
                    title: "12-week hypertrophy".into(),
                    start_date: "2026-06-01".into(),
                    target_end_date: None,
                    days_per_week: 3,
                    split: "upper/lower".into(),
                    progression_policy: "double progression".into(),
                    status: LifecycleStatus::Active,
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .unwrap();
            let slot_id = db
                .add_programme_slot(&ProgrammeSlot {
                    id: 0,
                    programme_id,
                    week_idx: 2,
                    day_idx: 1,
                    focus: "upper".into(),
                    status: SlotStatus::Pending,
                    updated_at: String::new(),
                })
                .unwrap();
            db.bind_roster_to_slot(roster_id, slot_id).unwrap();
        }
        db.bind_roster_to_session(roster_id, session_id).unwrap();
    }

    // ── The two-tier split ────────────────────────────────────────────────────

    /// The safety property [C6.5] is built around: an ad-hoc session's review is assembled
    /// entirely from the logged sets, and **no model is consulted at all**. Asserted on the
    /// mock's recorded requests rather than on the output, because "the commentary field is
    /// empty" would also be true of a call that was made and then discarded.
    #[tokio::test]
    async fn an_adhoc_session_review_makes_no_llm_call() {
        let (handler, llm) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 80.0, 3).await;
        let before = llm.recorded_requests().len();

        let review = handler.generate_session_review(&user, session_id).await.unwrap();

        assert_eq!(llm.recorded_requests().len(), before, "the ad-hoc tier must not reach the model");
        assert_eq!(review.kind, ReviewKindView::Summary);
        assert!(review.commentary.is_none());
        assert!(!review.headline.is_empty(), "and it still produces a verdict");
    }

    /// The programme tier makes exactly one call, and the prompt it sends carries the
    /// already-computed numbers — the grounding that stops the model inventing its own.
    #[tokio::test]
    async fn a_programme_session_grounds_one_commentary_call_in_the_stats() {
        let (handler, llm) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 82.5, 3).await;
        bind_roster(&handler, &user, session_id, 80.0, true).await;
        let before = llm.recorded_requests().len();

        let review = handler.generate_session_review(&user, session_id).await.unwrap();

        let calls = llm.recorded_requests();
        assert_eq!(calls.len() - before, 1, "exactly one commentary call");
        assert_eq!(review.kind, ReviewKindView::Report);
        assert_eq!(review.commentary.as_deref(), Some("You held the extra load for every set. Keep it there next week."));

        let prompt = &calls[before].messages[0].content;
        assert!(prompt.contains("SESSION FACTS"), "the call is grounded in the assembled facts: {prompt}");
        assert!(prompt.contains("Bench Press"), "{prompt}");
        assert!(prompt.contains("+2.5kg"), "the computed delta reaches the prompt: {prompt}");
        assert!(prompt.contains("DO NOT DO ARITHMETIC"), "{prompt}");
        assert!(prompt.contains("NEVER congratulate a session that missed its targets"), "{prompt}");
    }

    /// A commentary call that fails costs the commentary, never the record.
    #[tokio::test]
    async fn a_failed_commentary_still_produces_the_numbers() {
        let (handler, _llm) = setup_handler("not json at all, and no message field").await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 82.5, 3).await;
        bind_roster(&handler, &user, session_id, 80.0, true).await;

        let review = handler.generate_session_review(&user, session_id).await.unwrap();
        assert!(!review.exercises.is_empty(), "the deterministic half survives");
        assert!(!review.headline.is_empty());
    }

    // ── Deterministic content ─────────────────────────────────────────────────

    /// The delta against the prescription is the review's core sentence.
    #[tokio::test]
    async fn the_review_reports_the_delta_against_the_prescription() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 82.5, 3).await;
        bind_roster(&handler, &user, session_id, 80.0, false).await;

        let review = handler.generate_session_review(&user, session_id).await.unwrap();
        let bench = review.exercises.iter().find(|e| e.name == "Bench Press").expect("bench press line");
        assert_eq!(bench.prescribed.as_deref(), Some("3 sets × 6 reps @ 80kg"));
        assert_eq!(bench.actual, "3 sets × 6 reps @ 82.5kg");
        assert_eq!(bench.delta.as_deref(), Some("+2.5kg"));
        assert_eq!(review.adherence.as_deref(), Some("1 of 1 prescribed exercises completed"));
    }

    /// A session that fell short is described as one. The adjective is computed here, in
    /// code, precisely so a model inclined to encourage cannot upgrade it.
    #[tokio::test]
    async fn a_session_that_skipped_prescribed_work_is_not_called_solid() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 80.0, 3).await;
        {
            // A roster prescribing two exercises, of which only bench was performed.
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let squat = db.get_exercise_type_by_name("Squat").unwrap().unwrap();
            let roster_id = db.create_roster(user.id, "Upper", None, None).unwrap();
            [(bench.id, 0), (squat.id, 1)].iter().for_each(|(id, idx)| {
                db.add_roster_exercise(&RosterExercise {
                    id: 0,
                    roster_id,
                    exercise_type_id: *id,
                    order_idx: *idx,
                    target_sets: Some(3),
                    target_reps: Some(6),
                    target_weight_kg: Some(80.0),
                    target_secs: None,
                    notes: None,
                })
                .unwrap();
            });
            db.bind_roster_to_session(roster_id, session_id).unwrap();
        }

        let review = handler.generate_session_review(&user, session_id).await.unwrap();
        assert!(review.headline.starts_with("Partial session"), "half the prescription is not a solid session: {}", review.headline);
        assert!(review.exercises.iter().any(|e| e.delta.as_deref() == Some("skipped")), "the skipped work is named");
    }

    /// A PR set this session is reported with the mark it beat.
    #[tokio::test]
    async fn the_review_reports_records_set_this_session() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let _older = logged_session(&handler, &user, 80.0, 3).await;

        // A second, later session that beats it.
        let session_id = {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let id = db.start_session_at(user.id, "2026-07-08 09:00:00", None, None).unwrap();
            let entry = db.insert_entry(&new_exercise_entry_at(user.id, Some(id), None, "2026-07-08 09:05:00")).unwrap();
            let mut set = new_exercise_set(entry, bench.id, MeasurementType::WeightReps, 85.0);
            set.count = Some(5);
            set.logged_at = "2026-07-08 09:10:00".to_string();
            db.insert_set(&set).unwrap();
            db.end_session(id).unwrap();
            id
        };

        let review = handler.generate_session_review(&user, session_id).await.unwrap();
        let pr = review.records.first().expect("a record");
        assert_eq!(pr.exercise, "Bench Press");
        assert_eq!(pr.detail, "85kg × 5");
        assert_eq!(pr.previous.as_deref(), Some("80kg × 6"));
    }

    // ── Persistence and snapshot semantics ────────────────────────────────────

    /// The review is a record of what was true when the session ended. Editing a set
    /// afterwards must not rewrite it.
    #[tokio::test]
    async fn a_later_set_edit_does_not_rewrite_the_stored_review() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 80.0, 3).await;

        let original = handler.generate_session_review(&user, session_id).await.unwrap();
        assert!(original.exercises[0].actual.contains("80kg"));

        // Rewrite history through the real edit path: the set now says something else.
        {
            let db = handler.db.lock().await;
            let entry = db.list_entries_for_session(session_id).unwrap().remove(0);
            let set_id = db.list_sets_for_entry(entry.id).unwrap()[0].id;
            let edit = SetEdit { value: Some(200.0), ..Default::default() };
            db.edit_set(set_id, user.id, &[], &edit).unwrap();
        }

        let replayed = handler.latest_stored_review(&user).await.unwrap().expect("a stored review");
        assert_eq!(replayed.exercises[0].actual, original.exercises[0].actual, "the snapshot stands");
        assert!(!replayed.exercises[0].actual.contains("200kg"), "the review must not have followed the edit");
    }

    /// Regenerating replaces the stored review rather than adding a second one.
    #[tokio::test]
    async fn regenerating_replaces_the_stored_review() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 80.0, 3).await;

        handler.generate_session_review(&user, session_id).await.unwrap();
        let first = { handler.db.lock().await.get_session_review(session_id).unwrap().unwrap() };

        handler.generate_session_review(&user, session_id).await.unwrap();
        let second = { handler.db.lock().await.get_session_review(session_id).unwrap().unwrap() };

        assert_eq!(first.session_id, second.session_id, "one review, regenerated in place");
        assert_eq!(first.body, second.body, "and the same session yields the same record");
    }

    /// Goal achievement is persisted at generation time — the first production caller of
    /// `mark_goal_achieved`, and the only moment the date a goal was hit is knowable.
    #[tokio::test]
    async fn reaching_a_goal_is_persisted_and_leads_the_review() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };

        let goal_id = {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            // A goal the session below clears outright.
            let mut goal = new_exercise_goal(user.id, bench.id, 60.0);
            goal.start_date = "2026-01-01".into();
            db.insert_goal(&goal).unwrap()
        };
        let session_id = logged_session(&handler, &user, 80.0, 3).await;

        let review = handler.generate_session_review(&user, session_id).await.unwrap();
        assert!(!review.achieved_goals.is_empty(), "the completed goal is named");
        assert!(review.headline.starts_with("Goal reached:"), "and it leads: {}", review.headline);

        let db = handler.db.lock().await;
        let goal = db.get_goal(goal_id).unwrap().unwrap();
        assert!(goal.achieved, "the goal is marked achieved in the database");
    }

    // ── The auto-close path ───────────────────────────────────────────────────

    /// The gap the spec called out: a session the user walked away from used to be closed
    /// in silence. Now the triggering turn says so, quotes the review's headline, and offers
    /// the derived effort back for correction.
    #[tokio::test]
    async fn auto_closing_a_stale_session_reviews_it_and_says_so() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;

        // An active session whose last activity is long past the 4-hour test threshold.
        let session_id = {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let id = db.start_session_at(user.id, "2026-01-01 09:00:00", None, Some("upper push")).unwrap();
            let entry = db.insert_entry(&new_exercise_entry_at(user.id, Some(id), None, "2026-01-01 09:05:00")).unwrap();
            let mut set = new_exercise_set(entry, bench.id, MeasurementType::WeightReps, 80.0);
            set.count = Some(6);
            set.perceived_difficulty = crate::db::Difficulty::Hard;
            set.logged_at = "2026-01-01 09:10:00".to_string();
            db.insert_set(&set).unwrap();
            id
        };

        let reply = handler.handle_text_message(&make_message(12345, "morning"), "morning").await.unwrap();

        let text = shown(&reply);
        assert!(text.contains("I closed your last session."), "{text}");
        assert!(text.contains("/review for the full report"), "{text}");
        assert!(text.contains("tell me if hard sounds wrong"), "the derived effort is offered back: {text}");

        let db = handler.db.lock().await;
        assert!(db.get_session(session_id).unwrap().unwrap().ended_at.is_some(), "the session is closed");
        assert!(db.get_session_review(session_id).unwrap().is_some(), "and reviewed");
        assert_eq!(
            db.get_session(session_id).unwrap().unwrap().effort_source,
            Some(EffortSource::Derived),
            "nobody was asked, so the verdict is a reading",
        );
    }

    /// The correction the note invites: stating the effort confirms it and rewrites the
    /// review that quoted the guess.
    #[tokio::test]
    async fn correcting_the_effort_regenerates_the_review() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 80.0, 3).await;
        handler.generate_session_review(&user, session_id).await.unwrap();

        let outcome = crate::assistant::actions::AssistantAction::RecordSessionOutcome {
            overall_effort: Some(crate::db::Difficulty::Easy),
            felt: None,
            cut_short: None,
            cut_short_reason: None,
        };
        handler.execute_action(&outcome, &user).await.unwrap();

        let review = handler.latest_stored_review(&user).await.unwrap().expect("a review");
        let effort = review.effort.expect("an effort");
        assert_eq!(effort.label, "easy");
        assert!(effort.confirmed, "the regenerated review carries the user's own verdict");
    }

    // ── The /review command ───────────────────────────────────────────────────

    #[tokio::test]
    async fn slash_review_with_no_finished_session_says_so() {
        let (handler, _) = setup_handler(COMMENTARY).await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/review").await.unwrap();
        assert!(shown(&reply).contains("finished session to review"), "{}", shown(&reply));
    }

    /// `/review` replays the stored snapshot without consulting the model again — the
    /// review was written once, and re-reading it is not a second opinion.
    #[tokio::test]
    async fn slash_review_replays_the_stored_review_without_calling_the_model() {
        let (handler, llm) = setup_handler(COMMENTARY).await;
        let user = ready_user(&handler).await;
        let session_id = logged_session(&handler, &user, 82.5, 3).await;
        bind_roster(&handler, &user, session_id, 80.0, true).await;
        handler.generate_session_review(&user, session_id).await.unwrap();
        let after_generate = llm.recorded_requests().len();

        let reply = handler.handle_text_message(&make_message(12345, "/review"), "/review").await.unwrap();

        assert_eq!(llm.recorded_requests().len(), after_generate, "replaying a review consults nobody");
        assert!(matches!(reply.view, View::SessionReview(_)), "the command returns the review view");
        assert!(shown(&reply).contains("Bench Press"), "{}", shown(&reply));
    }
}

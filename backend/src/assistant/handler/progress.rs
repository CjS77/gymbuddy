//! The `/progress` command: the user-facing answer to "how far am I from my
//! goals" ([C6.3]).
//!
//! Every goal on file becomes one [`SeriesView`] — the [C6.2] contract, so the
//! clients that already plot charts need no new code — carrying the readings, the
//! target, and the [`Direction`] that says which way "better" runs. A `SeriesView`
//! has no field for the rest of the question ("does this trend get me there, and
//! is there time?"), so that lands as one [`GoalOutlookView`] per goal plus the
//! sentence it produces, and the headline counts the bands they fall in.
//!
//! The projection is deliberately not a number ([C6.4]). It is a straight line
//! through sparse, noisy, self-reported readings, so what travels is one of four
//! bands — on track, at risk, off track, too early to say — and the refusal to
//! project lives in [`GoalOutlookView::assess`], not in a client's wording. This
//! module's job is only to gather the evidence the assessment is entitled to: the
//! readings, the deadline, and whether the user is actually turning up.
//!
//! Direction-awareness runs the whole way through: the daily readings are pulled
//! with the goal's direction (see [`crate::db::Database::exercise_time_series`]),
//! the series is stamped with it so `improving()` reads the movement correctly,
//! and the projection compares against the target the same way. Get it wrong and a
//! cut's falling bodyweight renders as failure.
//!
//! Body metrics appear here under both bars the body-metrics policy sets (see
//! [`crate::db::body_metrics`]): the user asked, and an active goal of theirs is
//! denominated in the metric.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::assistant::prompts::goals_by_priority;
use crate::db::{Database, GoalDirection, GoalProgress, GoalStatus, MeasurementType, User};

use super::AssistantHandler;
use gymbuddy_proto::{
    Adherence, Direction, GoalOutlook, GoalOutlookView, MIN_PROJECTION_DAYS, ProgressView, SeriesPointView, SeriesShape, SeriesView, View,
};

/// The window the muscle-group volume breakdown covers.
const VOLUME_WINDOW: &str = "-7 days";

/// One goal, charted: the series a client plots, its outlook, and the sentence that
/// outlook produces.
struct GoalChart {
    series: SeriesView,
    note: String,
    outlook: GoalOutlookView,
}

impl AssistantHandler {
    /// Report progress against every goal on file as a [`View::Progress`].
    ///
    /// `/progress` logs nothing. Reading adherence runs [C4.4]'s missed-slot sweep,
    /// which only settles weeks the calendar has already closed — the same
    /// normalisation `/programme status` performs, never a new record of training.
    pub(super) async fn cmd_progress(&self, user: &User) -> anyhow::Result<View> {
        let today = chrono::Utc::now().date_naive();
        let (charts, volume) = {
            let db = self.db.lock().await;
            let goals = db.goal_progress_report(user.id, None, None)?;
            let adherence = adherence(&db, user.id, today)?;
            let charts = goals_by_priority(&goals)
                .into_iter()
                .map(|gp| self.goal_chart(&db, user.id, gp, today, adherence))
                .collect::<anyhow::Result<Vec<_>>>()?;
            (charts, volume_breakdown(&db, user.id)?)
        };

        let headline = headline(&charts.iter().map(|chart| chart.outlook.outlook).collect::<Vec<_>>());
        let notes = charts.iter().map(|chart| chart.note.clone()).collect();
        let (goal_series, goals): (Vec<SeriesView>, Vec<GoalOutlookView>) =
            charts.into_iter().map(|chart| (chart.series, chart.outlook)).unzip();
        let series = goal_series.into_iter().chain(volume).collect();

        Ok(View::Progress(ProgressView { headline, series, notes, goals }))
    }

    /// Chart one goal and read its outlook off the readings.
    fn goal_chart(
        &self,
        db: &Database,
        user_id: i64,
        gp: &GoalProgress,
        today: NaiveDate,
        adherence: Adherence,
    ) -> anyhow::Result<GoalChart> {
        let series = self.goal_series(db, user_id, gp, today)?;
        let outlook = goal_outlook(gp, &series, today, adherence);
        Ok(GoalChart { note: goal_note(gp, &series, &outlook, today), outlook, series })
    }

    /// The readings behind one goal, as a [`SeriesShape::Trajectory`] aimed at its
    /// target. Exercise goals read their daily bests from the log (rolled up over the
    /// exercise's subtree, matching how the goal itself is judged); metric goals read
    /// the user's measurement series.
    ///
    /// Shared with the programme report ([C4.6]), which charts the goals a programme
    /// serves: one goal has one trajectory, however it is being asked for.
    pub(super) fn goal_series(&self, db: &Database, user_id: i64, gp: &GoalProgress, today: NaiveDate) -> anyhow::Result<SeriesView> {
        let from = date_part(&gp.goal.start_date);
        let to = today.format("%Y-%m-%d").to_string();

        let (title, unit, points) = match (gp.goal.exercise_type_id, gp.goal.metric.as_deref()) {
            (Some(et_id), _) => {
                let readings = db.exercise_time_series(user_id, et_id, gp.goal.direction, Some(&from), Some(&to), true)?;
                let points = readings.iter().map(|p| SeriesPointView { label: p.date.clone(), value: p.value }).collect();
                (format!("{} — daily best", gp.exercise_name), self.exercise_unit(et_id), points)
            }
            (None, Some(metric)) => {
                let readings = db.list_body_metrics(user_id, metric, &from, &to)?;
                let points =
                    readings.iter().map(|m| SeriesPointView { label: date_part(&m.measured_at), value: m.value }).collect();
                (metric_label(metric), metric_unit(metric).to_string(), points)
            }
            (None, None) => (gp.exercise_name.clone(), String::new(), Vec::new()),
        };

        let shape = SeriesShape::Trajectory { target: gp.goal.target_value };
        Ok(SeriesView { title, unit, better: direction(gp.goal.direction), shape, points })
    }

    /// The unit an exercise's values are recorded in, via its measurement type.
    fn exercise_unit(&self, exercise_type_id: i64) -> String {
        self.catalogue
            .iter()
            .find(|e| e.exercise_type.id == exercise_type_id)
            .and_then(|e| e.exercise_type.measurement_type)
            .map(measurement_unit)
            .unwrap_or_default()
            .to_string()
    }
}

/// Recent training volume per muscle group, as the one [`SeriesShape::Breakdown`]
/// in the view: buckets compared side by side, with no better end to them. Empty
/// (so: absent) when nothing was logged in the window, rather than a chart of zeroes.
fn volume_breakdown(db: &Database, user_id: i64) -> anyhow::Result<Option<SeriesView>> {
    // The DAO groups by (ISO week, muscle group); a seven-day window can straddle
    // two weeks, so fold the weeks away — the bucket here is the group, not the week.
    let totals: BTreeMap<String, f64> =
        db.volume_by_muscle_group_weekly(user_id, VOLUME_WINDOW)?.into_iter().fold(BTreeMap::new(), |mut acc, row| {
            *acc.entry(row.muscle_group).or_default() += row.total_volume;
            acc
        });
    if totals.is_empty() {
        return Ok(None);
    }

    let mut points: Vec<SeriesPointView> =
        totals.into_iter().map(|(muscle_group, total)| SeriesPointView { label: muscle_group, value: total }).collect();
    points.sort_by(|a, b| b.value.total_cmp(&a.value));

    Ok(Some(SeriesView {
        title: "Training volume by muscle group (last 7 days)".to_string(),
        unit: "kg×reps".to_string(),
        // Volume is context, not a goal: more is not automatically better, and a
        // deload week is meant to be lower.
        better: Direction::Neutral,
        shape: SeriesShape::Breakdown,
        points,
    }))
}

/// The view's one-line answer to "how am I doing".
///
/// Counts what is landing, then names the two bands that count would otherwise
/// misrepresent. A goal at risk is not on track, and a goal with too little behind it
/// is neither — filing it under "not on track" is exactly the dishonesty [C6.4]
/// exists to prevent, so when *nothing* is projectable the line says so instead of
/// leading with a zero.
fn headline(outlooks: &[GoalOutlook]) -> String {
    let total = outlooks.len();
    let count = |band: GoalOutlook| outlooks.iter().filter(|o| **o == band).count();
    if total == 0 {
        return "No goals on file yet — tell me what you're working towards and I'll track it here.".to_string();
    }
    if count(GoalOutlook::TooEarly) == total {
        return format!("Too early to say on {} — keep logging and I'll trend {}.", goals_label(total), them(total));
    }

    let landing = outlooks.iter().filter(|o| matches!(o, GoalOutlook::OnTrack | GoalOutlook::Achieved)).count();
    let head = format!("{landing} of {} on track", goals_label(total));
    let caveats: Vec<String> = [(count(GoalOutlook::AtRisk), "at risk"), (count(GoalOutlook::TooEarly), "too early to say")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect();
    match caveats.is_empty() {
        true => format!("{head}."),
        false => format!("{head} — {}.", caveats.join(", ")),
    }
}

fn goals_label(total: usize) -> String {
    format!("{total} {}", if total == 1 { "goal" } else { "goals" })
}

fn them(total: usize) -> &'static str {
    if total == 1 { "it" } else { "them" }
}

/// One goal's outlook ([C6.4]).
///
/// Settled goals report what happened; a live one is assessed against its deadline
/// and the user's attendance. The band itself is decided in the wire model — this
/// only hands over the evidence it is entitled to.
fn goal_outlook(gp: &GoalProgress, series: &SeriesView, today: NaiveDate, adherence: Adherence) -> GoalOutlookView {
    let subject = subject(series);
    let target_date = gp.goal.target_date.clone();
    match gp.status {
        GoalStatus::Achieved => GoalOutlookView::achieved(subject, series, target_date),
        GoalStatus::Failed => GoalOutlookView::missed(subject, series, target_date, adherence),
        GoalStatus::Active => GoalOutlookView::assess(subject, series, projection(series, gp, today), target_date, adherence),
    }
}

/// The goal's subject, as the series names it: "Bench Press — daily best" is a chart
/// of "Bench Press".
fn subject(series: &SeriesView) -> String {
    series.title.split(" — ").next().unwrap_or(&series.title).to_string()
}

/// How the user is turning up, as the outlook's dominant term ([C4.4]). Only a live
/// programme has an attendance record to read, and its drift verdict is consumed as
/// it stands rather than recomputed here.
fn adherence(db: &Database, user_id: i64, today: NaiveDate) -> anyhow::Result<Adherence> {
    let Some(drift) = db.goal_adherence(user_id, &today.format("%Y-%m-%d").to_string())? else {
        return Ok(Adherence::Unprogrammed);
    };
    Ok(match drift.is_on_track() {
        true => Adherence::Keeping,
        false => Adherence::Drifting,
    })
}

/// Where the current rate lands by the goal's target date. `None` when the goal is
/// open-ended (nothing to project *to*) or the readings are too thin to give a rate.
fn projection(series: &SeriesView, gp: &GoalProgress, today: NaiveDate) -> Option<f64> {
    let days = days_remaining(gp, today)?;
    let latest = series.latest()?.value;
    Some(latest + daily_rate(series)? * days.max(0) as f64)
}

/// Change per day across the charted readings. `None` when they span less than
/// [`MIN_PROJECTION_DAYS`] — a handful of sets inside one week is a good week, not a
/// rate to carry months forward.
fn daily_rate(series: &SeriesView) -> Option<f64> {
    let change = series.change()?;
    let span = (parse_date(&series.points.last()?.label)? - parse_date(&series.points.first()?.label)?).num_days();
    (span >= MIN_PROJECTION_DAYS).then(|| change / span as f64)
}

/// Whole days from `today` to the goal's target date; `None` when it is open-ended.
/// Negative once the date has passed.
fn days_remaining(gp: &GoalProgress, today: NaiveDate) -> Option<i64> {
    Some((parse_date(gp.goal.target_date.as_deref()?)? - today).num_days())
}

/// The per-goal sentence: where the user stands, how long is left, and what the
/// outlook says — the half of [C6.3] a [`SeriesView`] has no field for, in the
/// vocabulary [C6.4] settled on.
fn goal_note(gp: &GoalProgress, series: &SeriesView, outlook: &GoalOutlookView, today: NaiveDate) -> String {
    let target = series.value_label(gp.goal.target_value);
    let standing = match gp.current_value {
        Some(current) => format!("{} of {target}", series.value_label(current)),
        None => format!("nothing logged yet, target {target}"),
    };
    format!("{}: {standing}{}", outlook.goal, outlook_clause(gp, series, outlook, today))
}

/// The outlook clause of a goal's note, leading with its own separator so a goal
/// with nothing to say about the future contributes nothing.
fn outlook_clause(gp: &GoalProgress, series: &SeriesView, outlook: &GoalOutlookView, today: NaiveDate) -> String {
    match outlook.outlook {
        GoalOutlook::Achieved => " — achieved.".to_string(),
        GoalOutlook::Missed => format!(" — {} passed without it.", gp.goal.target_date.as_deref().unwrap_or("the target date")),
        _ => match days_remaining(gp, today) {
            Some(days) => format!(", {} left — {}.", remaining_label(days), verdict(series, outlook)),
            None => format!(", open-ended — {}.", verdict(series, outlook)),
        },
    }
}

/// The judgement on a live goal, in words: the outlook's own vocabulary, and a number
/// only where one travelled with it.
fn verdict(series: &SeriesView, outlook: &GoalOutlookView) -> String {
    // Rounded for the sentence only: a straight-line extrapolation is not precise to
    // fourteen decimal places, and printing them claims it is.
    let landing = outlook.projected.map(|projected| series.value_label((projected * 10.0).round() / 10.0));
    match (outlook.outlook, landing) {
        (GoalOutlook::OnTrack, Some(label)) => format!("on track, heading for {label}"),
        (GoalOutlook::OnTrack, None) => "on track, moving the right way".to_string(),
        (GoalOutlook::TooEarly, _) => format!("too early to say: {}", evidence_label(outlook.readings)),
        (band, Some(label)) => format!("{}: this rate reaches only {label}", band.label()),
        (band, None) => match outlook.limiter {
            Some(limiter) => format!("{}: {}", band.label(), limiter.reason()),
            None => band.label().to_string(),
        },
    }
}

/// What the refusal to project rests on, said plainly rather than left implied.
fn evidence_label(readings: u32) -> String {
    match readings {
        0 => "nothing logged against it yet".to_string(),
        1 => "one reading so far".to_string(),
        n => format!("{n} readings so far, and no trend across them yet"),
    }
}

/// How much time is left, in the largest unit that stays readable.
fn remaining_label(days: i64) -> String {
    match days {
        d if d < 0 => "no time".to_string(),
        0 => "under a day".to_string(),
        1 => "1 day".to_string(),
        d if d < 14 => format!("{d} days"),
        d if d < 70 => format!("{} weeks", d / 7),
        d => format!("{} months", d / 30),
    }
}

/// Which way progress runs, on the wire.
fn direction(direction: GoalDirection) -> Direction {
    match direction {
        GoalDirection::Increase => Direction::Higher,
        GoalDirection::Decrease => Direction::Lower,
    }
}

/// The unit a measurement type's values are recorded in. Level-based work is a bare
/// number, so it carries none.
fn measurement_unit(measurement: MeasurementType) -> &'static str {
    match measurement {
        MeasurementType::WeightReps => "kg",
        MeasurementType::TimeBased => "s",
        MeasurementType::DistanceBased => "m",
        MeasurementType::LevelBased => "",
        MeasurementType::ScoreBased => "pts",
    }
}

/// The unit a body metric's readings are in, read off the unit suffix its canonical
/// name carries (see [`crate::db::canonical_body_metric`]).
fn metric_unit(metric: &str) -> &'static str {
    match metric.rsplit('_').next() {
        Some("kg") => "kg",
        Some("pct") => "%",
        Some("cm") => "cm",
        Some("bpm") => "bpm",
        _ => "",
    }
}

/// A body metric's display name: the canonical ones spelled out, anything else
/// de-snaked with its unit suffix dropped (it is already in the series' `unit`).
fn metric_label(metric: &str) -> String {
    match metric {
        "bodyweight_kg" => return "Bodyweight".to_string(),
        "body_fat_pct" => return "Body fat".to_string(),
        "waist_cm" => return "Waist".to_string(),
        "resting_hr_bpm" => return "Resting heart rate".to_string(),
        _ => {}
    }
    let stem = match metric_unit(metric).is_empty() {
        true => metric,
        false => metric.rsplit_once('_').map_or(metric, |(head, _)| head),
    };
    crate::assistant::prompts::capitalize(&stem.replace('_', " "))
}

/// The date half of a stored `YYYY-MM-DD HH:MM:SS` timestamp.
fn date_part(timestamp: &str) -> String {
    timestamp.get(..10).unwrap_or(timestamp).to_string()
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use crate::assistant::commands;
    use crate::db::{
        Database, GoalKind, MeasurementType, User, new_body_metric, new_exercise_entry, new_exercise_goal, new_exercise_set, new_goal,
        new_programme, new_programme_slot,
    };
    use gymbuddy_proto::{GoalLimiter, View};

    /// A registered user plus a handle on the database, the start of every test here.
    async fn setup() -> (super::AssistantHandler, User, crate::telegram::Message) {
        let (handler, _) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        (handler, user, msg)
    }

    /// The `ProgressView` behind a `/progress` reply.
    async fn progress_of(handler: &super::AssistantHandler, msg: &crate::telegram::Message) -> ProgressView {
        match handler.handle_text_message(msg, "/progress").await.unwrap().view {
            View::Progress(p) => p,
            other => panic!("/progress must answer with a Progress view, got {other:?}"),
        }
    }

    fn days_from_today(days: i64) -> String {
        (chrono::Utc::now().date_naive() + chrono::Duration::days(days)).format("%Y-%m-%d").to_string()
    }

    fn log_set(db: &Database, user_id: i64, et_id: i64, measurement: MeasurementType, day_offset: i64, value: f64) {
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, None, None)).unwrap();
        let mut s = new_exercise_set(entry_id, et_id, measurement, value);
        s.count = Some(5);
        s.logged_at = format!("{} 10:00:00", days_from_today(day_offset));
        db.insert_set(&s).unwrap();
    }

    /// A bench goal climbing 60 → 90 kg over the last 60 days, targeting 100 kg.
    async fn seed_rising_bench(handler: &super::AssistantHandler, user_id: i64, target_date: Option<String>) {
        let db = handler.db.lock().await;
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let mut goal = new_exercise_goal(user_id, bench.id, 100.0);
        goal.start_date = days_from_today(-90);
        goal.target_date = target_date;
        db.insert_goal(&goal).unwrap();
        [(-60, 60.0), (-30, 75.0), (-1, 90.0)].iter().for_each(|(day, kg)| {
            log_set(&db, user_id, bench.id, MeasurementType::WeightReps, *day, *kg);
        });
    }

    /// A live two-day-a-week programme whose three weeks have all closed. `kept`
    /// trains every slot; otherwise they all lapse to missed, which is drift by
    /// [C4.4]'s rule and the only way this module learns the user is not turning up.
    async fn seed_programme(handler: &super::AssistantHandler, user_id: i64, kept: bool) {
        let db = handler.db.lock().await;
        let mut draft = new_programme(user_id, "12-week hypertrophy", 2, "upper/lower", "linear");
        draft.start_date = days_from_today(-30);
        let programme_id = db.create_programme(&draft).unwrap();
        db.activate_programme(programme_id).unwrap();
        (1..=3).for_each(|week| {
            db.add_programme_slot(&new_programme_slot(programme_id, week, 1, "upper")).unwrap();
            db.add_programme_slot(&new_programme_slot(programme_id, week, 2, "lower")).unwrap();
        });
        if kept {
            db.list_programme_slots(programme_id).unwrap().iter().for_each(|slot| {
                let roster = db.create_roster(user_id, "Trained", None, None).unwrap();
                db.bind_roster_to_slot(roster, slot.id).unwrap();
                let session = db.start_session(user_id, None).unwrap();
                db.bind_roster_to_session(roster, session.id).unwrap();
            });
        }
    }

    // ── Registration ─────────────────────────────────────────────────────────

    /// The whole point of routing through the command table: `/help` and the
    /// advertised set pick the command up without a second list to maintain.
    #[test]
    fn progress_is_registered_in_the_command_table() {
        let spec = commands::COMMANDS.iter().find(|s| s.name == "/progress").expect("/progress must be in the command table");
        assert_eq!(commands::Command::parse("/progress"), Some(spec.command));
        assert!(!spec.description.is_empty());
    }

    #[tokio::test]
    async fn progress_is_advertised_and_listed_in_help() {
        let (handler, user, msg) = setup().await;
        assert!(commands::advertised_to(&user).iter().any(|c| c.name == "/progress"), "advertised set must carry /progress");

        let help = shown(&handler.handle_text_message(&msg, "/help").await.unwrap());
        assert!(help.contains("/progress"), "/help must list it: {help}");
    }

    // ── The view ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn with_no_goals_the_headline_invites_setting_one() {
        let (handler, _, msg) = setup().await;
        let progress = progress_of(&handler, &msg).await;
        assert!(progress.series.is_empty(), "no goals and no training means nothing to chart");
        assert!(progress.headline.to_lowercase().contains("no goals"), "headline should say so: {}", progress.headline);
    }

    /// Current value, target, trend and the time left all have to reach the client.
    #[tokio::test]
    async fn a_goal_charts_as_a_trajectory_with_its_target() {
        let (handler, user, msg) = setup().await;
        seed_rising_bench(&handler, user.id, Some(days_from_today(60))).await;

        let progress = progress_of(&handler, &msg).await;
        let series = progress.series.iter().find(|s| s.title.starts_with("Bench Press")).expect("bench series");
        assert_eq!(series.shape, SeriesShape::Trajectory { target: 100.0 }, "the target rides with the series");
        assert_eq!(series.better, Direction::Higher, "more weight is progress");
        assert_eq!(series.unit, "kg");
        assert_eq!(series.points.len(), 3);
        assert_eq!(series.latest().unwrap().value, 90.0);
        assert_eq!(series.improving(), Some(true));

        let note = progress.notes.iter().find(|n| n.starts_with("Bench Press")).expect("a note per goal");
        assert!(note.contains("90 kg of 100 kg"), "the note states where the user stands and the target: {note}");
        assert!(note.contains("8 weeks left"), "and how long is left: {note}");
        assert!(note.contains("on track, heading for 120.5 kg"), "and where the rate lands, rounded: {note}");
        assert_eq!(progress.headline, "1 of 1 goal on track.");

        // The same verdict, structured, for the clients and for [C6.6].
        let outlook = progress.goals.first().expect("an outlook per goal");
        assert_eq!(outlook.goal, "Bench Press");
        assert_eq!(outlook.outlook, GoalOutlook::OnTrack);
        assert_eq!(outlook.limiter, None);
        assert_eq!(outlook.readings, 3, "the evidence travels with the verdict");
        assert!(outlook.projected.is_some_and(|p| (p - 120.5).abs() < 0.1));
    }

    /// The correctness requirement of [C6.3]: on a cut a falling number is progress,
    /// and a client told otherwise renders success as regression.
    #[tokio::test]
    async fn a_weightloss_goal_reads_downwards_as_progress() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let mut goal = new_goal(user.id, GoalKind::Bodyweight, None, Some("bodyweight_kg".into()), 80.0, GoalDirection::Decrease);
            goal.start_date = days_from_today(-90);
            goal.target_date = Some(days_from_today(90));
            db.insert_goal(&goal).unwrap();
            [(-60, 95.0), (-30, 90.0), (-2, 86.0)].iter().for_each(|(day, kg)| {
                let mut m = new_body_metric(user.id, "bodyweight_kg", *kg);
                m.measured_at = format!("{} 08:00:00", days_from_today(*day));
                db.insert_body_metric(&m).unwrap();
            });
        }

        let progress = progress_of(&handler, &msg).await;
        let series = progress.series.iter().find(|s| s.title == "Bodyweight").expect("bodyweight series");
        assert_eq!(series.better, Direction::Lower, "down is progress on a cut");
        assert_eq!(series.unit, "kg");
        assert_eq!(series.improving(), Some(true), "95 → 86 kg towards 80 kg is progress");
        assert_eq!(series.change_line(), "95 → 86 kg (-9, better)");

        let note = progress.notes.iter().find(|n| n.starts_with("Bodyweight")).expect("a note for the goal");
        assert!(note.contains("on track"), "losing ~9kg in two months clears 80kg in three: {note}");
        assert_eq!(progress.headline, "1 of 1 goal on track.");
    }

    /// A decrease goal's readings come off the fast end of each day. Charted with
    /// MAX, this user's improving 5k would slope upwards.
    #[tokio::test]
    async fn a_decrease_exercise_goal_charts_the_days_best_low_value() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let plank = db.get_exercise_type_by_name("Plank").unwrap().unwrap();
            let mut goal = new_goal(user.id, GoalKind::Endurance, Some(plank.id), None, 60.0, GoalDirection::Decrease);
            goal.start_date = days_from_today(-90);
            goal.target_date = Some(days_from_today(60));
            db.insert_goal(&goal).unwrap();
            // Each day has a good attempt and a bad one; only the good one is the result.
            for (day, fast, slow) in [(-40i64, 100.0, 130.0), (-10, 80.0, 120.0)] {
                log_set(&db, user.id, plank.id, MeasurementType::TimeBased, day, fast);
                log_set(&db, user.id, plank.id, MeasurementType::TimeBased, day, slow);
            }
        }

        let progress = progress_of(&handler, &msg).await;
        let series = progress.series.iter().find(|s| s.title.starts_with("Plank")).expect("plank series");
        assert_eq!(series.better, Direction::Lower);
        assert_eq!(series.unit, "s");
        assert_eq!(series.points.iter().map(|p| p.value).collect::<Vec<_>>(), vec![100.0, 80.0], "the fast attempt is the day's result");
        assert_eq!(series.improving(), Some(true));
    }

    /// A goal whose deadline is too close for the current rate must not be counted
    /// as on track, however encouraging the trend looks. The user is turning up and
    /// improving, so what does not fit is the date — which is the goal's fault, and
    /// the distinction [C6.6] needs to give different advice.
    #[tokio::test]
    async fn a_trend_that_misses_the_deadline_reads_as_off_track() {
        let (handler, user, msg) = setup().await;
        seed_rising_bench(&handler, user.id, Some(days_from_today(2))).await;

        let progress = progress_of(&handler, &msg).await;
        assert_eq!(progress.headline, "0 of 1 goal on track.");
        let note = progress.notes.iter().find(|n| n.starts_with("Bench Press")).unwrap();
        assert!(note.contains("off track: this rate reaches only 91 kg"), "2 days at ~0.5kg/day does not close a 10kg gap: {note}");

        let outlook = &progress.goals[0];
        assert_eq!(outlook.outlook, GoalOutlook::OffTrack);
        assert_eq!(outlook.limiter, Some(GoalLimiter::Ambition), "the training is fine; the deadline never allowed for it");
    }

    /// The series still travels for a goal with nothing logged — the client shows an
    /// empty chart — but "nothing to project" must not be reported as "not on track".
    /// With no goal in any other band, that is the headline's whole answer.
    #[tokio::test]
    async fn a_goal_with_no_readings_is_too_early_rather_than_a_failure() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let mut goal = new_exercise_goal(user.id, bench.id, 100.0);
            goal.start_date = days_from_today(-30);
            goal.target_date = Some(days_from_today(30));
            db.insert_goal(&goal).unwrap();
        }

        let progress = progress_of(&handler, &msg).await;
        assert_eq!(progress.headline, "Too early to say on 1 goal — keep logging and I'll trend it.");
        let note = progress.notes.iter().find(|n| n.starts_with("Bench Press")).unwrap();
        assert!(note.contains("nothing logged yet"), "{note}");
        assert!(note.contains("too early to say: nothing logged against it yet"), "{note}");

        let outlook = &progress.goals[0];
        assert_eq!(outlook.outlook, GoalOutlook::TooEarly);
        assert_eq!(outlook.readings, 0);
        assert_eq!(outlook.projected, None);
        assert_eq!(outlook.limiter, None, "an unknown cause is not a diagnosed one");
    }

    /// Two readings are two readings, however far apart and however encouraging. The
    /// refusal has to reach the client as a refusal — a number here would be shown.
    #[tokio::test]
    async fn two_readings_do_not_make_a_projection() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let mut goal = new_exercise_goal(user.id, bench.id, 100.0);
            goal.start_date = days_from_today(-60);
            goal.target_date = Some(days_from_today(60));
            db.insert_goal(&goal).unwrap();
            [(-40, 80.0), (-5, 95.0)].iter().for_each(|(day, kg)| {
                log_set(&db, user.id, bench.id, MeasurementType::WeightReps, *day, *kg);
            });
        }

        let progress = progress_of(&handler, &msg).await;
        let outlook = &progress.goals[0];
        assert_eq!(outlook.outlook, GoalOutlook::TooEarly, "15kg in five weeks would sail past 100kg — and it is still two points");
        assert_eq!(outlook.readings, 2);
        assert_eq!(outlook.projected, None);
        let note = progress.notes.iter().find(|n| n.starts_with("Bench Press")).unwrap();
        assert!(note.contains("too early to say: 2 readings so far"), "{note}");
    }

    /// Readings bunched into one week give a slope, and a slope over three days is not
    /// a rate to carry two months forward.
    #[tokio::test]
    async fn readings_inside_a_single_week_give_no_rate() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let mut goal = new_exercise_goal(user.id, bench.id, 100.0);
            goal.start_date = days_from_today(-60);
            goal.target_date = Some(days_from_today(60));
            db.insert_goal(&goal).unwrap();
            [(-5, 80.0), (-3, 85.0), (-2, 90.0)].iter().for_each(|(day, kg)| {
                log_set(&db, user.id, bench.id, MeasurementType::WeightReps, *day, *kg);
            });
        }

        let progress = progress_of(&handler, &msg).await;
        let outlook = &progress.goals[0];
        assert_eq!(outlook.outlook, GoalOutlook::TooEarly, "three readings in three days are one good week");
        assert_eq!(outlook.readings, 3, "and the readings still travel — it is the rate that is withheld");
        assert_eq!(outlook.projected, None);
    }

    /// Adherence dominates ([C4.4]): the bench trend arrives comfortably, and the user
    /// is not turning up for the sessions that would produce it.
    #[tokio::test]
    async fn a_drifting_programme_pulls_an_arriving_goal_back_to_at_risk() {
        let (handler, user, msg) = setup().await;
        seed_rising_bench(&handler, user.id, Some(days_from_today(60))).await;
        seed_programme(&handler, user.id, false).await;

        let progress = progress_of(&handler, &msg).await;
        assert_eq!(progress.headline, "0 of 1 goal on track — 1 at risk.");
        let outlook = &progress.goals[0];
        assert_eq!(outlook.outlook, GoalOutlook::AtRisk);
        assert_eq!(outlook.limiter, Some(GoalLimiter::Attendance), "turning up outranks the trend line");
        assert!(outlook.projected.is_some(), "the projection is still a claim, and still travels");

        let note = progress.notes.iter().find(|n| n.starts_with("Bench Press")).unwrap();
        assert!(note.contains("at risk"), "{note}");
    }

    /// The same goal under a programme that is being kept to: nothing is in its way,
    /// so the attendance term must not manufacture a caveat out of a live programme.
    #[tokio::test]
    async fn a_programme_that_is_kept_to_leaves_the_verdict_alone() {
        let (handler, user, msg) = setup().await;
        seed_rising_bench(&handler, user.id, Some(days_from_today(60))).await;
        seed_programme(&handler, user.id, true).await;

        let progress = progress_of(&handler, &msg).await;
        assert_eq!(progress.headline, "1 of 1 goal on track.");
        assert_eq!(progress.goals[0].outlook, GoalOutlook::OnTrack);
        assert_eq!(progress.goals[0].limiter, None);
    }

    /// A settled goal is a record of what happened. Achieved names no cause; a goal
    /// whose date passed still names one, because that is what the post-mortem in
    /// [C6.6] is made of.
    #[tokio::test]
    async fn settled_goals_report_what_happened() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            let squat = db.get_exercise_type_by_name("Squat").unwrap().unwrap();

            let mut done = new_exercise_goal(user.id, bench.id, 100.0);
            done.start_date = days_from_today(-90);
            done.achieved = true;
            db.insert_goal(&done).unwrap();

            let mut lapsed = new_exercise_goal(user.id, squat.id, 140.0);
            lapsed.start_date = days_from_today(-90);
            lapsed.target_date = Some(days_from_today(-2));
            db.insert_goal(&lapsed).unwrap();
            [(-60, 100.0), (-30, 110.0), (-10, 120.0)].iter().for_each(|(day, kg)| {
                log_set(&db, user.id, squat.id, MeasurementType::WeightReps, *day, *kg);
            });
        }

        let progress = progress_of(&handler, &msg).await;
        let done = progress.goals.iter().find(|g| g.goal == "Bench Press").expect("the achieved goal");
        assert_eq!(done.outlook, GoalOutlook::Achieved);
        assert_eq!(done.limiter, None);
        assert_eq!(done.projected, None, "there is nothing left to project");

        let lapsed = progress.goals.iter().find(|g| g.goal == "Squat").expect("the lapsed goal");
        assert_eq!(lapsed.outlook, GoalOutlook::Missed);
        assert_eq!(lapsed.limiter, Some(GoalLimiter::Ambition), "20kg in two months was real progress towards an unreachable date");
        assert_eq!(progress.headline, "1 of 2 goals on track.");
    }

    /// Recent work becomes the one breakdown in the view: buckets, not a timeline,
    /// and with no better end to them.
    #[tokio::test]
    async fn recent_volume_rides_along_as_a_breakdown() {
        let (handler, user, msg) = setup().await;
        {
            let db = handler.db.lock().await;
            let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
            log_set(&db, user.id, bench.id, MeasurementType::WeightReps, -1, 80.0);
        }

        let progress = progress_of(&handler, &msg).await;
        let volume = progress.series.iter().find(|s| s.shape == SeriesShape::Breakdown).expect("a volume breakdown");
        assert_eq!(volume.better, Direction::Neutral, "volume has no better end — a deload is meant to be lower");
        assert_eq!(volume.points.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), vec!["Chest"]);
        assert_eq!(volume.points[0].value, 400.0, "5 reps x 80kg");
    }

    // ── The arithmetic ───────────────────────────────────────────────────────

    /// The headline is the one line every client leads with, so each band has to be
    /// visible in it — and "too early to say" must never be counted as a failure.
    #[test]
    fn the_headline_names_the_bands_a_count_would_hide() {
        use GoalOutlook::*;
        assert_eq!(headline(&[]), "No goals on file yet — tell me what you're working towards and I'll track it here.");
        assert_eq!(headline(&[OnTrack]), "1 of 1 goal on track.");
        assert_eq!(headline(&[OnTrack, Achieved, OffTrack]), "2 of 3 goals on track.", "off track is already implied by the count");
        assert_eq!(headline(&[OnTrack, AtRisk, TooEarly]), "1 of 3 goals on track — 1 at risk, 1 too early to say.");
        assert_eq!(headline(&[TooEarly, TooEarly]), "Too early to say on 2 goals — keep logging and I'll trend them.");
    }

    /// The evidence behind a refusal to project, said rather than implied.
    #[test]
    fn the_evidence_label_says_what_is_missing() {
        assert_eq!(evidence_label(0), "nothing logged against it yet");
        assert_eq!(evidence_label(1), "one reading so far");
        assert_eq!(evidence_label(2), "2 readings so far, and no trend across them yet");
    }

    #[test]
    fn remaining_reads_in_the_largest_readable_unit() {
        assert_eq!(remaining_label(-3), "no time");
        assert_eq!(remaining_label(0), "under a day");
        assert_eq!(remaining_label(1), "1 day");
        assert_eq!(remaining_label(9), "9 days");
        assert_eq!(remaining_label(21), "3 weeks");
        assert_eq!(remaining_label(180), "6 months");
    }

    #[test]
    fn metric_labels_and_units_come_off_the_canonical_names() {
        assert_eq!((metric_label("bodyweight_kg"), metric_unit("bodyweight_kg")), ("Bodyweight".to_string(), "kg"));
        assert_eq!((metric_label("body_fat_pct"), metric_unit("body_fat_pct")), ("Body fat".to_string(), "%"));
        assert_eq!((metric_label("resting_hr_bpm"), metric_unit("resting_hr_bpm")), ("Resting heart rate".to_string(), "bpm"));
        // An unknown metric is de-snaked, and its unit suffix dropped from the label.
        assert_eq!((metric_label("grip_strength_kg"), metric_unit("grip_strength_kg")), ("Grip strength".to_string(), "kg"));
        // No recognised suffix means no unit, and nothing to strip.
        assert_eq!((metric_label("sessions_per_week"), metric_unit("sessions_per_week")), ("Sessions per week".to_string(), ""));
    }
}


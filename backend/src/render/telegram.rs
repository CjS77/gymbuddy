//! Render a [`View`] to Telegram Bot API HTML (or plain text).
//!
//! Reproduces the exact output the handler emitted before the view refactor, so
//! the Telegram bot is visually unchanged — locked down by golden tests below.

use gymbuddy_proto::{
    CatalogView, HistoryView, PROGRAMME_LOCK_IN_ASK, ProgrammeProgressView, ProgrammeView, ProgressView, Render, SeriesShape, SeriesView,
    SessionReviewView, SessionRosterView, SetLine, StatusView, TrainingModeView, View,
};

/// The Telegram renderer. `Output` is `(text, parse_mode)` — `parse_mode` is
/// `Some("HTML")` for the rich `/status` and `/exercises` views, `None` otherwise.
pub struct Telegram;

impl Render for Telegram {
    type Output = (String, Option<&'static str>);

    fn render(&self, view: &View) -> Self::Output {
        match view {
            View::Message { text, notes, failures } => (compose_message(text, notes, failures), None),
            View::Notice { text } => (text.clone(), None),
            View::History(history) => (render_history(history), None),
            View::Status(status) => (render_status(status), Some("HTML")),
            View::Catalog(catalog) => (render_catalog(catalog), Some("HTML")),
            View::SessionRoster(roster) => (render_session_roster(roster, None), Some("HTML")),
            View::ProgrammeSessionRoster { roster, mode } => (render_session_roster(roster, Some(mode)), Some("HTML")),
            View::Programme(programme) => (render_programme(programme), Some("HTML")),
            View::Progress(progress) => (render_progress(progress), Some("HTML")),
            View::SessionReview(review) => (render_session_review(review), Some("HTML")),
            View::ProgrammeProgress(report) => (render_programme_progress(report), Some("HTML")),
            View::Timers { enabled } => (format!("Rest timers are now {}.", if *enabled { "on" } else { "off" }), None),
            // `View` is `#[non_exhaustive]`: a variant from a newer server lands here.
            // Degrade to plain text rather than sending Telegram an empty message
            // (which the Bot API rejects).
            _ => (view.fallback_text(), None),
        }
    }
}

/// Free-form reply text followed by conversational notes and any failure summary,
/// joined exactly as the handler used to assemble them.
fn compose_message(text: &str, notes: &[String], failures: &[String]) -> String {
    let mut out = text.to_string();
    for note in notes {
        out.push_str("\n\n");
        out.push_str(note);
    }
    if !failures.is_empty() {
        out.push_str(&format!("\n\n(Note: some actions failed: {})", failures.join("; ")));
    }
    out
}

fn render_status(status: &StatusView) -> String {
    let mut result = format!("<b>Status for {}</b>\n", escape_html(&status.user_name));

    match &status.session {
        Some(session) => {
            result.push_str(&format!("\n<b>Active session</b> (started {})\n", escape_html(&session.started_at)));

            if session.completed.is_empty() && session.in_progress.is_empty() {
                result.push_str("No exercises logged yet\n");
            }

            if !session.completed.is_empty() {
                result.push_str("<b>Completed:</b>\n");
                for log in &session.completed {
                    result.push_str(&format!("- <b>{}</b> ({}) — {}\n", escape_html(&log.name), set_count(log.sets.len()), escape_html(&joined_sets(&log.sets))));
                }
            }

            if session.in_progress.len() > 1 {
                result.push_str("<b>Superset (in progress):</b>\n");
                for (i, log) in session.in_progress.iter().enumerate() {
                    result.push_str(&format!(
                        "  {}. <b>{}</b> ({}) — {}\n",
                        i + 1,
                        escape_html(&log.name),
                        set_count(log.sets.len()),
                        escape_html(&joined_sets(&log.sets)),
                    ));
                }
            } else if let Some(log) = session.in_progress.first() {
                result.push_str("<b>Current exercise:</b>\n");
                result.push_str(&format!("- <b>{}</b> ({}) — {}\n", escape_html(&log.name), set_count(log.sets.len()), escape_html(&joined_sets(&log.sets))));
            }
        }
        None => result.push_str("No active session\n"),
    }

    if !status.health.is_empty() {
        result.push_str("\n<b>Active health issues</b>\n");
        for note in &status.health {
            result.push_str(&format!("- {} ({}): {}\n", note.kind, escape_html(&note.body_part), escape_html(&note.description)));
        }
    }

    result
}

fn render_catalog(catalog: &CatalogView) -> String {
    let mut result = String::new();
    for group in &catalog.groups {
        // Column widths are taken from the unescaped names/aliases, matching the
        // pre-refactor behaviour.
        let name_w = group.exercises.iter().map(|e| e.name.len()).max().unwrap_or(4).max(4);
        let alias_w = group.exercises.iter().map(|e| e.aliases.len()).max().unwrap_or(7).max(7);

        result.push_str(&format!("\n<b>{}</b>\n<pre>", escape_html(&group.muscle_group)));
        result.push_str(&format!("{:<name_w$} | {:<alias_w$} | Type\n", "Name", "Aliases"));
        result.push_str(&format!("{:-<name_w$}-+-{:-<alias_w$}-+------\n", "", ""));
        for entry in &group.exercises {
            result.push_str(&format!("{:<name_w$} | {:<alias_w$} | {}\n", escape_html(&entry.name), escape_html(&entry.aliases), entry.kind));
        }
        result.push_str("</pre>");
    }
    result
}

fn render_history(history: &HistoryView) -> String {
    if history.sessions.is_empty() {
        return "No workout history yet. Start by telling me about an exercise!".to_string();
    }

    let mut parts = vec!["Recent workouts:".to_string()];
    for summary in history.sessions.iter().take(5) {
        let duration = summary.minutes.map(|d| format!(" ({d} min)")).unwrap_or_default();
        parts.push(format!("- {} [{}]: {} entries{duration}", summary.started_at, summary.status, summary.entries));
    }
    parts.join("\n")
}

/// Render a designed session roster; `mode` (present only when a programme is
/// active, [C1.4]) adds one italic line under the title saying whether this fills
/// the programme's current slot or deliberately sidesteps it. `None` — ad-hoc with
/// no programme — keeps the golden pre-programme layout untouched.
fn render_session_roster(roster: &SessionRosterView, mode: Option<&TrainingModeView>) -> String {
    let mut result = format!("<b>{}</b>\n", escape_html(&roster.title));

    if let Some(mode) = mode {
        result.push_str(&format!("<i>{}</i>\n", escape_html(&mode.summary())));
    }

    if let Some(rationale) = &roster.rationale
        && !rationale.trim().is_empty()
    {
        result.push_str(&format!("\n{}\n", escape_html(rationale)));
    }

    if !roster.exercises.is_empty() {
        result.push('\n');
        for (i, exercise) in roster.exercises.iter().enumerate() {
            let target = exercise.target_line();
            let target_part = if target.is_empty() { String::new() } else { format!(" — {target}") };
            result.push_str(&format!("{}. <b>{}</b>{target_part}\n", i + 1, escape_html(&exercise.name)));
            if let Some(cue) = &exercise.cue
                && !cue.trim().is_empty()
            {
                result.push_str(&format!("   <i>{}</i>\n", escape_html(cue)));
            }
        }
    }

    if !roster.notes.is_empty() {
        result.push_str("\n<b>Notes:</b>\n");
        for note in &roster.notes {
            result.push_str(&format!("- {}\n", escape_html(note)));
        }
    }

    result.push_str("\nNothing is logged yet -- log your sets as you go and I'll adjust.");
    result
}

/// Render a designed programme ([C4.2]): its shape, the goals it serves, its mesocycle
/// blocks and the repeating week. No exercises appear — a programme is a skeleton, and
/// each session is still designed against it by `/nextworkout`.
///
/// A draft closes with [`PROGRAMME_LOCK_IN_ASK`]; an already-active programme does not,
/// because there is nothing left to confirm.
fn render_programme(p: &ProgrammeView) -> String {
    let mut result = format!("<b>{}</b>\n<i>{}</i>\n", escape_html(&p.title), escape_html(&p.shape_line()));

    let dates = match &p.target_end_date {
        Some(end) => format!("{} to {}", escape_html(&p.start_date), escape_html(end)),
        None => format!("from {}", escape_html(&p.start_date)),
    };
    result.push_str(&format!("{dates}\nProgression: {}\n", escape_html(&p.progression_policy)));

    // [R2.1]: present only on a live programme being reported on, and rendered first,
    // because "where am I?" is the whole question `/programme status` was asked.
    if let (Some(position), Some(status)) = (p.position_line(), p.status.as_ref()) {
        result.push_str(&format!("\n<b>Where you are:</b>\n{}\n", escape_html(&position)));
        if let Some(slot) = &status.next_slot {
            result.push_str(&format!("Next: {}\n", escape_html(&slot.label())));
        }
        result.push_str(&format!("{}\n", escape_html(&status.counts_line())));
    }

    if !p.goals.is_empty() {
        result.push_str("\n<b>Goals served:</b>\n");
        result.extend(p.goals.iter().map(|goal| format!("- {}\n", escape_html(goal))));
    }

    if !p.blocks.is_empty() {
        result.push_str("\n<b>Blocks:</b>\n");
        result.extend(
            p.blocks.iter().map(|b| format!("- {}: {}\n", escape_html(&b.weeks_label()), escape_html(&b.focus))),
        );
    }

    if !p.week_template.is_empty() {
        result.push_str("\n<b>Each week:</b>\n");
        result.extend(p.week_template.iter().map(|d| format!("- Day {}: {}\n", d.day_idx, escape_html(&d.focus))));
    }

    if !p.notes.is_empty() {
        result.push_str("\n<b>Notes:</b>\n");
        result.extend(p.notes.iter().map(|note| format!("- {}\n", escape_html(note))));
    }

    if !p.active {
        result.push_str(&format!("\n{PROGRAMME_LOCK_IN_ASK}"));
    }
    result
}

/// Render the full report on a live programme ([C4.6]).
///
/// Leads with the programme itself — the same layout `/programme` produces, position
/// included — because "where am I?" is the question the command was asked. Then the two
/// halves this view adds: how well the week is being kept to, and where the goals it
/// serves are heading. The charts go through the [C6.2] series renderer, never a second
/// one grown here.
fn render_programme_progress(p: &ProgrammeProgressView) -> String {
    let mut out = render_programme(&p.programme);

    out.push_str(&format!("\n<b>Keeping to it:</b>\n{}\n", escape_html(&p.adherence.rate_line())));
    out.extend(p.adherence.drifting_days.iter().map(|day| format!("- {}\n", escape_html(&day.line()))));
    if let Some(reschedule) = &p.adherence.reschedule {
        out.push_str(&format!("\n{}\n", escape_html(&reschedule.offer())));
    }

    if !p.goals.is_empty() {
        out.push_str("\n<b>Goals:</b>\n");
        out.extend(p.goals.iter().map(|series| format!("\n{}", render_series(series))));
    }
    out
}

/// How wide a [`SeriesShape::Breakdown`] bar may grow. Telegram wraps on narrow
/// phones, so the bar has to fit beside its label rather than fill a terminal.
const BAR_WIDTH: usize = 12;

/// Render progress ([C6.2]): the headline, then one block per series, then any
/// caveats. Telegram cannot draw, so the series arrive as text — but the choice of
/// text is made per *shape*, not per metric, which is what stops this growing a
/// branch every time Core adds something to chart.
fn render_progress(p: &ProgressView) -> String {
    let mut result = format!("<b>{}</b>\n", escape_html(&p.summary_line()));
    result.extend(p.series.iter().map(|series| format!("\n{}", render_series(series))));

    if !p.notes.is_empty() {
        result.push_str("\n<b>Notes:</b>\n");
        result.extend(p.notes.iter().map(|note| format!("- {}\n", escape_html(note))));
    }
    result
}

/// One series: a title, then the shape's own rendering.
/// The post-session review ([C6.5]).
///
/// Ordered by what the user most needs to see: a completed goal first, because finishing one
/// is the only thing here worth interrupting for, then the coach's read, then the numbers it
/// was drawn from. Records come before the per-exercise list — a PR is the one line a user
/// will screenshot.
fn render_session_review(r: &SessionReviewView) -> String {
    let mut out = format!("<b>{}</b>\n", escape_html(&r.summary_line()));

    if let Some(position) = &r.position {
        out.push_str(&format!("<i>{}</i>\n", escape_html(&position.summary())));
    }
    if let Some(intent) = &r.intent {
        out.push_str(&format!("<i>Set out to: {}</i>\n", escape_html(intent)));
    }

    if !r.achieved_goals.is_empty() {
        out.push_str("\n<b>Goal reached</b>\n");
        out.extend(r.achieved_goals.iter().map(|g| format!("- {}\n", escape_html(g))));
    }

    // The commentary is the model's, and only the programme tier has one. It sits under its
    // own heading so the user can always tell which words were computed and which were written.
    if let Some(commentary) = &r.commentary {
        out.push_str(&format!("\n{}\n", escape_html(commentary)));
    }

    if !r.records.is_empty() {
        out.push_str("\n<b>Personal records</b>\n");
        out.extend(r.records.iter().map(|record| format!("- {}\n", escape_html(&record.line()))));
    }

    if !r.exercises.is_empty() {
        out.push_str("\n<b>What you did</b>\n");
        out.extend(r.exercises.iter().map(|e| format!("- {}\n", escape_html(&e.line()))));
    }

    let context: Vec<String> = r
        .effort
        .iter()
        .map(|e| e.line())
        .chain(r.streak_days.map(|d| format!("Streak: {d} day{}", if d == 1 { "" } else { "s" })))
        .chain(r.week_line.clone().map(|w| format!("This week: {w}")))
        .collect();
    if !context.is_empty() {
        out.push('\n');
        out.extend(context.iter().map(|line| format!("{}\n", escape_html(line))));
    }

    if !r.goals.is_empty() {
        out.push_str("\n<b>Goals</b>\n");
        out.extend(r.goals.iter().map(|g| format!("- {}\n", escape_html(g))));
    }

    // The charts share the [C6.2] series renderer rather than growing a second one.
    out.extend(r.series.iter().map(|series| format!("\n{}", render_series(series))));

    if !r.notes.is_empty() {
        out.push_str("\n<b>Notes:</b>\n");
        out.extend(r.notes.iter().map(|note| format!("- {}\n", escape_html(note))));
    }
    out
}

fn render_series(s: &SeriesView) -> String {
    let body = match s.shape {
        SeriesShape::Breakdown => render_bars(s),
        SeriesShape::Trend | SeriesShape::Trajectory { .. } => render_trend(s),
    };
    format!("<b>{}</b>\n{body}", escape_html(&s.title))
}

/// A time-ordered series as a unicode sparkline plus its movement.
///
/// The sparkline is `<code>`-wrapped so Telegram renders the blocks in a monospace
/// run; the verdict on the movement is in words (from `change_line`), never in the
/// shape of the line — a falling series can be progress ([C6.2]).
fn render_trend(s: &SeriesView) -> String {
    let Some(latest) = s.latest() else {
        return "No readings yet.\n".to_string();
    };

    let mut out = format!("<code>{}</code>\n", s.spark());
    match s.change_line() {
        // Too short to have moved: report where it stands instead of nothing.
        line if line.is_empty() => out.push_str(&format!("{}\n", escape_html(&s.latest_line()))),
        line => out.push_str(&format!("{}\n", escape_html(&line))),
    }
    if !s.target_line().is_empty() {
        out.push_str(&format!("{}\n", escape_html(&s.target_line())));
    }
    if let Some(first) = s.points.first()
        && first.label != latest.label
    {
        out.push_str(&format!("<i>{} to {}</i>\n", escape_html(&first.label), escape_html(&latest.label)));
    }
    out
}

/// Independent buckets as proportional bars, scaled to the largest. A breakdown has
/// no time order, so it gets no sparkline and no trend line.
fn render_bars(s: &SeriesView) -> String {
    let Some((_, max)) = s.bounds() else {
        return "No readings yet.\n".to_string();
    };
    let width = s.points.iter().map(|p| p.label.chars().count()).max().unwrap_or(0);

    s.points
        .iter()
        .map(|p| {
            let filled = if max <= 0.0 { 0 } else { ((p.value / max) * BAR_WIDTH as f64).round() as usize };
            let bar = "█".repeat(filled.min(BAR_WIDTH));
            format!("<code>{:<width$} {bar:<BAR_WIDTH$}</code> {}\n", escape_html(&p.label), escape_html(&s.value_label(p.value)))
        })
        .collect()
}

/// Compact rendering of one set ("8×80kg", "30s", …) shared with every client via
/// [`SetLine::compact`](gymbuddy_proto::SetLine::compact).
fn joined_sets(sets: &[SetLine]) -> String {
    sets.iter().map(SetLine::compact).collect::<Vec<_>>().join(", ")
}

fn set_count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "set" } else { "sets" })
}

/// Escape the three characters that are significant in Telegram HTML.
pub(crate) fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gymbuddy_proto::{
        CatalogEntry, CatalogGroup, Direction, ExerciseLog, HealthNote, Measurement, ReviewEffortView, ReviewExerciseView,
        ReviewKindView, ReviewRecordView, SeriesPointView, SessionView,
    };

    fn set(count: Option<u32>, value: f64) -> SetLine {
        SetLine { measurement: Measurement::WeightReps, count, value }
    }

    #[test]
    fn status_html_matches_legacy_layout() {
        let status = StatusView {
            user_name: "Alice".into(),
            session: Some(SessionView {
                started_at: "2026-06-16 10:00:00".into(),
                completed: vec![ExerciseLog { name: "Bench Press".into(), sets: vec![set(Some(8), 80.0), set(Some(8), 80.0)] }],
                in_progress: vec![ExerciseLog { name: "Squat".into(), sets: vec![set(Some(5), 100.0)] }],
            }),
            health: vec![HealthNote { kind: "injury".into(), body_part: "shoulder".into(), description: "sore".into() }],
        };
        let (html, mode) = Telegram.render(&View::Status(status));
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>Status for Alice</b>\n\
                        \n<b>Active session</b> (started 2026-06-16 10:00:00)\n\
                        <b>Completed:</b>\n\
                        - <b>Bench Press</b> (2 sets) — 8×80kg, 8×80kg\n\
                        <b>Current exercise:</b>\n\
                        - <b>Squat</b> (1 set) — 5×100kg\n\
                        \n<b>Active health issues</b>\n\
                        - injury (shoulder): sore\n";
        assert_eq!(html, expected);
    }

    #[test]
    fn status_no_session() {
        let status = StatusView { user_name: "Bob".into(), session: None, health: vec![] };
        let (html, _) = Telegram.render(&View::Status(status));
        assert_eq!(html, "<b>Status for Bob</b>\nNo active session\n");
    }

    #[test]
    fn catalog_html_matches_legacy_pre_table() {
        let catalog = CatalogView {
            groups: vec![CatalogGroup {
                muscle_group: "Chest".into(),
                exercises: vec![CatalogEntry { name: "Bench Press".into(), aliases: "bench".into(), kind: "weight_reps".into() }],
            }],
        };
        let (html, mode) = Telegram.render(&View::Catalog(catalog));
        assert_eq!(mode, Some("HTML"));
        let expected = "\n<b>Chest</b>\n<pre>\
                        Name        | Aliases | Type\n\
                        ------------+---------+------\n\
                        Bench Press | bench   | weight_reps\n\
                        </pre>";
        assert_eq!(html, expected);
    }

    #[test]
    fn message_appends_notes_and_failures() {
        let view = View::Message {
            text: "Logged your bench press!".into(),
            notes: vec!["You've logged 3 sets of Bench Press. Want another set, or move to the next exercise?".into()],
            failures: vec!["Unknown exercise: foo".into()],
        };
        let (text, mode) = Telegram.render(&view);
        assert_eq!(mode, None);
        assert_eq!(
            text,
            "Logged your bench press!\n\n\
             You've logged 3 sets of Bench Press. Want another set, or move to the next exercise?\n\n\
             (Note: some actions failed: Unknown exercise: foo)"
        );
    }

    #[test]
    fn escape_html_escapes_amp_lt_gt() {
        assert_eq!(escape_html("a & b < c > d"), "a &amp; b &lt; c &gt; d");
    }

    #[test]
    fn session_roster_html_renders_the_prescription() {
        use gymbuddy_proto::RosterExerciseView;
        let roster = SessionRosterView {
            title: "Upper push + lat focus".into(),
            rationale: Some("2 days rest on bench and your last session was easy, so we push the weight.".into()),
            exercises: vec![
                RosterExerciseView {
                    name: "Bench Press".into(),
                    target_sets: Some(3),
                    target_reps: Some(6),
                    target_weight_kg: Some(65.0),
                    target_secs: None,
                    cue: Some("Last time 55kg felt easy.".into()),
                },
                RosterExerciseView {
                    name: "One Arm Dumbbell Row".into(),
                    target_sets: Some(3),
                    target_reps: Some(8),
                    target_weight_kg: Some(24.0),
                    target_secs: None,
                    cue: None,
                },
                RosterExerciseView {
                    name: "Plank".into(),
                    target_sets: Some(3),
                    target_reps: None,
                    target_weight_kg: None,
                    target_secs: Some(60),
                    cue: None,
                },
            ],
            notes: vec!["Skipped deadlift to protect your lower back.".into()],
        };
        let (html, mode) = Telegram.render(&View::SessionRoster(roster));
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>Upper push + lat focus</b>\n\
                        \n2 days rest on bench and your last session was easy, so we push the weight.\n\
                        \n\
                        1. <b>Bench Press</b> — 3 sets × 6 reps @ 65kg\n\
                        \u{20}  <i>Last time 55kg felt easy.</i>\n\
                        2. <b>One Arm Dumbbell Row</b> — 3 sets × 8 reps @ 24kg\n\
                        3. <b>Plank</b> — 3 sets × 60s\n\
                        \n<b>Notes:</b>\n\
                        - Skipped deadlift to protect your lower back.\n\
                        \nNothing is logged yet -- log your sets as you go and I'll adjust.";
        assert_eq!(html, expected);
    }

    fn one_exercise_roster() -> SessionRosterView {
        use gymbuddy_proto::RosterExerciseView;
        SessionRosterView {
            title: "Upper".into(),
            rationale: None,
            exercises: vec![RosterExerciseView {
                name: "Bench Press".into(),
                target_sets: Some(3),
                target_reps: Some(6),
                target_weight_kg: Some(65.0),
                target_secs: None,
                cue: None,
            }],
            notes: vec![],
        }
    }

    /// [C1.4]: a programme-slot design carries an italic mode line under the title;
    /// the rest of the layout is the untouched ad-hoc one.
    #[test]
    fn programme_session_roster_html_names_the_slot() {
        let view = View::ProgrammeSessionRoster {
            roster: one_exercise_roster(),
            mode: TrainingModeView::Programme { programme_title: "12-week <hyper>".into(), week: 2, day: 1, focus: "upper".into() },
        };
        let (html, mode) = Telegram.render(&view);
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>Upper</b>\n\
                        <i>Programme: 12-week &lt;hyper&gt; — week 2, day 1: upper</i>\n\
                        \n\
                        1. <b>Bench Press</b> — 3 sets × 6 reps @ 65kg\n\
                        \nNothing is logged yet -- log your sets as you go and I'll adjust.";
        assert_eq!(html, expected);
    }

    fn points(raw: &[(&str, f64)]) -> Vec<SeriesPointView> {
        raw.iter().map(|(label, value)| SeriesPointView { label: (*label).into(), value: *value }).collect()
    }

    /// [C6.2]: a trend arrives as a sparkline, its movement, its target and the span
    /// the readings cover.
    #[test]
    fn progress_html_renders_a_trend_block() {
        let view = View::Progress(ProgressView {
            headline: "2 of 3 goals on track".into(),
            series: vec![SeriesView {
                title: "Bench Press".into(),
                unit: "kg".into(),
                better: Direction::Higher,
                shape: SeriesShape::Trajectory { target: 100.0 },
                points: points(&[("2026-05-01", 80.0), ("2026-06-01", 85.0), ("2026-07-01", 92.5)]),
            }],
            notes: vec!["Squat has too few sessions to trend.".into()],
        });
        let (html, mode) = Telegram.render(&view);
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>2 of 3 goals on track</b>\n\
                        \n<b>Bench Press</b>\n\
                        <code>▁▄█</code>\n\
                        80 → 92.5 kg (+12.5, better)\n\
                        Target: 100 kg\n\
                        <i>2026-05-01 to 2026-07-01</i>\n\
                        \n<b>Notes:</b>\n\
                        - Squat has too few sessions to trend.\n";
        assert_eq!(html, expected);
    }

    /// A programme-tier report, end to end. Golden because the *order* is the design:
    /// a completed goal leads, the commentary follows, and the numbers it was drawn from
    /// come after — a user who reads only the first line should get the important part.
    #[test]
    fn a_programme_report_leads_with_the_goal_then_the_commentary() {
        let view = View::SessionReview(Box::new(review_fixture()));
        let (html, mode) = Telegram.render(&view);
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>Goal reached: Overhead Press to 40</b>\n\
                        <i>Programme: 12-week hypertrophy — week 2, day 1: upper</i>\n\
                        <i>Set out to: upper push</i>\n\
                        \n<b>Goal reached</b>\n\
                        - Overhead Press to 40\n\
                        \nThe extra 2.5kg held for all three sets.\n\
                        \n<b>Personal records</b>\n\
                        - Bench Press: 67.5kg × 6 (was 65kg × 6)\n\
                        \n<b>What you did</b>\n\
                        - Bench Press: 3 sets × 6 reps @ 67.5kg (asked 3 sets × 6 reps @ 65kg) — +2.5kg\n\
                        \nEffort: hard\n\
                        Streak: 4 days\n\
                        This week: 3 sessions, 12400 kg total volume\n";
        assert_eq!(html, expected);
    }

    /// The ad-hoc tier renders without a commentary section at all — the visible half of
    /// the [C6.5] guarantee that no model was asked.
    #[test]
    fn an_adhoc_summary_renders_no_commentary_section() {
        let review = SessionReviewView {
            headline: "Session logged — 1 exercise".into(),
            kind: ReviewKindView::Summary,
            commentary: None,
            position: None,
            achieved_goals: vec![],
            records: vec![],
            exercises: vec![ReviewExerciseView {
                name: "Bench Press".into(),
                prescribed: None,
                actual: "3 sets × 6 reps @ 67.5kg".into(),
                delta: None,
            }],
            ..review_fixture()
        };
        let (html, _) = Telegram.render(&View::SessionReview(Box::new(review)));
        let expected = "<b>Session logged — 1 exercise</b>\n\
                        <i>Set out to: upper push</i>\n\
                        \n<b>What you did</b>\n\
                        - Bench Press: 3 sets × 6 reps @ 67.5kg\n\
                        \nEffort: hard\n\
                        Streak: 4 days\n\
                        This week: 3 sessions, 12400 kg total volume\n";
        assert_eq!(html, expected);
    }

    /// A derived effort says so, so the user knows it is open to correction — the note
    /// the auto-close path sends depends on this reading as a guess, not a verdict.
    #[test]
    fn a_derived_effort_is_rendered_as_a_guess() {
        let review =
            SessionReviewView { effort: Some(ReviewEffortView { label: "hard".into(), confirmed: false }), ..review_fixture() };
        let (html, _) = Telegram.render(&View::SessionReview(Box::new(review)));
        assert!(html.contains("Effort: hard (my read, not yours)"), "{html}");
    }

    /// A review's charts go through the same [C6.2] series renderer as /progress, rather
    /// than a second one grown for the review.
    #[test]
    fn a_review_renders_its_series_with_the_shared_sparkline() {
        let review = SessionReviewView {
            series: vec![SeriesView {
                title: "Bench Press".into(),
                unit: "kg".into(),
                better: Direction::Higher,
                shape: SeriesShape::Trend,
                points: points(&[("2026-05-01", 80.0), ("2026-06-01", 85.0), ("2026-07-01", 92.5)]),
            }],
            ..review_fixture()
        };
        let (html, _) = Telegram.render(&View::SessionReview(Box::new(review)));
        assert!(html.contains("<code>▁▄█</code>"), "{html}");
        assert!(html.contains("80 → 92.5 kg (+12.5, better)"), "{html}");
    }

    /// Everything a review carries is user-supplied or model-written, so all of it is
    /// escaped — an exercise name with an angle bracket must not become markup.
    #[test]
    fn review_escapes_every_text_field() {
        let review = SessionReviewView {
            headline: "A <b>bold</b> session".into(),
            commentary: Some("You & I both know <that>".into()),
            exercises: vec![ReviewExerciseView {
                name: "<script>".into(),
                prescribed: None,
                actual: "1 set".into(),
                delta: None,
            }],
            ..review_fixture()
        };
        let (html, _) = Telegram.render(&View::SessionReview(Box::new(review)));
        assert!(html.contains("A &lt;b&gt;bold&lt;/b&gt; session"), "{html}");
        assert!(html.contains("You &amp; I both know &lt;that&gt;"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script>"), "{html}");
    }

    /// A representative programme-tier review, shared by the tests above.
    fn review_fixture() -> SessionReviewView {
        SessionReviewView {
            headline: "Goal reached: Overhead Press to 40".into(),
            kind: ReviewKindView::Report,
            session_date: "2026-07-19 10:00:00".into(),
            intent: Some("upper push".into()),
            effort: Some(ReviewEffortView { label: "hard".into(), confirmed: true }),
            exercises: vec![ReviewExerciseView {
                name: "Bench Press".into(),
                prescribed: Some("3 sets × 6 reps @ 65kg".into()),
                actual: "3 sets × 6 reps @ 67.5kg".into(),
                delta: Some("+2.5kg".into()),
            }],
            records: vec![ReviewRecordView {
                exercise: "Bench Press".into(),
                detail: "67.5kg × 6".into(),
                previous: Some("65kg × 6".into()),
            }],
            commentary: Some("The extra 2.5kg held for all three sets.".into()),
            goals: vec![],
            achieved_goals: vec!["Overhead Press to 40".into()],
            position: Some(TrainingModeView::Programme {
                programme_title: "12-week hypertrophy".into(),
                week: 2,
                day: 1,
                focus: "upper".into(),
            }),
            adherence: Some("1 of 1 prescribed exercises completed".into()),
            streak_days: Some(4),
            week_line: Some("3 sessions, 12400 kg total volume".into()),
            series: vec![],
            notes: vec![],
        }
    }

    /// The [C6.2] rule that makes the whole contract worth having: "better" is the
    /// series' own, not the direction of the line. A cut's bodyweight falls, and that
    /// is progress — the sparkline still slopes down, because that is what was logged.
    #[test]
    fn a_falling_series_can_still_read_as_progress() {
        let view = View::Progress(ProgressView {
            headline: "Cutting".into(),
            series: vec![SeriesView {
                title: "Bodyweight".into(),
                unit: "kg".into(),
                better: Direction::Lower,
                shape: SeriesShape::Trend,
                points: points(&[("2026-05-01", 90.0), ("2026-06-01", 87.5)]),
            }],
            notes: vec![],
        });
        let (html, _) = Telegram.render(&view);
        assert!(html.contains("90 → 87.5 kg (-2.5, better)"), "got: {html}");
        assert!(html.contains("<code>█▁</code>"), "the readings are plotted as logged: {html}");
    }

    /// A breakdown gets bars scaled to its largest bucket — and no trend line, since
    /// its buckets are not ordered in time.
    #[test]
    fn progress_html_bars_scale_to_the_largest_bucket() {
        let view = View::Progress(ProgressView {
            headline: "This week".into(),
            series: vec![SeriesView {
                title: "Volume by group".into(),
                unit: "sets".into(),
                better: Direction::Neutral,
                shape: SeriesShape::Breakdown,
                points: points(&[("Chest", 12.0), ("Back", 16.0), ("Legs", 9.0)]),
            }],
            notes: vec![],
        });
        let (html, _) = Telegram.render(&view);
        // Labels are padded to a common width, and the largest bucket fills the bar.
        assert!(html.contains("<code>Back  ████████████</code> 16 sets\n"), "got: {html}");
        assert!(html.contains("<code>Chest █████████   </code> 12 sets\n"), "got: {html}");
        assert!(html.contains("<code>Legs  ███████     </code> 9 sets\n"), "got: {html}");
        assert!(!html.contains('→'), "a breakdown has no trend to report: {html}");
    }

    #[test]
    fn progress_html_escapes_titles_labels_and_notes() {
        let view = View::Progress(ProgressView {
            headline: "Progress & <goals>".into(),
            series: vec![SeriesView {
                title: "Row <wide> & narrow".into(),
                unit: "kg".into(),
                better: Direction::Neutral,
                shape: SeriesShape::Breakdown,
                points: points(&[("A & B", 5.0)]),
            }],
            notes: vec!["<not markup>".into()],
        });
        let (html, _) = Telegram.render(&view);
        assert!(html.contains("Progress &amp; &lt;goals&gt;"), "got: {html}");
        assert!(html.contains("Row &lt;wide&gt; &amp; narrow"), "got: {html}");
        assert!(html.contains("A &amp; B"), "got: {html}");
        assert!(html.contains("- &lt;not markup&gt;"), "got: {html}");
    }

    fn programme(status: Option<gymbuddy_proto::ProgrammeStatusView>) -> View {
        View::Programme(Box::new(ProgrammeView {
            title: "6-week base".into(),
            start_date: "2026-07-01".into(),
            target_end_date: Some("2026-08-12".into()),
            weeks: 6,
            days_per_week: 2,
            split: "upper/lower".into(),
            progression_policy: "linear".into(),
            blocks: vec![],
            week_template: vec![],
            goals: vec![],
            notes: vec![],
            active: status.is_some(),
            status,
        }))
    }

    /// [R2.1]: a live programme leads with where the user is — position, what is due
    /// next, and the tally — before the skeleton they already know.
    #[test]
    fn programme_status_html_leads_with_the_position() {
        let (html, mode) = Telegram.render(&programme(Some(gymbuddy_proto::ProgrammeStatusView {
            current_week: 3,
            block_focus: Some("accumulation".into()),
            next_slot: Some(gymbuddy_proto::ProgrammeSlotView { week_idx: 3, day_idx: 1, focus: "upper".into() }),
            trained: 2,
            missed: 2,
            skipped: 0,
            remaining: 8,
        })));
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>6-week base</b>\n\
                        <i>6 weeks × 2 days/week, upper/lower</i>\n\
                        2026-07-01 to 2026-08-12\n\
                        Progression: linear\n\
                        \n<b>Where you are:</b>\n\
                        Week 3 of 6 — accumulation\n\
                        Next: Week 3, day 1: upper\n\
                        2 trained · 2 missed · 0 skipped · 8 to go\n";
        assert_eq!(html, expected);
    }

    /// A proposed programme has no position, so it renders exactly as it did before
    /// [R2.1] — and still asks to be locked in.
    #[test]
    fn a_programme_without_a_status_renders_no_position_section() {
        let (html, _) = Telegram.render(&programme(None));
        assert!(!html.contains("Where you are"), "a draft has nowhere to be: {html}");
        assert!(html.contains(PROGRAMME_LOCK_IN_ASK), "and is still awaiting confirmation: {html}");
    }

    /// A settled grid has nothing due, which must render as an omitted line rather than
    /// an empty "Next:".
    #[test]
    fn a_settled_grid_renders_no_next_line() {
        let (html, _) = Telegram.render(&programme(Some(gymbuddy_proto::ProgrammeStatusView {
            current_week: 6,
            block_focus: None,
            next_slot: None,
            trained: 10,
            missed: 2,
            skipped: 0,
            remaining: 0,
        })));
        assert!(html.contains("Week 6 of 6\n"), "no block means no focus suffix: {html}");
        assert!(!html.contains("Next:"), "nothing is due: {html}");
        assert!(html.contains("10 trained · 2 missed · 0 skipped · 0 to go"));
    }

    /// [C1.4]: an explicit one-off during an active programme says so, and says the
    /// programme is untouched.
    #[test]
    fn ad_hoc_roster_under_a_programme_says_it_is_untouched() {
        let view = View::ProgrammeSessionRoster {
            roster: one_exercise_roster(),
            mode: TrainingModeView::AdHoc { programme_title: "12-week hypertrophy".into() },
        };
        let (html, _) = Telegram.render(&view);
        assert!(html.contains("<i>Ad-hoc session — 12-week hypertrophy is untouched</i>\n"), "got: {html}");
    }
}

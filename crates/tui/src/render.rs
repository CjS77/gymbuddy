//! Render a domain [`View`] into ratatui lines — the TUI's own presentation
//! choices. Free to use the terminal's vertical space generously (this is where
//! the IRC-style transcript has more room than a Telegram bubble): coloured
//! headings, bullets, and space-aligned columns (no bordered `Table` widget,
//! which clashes with the flowing transcript).

use gymbuddy_proto::{
    CatalogView, ExerciseLog, HistoryView, PROGRAMME_LOCK_IN_ASK, ProgrammeView, ProgressView, SeriesShape, SeriesView, SessionRosterView,
    SetLine, StatusView, TrainingModeView, View,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const ACCENT: Color = Color::Cyan;
const SUCCESS: Color = Color::Green;
const WARNING: Color = Color::Yellow;
const ERROR: Color = Color::Red;
const MUTED: Color = Color::DarkGray;

/// Render a view into transcript lines (without the speaker prefix, which the
/// transcript adds).
pub fn render_view(view: &View) -> Vec<Line<'static>> {
    match view {
        View::Message { text, notes, failures } => render_message(text, notes, failures),
        View::Notice { text } => plain_lines(text),
        View::Status(status) => render_status(status),
        View::Catalog(catalog) => render_catalog(catalog),
        View::History(history) => render_history(history),
        View::SessionRoster(roster) => render_session_roster(roster, None),
        View::ProgrammeSessionRoster { roster, mode } => render_session_roster(roster, Some(mode)),
        View::Programme(programme) => render_programme(programme),
        View::Progress(progress) => render_progress(progress),
        View::Timers { enabled } => vec![Line::from(Span::styled(
            format!("Rest timers {}", if *enabled { "on" } else { "off" }),
            Style::default().fg(if *enabled { SUCCESS } else { MUTED }).add_modifier(Modifier::BOLD),
        ))],
        // `View` is `#[non_exhaustive]`: degrade gracefully on an unknown variant.
        _ => vec![Line::from(Span::styled("[unsupported message]", Style::default().fg(MUTED)))],
    }
}

fn render_message(text: &str, notes: &[String], failures: &[String]) -> Vec<Line<'static>> {
    let mut lines = plain_lines(text);
    for note in notes {
        lines.push(Line::from(""));
        for l in note.split('\n') {
            lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(MUTED))));
        }
    }
    for failure in failures {
        lines.push(Line::from(Span::styled(format!("⚠ {failure}"), Style::default().fg(ERROR))));
    }
    lines
}

fn render_status(status: &StatusView) -> Vec<Line<'static>> {
    let mut lines = vec![heading(format!("Status for {}", status.user_name))];

    match &status.session {
        Some(session) => {
            lines.push(muted(format!("Active session — started {}", session.started_at)));
            if session.completed.is_empty() && session.in_progress.is_empty() {
                lines.push(muted("  No exercises logged yet".into()));
            }
            if !session.completed.is_empty() {
                lines.push(bold("Completed"));
                lines.extend(session.completed.iter().map(|log| exercise_bullet(log, None)));
            }
            if session.in_progress.len() > 1 {
                lines.push(bold("Superset (in progress)"));
                lines.extend(session.in_progress.iter().enumerate().map(|(i, log)| exercise_bullet(log, Some(i + 1))));
            } else if let Some(log) = session.in_progress.first() {
                lines.push(bold("Current exercise"));
                lines.push(exercise_bullet(log, None));
            }
        }
        None => lines.push(muted("No active session".into())),
    }

    if !status.health.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Active health issues", Style::default().fg(WARNING).add_modifier(Modifier::BOLD))));
        for note in &status.health {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(WARNING)),
                Span::raw(format!("{} ({}): {}", note.kind, note.body_part, note.description)),
            ]));
        }
    }
    lines
}

fn render_catalog(catalog: &CatalogView) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for group in &catalog.groups {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(heading(group.muscle_group.clone()));

        let name_w = group.exercises.iter().map(|e| e.name.chars().count()).max().unwrap_or(4).max(4);
        let alias_w = group.exercises.iter().map(|e| e.aliases.chars().count()).max().unwrap_or(7).max(7);

        lines.push(Line::from(Span::styled(
            format!("  {:<name_w$}  {:<alias_w$}  Type", "Name", "Aliases"),
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        )));
        for entry in &group.exercises {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{:<name_w$}", entry.name), Style::default().fg(ACCENT)),
                Span::raw(format!("  {:<alias_w$}  {}", entry.aliases, entry.kind)),
            ]));
        }
    }
    lines
}

fn render_history(history: &HistoryView) -> Vec<Line<'static>> {
    let mut lines = vec![heading("Recent workouts".into())];
    if history.sessions.is_empty() {
        lines.push(muted("No workout history yet — tell me about an exercise!".into()));
        return lines;
    }
    // The TUI shows every session it is given; the server decides how many to send.
    for summary in &history.sessions {
        let duration = summary.minutes.map(|m| format!("  ({m} min)")).unwrap_or_default();
        let status_color = if summary.status == "done" { SUCCESS } else { WARNING };
        lines.push(Line::from(vec![
            Span::styled("  • ", Style::default().fg(MUTED)),
            Span::raw(format!("{}  ", summary.started_at)),
            Span::styled(format!("[{}]", summary.status), Style::default().fg(status_color)),
            Span::raw(format!("  {} entries{duration}", summary.entries)),
        ]));
    }
    lines
}

/// Render a designed session roster; `mode` (present only when a programme is
/// active, [C1.4]) adds one muted line under the title saying whether this fills
/// the programme's current slot or deliberately sidesteps it. `None` — ad-hoc with
/// no programme — keeps the pre-programme layout untouched.
fn render_session_roster(roster: &SessionRosterView, mode: Option<&TrainingModeView>) -> Vec<Line<'static>> {
    let mut lines = vec![heading(roster.title.clone())];

    if let Some(mode) = mode {
        lines.push(muted(mode.summary()));
    }

    if let Some(rationale) = &roster.rationale
        && !rationale.trim().is_empty()
    {
        lines.extend(rationale.split('\n').map(|l| muted(l.to_string())));
    }

    if !roster.exercises.is_empty() {
        lines.push(Line::from(""));
        for (i, exercise) in roster.exercises.iter().enumerate() {
            let target = exercise.target_line();
            let target_part = if target.is_empty() { String::new() } else { format!(" — {target}") };
            lines.push(Line::from(vec![
                Span::styled(format!("  {}. ", i + 1), Style::default().fg(MUTED)),
                Span::styled(exercise.name.clone(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::raw(target_part),
            ]));
            if let Some(cue) = &exercise.cue
                && !cue.trim().is_empty()
            {
                lines.push(muted(format!("     {cue}")));
            }
        }
    }

    if !roster.notes.is_empty() {
        lines.push(Line::from(""));
        lines.push(bold("Notes"));
        for note in &roster.notes {
            lines.push(Line::from(vec![Span::styled("  • ", Style::default().fg(MUTED)), Span::raw(note.clone())]));
        }
    }

    lines.push(Line::from(""));
    lines.push(muted("Nothing is logged yet — log your sets as you go and I'll adjust.".into()));
    lines
}

/// Render a designed programme ([C4.2]): its shape, the goals it serves, its blocks and
/// the repeating week. It holds no exercises by design — those arrive per session, when
/// `/nextworkout` designs a roster for a slot.
///
/// A draft closes with the lock-in ask; an active programme has nothing left to confirm.
fn render_programme(p: &ProgrammeView) -> Vec<Line<'static>> {
    let mut lines = vec![heading(p.title.clone()), muted(p.shape_line())];

    lines.push(muted(match &p.target_end_date {
        Some(end) => format!("{} to {end}", p.start_date),
        None => format!("from {}", p.start_date),
    }));
    lines.push(muted(format!("Progression: {}", p.progression_policy)));

    let section = |lines: &mut Vec<Line<'static>>, title: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        lines.push(Line::from(""));
        lines.push(bold(title));
        lines.extend(
            items.into_iter().map(|item| Line::from(vec![Span::styled("  • ", Style::default().fg(MUTED)), Span::raw(item)])),
        );
    };

    section(&mut lines, "Goals served", p.goals.clone());
    section(&mut lines, "Blocks", p.blocks.iter().map(|b| format!("{}: {}", b.weeks_label(), b.focus)).collect());
    section(&mut lines, "Each week", p.week_template.iter().map(|d| format!("Day {}: {}", d.day_idx, d.focus)).collect());
    section(&mut lines, "Notes", p.notes.clone());

    if !p.active {
        lines.push(Line::from(""));
        lines.push(muted(PROGRAMME_LOCK_IN_ASK.to_string()));
    }
    lines
}

/// How wide a [`SeriesShape::Breakdown`] bar may grow. The transcript has more room
/// than a Telegram bubble, but still shares the line with a label and a value.
const BAR_WIDTH: usize = 20;

/// Render progress ([C6.2]) as sparklines and text.
///
/// Real ratatui `Chart`/`Sparkline` widgets are [T2.2]; this shows the same series
/// those charts will be drawn from, with one rendering per *shape* rather than per
/// metric — a new Core metric of a known shape needs nothing here.
fn render_progress(p: &ProgressView) -> Vec<Line<'static>> {
    let mut lines = vec![heading(p.summary_line())];
    lines.extend(p.series.iter().flat_map(|s| std::iter::once(Line::from("")).chain(render_series(s))));

    if !p.notes.is_empty() {
        lines.push(Line::from(""));
        lines.push(bold("Notes"));
        lines.extend(
            p.notes.iter().map(|n| Line::from(vec![Span::styled("  • ", Style::default().fg(MUTED)), Span::raw(n.clone())])),
        );
    }
    lines
}

/// One series: its title, then whatever its shape calls for.
fn render_series(s: &SeriesView) -> Vec<Line<'static>> {
    let mut lines = vec![bold(&s.title)];
    match s.shape {
        SeriesShape::Breakdown => lines.extend(render_bars(s)),
        SeriesShape::Trend | SeriesShape::Trajectory { .. } => lines.extend(render_trend(s)),
    }
    lines
}

/// A time-ordered series as a sparkline plus its movement and target.
///
/// The sparkline plots the readings as recorded — a cut's bodyweight slopes down.
/// Whether that is progress is said in the movement line's words *and* its colour;
/// the words are what carry it, since a 16-colour terminal may render both verdicts
/// alike.
fn render_trend(s: &SeriesView) -> Vec<Line<'static>> {
    let Some(latest) = s.latest() else {
        return vec![muted("  No readings yet".into())];
    };

    let movement = match s.change_line() {
        // Too short to have moved — say where it stands rather than nothing.
        line if line.is_empty() => s.latest_line(),
        line => line,
    };
    let mut lines = vec![
        Line::from(vec![Span::raw("  "), Span::styled(s.spark(), Style::default().fg(ACCENT))]),
        Line::from(vec![Span::raw("  "), Span::styled(movement, Style::default().fg(verdict_colour(s)))]),
    ];

    if !s.target_line().is_empty() {
        lines.push(muted(format!("  {}", s.target_line())));
    }
    if let Some(first) = s.points.first()
        && first.label != latest.label
    {
        lines.push(muted(format!("  {} to {}", first.label, latest.label)));
    }
    lines
}

/// Independent buckets as proportional bars, scaled to the largest. A breakdown has
/// no time order, so no sparkline and no trend line.
fn render_bars(s: &SeriesView) -> Vec<Line<'static>> {
    let Some((_, max)) = s.bounds() else {
        return vec![muted("  No readings yet".into())];
    };
    let width = s.points.iter().map(|p| p.label.chars().count()).max().unwrap_or(0);

    s.points
        .iter()
        .map(|p| {
            let filled = if max <= 0.0 { 0 } else { ((p.value / max) * BAR_WIDTH as f64).round() as usize };
            Line::from(vec![
                Span::raw(format!("  {:<width$} ", p.label)),
                Span::styled("█".repeat(filled.min(BAR_WIDTH)), Style::default().fg(ACCENT)),
                Span::raw(format!(" {}", s.value_label(p.value))),
            ])
        })
        .collect()
}

/// The colour for a series' movement: its own [`SeriesView::improving`] verdict, never
/// the direction the line happens to run. A series with no verdict stays muted rather
/// than being guessed at.
fn verdict_colour(s: &SeriesView) -> Color {
    match s.improving() {
        Some(true) => SUCCESS,
        // A regression is information, not a failure — yellow, not red.
        Some(false) => WARNING,
        None => MUTED,
    }
}

fn exercise_bullet(log: &ExerciseLog, index: Option<usize>) -> Line<'static> {
    let bullet = match index {
        Some(i) => format!("  {i}. "),
        None => "  • ".to_string(),
    };
    let n = log.sets.len();
    let sets = log.sets.iter().map(SetLine::compact).collect::<Vec<_>>().join(", ");
    Line::from(vec![
        Span::styled(bullet, Style::default().fg(MUTED)),
        Span::styled(log.name.clone(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw(format!(" ({n} {}) — {sets}", if n == 1 { "set" } else { "sets" })),
    ])
}

fn plain_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n').map(|l| Line::from(Span::raw(l.to_string()))).collect()
}

fn heading(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
}

fn bold(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), Style::default().add_modifier(Modifier::BOLD)))
}

fn muted(text: String) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().fg(MUTED)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gymbuddy_proto::{CatalogEntry, CatalogGroup, ExerciseLog, HealthNote, Measurement, SessionView};

    fn flat(lines: &[Line]) -> String {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn message_renders_notes_and_failures() {
        let view = View::Message { text: "Logged it!".into(), notes: vec!["Nice streak".into()], failures: vec!["bad thing".into()] };
        let text = flat(&render_view(&view));
        assert!(text.contains("Logged it!"));
        assert!(text.contains("Nice streak"));
        assert!(text.contains("⚠ bad thing"));
    }

    #[test]
    fn status_renders_without_markup() {
        let view = View::Status(StatusView {
            user_name: "Alice".into(),
            session: Some(SessionView {
                started_at: "2026-06-16 10:00:00".into(),
                completed: vec![ExerciseLog {
                    name: "Bench Press".into(),
                    sets: vec![SetLine { measurement: Measurement::WeightReps, count: Some(8), value: 80.0 }],
                }],
                in_progress: vec![],
            }),
            health: vec![HealthNote { kind: "injury".into(), body_part: "shoulder".into(), description: "sore".into() }],
        });
        let text = flat(&render_view(&view));
        assert!(text.contains("Status for Alice"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("8×80kg"));
        assert!(text.contains("injury (shoulder): sore"));
        assert!(!text.contains('<'), "no HTML markup should appear: {text}");
    }

    #[test]
    fn session_roster_renders_the_prescription_without_markup() {
        use gymbuddy_proto::{RosterExerciseView, SessionRosterView};
        let view = View::SessionRoster(SessionRosterView {
            title: "Push focus".into(),
            rationale: Some("Bench was easy last time.".into()),
            exercises: vec![RosterExerciseView {
                name: "Bench Press".into(),
                target_sets: Some(3),
                target_reps: Some(6),
                target_weight_kg: Some(65.0),
                target_secs: None,
                cue: Some("Push the weight.".into()),
            }],
            notes: vec!["Skipped deadlift for your back.".into()],
        });
        let text = flat(&render_view(&view));
        assert!(text.contains("Push focus"));
        assert!(text.contains("Bench was easy last time."));
        assert!(text.contains("1. Bench Press — 3 sets × 6 reps @ 65kg"));
        assert!(text.contains("Push the weight."));
        assert!(text.contains("Skipped deadlift for your back."));
        assert!(!text.contains('<'), "no HTML markup should appear: {text}");
    }

    /// [C1.4]: a roster designed under an active programme carries its mode line;
    /// both modes name the programme so the user always knows what today counts for.
    #[test]
    fn programme_session_roster_renders_the_mode_line() {
        use gymbuddy_proto::SessionRosterView;
        let roster = SessionRosterView { title: "Upper".into(), rationale: None, exercises: vec![], notes: vec![] };

        let slot = View::ProgrammeSessionRoster {
            roster: roster.clone(),
            mode: TrainingModeView::Programme { programme_title: "12-week".into(), week: 2, day: 1, focus: "upper".into() },
        };
        assert!(flat(&render_view(&slot)).contains("Programme: 12-week — week 2, day 1: upper"));

        let ad_hoc = View::ProgrammeSessionRoster { roster, mode: TrainingModeView::AdHoc { programme_title: "12-week".into() } };
        assert!(flat(&render_view(&ad_hoc)).contains("Ad-hoc session — 12-week is untouched"));
    }

    fn series_points(raw: &[(&str, f64)]) -> Vec<gymbuddy_proto::SeriesPointView> {
        raw.iter().map(|(label, value)| gymbuddy_proto::SeriesPointView { label: (*label).into(), value: *value }).collect()
    }

    /// [C6.2]: a trend shows the sparkline, the movement, the target and the span.
    #[test]
    fn progress_renders_a_trend_with_its_target() {
        use gymbuddy_proto::{Direction, ProgressView, SeriesShape, SeriesView};
        let view = View::Progress(ProgressView {
            headline: "2 of 3 goals on track".into(),
            series: vec![SeriesView {
                title: "Bench Press".into(),
                unit: "kg".into(),
                better: Direction::Higher,
                shape: SeriesShape::Trajectory { target: 100.0 },
                points: series_points(&[("2026-05-01", 80.0), ("2026-06-01", 85.0), ("2026-07-01", 92.5)]),
            }],
            notes: vec!["Squat has too few sessions to trend.".into()],
        });
        let text = flat(&render_view(&view));
        assert!(text.contains("2 of 3 goals on track"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("▁▄█"));
        assert!(text.contains("80 → 92.5 kg (+12.5, better)"));
        assert!(text.contains("Target: 100 kg"));
        assert!(text.contains("2026-05-01 to 2026-07-01"));
        assert!(text.contains("Squat has too few sessions to trend."));
        assert!(!text.contains('<'), "no HTML markup should appear: {text}");
    }

    /// The direction-aware rule [T2.2] turns on: falling bodyweight on a cut is
    /// progress, so it is coloured as success — while the sparkline still slopes down,
    /// because that is what the user logged.
    #[test]
    fn a_falling_series_that_is_progress_is_not_coloured_as_a_regression() {
        use gymbuddy_proto::{Direction, ProgressView, SeriesShape, SeriesView};
        let series = SeriesView {
            title: "Bodyweight".into(),
            unit: "kg".into(),
            better: Direction::Lower,
            shape: SeriesShape::Trend,
            points: series_points(&[("2026-05-01", 90.0), ("2026-06-01", 87.5)]),
        };
        let lines = render_view(&View::Progress(ProgressView {
            headline: "Cutting".into(),
            series: vec![series.clone()],
            notes: vec![],
        }));
        let text = flat(&lines);
        assert!(text.contains("█▁"), "the readings are plotted as logged: {text}");
        assert!(text.contains("90 → 87.5 kg (-2.5, better)"), "got: {text}");

        let movement = lines.iter().find(|l| flat(std::slice::from_ref(l)).contains("→")).expect("a movement line");
        assert_eq!(movement.spans.last().unwrap().style.fg, Some(SUCCESS), "losing weight on a cut is progress, not a regression");

        // The same numbers with the opposite goal are a regression, and say so.
        let bulking = SeriesView { better: Direction::Higher, ..series };
        let text = flat(&render_view(&View::Progress(ProgressView {
            headline: "Bulking".into(),
            series: vec![bulking],
            notes: vec![],
        })));
        assert!(text.contains("(-2.5, worse)"), "got: {text}");
    }

    /// A breakdown is bars, not a trend — its buckets carry no time order.
    #[test]
    fn progress_renders_a_breakdown_as_bars() {
        use gymbuddy_proto::{Direction, ProgressView, SeriesShape, SeriesView};
        let view = View::Progress(ProgressView {
            headline: "This week".into(),
            series: vec![SeriesView {
                title: "Volume by group".into(),
                unit: "sets".into(),
                better: Direction::Neutral,
                shape: SeriesShape::Breakdown,
                points: series_points(&[("Chest", 12.0), ("Back", 16.0), ("Legs", 9.0)]),
            }],
            notes: vec![],
        });
        let text = flat(&render_view(&view));
        assert!(text.contains(&format!("Back  {} 16 sets", "█".repeat(20))), "the largest bucket fills the bar: {text}");
        assert!(text.contains(&format!("Chest {} 12 sets", "█".repeat(15))), "got: {text}");
        assert!(!text.contains('→'), "a breakdown has no trend to report: {text}");
    }

    #[test]
    fn catalog_columns_align() {
        let view = View::Catalog(CatalogView {
            groups: vec![CatalogGroup {
                muscle_group: "Chest".into(),
                exercises: vec![CatalogEntry { name: "Bench Press".into(), aliases: "bench".into(), kind: "weight_reps".into() }],
            }],
        });
        let lines = render_view(&view);
        let text = flat(&lines);
        assert!(text.contains("Chest"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("weight_reps"));
    }
}

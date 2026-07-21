//! Render a domain [`View`] into ratatui lines — the TUI's own presentation
//! choices. Free to use the terminal's vertical space generously (this is where
//! the IRC-style transcript has more room than a Telegram bubble): coloured
//! headings, bullets, and space-aligned columns (no bordered `Table` widget,
//! which clashes with the flowing transcript).
//!
//! Everything leaves here as `Line`s, including the progress charts: a `Chart` or
//! `BarChart` is drawn into an off-screen buffer and lifted back out (see
//! [`plot_lines`]). That is what lets a widget that wants a fixed `Rect` live in a
//! transcript that is a flat, scrollable run of text — and keeps every widget type on
//! this side of the wall, where `app.rs` never has to know about them ([T1.1]).

use gymbuddy_proto::{
    CatalogView, ExerciseLog, HistoryView, PROGRAMME_LOCK_IN_ASK, ProgrammeView, ProgressView, ReviewEffortView, ReviewExerciseView,
    ReviewRecordView, SeriesShape, SeriesView, SessionReviewView, SessionRosterView, SetLine, StatusView, TrainingModeView, View,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Bar, BarChart, Chart, Dataset, GraphType, Widget};

const ACCENT: Color = Color::Cyan;
const SUCCESS: Color = Color::Green;
const WARNING: Color = Color::Yellow;
const ERROR: Color = Color::Red;
const MUTED: Color = Color::DarkGray;

/// Render a view into transcript lines (without the speaker prefix, which the
/// transcript adds).
///
/// `width` is the room the transcript has for a line. The charts of [`View::Progress`]
/// and [`View::SessionReview`] use it, and they need it: a `Chart` is drawn into a fixed
/// area, and one drawn wider than the transcript would be folded by the `Paragraph`'s
/// wrap into something worse than no chart at all.
pub fn render_view(view: &View, width: u16) -> Vec<Line<'static>> {
    match view {
        View::Message { text, notes, failures } => render_message(text, notes, failures),
        View::Notice { text } => plain_lines(text),
        View::Status(status) => render_status(status),
        View::Catalog(catalog) => render_catalog(catalog),
        View::History(history) => render_history(history),
        View::SessionRoster(roster) => render_session_roster(roster, None),
        View::ProgrammeSessionRoster { roster, mode } => render_session_roster(roster, Some(mode)),
        View::Programme(programme) => render_programme(programme),
        View::Progress(progress) => render_progress(progress, width),
        View::SessionReview(review) => render_session_review(review, width),
        // [C4.6] lands as its headline until [T2.1] gives it a layout: a programme is
        // long-lived context, and whether it belongs in the transcript or beside it in the
        // persistent frame is that ticket's call.
        View::ProgrammeProgress(report) => plain_lines(&report.summary_line()),
        View::Timers { enabled } => vec![Line::from(Span::styled(
            format!("Rest timers {}", if *enabled { "on" } else { "off" }),
            Style::default().fg(if *enabled { SUCCESS } else { MUTED }).add_modifier(Modifier::BOLD),
        ))],
        // `View` is `#[non_exhaustive]`: degrade gracefully on an unknown variant.
        _ => vec![Line::from(Span::styled("[unsupported message]", Style::default().fg(MUTED)))],
    }
}

/// The post-session review ([C6.5]) — what the session was, how it measured up to the
/// prescription, what it moved.
///
/// Arrives unprompted when a session ends (including one auto-closed by
/// `close_stale_session`), not as a reply to a submitted line, so it stands entirely on
/// the [`SessionReviewView`] it is handed and assumes no preceding user turn.
///
/// The verdict is Core's ([C6.5]) and is rendered as given — a "Partial session"
/// headline is not softened here. Achieved goals lead, because finishing a goal is the
/// one thing worth interrupting for. Charts route through the same [T2.2] helpers
/// `/progress` uses, degrading to text where the width or the point count is too small.
fn render_session_review(r: &SessionReviewView, width: u16) -> Vec<Line<'static>> {
    let mut lines = review_header(r);
    lines.extend(review_achievements(r));
    lines.extend(review_exercises(r));
    lines.extend(review_records(r));
    lines.extend(review_commentary(r));
    lines.extend(bullet_section("Goals", &r.goals));
    lines.extend(review_series(r, width));
    lines.extend(review_footer(r));
    lines.extend(bullet_section("Notes", &r.notes));
    lines
}

/// The headline verdict, then the session's date and — when it had them — its programme
/// slot and the intent the user set out with, as muted context beneath.
fn review_header(r: &SessionReviewView) -> Vec<Line<'static>> {
    let mut lines = vec![heading(r.summary_line()), muted(r.session_date.clone())];
    lines.extend(r.position.iter().map(|p| muted(p.summary())));
    lines.extend(r.intent.iter().map(|i| muted(format!("Aiming for {i}"))));
    lines
}

/// Goals finished this session — led with, and coloured as the win they are. The word
/// "reached" carries the verdict so a colourless terminal still shows it.
fn review_achievements(r: &SessionReviewView) -> Vec<Line<'static>> {
    if r.achieved_goals.is_empty() {
        return Vec::new();
    }
    let win = Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD);
    std::iter::once(Line::from(""))
        .chain(r.achieved_goals.iter().map(|g| Line::from(Span::styled(format!("Goal reached: {g}"), win))))
        .collect()
}

/// The exercises as performed against what was asked. The delta is Core's verbatim text
/// — a bare string with no direction attached — so it is shown as given and never
/// recoloured by a guess at whether, say, "-2.5kg" was progress.
fn review_exercises(r: &SessionReviewView) -> Vec<Line<'static>> {
    if r.exercises.is_empty() {
        return Vec::new();
    }
    std::iter::once(Line::from(""))
        .chain(std::iter::once(bold("Exercises")))
        .chain(r.exercises.iter().map(review_exercise_line))
        .collect()
}

/// One exercise line with its name in the accent used for exercises everywhere else,
/// the rest of [`ReviewExerciseView::line`] following verbatim.
fn review_exercise_line(e: &ReviewExerciseView) -> Line<'static> {
    named_line("  ", &e.name, MUTED, e.line())
}

/// Personal records set this session. A record is by definition an improvement, so the
/// "record" framing carries the good news and the success-coloured marker underlines it.
fn review_records(r: &SessionReviewView) -> Vec<Line<'static>> {
    if r.records.is_empty() {
        return Vec::new();
    }
    std::iter::once(Line::from(""))
        .chain(std::iter::once(bold("Personal records")))
        .chain(r.records.iter().map(review_record_line))
        .collect()
}

/// One record line, marked with a success-coloured bullet and the exercise name accented.
fn review_record_line(record: &ReviewRecordView) -> Line<'static> {
    named_line("  • ", &record.exercise, SUCCESS, record.line())
}

/// A `<marker><name>: <rest>` line where the name takes the exercise accent, reusing the
/// proto helper's own text for everything after the name. `line` is expected to start
/// `"<name>: "`; anything else is rendered whole, indented, rather than mis-split.
fn named_line(marker: &str, name: &str, marker_colour: Color, line: String) -> Line<'static> {
    match line.strip_prefix(&format!("{name}: ")) {
        Some(rest) => Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(marker_colour)),
            Span::styled(name.to_string(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(format!(": {rest}")),
        ]),
        None => Line::from(Span::raw(format!("{marker}{line}"))),
    }
}

/// The grounded commentary ([C6.5], the programme tier only). Core's honest assessment,
/// rendered verbatim — never softened, never reflowed.
fn review_commentary(r: &SessionReviewView) -> Vec<Line<'static>> {
    match &r.commentary {
        Some(c) => std::iter::once(Line::from("")).chain(plain_lines(c)).collect(),
        None => Vec::new(),
    }
}

/// The charts behind the review, plotted with the same [T2.2] helpers `/progress` uses —
/// the [C6.2] promise that every chart travels one path. Each degrades to a sparkline or
/// a bare movement line exactly as [`render_series`] does when there is no room for axes.
fn review_series(r: &SessionReviewView, width: u16) -> Vec<Line<'static>> {
    r.series.iter().flat_map(|s| std::iter::once(Line::from("")).chain(render_series(s, width))).collect()
}

/// The session's summary stats as muted lines under the detail: how it tracked against
/// the prescription, how hard it was, the streak it continued, and the week so far.
fn review_footer(r: &SessionReviewView) -> Vec<Line<'static>> {
    let stats: Vec<String> = r
        .adherence
        .clone()
        .into_iter()
        .chain(r.effort.iter().map(ReviewEffortView::line))
        .chain(r.streak_days.map(|n| format!("{n}-day training streak")))
        .chain(r.week_line.iter().map(|w| format!("This week: {w}")))
        .collect();
    if stats.is_empty() {
        return Vec::new();
    }
    std::iter::once(Line::from("")).chain(stats.into_iter().map(muted)).collect()
}

/// A bulleted section — a bold title over `  • item` lines, led by a blank line — or
/// nothing at all when there is nothing to list.
fn bullet_section(title: &str, items: &[String]) -> Vec<Line<'static>> {
    if items.is_empty() {
        return Vec::new();
    }
    std::iter::once(Line::from(""))
        .chain(std::iter::once(bold(title)))
        .chain(items.iter().map(|item| Line::from(vec![Span::styled("  • ", Style::default().fg(MUTED)), Span::raw(item.clone())])))
        .collect()
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
/// The "where you are" bullets of a live programme, or empty for one that is merely
/// proposed — which renders as no section at all, the same as any other empty list.
fn position_items(p: &ProgrammeView) -> Vec<String> {
    let (Some(position), Some(status)) = (p.position_line(), p.status.as_ref()) else {
        return Vec::new();
    };
    let next = status.next_slot.as_ref().map(|slot| format!("Next: {}", slot.label()));
    [position].into_iter().chain(next).chain([status.counts_line()]).collect()
}

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

    // [R2.1]: present only on a live programme being reported on, and rendered first,
    // because "where am I?" is the whole question `/programme status` was asked.
    section(&mut lines, "Where you are", position_items(p));
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

/// Charts are indented to sit under the series title, like every other detail line.
const INDENT: &str = "  ";

/// Rows a trend [`Chart`] occupies: the plot, plus the x-axis and its labels.
const CHART_HEIGHT: u16 = 9;

/// Below this the y-axis labels and the two date labels leave no plot worth drawing,
/// and the sparkline says more per column than a squeezed chart does.
const MIN_CHART_WIDTH: u16 = 32;

/// The integer domain [`Bar`] values are scaled into. `Bar` counts in `u64` and a
/// reading is an `f64`, so bar *lengths* are scaled integers, finely enough that the
/// rounding never reaches a column. The reading itself is printed verbatim beside the
/// bar: the bar is the comparison, the text is the number, and neither is rounded into
/// the other.
const BAR_TICKS: u64 = 10_000;

/// Render progress ([C6.2]) as ratatui charts and text ([T2.2]).
///
/// One rendering per *shape* rather than per metric — a new Core metric of a known
/// shape needs nothing here.
fn render_progress(p: &ProgressView, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![heading(p.summary_line())];
    lines.extend(p.series.iter().flat_map(|s| std::iter::once(Line::from("")).chain(render_series(s, width))));

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
fn render_series(s: &SeriesView, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![bold(&s.title)];
    match s.shape {
        SeriesShape::Breakdown => lines.extend(render_bars(s, width)),
        SeriesShape::Trend | SeriesShape::Trajectory { .. } => lines.extend(render_trend(s, width)),
    }
    lines
}

/// A time-ordered series as a plot plus its movement and target.
///
/// The plot draws the readings as recorded — a cut's bodyweight slopes down. Whether
/// that is progress is said in the movement line's words *and* its colour; the words
/// are what carry it, since a 16-colour terminal may render both verdicts alike.
fn render_trend(s: &SeriesView, width: u16) -> Vec<Line<'static>> {
    let Some(latest) = s.latest() else {
        return vec![muted("  No readings yet".into())];
    };

    let movement = match s.change_line() {
        // Too short to have moved — say where it stands rather than nothing.
        line if line.is_empty() => s.latest_line(),
        line => line,
    };
    let mut lines = trend_plot(s, width);
    lines.push(Line::from(vec![Span::raw(INDENT), Span::styled(movement, Style::default().fg(verdict_colour(s)))]));

    if !s.target_line().is_empty() {
        lines.push(muted(format!("{INDENT}{}", s.target_line())));
    }
    // A chart names both ends on its x-axis already; only the sparkline needs telling
    // what span it covers.
    if !is_charted(s, width)
        && let Some(first) = s.points.first()
        && first.label != latest.label
    {
        lines.push(muted(format!("{INDENT}{} to {}", first.label, latest.label)));
    }
    lines
}

/// Whether a series gets a real [`Chart`] or falls back to the sparkline. Two readings
/// chart as a bare line segment that says less than the numbers under it, and a narrow
/// transcript has no columns to spare for axes.
fn is_charted(s: &SeriesView, width: u16) -> bool {
    plot_width(width) >= MIN_CHART_WIDTH && s.points.len() >= 3
}

/// The readings as a [`Chart`], or as a sparkline where a chart would not fit.
fn trend_plot(s: &SeriesView, width: u16) -> Vec<Line<'static>> {
    match s.bounds().filter(|_| is_charted(s, width)) {
        Some((min, max)) => trend_chart(s, min, max, width),
        None => vec![Line::from(vec![Span::raw(INDENT), Span::styled(s.spark(), Style::default().fg(ACCENT))])],
    }
}

/// The readings as a braille line against a labelled axis pair.
///
/// A [`SeriesShape::Trajectory`] adds its target as a flat reference line drawn in a
/// different *marker* — dots against braille — so the two are told apart by shape
/// rather than by a hue the terminal may not have. The y-axis takes
/// [`SeriesView::bounds`] as given: it has already been widened to hold that target,
/// which is what keeps the reference line on the chart.
fn trend_chart(s: &SeriesView, min: f64, max: f64, width: u16) -> Vec<Line<'static>> {
    let (low, high) = pad(min, max);
    let last_x = (s.points.len() - 1) as f64;
    let readings: Vec<(f64, f64)> = s.points.iter().enumerate().map(|(i, p)| (i as f64, p.value)).collect();
    let target: Vec<(f64, f64)> = match s.shape {
        SeriesShape::Trajectory { target } => vec![(0.0, target), (last_x, target)],
        _ => Vec::new(),
    };

    let readings_set = Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(verdict_colour(s)))
        .data(&readings);
    // The target goes in first so the readings draw over it where they meet.
    let datasets = match target.is_empty() {
        true => vec![readings_set],
        false => vec![
            Dataset::default().marker(Marker::Dot).graph_type(GraphType::Line).style(Style::default().fg(MUTED)).data(&target),
            readings_set,
        ],
    };

    let axis = Style::default().fg(MUTED);
    let chart = Chart::new(datasets)
        .x_axis(Axis::default().style(axis).bounds([0.0, last_x]).labels(span_labels(s)))
        .y_axis(Axis::default().style(axis).bounds([low, high]).labels([s.value_label(low), s.value_label(high)]));
    plot_lines(chart, plot_width(width), CHART_HEIGHT)
}

/// The first and last point labels — the dates the x-axis runs between.
fn span_labels(s: &SeriesView) -> Vec<String> {
    match (s.points.first(), s.points.last()) {
        (Some(first), Some(last)) => vec![first.label.clone(), last.label.clone()],
        _ => Vec::new(),
    }
}

/// Give a flat range some height. A series that never moved has `min == max`, and a
/// zero-tall axis plots every reading on the same row as the axis itself.
fn pad(min: f64, max: f64) -> (f64, f64) {
    match (max - min).abs() < f64::EPSILON {
        true => (min - 1.0, max + 1.0),
        false => (min, max),
    }
}

/// Independent buckets as a horizontal [`BarChart`], the largest filling the width the
/// transcript actually has. A breakdown has no time order, so no trend line and no
/// direction to have a verdict about.
fn render_bars(s: &SeriesView, width: u16) -> Vec<Line<'static>> {
    let Some((_, max)) = s.bounds() else {
        return vec![muted("  No readings yet".into())];
    };
    let bars: Vec<Bar<'static>> = s
        .points
        .iter()
        .zip(bucket_labels(s))
        .map(|(p, label)| {
            Bar::default()
                .label(Line::from(label))
                .value(ticks(p.value, max))
                // Emptied deliberately: `BarChart` draws a bar's own value *over* the
                // bar, which eats the short buckets it matters most to see. The
                // reading is in the label instead, where every bar starts after it.
                .text_value(String::new())
                .style(Style::default().fg(ACCENT))
        })
        .collect();
    let chart = BarChart::horizontal(bars).bar_width(1).bar_gap(0).max(BAR_TICKS);
    plot_lines(chart, plot_width(width), s.points.len() as u16)
}

/// `Chest      12 sets` — bucket name and reading in two aligned columns, which
/// `BarChart` pads to a common width and starts every bar after.
fn bucket_labels(s: &SeriesView) -> Vec<String> {
    let name_w = s.points.iter().map(|p| p.label.chars().count()).max().unwrap_or(0);
    let value_w = s.points.iter().map(|p| s.value_label(p.value).chars().count()).max().unwrap_or(0);
    s.points.iter().map(|p| format!("{:<name_w$} {:>value_w$}", p.label, s.value_label(p.value))).collect()
}

/// A reading as a share of the largest bucket, in [`BAR_TICKS`]. A negative reading has
/// no bar length to speak of, so it keeps its number and loses its bar rather than
/// being clamped into a misleading stub.
fn ticks(value: f64, max: f64) -> u64 {
    match max > 0.0 {
        true => ((value.max(0.0) / max) * BAR_TICKS as f64).round() as u64,
        false => 0,
    }
}

/// Columns a plot has once the transcript's indent is taken out.
fn plot_width(width: u16) -> u16 {
    width.saturating_sub(INDENT.len() as u16)
}

/// The transcript is a flowing `Vec<Line>`, but `Chart` and `BarChart` want a fixed
/// `Rect`. Hand them one off-screen and lift the result back into lines: the widgets
/// get their area and their axes, the transcript keeps the flat run of text it scrolls
/// and measures, and `app.rs` still never sees a widget type ([T1.1]).
fn plot_lines<W: Widget>(widget: W, width: u16, height: u16) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    widget.render(area, &mut buffer);
    (0..height).map(|y| buffer_row(&buffer, y, width)).collect()
}

/// One row of a rendered buffer as an indented `Line`, runs of a common style merged
/// into single spans.
fn buffer_row(buffer: &Buffer, y: u16, width: u16) -> Line<'static> {
    let runs = (0..width).filter_map(|x| buffer.cell((x, y))).fold(Vec::<(String, Style)>::new(), |mut runs, cell| {
        match runs.last_mut() {
            Some((text, style)) if *style == cell.style() => text.push_str(cell.symbol()),
            _ => runs.push((cell.symbol().to_string(), cell.style())),
        }
        runs
    });
    Line::from(
        std::iter::once(Span::raw(INDENT))
            .chain(trim_trailing(runs).into_iter().map(|(text, style)| Span::styled(text, style)))
            .collect::<Vec<_>>(),
    )
}

/// Drop the blank cells a widget leaves at the end of a row. They are invisible but
/// not free: the transcript wraps on a line's width, so padding every chart row out to
/// the full width would cost a wrapped row per chart row on the narrowest terminals.
fn trim_trailing(mut runs: Vec<(String, Style)>) -> Vec<(String, Style)> {
    let kept = runs.iter().rposition(|(text, _)| !text.trim_end().is_empty()).map_or(0, |i| i + 1);
    runs.truncate(kept);
    if let Some((text, _)) = runs.last_mut() {
        text.truncate(text.trim_end().len());
    }
    runs
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

    /// A comfortable transcript width — wide enough that a chartable series charts.
    const WIDE: u16 = 72;

    fn render(view: &View) -> Vec<Line<'static>> {
        render_view(view, WIDE)
    }
    /// The core of a [C6.5] review reads through end to end: the headline, an achieved
    /// goal, the grounded commentary, the per-exercise delta and the derived-effort line.
    #[test]
    fn session_review_renders_its_headline_and_exercise_lines() {
        use gymbuddy_proto::{ReviewEffortView, ReviewExerciseView, ReviewKindView, SessionReviewView};
        let view = View::SessionReview(Box::new(SessionReviewView {
            headline: "Solid session — 2 of 3 prescribed exercises completed".into(),
            kind: ReviewKindView::Report,
            session_date: "2026-07-19 10:00:00".into(),
            intent: None,
            effort: Some(ReviewEffortView { label: "hard".into(), confirmed: false }),
            exercises: vec![ReviewExerciseView {
                name: "Bench Press".into(),
                prescribed: Some("3 sets × 6 reps @ 65kg".into()),
                actual: "3 sets × 6 reps @ 67.5kg".into(),
                delta: Some("+2.5kg".into()),
            }],
            records: vec![],
            commentary: Some("The extra load held all three sets.".into()),
            goals: vec![],
            achieved_goals: vec!["Overhead Press to 40".into()],
            position: None,
            adherence: Some("2 of 3 prescribed exercises completed".into()),
            streak_days: Some(4),
            week_line: None,
            series: vec![],
            notes: vec![],
        }));
        let text = flat(&render(&view));
        assert!(text.contains("Solid session — 2 of 3 prescribed exercises completed"), "{text}");
        assert!(text.contains("Goal reached: Overhead Press to 40"), "{text}");
        assert!(text.contains("The extra load held all three sets."), "{text}");
        assert!(text.contains("Bench Press: 3 sets × 6 reps @ 67.5kg (asked 3 sets × 6 reps @ 65kg) — +2.5kg"), "{text}");
        assert!(text.contains("Effort: hard (my read, not yours)"), "{text}");
        assert!(!text.contains("[unsupported message]"), "the variant must not fall through: {text}");
        assert!(!text.contains('<'), "no HTML markup should appear: {text}");
    }

    /// A fuller programme-tier review — the shape a completed session with a bound
    /// roster produces, carrying every optional field so the render exercises each.
    fn full_review() -> SessionReviewView {
        use gymbuddy_proto::{ReviewEffortView, ReviewExerciseView, ReviewKindView, ReviewRecordView};
        SessionReviewView {
            headline: "Strong session — every lift at or above target".into(),
            kind: ReviewKindView::Report,
            session_date: "2026-07-19".into(),
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
                detail: "67.5 kg × 6".into(),
                previous: Some("65 kg × 6".into()),
            }],
            commentary: Some("The extra 2.5kg held for all three sets, which is the signal to keep the load.".into()),
            goals: vec!["Bench Press to 100kg: 67.5kg (68%)".into()],
            achieved_goals: vec!["Overhead Press to 40kg".into()],
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
            notes: vec!["Squat has too few sessions to trend yet.".into()],
        }
    }

    /// Every section of a full programme-tier review lands: the header context, the
    /// achievement, the exercise and record detail, the commentary, the goal line, the
    /// footer stats and the caveat — and none of it as markup.
    #[test]
    fn session_review_renders_every_section() {
        let text = flat(&render(&View::SessionReview(Box::new(full_review()))));
        assert!(text.contains("Strong session — every lift at or above target"), "the headline: {text}");
        assert!(text.contains("2026-07-19"), "the session date: {text}");
        assert!(text.contains("Programme: 12-week hypertrophy — week 2, day 1: upper"), "the slot it filled: {text}");
        assert!(text.contains("Aiming for upper push"), "the intent: {text}");
        assert!(text.contains("Goal reached: Overhead Press to 40kg"), "the achievement: {text}");
        assert!(text.contains("Exercises"), "the exercises heading: {text}");
        assert!(text.contains("Bench Press: 3 sets × 6 reps @ 67.5kg (asked 3 sets × 6 reps @ 65kg) — +2.5kg"), "the delta: {text}");
        assert!(text.contains("Personal records"), "the records heading: {text}");
        assert!(text.contains("Bench Press: 67.5 kg × 6 (was 65 kg × 6)"), "the record: {text}");
        assert!(text.contains("The extra 2.5kg held for all three sets"), "the commentary: {text}");
        assert!(text.contains("Bench Press to 100kg: 67.5kg (68%)"), "the goal line: {text}");
        assert!(text.contains("1 of 1 prescribed exercises completed"), "adherence: {text}");
        assert!(text.contains("Effort: hard"), "effort: {text}");
        assert!(text.contains("4-day training streak"), "the streak: {text}");
        assert!(text.contains("This week: 3 sessions, 12400 kg total volume"), "the week line: {text}");
        assert!(text.contains("Squat has too few sessions to trend yet."), "the caveat: {text}");
        assert!(!text.contains('<'), "no HTML markup should appear: {text}");
    }

    /// Achievements lead: finishing a goal is the one thing worth interrupting for, so it
    /// renders above the exercise detail. Colour marks it, but the word "reached" carries
    /// the verdict for a terminal that renders the green away.
    #[test]
    fn session_review_leads_with_its_achievements() {
        let lines = render(&View::SessionReview(Box::new(full_review())));
        let goal = lines.iter().position(|l| flat(std::slice::from_ref(l)).contains("Goal reached")).expect("an achievement line");
        let exercise = lines.iter().position(|l| flat(std::slice::from_ref(l)).contains("Bench Press")).expect("an exercise line");
        assert!(goal < exercise, "the achievement leads the exercise detail: {goal} vs {exercise}");
        assert_eq!(lines[goal].spans.last().unwrap().style.fg, Some(SUCCESS), "a reached goal is coloured as the win it is");
    }

    /// The verdict is Core's, and a blunt one is not softened in the render — a partial
    /// session says so, verbatim.
    #[test]
    fn session_review_does_not_soften_a_blunt_verdict() {
        let blunt = SessionReviewView {
            headline: "Partial session — 1 of 3 prescribed exercises completed".into(),
            achieved_goals: vec![],
            ..full_review()
        };
        let text = flat(&render(&View::SessionReview(Box::new(blunt))));
        assert!(text.contains("Partial session — 1 of 3 prescribed exercises completed"), "the honest headline stands: {text}");
    }

    /// Composes with [T2.2]: a review carrying a chartable series plots it through the
    /// same helpers `/progress` uses — axes where there is room, a sparkline where there
    /// is not — rather than growing a second chart path.
    #[test]
    fn session_review_composes_charts_and_degrades_to_text() {
        use gymbuddy_proto::{Direction, SeriesShape, SeriesView};
        let series = SeriesView {
            title: "Bench Press".into(),
            unit: "kg".into(),
            better: Direction::Higher,
            shape: SeriesShape::Trajectory { target: 100.0 },
            points: series_points(&[("2026-05-01", 80.0), ("2026-06-01", 85.0), ("2026-07-01", 92.5)]),
        };
        let view = View::SessionReview(Box::new(SessionReviewView { series: vec![series], ..full_review() }));

        let wide = flat(&render_view(&view, WIDE));
        assert!(wide.contains("80 → 92.5 kg (+12.5, better)"), "the movement, direction-aware: {wide}");
        assert!(wide.contains('│') && wide.contains('└'), "a wide review charts the series: {wide}");
        assert!(wide.contains("Target: 100 kg"), "the trajectory target is named: {wide}");

        let narrow = flat(&render_view(&view, 20));
        assert!(narrow.contains("▁▄█"), "a narrow review degrades to a sparkline: {narrow}");
        assert!(!narrow.contains('│'), "no room for axes at 20 columns: {narrow}");
    }

    /// The timeout path ([C6.5]/`close_stale_session`): a review arrives unprompted, not
    /// as a reply to a submitted line. It stands on its view alone — no intent, no
    /// commentary, a derived effort it admits to guessing — and still renders in full.
    #[test]
    fn session_review_from_a_timeout_renders_without_a_user_line() {
        use gymbuddy_proto::{ReviewEffortView, ReviewExerciseView, ReviewKindView};
        let auto_closed = SessionReviewView {
            headline: "Session logged — 1 exercise".into(),
            kind: ReviewKindView::Summary,
            intent: None,
            commentary: None,
            position: None,
            adherence: None,
            achieved_goals: vec![],
            records: vec![],
            goals: vec![],
            series: vec![],
            notes: vec![],
            effort: Some(ReviewEffortView { label: "hard".into(), confirmed: false }),
            exercises: vec![ReviewExerciseView {
                name: "Bench Press".into(),
                prescribed: None,
                actual: "3 sets × 6 reps @ 67.5kg".into(),
                delta: None,
            }],
            ..full_review()
        };
        let text = flat(&render(&View::SessionReview(Box::new(auto_closed))));
        assert!(text.contains("Session logged — 1 exercise"), "the headline: {text}");
        assert!(text.contains("Bench Press: 3 sets × 6 reps @ 67.5kg"), "the unrostered exercise: {text}");
        assert!(text.contains("Effort: hard (my read, not yours)"), "a derived effort admits it is a guess: {text}");
        assert!(!text.contains("[unsupported message]"), "the variant must not fall through: {text}");
    }

    fn flat(lines: &[Line]) -> String {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn message_renders_notes_and_failures() {
        let view = View::Message { text: "Logged it!".into(), notes: vec!["Nice streak".into()], failures: vec!["bad thing".into()] };
        let text = flat(&render(&view));
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
        let text = flat(&render(&view));
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
        let text = flat(&render(&view));
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
        assert!(flat(&render(&slot)).contains("Programme: 12-week — week 2, day 1: upper"));

        let ad_hoc = View::ProgrammeSessionRoster { roster, mode: TrainingModeView::AdHoc { programme_title: "12-week".into() } };
        assert!(flat(&render(&ad_hoc)).contains("Ad-hoc session — 12-week is untouched"));
    }

    fn series_points(raw: &[(&str, f64)]) -> Vec<gymbuddy_proto::SeriesPointView> {
        raw.iter().map(|(label, value)| gymbuddy_proto::SeriesPointView { label: (*label).into(), value: *value }).collect()
    }

    /// A bench press climbing towards a 100 kg goal — enough readings to chart. Held
    /// note-free so tests about chart geometry are not measuring wrappable prose.
    fn trajectory_view() -> View {
        use gymbuddy_proto::{Direction, ProgressView, SeriesShape, SeriesView};
        View::Progress(ProgressView {
            headline: "2 of 3 goals on track".into(),
            series: vec![SeriesView {
                title: "Bench Press".into(),
                unit: "kg".into(),
                better: Direction::Higher,
                shape: SeriesShape::Trajectory { target: 100.0 },
                points: series_points(&[("2026-05-01", 80.0), ("2026-06-01", 85.0), ("2026-07-01", 92.5)]),
            }],
            notes: vec![],
            goals: vec![],
        })
    }

    /// Whether any *chart* row carries the dotted reference marker. Scoped to rows with
    /// an axis on them: `•` is also the bullet the notes list uses.
    fn has_reference_line(view: &View) -> bool {
        render_view(view, WIDE).iter().map(|line| flat(std::slice::from_ref(line))).any(|row| row.contains('│') && row.contains('•'))
    }

    /// A trajectory: the readings charted against axes, its movement, and its target.
    #[test]
    fn progress_charts_a_trajectory_with_its_target() {
        let View::Progress(mut progress) = trajectory_view() else { panic!("a progress view") };
        progress.notes = vec!["Squat has too few sessions to trend.".into()];
        let text = flat(&render(&View::Progress(progress)));
        assert!(text.contains("2 of 3 goals on track"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("80 → 92.5 kg (+12.5, better)"));
        assert!(text.contains("Target: 100 kg"));
        assert!(text.contains("Squat has too few sessions to trend."));
        // The axes: the span across the bottom, and a y-axis widened by `bounds()` to
        // hold the target so the reference line cannot fall off the chart.
        assert!(text.contains("2026-05-01"), "the x-axis names where the series starts: {text}");
        assert!(text.contains("2026-07-01"), "the x-axis names where it ends: {text}");
        assert!(text.contains("100 kg"), "the y-axis reaches the target, not just the readings: {text}");
        assert!(text.contains('│') && text.contains('└'), "a chart has axes: {text}");
        assert!(!text.contains('<'), "no HTML markup should appear: {text}");
    }

    /// Colour is not load-bearing: the target reference line is told from the readings
    /// by its *marker* — dots against braille — which survives a 16-colour terminal
    /// where cyan and dark grey may come out alike.
    #[test]
    fn a_target_is_a_reference_line_in_its_own_marker() {
        use gymbuddy_proto::SeriesShape;
        let View::Progress(mut progress) = trajectory_view() else { panic!("a progress view") };
        assert!(has_reference_line(&View::Progress(progress.clone())), "the target is plotted, not just named");

        // The same readings with no target have nothing to draw a reference line for.
        progress.series[0].shape = SeriesShape::Trend;
        let trend = View::Progress(progress);
        assert!(!has_reference_line(&trend), "a plain trend has no reference line");
        assert!(flat(&render(&trend)).contains('│'), "it is still charted");
    }

    /// A chart is drawn into a fixed area, so it must be told the transcript's width:
    /// one drawn wider would be folded by the `Paragraph`'s wrap into nonsense.
    #[test]
    fn a_chart_never_outruns_the_width_it_is_given() {
        let view = trajectory_view();
        for width in [34u16, 40, 60, 72, 120] {
            let widest = render_view(&view, width).iter().map(Line::width).max().unwrap_or(0);
            assert!(widest <= width as usize, "a {widest}-wide line in a {width}-wide transcript");
        }
    }

    /// Under the width axes need, the sparkline says more per column than a squeezed
    /// chart does — and it carries the span the x-axis would otherwise have named.
    #[test]
    fn a_narrow_transcript_falls_back_to_the_sparkline() {
        let text = flat(&render_view(&trajectory_view(), 20));
        assert!(text.contains("▁▄█"), "got: {text}");
        assert!(!text.contains('│'), "no room for axes: {text}");
        assert!(text.contains("2026-05-01 to 2026-07-01"), "the span is said in words instead: {text}");
        assert!(text.contains("Target: 100 kg"), "the target is still named: {text}");
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
        let lines = render(&View::Progress(ProgressView {
            headline: "Cutting".into(),
            series: vec![series.clone()],
            notes: vec![],
            goals: vec![],
        }));
        let text = flat(&lines);
        assert!(text.contains("█▁"), "the readings are plotted as logged: {text}");
        assert!(text.contains("90 → 87.5 kg (-2.5, better)"), "got: {text}");

        let movement = lines.iter().find(|l| flat(std::slice::from_ref(l)).contains("→")).expect("a movement line");
        assert_eq!(movement.spans.last().unwrap().style.fg, Some(SUCCESS), "losing weight on a cut is progress, not a regression");

        // The same numbers with the opposite goal are a regression, and say so.
        let bulking = SeriesView { better: Direction::Higher, ..series };
        let text = flat(&render(&View::Progress(ProgressView {
            headline: "Bulking".into(),
            series: vec![bulking],
            notes: vec![],
            goals: vec![],
        })));
        assert!(text.contains("(-2.5, worse)"), "got: {text}");
    }

    /// The same rule inside a chart: the plotted line slopes the way the readings run,
    /// and takes its colour from the verdict rather than from that slope. Flipping the
    /// geometry would mean drawing the user data they never recorded.
    #[test]
    fn a_charted_series_is_coloured_by_its_verdict_not_its_slope() {
        use gymbuddy_proto::{Direction, ProgressView, SeriesShape, SeriesView};
        let cutting = SeriesView {
            title: "Bodyweight".into(),
            unit: "kg".into(),
            better: Direction::Lower,
            shape: SeriesShape::Trend,
            points: series_points(&[("2026-05-01", 90.0), ("2026-06-01", 88.0), ("2026-07-01", 87.5)]),
        };
        // The y-axis runs low-to-high whichever way "better" points, so a falling
        // series still starts at the top of the chart.
        let plot_colours = |series: SeriesView| -> Vec<Color> {
            let view = View::Progress(ProgressView { headline: "Cutting".into(), series: vec![series], notes: vec![], goals: vec![] });
            render(&view)
                .iter()
                .flat_map(|line| line.spans.clone())
                .filter(|span| span.content.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)))
                .filter_map(|span| span.style.fg)
                .collect()
        };

        let falling = plot_colours(cutting.clone());
        assert!(!falling.is_empty(), "the readings are plotted as braille");
        assert!(falling.iter().all(|c| *c == SUCCESS), "losing weight on a cut is progress, not a regression: {falling:?}");

        // The same readings against the opposite goal are the same shape, other verdict.
        let bulking = plot_colours(SeriesView { better: Direction::Higher, ..cutting });
        assert!(bulking.iter().all(|c| *c == WARNING), "got: {bulking:?}");
    }

    /// A breakdown is bars, not a trend — its buckets carry no time order. The bars are
    /// proportional to the largest bucket and scaled to the width on offer, so the same
    /// data fills a wide transcript and still fits a narrow one.
    #[test]
    fn progress_renders_a_breakdown_as_proportional_bars() {
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
            goals: vec![],
        });

        let bars = |width: u16| -> Vec<usize> {
            flat(&render_view(&view, width))
                .lines()
                .map(|line| line.chars().filter(|c| *c == '█').count())
                .filter(|filled| *filled > 0)
                .collect()
        };

        let text = flat(&render(&view));
        assert!(text.contains("Chest 12 sets"), "each bucket is named and numbered: {text}");
        assert!(text.contains("Back  16 sets"), "the labels align: {text}");
        assert!(text.contains("Legs   9 sets"), "and so do the readings: {text}");
        assert!(!text.contains('→'), "a breakdown has no trend to report: {text}");

        let [chest, back, legs] = bars(WIDE)[..] else { panic!("one bar per bucket: {:?}", bars(WIDE)) };
        assert!(back > chest && chest > legs, "bars rank with their buckets: {back} {chest} {legs}");
        assert_eq!(back, WIDE as usize - INDENT.len() - "Back  16 sets ".len(), "the largest bucket fills the width");
        assert!(bars(40)[0] < chest, "a narrower transcript gets narrower bars: {:?}", bars(40));
    }

    fn programme_view(status: Option<gymbuddy_proto::ProgrammeStatusView>) -> View {
        View::Programme(Box::new(ProgrammeView {
            title: "6-week base".into(),
            start_date: "2026-07-01".into(),
            target_end_date: None,
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

    /// [R2.1]: a live programme renders where the user is; a proposed one has no
    /// position and renders the section not at all, as with any other empty list.
    #[test]
    fn programme_status_renders_position_next_and_counts() {
        let text = flat(&render(&programme_view(Some(gymbuddy_proto::ProgrammeStatusView {
            current_week: 3,
            block_focus: Some("accumulation".into()),
            next_slot: Some(gymbuddy_proto::ProgrammeSlotView { week_idx: 3, day_idx: 1, focus: "upper".into() }),
            trained: 2,
            missed: 2,
            skipped: 0,
            remaining: 8,
        }))));
        assert!(text.contains("Where you are"));
        assert!(text.contains("Week 3 of 6 — accumulation"));
        assert!(text.contains("Next: Week 3, day 1: upper"));
        assert!(text.contains("2 trained · 2 missed · 0 skipped · 8 to go"));
        assert!(!text.contains(PROGRAMME_LOCK_IN_ASK), "a live programme is not awaiting confirmation: {text}");

        let draft = flat(&render(&programme_view(None)));
        assert!(!draft.contains("Where you are"), "a draft has nowhere to be: {draft}");
        assert!(draft.contains(PROGRAMME_LOCK_IN_ASK));
    }

    /// The [C4.6] report has no layout of its own yet ([T2.1] gives it one), but it must
    /// still say what it is: an unhandled variant renders as "[unsupported message]", and
    /// a programme report reaching the user as that would be worse than no report.
    #[test]
    fn a_programme_report_announces_itself_before_it_has_a_layout() {
        let View::Programme(programme) = programme_view(Some(gymbuddy_proto::ProgrammeStatusView {
            current_week: 3,
            block_focus: Some("accumulation".into()),
            next_slot: None,
            trained: 2,
            missed: 2,
            skipped: 0,
            remaining: 8,
        })) else {
            panic!("the fixture builds a programme view");
        };

        let report = gymbuddy_proto::ProgrammeProgressView {
            programme: *programme,
            adherence: gymbuddy_proto::ProgrammeAdherenceView {
                settled: 4,
                trained: 2,
                drifting_days: vec![],
                reschedule: None,
            },
            goals: vec![],
        };
        let text = flat(&render(&View::ProgrammeProgress(Box::new(report))));
        assert!(text.contains("6-week base"), "the programme is named: {text}");
        assert!(text.contains("Week 3 of 6 — accumulation"), "and the position reaches the user: {text}");
        assert!(!text.contains("unsupported"), "the variant is handled, however plainly: {text}");
    }

    #[test]
    fn catalog_columns_align() {
        let view = View::Catalog(CatalogView {
            groups: vec![CatalogGroup {
                muscle_group: "Chest".into(),
                exercises: vec![CatalogEntry { name: "Bench Press".into(), aliases: "bench".into(), kind: "weight_reps".into() }],
            }],
        });
        let lines = render(&view);
        let text = flat(&lines);
        assert!(text.contains("Chest"));
        assert!(text.contains("Bench Press"));
        assert!(text.contains("weight_reps"));
    }
}


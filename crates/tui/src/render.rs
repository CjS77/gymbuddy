//! Render a domain [`View`] into ratatui lines — the TUI's own presentation
//! choices. Free to use the terminal's vertical space generously (this is where
//! the IRC-style transcript has more room than a Telegram bubble): coloured
//! headings, bullets, and space-aligned columns (no bordered `Table` widget,
//! which clashes with the flowing transcript).

use gymbuddy_proto::{CatalogView, ExerciseLog, HistoryView, SetLine, StatusView, View, WorkoutView};
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
        View::Workout(workout) => render_workout(workout),
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

fn render_workout(workout: &WorkoutView) -> Vec<Line<'static>> {
    let mut lines = vec![heading(workout.title.clone())];

    if let Some(rationale) = &workout.rationale
        && !rationale.trim().is_empty()
    {
        lines.extend(rationale.split('\n').map(|l| muted(l.to_string())));
    }

    if !workout.exercises.is_empty() {
        lines.push(Line::from(""));
        for (i, exercise) in workout.exercises.iter().enumerate() {
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

    if !workout.notes.is_empty() {
        lines.push(Line::from(""));
        lines.push(bold("Notes"));
        for note in &workout.notes {
            lines.push(Line::from(vec![Span::styled("  • ", Style::default().fg(MUTED)), Span::raw(note.clone())]));
        }
    }

    lines.push(Line::from(""));
    lines.push(muted("This is just a plan — log your sets as you go and I'll adjust.".into()));
    lines
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
    fn workout_renders_plan_without_markup() {
        use gymbuddy_proto::{PlannedExerciseView, WorkoutView};
        let view = View::Workout(WorkoutView {
            title: "Push focus".into(),
            rationale: Some("Bench was easy last time.".into()),
            exercises: vec![PlannedExerciseView {
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

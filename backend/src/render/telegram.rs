//! Render a [`View`] to Telegram Bot API HTML (or plain text).
//!
//! Reproduces the exact output the handler emitted before the view refactor, so
//! the Telegram bot is visually unchanged — locked down by golden tests below.

use gymbuddy_proto::{CatalogView, HistoryView, Render, SessionRosterView, SetLine, StatusView, TrainingModeView, View};

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

    result.push_str("\nThis is just a plan -- log your sets as you go and I'll adjust.");
    result
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
    use gymbuddy_proto::{CatalogEntry, CatalogGroup, ExerciseLog, HealthNote, Measurement, SessionView};

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
                        \nThis is just a plan -- log your sets as you go and I'll adjust.";
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
            mode: TrainingModeView::Programme { programme_title: "12-week <plan>".into(), week: 2, day: 1, focus: "upper".into() },
        };
        let (html, mode) = Telegram.render(&view);
        assert_eq!(mode, Some("HTML"));
        let expected = "<b>Upper</b>\n\
                        <i>Programme: 12-week &lt;plan&gt; — week 2, day 1: upper</i>\n\
                        \n\
                        1. <b>Bench Press</b> — 3 sets × 6 reps @ 65kg\n\
                        \nThis is just a plan -- log your sets as you go and I'll adjust.";
        assert_eq!(html, expected);
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

//! ratatui rendering: a scrollable transcript, an input box, and a status line.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, Entry, Speaker};

/// Render the whole screen.
pub fn draw(frame: &mut Frame, app: &App) {
    let [transcript_area, input_area, status_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)]).areas(frame.area());

    draw_transcript(frame, app, transcript_area);
    draw_input(frame, app, input_area);
    draw_status(frame, app, status_area);
}

fn draw_transcript(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let lines: Vec<Line> = app.transcript.iter().flat_map(entry_to_lines).collect();

    let inner_width = area.width.saturating_sub(2);
    let viewport = area.height.saturating_sub(2) as usize;
    let total = wrapped_line_count(&lines, inner_width);
    let max_top = total.saturating_sub(viewport) as u16;
    let top = max_top.saturating_sub(app.scroll_back);

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("GymBuddy"))
        .wrap(Wrap { trim: false })
        .scroll((top, 0));
    frame.render_widget(paragraph, area);
}

fn draw_input(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let paragraph = Paragraph::new(app.input.as_str()).block(Block::default().borders(Borders::ALL).title(app.input_label()));
    frame.render_widget(paragraph, area);

    // Place the cursor after the typed text, clamped to the inner width.
    let max_x = area.x + area.width.saturating_sub(2);
    let cursor_x = (area.x + 1 + app.input.chars().count() as u16).min(max_x);
    frame.set_cursor_position((cursor_x, area.y + 1));
}

fn draw_status(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let (marker, style) = if app.connected {
        ("● connected", Style::default().fg(Color::Green))
    } else {
        ("○ disconnected", Style::default().fg(Color::Red))
    };
    let text = format!(" {marker}   you:{}   ^C/Esc quit · PgUp/PgDn scroll", short_key(&app.my_pubkey));
    frame.render_widget(Paragraph::new(text).style(style), area);
}

/// Convert a transcript entry into one or more lines (split on embedded newlines,
/// e.g. the multi-line `/status` reply), prefixing the first with the speaker tag.
fn entry_to_lines(entry: &Entry) -> Vec<Line<'static>> {
    let (prefix, style) = match entry.speaker {
        Speaker::You => ("you ▸ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Speaker::Buddy => ("buddy ▸ ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Speaker::System => ("· ", Style::default().fg(Color::DarkGray)),
    };
    let mut lines: Vec<Line<'static>> = entry
        .text
        .split('\n')
        .enumerate()
        .map(|(i, raw)| {
            if i == 0 {
                Line::from(vec![Span::styled(prefix, style), Span::raw(raw.to_string())])
            } else {
                Line::from(Span::raw(raw.to_string()))
            }
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(prefix, style)));
    }
    lines
}

/// Count how many terminal rows the lines occupy once wrapped to `width`.
fn wrapped_line_count(lines: &[Line], width: u16) -> usize {
    let width = width.max(1) as usize;
    lines.iter().map(|line| line.width().max(1).div_ceil(width)).sum()
}

/// Abbreviate a 64-char hex key to `aaaaaa…ffffff` for the status line.
fn short_key(key: &str) -> String {
    if key.len() <= 14 {
        key.to_string()
    } else {
        format!("{}…{}", &key[..6], &key[key.len() - 6..])
    }
}

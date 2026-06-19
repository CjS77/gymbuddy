//! Domain-semantic view model: *what* the assistant is communicating, never *how*
//! to render it.
//!
//! The server maps its internal state into these DTOs; each client (the TUI, the
//! in-backend Telegram renderer, a future Android app) decides entirely how a set,
//! a status report or the catalogue should look. No presentation vocabulary
//! (paragraphs, columns, colours, bold) lives here — only domain meaning.
//!
//! The types are deliberately decoupled from the server's DB models so a schema
//! change does not ripple onto the wire.

use serde::{Deserialize, Serialize};

/// One assistant response, expressed in domain terms.
///
/// `#[non_exhaustive]` plus the [`View::Message`] fallback give graceful
/// degradation: a client built against an older protocol can still render a
/// `Message`, and adding variants later does not break the type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum View {
    /// Free-form assistant reply, plus the conversational follow-ups (`notes`,
    /// e.g. the set-count checkpoint) and any action `failures` produced while
    /// handling the turn. The universal fallback every client can render.
    Message { text: String, notes: Vec<String>, failures: Vec<String> },
    /// Current training session and active health flags ( `/status` ).
    Status(StatusView),
    /// The exercise catalogue grouped by muscle group ( `/exercises` ).
    Catalog(CatalogView),
    /// Recent workout summaries ( `/history` ).
    History(HistoryView),
    /// A short textual notice — help, registration confirmations, acknowledgements,
    /// error messages.
    Notice { text: String },
    /// The new state of the user's rest-timer preference, after a `/timers` flip.
    /// Clients show it however they like — Telegram as a one-line notice, the TUI by
    /// updating its sidebar switch.
    Timers { enabled: bool },
    /// A designed-but-unlogged workout ( `/nextworkout` ): the rationale plus the
    /// prescribed exercises and target sets. Nothing here is logged — the user still
    /// logs sets the normal way.
    Workout(WorkoutView),
}

impl View {
    /// A plain assistant message with no notes or failures.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message { text: text.into(), notes: Vec::new(), failures: Vec::new() }
    }

    /// A short notice.
    pub fn notice(text: impl Into<String>) -> Self {
        Self::Notice { text: text.into() }
    }

    /// A plain-text rendering every client can fall back to when it has no bespoke
    /// rendering for a variant. Renderers use this for their catch-all arm so an
    /// unhandled view degrades to a readable line rather than an empty message.
    ///
    /// The match is deliberately exhaustive (no wildcard): adding a `View` variant
    /// forces a line here, which is what keeps the fallback honest. `#[non_exhaustive]`
    /// only obliges *other* crates to add a wildcard, so a future variant arriving
    /// from a newer server still lands on a renderer's own catch-all — covered by
    /// the generic text below once recompiled against it.
    pub fn fallback_text(&self) -> String {
        match self {
            View::Message { text, .. } => text.clone(),
            View::Notice { text } => text.clone(),
            View::Timers { enabled } => format!("Rest timers are now {}.", if *enabled { "on" } else { "off" }),
            View::Status(_) => "Here's your current session.".to_string(),
            View::Catalog(_) => "Here's the exercise catalogue.".to_string(),
            View::History(_) => "Here's your recent workout history.".to_string(),
            View::Workout(w) => format!("Here's a workout: {}.", w.title),
        }
    }
}

/// A renderer turns a [`View`] into a client-native representation (HTML for
/// Telegram, ratatui `Line`s for the TUI, …). The trait only names the contract;
/// each client implements it in its own crate with its own `Output`.
pub trait Render {
    type Output;
    fn render(&self, view: &View) -> Self::Output;
}

// ── /status ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusView {
    pub user_name: String,
    /// `None` when there is no active session.
    pub session: Option<SessionView>,
    pub health: Vec<HealthNote>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub started_at: String,
    pub completed: Vec<ExerciseLog>,
    /// Open entries. Exactly one is the "current exercise"; more than one is an
    /// in-progress superset. The client decides how to phrase it.
    pub in_progress: Vec<ExerciseLog>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExerciseLog {
    pub name: String,
    pub sets: Vec<SetLine>,
}

/// One recorded set, decoupled from the server's DB types. `(count, value)` is
/// interpreted via `measurement`, exactly as the backend stores it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetLine {
    pub measurement: Measurement,
    pub count: Option<u32>,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Measurement {
    WeightReps,
    TimeBased,
    DistanceBased,
    LevelBased,
    ScoreBased,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthNote {
    /// "injury" / "illness" / "wellbeing".
    pub kind: String,
    /// Body part, already defaulted to "general" when unspecified.
    pub body_part: String,
    pub description: String,
}

// ── /exercises ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogView {
    pub groups: Vec<CatalogGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogGroup {
    /// Display name of the muscle group.
    pub muscle_group: String,
    pub exercises: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    /// Comma-or-space separated aliases, empty when none.
    pub aliases: String,
    /// Measurement-type label, e.g. "weight_reps".
    pub kind: String,
}

// ── /history ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryView {
    pub sessions: Vec<SessionSummaryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummaryView {
    pub started_at: String,
    /// "done" or "active".
    pub status: String,
    pub entries: u32,
    pub minutes: Option<u32>,
}

// ── /nextworkout ─────────────────────────────────────────────────────────────────

/// A designed workout: a title, the coach's reasoning, and the prescribed
/// exercises. Purely a proposal — logging still happens the normal way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkoutView {
    pub title: String,
    /// The coach's reasoning for this session (why these exercises today).
    pub rationale: Option<String>,
    pub exercises: Vec<PlannedExerciseView>,
    /// Free-text caveats the coach attached (e.g. a dropped exercise, equipment note).
    pub notes: Vec<String>,
}

/// One prescribed exercise in a [`WorkoutView`]. The target fields are
/// presentation hints; `(target_reps, target_weight_kg)` cover the weight_reps
/// case and `target_secs` covers timed work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedExerciseView {
    pub name: String,
    pub target_sets: Option<u32>,
    pub target_reps: Option<u32>,
    pub target_weight_kg: Option<f64>,
    pub target_secs: Option<u32>,
    /// A short coaching cue or substitution note for this exercise.
    pub cue: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant must yield non-empty fallback text — that is the whole point of
    /// the helper (renderers send it instead of an empty message). Constructed via
    /// the same `View` values the server emits.
    #[test]
    fn fallback_text_is_never_empty() {
        let views = [
            View::message("logged"),
            View::notice("ok"),
            View::Timers { enabled: true },
            View::Timers { enabled: false },
            View::Status(StatusView { user_name: "Al".into(), session: None, health: vec![] }),
            View::Catalog(CatalogView { groups: vec![] }),
            View::History(HistoryView { sessions: vec![] }),
            View::Workout(WorkoutView { title: "Push focus".into(), rationale: None, exercises: vec![], notes: vec![] }),
        ];
        for view in &views {
            assert!(!view.fallback_text().is_empty(), "empty fallback for {view:?}");
        }
    }
}

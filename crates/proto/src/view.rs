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
    /// A designed-but-unlogged session roster ( `/nextworkout` ): the rationale plus
    /// the prescribed exercises and target sets. Nothing here is logged — the user
    /// still logs sets the normal way.
    SessionRoster(SessionRosterView),
    /// A session roster designed while the user has an active programme ([C1.4]):
    /// either it fills the programme's current slot, or it is a deliberate one-off
    /// that leaves the programme untouched — `mode` says which. Designs with no
    /// programme in play keep travelling as plain [`View::SessionRoster`], byte-identical
    /// to the pre-programme protocol (postcard is positional, so extending
    /// [`SessionRosterView`] itself would silently reshape existing messages — see the
    /// append-only rule on the envelope enums).
    ProgrammeSessionRoster { roster: SessionRosterView, mode: TrainingModeView },
    /// A multi-week programme skeleton ( `/programme` ): the goals it serves, its
    /// dates, its mesocycle blocks and the week template its slot grid was built
    /// from. Holds no exercises — a programme is a skeleton, not a script, and each
    /// session is still designed on demand against it.
    ///
    /// Boxed only to keep [`View`] small: a `ProgrammeView` is several times the size
    /// of every other variant, and `View` travels inside `ServerResponse` on every
    /// reply. `Box` is transparent to serde, so the wire bytes are exactly those of an
    /// unboxed `ProgrammeView` — the indirection is invisible to any peer.
    Programme(Box<ProgrammeView>),
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
            View::SessionRoster(r) => format!("Here's a workout: {}.", r.title),
            View::ProgrammeSessionRoster { roster, mode } => format!("Here's a workout: {} ({}).", roster.title, mode.summary()),
            View::Programme(p) => format!("Here's a programme: {} ({}).", p.title, p.shape_line()),
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

impl SetLine {
    /// Compact, human-friendly rendering of one set, e.g. "8×80kg", "82.5kg", "30s".
    /// Shared by every client renderer so a wire value displays identically across
    /// Telegram, the TUI and a future Android client.
    pub fn compact(&self) -> String {
        let v = trim_decimal(self.value);
        match self.measurement {
            Measurement::WeightReps => match self.count {
                Some(c) => format!("{c}×{v}kg"),
                None => format!("{v}kg"),
            },
            Measurement::TimeBased => format!("{v}s"),
            Measurement::DistanceBased => format!("{v}m"),
            Measurement::LevelBased => format!("L{v}"),
            Measurement::ScoreBased => format!("{v}pt"),
        }
    }
}

/// Drop a trailing ".0" so whole numbers read cleanly (80.0 → "80", 82.5 → "82.5").
fn trim_decimal(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v}") }
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

/// A designed session roster: a title, the coach's reasoning, and the prescribed
/// exercises. Purely a proposal — logging still happens the normal way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRosterView {
    pub title: String,
    /// The coach's reasoning for this session (why these exercises today).
    pub rationale: Option<String>,
    pub exercises: Vec<RosterExerciseView>,
    /// Free-text caveats the coach attached (e.g. a dropped exercise, equipment note).
    pub notes: Vec<String>,
}

/// The training mode that produced a designed session roster ([C1.4]), for rosters
/// designed while a programme is active. Plain ad-hoc with no programme in play
/// has no mode to report and travels as [`View::SessionRoster`], so "no programme"
/// is deliberately unrepresentable here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrainingModeView {
    /// A deliberate one-off during an active programme ("travelling, dumbbells
    /// only"): the named programme's slots are left completely untouched.
    AdHoc { programme_title: String },
    /// The roster fills the active programme's current slot.
    Programme { programme_title: String, week: u32, day: u32, focus: String },
}

impl TrainingModeView {
    /// One-line description of the mode, shared by every client renderer, e.g.
    /// `Programme: 12-week hypertrophy — week 2, day 1: upper` or
    /// `Ad-hoc session — 12-week hypertrophy is untouched`. Plain text; styling
    /// and escaping stay at the renderer.
    pub fn summary(&self) -> String {
        match self {
            Self::AdHoc { programme_title } => format!("Ad-hoc session — {programme_title} is untouched"),
            Self::Programme { programme_title, week, day, focus } => {
                format!("Programme: {programme_title} — week {week}, day {day}: {focus}")
            }
        }
    }
}

/// One prescribed exercise in a [`SessionRosterView`]. The target fields are
/// presentation hints; `(target_reps, target_weight_kg)` cover the weight_reps
/// case and `target_secs` covers timed work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RosterExerciseView {
    pub name: String,
    pub target_sets: Option<u32>,
    pub target_reps: Option<u32>,
    pub target_weight_kg: Option<f64>,
    pub target_secs: Option<u32>,
    /// A short coaching cue or substitution note for this exercise.
    pub cue: Option<String>,
}

impl RosterExerciseView {
    /// The prescription line for this exercise, e.g. "3 sets × 6 reps @ 65kg" or
    /// "3 sets × 60s". Empty when no targets are set. Shared by all client renderers
    /// (the value is plain text — escaping/styling stays at the renderer).
    pub fn target_line(&self) -> String {
        let mut parts = String::new();
        if let Some(sets) = self.target_sets {
            parts.push_str(&format!("{sets} sets"));
        }
        if let Some(secs) = self.target_secs {
            if !parts.is_empty() {
                parts.push_str(" × ");
            }
            parts.push_str(&format!("{secs}s"));
        } else if let Some(reps) = self.target_reps {
            if !parts.is_empty() {
                parts.push_str(" × ");
            }
            parts.push_str(&format!("{reps} reps"));
        }
        if let Some(weight) = self.target_weight_kg {
            if !parts.is_empty() {
                parts.push(' ');
            }
            parts.push_str(&format!("@ {}kg", trim_decimal(weight)));
        }
        parts
    }
}

// ── /programme ───────────────────────────────────────────────────────────────────

/// A designed programme: the long-term skeleton a user's sessions are then designed
/// against. Deliberately carries no exercises — a [`ProgrammeSlotView`]'s `focus` is a
/// text intent ("upper"), and exercises appear only once a session roster is designed
/// for that slot.
///
/// `week_template` is the repeating week the slot grid was expanded from, not the whole
/// grid: `weeks × days_per_week` cells all read from the same handful of day intents, so
/// sending the grid itself would be the same few strings repeated dozens of times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeView {
    pub title: String,
    /// `YYYY-MM-DD`, or the stored datetime — clients display it as given.
    pub start_date: String,
    /// The date the programme aims to conclude by; `None` when open-ended.
    pub target_end_date: Option<String>,
    pub weeks: u32,
    pub days_per_week: u32,
    /// Free text, e.g. "upper/lower".
    pub split: String,
    /// Free text, e.g. "double progression: add reps, then load".
    pub progression_policy: String,
    /// The mesocycles, in week order.
    pub blocks: Vec<ProgrammeBlockView>,
    /// One repeating week, in day order.
    pub week_template: Vec<ProgrammeDayView>,
    /// The goals this programme serves, highest priority first, already rendered as
    /// display labels ("Bench Press to 100.0").
    pub goals: Vec<String>,
    /// Free-text caveats from persisting the design (e.g. a goal that could not be linked).
    pub notes: Vec<String>,
    /// Whether this programme is live. A draft is still awaiting the user's "lock it in";
    /// clients use this to decide whether to ask for that confirmation.
    pub active: bool,
}

impl ProgrammeView {
    /// One-line shape of the programme, e.g. "8 weeks x 3 days, upper/lower". Shared by
    /// every client renderer so the summary reads identically everywhere.
    pub fn shape_line(&self) -> String {
        format!("{} weeks × {} days/week, {}", self.weeks, self.days_per_week, self.split)
    }
}

/// One mesocycle of a [`ProgrammeView`]: an inclusive, 1-based week range with an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeBlockView {
    pub start_week: u32,
    pub end_week: u32,
    pub focus: String,
}

impl ProgrammeBlockView {
    /// The week-range label, e.g. "Weeks 1-4" or "Week 5" for a single-week block.
    pub fn weeks_label(&self) -> String {
        if self.start_week == self.end_week {
            format!("Week {}", self.start_week)
        } else {
            format!("Weeks {}-{}", self.start_week, self.end_week)
        }
    }
}

/// One training day of a [`ProgrammeView`]'s repeating week. `day_idx` is the 1-based
/// ordinal training day within the week, never a calendar weekday, and `focus` is a
/// text intent — never an exercise list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeDayView {
    pub day_idx: u32,
    pub focus: String,
}

#[cfg(test)]
pub(crate) mod tests {
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
            View::SessionRoster(SessionRosterView { title: "Push focus".into(), rationale: None, exercises: vec![], notes: vec![] }),
            View::ProgrammeSessionRoster {
                roster: SessionRosterView { title: "Upper".into(), rationale: None, exercises: vec![], notes: vec![] },
                mode: TrainingModeView::Programme { programme_title: "12-week".into(), week: 1, day: 1, focus: "upper".into() },
            },
            View::Programme(Box::new(programme_view())),
        ];
        for view in &views {
            assert!(!view.fallback_text().is_empty(), "empty fallback for {view:?}");
        }
    }

    /// A representative designed programme, shared by the tests below.
    pub(crate) fn programme_view() -> ProgrammeView {
        ProgrammeView {
            title: "12-week hypertrophy".into(),
            start_date: "2026-07-20".into(),
            target_end_date: Some("2026-10-12".into()),
            weeks: 12,
            days_per_week: 3,
            split: "upper/lower".into(),
            progression_policy: "double progression".into(),
            blocks: vec![
                ProgrammeBlockView { start_week: 1, end_week: 4, focus: "accumulation".into() },
                ProgrammeBlockView { start_week: 5, end_week: 5, focus: "deload".into() },
            ],
            week_template: vec![
                ProgrammeDayView { day_idx: 1, focus: "upper".into() },
                ProgrammeDayView { day_idx: 2, focus: "lower".into() },
            ],
            goals: vec!["Bench Press to 100.0".into()],
            notes: vec![],
            active: false,
        }
    }

    #[test]
    fn programme_shape_and_block_labels_read_for_humans() {
        let p = programme_view();
        assert_eq!(p.shape_line(), "12 weeks × 3 days/week, upper/lower");
        // A multi-week block reads as a range; a one-week deload must not say "Weeks 5-5".
        assert_eq!(p.blocks[0].weeks_label(), "Weeks 1-4");
        assert_eq!(p.blocks[1].weeks_label(), "Week 5");
    }

    /// The mode summary is what every renderer surfaces, so both modes must name
    /// the programme and the programme mode must place the user in the grid.
    #[test]
    fn training_mode_summary_names_the_programme() {
        let ad_hoc = TrainingModeView::AdHoc { programme_title: "12-week hypertrophy".into() };
        assert_eq!(ad_hoc.summary(), "Ad-hoc session — 12-week hypertrophy is untouched");

        let slot = TrainingModeView::Programme { programme_title: "12-week hypertrophy".into(), week: 2, day: 1, focus: "upper".into() };
        assert_eq!(slot.summary(), "Programme: 12-week hypertrophy — week 2, day 1: upper");
    }

    fn set(measurement: Measurement, count: Option<u32>, value: f64) -> SetLine {
        SetLine { measurement, count, value }
    }

    #[test]
    fn set_line_compact_per_measurement() {
        assert_eq!(set(Measurement::WeightReps, Some(8), 80.0).compact(), "8×80kg");
        assert_eq!(set(Measurement::WeightReps, Some(8), 82.5).compact(), "8×82.5kg");
        assert_eq!(set(Measurement::WeightReps, None, 80.0).compact(), "80kg");
        assert_eq!(set(Measurement::TimeBased, None, 30.0).compact(), "30s");
        assert_eq!(set(Measurement::DistanceBased, None, 5000.0).compact(), "5000m");
        assert_eq!(set(Measurement::LevelBased, None, 3.0).compact(), "L3");
        assert_eq!(set(Measurement::ScoreBased, None, 9.5).compact(), "9.5pt");
    }

    #[test]
    fn roster_exercise_target_line() {
        let weighted = RosterExerciseView {
            name: "Bench Press".into(),
            target_sets: Some(3),
            target_reps: Some(6),
            target_weight_kg: Some(65.0),
            target_secs: None,
            cue: None,
        };
        assert_eq!(weighted.target_line(), "3 sets × 6 reps @ 65kg");

        let timed = RosterExerciseView {
            name: "Plank".into(),
            target_sets: Some(3),
            target_reps: None,
            target_weight_kg: None,
            target_secs: Some(60),
            cue: None,
        };
        assert_eq!(timed.target_line(), "3 sets × 60s");

        let bare = RosterExerciseView {
            name: "Mystery".into(),
            target_sets: None,
            target_reps: None,
            target_weight_kg: None,
            target_secs: None,
            cue: None,
        };
        assert_eq!(bare.target_line(), "");
    }
}

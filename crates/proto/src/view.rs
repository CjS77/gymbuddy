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
    /// How the user is tracking against their goals ( `/progress` ), as chartable
    /// [`SeriesView`]s the client plots however it can ([C6.2]/[C6.3]).
    Progress(ProgressView),
    /// The post-session review ([C6.5]): what happened in a session that just ended,
    /// how it compared to what was prescribed, and what it moved. Emitted when a
    /// session ends, when a stale one is auto-closed, and by `/review`.
    ///
    /// Boxed for the same reason as [`View::Programme`] — it is the largest variant and
    /// `View` travels inside `ServerResponse` on every reply. `Box` is transparent to
    /// serde, so the wire bytes are exactly those of an unboxed `SessionReviewView`.
    SessionReview(Box<SessionReviewView>),
    /// The full report on a live programme ( `/programme status` , [C4.6]): where the
    /// user is, how well the grid is being kept to, and where the goals it serves are
    /// heading.
    ///
    /// A new variant rather than more fields on [`ProgrammeView`], which is closed — see
    /// the append-only note on its `status` field. Boxed like its two neighbours, and for
    /// the same reason.
    ProgrammeProgress(Box<ProgrammeProgressView>),
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
            View::Progress(p) => p.summary_line(),
            View::SessionReview(r) => r.summary_line(),
            View::ProgrammeProgress(p) => p.summary_line(),
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
///
/// The struct carries both a *proposed* programme (the `/programme` interview) and a
/// *live* one being reported on (`/programme status`); `status` is what separates them.
/// One type rather than two because the skeleton is the same artefact either way, and a
/// second `View` variant would render the same nine fields twice in every client.
///
/// [R2.1] appended `status`, which the append-only rule in the crate root otherwise
/// forbids for an existing type. It is sound exactly once and only here: `ProgrammeView`
/// was introduced after 0.22.0 and has never been in a release, so no deployed peer has
/// ever decoded one and there are no old bytes to misparse. Once it ships, this struct
/// is closed like every other — a later addition wants a new appended `View` variant.
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
    /// Where the user has got to, present only when the programme is being *reported on*
    /// (inside a [`ProgrammeProgressView`]) rather than proposed. A freshly designed
    /// programme has no position to report, so this stays `None` through the whole
    /// `/programme` interview.
    pub status: Option<ProgrammeStatusView>,
}

impl ProgrammeView {
    /// One-line shape of the programme, e.g. "8 weeks x 3 days, upper/lower". Shared by
    /// every client renderer so the summary reads identically everywhere.
    pub fn shape_line(&self) -> String {
        format!("{} weeks × {} days/week, {}", self.weeks, self.days_per_week, self.split)
    }

    /// Where the user is, e.g. "Week 3 of 12 — accumulation", or `None` for a programme
    /// with no [`status`](Self::status) to report.
    pub fn position_line(&self) -> Option<String> {
        Some(self.status.as_ref()?.position_line(self.weeks))
    }
}

/// Where a live programme has got to ([R2.1]): calendar position, the block that covers
/// it, the next session due, and how the grid behind the user has resolved.
///
/// Only the cheap half of programme reporting — counts of settled slots, no adherence
/// ratios and no drift verdict. Those live in [`ProgrammeAdherenceView`] ([C4.6]), built
/// on these numbers rather than beside them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeStatusView {
    /// The 1-based week the calendar puts the user in, clamped to the programme's span.
    /// Read off `start_date` and today, so it says where the user *is* — which can be
    /// behind `next_slot` when they train more than one slot in a week.
    pub current_week: u32,
    /// Focus of the mesocycle covering `current_week`, when a block covers it. Blocks
    /// need not tile the programme, so a week between them has none.
    pub block_focus: Option<String>,
    /// The next session due, or `None` once every slot is settled.
    pub next_slot: Option<ProgrammeSlotView>,
    /// Slots whose bound roster was actually executed — designing one is not training it.
    pub trained: u32,
    /// Slots whose week passed with nothing designed for them.
    pub missed: u32,
    /// Slots deliberately dropped.
    pub skipped: u32,
    /// Slots still ahead: everything neither trained, missed nor skipped. The four counts
    /// are disjoint and sum to the grid, so a client can render them as a whole.
    pub remaining: u32,
}

impl ProgrammeStatusView {
    /// The counts as one line, e.g. "5 trained · 1 missed · 0 skipped · 12 to go". Shared
    /// by every client renderer so the tally reads identically everywhere.
    pub fn counts_line(&self) -> String {
        format!("{} trained · {} missed · {} skipped · {} to go", self.trained, self.missed, self.skipped, self.remaining)
    }

    /// Where the user is against a programme of `weeks` weeks, e.g.
    /// "Week 3 of 12 — accumulation". Takes the span as an argument because it belongs to
    /// the programme, not to the position — [`ProgrammeView::position_line`] and
    /// [`ProgrammeProgressView`] both read it from here so the two reports cannot end up
    /// with two spellings of the same sentence.
    pub fn position_line(&self, weeks: u32) -> String {
        match &self.block_focus {
            Some(focus) => format!("Week {} of {weeks} — {focus}", self.current_week),
            None => format!("Week {} of {weeks}", self.current_week),
        }
    }
}

/// One cell of a programme's week/day grid. `week_idx` and `day_idx` are both 1-based,
/// and `day_idx` is the ordinal training day within the week, never a calendar weekday.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeSlotView {
    pub week_idx: u32,
    pub day_idx: u32,
    pub focus: String,
}

impl ProgrammeSlotView {
    /// The slot's position and intent as one line, e.g. "Week 3, day 2: pull".
    pub fn label(&self) -> String {
        format!("Week {}, day {}: {}", self.week_idx, self.day_idx, self.focus)
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

// ── chartable series ─────────────────────────────────────────────────────────────

/// The eight block glyphs a [`SeriesView::spark`] is drawn from, shortest first.
const SPARK_TICKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Which way progress runs for a [`SeriesView`].
///
/// Carried as data because it is not derivable from the numbers: 90 → 85 kg is
/// progress for a cut and a regression for a bulk, and only the server knows which
/// goal the series was drawn for. A client that assumes "up is good" renders half
/// the app's series backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// Up is progress — load lifted, weekly volume, reps at a fixed load.
    Higher,
    /// Down is progress — bodyweight on a cut, time over a fixed distance.
    Lower,
    /// The series is worth showing but has no better end: a reading tracked without
    /// a goal attached. Clients report the movement without a verdict on it.
    Neutral,
}

/// What kind of chart a [`SeriesView`] wants — the *shape* of the data, never a
/// widget name.
///
/// The point of this enum is that a client writes one renderer per variant here and
/// is then done: exercise progression, body metrics and goal trajectory are all
/// [`Trend`](SeriesShape::Trend)s or [`Trajectory`](SeriesShape::Trajectory)s, and
/// muscle-group volume is a [`Breakdown`](SeriesShape::Breakdown). Adding a metric
/// server-side must never mean adding client code — that is how a chart layer ends
/// up rebuilt once per client.
///
/// Rides inside an appended `View` variant, so its own order is wire format too.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SeriesShape {
    /// Readings in time order — a line or sparkline. Point labels are dates.
    Trend,
    /// Readings in time order against a fixed target — a line plus a reference line
    /// at `target`. The goal-trajectory shape.
    Trajectory { target: f64 },
    /// Independent buckets compared side by side — bars. Point labels are category
    /// names (a muscle group, a weekday), and their order carries no time meaning,
    /// so movement between them is not a trend.
    Breakdown,
}

/// One reading in a [`SeriesView`]: a display label and a number.
///
/// `label` is deliberately a string rather than a timestamp — it is a date for a
/// trend and a category name for a breakdown, and collapsing both to "the x-axis
/// label" is what lets one client renderer serve every metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesPointView {
    pub label: String,
    pub value: f64,
}

/// One chartable series — the single contract for every chart GymBuddy emits
/// ([C6.2]).
///
/// Core ships the data; each client plots it (Telegram as text plus a unicode
/// sparkline, the TUI with ratatui widgets, a future app with real pixels). The
/// server's own `TimeSeries` aggregate stays behind the DAO: this is the wire model,
/// decoupled on purpose so a query change does not reshape the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesView {
    /// What the series measures, e.g. "Bench Press — top set".
    pub title: String,
    /// The unit `value`s are in ("kg", "reps", "sets"); empty for a bare count.
    pub unit: String,
    /// Which way "better" runs — see [`Direction`].
    pub better: Direction,
    pub shape: SeriesShape,
    pub points: Vec<SeriesPointView>,
}

impl SeriesView {
    /// The most recent (or last) reading.
    pub fn latest(&self) -> Option<&SeriesPointView> {
        self.points.last()
    }

    /// Movement from the first reading to the last, in `unit`s.
    ///
    /// `None` for a [`SeriesShape::Breakdown`] — its buckets are not ordered in time,
    /// so "first to last" would be an arithmetic result with no meaning — and for a
    /// series of fewer than two points.
    pub fn change(&self) -> Option<f64> {
        if matches!(self.shape, SeriesShape::Breakdown) || self.points.len() < 2 {
            return None;
        }
        Some(self.points.last()?.value - self.points.first()?.value)
    }

    /// Whether the [`change`](Self::change) is progress, per [`better`](Self::better).
    ///
    /// `None` means there is no verdict to give — not enough points, a
    /// [`Direction::Neutral`] series, or no movement at all — and clients should say
    /// nothing rather than guess.
    pub fn improving(&self) -> Option<bool> {
        let change = self.change()?;
        match self.better {
            _ if change == 0.0 => None,
            Direction::Neutral => None,
            Direction::Higher => Some(change > 0.0),
            Direction::Lower => Some(change < 0.0),
        }
    }

    /// `(min, max)` for a value axis, widened to include a
    /// [`SeriesShape::Trajectory`]'s target so the reference line cannot fall off the
    /// chart. `None` for an empty series.
    ///
    /// Computed here so every client scales its axis identically instead of each one
    /// rediscovering that the target belongs in the range.
    pub fn bounds(&self) -> Option<(f64, f64)> {
        let (min, max) = self.point_bounds()?;
        match self.shape {
            SeriesShape::Trajectory { target } => Some((min.min(target), max.max(target))),
            _ => Some((min, max)),
        }
    }

    /// `(min, max)` over the points alone — what a sparkline scales to, since a distant
    /// target would otherwise flatten the readings into one glyph.
    fn point_bounds(&self) -> Option<(f64, f64)> {
        let first = self.points.first()?.value;
        Some(self.points.iter().fold((first, first), |(lo, hi), p| (lo.min(p.value), hi.max(p.value))))
    }

    /// A unicode block sparkline of the readings, e.g. `▁▂▄▅█`.
    ///
    /// The plain-text fallback for clients that cannot draw a chart, shared here so
    /// Telegram and the TUI produce the same glyphs. It plots the values **as they
    /// are**: a cut's bodyweight series slopes down because that is what the numbers
    /// do. Direction is a verdict on the movement, not a transform of it — flipping
    /// the geometry would mean showing the user data they did not record. Clients say
    /// which way is good with [`improving`](Self::improving), in words or colour.
    ///
    /// Empty for an empty series; a flat series renders as a mid-height band.
    pub fn spark(&self) -> String {
        let Some((min, max)) = self.point_bounds() else {
            return String::new();
        };
        let span = max - min;
        let top = SPARK_TICKS.len() - 1;
        self.points
            .iter()
            .map(|p| {
                let tick = if span == 0.0 { top / 2 } else { (((p.value - min) / span) * top as f64).round() as usize };
                SPARK_TICKS[tick.min(top)]
            })
            .collect()
    }

    /// One-line summary of the movement, e.g. `80 → 92.5 kg (+12.5, better)`.
    ///
    /// Empty when [`change`](Self::change) has nothing to report. Plain text —
    /// styling and escaping stay at the renderer.
    pub fn change_line(&self) -> String {
        let (Some(change), Some(first), Some(last)) = (self.change(), self.points.first(), self.points.last()) else {
            return String::new();
        };
        let verdict = match self.improving() {
            Some(true) => ", better",
            Some(false) => ", worse",
            None => "",
        };
        format!("{} → {} ({}{verdict})", trim_decimal(first.value), self.value_label(last.value), signed(change))
    }

    /// A value in this series' unit, e.g. `92.5 kg` — or just `92.5` when the series
    /// carries no unit. The one place a series' numbers get formatted, so every client
    /// prints them the same way.
    pub fn value_label(&self, value: f64) -> String {
        match self.unit.is_empty() {
            true => trim_decimal(value),
            false => format!("{} {}", trim_decimal(value), self.unit),
        }
    }

    /// The value a [`SeriesShape::Trajectory`] aims at, e.g. `Target: 100 kg`. Empty
    /// for every other shape.
    pub fn target_line(&self) -> String {
        match self.shape {
            SeriesShape::Trajectory { target } => format!("Target: {}", self.value_label(target)),
            _ => String::new(),
        }
    }

    /// The latest reading with its label, e.g. `92.5 kg (2026-07-01)`. Empty for an
    /// empty series. What a client shows when the series is too short for a
    /// [`change_line`](Self::change_line).
    pub fn latest_line(&self) -> String {
        self.latest().map(|p| format!("{} ({})", self.value_label(p.value), p.label)).unwrap_or_default()
    }

    /// Whether `value` clears a [`SeriesShape::Trajectory`]'s target, the way
    /// [`better`](Self::better) runs — below the target is success for a cut.
    ///
    /// `false` for every other shape: a series with no target never arrives at one,
    /// and neither does a [`Direction::Neutral`] one, which has no better end to
    /// arrive at.
    pub fn reaches(&self, value: f64) -> bool {
        let SeriesShape::Trajectory { target } = self.shape else {
            return false;
        };
        match self.better {
            Direction::Higher => value >= target,
            Direction::Lower => value <= target,
            Direction::Neutral => false,
        }
    }

    /// How much of the distance from the latest reading to a
    /// [`SeriesShape::Trajectory`]'s target `value` covers: `1.0` arrives, `0.5` gets
    /// halfway, `0.0` stands still and negative moves away from it.
    ///
    /// Direction-free by construction — the target is subtracted from the same end
    /// for a cut as for a lift — so callers grade a shortfall without re-deriving
    /// which way "better" runs. `None` when there is no target, no reading to measure
    /// from, or the target is already met exactly, which leaves nothing to cover.
    pub fn coverage(&self, value: f64) -> Option<f64> {
        let SeriesShape::Trajectory { target } = self.shape else {
            return None;
        };
        let latest = self.latest()?.value;
        let gap = target - latest;
        (gap != 0.0).then(|| (value - latest) / gap)
    }
}

// ── goal outlook ([C6.4]) ────────────────────────────────────────────────────────

/// The fewest readings an outlook may be projected from.
///
/// Two points draw a line through any two accidents; three is the fewest that can
/// disagree with each other. Below this the answer is [`GoalOutlook::TooEarly`], and
/// [`GoalOutlookView::assess`] drops the projected value rather than carry a number
/// it will not stand behind.
pub const MIN_PROJECTION_READINGS: usize = 3;

/// The shortest span of readings, in days, a rate may be extrapolated from. Three
/// good sets inside one week are a good week, not a trend to carry across a training
/// block. Enforced by whoever computes the rate — it needs a calendar, and this crate
/// deliberately has none.
pub const MIN_PROJECTION_DAYS: i64 = 7;

/// How much of the remaining gap a rate has to close by the deadline before the
/// shortfall stops being a near miss. Three quarters: at that point a push, a
/// deadline nudge or a slightly kinder target still gets there, which is "at risk";
/// below it the plan does not arrive, which is "off track".
const AT_RISK_COVERAGE: f64 = 0.75;

/// How likely a goal is to land, in the only vocabulary this data supports ([C6.4]).
///
/// Four bands and no percentage, deliberately. A goal projection is a straight line
/// through a handful of noisy, sparse, self-reported readings; "87% likely" claims a
/// precision that is not there, and a user who is shown a number believes it. These
/// are the answers a PT would actually give.
///
/// [`TooEarly`](Self::TooEarly) is a verdict, not a missing value. It sits at tag 0
/// so the most conservative answer is also the cheapest thing to say, and it must
/// survive the wire as itself rather than collapsing into "not on track" — those are
/// different claims, and only one of them is true of a user who has just started.
///
/// Rides inside `View::Progress`, so its variant order is wire format: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalOutlook {
    /// Not enough logged to project from: too few readings, or too little time
    /// between them. "Ask me again in a fortnight", and a real answer.
    TooEarly,
    /// The current trend arrives by the target date — or, for an open-ended goal, is
    /// moving the right way — and the sessions behind it are happening.
    OnTrack,
    /// It nearly arrives, or it arrives on paper while the sessions are being missed.
    /// Recoverable, and worth saying out loud before it is not.
    AtRisk,
    /// The current trend does not get there, and not by a little.
    OffTrack,
    /// Already met. A record of what happened, not a projection.
    Achieved,
    /// The target date passed without it. Also a record — kept in this enum so a goal
    /// always has exactly one outlook, and so a post-mortem can still name what went
    /// wrong ([`GoalLimiter`]).
    Missed,
}

impl GoalOutlook {
    /// The verdict in the words a client should use, e.g. `too early to say`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::TooEarly => "too early to say",
            Self::OnTrack => "on track",
            Self::AtRisk => "at risk",
            Self::OffTrack => "off track",
            Self::Achieved => "achieved",
            Self::Missed => "missed",
        }
    }

    /// Whether the goal is finished either way — a record rather than a forecast.
    pub fn is_settled(&self) -> bool {
        matches!(self, Self::Achieved | Self::Missed)
    }

    /// Whether this verdict is a claim about where the goal lands. `false` for
    /// [`TooEarly`](Self::TooEarly) and for both settled outcomes, so a client can
    /// tell "I project you miss" from "I am not projecting".
    pub fn is_projection(&self) -> bool {
        matches!(self, Self::OnTrack | Self::AtRisk | Self::OffTrack)
    }
}

/// What is holding a goal back — the cause a shortfall commentary hangs off ([C6.6]).
///
/// Exactly three, because a goal that is not landing fails in exactly three ways, and
/// the remedy differs completely between them: turn up, train harder, or fix the
/// goal. Carried rather than re-derived so the diagnosis is made once, next to the
/// evidence, instead of being guessed at from prose further down the pipeline.
///
/// Rides inside `View::Progress`: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalLimiter {
    /// The sessions are not happening. A live programme is drifting ([C4.4]), and
    /// turning up is the best predictor there is — so this outranks whatever the
    /// trend line says.
    Attendance,
    /// The sessions are happening and the numbers are not moving, or are moving the
    /// wrong way. The training, not the diary.
    Performance,
    /// Attendance and trend are both fine and the arithmetic still does not fit the
    /// date: the goal was set beyond what the time allows. The one cause that is not
    /// the user's to fix by training harder.
    Ambition,
}

impl GoalLimiter {
    /// What to tell the user is in the way, e.g. `sessions are being missed`.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Attendance => "sessions are being missed",
            Self::Performance => "the sessions are happening, the numbers are not moving",
            Self::Ambition => "the trend is fine — the target date never allowed for it",
        }
    }
}

/// How the user is turning up, as the outlook's dominant term ([C4.4]).
///
/// An *input* to [`GoalOutlookView::assess`], not wire data: the server derives it
/// from the live programme's drift, and what reaches a client is the verdict it
/// produced plus, where it bites, [`GoalLimiter::Attendance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Adherence {
    /// No programme is live, so there is no attendance to judge and the outlook rests
    /// on the readings alone. The default: absent evidence must not read as bad news.
    #[default]
    Unprogrammed,
    /// A programme is live and is being kept to.
    Keeping,
    /// A programme is live and a recurring day is being consistently missed.
    Drifting,
}

/// One goal's projected outlook ([C6.4]): the verdict, what is holding it back, and
/// the evidence the verdict rests on.
///
/// The evidence travels because the verdict alone invites over-reading: three
/// readings and thirty produce the same four words, and a client (or [C6.6]) that
/// wants to hedge needs to know which it has. `projected` is present only when a
/// projection was actually made — see [`GoalOutlook::is_projection`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalOutlookView {
    /// The goal's subject, e.g. "Bench Press" or "Bodyweight" — the head of its
    /// series title, so a client can pair the two.
    pub goal: String,
    pub outlook: GoalOutlook,
    /// What is in the way. `None` when nothing is (on track, achieved) and when there
    /// is too little evidence to name a cause (too early) — an unknown cause must not
    /// be dressed up as a diagnosed one.
    pub limiter: Option<GoalLimiter>,
    /// Where the current rate lands by `target_date`, in the series' unit. `None`
    /// whenever no projection was made, which is most of the honest cases.
    pub projected: Option<f64>,
    /// The date the goal aims at; `None` for an open-ended goal.
    pub target_date: Option<String>,
    /// How many readings the verdict rests on.
    pub readings: u32,
}

impl GoalOutlookView {
    /// Read a live goal's outlook off its trend, its deadline and the user's
    /// attendance.
    ///
    /// `projected` is where the caller's rate lands by `target_date` — `None` for an
    /// open-ended goal, and `None` whenever the readings were too thin or too
    /// bunched to give a rate at all (see [`MIN_PROJECTION_DAYS`]). It is dropped
    /// here too if the series has fewer than [`MIN_PROJECTION_READINGS`] points: the
    /// refusal to project belongs in the model, not in a client's wording.
    ///
    /// The verdict on the movement comes from [`SeriesView::improving`], never from a
    /// raw slope — a flat series and a neutral one have no verdict to give, and
    /// inventing one is the whole failure mode this type exists to prevent.
    pub fn assess(
        goal: impl Into<String>,
        series: &SeriesView,
        projected: Option<f64>,
        target_date: Option<String>,
        adherence: Adherence,
    ) -> Self {
        let projected = (series.points.len() >= MIN_PROJECTION_READINGS).then_some(projected).flatten();
        let outlook = trend_outlook(series, projected, target_date.is_some());
        let (outlook, limiter) = under_adherence(outlook, series, adherence);
        Self { outlook, limiter, projected: outlook.is_projection().then_some(projected).flatten(), ..Self::of(goal, series, target_date) }
    }

    /// A goal already met. Nothing is in its way and there is nothing left to project.
    pub fn achieved(goal: impl Into<String>, series: &SeriesView, target_date: Option<String>) -> Self {
        Self { outlook: GoalOutlook::Achieved, ..Self::of(goal, series, target_date) }
    }

    /// A goal whose target date passed without it. Settled, and still diagnosed: what
    /// went wrong is exactly what [C6.6] needs to talk about afterwards.
    pub fn missed(goal: impl Into<String>, series: &SeriesView, target_date: Option<String>, adherence: Adherence) -> Self {
        Self { outlook: GoalOutlook::Missed, limiter: Some(limiter(series, adherence)), ..Self::of(goal, series, target_date) }
    }

    /// The shared skeleton: the subject, the deadline and the weight of evidence.
    fn of(goal: impl Into<String>, series: &SeriesView, target_date: Option<String>) -> Self {
        Self {
            goal: goal.into(),
            outlook: GoalOutlook::TooEarly,
            limiter: None,
            projected: None,
            target_date,
            readings: series.points.len() as u32,
        }
    }

    /// The outlook as one line, e.g.
    /// `Bench Press — off track: the trend is fine — the target date never allowed for it`.
    /// Plain text; styling stays at the renderer.
    pub fn line(&self) -> String {
        match self.limiter {
            Some(limiter) => format!("{} — {}: {}", self.goal, self.outlook.label(), limiter.reason()),
            None => format!("{} — {}", self.goal, self.outlook.label()),
        }
    }
}

/// The band the readings alone put a live goal in, before attendance has its say.
fn trend_outlook(series: &SeriesView, projected: Option<f64>, dated: bool) -> GoalOutlook {
    if series.points.len() < MIN_PROJECTION_READINGS {
        return GoalOutlook::TooEarly;
    }
    match (projected, dated) {
        // A dated goal with a rate: grade how much of the remaining gap it closes.
        (Some(value), _) => shortfall(series, value),
        // A dated goal whose readings gave no rate — nothing to extrapolate from.
        (None, true) => GoalOutlook::TooEarly,
        // Open-ended: there is no date to arrive by, so the only question is whether
        // it is moving the right way at all.
        (None, false) => match series.improving() {
            Some(true) => GoalOutlook::OnTrack,
            Some(false) => GoalOutlook::OffTrack,
            None => GoalOutlook::TooEarly,
        },
    }
}

/// Grade a projection that has a target to be measured against.
fn shortfall(series: &SeriesView, projected: f64) -> GoalOutlook {
    if series.reaches(projected) {
        return GoalOutlook::OnTrack;
    }
    match series.coverage(projected) {
        Some(covered) if covered >= AT_RISK_COVERAGE => GoalOutlook::AtRisk,
        _ => GoalOutlook::OffTrack,
    }
}

/// Let attendance dominate, and name the cause of anything short of on track.
///
/// Nobody is on track while they are not turning up, and missed sessions are a
/// verdict where a thin trend line is not: a drifting programme is enough to call a
/// goal at risk on its own, which is the [C4.4] signal doing the work the readings
/// cannot.
fn under_adherence(outlook: GoalOutlook, series: &SeriesView, adherence: Adherence) -> (GoalOutlook, Option<GoalLimiter>) {
    if adherence == Adherence::Drifting {
        let outlook = match outlook {
            GoalOutlook::OffTrack => GoalOutlook::OffTrack,
            _ => GoalOutlook::AtRisk,
        };
        return (outlook, Some(GoalLimiter::Attendance));
    }
    match outlook {
        GoalOutlook::AtRisk | GoalOutlook::OffTrack => (outlook, Some(limiter(series, adherence))),
        _ => (outlook, None),
    }
}

/// Which of the three things is in the way of a goal that is not landing.
fn limiter(series: &SeriesView, adherence: Adherence) -> GoalLimiter {
    match (adherence, series.improving()) {
        (Adherence::Drifting, _) => GoalLimiter::Attendance,
        // Turning up, moving the right way, and it still does not arrive: the target
        // or the date is what does not fit, and no amount of training fixes that.
        (_, Some(true)) => GoalLimiter::Ambition,
        _ => GoalLimiter::Performance,
    }
}

/// Progress against the user's goals ( `/progress` ): a headline plus the chartable
/// series behind it.
///
/// Holds nothing but [`SeriesView`]s, outlooks and text, so every chart in the app —
/// here, and in the post-session review — travels through the same contract rather
/// than growing a second one.
///
/// [C6.4] appended `goals`. The append-only rule in the crate root otherwise forbids
/// that for an existing type, and the exemption is the same one [R2.1] used on
/// `ProgrammeView`: `ProgressView` was introduced after 0.22.0 and has never been in
/// a release, so no deployed peer has ever decoded one and there are no old bytes to
/// misparse. Once it ships, this struct is closed like every other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressView {
    /// The server's one-line answer to "how am I doing", e.g. "2 of 3 goals on track".
    pub headline: String,
    pub series: Vec<SeriesView>,
    /// Free-text caveats — a goal with too little data to chart, a metric not logged
    /// recently enough to trend. Also where each goal's sentence lands, in the order
    /// its outlook appears in [`goals`](Self::goals).
    pub notes: Vec<String>,
    /// One outlook per goal, in the same order as the goal series ([C6.4]): the
    /// structured half of what `notes` says in prose, so a client — or the shortfall
    /// commentary in [C6.6] — reads a verdict and a cause rather than parsing English.
    pub goals: Vec<GoalOutlookView>,
}

impl ProgressView {
    /// The line every renderer leads with: the server's headline, or a count of what
    /// is enclosed when it did not set one (a view still has to announce itself).
    pub fn summary_line(&self) -> String {
        if self.headline.trim().is_empty() {
            let n = self.series.len();
            format!("Here's your progress ({n} {}).", if n == 1 { "chart" } else { "charts" })
        } else {
            self.headline.clone()
        }
    }
}

// ── /review ──────────────────────────────────────────────────────────────────────

/// Which tier of review this is — the wire record of the two-tier split in [C6.5].
///
/// Rides inside an appended `View` variant, so its own variant order is wire format
/// too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewKindView {
    /// An ad-hoc session: assembled entirely from the logged sets, with no model
    /// consulted. [`SessionReviewView::commentary`] is always `None` here, and clients
    /// may say so — "no LLM was asked" is a property worth being able to show.
    Summary,
    /// A programme session: the same deterministic stats, plus one commentary call
    /// grounded in the deltas the review already computed.
    Report,
}

/// The post-session review ([C6.5]): a snapshot of what a session was, taken when it
/// ended.
///
/// Everything except [`series`](Self::series) is a record of what was true at
/// generation time and is replayed verbatim by `/review` — a later edit to a set does
/// not rewrite it. The series are recomputed live on each render, because a chart of
/// the user's history is a current view of that history, not part of the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReviewView {
    /// The one-line verdict, e.g. "Solid session — 3 of 4 lifts at or above target".
    /// Always deterministic, never the model's words, in both tiers.
    pub headline: String,
    pub kind: ReviewKindView,
    /// When the session ended, as stored — clients display it as given.
    pub session_date: String,
    /// What the user said they were setting out to do, when they said anything.
    pub intent: Option<String>,
    /// How hard the session was, and whether the user confirmed that or the server
    /// derived it from the set-by-set difficulties.
    pub effort: Option<ReviewEffortView>,
    /// One line per exercise, in the order performed.
    pub exercises: Vec<ReviewExerciseView>,
    /// Personal records set *in this session* — never the user's all-time record list.
    pub records: Vec<ReviewRecordView>,
    /// The grounded commentary, present only for [`ReviewKindView::Report`]. `None` for
    /// an ad-hoc summary, which makes no model call at all.
    pub commentary: Option<String>,
    /// Where the session left each live goal, already rendered as display lines.
    pub goals: Vec<String>,
    /// Goals this session *completed* — persisted as achieved at generation time, so
    /// this list is the record of when it happened, not a re-derivation. Clients lead
    /// with these: finishing a goal is the one thing worth interrupting for.
    pub achieved_goals: Vec<String>,
    /// The programme slot this session filled, when it was run in programme mode.
    pub position: Option<TrainingModeView>,
    /// How the session tracked against its prescription, e.g. "4 of 5 prescribed
    /// exercises completed". `None` when no roster was bound to compare against.
    pub adherence: Option<String>,
    /// Consecutive days trained, including this session.
    pub streak_days: Option<u32>,
    /// The week so far, e.g. "3 sessions, 12,400 kg total volume".
    pub week_line: Option<String>,
    /// The charts behind the review — recomputed on every render, so they are the only
    /// part of this view that moves after the fact. The [C6.2] contract: this list must
    /// never grow a second series path.
    pub series: Vec<SeriesView>,
    /// Free-text caveats — a goal with too little data, a metric that could not be
    /// compared, a commentary call that failed.
    pub notes: Vec<String>,
    /// Set only on the review of the session that finished a programme ([R4.1]), which
    /// is one review in a hundred. Appended last: postcard is positional, so a field
    /// added anywhere else would reinterpret every review already on the wire.
    pub programme_complete: Option<ProgrammeCompleteView>,
}

impl SessionReviewView {
    /// The line every renderer leads with: the server's headline, or a self-announcing
    /// fallback when it somehow set none.
    pub fn summary_line(&self) -> String {
        if self.headline.trim().is_empty() {
            let n = self.exercises.len();
            format!("Here's your session review ({n} {}).", if n == 1 { "exercise" } else { "exercises" })
        } else {
            self.headline.clone()
        }
    }
}

/// The programme this session finished ([R4.1]) — step 6 of the North Star, achieved branch.
///
/// The server decides completion deterministically and stamps the programme completed before
/// building this; a client that sees it can state the programme is over as fact.
///
/// The counts are carried rather than a ready-made compliment because the three ways a
/// programme ends are not equally good news. One that reached every goal it served has earned
/// the congratulation; one whose target end date simply arrived with four of twenty-four
/// sessions trained has not, and [`Self::verdict`] says so in the same words either way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgrammeCompleteView {
    pub title: String,
    /// What ended it, phrased as the completion check decided it — "every goal it served is
    /// reached", "every session in the plan is settled", "its target end date has passed".
    pub reason: String,
    /// Slots of the grid actually trained, out of every slot it had.
    pub trained: u32,
    pub total: u32,
    /// The goals the programme served that were reached, as display lines — the same
    /// `"<exercise> to <target>"` shape [`SessionReviewView::achieved_goals`] uses.
    pub achieved_goals: Vec<String>,
}

impl ProgrammeCompleteView {
    /// The banner a review leads with, e.g. `Programme complete: 12-week hypertrophy`.
    pub fn banner(&self) -> String {
        format!("Programme complete: {}", self.title)
    }

    /// How it actually went, in one line: the reason it ended and the adherence behind that,
    /// e.g. `Every goal it served is reached — 22 of 24 sessions trained.`
    pub fn verdict(&self) -> String {
        let mut reason = self.reason.clone();
        if let Some(first) = reason.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        format!("{reason} — {} of {} sessions trained.", self.trained, self.total)
    }
}

/// How hard a session was, plus where that verdict came from.
///
/// The provenance is carried rather than inferred because it changes what a client
/// should say: a derived effort is a guess the user has not seen yet and is worth
/// offering back for correction, a confirmed one is theirs and must not be
/// second-guessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEffortView {
    /// "easy", "medium", "hard", "failure".
    pub label: String,
    /// `true` when the user told us; `false` when the server derived it from the
    /// perceived difficulty of the logged sets (the auto-close path).
    pub confirmed: bool,
}

impl ReviewEffortView {
    /// The effort line, e.g. `Effort: hard` or `Effort: hard (my read, not yours)`.
    pub fn line(&self) -> String {
        match self.confirmed {
            true => format!("Effort: {}", self.label),
            false => format!("Effort: {} (my read, not yours)", self.label),
        }
    }
}

/// One exercise as it was actually performed, against what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewExerciseView {
    pub name: String,
    /// The roster's prescription, e.g. "3 sets × 6 reps @ 65kg". `None` when the
    /// session ran without a roster, or when this exercise was not on it.
    pub prescribed: Option<String>,
    /// What was logged, e.g. "3 sets × 6 reps @ 67.5kg".
    pub actual: String,
    /// How the two differ, e.g. "+2.5kg" or "1 set short". `None` when there was no
    /// prescription to differ from, or when the session matched it exactly.
    pub delta: Option<String>,
}

impl ReviewExerciseView {
    /// The per-exercise line, e.g. `Bench Press: 3 sets × 6 reps @ 67.5kg (asked 65kg) — +2.5kg`.
    /// Plain text; styling and escaping stay at the renderer.
    pub fn line(&self) -> String {
        let base = format!("{}: {}", self.name, self.actual);
        match (&self.prescribed, &self.delta) {
            (Some(p), Some(d)) => format!("{base} (asked {p}) — {d}"),
            (Some(p), None) => format!("{base} (as prescribed: {p})"),
            (None, _) => base,
        }
    }
}

/// A personal record set during the session under review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRecordView {
    pub exercise: String,
    /// The new best, already formatted with its unit, e.g. "100 kg × 5".
    pub detail: String,
    /// The best that stood before it, when there was one. Absent for a first-ever
    /// logged effort, which is a record only in the trivial sense.
    pub previous: Option<String>,
}

impl ReviewRecordView {
    /// The record line, e.g. `Bench Press: 100 kg × 5 (was 97.5 kg × 5)`.
    pub fn line(&self) -> String {
        match &self.previous {
            Some(prev) => format!("{}: {} (was {prev})", self.exercise, self.detail),
            None => format!("{}: {} (first logged)", self.exercise, self.detail),
        }
    }
}

// ── /programme status ────────────────────────────────────────────────────────────

/// The full report on a live programme ([C4.6]): the skeleton being walked, how well it
/// is being kept to, and where the goals it serves are heading.
///
/// The three questions §2 asks of a programme, in the order a user asks them: *where am
/// I* (the [`ProgrammeView`]'s own [`status`](ProgrammeView::status)), *am I keeping to
/// it* ([`adherence`](Self::adherence)), *is it working* ([`goals`](Self::goals)).
///
/// The programme travels whole rather than restated field by field: the position half
/// already ships inside a [`ProgrammeView`] ([R2.1]), and a client that can render one
/// needs no second layout for the same nine fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgrammeProgressView {
    /// The programme being reported on. Always live and always carrying a
    /// [`ProgrammeStatusView`] — a draft has nowhere to be and nothing to adhere to.
    pub programme: ProgrammeView,
    /// How well the repeating week is actually being kept to.
    pub adherence: ProgrammeAdherenceView,
    /// One [`SeriesShape::Trajectory`] per goal the programme serves, highest priority
    /// first — the [C6.2] contract, so no client needs new chart code to plot them.
    ///
    /// The readings and the target, nothing more: what they are *likely* to add up to is
    /// a separate question ([C6.4]) with its own answer, and is deliberately not folded
    /// into the numbers here.
    pub goals: Vec<SeriesView>,
}

impl ProgrammeProgressView {
    /// The line every renderer leads with, e.g. "12-week hypertrophy — Week 3 of 12 —
    /// accumulation". Falls back to the programme's shape if it somehow arrives without a
    /// position, so the view still announces itself.
    pub fn summary_line(&self) -> String {
        let tail = self.programme.position_line().unwrap_or_else(|| self.programme.shape_line());
        format!("{} — {tail}", self.programme.title)
    }
}

/// How well a live programme is being kept to ([C4.6]), read off the drift [C4.4] already
/// measured rather than re-derived here.
///
/// Counts *settled* slots — those the calendar has closed — because a slot still ahead has
/// been neither kept nor missed, and counting it as either would report every programme as
/// failing until its final week.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgrammeAdherenceView {
    /// Slots the calendar has closed: trained, missed and skipped together.
    pub settled: u32,
    /// Of those, the ones actually trained.
    pub trained: u32,
    /// Every recurring day whose misses are a standing pattern rather than one bad week,
    /// in day order. Empty while the programme is being kept to.
    pub drifting_days: Vec<DayAdherenceView>,
    /// What to do about that pattern, or `None` when nothing needs moving.
    pub reschedule: Option<RescheduleView>,
}

impl ProgrammeAdherenceView {
    /// Share of the settled slots that were trained, `0.0..=1.0`.
    ///
    /// `None` before the calendar has closed a single slot: a programme starting today has
    /// missed nothing, and "0%" would read as a failure the user has not yet had the
    /// chance to have.
    pub fn rate(&self) -> Option<f64> {
        (self.settled > 0).then(|| f64::from(self.trained) / f64::from(self.settled))
    }

    /// The adherence line, e.g. "Trained 5 of the 6 sessions due so far (83%)". Shared by
    /// every client renderer so the figure reads identically everywhere.
    pub fn rate_line(&self) -> String {
        match self.rate() {
            Some(rate) => format!("Trained {} of the {} sessions due so far ({:.0}%)", self.trained, self.settled, rate * 100.0),
            None => "Nothing has come due yet.".to_string(),
        }
    }
}

/// One recurring training day of the programme's repeating week, rolled up over every
/// settled slot that shares it ([C4.4]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayAdherenceView {
    /// 1-based ordinal training day within the week, never a calendar weekday — the same
    /// index [`ProgrammeDayView`] carries.
    pub day_idx: u32,
    /// The day's focus, shared across every week it repeats in.
    pub focus: String,
    pub trained: u32,
    pub missed: u32,
}

impl DayAdherenceView {
    /// The per-day line, e.g. "Day 1 (upper): 1 of 4 trained".
    pub fn line(&self) -> String {
        format!("Day {} ({}): {} of {} trained", self.day_idx, self.focus, self.trained, self.trained + self.missed)
    }
}

/// What to do about a day the user keeps missing ([C4.4]) — shift it, drop it, or compress
/// what is left — as a decision a client can act on rather than only a sentence to print.
///
/// Rides inside an appended `View` variant, so its own variant order is wire format too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RescheduleView {
    /// One recurring day is repeatedly missed while the rest of the week holds: move it to
    /// a day the user can make. Weekly volume and the programme's span both unchanged.
    Shift { day_idx: u32, focus: String },
    /// The user is realistically training fewer days a week than the plan asks: drop the
    /// worst-adhered day from the weeks ahead. Lower weekly volume, same span.
    Drop { day_idx: u32, focus: String },
    /// The user is behind and the target end date is closing in: consolidate the remaining
    /// work into the weeks that are left.
    Compress,
}

impl RescheduleView {
    /// The offer to put to the user, in words — a PT moving leg day, not a scold, and
    /// always an offer rather than a change already made. Lives here so every client makes
    /// the same one.
    pub fn offer(&self) -> String {
        match self {
            Self::Shift { day_idx, focus } => format!(
                "You keep missing day {day_idx} ({focus}) -- that usually means the day is wrong, not the effort. \
                 Tell me a day that works and I'll shift it."
            ),
            Self::Drop { day_idx, focus } => format!(
                "Day {day_idx} ({focus}) keeps slipping -- it may be one day a week more than fits your life right now. \
                 Say the word and I'll drop it from the weeks ahead."
            ),
            Self::Compress => "You're behind where the calendar expected and the end date is close. I can compress what's left into \
                 the weeks that remain so it still lands on time -- want me to?"
                .to_string(),
        }
    }
}

/// Render a change with an explicit sign, so "+2.5" and "-2.5" read as movement
/// rather than as values.
fn signed(v: f64) -> String {
    if v > 0.0 { format!("+{}", trim_decimal(v)) } else { trim_decimal(v) }
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
            View::Progress(progress_view()),
            // A headline the server left empty is the case that would otherwise
            // fall through to an empty message.
            View::Progress(ProgressView { headline: String::new(), series: vec![], notes: vec![], goals: vec![] }),
            View::SessionReview(Box::new(session_review_view())),
            View::SessionReview(Box::new(SessionReviewView { headline: String::new(), ..session_review_view() })),
            View::ProgrammeProgress(Box::new(programme_progress_view())),
        ];
        for view in &views {
            assert!(!view.fallback_text().is_empty(), "empty fallback for {view:?}");
        }
    }

    /// A representative progress payload, shared by the tests below and by `lib.rs`.
    pub(crate) fn progress_view() -> ProgressView {
        ProgressView {
            headline: "2 of 3 goals on track".into(),
            series: vec![trend(), bodyweight(), breakdown()],
            notes: vec!["Squat has too few sessions to trend yet.".into()],
            goals: vec![
                GoalOutlookView::assess("Bench Press", &trend(), Some(120.5), Some("2026-12-31".into()), Adherence::Keeping),
                GoalOutlookView::assess("Squat", &sparse(), None, Some("2026-12-31".into()), Adherence::Unprogrammed),
            ],
        }
    }

    /// A representative programme-mode review, shared by the tests below and by
    /// `lib.rs`. Deliberately the *fuller* tier: it carries commentary, a programme
    /// position and an achieved goal, so a roundtrip exercises every optional field.
    pub(crate) fn session_review_view() -> SessionReviewView {
        SessionReviewView {
            headline: "Strong session — every lift at or above target".into(),
            kind: ReviewKindView::Report,
            session_date: "2026-07-19".into(),
            intent: Some("upper push".into()),
            effort: Some(ReviewEffortView { label: "hard".into(), confirmed: true }),
            exercises: vec![
                ReviewExerciseView {
                    name: "Bench Press".into(),
                    prescribed: Some("3 sets × 6 reps @ 65kg".into()),
                    actual: "3 sets × 6 reps @ 67.5kg".into(),
                    delta: Some("+2.5kg".into()),
                },
                ReviewExerciseView {
                    name: "Overhead Press".into(),
                    prescribed: Some("3 sets × 8 reps @ 40kg".into()),
                    actual: "3 sets × 8 reps @ 40kg".into(),
                    delta: None,
                },
            ],
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
            adherence: Some("2 of 2 prescribed exercises completed".into()),
            streak_days: Some(4),
            week_line: Some("3 sessions, 12400 kg total volume".into()),
            series: vec![trend()],
            notes: vec!["Squat has too few sessions to trend yet.".into()],
            programme_complete: None,
        }
    }

    /// The review of the session that finished a programme, every goal it served reached.
    pub(crate) fn completed_review_view() -> SessionReviewView {
        SessionReviewView {
            programme_complete: Some(ProgrammeCompleteView {
                title: "12-week hypertrophy".into(),
                reason: "every goal it served is reached".into(),
                trained: 22,
                total: 24,
                achieved_goals: vec!["Overhead Press to 40kg".into()],
            }),
            ..session_review_view()
        }
    }

    /// The banner states the programme is over; the verdict states how it went, and the
    /// adherence rides along so the wording cannot flatter a programme the calendar merely
    /// ran out on.
    #[test]
    fn a_completed_programme_reports_its_adherence_alongside_its_reason() {
        let won = completed_review_view().programme_complete.expect("the completion");
        assert_eq!(won.banner(), "Programme complete: 12-week hypertrophy");
        assert_eq!(won.verdict(), "Every goal it served is reached — 22 of 24 sessions trained.");

        let ran_out = ProgrammeCompleteView {
            reason: "its target end date has passed".into(),
            trained: 4,
            achieved_goals: vec![],
            ..won
        };
        assert_eq!(ran_out.verdict(), "Its target end date has passed — 4 of 24 sessions trained.");
    }

    /// The ad-hoc tier: no commentary, no programme position, no prescription to
    /// measure against. The shape the no-LLM path produces.
    pub(crate) fn adhoc_review_view() -> SessionReviewView {
        SessionReviewView {
            headline: "Session logged — 1 exercise".into(),
            kind: ReviewKindView::Summary,
            commentary: None,
            position: None,
            adherence: None,
            achieved_goals: vec![],
            exercises: vec![ReviewExerciseView {
                name: "Bench Press".into(),
                prescribed: None,
                actual: "3 sets × 6 reps @ 67.5kg".into(),
                delta: None,
            }],
            ..session_review_view()
        }
    }

    /// The two tiers differ in exactly one visible way: whether a model was asked.
    /// A `Summary` carrying commentary would mean the ad-hoc path had made an LLM
    /// call, which is the safety property [C6.5] is built around.
    #[test]
    fn an_adhoc_summary_carries_no_commentary() {
        let adhoc = adhoc_review_view();
        assert_eq!(adhoc.kind, ReviewKindView::Summary);
        assert!(adhoc.commentary.is_none(), "the ad-hoc tier must never carry model prose");
        assert!(session_review_view().commentary.is_some(), "the programme tier grounds one commentary call");
    }

    /// The per-exercise line is the review's core sentence, and it reads differently
    /// in each of its three states.
    #[test]
    fn exercise_lines_report_the_delta_against_the_prescription() {
        let beat = &session_review_view().exercises[0];
        assert_eq!(beat.line(), "Bench Press: 3 sets × 6 reps @ 67.5kg (asked 3 sets × 6 reps @ 65kg) — +2.5kg");

        let matched = &session_review_view().exercises[1];
        assert_eq!(matched.line(), "Overhead Press: 3 sets × 8 reps @ 40kg (as prescribed: 3 sets × 8 reps @ 40kg)");

        let unrostered = &adhoc_review_view().exercises[0];
        assert_eq!(unrostered.line(), "Bench Press: 3 sets × 6 reps @ 67.5kg", "nothing prescribed, nothing to compare");
    }

    /// A record is only impressive against the one it beat, and a first-ever effort
    /// must not be dressed up as one.
    #[test]
    fn record_lines_name_what_they_beat() {
        assert_eq!(session_review_view().records[0].line(), "Bench Press: 67.5 kg × 6 (was 65 kg × 6)");

        let first = ReviewRecordView { exercise: "Dips".into(), detail: "12 reps".into(), previous: None };
        assert_eq!(first.line(), "Dips: 12 reps (first logged)");
    }

    /// A derived effort is a guess the user has not confirmed — the auto-close path's
    /// whole reason for offering it back.
    #[test]
    fn effort_line_admits_when_it_is_a_guess() {
        assert_eq!(ReviewEffortView { label: "hard".into(), confirmed: true }.line(), "Effort: hard");
        assert_eq!(ReviewEffortView { label: "hard".into(), confirmed: false }.line(), "Effort: hard (my read, not yours)");
    }

    #[test]
    fn review_summary_falls_back_to_a_count() {
        let blank = SessionReviewView { headline: "  ".into(), ..session_review_view() };
        assert_eq!(blank.summary_line(), "Here's your session review (2 exercises).");

        let one = SessionReviewView { headline: String::new(), ..adhoc_review_view() };
        assert_eq!(one.summary_line(), "Here's your session review (1 exercise).");
    }

    fn points(raw: &[(&str, f64)]) -> Vec<SeriesPointView> {
        raw.iter().map(|(label, value)| SeriesPointView { label: (*label).into(), value: *value }).collect()
    }

    /// Exercise progression: up is better.
    fn trend() -> SeriesView {
        SeriesView {
            title: "Bench Press — top set".into(),
            unit: "kg".into(),
            better: Direction::Higher,
            shape: SeriesShape::Trajectory { target: 100.0 },
            points: points(&[("2026-05-01", 80.0), ("2026-06-01", 85.0), ("2026-07-01", 92.5)]),
        }
    }

    /// A body metric on a cut: down is better. The series everyone renders backwards.
    fn bodyweight() -> SeriesView {
        SeriesView {
            title: "Bodyweight".into(),
            unit: "kg".into(),
            better: Direction::Lower,
            shape: SeriesShape::Trend,
            points: points(&[("2026-05-01", 90.0), ("2026-06-01", 87.5)]),
        }
    }

    /// Two readings a fortnight apart: real movement, and still not a trend.
    fn sparse() -> SeriesView {
        SeriesView { points: points(&[("2026-06-01", 80.0), ("2026-06-15", 85.0)]), ..trend() }
    }

    /// Muscle-group volume: buckets, not a timeline.
    fn breakdown() -> SeriesView {
        SeriesView {
            title: "Weekly volume by muscle group".into(),
            unit: "sets".into(),
            better: Direction::Neutral,
            shape: SeriesShape::Breakdown,
            points: points(&[("Chest", 12.0), ("Back", 16.0), ("Legs", 9.0)]),
        }
    }

    /// The whole point of carrying `better` on the wire: the same downward movement is
    /// progress for a cut and a regression for a lift.
    #[test]
    fn improving_is_direction_aware() {
        assert_eq!(trend().improving(), Some(true), "load going up is progress");
        assert_eq!(bodyweight().improving(), Some(true), "weight coming down is progress on a cut");

        let mut regressed = trend();
        regressed.points = points(&[("2026-05-01", 92.5), ("2026-06-01", 85.0)]);
        assert_eq!(regressed.improving(), Some(false));

        let mut gaining = bodyweight();
        gaining.points = points(&[("2026-05-01", 87.5), ("2026-06-01", 90.0)]);
        assert_eq!(gaining.improving(), Some(false));
    }

    /// `None` is "no verdict", and the three ways to get there must not be confused
    /// with "not improving" — a client that renders them as a regression is lying.
    #[test]
    fn improving_withholds_a_verdict_when_it_has_none() {
        let mut flat = bodyweight();
        flat.points = points(&[("2026-05-01", 90.0), ("2026-06-01", 90.0)]);
        assert_eq!(flat.improving(), None, "no movement");

        let mut single = bodyweight();
        single.points = points(&[("2026-05-01", 90.0)]);
        assert_eq!(single.improving(), None, "one point is not a trend");

        assert_eq!(breakdown().improving(), None, "a neutral breakdown has no better end");
    }

    /// Breakdown buckets are not ordered in time, so last-minus-first is arithmetic
    /// with no meaning — better withheld than reported as a trend.
    #[test]
    fn a_breakdown_reports_no_change() {
        assert_eq!(breakdown().change(), None);
        assert_eq!(breakdown().change_line(), "");
        assert_eq!(trend().change(), Some(12.5));
    }

    /// A trajectory's target has to sit inside the axis range or the reference line
    /// is drawn off the chart.
    #[test]
    fn bounds_make_room_for_a_trajectory_target() {
        assert_eq!(trend().bounds(), Some((80.0, 100.0)), "target 100 widens the 80..92.5 readings");
        assert_eq!(bodyweight().bounds(), Some((87.5, 90.0)), "a plain trend spans its readings only");

        let empty = SeriesView { points: vec![], ..trend() };
        assert_eq!(empty.bounds(), None);
    }

    /// The sparkline plots the readings as recorded; the verdict on them is
    /// `improving()`'s job, not a transform of the geometry.
    #[test]
    fn spark_draws_the_readings_as_they_are() {
        let mut rising = trend();
        rising.points = points(&[("a", 0.0), ("b", 4.0), ("c", 8.0)]);
        assert_eq!(rising.spark(), "▁▅█");

        // Falling bodyweight is progress, and still slopes down.
        let mut falling = bodyweight();
        falling.points = points(&[("a", 8.0), ("b", 4.0), ("c", 0.0)]);
        assert_eq!(falling.improving(), Some(true));
        assert_eq!(falling.spark(), "█▅▁");
    }

    #[test]
    fn spark_handles_flat_and_empty_series() {
        let mut flat = bodyweight();
        flat.points = points(&[("a", 70.0), ("b", 70.0), ("c", 70.0)]);
        assert_eq!(flat.spark(), "▄▄▄", "a flat series is a mid-height band, not a divide by zero");

        let empty = SeriesView { points: vec![], ..bodyweight() };
        assert_eq!(empty.spark(), "");
    }

    #[test]
    fn change_line_reads_for_humans() {
        assert_eq!(trend().change_line(), "80 → 92.5 kg (+12.5, better)");
        assert_eq!(bodyweight().change_line(), "90 → 87.5 kg (-2.5, better)");

        let mut unitless = trend();
        unitless.unit = String::new();
        unitless.better = Direction::Neutral;
        unitless.points = points(&[("a", 3.0), ("b", 5.0)]);
        assert_eq!(unitless.change_line(), "3 → 5 (+2)", "no unit, and no verdict on a neutral series");
    }

    #[test]
    fn value_target_and_latest_labels() {
        let s = trend();
        assert_eq!(s.value_label(92.5), "92.5 kg");
        assert_eq!(s.target_line(), "Target: 100 kg");
        assert_eq!(s.latest_line(), "92.5 kg (2026-07-01)");

        // A plain trend aims at nothing, and a unitless series prints a bare number.
        assert_eq!(bodyweight().target_line(), "");
        let bare = SeriesView { unit: String::new(), ..breakdown() };
        assert_eq!(bare.value_label(9.0), "9");

        let empty = SeriesView { points: vec![], ..trend() };
        assert_eq!(empty.latest_line(), "");
    }

    #[test]
    fn progress_summary_falls_back_to_a_count() {
        assert_eq!(progress_view().summary_line(), "2 of 3 goals on track");

        let one = ProgressView { headline: "  ".into(), series: vec![trend()], notes: vec![], goals: vec![] };
        assert_eq!(one.summary_line(), "Here's your progress (1 chart).");

        let none = ProgressView { headline: String::new(), series: vec![], notes: vec![], goals: vec![] };
        assert_eq!(none.summary_line(), "Here's your progress (0 charts).");
    }

    // ── goal outlook ([C6.4]) ────────────────────────────────────────────────

    /// A three-reading trend that clears its target is the one case the four bands
    /// are allowed to say "yes" to, and nothing is in its way.
    #[test]
    fn a_trend_that_arrives_reads_as_on_track() {
        let outlook = GoalOutlookView::assess("Bench Press", &trend(), Some(120.5), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(outlook.outlook, GoalOutlook::OnTrack);
        assert_eq!(outlook.limiter, None, "nothing is in the way of a goal that lands");
        assert_eq!(outlook.projected, Some(120.5));
        assert_eq!(outlook.readings, 3);
        assert_eq!(outlook.line(), "Bench Press — on track");
    }

    /// Two points are a line through two accidents. The refusal has to live in the
    /// model: the caller may hand over a projection and it is still dropped, because
    /// a client that receives a number will show it.
    #[test]
    fn two_readings_are_too_early_to_say_however_good_they_look() {
        let outlook = GoalOutlookView::assess("Bench Press", &sparse(), Some(140.0), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(outlook.outlook, GoalOutlook::TooEarly);
        assert_eq!(outlook.projected, None, "the number must not travel — a client would render it");
        assert_eq!(outlook.limiter, None, "an unknown cause is not a diagnosed one");
        assert_eq!(outlook.readings, 2);
        assert!(!outlook.outlook.is_projection());
        assert_eq!(outlook.line(), "Bench Press — too early to say");
    }

    /// The sparsest case of all, and the one a user sees on day one: a goal with
    /// nothing logged against it is not failing, it has not started.
    #[test]
    fn a_goal_with_no_readings_is_too_early_not_off_track() {
        let empty = SeriesView { points: vec![], ..trend() };
        let outlook = GoalOutlookView::assess("Squat", &empty, None, Some("2026-12-31".into()), Adherence::Unprogrammed);
        assert_eq!(outlook.outlook, GoalOutlook::TooEarly);
        assert_eq!(outlook.readings, 0);
        assert_eq!(outlook.projected, None);
    }

    /// Enough readings, but bunched inside a day or two, so the caller could give no
    /// rate. "No rate" and "a rate that misses" are different answers.
    #[test]
    fn readings_that_give_no_rate_are_too_early_too() {
        let bunched = SeriesView { points: points(&[("2026-06-01", 80.0), ("2026-06-01", 82.5), ("2026-06-01", 85.0)]), ..trend() };
        let outlook = GoalOutlookView::assess("Bench Press", &bunched, None, Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(outlook.outlook, GoalOutlook::TooEarly, "three points inside one day are one session, not a trend");
    }

    /// A near miss and a hopeless one are different news, and the split is a stated
    /// fraction of the remaining gap rather than a feeling.
    #[test]
    fn a_near_miss_is_at_risk_and_a_wide_one_is_off_track() {
        let near = GoalOutlookView::assess("Bench Press", &trend(), Some(98.5), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(near.outlook, GoalOutlook::AtRisk, "92.5 → 98.5 closes 80% of the 7.5 kg still owed");
        assert_eq!(near.projected, Some(98.5), "a projection that is a claim still travels");

        let wide = GoalOutlookView::assess("Bench Press", &trend(), Some(93.0), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(wide.outlook, GoalOutlook::OffTrack, "and 92.5 → 93 closes 7% of it");
    }

    /// The user is turning up and improving, and the sum still does not work: that is
    /// the goal's fault, not theirs, and [C6.6] has to be able to say so.
    #[test]
    fn a_goal_the_time_never_allowed_for_blames_the_goal() {
        let wide = GoalOutlookView::assess("Bench Press", &trend(), Some(93.0), Some("2026-08-01".into()), Adherence::Keeping);
        assert_eq!(wide.limiter, Some(GoalLimiter::Ambition));
        assert_eq!(wide.line(), "Bench Press — off track: the trend is fine — the target date never allowed for it");
    }

    /// Turning up and going backwards is a training problem, and must not be
    /// confused with the two causes that are not.
    #[test]
    fn a_trend_going_the_wrong_way_blames_the_training() {
        let regressed = SeriesView { points: points(&[("2026-05-01", 92.5), ("2026-06-01", 88.0), ("2026-07-01", 85.0)]), ..trend() };
        let outlook = GoalOutlookView::assess("Bench Press", &regressed, Some(80.0), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(outlook.outlook, GoalOutlook::OffTrack);
        assert_eq!(outlook.limiter, Some(GoalLimiter::Performance));
    }

    /// Adherence dominates: the readings say this lands, the diary says the user is
    /// not there for it. Nobody is on track while they are missing sessions.
    #[test]
    fn a_drifting_programme_pulls_a_landing_trend_back_to_at_risk() {
        let kept = GoalOutlookView::assess("Bench Press", &trend(), Some(120.5), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(kept.outlook, GoalOutlook::OnTrack);

        let drifting = GoalOutlookView::assess("Bench Press", &trend(), Some(120.5), Some("2026-12-31".into()), Adherence::Drifting);
        assert_eq!(drifting.outlook, GoalOutlook::AtRisk);
        assert_eq!(drifting.limiter, Some(GoalLimiter::Attendance), "attendance outranks the trend line");
        assert_eq!(drifting.projected, Some(120.5), "the projection is still a claim, and still travels");
    }

    /// Missed sessions are a verdict where a thin trend is not: not turning up is
    /// evidence in its own right, so "too early to say" is not the last word when
    /// the diary already says something.
    #[test]
    fn drift_speaks_where_the_readings_cannot() {
        let outlook = GoalOutlookView::assess("Squat", &sparse(), None, Some("2026-12-31".into()), Adherence::Drifting);
        assert_eq!(outlook.outlook, GoalOutlook::AtRisk);
        assert_eq!(outlook.limiter, Some(GoalLimiter::Attendance));
        assert_eq!(outlook.projected, None, "and it still invents no number");
    }

    /// An open-ended goal has no date to arrive by, so the only honest question is
    /// whether it is moving the right way — read off `improving()`, never a slope.
    #[test]
    fn an_open_ended_goal_is_judged_on_movement_alone() {
        let rising = GoalOutlookView::assess("Bench Press", &trend(), None, None, Adherence::Unprogrammed);
        assert_eq!(rising.outlook, GoalOutlook::OnTrack);
        assert_eq!(rising.target_date, None);

        let flat = SeriesView { points: points(&[("2026-05-01", 90.0), ("2026-06-01", 90.0), ("2026-07-01", 90.0)]), ..trend() };
        let stalled = GoalOutlookView::assess("Bench Press", &flat, None, None, Adherence::Unprogrammed);
        assert_eq!(stalled.outlook, GoalOutlook::TooEarly, "no movement is no verdict, not a bad one");
    }

    /// A settled goal is a record. Achieved names no cause; missed still does, so a
    /// post-mortem has something to work from.
    #[test]
    fn settled_goals_carry_records_not_forecasts() {
        let done = GoalOutlookView::achieved("Overhead Press", &trend(), Some("2026-12-31".into()));
        assert!(done.outlook.is_settled() && !done.outlook.is_projection());
        assert_eq!(done.limiter, None);
        assert_eq!(done.projected, None, "there is nothing left to project");

        let missed = GoalOutlookView::missed("Squat", &sparse(), Some("2026-01-01".into()), Adherence::Drifting);
        assert!(missed.outlook.is_settled());
        assert_eq!(missed.limiter, Some(GoalLimiter::Attendance), "a post-mortem still names what went wrong");
    }

    /// Direction-awareness runs through the projection too: on a cut, under the
    /// target is success and coverage is measured from the same end.
    #[test]
    fn reaching_and_coverage_are_direction_aware() {
        let cut = SeriesView { shape: SeriesShape::Trajectory { target: 80.0 }, ..bodyweight() };
        assert!(cut.reaches(79.0), "under the target is success for a cut");
        assert!(!cut.reaches(81.0));
        assert!((cut.coverage(85.0).unwrap() - 1.0 / 3.0).abs() < 1e-9, "87.5 → 85 of a 7.5 kg gap");
        assert!(cut.coverage(90.0).unwrap() < 0.0, "moving away from the target covers less than nothing");

        assert!(trend().reaches(100.0), "hitting a rising target exactly counts");
        assert_eq!(bodyweight().coverage(85.0), None, "a plain trend has no target to cover");
        assert!(!breakdown().reaches(20.0), "a series with no better end never arrives");
    }

    /// A cut whose weight is falling nicely and still not fast enough lands in the
    /// same bands as a lift — the arithmetic must not flip with the direction.
    #[test]
    fn a_cut_that_misses_its_date_is_graded_like_any_other_goal() {
        let cut = SeriesView { shape: SeriesShape::Trajectory { target: 80.0 }, ..bodyweight() };
        let near = GoalOutlookView::assess("Bodyweight", &cut, Some(82.0), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(near.outlook, GoalOutlook::TooEarly, "two weigh-ins are still two points");

        let three = SeriesView { points: points(&[("2026-05-01", 90.0), ("2026-06-01", 88.0), ("2026-07-01", 87.5)]), ..cut };
        let outlook = GoalOutlookView::assess("Bodyweight", &three, Some(81.5), Some("2026-12-31".into()), Adherence::Keeping);
        assert_eq!(outlook.outlook, GoalOutlook::AtRisk, "87.5 → 81.5 sheds 80% of the 7.5 kg still owed");
        assert_eq!(outlook.limiter, Some(GoalLimiter::Ambition), "the weight is coming off; the date is the problem");
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
            status: None,
        }
    }

    /// The same programme, live and reported on — what `/programme status` emits.
    pub(crate) fn programme_status_view() -> ProgrammeView {
        ProgrammeView {
            active: true,
            status: Some(ProgrammeStatusView {
                current_week: 3,
                block_focus: Some("accumulation".into()),
                next_slot: Some(ProgrammeSlotView { week_idx: 3, day_idx: 2, focus: "lower".into() }),
                trained: 5,
                missed: 1,
                skipped: 0,
                remaining: 12,
            }),
            ..programme_view()
        }
    }

    /// The full [C4.6] report on that same live programme: its position, the day it keeps
    /// missing, and the goal it serves as a trajectory.
    pub(crate) fn programme_progress_view() -> ProgrammeProgressView {
        ProgrammeProgressView {
            programme: programme_status_view(),
            adherence: ProgrammeAdherenceView {
                settled: 6,
                trained: 5,
                drifting_days: vec![DayAdherenceView { day_idx: 1, focus: "upper".into(), trained: 1, missed: 3 }],
                reschedule: Some(RescheduleView::Shift { day_idx: 1, focus: "upper".into() }),
            },
            goals: vec![trend()],
        }
    }

    #[test]
    fn programme_progress_lines_read_for_humans() {
        let view = programme_progress_view();
        assert_eq!(view.summary_line(), "12-week hypertrophy — Week 3 of 12 — accumulation");
        assert_eq!(view.adherence.rate_line(), "Trained 5 of the 6 sessions due so far (83%)");
        assert_eq!(view.adherence.drifting_days[0].line(), "Day 1 (upper): 1 of 4 trained");

        let offer = view.adherence.reschedule.unwrap().offer();
        assert!(offer.contains("missing day 1 (upper)"), "the offer must name the day: {offer}");
        assert!(offer.contains("shift it"), "and offer to move it rather than scold: {offer}");
    }

    /// A programme that started today has missed nothing, and "0%" would report a failure
    /// the user has not yet had the chance to have.
    #[test]
    fn adherence_withholds_a_rate_until_something_has_settled() {
        let fresh = ProgrammeAdherenceView { settled: 0, trained: 0, drifting_days: vec![], reschedule: None };
        assert_eq!(fresh.rate(), None);
        assert_eq!(fresh.rate_line(), "Nothing has come due yet.");

        let missed_everything = ProgrammeAdherenceView { settled: 4, trained: 0, ..fresh };
        assert_eq!(missed_everything.rate(), Some(0.0), "settled and untrained is a real 0%, not an absent reading");
    }

    /// Goals ride in as trajectories and nothing more: the target and the readings, with
    /// the question of where they land left to [C6.4].
    #[test]
    fn programme_goals_travel_as_plain_trajectories() {
        let view = programme_progress_view();
        assert_eq!(view.goals[0].shape, SeriesShape::Trajectory { target: 100.0 });
        assert_eq!(view.goals[0].target_line(), "Target: 100 kg");
        assert_eq!(view.goals[0].change_line(), "80 → 92.5 kg (+12.5, better)");
    }

    #[test]
    fn status_lines_read_for_humans() {
        let view = programme_status_view();
        assert_eq!(view.position_line().as_deref(), Some("Week 3 of 12 — accumulation"));
        let status = view.status.unwrap();
        assert_eq!(status.counts_line(), "5 trained · 1 missed · 0 skipped · 12 to go");
        assert_eq!(status.next_slot.unwrap().label(), "Week 3, day 2: lower");
    }

    /// A week between blocks has no focus to name, and a proposed programme has no
    /// position at all — neither may render as an empty or half-written line.
    #[test]
    fn position_line_degrades_without_a_block_and_without_a_status() {
        let mut view = programme_status_view();
        view.status.as_mut().unwrap().block_focus = None;
        assert_eq!(view.position_line().as_deref(), Some("Week 3 of 12"));
        assert_eq!(programme_view().position_line(), None, "a draft has nowhere to be");
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

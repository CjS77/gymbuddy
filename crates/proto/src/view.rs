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
    /// (`/programme status`) rather than proposed. A freshly designed programme has no
    /// position to report, so this stays `None` through the whole `/programme` interview.
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
/// ratios and no drift verdict. Those are [C4.4]'s, and will be built on these numbers
/// rather than beside them.
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
}

/// Progress against the user's goals ( `/progress` ): a headline plus the chartable
/// series behind it.
///
/// Holds nothing but [`SeriesView`]s and text, so every chart in the app — here, and
/// in the post-session review — travels through the same contract rather than
/// growing a second one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressView {
    /// The server's one-line answer to "how am I doing", e.g. "2 of 3 goals on track".
    pub headline: String,
    pub series: Vec<SeriesView>,
    /// Free-text caveats — a goal with too little data to chart, a metric not logged
    /// recently enough to trend.
    pub notes: Vec<String>,
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
            View::Progress(ProgressView { headline: String::new(), series: vec![], notes: vec![] }),
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
        }
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

        let one = ProgressView { headline: "  ".into(), series: vec![trend()], notes: vec![] };
        assert_eq!(one.summary_line(), "Here's your progress (1 chart).");

        let none = ProgressView { headline: String::new(), series: vec![], notes: vec![] };
        assert_eq!(none.summary_line(), "Here's your progress (0 charts).");
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

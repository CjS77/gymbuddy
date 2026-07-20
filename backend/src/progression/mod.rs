//! The progressive-overload policy ([C5.3]).
//!
//! "Tries to push the user out of their comfort zone every session whilst still being safe." Both
//! halves are requirements, and this module is where the second one is actually enforced: it turns
//! logged performance, per-set effort and the roster's prescription into an explicit per-exercise
//! [`ProgressionDirective`] — progress by *this much*, hold, back off to *this load*, or deload —
//! which the designer prompt then carries as a binding instruction rather than as advice.
//!
//! The science it implements is curated in `backend/science/progressive-overload.md`
//! (`[S:progressive-overload]`), and the rules below are deliberately traceable to it:
//!
//! - **When to add load** — the NSCA "2-for-2" rule: beat the prescribed repetitions by two or more
//!   in two consecutive sessions on the same movement, then add one increment. Self-regulating, so
//!   a bad week simply does not trigger it.
//! - **How much** — relatively smaller on upper-body and single-joint work than on lower-body
//!   compounds. See [`ExerciseClass`], which takes the conservative end of the corpus's bands.
//! - **When to back off** — repetitions falling short at a load previously handled, or repeated
//!   work at or beyond failure. Both need *two* sessions: "a single bad session is noise".
//! - **Deloads** — a block that says deload overrides ordinary progression outright, and holds load
//!   while cutting volume rather than the reverse.
//!
//! # Effort, and the RPE/RIR question ([C3.3])
//!
//! Sets are logged on the four-point [`Difficulty`] scale, because that is how people describe a
//! set out loud. This module does **not** add an RPE or RIR field to logging; it reads the existing
//! scale as a repetitions-in-reserve band ([`reps_in_reserve`]) and states that reading in the
//! prompt, so the model can hold the log against the corpus's RIR-denominated prescriptions. The
//! corpus is explicit that reported effort is "informative, not exact", which is precisely what a
//! four-point scale is good for — and it is why effort alone never progresses a load when a
//! prescription was recorded to measure against.

use crate::db::{Difficulty, ExerciseTypeWithAncestry, MeasurementType, ProgrammeBlock};

/// How a movement responds to added load, which is what sets the size of a sensible increment. The
/// corpus puts the bands at roughly 2-5% for upper-body lifts and 5-10% for lower-body ones, and
/// notes that the same absolute jump is routine on a squat and large on a lateral raise.
///
/// Every variant takes the **conservative end** of its band. Under-progressing costs a session;
/// over-progressing costs a shoulder, and the user's own logged performance corrects the former
/// within two sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExerciseClass {
    /// A multi-joint lower-body lift (squat, deadlift, hip thrust): the biggest absolute steps.
    LowerBodyCompound,
    /// A multi-joint upper-body lift (bench, overhead press, pull-up): smaller relative steps.
    UpperBodyCompound,
    /// Single-joint or assistance work (curls, lateral raises, leg curls), wherever it sits on the
    /// body: small relative steps and a small absolute floor, since a 2.5kg jump on a lateral raise
    /// is a 20% jump.
    Isolation,
    /// Work measured in time or distance rather than load — the lever is duration or pace, not kg.
    Conditioning,
}

/// Relative step, as a fraction of the working load.
const LOWER_COMPOUND_PCT: f64 = 0.05;
const UPPER_COMPOUND_PCT: f64 = 0.025;
const ISOLATION_PCT: f64 = 0.025;
const CONDITIONING_PCT: f64 = 0.05;

/// Smallest step worth prescribing, and the granularity increments round to. 2.5kg is the usual
/// smallest pair of plates; 1kg stands in for the fixed dumbbells and pin stacks isolation work
/// actually runs on. The corpus's "subject to the smallest plates available" is a per-user fact the
/// schema does not hold — see the module's follow-ups in the ticket rather than guessing at it.
const BARBELL_STEP_KG: f64 = 2.5;
const ISOLATION_STEP_KG: f64 = 1.0;
/// Seconds or metres: one whole unit is the finest granularity worth stating.
const CONDITIONING_STEP: f64 = 1.0;

/// A back-off is a real reduction, not the increment run backwards — the load has already been
/// shown to be too much. The corpus's own back-off section says "reduce the load"; 10% is the
/// conventional single step.
const BACK_OFF_PCT: f64 = 0.10;

/// How many repetitions past the target count as beating it, per the 2-for-2 rule.
const REPS_OVER_TARGET: i32 = 2;

/// How many consecutive qualifying sessions a signal needs before it acts. Two, in both directions:
/// "a single bad session is noise — poor sleep, a hard day, a missed meal".
const QUALIFYING_SESSIONS: usize = 2;

impl ExerciseClass {
    /// Which class a catalogue entry belongs to.
    ///
    /// Both inputs are real catalogue data rather than a name-matching table: `muscle_group` is the
    /// top-level ancestor, and `purpose` is the exercise-level row's own `strength` /
    /// `hypertrophy` / `endurance` / `cardio` label. A movement the catalogue calls `strength` is
    /// the compound of its group; everything else takes the isolation step, which is the safe way
    /// to be wrong about a movement nobody has classified yet.
    pub fn classify(muscle_group: Option<&str>, purpose: Option<&str>, measurement: MeasurementType) -> Self {
        if measurement != MeasurementType::WeightReps {
            return Self::Conditioning;
        }
        match (muscle_group.map(str::to_lowercase).as_deref(), purpose) {
            (_, Some("cardio")) => Self::Conditioning,
            (Some("legs"), Some("strength")) => Self::LowerBodyCompound,
            (Some(_), Some("strength")) => Self::UpperBodyCompound,
            _ => Self::Isolation,
        }
    }

    /// The class of the catalogue entry `ancestry`, resolving `purpose` through the exercise-level
    /// ancestor when the row is a variation (variations carry no `purpose` of their own — "Front
    /// Squat" inherits its class from "Squat").
    pub fn of(ancestry: &ExerciseTypeWithAncestry, catalogue: &[ExerciseTypeWithAncestry]) -> Self {
        let purpose = ancestry.exercise_type.purpose.clone().or_else(|| exercise_purpose(ancestry, catalogue));
        let measurement = ancestry.exercise_type.measurement_type.unwrap_or(MeasurementType::WeightReps);
        Self::classify(ancestry.muscle_group.as_deref(), purpose.as_deref(), measurement)
    }

    /// Fraction of the working load one step represents.
    const fn step_pct(self) -> f64 {
        match self {
            Self::LowerBodyCompound => LOWER_COMPOUND_PCT,
            Self::UpperBodyCompound => UPPER_COMPOUND_PCT,
            Self::Isolation => ISOLATION_PCT,
            Self::Conditioning => CONDITIONING_PCT,
        }
    }

    /// Smallest loadable step, and the granularity every increment rounds to.
    const fn min_step(self) -> f64 {
        match self {
            Self::LowerBodyCompound | Self::UpperBodyCompound => BARBELL_STEP_KG,
            Self::Isolation => ISOLATION_STEP_KG,
            Self::Conditioning => CONDITIONING_STEP,
        }
    }

    /// How much to add to `load`: the class's relative step, rounded to a loadable granularity and
    /// never below one step. Zero or negative loads (bodyweight work logged without added weight)
    /// get one step, which is the smallest honest thing to say.
    pub fn increment(self, load: f64) -> f64 {
        round_to_step(load * self.step_pct(), self.min_step())
    }

    /// How much to take off `load` when the evidence says it is too heavy. Larger than
    /// [`increment`](Self::increment) on purpose — backing off by the step that got you here just
    /// re-runs the session that failed.
    pub fn back_off(self, load: f64) -> f64 {
        round_to_step(load * BACK_OFF_PCT, self.min_step())
    }

    /// Human-readable class, for the reason strings the prompt carries.
    pub const fn label(self) -> &'static str {
        match self {
            Self::LowerBodyCompound => "lower-body compound",
            Self::UpperBodyCompound => "upper-body compound",
            Self::Isolation => "isolation/assistance",
            Self::Conditioning => "conditioning",
        }
    }
}

/// The `purpose` of an entry's exercise-level ancestor, looked up by name through the catalogue.
fn exercise_purpose(ancestry: &ExerciseTypeWithAncestry, catalogue: &[ExerciseTypeWithAncestry]) -> Option<String> {
    let parent = ancestry.exercise.as_deref()?;
    catalogue
        .iter()
        .find(|e| e.exercise_type.name.eq_ignore_ascii_case(parent) && e.exercise_type.purpose.is_some())
        .and_then(|e| e.exercise_type.purpose.clone())
}

/// Round `raw` up to a whole number of `step`s, with one step as the floor. A step is the smallest
/// change the user can actually load, so anything finer is a prescription they cannot follow.
fn round_to_step(raw: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return raw.max(0.0);
    }
    (raw / step).round().max(1.0) * step
}

/// The repetitions-in-reserve band a logged [`Difficulty`] stands for — the answer to the RPE/RIR
/// question parked in [C3.3]. See the module docs: the four-point scale stays the capture
/// vocabulary, and this is how it is read against the corpus's RIR-denominated bands.
pub const fn reps_in_reserve(effort: Difficulty) -> &'static str {
    match effort {
        Difficulty::Easy => "4+ RIR",
        Difficulty::Medium => "2-3 RIR",
        Difficulty::Hard => "1-2 RIR",
        Difficulty::Failure => "0 RIR",
    }
}

/// Whether a set was taken close enough to failure that adding load to it is not a safe call.
const fn at_or_near_failure(effort: Difficulty) -> bool {
    matches!(effort, Difficulty::Failure)
}

/// What a programme block says this week is for. Ordinary progression applies unless a block
/// declares a deload, which overrides it outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockIntent {
    Ordinary,
    /// A deload week, named as the block spelled it ("deload", "weeks 5-6: deload/technique").
    Deload { focus: String },
}

/// The word a block or slot focus uses to declare a deload. Matched as a substring so
/// "deload / technique" and "Deload week" both land.
const DELOAD_MARKER: &str = "deload";

impl BlockIntent {
    /// Read the intent off a programme block. `None` (no programme, or no block covering this week)
    /// is ordinary progression: absence of a block is not a deload.
    pub fn of_block(block: Option<&ProgrammeBlock>) -> Self {
        match block {
            Some(b) if b.focus.to_lowercase().contains(DELOAD_MARKER) => Self::Deload { focus: b.focus.clone() },
            _ => Self::Ordinary,
        }
    }

    pub const fn is_deload(&self) -> bool {
        matches!(self, Self::Deload { .. })
    }
}

/// One session's work on one exercise, rolled up to the numbers the policy reads.
#[derive(Debug, Clone)]
pub struct Outing {
    /// Calendar date of the session, for the reason strings.
    pub date: String,
    /// The top set's value in the measurement's own unit — kg, seconds or metres.
    pub load: f64,
    /// Repetitions of the top set, for `weight_reps` work only.
    pub reps: Option<i32>,
    /// The hardest effort recorded across the exercise's working sets. The hardest, not the mean:
    /// one set to failure is the fact that matters for whether more load is safe.
    pub effort: Difficulty,
    /// What the bound roster prescribed for this exercise, when the session ran against one. This
    /// is the prescribed-vs-actual signal ([C1.5]) the 2-for-2 rule is measured against.
    pub target_reps: Option<i32>,
    pub target_load: Option<f64>,
}

impl Outing {
    /// Repetitions beyond the prescription, when both are known.
    fn reps_over_target(&self) -> Option<i32> {
        Some(self.reps? - self.target_reps?)
    }

    /// Whether this outing beat its prescription by the 2-for-2 margin without going to failure.
    fn cleared_target(&self) -> bool {
        !at_or_near_failure(self.effort) && self.reps_over_target().is_some_and(|over| over >= REPS_OVER_TARGET)
    }

    /// Whether this outing fell short of the repetitions it was prescribed.
    fn fell_short(&self) -> bool {
        self.reps_over_target().is_some_and(|over| over < 0)
    }
}

/// The recent record of one exercise, most recent outing first.
#[derive(Debug, Clone)]
pub struct ExerciseRecord {
    pub exercise_name: String,
    pub class: ExerciseClass,
    pub measurement_type: MeasurementType,
    /// One entry per session the exercise was trained in, most recent first.
    pub outings: Vec<Outing>,
}

/// What to do with one exercise's load next session.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressionAction {
    /// Add load: the user has earned it.
    Progress { from: f64, to: f64 },
    /// Repeat the load. The default, and not a failure state — most sessions hold.
    Hold { at: f64 },
    /// Reduce the load. The evidence says the current one is beyond what the user is recovering
    /// from, and this is the half of the policy that keeps people training.
    BackOff { from: f64, to: f64 },
    /// A deload week: hold the load, cut the volume.
    Deload { at: f64 },
}

impl ProgressionAction {
    /// The single word the prompt leads the directive with.
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Progress { .. } => "PROGRESS",
            Self::Hold { .. } => "HOLD",
            Self::BackOff { .. } => "BACK OFF",
            Self::Deload { .. } => "DELOAD",
        }
    }
}

/// One exercise's binding instruction for the next session, with the reasoning that produced it.
/// The reason is not decoration: a directive the user cannot have explained to them is one they
/// cannot argue with, and this coach is meant to be arguable with.
#[derive(Debug, Clone)]
pub struct ProgressionDirective {
    pub exercise_name: String,
    pub class: ExerciseClass,
    pub measurement_type: MeasurementType,
    pub action: ProgressionAction,
    pub reason: String,
}

/// The whole progression picture for one design: the week's intent, the per-exercise directives,
/// and an unplanned-deload recommendation when back-off signals have piled up across the board.
#[derive(Debug, Clone)]
pub struct ProgressionPolicy {
    pub block: BlockIntent,
    pub directives: Vec<ProgressionDirective>,
    /// Present when enough exercises are backing off at once that the corpus's "deload when
    /// back-off signals accumulate" clause applies, and the programme has not already called one.
    pub deload_advice: Option<String>,
}

impl ProgressionPolicy {
    pub fn is_empty(&self) -> bool {
        self.directives.is_empty()
    }
}

/// How many exercises must be backing off before it reads as systemic fatigue rather than one lift
/// having a bad fortnight.
const DELOAD_ADVICE_MIN_BACK_OFFS: usize = 2;

/// Build the policy for a design from each trained exercise's recent record and the week's intent.
pub fn build_policy(records: &[ExerciseRecord], block: BlockIntent) -> ProgressionPolicy {
    let directives: Vec<ProgressionDirective> = records.iter().filter_map(|record| directive_for(record, &block)).collect();
    let deload_advice = (!block.is_deload()).then(|| accumulated_back_off_advice(&directives)).flatten();
    ProgressionPolicy { block, directives, deload_advice }
}

/// The directive for one exercise, or `None` when it has no usable history — an exercise nobody has
/// logged has nothing to progress from, and inventing a load for it is the model's job, not this
/// module's.
fn directive_for(record: &ExerciseRecord, block: &BlockIntent) -> Option<ProgressionDirective> {
    let latest = record.outings.first()?;
    let (action, reason) = match block {
        BlockIntent::Deload { focus } => (
            ProgressionAction::Deload { at: latest.load },
            format!("deload block (\"{focus}\") — hold the load and cut the working sets, per [S:progressive-overload]"),
        ),
        BlockIntent::Ordinary => ordinary_action(record, latest),
    };
    Some(ProgressionDirective {
        exercise_name: record.exercise_name.clone(),
        class: record.class,
        measurement_type: record.measurement_type,
        action,
        reason,
    })
}

/// The action for an ordinary (non-deload) week. Back-off is evaluated first and wins outright:
/// when the evidence points both ways at once, the safe reading is the correct one.
fn ordinary_action(record: &ExerciseRecord, latest: &Outing) -> (ProgressionAction, String) {
    if let Some(reason) = back_off_signal(&record.outings) {
        let step = record.class.back_off(latest.load);
        let to = (latest.load - step).max(0.0);
        return (ProgressionAction::BackOff { from: latest.load, to }, reason);
    }
    if let Some(reason) = progress_signal(&record.outings) {
        let step = record.class.increment(latest.load);
        return (ProgressionAction::Progress { from: latest.load, to: latest.load + step }, reason);
    }
    (ProgressionAction::Hold { at: latest.load }, hold_reason(&record.outings))
}

/// The two most recent outings, when there are two to compare. Every signal below needs a pair:
/// one session is noise in either direction.
fn recent_pair(outings: &[Outing]) -> Option<(&Outing, &Outing)> {
    match outings {
        [latest, previous, ..] => Some((latest, previous)),
        _ => None,
    }
}

/// Whether the load held or rose between the earlier and the later outing. A rep PR set at a lighter
/// load is not evidence that the heavier load is ready, and a short set at a lighter load is not
/// evidence that the heavier one was too much.
fn load_not_reduced(latest: &Outing, previous: &Outing) -> bool {
    latest.load >= previous.load
}

/// Why this exercise should back off, if it should. Two independent triggers, both from the
/// corpus's back-off section, and both requiring two sessions.
fn back_off_signal(outings: &[Outing]) -> Option<String> {
    let (latest, previous) = recent_pair(outings)?;
    if latest.fell_short() && previous.fell_short() && load_not_reduced(latest, previous) {
        return Some(format!(
            "reps fell short of the prescription at this load in the last {QUALIFYING_SESSIONS} sessions ({}, {}) — \
             the load is ahead of what is being recovered from",
            previous.date, latest.date
        ));
    }
    if at_or_near_failure(latest.effort) && (at_or_near_failure(previous.effort) || previous.fell_short()) {
        return Some(format!(
            "taken to failure ({}) on top of a session already at its limit ({}) — repeated failure on the same \
             movement costs more in fatigue than it returns",
            latest.date, previous.date
        ));
    }
    None
}

/// Why this exercise has earned load, if it has.
///
/// The primary rule is the NSCA 2-for-2 rule, measured against what the roster actually prescribed
/// ([C1.5]). The fallback exists because ad-hoc training is first-class here: with no prescription
/// on file there is no target to beat, so two consecutive sessions logged `easy` — 4+ reps in
/// reserve — stand in. It is deliberately the weaker of the two, and needs the whole exercise to
/// have felt easy both times.
fn progress_signal(outings: &[Outing]) -> Option<String> {
    let (latest, previous) = recent_pair(outings)?;
    if !load_not_reduced(latest, previous) {
        return None;
    }
    if latest.cleared_target() && previous.cleared_target() {
        return Some(format!(
            "beat the prescribed reps by {REPS_OVER_TARGET}+ in {QUALIFYING_SESSIONS} consecutive sessions \
             ({}, {}) with nothing taken to failure — the 2-for-2 rule is met",
            previous.date, latest.date
        ));
    }
    if latest.effort == Difficulty::Easy && previous.effort == Difficulty::Easy {
        return Some(format!(
            "every working set logged easy ({}) in {QUALIFYING_SESSIONS} consecutive sessions ({}, {}) — \
             the load is below the working range",
            reps_in_reserve(Difficulty::Easy),
            previous.date,
            latest.date
        ));
    }
    None
}

/// Why this exercise is holding. Holding is the default, so the reason says what is still missing
/// rather than implying something went wrong.
fn hold_reason(outings: &[Outing]) -> String {
    let Some((latest, _)) = recent_pair(outings) else {
        return "only one logged session at this load — repeat it before deciding anything".to_string();
    };
    if latest.cleared_target() || latest.effort == Difficulty::Easy {
        return format!("one qualifying session so far ({}) — one more clears the 2-for-2 rule and the load goes up", latest.date);
    }
    format!("last session sat inside the working range ({}, {}) — hold and let the reps come up first", latest.date, reps_in_reserve(latest.effort))
}

/// The corpus also places a deload "when back-off signals accumulate". Several exercises backing off
/// at once is that signal: the common factor is the user, not the lift.
fn accumulated_back_off_advice(directives: &[ProgressionDirective]) -> Option<String> {
    let backing_off = directives.iter().filter(|d| matches!(d.action, ProgressionAction::BackOff { .. })).count();
    let enough = backing_off >= DELOAD_ADVICE_MIN_BACK_OFFS && backing_off * 2 >= directives.len();
    enough.then(|| {
        format!(
            "{backing_off} of {} exercises are backing off at once. That is accumulated fatigue rather than one lift \
             stalling: keep the loads below, cut total working sets by about a third this session, and say so in the \
             rationale ([S:progressive-overload]).",
            directives.len()
        )
    })
}

#[cfg(test)]
mod tests;

//! Tests for the progressive-overload policy ([C5.3]).
//!
//! The ticket's central claim is that both halves are requirements — "never progressing is useless;
//! always progressing injures people" — so the back-off and deload rules are tested as hard as the
//! increments, and the cases where the two signals collide assert that safety wins.

use super::*;
use crate::db::{ExerciseLevel, ExerciseType, ExerciseTypeWithAncestry};

/// An outing with a prescription to measure against — the roster-backed path ([C1.5]).
fn prescribed(date: &str, load: f64, reps: i32, effort: Difficulty, target_reps: i32) -> Outing {
    Outing { date: date.to_string(), load, reps: Some(reps), effort, target_reps: Some(target_reps), target_load: Some(load) }
}

/// An outing logged ad-hoc, with no roster and so no target to beat.
fn ad_hoc(date: &str, load: f64, reps: i32, effort: Difficulty) -> Outing {
    Outing { date: date.to_string(), load, reps: Some(reps), effort, target_reps: None, target_load: None }
}

/// A record whose outings are given most-recent-first, as the policy expects them.
fn record(class: ExerciseClass, outings: Vec<Outing>) -> ExerciseRecord {
    ExerciseRecord { exercise_name: "Bench Press".to_string(), class, measurement_type: MeasurementType::WeightReps, outings }
}

fn action_of(record: &ExerciseRecord, block: BlockIntent) -> ProgressionAction {
    build_policy(std::slice::from_ref(record), block).directives.first().expect("a record with outings yields a directive").action.clone()
}

// ── Increments: how much, per exercise class ──────────────────────────────────

/// The corpus's bands, applied: the same lift-relative step is a different number of kilograms on a
/// squat and on a lateral raise, and neither may come out finer than the user can load.
#[test]
fn increments_scale_with_the_class_and_round_to_a_loadable_step() {
    assert_eq!(ExerciseClass::LowerBodyCompound.increment(100.0), 5.0, "5% of a 100kg squat");
    assert_eq!(ExerciseClass::UpperBodyCompound.increment(100.0), 2.5, "2.5% of a 100kg bench");
    assert_eq!(ExerciseClass::Isolation.increment(40.0), 1.0, "2.5% of 40kg, at the 1kg isolation step");

    // A light lift's percentage is below one plate, so the plate is the increment — never a jump
    // the user has no way to load.
    assert_eq!(ExerciseClass::UpperBodyCompound.increment(20.0), 2.5);
    assert_eq!(ExerciseClass::Isolation.increment(6.0), 1.0);
}

/// Backing off is a bigger move than progressing. Reducing by the step that got you here just
/// re-runs the session that already failed.
#[test]
fn a_back_off_is_larger_than_an_increment() {
    let class = ExerciseClass::UpperBodyCompound;
    assert!(class.back_off(100.0) > class.increment(100.0));
    assert_eq!(class.back_off(100.0), 10.0);
}

/// Classification comes from catalogue data — the top-level muscle group and the exercise's own
/// `purpose` — and anything unclassified takes the *small* step. Being wrong quietly must mean
/// under-progressing, not over-loading someone.
#[test]
fn classification_defaults_to_the_conservative_class() {
    use ExerciseClass::*;
    let weight = MeasurementType::WeightReps;
    assert_eq!(ExerciseClass::classify(Some("Legs"), Some("strength"), weight), LowerBodyCompound);
    assert_eq!(ExerciseClass::classify(Some("Chest"), Some("strength"), weight), UpperBodyCompound);
    assert_eq!(ExerciseClass::classify(Some("Legs"), Some("hypertrophy"), weight), Isolation, "a leg curl is not a squat");
    assert_eq!(ExerciseClass::classify(Some("Arms"), Some("hypertrophy"), weight), Isolation);
    assert_eq!(ExerciseClass::classify(None, None, weight), Isolation, "unknown classifies down, never up");
    assert_eq!(ExerciseClass::classify(Some("Cardio"), Some("cardio"), MeasurementType::DistanceBased), Conditioning);
    assert_eq!(ExerciseClass::classify(Some("Core"), Some("endurance"), MeasurementType::TimeBased), Conditioning);
}

/// A variation carries no `purpose` of its own: "Front Squat" is a lower-body compound because
/// "Squat" is, and resolving that through the taxonomy is what stops every variation in the
/// catalogue defaulting to the isolation step.
#[test]
fn a_variation_inherits_its_class_from_its_exercise_level_parent() {
    let squat = catalogue_entry(5000, "Squat", ExerciseLevel::Exercise, Some("Legs"), None, Some("strength"));
    let front_squat = catalogue_entry(50001, "Front Squat", ExerciseLevel::Variation, Some("Legs"), Some("Squat"), None);
    let catalogue = vec![squat, front_squat.clone()];

    assert_eq!(ExerciseClass::of(&front_squat, &catalogue), ExerciseClass::LowerBodyCompound);
}

fn catalogue_entry(
    id: i64,
    name: &str,
    level: ExerciseLevel,
    muscle_group: Option<&str>,
    exercise: Option<&str>,
    purpose: Option<&str>,
) -> ExerciseTypeWithAncestry {
    ExerciseTypeWithAncestry {
        exercise_type: ExerciseType {
            id,
            name: name.to_string(),
            parent_id: Some(1),
            level,
            aliases: None,
            purpose: purpose.map(str::to_string),
            measurement_type: Some(MeasurementType::WeightReps),
            url: None,
            created_at: String::new(),
        },
        muscle_group: muscle_group.map(str::to_string),
        specific_muscle: None,
        exercise: exercise.map(str::to_string),
    }
}

// ── Progressing: the 2-for-2 rule ─────────────────────────────────────────────

/// The NSCA rule, through the roster-backed path: beat the prescription by two reps twice running
/// and the load goes up by exactly one class increment.
#[test]
fn two_sessions_beating_the_target_by_two_reps_earn_one_increment() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![prescribed("2026-07-15", 60.0, 10, Difficulty::Medium, 8), prescribed("2026-07-11", 60.0, 10, Difficulty::Medium, 8)],
    );
    assert_eq!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::Progress { from: 60.0, to: 62.5 });
}

/// One good session is not a trend. This is the half of the rule that keeps the policy honest in
/// the other direction: it does not chase a single strong day.
#[test]
fn one_qualifying_session_holds_and_says_what_is_pending() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![prescribed("2026-07-15", 60.0, 10, Difficulty::Medium, 8), prescribed("2026-07-11", 60.0, 8, Difficulty::Hard, 8)],
    );
    let policy = build_policy(&[bench], BlockIntent::Ordinary);
    let directive = &policy.directives[0];
    assert_eq!(directive.action, ProgressionAction::Hold { at: 60.0 });
    assert!(directive.reason.contains("one more clears the 2-for-2 rule"), "the reason must say what is pending: {}", directive.reason);
}

/// Beating the target while taking the set to failure is not the same evidence: the reps came out
/// of the tank, not out of spare capacity.
#[test]
fn reps_bought_at_failure_do_not_count_toward_the_rule() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![prescribed("2026-07-15", 60.0, 10, Difficulty::Failure, 8), prescribed("2026-07-11", 60.0, 10, Difficulty::Medium, 8)],
    );
    assert!(matches!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::Hold { .. }));
}

/// Ad-hoc training is first-class here, so there is often no prescription to beat. Two sessions
/// where every working set was logged easy — 4+ reps in reserve — stand in for it.
#[test]
fn without_a_prescription_two_easy_sessions_progress_the_load() {
    let squat = record(
        ExerciseClass::LowerBodyCompound,
        vec![ad_hoc("2026-07-15", 100.0, 5, Difficulty::Easy), ad_hoc("2026-07-11", 100.0, 5, Difficulty::Easy)],
    );
    assert_eq!(action_of(&squat, BlockIntent::Ordinary), ProgressionAction::Progress { from: 100.0, to: 105.0 });
}

/// The fallback is the weaker rule and must stay weak: one easy session next to a hard one is a
/// hold, not a promotion.
#[test]
fn the_effort_fallback_needs_both_sessions_easy() {
    let squat = record(
        ExerciseClass::LowerBodyCompound,
        vec![ad_hoc("2026-07-15", 100.0, 5, Difficulty::Easy), ad_hoc("2026-07-11", 100.0, 5, Difficulty::Hard)],
    );
    assert!(matches!(action_of(&squat, BlockIntent::Ordinary), ProgressionAction::Hold { .. }));
}

/// Two easy sessions at *falling* loads are not evidence that the heavier load is ready — the user
/// made it easy by taking weight off.
#[test]
fn a_reduced_load_does_not_earn_an_increment() {
    let squat = record(
        ExerciseClass::LowerBodyCompound,
        vec![ad_hoc("2026-07-15", 80.0, 5, Difficulty::Easy), ad_hoc("2026-07-11", 100.0, 5, Difficulty::Easy)],
    );
    assert!(matches!(action_of(&squat, BlockIntent::Ordinary), ProgressionAction::Hold { .. }));
}

// ── Backing off: the half that keeps people training ──────────────────────────

/// Reps falling short at a load previously handled, twice running, is the corpus's first back-off
/// trigger.
#[test]
fn reps_falling_short_twice_backs_the_load_off() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![prescribed("2026-07-15", 60.0, 5, Difficulty::Hard, 8), prescribed("2026-07-11", 60.0, 6, Difficulty::Hard, 8)],
    );
    // 10% of 60kg is 6kg, which rounds to the nearest loadable 2.5kg step.
    assert_eq!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::BackOff { from: 60.0, to: 55.0 });
}

/// Repeated work at or beyond failure on the same movement is the second trigger, and it fires even
/// when no roster prescribed anything to fall short of.
#[test]
fn repeated_failure_backs_the_load_off_without_a_prescription() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![ad_hoc("2026-07-15", 60.0, 5, Difficulty::Failure), ad_hoc("2026-07-11", 60.0, 6, Difficulty::Failure)],
    );
    assert!(matches!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::BackOff { .. }));
}

/// "A single bad session is noise — poor sleep, a hard day, a missed meal." One failure holds; it
/// does not strip weight off the bar.
#[test]
fn a_single_bad_session_holds_rather_than_backing_off() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![ad_hoc("2026-07-15", 60.0, 5, Difficulty::Failure), ad_hoc("2026-07-11", 60.0, 8, Difficulty::Medium)],
    );
    assert!(matches!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::Hold { .. }));
}

/// The collision case, and the reason back-off is evaluated first: an exercise that beat its target
/// twice *and* was driven to failure twice must not have load added. When the evidence points both
/// ways, the safe reading wins.
#[test]
fn safety_wins_when_both_signals_fire_at_once() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![prescribed("2026-07-15", 60.0, 10, Difficulty::Failure, 8), prescribed("2026-07-11", 60.0, 10, Difficulty::Failure, 8)],
    );
    assert!(
        matches!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::BackOff { .. }),
        "repeated failure must outrank a met rep target"
    );
}

/// Nothing to progress from means no directive at all, rather than a guess.
#[test]
fn an_exercise_with_no_outings_yields_no_directive() {
    let empty = record(ExerciseClass::UpperBodyCompound, Vec::new());
    assert!(build_policy(&[empty], BlockIntent::Ordinary).is_empty());
}

/// One outing is enough to state the load to repeat, and not enough to move it in either direction.
#[test]
fn a_single_outing_holds() {
    let bench = record(ExerciseClass::UpperBodyCompound, vec![ad_hoc("2026-07-15", 60.0, 8, Difficulty::Easy)]);
    assert_eq!(action_of(&bench, BlockIntent::Ordinary), ProgressionAction::Hold { at: 60.0 });
}

// ── Deloads ───────────────────────────────────────────────────────────────────

/// The ticket's explicit requirement: a deload week overrides progression. An exercise that has
/// unambiguously earned load gets held anyway, because the week is not for adding load.
#[test]
fn a_deload_block_overrides_an_earned_increment() {
    let bench = record(
        ExerciseClass::UpperBodyCompound,
        vec![prescribed("2026-07-15", 60.0, 10, Difficulty::Easy, 8), prescribed("2026-07-11", 60.0, 10, Difficulty::Easy, 8)],
    );
    let deload = BlockIntent::Deload { focus: "deload".to_string() };
    assert_eq!(action_of(&bench, deload.clone()), ProgressionAction::Deload { at: 60.0 });

    let policy = build_policy(&[bench], deload);
    assert!(policy.directives[0].reason.contains("cut the working sets"), "a deload holds load and cuts volume, not the reverse");
}

/// Block intent is read off the focus text, matched loosely because a block's focus is free text a
/// human wrote. Absence of a block is ordinary progression — never an implied deload.
#[test]
fn block_intent_reads_the_focus_text() {
    let block = |focus: &str| ProgrammeBlock {
        id: 1,
        programme_id: 1,
        start_week: 5,
        end_week: 6,
        focus: focus.to_string(),
        notes: None,
    };
    assert!(BlockIntent::of_block(Some(&block("deload"))).is_deload());
    assert!(BlockIntent::of_block(Some(&block("Deload / technique"))).is_deload());
    assert!(!BlockIntent::of_block(Some(&block("hypertrophy"))).is_deload());
    assert!(!BlockIntent::of_block(None).is_deload(), "no block is not a deload");
}

/// The corpus also places a deload "when back-off signals accumulate". Several lifts backing off at
/// once is that signal — the common factor is the user, not any one lift.
#[test]
fn accumulated_back_offs_recommend_an_unplanned_deload() {
    let failing = |name: &str| ExerciseRecord {
        exercise_name: name.to_string(),
        class: ExerciseClass::UpperBodyCompound,
        measurement_type: MeasurementType::WeightReps,
        outings: vec![ad_hoc("2026-07-15", 60.0, 5, Difficulty::Failure), ad_hoc("2026-07-11", 60.0, 5, Difficulty::Failure)],
    };
    let steady = record(
        ExerciseClass::UpperBodyCompound,
        vec![ad_hoc("2026-07-15", 40.0, 8, Difficulty::Medium), ad_hoc("2026-07-11", 40.0, 8, Difficulty::Medium)],
    );

    let policy = build_policy(&[failing("Bench Press"), failing("Overhead Press"), steady], BlockIntent::Ordinary);
    let advice = policy.deload_advice.expect("two of three backing off should recommend a deload");
    assert!(advice.contains("2 of 3"), "{advice}");
    assert!(advice.contains("cut total working sets"), "{advice}");
}

/// One lift stalling is one lift stalling. The advice must not fire on it, or it fires constantly
/// and stops meaning anything.
#[test]
fn a_lone_back_off_does_not_recommend_a_deload() {
    let failing = record(
        ExerciseClass::UpperBodyCompound,
        vec![ad_hoc("2026-07-15", 60.0, 5, Difficulty::Failure), ad_hoc("2026-07-11", 60.0, 5, Difficulty::Failure)],
    );
    assert!(build_policy(&[failing], BlockIntent::Ordinary).deload_advice.is_none());
}

/// A programme that already called a deload does not need to be told to deload.
#[test]
fn a_deload_week_does_not_also_advise_a_deload() {
    let failing = record(
        ExerciseClass::UpperBodyCompound,
        vec![ad_hoc("2026-07-15", 60.0, 5, Difficulty::Failure), ad_hoc("2026-07-11", 60.0, 5, Difficulty::Failure)],
    );
    let policy = build_policy(&[failing.clone(), failing], BlockIntent::Deload { focus: "deload".to_string() });
    assert!(policy.deload_advice.is_none());
}

// ── Effort as reps in reserve [C3.3] ──────────────────────────────────────────

/// The mapping is the answer to the parked RPE/RIR question: the four-point scale stays the capture
/// vocabulary and is *read* as RIR, so the log can be held against the corpus's RIR-denominated
/// bands without a schema change.
#[test]
fn the_effort_scale_maps_onto_reps_in_reserve() {
    assert_eq!(reps_in_reserve(Difficulty::Easy), "4+ RIR");
    assert_eq!(reps_in_reserve(Difficulty::Medium), "2-3 RIR");
    assert_eq!(reps_in_reserve(Difficulty::Hard), "1-2 RIR");
    assert_eq!(reps_in_reserve(Difficulty::Failure), "0 RIR");
}

// ── Non-weight work ───────────────────────────────────────────────────────────

/// Timed work has no load to add, so the lever is duration. The policy runs the same rules over it
/// and the increment lands on a whole second.
#[test]
fn conditioning_work_progresses_in_its_own_unit() {
    let plank = ExerciseRecord {
        exercise_name: "Plank".to_string(),
        class: ExerciseClass::Conditioning,
        measurement_type: MeasurementType::TimeBased,
        outings: vec![ad_hoc("2026-07-15", 60.0, 0, Difficulty::Easy), ad_hoc("2026-07-11", 60.0, 0, Difficulty::Easy)],
    };
    assert_eq!(action_of(&plank, BlockIntent::Ordinary), ProgressionAction::Progress { from: 60.0, to: 63.0 });
}

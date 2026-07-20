use super::*;
use crate::db::{HealthEntry, HealthEntryType, Severity};

fn entry(entry_type: HealthEntryType, body_part: Option<&str>, severity: Severity) -> HealthEntry {
    HealthEntry {
        id: 1,
        user_id: 1,
        entry_type,
        body_part: body_part.map(str::to_string),
        severity,
        description: "test entry".to_string(),
        started_at: "2026-07-01 08:00:00".to_string(),
        resolved_at: None,
        notes: None,
        updated_at: "2026-07-01 08:00:00".to_string(),
    }
}

fn injury(body_part: &str, severity: Severity) -> HealthEntry {
    entry(HealthEntryType::Injury, Some(body_part), severity)
}

fn roster(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn barred(entries: &[HealthEntry], names: &[&str]) -> Vec<String> {
    violations(entries, &roster(names)).into_iter().map(|v| v.exercise).collect()
}

/// The rails read `severity >= rule.bars_from`, so the variant order in [`Severity`] *is* the
/// graduation. Reordering it would invert every threshold silently and no other test would notice.
#[test]
fn severity_is_ordered() {
    assert!(Severity::Mild < Severity::Moderate);
    assert!(Severity::Moderate < Severity::Severe);
    assert!(Severity::Severe > Severity::Mild);
}

/// Every rail must cite a document that exists and is tagged for the body part it covers. The table
/// is a projection of the corpus; a rail citing a document that was renamed or deleted is a rule
/// whose evidence has silently gone away.
#[test]
fn every_rail_cites_a_real_corpus_document() {
    for rails in RAILS {
        let doc = super::super::doc(rails.doc_id).unwrap_or_else(|| panic!("rail for `{}` cites missing doc `{}`", rails.body_part, rails.doc_id));
        assert!(
            super::super::INJURY_BODY_PARTS.contains(&rails.body_part),
            "`{}` is not in the injury vocabulary",
            rails.body_part
        );
        assert!(
            doc.injury_body_parts().any(|part| part == rails.body_part),
            "`{}` does not carry the `injury:{}` tag its rail claims",
            rails.doc_id,
            rails.body_part
        );
        assert!(!rails.contraindications.is_empty(), "`{}` has a rail with no rules", rails.body_part);
    }
}

/// A rail that offered back the movement it just barred would be worse than no rail. Every
/// substitution is checked against every pattern barred for its own body part at its own threshold.
#[test]
fn no_substitution_is_itself_contraindicated() {
    for rails in RAILS {
        for rule in rails.contraindications {
            for substitution in rule.substitutions {
                let offending = rails
                    .contraindications
                    .iter()
                    .filter(|other| other.bars_from <= rule.bars_from)
                    .find(|other| other.pattern.matches(substitution));
                assert!(
                    offending.is_none(),
                    "`{}` is offered as a substitute for {} on a {} injury, but is itself {}",
                    substitution,
                    rule.pattern.as_str(),
                    rails.body_part,
                    offending.unwrap().pattern.as_str(),
                );
            }
        }
    }
}

/// The spec's named gap: the old prompt happened to describe the lower-back case and nothing at all
/// covered a shoulder. An overhead press on a moderate shoulder injury is the canonical miss.
#[test]
fn a_shoulder_injury_bars_overhead_pressing() {
    let entries = [injury("shoulder", Severity::Moderate)];
    let found = violations(&entries, &roster(&["Barbell Overhead Press", "Leg Press", "Front Lat Pulldown"]));

    assert_eq!(found.len(), 1, "only the press should be barred: {found:?}");
    assert_eq!(found[0].exercise, "Barbell Overhead Press");
    assert_eq!(found[0].pattern, MovementPattern::OverheadPressing);
    assert!(found[0].substitutions.contains(&"landmine press"), "a bar must come with a way to keep the session");
}

/// The ticket's headline assertion, stated as plainly as it is written: given an active injury, the
/// design contains no contraindicated movement.
#[test]
fn an_active_injury_bars_its_movements_across_every_covered_body_part() {
    let cases = [
        ("lower_back", "Conventional Deadlift", MovementPattern::LoadedSpinalFlexion),
        ("lower_back", "Back Squat", MovementPattern::AxialCompression),
        ("lower_back", "Russian Twist", MovementPattern::LoadedSpinalRotation),
        ("shoulder", "Upright Row", MovementPattern::UprightRowing),
        ("shoulder", "Dumbbell Lateral Raise", MovementPattern::RaiseAboveShoulderHeight),
        ("knee", "Walking Lunge", MovementPattern::DeepKneeFlexionUnderLoad),
        ("knee", "Leg Extension", MovementPattern::OpenChainKneeExtension),
        ("knee", "Running", MovementPattern::RunningVolume),
    ];

    for (body_part, exercise, pattern) in cases {
        let entries = [injury(body_part, Severity::Moderate)];
        let found = violations(&entries, &roster(&[exercise]));
        assert_eq!(found.len(), 1, "`{exercise}` must be barred on a moderate {body_part} injury");
        assert_eq!(found[0].pattern, pattern, "`{exercise}` matched the wrong pattern");
    }
}

/// Mild means work around it, moderate means remove the pattern, severe means do not train it — the
/// graduation the ticket calls for, and the reason `severity` stopped being a column nothing reads.
#[test]
fn severity_graduates_the_response() {
    let deadlift_and_crunch = ["Conventional Deadlift", "Cable Crunch"];

    // Mild: the corpus keeps the pattern and reduces load and range, so a deadlift survives. The
    // exception is end-range spinal flexion, whose whole problem is the range.
    let mild = barred(&[injury("lower_back", Severity::Mild)], &deadlift_and_crunch);
    assert_eq!(mild, vec!["Cable Crunch"], "mild should work around the back, not cancel the session");

    // Moderate: the pattern comes out.
    let moderate = barred(&[injury("lower_back", Severity::Moderate)], &deadlift_and_crunch);
    assert_eq!(moderate.len(), 2, "moderate removes the pattern: {moderate:?}");

    // Severe outranks every threshold in the table, so it bars everything the body part has a rule
    // for — which is what "do not load it" means, without a special case to get wrong.
    let severe = barred(&[injury("lower_back", Severity::Severe)], &["Back Squat", "Good Morning", "Sit-Up", "Woodchopper"]);
    assert_eq!(severe.len(), 4, "severe should bar every lower-back pattern: {severe:?}");
}

/// A severe injury must not leave the user with nothing they can train. The corpus is emphatic that
/// a shoulder complaint is no reason to skip legs, and a rail that quietly barred the whole gym
/// would push users to ignore it.
#[test]
fn a_severe_injury_still_leaves_the_rest_of_the_body_trainable() {
    let entries = [injury("shoulder", Severity::Severe)];
    let safe = ["Leg Press", "Romanian Deadlift", "Seated Leg Curl", "Standing Calf Raise", "Plank"];
    assert!(violations(&entries, &roster(&safe)).is_empty(), "a severe shoulder should not bar leg and trunk work");
}

/// The rail classifies the name the designer wrote, not the catalogue row it resolves to. An
/// invented name is the more dangerous input: it would be dropped as unmatchable further down the
/// designer, but only after the rail had already had its chance to see it.
#[test]
fn an_invented_exercise_name_is_still_caught() {
    let entries = [injury("shoulder", Severity::Mild)];
    let found = violations(&entries, &roster(&["Behind-the-Neck Press"]));
    assert_eq!(found.len(), 1, "a name absent from the catalogue must still trip the rail");
    assert_eq!(found[0].pattern, MovementPattern::BehindTheNeckLoading);
}

/// The corpus prescribes these by name. A rail that barred the fix it recommends would make every
/// design unsatisfiable, and the model would have no roster it could legally return.
#[test]
fn the_corpus_recommended_substitutions_are_never_barred() {
    let back = [injury("lower_back", Severity::Severe)];
    let back_safe = ["Leg Press", "Split Squat", "Hack Squat", "Hip Thrust", "Glute Bridge", "Machine Seated Row", "Side Plank", "Dead Bug"];
    assert!(violations(&back, &roster(&back_safe)).is_empty(), "{:?}", barred(&back, &back_safe));

    let shoulder = [injury("shoulder", Severity::Severe)];
    let shoulder_safe = ["Lat Pulldown", "Machine Seated Row", "Face Pull", "Cable Tricep Pushdown", "Standard Push-Up"];
    assert!(violations(&shoulder, &roster(&shoulder_safe)).is_empty(), "{:?}", barred(&shoulder, &shoulder_safe));

    let knee = [injury("knee", Severity::Severe)];
    let knee_safe = ["Leg Press", "Split Squat", "Hip Thrust", "Seated Leg Curl", "Cycling", "Swimming", "Rowing"];
    assert!(violations(&knee, &roster(&knee_safe)).is_empty(), "{:?}", barred(&knee, &knee_safe));
}

/// Only an unresolved injury constrains the design. An illness has a severity but no movement
/// pattern; a wellbeing note is an observation. Both still reach the model as prose.
#[test]
fn only_active_injuries_drive_the_rail() {
    let deadlift = roster(&["Conventional Deadlift"]);

    let illness = [entry(HealthEntryType::Illness, Some("lower_back"), Severity::Severe)];
    assert!(violations(&illness, &deadlift).is_empty(), "an illness is not a contraindication");

    let wellbeing = [entry(HealthEntryType::Wellbeing, Some("lower_back"), Severity::Severe)];
    assert!(violations(&wellbeing, &deadlift).is_empty(), "a wellbeing note is not a contraindication");

    let mut resolved = injury("lower_back", Severity::Severe);
    resolved.resolved_at = Some("2026-07-10 09:00:00".to_string());
    assert!(violations(&[resolved], &deadlift).is_empty(), "a resolved injury no longer constrains");

    let no_part = [entry(HealthEntryType::Injury, None, Severity::Severe)];
    assert!(violations(&no_part, &deadlift).is_empty(), "an injury with no body part has no pattern to bar");

    let unknown = [injury("soul", Severity::Severe)];
    assert!(violations(&unknown, &deadlift).is_empty(), "a body part outside the vocabulary bars nothing");
}

/// A body part in the vocabulary that no curated document covers has no rail — deliberately. The
/// prose path still carries it to the model; what it must not do is invent contraindications.
#[test]
fn a_body_part_without_a_document_has_no_rail() {
    assert!(rails_for("elbow").is_none(), "`elbow` has no curated document, so it must have no rules");
    assert!(rails_for("lower_back").is_some());
}

/// Free-text body parts reach the rail exactly as the user typed them.
#[test]
fn body_parts_are_normalised_before_matching() {
    for spelling in ["lower back", "Lower-Back", " LOWER_BACK "] {
        let found = violations(&[injury(spelling, Severity::Moderate)], &roster(&["Conventional Deadlift"]));
        assert_eq!(found.len(), 1, "`{spelling}` should normalise onto the lower-back rail");
    }
}

/// Two injuries at once are two sets of rules, not the more lenient one.
#[test]
fn concurrent_injuries_each_apply() {
    let entries = [injury("shoulder", Severity::Moderate), injury("knee", Severity::Moderate)];
    let found = barred(&entries, &["Barbell Overhead Press", "Leg Extension", "Machine Seated Row"]);
    assert_eq!(found.len(), 2, "each injury contributes its own bars: {found:?}");
    assert!(!found.contains(&"Machine Seated Row".to_string()), "a row loads neither");
}

/// The pinned-document ids the designer feeds to retrieval.
#[test]
fn rail_documents_are_named_for_pinning() {
    let ids: Vec<&str> = rail_doc_ids(["shoulder", "knee", "elbow"]).collect();
    assert_eq!(ids, vec!["injury-shoulder", "injury-knee"], "an uncovered body part pins nothing");
}

/// The user-facing line names the movement, the injury and the way out, and never a diagnosis.
#[test]
fn a_violation_explains_itself_without_diagnosing() {
    let found = violations(&[injury("lower_back", Severity::Moderate)], &roster(&["Conventional Deadlift"]));
    let described = found[0].describe();
    assert!(described.contains("Conventional Deadlift"), "{described}");
    assert!(described.contains("lower back"), "the body part reads as prose, not `lower_back`: {described}");
    assert!(described.contains("hip thrust"), "a bar must name what to do instead: {described}");
    assert!(REFER_OUT.contains("not medical advice"), "the boundary must be explicit");
}

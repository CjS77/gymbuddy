//! Contraindications: the deterministic rail between an active injury and a designed session.
//!
//! The corpus ([`crate::science`]) tells the *model* how to train around an injury, in prose, and the
//! designer prompt asks it nicely. This module is what happens when it does not listen. Every rule
//! here is data — body part → [`MovementPattern`] → the substitutions that keep the session's intent —
//! and [`violations`] is a pure function over a proposed roster that the designer runs *after* parsing
//! and *before* persisting anything.
//!
//! # Why a rail and not a better prompt
//!
//! An active [`HealthEntry`](crate::db::HealthEntry) is a hard constraint on session design (see the
//! README's Nomenclature), and a hard constraint that is only ever *requested* is not one. The prompt
//! remains — a model that substitutes correctly on its own produces a better session than one that
//! gets its roster rejected — but the prompt is the fast path, not the guarantee.
//!
//! # What this is not
//!
//! This is training decision support, not medical advice, and the distinction is load-bearing rather
//! than legal boilerplate. Nothing here diagnoses: the rules key off the body part and severity the
//! *user* reported, and the corpus documents are explicit that naming the problem from a typed
//! description is not possible. Referring out is the corpus's first section for every injury document
//! and [`REFER_OUT`] carries that boundary into what the user sees.
//!
//! # Fidelity to the corpus
//!
//! Every entry in [`RAILS`] is transcribed from the curated document named in its `doc_id` — the
//! "Movements to avoid or modify" and "Substitutions that keep the session's intent" sections. The
//! corpus is where this knowledge is reviewed *as science*, with citations; this module is only its
//! machine-checkable projection. **A change here without a matching change to the document it cites is
//! a bug**, and [`tests`] asserts every `doc_id` resolves.

use crate::db::{HealthEntry, HealthEntryType, Severity};

/// A class of movement an injured body part may not tolerate. Deliberately coarser than an exercise
/// and finer than a muscle group: the corpus reasons in patterns ("loaded spinal flexion"), a
/// catalogue grows new exercises without review, and a muscle group is far too blunt to be a rail —
/// "no back work" would bar the chest-supported row that `injury-lower-back` actually prescribes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementPattern {
    /// Loading the spine into flexion — deadlifts, good mornings, bent-over rows, heavy swings.
    LoadedSpinalFlexion,
    /// Vertical load down the spine — back and front squats, standing presses, heavy carries.
    AxialCompression,
    /// Trunk flexion taken to end range under any load — sit-ups, weighted crunches, leg raises.
    EndRangeSpinalFlexion,
    /// Rotating a loaded trunk — Russian twists, woodchoppers, weighted side bends.
    LoadedSpinalRotation,
    /// Pressing a load overhead — overhead, military and push presses.
    OverheadPressing,
    /// Anything taken behind the neck — the least forgiving shoulder position there is.
    BehindTheNeckLoading,
    /// Wide-grip horizontal pressing and end-range front-of-shoulder stretch under load.
    WideGripHorizontalPressing,
    /// Dipping, where the shoulder ends up well behind the torso under bodyweight.
    DeepDipping,
    /// Upright rowing — internal rotation with the elbow driven above the shoulder.
    UprightRowing,
    /// Raising a load to or above shoulder height on a straight arm.
    RaiseAboveShoulderHeight,
    /// Deep knee flexion under load — full-depth squats, deep leg press, long-range lunging.
    DeepKneeFlexionUnderLoad,
    /// Open-chain resisted knee extension, which loads the patellofemoral joint hardest deep.
    OpenChainKneeExtension,
    /// High-impact plyometrics — jumps, bounding, depth work.
    HighImpactPlyometrics,
    /// Sustained repetitive running impact.
    RunningVolume,
}

impl MovementPattern {
    /// How the pattern reads in a sentence to the user, lowercase and un-capitalised.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LoadedSpinalFlexion => "loaded spinal flexion",
            Self::AxialCompression => "axial spinal compression",
            Self::EndRangeSpinalFlexion => "end-range spinal flexion",
            Self::LoadedSpinalRotation => "loaded spinal rotation",
            Self::OverheadPressing => "overhead pressing",
            Self::BehindTheNeckLoading => "behind-the-neck loading",
            Self::WideGripHorizontalPressing => "wide-grip horizontal pressing",
            Self::DeepDipping => "deep dipping",
            Self::UprightRowing => "upright rowing",
            Self::RaiseAboveShoulderHeight => "raising load to shoulder height or above",
            Self::DeepKneeFlexionUnderLoad => "deep knee flexion under load",
            Self::OpenChainKneeExtension => "open-chain knee extension",
            Self::HighImpactPlyometrics => "high-impact plyometrics",
            Self::RunningVolume => "running volume",
        }
    }

    /// The name fragments that identify this pattern, matched case-insensitively as substrings of an
    /// exercise name.
    ///
    /// Substring matching over a curated fragment list, rather than a lookup keyed by catalogue id,
    /// is deliberate: the rail must also catch a name the model *invented*. "Behind-the-neck press"
    /// is not in the catalogue, would not resolve, and is precisely the prescription that must never
    /// reach a user with a sore shoulder.
    ///
    /// Broad where the movement family is the risk, narrow where it is not. `"squat"` is bare on
    /// purpose: the catalogue really does contain a `Squat` category, and the designer may prescribe
    /// it by that name — a rule written only for `"back squat"` waves it straight through.
    /// [`Self::exemptions`] carves the variants the corpus prescribes back out. There is no bare
    /// `"lat pulldown"`, by contrast, because `injury-shoulder` offers the front pulldown as the
    /// *fix* for the behind-neck one: there the family is not the risk, one position is.
    fn fragments(&self) -> &'static [&'static str] {
        match self {
            Self::LoadedSpinalFlexion => {
                &["deadlift", "good morning", "bent over row", "bent-over row", "barbell row", "pendlay row", "kettlebell swing"]
            }
            Self::AxialCompression => &["squat", "military press", "push press", "shrug", "farmer", "loaded carry"],
            Self::EndRangeSpinalFlexion => &["sit-up", "situp", "sit up", "crunch", "leg raise", "ab wheel", "roman chair", "toe touch"],
            Self::LoadedSpinalRotation => &["russian twist", "woodchop", "wood chop", "side bend", "landmine twist", "rotational throw"],
            Self::OverheadPressing => &["overhead press", "military press", "push press", "shoulder press", "arnold press"],
            Self::BehindTheNeckLoading => &["behind the neck", "behind-the-neck", "behind neck", "behind-neck"],
            Self::WideGripHorizontalPressing => &["wide grip bench", "wide-grip bench", "decline barbell bench", "cable crossover", "pec deck"],
            Self::DeepDipping => &["dip"],
            Self::UprightRowing => &["upright row"],
            Self::RaiseAboveShoulderHeight => &["lateral raise", "front raise"],
            // No bare "step-up": the knee document's concern is step-ups "through a large range" and
            // its substitution is a step-up "to a low box" — a range qualifier a name cannot carry.
            Self::DeepKneeFlexionUnderLoad => &["squat", "lunge", "deep leg press"],
            Self::OpenChainKneeExtension => &["leg extension"],
            Self::HighImpactPlyometrics => &["jump", "plyometric", "bounding", "box jump", "depth jump", "burpee"],
            Self::RunningVolume => &["running", "sprint", "jogging"],
        }
    }

    /// The variants a fragment catches but the corpus prescribes anyway — checked after
    /// [`Self::fragments`] and overriding it.
    ///
    /// This is where "reduce the range" survives contact with a matcher that can only read a name. A
    /// box squat and a belt squat are squats that the knee document offers *as the substitution*, and
    /// the load is taken off the lumbar spine in a hack or split squat. Each of these is named in the
    /// document cited by the rail that would otherwise bar it.
    ///
    /// What is deliberately *not* exempt: qualifiers the corpus attaches to a movement but a name
    /// cannot carry — "lighter", "to a pain-free depth", "through a shortened range". Those describe
    /// the mild response, and mild does not reach most thresholds in the first place.
    fn exemptions(&self) -> &'static [&'static str] {
        match self {
            // The lower-back document's own substitution table: leg press, split squat, hack squat,
            // belt squat. Each keeps the pattern's training effect with the spine unloaded.
            Self::AxialCompression => &["split squat", "hack squat", "belt squat", "box squat"],
            // The knee document prescribes squatting to a controlled depth rather than not at all.
            // A Bulgarian split squat is *not* exempt: it is named in the list to avoid, at depth.
            Self::DeepKneeFlexionUnderLoad => &["box squat", "belt squat", "wall sit"],
            _ => &[],
        }
    }

    /// Whether `exercise_name` is an instance of this pattern.
    pub fn matches(&self, exercise_name: &str) -> bool {
        let name = exercise_name.to_lowercase();
        let hit = self.fragments().iter().any(|fragment| name.contains(fragment));
        hit && !self.exemptions().iter().any(|exempt| name.contains(exempt))
    }
}

/// One rule: a pattern an injured body part does not tolerate, the severity at which it stops being a
/// modification and becomes a bar, and what to do instead.
#[derive(Debug, Clone, Copy)]
pub struct Contraindication {
    pub pattern: MovementPattern,
    /// The least [`Severity`] at which this pattern is barred outright.
    ///
    /// The corpus graduates identically for every body part: **mild** means reduce load and range but
    /// keep the pattern, **moderate** means remove the pattern, **severe** means do not load the area
    /// at all. So most rules bar from [`Severity::Moderate`], and the handful the corpus says to
    /// remove outright — behind-the-neck work, deep dips, plyometrics on a sore knee, end-range
    /// spinal flexion on a sore back — bar from [`Severity::Mild`].
    ///
    /// Severe needs no special case: it outranks every threshold here, so a severe entry bars every
    /// pattern listed for its body part, which *is* "do not load it".
    pub bars_from: Severity,
    /// What keeps the session's intent instead, transcribed from the document's substitution table.
    pub substitutions: &'static [&'static str],
}

/// Everything the corpus says about training around one body part, in machine-checkable form.
#[derive(Debug, Clone, Copy)]
pub struct BodyPartRails {
    /// The body part, in [`super::INJURY_BODY_PARTS`] spelling.
    pub body_part: &'static str,
    /// The curated document these rules are transcribed from. Pinned into the designer's
    /// [`ScienceQuery`](super::ScienceQuery) so the prose reaches the model alongside the rail.
    pub doc_id: &'static str,
    pub contraindications: &'static [Contraindication],
}

/// The contraindication table, one entry per body part the corpus has a document for.
///
/// The other members of [`super::INJURY_BODY_PARTS`] are absent by design rather than by oversight:
/// a rail is only as good as the reviewed science behind it, and inventing contraindications for a
/// body part no curated document covers would be exactly the confident guessing this module exists to
/// replace. An injury to an uncovered part still reaches the model as prose, still pins
/// `scope-and-safety`, and still shows the user the refer-out boundary — it just has no automated
/// bar. Adding one means writing the document first.
pub static RAILS: &[BodyPartRails] = &[
    BodyPartRails {
        body_part: "lower_back",
        doc_id: "injury-lower-back",
        contraindications: &[
            Contraindication {
                pattern: MovementPattern::LoadedSpinalFlexion,
                bars_from: Severity::Moderate,
                substitutions: &["hip thrust", "glute bridge", "chest-supported row", "seated cable row", "back extension through a pain-free range"],
            },
            Contraindication {
                pattern: MovementPattern::AxialCompression,
                bars_from: Severity::Moderate,
                substitutions: &["leg press", "split squat", "hack squat", "belt squat", "seated press with back support", "landmine press"],
            },
            Contraindication {
                pattern: MovementPattern::EndRangeSpinalFlexion,
                bars_from: Severity::Mild,
                substitutions: &["dead bug", "bird dog", "side plank", "Pallof press"],
            },
            Contraindication {
                pattern: MovementPattern::LoadedSpinalRotation,
                bars_from: Severity::Moderate,
                substitutions: &["Pallof press", "side plank", "bird dog"],
            },
        ],
    },
    BodyPartRails {
        body_part: "shoulder",
        doc_id: "injury-shoulder",
        contraindications: &[
            Contraindication {
                pattern: MovementPattern::BehindTheNeckLoading,
                bars_from: Severity::Mild,
                substitutions: &["front lat pulldown", "neutral-grip pulldown", "chest-supported row"],
            },
            Contraindication {
                pattern: MovementPattern::OverheadPressing,
                bars_from: Severity::Moderate,
                substitutions: &["landmine press", "incline press at a shallow angle", "neutral-grip dumbbell press within tolerance"],
            },
            Contraindication {
                pattern: MovementPattern::WideGripHorizontalPressing,
                bars_from: Severity::Moderate,
                substitutions: &["neutral-grip dumbbell press", "floor press", "machine press with a narrower path"],
            },
            Contraindication {
                pattern: MovementPattern::DeepDipping,
                bars_from: Severity::Mild,
                substitutions: &["close-grip push-up", "push-up on handles", "cable tricep pushdown"],
            },
            Contraindication {
                pattern: MovementPattern::UprightRowing,
                bars_from: Severity::Moderate,
                substitutions: &["face pull", "cable rear-delt fly"],
            },
            // The corpus's own substitution here is "a lighter lateral raise stopped below shoulder
            // height" — which is the *mild* response, and mild does not reach this threshold. Naming
            // it as the moderate substitution would offer back the pattern being removed, and the
            // rail cannot read "lighter" or "below shoulder height" off an exercise name.
            Contraindication {
                pattern: MovementPattern::RaiseAboveShoulderHeight,
                bars_from: Severity::Moderate,
                substitutions: &["face pull", "cable rear-delt fly", "external rotation at the side"],
            },
        ],
    },
    BodyPartRails {
        body_part: "knee",
        doc_id: "injury-knee",
        contraindications: &[
            Contraindication {
                pattern: MovementPattern::HighImpactPlyometrics,
                bars_from: Severity::Mild,
                substitutions: &["non-impact power work", "cycling", "rowing"],
            },
            Contraindication {
                pattern: MovementPattern::DeepKneeFlexionUnderLoad,
                bars_from: Severity::Moderate,
                substitutions: &["box squat to a pain-free depth", "belt squat", "step-up to a low box", "hip thrust", "leg curl"],
            },
            // As with the shoulder raise: the corpus offers "a lighter leg extension through a
            // pain-free range", which is the mild response and does not reach this threshold.
            Contraindication {
                pattern: MovementPattern::OpenChainKneeExtension,
                bars_from: Severity::Moderate,
                substitutions: &["isometric holds", "leg curl", "hip thrust", "cycling"],
            },
            Contraindication {
                pattern: MovementPattern::RunningVolume,
                bars_from: Severity::Moderate,
                substitutions: &["cycling", "elliptical", "swimming", "rowing", "incline walking"],
            },
        ],
    },
];

/// The decision-support boundary, in the user's words rather than a disclaimer's.
///
/// Shown whenever the rail fires. A physiotherapist refers out; the corpus's injury documents all open
/// with a refer-out screen, and this is that screen's one-line form for a surface that has no room for
/// the full list.
pub const REFER_OUT: &str = "This is training decision support, not medical advice \u{2014} for an injury that is new, \
worsening, or came with numbness, weakness or a fall, see a physiotherapist or doctor.";

/// The rails for a body part, if the corpus covers it.
pub fn rails_for(body_part: &str) -> Option<&'static BodyPartRails> {
    RAILS.iter().find(|rails| rails.body_part == body_part)
}

/// The curated document ids for the body parts in `injuries`, for pinning into a
/// [`ScienceQuery`](super::ScienceQuery). Body parts with no document contribute nothing.
pub fn rail_doc_ids<'a>(injuries: impl IntoIterator<Item = &'a str>) -> impl Iterator<Item = &'static str> {
    injuries.into_iter().filter_map(rails_for).map(|rails| rails.doc_id).collect::<Vec<_>>().into_iter()
}

/// One prescribed exercise that an active injury bars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The exercise as the designer named it.
    pub exercise: String,
    /// The body part it loads, in corpus spelling.
    pub body_part: &'static str,
    pub severity: Severity,
    pub pattern: MovementPattern,
    pub substitutions: &'static [&'static str],
}

impl Violation {
    /// One line of the user-facing explanation: what was dropped, and what to do instead.
    pub fn describe(&self) -> String {
        let part = self.body_part.replace('_', " ");
        match self.substitutions {
            [] => format!("{} ({} on a {} {part})", self.exercise, self.pattern.as_str(), self.severity),
            subs => format!("{} \u{2014} {} on a {} {part}; try {} instead", self.exercise, self.pattern.as_str(), self.severity, subs.join(", ")),
        }
    }
}

/// The active injuries in `entries`, as (corpus body part, severity) pairs.
///
/// Only [`HealthEntryType::Injury`] entries with a body part the corpus vocabulary recognises can
/// drive the rail: an illness has a severity but no contraindicated movement pattern, and a wellbeing
/// note ("shoulders feel tight") is an observation rather than a constraint. Both still reach the
/// model as prose through `format_health_entries`.
pub fn active_injuries(entries: &[HealthEntry]) -> impl Iterator<Item = (&'static str, Severity)> {
    entries
        .iter()
        .filter(|entry| entry.entry_type == HealthEntryType::Injury && entry.resolved_at.is_none())
        .filter_map(|entry| {
            let part = super::normalise_body_part(entry.body_part.as_deref()?)?;
            Some((part, entry.severity))
        })
        .collect::<Vec<_>>()
        .into_iter()
}

/// Every way `exercises` contradicts the active injuries in `entries` — empty when the roster is safe
/// to persist.
///
/// The whole rail, and the function the designer's test asserts against. Pure over its inputs, so the
/// test needs no database, no LLM and no catalogue: the argument that a design is safe should not
/// depend on anything that can be stubbed wrongly.
///
/// Exercise names are taken as the designer wrote them rather than resolved through the catalogue
/// first, because an unresolvable name is the more dangerous input, not the less — see
/// [`MovementPattern::fragments`].
pub fn violations(entries: &[HealthEntry], exercises: &[String]) -> Vec<Violation> {
    active_injuries(entries)
        .flat_map(|(body_part, severity)| {
            let rails = rails_for(body_part).into_iter().flat_map(|r| r.contraindications.iter());
            rails
                .filter(move |rule| severity >= rule.bars_from)
                .flat_map(move |rule| {
                    exercises.iter().filter(|name| rule.pattern.matches(name)).map(move |name| Violation {
                        exercise: name.clone(),
                        body_part,
                        severity,
                        pattern: rule.pattern,
                        substitutions: rule.substitutions,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests;

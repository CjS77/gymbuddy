//! Deterministic yes-detection, shared by every place the host has to read a plain
//! "yes" without spending an LLM round-trip on it.
//!
//! Three surfaces need the same judgement and must not drift into three dialects:
//! the session-continuity resume ([`super::continuity`]), the onboarding ask
//! ([`super::onboarding`]) and the programme lock-in ([`super::programme`]). The
//! reason is the same in all three: a small model is unreliable at turning "yeah go
//! on" into the right control flow, and in the onboarding case it has to work on the
//! user's very first message.
//!
//! The shape is two-tier, and the tiers are not interchangeable. A distinctive
//! phrase says nothing *but* yes, so it may match anywhere in the message; a bare
//! acknowledgement is only a yes when it is the entire message, because "ok" is an
//! acceptance while "ok but not this week" is the opposite one.

/// Distinctive multi-word acceptances. Safe to match anywhere in the message
/// because none of them says anything else.
const YES_PHRASES: &[&str] =
    &["let's do it", "lets do it", "set it up", "sounds good", "go for it", "yes please", "sure thing", "why not", "lock it in"];

/// Bare acknowledgements, matched ONLY as the whole message. "ok" alone is a yes;
/// "ok but I'll just log today" is not, and must reach the normal chat path.
const YES_WORDS: &[&str] =
    &["yes", "yeah", "yep", "yup", "ya", "sure", "ok", "okay", "please", "absolutely", "definitely", "do it", "go ahead"];

/// Explicit negations. A phrase match reads the *phrase*, not the sentence around
/// it, so "don't lock it in yet" would otherwise come back as an acceptance. None of
/// these appears inside a [`YES_PHRASES`] entry, so the guard cannot veto a genuine
/// yes — note that "why not" survives it.
const NEGATIONS: &[&str] = &["don't", "dont", "do not", "never", "not yet", "rather not", "hold off"];

/// Does `text` read as a plain affirmative? See the module docs for why the two
/// tiers match differently.
pub(super) fn is_affirmative(text: &str) -> bool {
    let lowered = text.to_lowercase();
    if NEGATIONS.iter().any(|negation| lowered.contains(negation)) {
        return false;
    }
    if YES_PHRASES.iter().any(|phrase| lowered.contains(phrase)) {
        return true;
    }
    let bare = lowered.trim().trim_end_matches(['.', '!', '?', ' ']);
    YES_WORDS.contains(&bare)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affirmatives_and_refusals_are_told_apart() {
        for yes in ["yes", "Yes", "yeah", "yep", "OK", "okay!", "sure.", "Let's do it", "yes please", "why not"] {
            assert!(is_affirmative(yes), "{yes:?} should read as a yes");
        }
        for no in ["no", "no thanks", "not now", "later", "ok but I'll just log for now", "3 sets of bench at 80kg"] {
            assert!(!is_affirmative(no), "{no:?} should NOT read as a yes");
        }
    }

    /// The lock-in turn's own vocabulary ([C4.2]). "Lock it in" is distinctive enough
    /// to match anywhere; "do it" only as the whole message, so "do it later" is not
    /// read as an acceptance.
    #[test]
    fn lock_in_phrasings_are_affirmative() {
        for yes in ["lock it in", "Lock it in!", "yes, lock it in please", "do it", "go ahead"] {
            assert!(is_affirmative(yes), "{yes:?} should read as a yes");
        }
        for no in ["do it later", "go ahead and change week 3 first"] {
            assert!(!is_affirmative(no), "{no:?} should NOT read as a yes");
        }
    }

    /// A phrase match sees the phrase, not the sentence: without the negation guard
    /// every one of these would activate a programme the user just declined.
    #[test]
    fn an_explicit_negation_beats_a_phrase_match() {
        for no in ["don't lock it in yet", "do not set it up", "not yet — sounds good in principle", "I'd rather not, let's do it later"] {
            assert!(!is_affirmative(no), "{no:?} should NOT read as a yes");
        }
        // The guard must not swallow the one yes-phrase that contains a negative word.
        assert!(is_affirmative("why not"));
    }
}

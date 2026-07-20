//! Per-collection row counts over a dump.
//!
//! An export that silently drops a table is the failure mode this module exists to make loud. The
//! counts are taken from the **dump tree**, never from the source database — counting the source
//! would only prove the source can be counted. Reconciliation is the caller's job: count the dump
//! here, `SELECT COUNT(*)` the source, compare. `gymbuddy export` reports the counts so an operator
//! can do that by eye, and the fidelity tests do it exhaustively.
//!
//! Keys are the **v2 collection names**, matching the dump's own vocabulary rather than the legacy
//! table names, so a v1 export and a v2 export of the same data report the same keys.

use std::collections::BTreeMap;
use std::fmt;

use super::model::{Dump, User};

/// Every collection a dump can carry, in reporting order.
///
/// Iterating this rather than the map keeps the report in a deliberate order (globals, then the
/// user tree roughly outside-in, then the archived legacy tables) instead of alphabetical, and
/// guarantees a collection that happens to be empty still shows up as `0` rather than vanishing.
pub const COLLECTIONS: [&str; 23] = [
    "users",
    "groups",
    "group_memberships",
    "philosophies",
    "interview_states",
    "goals",
    "sessions",
    "exercise_entries",
    "unsessioned_entries",
    "sets",
    "session_rosters",
    "roster_exercises",
    "programmes",
    "programme_goals",
    "programme_blocks",
    "programme_slots",
    "health_entries",
    "body_metrics",
    "conversation_history",
    "session_reviews",
    "legacy_schedules",
    "legacy_schedule_exercises",
    "legacy_session_plan_names",
];

/// Collections whose rows are also counted under another key, and are therefore left out of
/// [`RowCounts::total`] so the total stays a true row count.
///
/// `unsessioned_entries` is the only one: those entries are part of `exercise_entries` (which
/// matches the source table exactly) and are broken out separately because an entry with no session
/// is the arm an exporter drops without anyone noticing.
const SUBSET_COLLECTIONS: [&str; 1] = ["unsessioned_entries"];

/// How many rows a dump carries, per collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowCounts(BTreeMap<&'static str, usize>);

impl RowCounts {
    /// Count every collection in `dump`.
    pub fn of(dump: &Dump) -> Self {
        let zeroed = COLLECTIONS.iter().map(|name| (*name, 0usize));
        let mut counts: BTreeMap<&'static str, usize> = zeroed.collect();
        add(&mut counts, [("users", dump.users.len()), ("groups", dump.groups.len())]);
        dump.users.iter().for_each(|user| add(&mut counts, user_counts(user)));
        Self(counts)
    }

    /// Rows in one collection. An unknown name reads as `0`, matching a collection that exists but
    /// holds nothing — callers reconcile against [`COLLECTIONS`], not against absence.
    pub fn get(&self, collection: &str) -> usize {
        self.0.get(collection).copied().unwrap_or(0)
    }

    /// Every collection and its count, in [`COLLECTIONS`] order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, usize)> + '_ {
        COLLECTIONS.iter().map(|name| (*name, self.get(name)))
    }

    /// Total rows in the dump, counting each row once.
    pub fn total(&self) -> usize {
        self.iter().filter(|(name, _)| !SUBSET_COLLECTIONS.contains(name)).map(|(_, count)| count).sum()
    }
}

/// `users=3 groups=2 …` — one line, greppable, and short enough for a log field.
impl fmt::Display for RowCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered = self.iter().map(|(name, count)| format!("{name}={count}")).collect::<Vec<_>>();
        f.write_str(&rendered.join(" "))
    }
}

fn add(counts: &mut BTreeMap<&'static str, usize>, entries: impl IntoIterator<Item = (&'static str, usize)>) {
    entries.into_iter().for_each(|(name, count)| *counts.entry(name).or_default() += count);
}

/// Everything hanging off one user tree. `users` and `groups` are global and counted by the caller.
fn user_counts(user: &User) -> [(&'static str, usize); 21] {
    [
        ("group_memberships", user.group_memberships.len()),
        ("philosophies", user.philosophies.len()),
        ("interview_states", user.interview_states.len()),
        ("goals", user.goals.len()),
        ("sessions", user.sessions.len()),
        ("exercise_entries", user.entries().count()),
        ("unsessioned_entries", user.unsessioned_entries.len()),
        ("sets", user.entries().map(|entry| entry.sets.len()).sum()),
        ("session_rosters", user.session_rosters.len()),
        ("roster_exercises", user.session_rosters.iter().map(|roster| roster.exercises.len()).sum()),
        ("programmes", user.programmes.len()),
        ("programme_goals", user.programmes.iter().map(|programme| programme.goal_ids.len()).sum()),
        ("programme_blocks", user.programmes.iter().map(|programme| programme.blocks.len()).sum()),
        ("programme_slots", user.programmes.iter().map(|programme| programme.slots.len()).sum()),
        ("health_entries", user.health_entries.len()),
        ("body_metrics", user.body_metrics.len()),
        ("conversation_history", user.conversation_history.len()),
        ("session_reviews", user.session_reviews.len()),
        ("legacy_schedules", user.legacy.schedules.len()),
        ("legacy_schedule_exercises", user.legacy.schedules.iter().map(|schedule| schedule.exercises.len()).sum()),
        ("legacy_session_plan_names", user.legacy.session_plan_names.len()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key `user_counts` reports but `COLLECTIONS` omits would be counted and then never
    /// displayed, reconciled, or totalled — invisible in exactly the way this module exists to
    /// prevent.
    #[test]
    fn every_counted_collection_is_declared() {
        let user = User {
            id: 1,
            name: "probe".into(),
            telegram_id: None,
            pubkey: None,
            timezone: "UTC".into(),
            beta_tester: false,
            timers_enabled: false,
            created_at: String::new(),
            updated_at: String::new(),
            group_memberships: Vec::new(),
            philosophies: Vec::new(),
            interview_states: Vec::new(),
            goals: Vec::new(),
            sessions: Vec::new(),
            unsessioned_entries: Vec::new(),
            session_rosters: Vec::new(),
            programmes: Vec::new(),
            health_entries: Vec::new(),
            body_metrics: Vec::new(),
            conversation_history: Vec::new(),
            session_reviews: Vec::new(),
            legacy: Default::default(),
        };
        user_counts(&user)
            .iter()
            .for_each(|(name, _)| assert!(COLLECTIONS.contains(name), "`{name}` is counted but missing from COLLECTIONS"));
        // The two global keys are added by `RowCounts::of` rather than by `user_counts`.
        ["users", "groups"].iter().for_each(|name| assert!(COLLECTIONS.contains(name)));
        assert_eq!(COLLECTIONS.len(), user_counts(&user).len() + 2, "COLLECTIONS carries a key nothing ever counts");
    }

    #[test]
    fn subset_collections_are_declared_and_excluded_from_the_total() {
        SUBSET_COLLECTIONS.iter().for_each(|name| assert!(COLLECTIONS.contains(name), "`{name}` is a subset of nothing declared"));
    }
}

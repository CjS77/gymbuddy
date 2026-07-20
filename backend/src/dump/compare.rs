//! Structural comparison of two dumps — the evidence behind `gymbuddy migrate --verify`.
//!
//! # What is being proved
//!
//! A migration exports a legacy database, builds a v2 one, imports, and re-exports. If the two
//! dumps describe the same data, the import lost nothing. "The same data" cannot mean "byte
//! identical": the importer assigns fresh primary keys, so every id differs, and the second export
//! happens later, so `exported_at` differs. Everything else must match exactly — including every
//! timestamp, because nothing in the import path regenerates one.
//!
//! # Total by construction
//!
//! The comparison is **not** a hand-written list of fields to check. A list is exactly the thing
//! that goes stale: add a column to the schema, a field to the dump model, and a comparator that
//! enumerates fields keeps returning "identical" while quietly ignoring it. That failure mode is
//! worse than having no verifier, because it is indistinguishable from success.
//!
//! Instead both dumps are normalised into `serde_json::Value` and compared whole, key by key. Any
//! field that serialises is compared; a field added to [`super::model`] tomorrow is compared
//! tomorrow, with no edit here. What normalisation does is confined to the three things that
//! legitimately differ, each subtracted deliberately and documented below.
//!
//! # What normalisation removes, and why each is safe
//!
//! 1. **`exported_at` and `source_schema`.** The re-export is a different export of a different
//!    generation. Comparing them would fail on every correct migration.
//! 2. **Primary keys, replaced by positional surrogates.** Ids are renumbered by position after
//!    each collection is sorted, and every *reference* is rewritten through the same map. So the
//!    comparison still proves that a roster points at the same session it used to — it just does
//!    not care what number that session is called. A reference that moved to a different row is a
//!    difference and is reported.
//! 3. **Metric name spellings, canonicalised on both sides.** The import resolves every spelling of
//!    a metric onto one `metrics` row, so a v1 database holding `weight` re-exports as
//!    `bodyweight_kg`. That rewrite is the repair schema v2 exists to make, not a loss — see
//!    [`canonicalise_metrics`] for why comparing raw spellings would fail a correct migration and
//!    report a misleading cascade while doing it.
//! 4. **`legacy`.** Schedules and `signal_id` are archival: schema v2 has nowhere to put them and
//!    the importer skips them by design, so a re-export cannot contain them. Their preservation is
//!    a property of the *dump file*, which is retained, not of the migrated database — and the
//!    original database is never written, so it remains the real archive either way.
//!
//! # The blind spot worth stating
//!
//! The comparison covers exactly what the dump format carries. A column that exists in schema v2
//! but that neither reader reads and no [`super::model`] field holds is invisible here — it would
//! be dropped by the *export*, silently, and this compare would agree the two dumps match. That is
//! why `migrate --verify` also asserts per-table count invariants straight off the database, and
//! why the fidelity tests reconcile the dump against `SELECT COUNT(*)` on the source. Rows cannot
//! hide from those; a column added without a reader still can. Adding a column to schema v2 means
//! adding it to the model and both readers, and no tooling here will remind you.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::model::Dump;
use crate::db::canonical_body_metric;

/// How many differences are reported before the list is truncated. A migration that goes wrong
/// tends to go wrong everywhere at once, and ten thousand lines of diff help nobody find the first
/// one.
const MAX_REPORTED: usize = 50;

/// One structural difference, located by a JSON path into the normalised dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// e.g. `users[0].sessions[2].entries[0].sets[1].value`.
    pub path: String,
    pub before: String,
    pub after: String,
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} != {}", self.path, self.before, self.after)
    }
}

/// Compare a dump against a re-export of the database it was imported into.
///
/// An empty result means the two are structurally identical under the normalisation described in
/// the module doc. Anything else is a migration bug.
pub fn compare(before: &Dump, after: &Dump) -> Vec<Difference> {
    let mut differences = Vec::new();
    diff(&mut String::new(), &normalise(before), &normalise(after), &mut differences);
    differences.truncate(MAX_REPORTED);
    differences
}

/// Render a difference list for an operator, or `None` when there is nothing to report.
pub fn describe(differences: &[Difference]) -> Option<String> {
    (!differences.is_empty()).then(|| differences.iter().map(|difference| format!("  {difference}")).collect::<Vec<_>>().join("\n"))
}

// -----------------------------------------------------------------------------------------------
// Normalisation
// -----------------------------------------------------------------------------------------------

/// The whole dump as canonical JSON: collections sorted, ids replaced by positional surrogates,
/// archival and export-time fields removed.
fn normalise(dump: &Dump) -> Value {
    let mut users: Vec<Value> = dump.users.iter().map(normalise_user).collect();
    sort_by_json(&mut users);
    let mut groups = to_values(&dump.groups);
    sort_by_json(&mut groups);
    let mut root = Map::new();
    // `format` and `dump_version` are compared: two dumps of different shapes are not comparable,
    // and silently tolerating that would be the same mistake as skipping a collection.
    root.insert("format".into(), Value::String(dump.format.clone()));
    root.insert("dump_version".into(), Value::from(dump.dump_version));
    root.insert("groups".into(), Value::Array(groups));
    root.insert("users".into(), Value::Array(users));
    Value::Object(root)
}

/// One user tree, normalised bottom-up: children are given their surrogates before a parent's sort
/// key is computed, so no collection's order depends on ids that have not been assigned yet.
///
/// The order of the steps is the dependency order — philosophies, goals, sessions and slots exist
/// before the rosters that point at them, and rosters before the reviews that point at rosters.
fn normalise_user(user: &super::model::User) -> Value {
    let Value::Object(mut tree) = serde_json::to_value(user).expect("a dump user always serialises") else {
        unreachable!("a struct serialises to an object")
    };

    // Archival by design: never imported, so never re-exported. See the module doc.
    tree.remove("legacy");
    // The user's own id carries no references — the tree is the reference — so it is dropped rather
    // than surrogated.
    tree.remove("id");
    // Before anything sorts or renumbers: the metric name is part of every sort key it appears in.
    canonicalise_metrics(&mut tree);

    let philosophies = renumber_collection(&mut tree, "philosophies");
    let goals = renumber_collection(&mut tree, "goals");
    let sessions = normalise_sessions(&mut tree);
    normalise_entries(&mut tree, "unsessioned_entries");
    let slots = normalise_programmes(&mut tree, &goals);
    let rosters = normalise_rosters(&mut tree, &philosophies, &sessions, &slots);
    normalise_session_reviews(&mut tree, &sessions, &rosters);

    // Everything left holds no id and is referenced by nothing: sorting is all it needs. Listing
    // them is safe in a way that listing *fields* would not be — a collection missing here keeps
    // its source order, which the diff reports as a mismatch rather than passing in silence.
    ["group_memberships", "interview_states", "health_entries", "body_metrics", "conversation_history"].iter().for_each(|key| {
        let items = sorted(collection(&mut tree, key));
        set_collection(&mut tree, key, items);
    });
    Value::Object(tree)
}

/// Compare metric names canonically, because the import deliberately rewrites them.
///
/// Schema v1 spelled a metric as free text in two columns that had to agree by convention —
/// `goals.metric` and `body_metrics.metric`. Schema v2 resolves both onto one `metrics` row, and
/// [`crate::db::Database::get_or_create_metric`] canonicalises on the way in. So a v1 row saying
/// `weight` genuinely comes back as `bodyweight_kg`: that is the broken join R1.1 repaired, not
/// data the migration lost, and `--verify` must not condemn a correct migration for it.
///
/// The cascade is what makes this worth handling rather than tolerating. Collections sort by their
/// content, so a metric renamed on one side alone also *reorders* `body_metrics` — and the diff
/// then reports a run of unrelated `value` mismatches from rows compared against the wrong
/// partners. The operator sees what looks like scrambled measurements at exactly the moment they
/// most need to trust the tool.
///
/// Only equivalent spellings collapse: two genuinely different metrics canonicalise to themselves
/// and still compare as a difference.
fn canonicalise_metrics(tree: &mut Map<String, Value>) {
    ["goals", "body_metrics"].iter().for_each(|key| {
        tree.get_mut(*key).and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut).for_each(canonicalise_one);
    });
}

/// A goal denominated by an exercise carries no metric, and `null` is left exactly as it is.
fn canonicalise_one(item: &mut Map<String, Value>) {
    if let Some(canonical) = item.get("metric").and_then(Value::as_str).map(canonical_body_metric) {
        item.insert("metric".into(), Value::from(canonical));
    }
}

/// Take a collection out of the tree, renumber it, and put it back.
fn renumber_collection(tree: &mut Map<String, Value>, key: &str) -> HashMap<i64, i64> {
    let mut items = collection(tree, key);
    let map = renumber(&mut items);
    set_collection(tree, key, items);
    map
}

/// Sessions carry entries, which carry sets. Sets are sorted, then entries renumbered, then the
/// sessions themselves — each level settled before the level above uses it as a sort key.
fn normalise_sessions(tree: &mut Map<String, Value>) -> HashMap<i64, i64> {
    let mut sessions = collection(tree, "sessions");
    sessions.iter_mut().for_each(|session| {
        normalise_entries(object(session), "entries");
    });
    let map = renumber(&mut sessions);
    set_collection(tree, "sessions", sessions);
    map
}

/// Exercise entries and their sets. Nothing references an entry, so they need sorting and their own
/// surrogates but no map — `unsessioned_entries` goes through here too, which is the point: it is
/// reachable by no other path and is the arm a round trip drops without anyone noticing.
fn normalise_entries(tree: &mut Map<String, Value>, key: &str) {
    let mut entries = collection(tree, key);
    entries.iter_mut().for_each(|entry| {
        let sets = sorted(collection(object(entry), "sets"));
        set_collection(object(entry), "sets", sets);
    });
    renumber(&mut entries);
    set_collection(tree, key, entries);
}

/// Programmes, returning the user-wide slot surrogate map that rosters resolve against.
///
/// Slots nest under programmes but are referenced from rosters, so their surrogates must be unique
/// across the whole user — a slot numbered per programme would let a roster rebound to the *other*
/// programme's slot 1 and compare equal.
fn normalise_programmes(tree: &mut Map<String, Value>, goals: &HashMap<i64, i64>) -> HashMap<i64, i64> {
    let mut programmes = collection(tree, "programmes");
    programmes.iter_mut().for_each(|programme| {
        let programme = object(programme);
        let blocks = sorted(collection(programme, "blocks"));
        set_collection(programme, "blocks", blocks);
        remap_list(programme, "goal_ids", goals);
    });

    let slot_map = number_slots(&mut programmes);
    // Slot ids are surrogates now, so a plain content sort inside each programme is stable, and the
    // programmes themselves can be ordered by content that no longer carries a source id.
    programmes.iter_mut().for_each(|programme| {
        let slots = sorted(collection(object(programme), "slots"));
        set_collection(object(programme), "slots", slots);
    });
    renumber(&mut programmes);
    set_collection(tree, "programmes", programmes);
    slot_map
}

/// Number every slot in the user, ordering them by their parent programme's content and then their
/// own — never by either one's source id, which is exactly what differs between the two dumps.
fn number_slots(programmes: &mut [Value]) -> HashMap<i64, i64> {
    let mut keyed: Vec<(String, String, i64)> = programmes
        .iter()
        .flat_map(|programme| {
            // The parent key excludes `slots` as well as `id`: the slots still carry source ids at
            // this point, and letting them into the parent's key would put the source database's
            // numbering back into the ordering by the back door.
            let parent = content_key(programme, &["id", "slots"]);
            slots_of(programme).map(move |slot| (parent.clone(), content_key(slot, &["id"]), source_id(slot)))
        })
        .collect();
    keyed.sort();

    let map: HashMap<i64, i64> = keyed.iter().enumerate().map(|(index, (_, _, source))| (*source, index as i64 + 1)).collect();
    programmes.iter_mut().filter_map(|programme| programme.get_mut("slots")?.as_array_mut()).flatten().for_each(|slot| {
        remap(object(slot), "id", &map);
    });
    map
}

fn slots_of(programme: &Value) -> impl Iterator<Item = &Value> {
    programme.get("slots").and_then(Value::as_array).into_iter().flatten()
}

fn source_id(value: &Value) -> i64 {
    value.get("id").and_then(Value::as_i64).unwrap_or_default()
}

fn normalise_rosters(
    tree: &mut Map<String, Value>,
    philosophies: &HashMap<i64, i64>,
    sessions: &HashMap<i64, i64>,
    slots: &HashMap<i64, i64>,
) -> HashMap<i64, i64> {
    let mut rosters = collection(tree, "session_rosters");
    rosters.iter_mut().for_each(|roster| {
        let roster = object(roster);
        remap(roster, "philosophy_id", philosophies);
        remap(roster, "session_id", sessions);
        remap(roster, "programme_slot_id", slots);
        let exercises = sorted(collection(roster, "exercises"));
        set_collection(roster, "exercises", exercises);
    });
    let map = renumber(&mut rosters);
    set_collection(tree, "session_rosters", rosters);
    map
}

fn normalise_session_reviews(tree: &mut Map<String, Value>, sessions: &HashMap<i64, i64>, rosters: &HashMap<i64, i64>) {
    let mut reviews = collection(tree, "session_reviews");
    reviews.iter_mut().for_each(|review| {
        let review = object(review);
        remap(review, "session_id", sessions);
        remap(review, "roster_id", rosters);
    });
    set_collection(tree, "session_reviews", sorted(reviews));
}

// -----------------------------------------------------------------------------------------------
// Value helpers
// -----------------------------------------------------------------------------------------------

fn to_values<T: serde::Serialize>(items: &[T]) -> Vec<Value> {
    items.iter().map(|item| serde_json::to_value(item).expect("a dump model always serialises")).collect()
}

fn object(value: &mut Value) -> &mut Map<String, Value> {
    value.as_object_mut().expect("a dump collection holds objects")
}

/// Take a collection out of a tree, leaving nothing behind. Absent (the `skip_serializing_if` case)
/// and empty are the same thing, which is what makes the two spellings compare equal.
fn collection(tree: &mut Map<String, Value>, key: &str) -> Vec<Value> {
    match tree.remove(key) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

fn set_collection(tree: &mut Map<String, Value>, key: &str, items: Vec<Value>) {
    tree.insert(key.to_string(), Value::Array(items));
}

/// Sort by the element's own canonical JSON. Deterministic, and total over whatever fields the
/// element happens to have — no per-type key list to fall out of date.
fn sort_by_json(items: &mut [Value]) {
    items.sort_by_cached_key(Value::to_string);
}

fn sorted(mut items: Vec<Value>) -> Vec<Value> {
    sort_by_json(&mut items);
    items
}

/// An element's canonical JSON with `skip` keys removed — a sort key that ignores the fields whose
/// values legitimately differ between the two dumps.
fn content_key(value: &Value, skip: &[&str]) -> String {
    let mut keyed = value.clone();
    if let Some(map) = keyed.as_object_mut() {
        skip.iter().for_each(|key| {
            map.remove(*key);
        });
    }
    keyed.to_string()
}

/// Sort a collection by content, then replace each element's `id` with its 1-based position,
/// returning `source id → surrogate` for the references that point at it.
///
/// The sort key omits the element's own `id`, so ordering depends only on content and on references
/// that were already surrogated — never on the source database's key assignment.
fn renumber(items: &mut [Value]) -> HashMap<i64, i64> {
    items.sort_by_cached_key(|item| content_key(item, &["id"]));
    items
        .iter_mut()
        .enumerate()
        .filter_map(|(index, item)| {
            let surrogate = index as i64 + 1;
            let source = item.as_object_mut()?.insert("id".into(), Value::from(surrogate))?.as_i64()?;
            Some((source, surrogate))
        })
        .collect()
}

/// Rewrite a reference through a surrogate map. An id with no entry is left exactly as it was, so
/// the comparison reports it rather than quietly nulling it — an unresolvable reference is the bug
/// the verifier is looking for, not noise to suppress.
fn remap(tree: &mut Map<String, Value>, key: &str, map: &HashMap<i64, i64>) {
    let Some(source) = tree.get(key).and_then(Value::as_i64) else { return };
    if let Some(surrogate) = map.get(&source) {
        tree.insert(key.to_string(), Value::from(*surrogate));
    }
}

/// The list-valued form of [`remap`] — `programme.goal_ids`. Sorted afterwards so the link set is
/// compared as a set rather than in whatever order the source happened to return it.
fn remap_list(tree: &mut Map<String, Value>, key: &str, map: &HashMap<i64, i64>) {
    let mut ids: Vec<i64> =
        collection(tree, key).iter().filter_map(Value::as_i64).map(|id| map.get(&id).copied().unwrap_or(id)).collect();
    ids.sort_unstable();
    set_collection(tree, key, ids.into_iter().map(Value::from).collect());
}

// -----------------------------------------------------------------------------------------------
// Diffing
// -----------------------------------------------------------------------------------------------

/// Walk two normalised values together, recording every path at which they disagree.
///
/// Recursing rather than comparing the two roots wholesale is what makes a failure actionable: a
/// verifier that can only say "these differ" leaves an operator to diff two multi-megabyte JSON
/// files by hand at exactly the moment they are least able to.
fn diff(path: &mut String, before: &Value, after: &Value, out: &mut Vec<Difference>) {
    if out.len() >= MAX_REPORTED {
        return;
    }
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => diff_objects(path, before, after, out),
        (Value::Array(before), Value::Array(after)) => diff_arrays(path, before, after, out),
        _ if before != after => out.push(difference(path, before, after)),
        _ => {}
    }
}

fn diff_objects(path: &mut String, before: &Map<String, Value>, after: &Map<String, Value>, out: &mut Vec<Difference>) {
    // The union of both key sets: a key present on one side only is a difference, and iterating
    // only the left-hand side would miss a field the re-export invented.
    let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter().for_each(|key| {
        let restore = path.len();
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(key);
        let missing = Value::Null;
        diff(path, before.get(key).unwrap_or(&missing), after.get(key).unwrap_or(&missing), out);
        path.truncate(restore);
    });
}

fn diff_arrays(path: &mut String, before: &[Value], after: &[Value], out: &mut Vec<Difference>) {
    if before.len() != after.len() {
        out.push(Difference {
            path: format!("{path}.length"),
            before: before.len().to_string(),
            after: after.len().to_string(),
        });
    }
    (0..before.len().min(after.len())).for_each(|index| {
        let restore = path.len();
        path.push_str(&format!("[{index}]"));
        diff(path, &before[index], &after[index], out);
        path.truncate(restore);
    });
}

fn difference(path: &str, before: &Value, after: &Value) -> Difference {
    Difference { path: path.to_string(), before: before.to_string(), after: after.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump::model::*;

    fn dump_with(users: Vec<User>) -> Dump {
        Dump {
            format: DUMP_FORMAT.to_string(),
            dump_version: DUMP_VERSION,
            source_schema: SourceSchema { generation: 1, user_version: 13 },
            exported_at: "2026-07-20T12:00:00+00:00".to_string(),
            groups: Vec::new(),
            users,
        }
    }

    fn user() -> User {
        User {
            id: 1,
            name: "Alice".into(),
            telegram_id: None,
            pubkey: None,
            timezone: "UTC".into(),
            beta_tester: false,
            timers_enabled: true,
            created_at: "2026-01-01 09:00:00".into(),
            updated_at: "2026-01-01 09:00:00".into(),
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
            legacy: Legacy::default(),
        }
    }

    fn session(id: i64, started_at: &str) -> Session {
        Session {
            id,
            started_at: started_at.into(),
            ended_at: None,
            intent: None,
            overall_effort: None,
            effort_source: None,
            felt: None,
            cut_short: false,
            cut_short_reason: None,
            entries: Vec::new(),
        }
    }

    fn roster(id: i64, session_id: Option<i64>) -> SessionRoster {
        SessionRoster {
            id,
            title: "Push".into(),
            rationale: None,
            philosophy_id: None,
            status: "completed".into(),
            session_id,
            programme_slot_id: None,
            override_note: None,
            created_at: "2026-02-01 16:00:00".into(),
            updated_at: "2026-02-01 18:15:00".into(),
            exercises: Vec::new(),
        }
    }

    fn goal_on(metric: &str) -> Goal {
        Goal {
            id: 12,
            kind: "bodyweight".into(),
            exercise: None,
            metric: Some(metric.into()),
            target_value: 78.0,
            direction: "decrease".into(),
            priority: 3,
            start_date: "2026-01-01".into(),
            target_date: None,
            achieved: false,
            achieved_at: None,
            notes: None,
            created_at: "2026-01-01 09:00:00".into(),
            updated_at: "2026-01-01 09:00:00".into(),
        }
    }

    fn measurement(metric: &str) -> BodyMetric {
        BodyMetric { metric: metric.into(), value: 82.5, measured_at: "2026-01-10 07:00:00".into() }
    }

    #[test]
    fn a_dump_is_identical_to_itself() {
        let dump = dump_with(vec![user()]);
        assert_eq!(compare(&dump, &dump), Vec::new());
    }

    /// The property the whole verifier rests on: the importer reassigns every primary key, so ids
    /// that differ by a constant must not register as a difference.
    #[test]
    fn renumbered_ids_are_not_a_difference() {
        let mut before = user();
        before.sessions = vec![session(21, "2026-02-01 17:00:00"), session(22, "2026-02-03 17:00:00")];
        before.session_rosters = vec![roster(81, Some(22))];

        let mut after = user();
        after.sessions = vec![session(1, "2026-02-01 17:00:00"), session(2, "2026-02-03 17:00:00")];
        after.session_rosters = vec![roster(1, Some(2))];

        assert_eq!(compare(&dump_with(vec![before]), &dump_with(vec![after])), Vec::new());
    }

    /// The other half of the same property: a reference that now points at a *different* row must
    /// register, or the compare would bless a migration that detached a roster from its session.
    #[test]
    fn a_reference_pointing_at_a_different_row_is_a_difference() {
        let mut before = user();
        before.sessions = vec![session(21, "2026-02-01 17:00:00"), session(22, "2026-02-03 17:00:00")];
        before.session_rosters = vec![roster(81, Some(22))];

        let mut after = user();
        after.sessions = vec![session(1, "2026-02-01 17:00:00"), session(2, "2026-02-03 17:00:00")];
        // Rebound to the first session.
        after.session_rosters = vec![roster(1, Some(1))];

        let differences = compare(&dump_with(vec![before]), &dump_with(vec![after]));
        assert_eq!(differences.len(), 1, "expected exactly one difference, got {differences:?}");
        assert!(differences[0].path.ends_with("session_id"), "unexpected path: {}", differences[0].path);
    }

    /// A roster whose session reference was dropped entirely — the quiet loss the importer's
    /// `resolve` refuses to produce, pinned here so the verifier would catch it if it ever did.
    #[test]
    fn a_dropped_reference_is_a_difference() {
        let mut before = user();
        before.sessions = vec![session(21, "2026-02-01 17:00:00")];
        before.session_rosters = vec![roster(81, Some(21))];

        let mut after = user();
        after.sessions = vec![session(1, "2026-02-01 17:00:00")];
        after.session_rosters = vec![roster(1, None)];

        let differences = compare(&dump_with(vec![before]), &dump_with(vec![after]));
        assert!(differences.iter().any(|d| d.path.ends_with("session_id")), "expected a session_id difference, got {differences:?}");
    }

    /// Collections are compared as sets: the two readers may order rows differently and that is not
    /// a data difference.
    #[test]
    fn ordering_within_a_collection_is_not_a_difference() {
        let mut before = user();
        before.sessions = vec![session(1, "2026-02-01 17:00:00"), session(2, "2026-02-03 17:00:00")];
        let mut after = user();
        after.sessions = vec![session(9, "2026-02-03 17:00:00"), session(8, "2026-02-01 17:00:00")];
        assert_eq!(compare(&dump_with(vec![before]), &dump_with(vec![after])), Vec::new());
    }

    /// Timestamps are the one thing a migration must never regenerate, so they are compared exactly
    /// — not parsed, not normalised, not tolerated within a window.
    #[test]
    fn a_regenerated_timestamp_is_a_difference() {
        let mut before = user();
        before.sessions = vec![session(1, "2026-02-01 17:00:00")];
        let mut after = user();
        after.sessions = vec![session(1, "2026-07-20 12:00:00")];
        let differences = compare(&dump_with(vec![before]), &dump_with(vec![after]));
        assert!(differences.iter().any(|d| d.path.ends_with("started_at")), "expected a started_at difference, got {differences:?}");
    }

    /// A row that vanished must be reported even though every surviving row still matches — the
    /// length check is what catches a collection the importer walked past.
    #[test]
    fn a_missing_row_is_a_difference() {
        let mut before = user();
        before.sessions = vec![session(1, "2026-02-01 17:00:00"), session(2, "2026-02-03 17:00:00")];
        let mut after = user();
        after.sessions = vec![session(1, "2026-02-01 17:00:00")];
        let differences = compare(&dump_with(vec![before]), &dump_with(vec![after]));
        assert!(differences.iter().any(|d| d.path.ends_with("sessions.length")), "expected a length difference, got {differences:?}");
    }

    /// `exported_at` and `source_schema` differ on every correct migration, by definition.
    #[test]
    fn export_metadata_is_not_compared() {
        let before = dump_with(vec![user()]);
        let mut after = dump_with(vec![user()]);
        after.exported_at = "2027-01-01T00:00:00+00:00".into();
        after.source_schema = SourceSchema { generation: 2, user_version: 1 };
        assert_eq!(compare(&before, &after), Vec::new());
    }

    /// The legacy block is archival and never survives a migration; excluding it is deliberate and
    /// is the compare's documented blind spot.
    #[test]
    fn the_legacy_block_is_excluded() {
        let before_user = User {
            legacy: Legacy { signal_id: Some("signal-alice".into()), schedules: Vec::new(), session_plan_names: Vec::new() },
            ..user()
        };
        assert_eq!(compare(&dump_with(vec![before_user]), &dump_with(vec![user()])), Vec::new());
    }

    /// The import resolves every spelling of a metric onto one `metrics` row, so a v1 goal saying
    /// `weight` genuinely re-exports as `bodyweight_kg`. That is the repair schema v2 exists to
    /// make, and `--verify` must not condemn a correct migration for it.
    #[test]
    fn equivalent_metric_spellings_are_not_a_difference() {
        let before = User { goals: vec![goal_on("weight")], body_metrics: vec![measurement("Body Weight")], ..user() };
        let after = User { goals: vec![goal_on("bodyweight_kg")], body_metrics: vec![measurement("bodyweight_kg")], ..user() };
        assert_eq!(compare(&dump_with(vec![before]), &dump_with(vec![after])), Vec::new());
    }

    /// The other side of that leniency, and the reason it is safe: canonicalisation collapses
    /// *spellings*, never distinct metrics. A weigh-in that came back as a body-fat reading is a
    /// difference and stays one.
    #[test]
    fn a_genuinely_different_metric_is_still_a_difference() {
        let before = User { body_metrics: vec![measurement("bodyweight_kg")], ..user() };
        let after = User { body_metrics: vec![measurement("body_fat_pct")], ..user() };
        let differences = compare(&dump_with(vec![before]), &dump_with(vec![after]));
        assert!(differences.iter().any(|d| d.path.ends_with("metric")), "expected a metric difference, got {differences:?}");
    }

    /// Entries with no session hang off the user and are reachable by no other path — the arm most
    /// easily dropped, so the compare is pinned on it explicitly.
    #[test]
    fn a_dropped_unsessioned_entry_is_a_difference() {
        let entry = ExerciseEntry {
            id: 33,
            start_timestamp: "2026-01-15 12:00:00".into(),
            end_timestamp: None,
            comment: Some("logged outside a session".into()),
            sets: Vec::new(),
        };
        let before = User { unsessioned_entries: vec![entry], ..user() };
        let differences = compare(&dump_with(vec![before]), &dump_with(vec![user()]));
        assert!(
            differences.iter().any(|d| d.path.contains("unsessioned_entries")),
            "expected an unsessioned_entries difference, got {differences:?}"
        );
    }
}

//! Export tests, driven by the frozen schema v1 fixture.
//!
//! The fixture is a real database: `tests/fixtures/v1_migrations/` applied, then
//! `tests/fixtures/v1_seed.sql` on top. Both are frozen copies rather than references to the live
//! `backend/migrations/`, because Phase 1 of the realignment replaces the live set with schema v2
//! while the v1 reader must keep reading v1 databases — a test built on the live set would quietly
//! stop testing v1 the day that happens. A hand-written `CREATE TABLE` approximation was never an
//! option either: it drifts from the schema it claims to test.
//!
//! Three layers of assertion here, weakest to strongest:
//!
//! 1. **Named facts** — the translations in the module's mapping table, checked one by one. These
//!    say what the exporter is *supposed* to do.
//! 2. **Count invariants** — every collection's row count against `SELECT COUNT(*)` on the source.
//!    These catch a table walked past entirely, which no named assertion can: a test only asserts
//!    about data someone thought to look for, and the risk is forgetting one.
//! 3. **The snapshot** — the whole envelope, byte for byte. This catches everything else: a field
//!    dropped from one row, an order that changed, a NULL that became an empty string.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use super::*;

use super::fixtures::{self, V1_USER_VERSION, seeded_v1_db, source_row_counts};

fn alice(dump: &Dump) -> &model::User {
    user(dump, "Alice")
}

fn user<'a>(dump: &'a Dump, name: &str) -> &'a model::User {
    dump.users.iter().find(|user| user.name == name).unwrap_or_else(|| panic!("{name} missing from dump"))
}

fn session(user: &model::User, id: i64) -> &model::Session {
    user.sessions.iter().find(|session| session.id == id).unwrap_or_else(|| panic!("session {id} missing from dump"))
}

fn roster<'a>(user: &'a model::User, title: &str) -> &'a model::SessionRoster {
    user.session_rosters.iter().find(|roster| roster.title == title).unwrap_or_else(|| panic!("roster {title} missing from dump"))
}

/// Distinct values of one field across a collection — the shape every enum-coverage check takes.
fn distinct<T: Ord, I: IntoIterator<Item = T>>(values: I) -> BTreeSet<T> {
    values.into_iter().collect()
}

// -----------------------------------------------------------------------------------------------
// Envelope and shape.
// -----------------------------------------------------------------------------------------------

#[test]
fn envelope_carries_format_version_and_provenance() {
    let dump = export(&seeded_v1_db()).unwrap();
    assert_eq!(dump.format, DUMP_FORMAT);
    assert_eq!(dump.dump_version, DUMP_VERSION);
    assert_eq!(dump.source_schema.generation, 1);
    assert_eq!(dump.source_schema.user_version, V1_USER_VERSION);
    assert!(dump.exported_at.contains('T'), "exported_at should be RFC 3339, got {}", dump.exported_at);
}

#[test]
fn every_user_gets_a_tree_and_groups_stay_global() {
    let dump = export(&seeded_v1_db()).unwrap();
    assert_eq!(dump.users.len(), 3);
    assert_eq!(dump.groups.len(), 2, "groups are global data, not part of a user tree");
    assert_eq!(dump.groups[0].name, "beta");

    let alice = alice(&dump);
    assert_eq!(alice.group_memberships.len(), 2);
    assert_eq!(alice.group_memberships[0].group, "beta", "membership references the group by name, not id");
    assert_eq!(alice.group_memberships[0].level, "admin");

    assert!(user(&dump, "Bob").sessions.is_empty(), "a user with no data still gets a tree");
}

// -----------------------------------------------------------------------------------------------
// Vocabulary: the v1 reader must emit v2 names and values, never legacy ones.
// -----------------------------------------------------------------------------------------------

#[test]
fn workout_plans_export_as_session_rosters_with_proposed_renamed_to_draft() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);
    assert_eq!(alice.session_rosters.len(), 4);

    let draft = roster(alice, "Next Pull");
    assert_eq!(draft.status, "draft", "v1 'proposed' must land as v2 'draft'");
    assert_eq!(draft.philosophy_id, Some(7));
    assert!(draft.exercises.is_empty(), "a roster with no exercises is still a roster");

    let completed = roster(alice, "Push Day");
    assert_eq!(completed.status, "completed", "statuses outside the rename pass through");
    assert_eq!(completed.session_id, Some(21), "the roster still points at the session it ran as");
    assert_eq!(completed.programme_slot_id, Some(71), "program_slot_id is renamed, and the reference survives");
    assert_eq!(completed.override_note.as_deref(), Some("no dips today"));
}

#[test]
fn every_v1_roster_status_survives_the_rename() {
    let dump = export(&seeded_v1_db()).unwrap();
    let statuses = distinct(dump.users.iter().flat_map(|user| &user.session_rosters).map(|roster| roster.status.as_str()));
    assert_eq!(statuses, distinct(["draft", "active", "completed", "abandoned"]), "only `proposed` is renamed; the rest pass through");
}

#[test]
fn workout_plan_exercises_export_as_roster_exercises_in_order() {
    let dump = export(&seeded_v1_db()).unwrap();
    let roster = roster(alice(&dump), "Push Day");
    assert_eq!(roster.exercises.len(), 3);

    let first = &roster.exercises[0];
    assert_eq!(first.exercise.name, "Flat Barbell Bench Press");
    assert_eq!(first.target_sets, Some(3));
    assert_eq!(first.target_weight_kg, Some(80.0));
    assert_eq!(first.notes.as_deref(), Some("controlled tempo"));

    assert_eq!(roster.exercises[1].exercise.name, "Plank");
    assert_eq!(roster.exercises[1].target_secs, Some(60));
    assert_eq!(roster.exercises[1].target_reps, None, "an unset target stays NULL rather than becoming zero");

    let orders = roster.exercises.iter().map(|exercise| exercise.order_idx).collect::<Vec<_>>();
    assert_eq!(orders, vec![0, 1, 2], "prescribed order is most of what a roster is");
}

#[test]
fn programs_export_as_programmes_with_blocks_slots_and_goal_links() {
    let dump = export(&seeded_v1_db()).unwrap();
    let programmes = &alice(&dump).programmes;
    assert_eq!(programmes.len(), 3);

    let programme = programmes.iter().find(|programme| programme.title == "Winter Strength").unwrap();
    assert_eq!(programme.days_per_week, 4);
    assert_eq!(programme.goal_ids, vec![11, 13], "program_goals becomes goal_ids into this user's goals");
    assert_eq!(programme.blocks.len(), 3);
    assert_eq!(programme.blocks[0].focus, "hypertrophy");
    assert_eq!(programme.blocks[1].notes, None);
    assert_eq!(programme.slots.len(), 4);
    assert_eq!(programme.slots[0].id, 71);
    assert_eq!(programme.slots[0].status, "filled");
}

#[test]
fn every_programme_and_slot_status_reaches_the_dump() {
    let dump = export(&seeded_v1_db()).unwrap();
    let programmes = || dump.users.iter().flat_map(|user| &user.programmes);
    assert_eq!(
        distinct(programmes().map(|programme| programme.status.as_str())),
        distinct(["draft", "active", "completed", "abandoned"]),
        "programme status passes through, so every value must be seen passing through"
    );
    assert_eq!(
        distinct(programmes().flat_map(|programme| &programme.slots).map(|slot| slot.status.as_str())),
        distinct(["pending", "filled", "missed", "skipped"])
    );
}

#[test]
fn session_notes_become_intent_with_the_plan_sentinel_stripped_into_legacy() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);

    assert_eq!(session(alice, 21).intent.as_deref(), Some("felt strong"), "the `plan:` prefix must not survive into intent");
    assert_eq!(session(alice, 22).intent.as_deref(), Some("no sentinel here"), "notes without a sentinel pass through verbatim");
    assert_eq!(session(alice, 23).intent, None, "a sentinel with no body leaves no intent behind, not an empty string");
    assert_eq!(session(alice, 24).intent, None, "NULL notes stay NULL");

    assert!(session(alice, 22).cut_short);
    assert_eq!(session(alice, 22).cut_short_reason.as_deref(), Some("gym closed"));

    let archived = alice.legacy.session_plan_names.iter().map(|plan| (plan.session_id, plan.plan.as_str())).collect::<Vec<_>>();
    assert_eq!(archived, vec![(21, "Push Day"), (23, "Leg Day")], "the stripped schedule name is archived, not discarded");
}

#[test]
fn every_session_outcome_value_reaches_the_dump() {
    let dump = export(&seeded_v1_db()).unwrap();
    let sessions = || dump.users.iter().flat_map(|user| &user.sessions);
    assert_eq!(
        distinct(sessions().filter_map(|session| session.overall_effort.as_deref())),
        distinct(["easy", "medium", "hard", "failure"])
    );
    assert_eq!(distinct(sessions().filter_map(|session| session.felt.as_deref())), distinct(["great", "good", "ok", "rough"]));
    assert!(sessions().any(|session| session.overall_effort.is_none()), "an unrated session keeps NULL rather than gaining a default");
}

#[test]
fn v2_only_columns_are_present_and_empty() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);
    assert!(alice.sessions.iter().all(|session| session.effort_source.is_none()), "v1 has no effort_source");
    assert!(alice.goals.iter().all(|goal| goal.achieved_at.is_none()), "v1 records `achieved` but never when");
    assert!(alice.session_reviews.is_empty(), "v1 has no session_reviews table");
}

#[test]
fn no_legacy_vocabulary_survives_into_the_json() {
    let json = to_json(&export(&seeded_v1_db()).unwrap()).unwrap();
    // Field names only — user *content* may legitimately contain these words, so check the keys.
    ["\"workout_plans\"", "\"workout_plan_exercises\"", "\"program_slot_id\"", "\"plan_id\"", "\"notes\": \"plan:"]
        .iter()
        .for_each(|legacy| assert!(!json.contains(legacy), "dump still contains legacy vocabulary {legacy}"));
    assert!(json.contains("\"session_rosters\""));
    assert!(json.contains("\"programmes\""));
    assert!(json.contains("\"intent\""));
}

// -----------------------------------------------------------------------------------------------
// Exercise references travel as names.
// -----------------------------------------------------------------------------------------------

#[test]
fn sets_reference_exercises_by_canonical_name_and_parent() {
    let dump = export(&seeded_v1_db()).unwrap();
    let entry = session(alice(&dump), 21).entries.iter().find(|entry| entry.id == 31).unwrap();

    assert_eq!(entry.sets.len(), 2);
    assert_eq!(entry.sets[0].exercise.name, "Flat Barbell Bench Press");
    assert_eq!(entry.sets[0].exercise.parent.as_deref(), Some("Bench Press"), "parent disambiguates the leaf name");
    assert_eq!(entry.sets[0].measurement_type, "weight_reps", "measurement types travel as names too");
    assert_eq!(entry.sets[0].count, Some(8));
    assert_eq!(entry.sets[0].value, 80.0);
}

#[test]
fn time_based_sets_keep_their_null_count() {
    let dump = export(&seeded_v1_db()).unwrap();
    let plank = session(alice(&dump), 21).entries.iter().find(|entry| entry.id == 32).unwrap();
    assert_eq!(plank.sets[0].measurement_type, "time_based");
    assert_eq!(plank.sets[0].count, None, "the (count, value) polymorphism is preserved verbatim");
    assert_eq!(plank.sets[0].value, 60.0);
}

#[test]
fn every_measurement_type_and_difficulty_reaches_the_dump() {
    let dump = export(&seeded_v1_db()).unwrap();
    let sets = || dump.users.iter().flat_map(|user| user.entries()).flat_map(|entry| &entry.sets);
    assert_eq!(
        distinct(sets().map(|set| set.measurement_type.as_str())),
        distinct(["weight_reps", "time_based", "distance_based", "level_based", "score_based"]),
        "every measurement type must be proven to resolve id → name"
    );
    assert_eq!(distinct(sets().map(|set| set.perceived_difficulty.as_str())), distinct(["easy", "medium", "hard", "failure"]));
    assert!(sets().filter(|set| set.measurement_type != "weight_reps").all(|set| set.count.is_none()));
}

#[test]
fn goals_reference_their_exercise_by_name_and_keep_metric_names_as_text() {
    let dump = export(&seeded_v1_db()).unwrap();
    let goals = &alice(&dump).goals;
    let goal = |id: i64| goals.iter().find(|goal| goal.id == id).unwrap();

    let strength = goal(11);
    assert_eq!(strength.exercise.as_ref().unwrap().name, "Bench Press");
    assert_eq!(strength.exercise.as_ref().unwrap().parent.as_deref(), Some("Pectoral"));
    assert_eq!(strength.metric, None);

    let bodyweight = goal(12);
    assert_eq!(bodyweight.exercise, None);
    assert_eq!(bodyweight.metric.as_deref(), Some("bodyweight_kg"), "v2 resolves this name to a metrics row on import");
    assert!(bodyweight.achieved);
    assert_eq!(bodyweight.target_date, None);

    // A goal on a muscle_group has no parent to name — the one ExerciseRef arm where `parent` is
    // legitimately absent, and the one a careless `parent.unwrap()` would panic on.
    let habit = goal(15);
    assert_eq!(habit.exercise.as_ref().unwrap().name, "Cardio");
    assert_eq!(habit.exercise.as_ref().unwrap().parent, None, "a muscle_group is the root of the taxonomy");
}

#[test]
fn every_goal_kind_and_direction_reaches_the_dump() {
    let dump = export(&seeded_v1_db()).unwrap();
    let goals = || dump.users.iter().flat_map(|user| &user.goals);
    assert_eq!(
        distinct(goals().map(|goal| goal.kind.as_str())),
        distinct(["strength", "endurance", "bodyweight", "body_composition", "habit"])
    );
    assert_eq!(distinct(goals().map(|goal| goal.direction.as_str())), distinct(["increase", "decrease"]));
    assert!(goals().all(|goal| goal.exercise.is_some() || goal.metric.is_some()), "the schema's either-or must hold in the dump too");
}

#[test]
fn no_catalogue_ids_leak_into_the_dump() {
    let dump = export(&seeded_v1_db()).unwrap();
    // Exercise/measurement ids are the ones that drift between generations; nothing may carry them.
    let json = to_json(&dump).unwrap();
    ["\"exercise_type_id\"", "\"measurement_type_id\"", "\"parent_id\""]
        .iter()
        .for_each(|key| assert!(!json.contains(key), "dump leaks catalogue id field {key}"));
}

// -----------------------------------------------------------------------------------------------
// Fidelity: nothing is silently dropped.
// -----------------------------------------------------------------------------------------------

#[test]
fn entries_without_a_session_are_kept_under_the_user() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);
    assert_eq!(alice.unsessioned_entries.len(), 2, "a NULL session_id entry has no session to nest under");
    assert_eq!(alice.unsessioned_entries.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![33, 36]);
    assert_eq!(alice.unsessioned_entries[0].sets[0].exercise.name, "Squat");

    // Scoped per user, like everything else in a tree.
    assert_eq!(user(&dump, "Carol").unsessioned_entries.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![38]);
    assert!(user(&dump, "Bob").unsessioned_entries.is_empty());
}

#[test]
fn empty_children_are_exported_as_empty_rather_than_dropped() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);
    assert!(session(alice, 24).entries.is_empty(), "a session nobody logged into is still a session");
    let bare = session(alice, 23).entries.iter().find(|entry| entry.id == 35).unwrap();
    assert!(bare.sets.is_empty(), "an entry must not vanish along with its missing sets");
}

/// The invariant that catches a table the reader never walked. Named assertions cannot: they only
/// check the collections someone remembered to name, and forgetting one is the whole risk.
#[test]
fn row_counts_match_the_source_database_for_every_collection() {
    let conn = seeded_v1_db();
    let dump = export(&conn).unwrap();

    let exported: BTreeMap<&str, usize> = dump.row_counts().iter().collect();
    assert_eq!(exported, source_row_counts(&conn), "the dump's row counts must reconcile against the source, collection by collection");
}

/// The invariant is only as good as the seed behind it: a collection nobody seeded reconciles at
/// 0 = 0 and proves nothing at all.
#[test]
fn the_fixture_seeds_every_collection_the_exporter_reports() {
    let counts = export(&seeded_v1_db()).unwrap().row_counts();
    let unseeded = counts.iter().filter(|(_, count)| *count == 0).map(|(name, _)| name).collect::<Vec<_>>();
    assert_eq!(unseeded, vec!["session_reviews"], "every collection but the v2-only one must carry seeded rows");
}

#[test]
fn row_counts_total_counts_each_row_once() {
    let counts = export(&seeded_v1_db()).unwrap().row_counts();
    let summed: usize = counts.iter().map(|(_, count)| count).sum();
    assert_eq!(
        counts.total(),
        summed - counts.get("unsessioned_entries"),
        "unsessioned entries are already inside exercise_entries and must not be counted twice"
    );
}

#[test]
fn dropped_v2_data_is_archived_under_legacy() {
    let dump = export(&seeded_v1_db()).unwrap();
    let legacy = &alice(&dump).legacy;

    assert_eq!(legacy.signal_id.as_deref(), Some("signal-alice"), "v2 drops users.signal_id, so it is archived");
    assert_eq!(legacy.schedules.len(), 2);
    assert_eq!(legacy.schedules[0].cron_expr, "0 17 * * 1");
    assert_eq!(legacy.schedules[0].reminder_type, "voice");
    assert!(legacy.schedules[0].enabled);
    assert_eq!(legacy.schedules[0].exercises[0].exercise.name, "Flat Barbell Bench Press");

    assert_eq!(legacy.schedules[1].reminder_type, "text");
    assert!(!legacy.schedules[1].enabled, "a disabled schedule is archived too — this is a backup, not a migration");
    assert!(legacy.schedules[1].exercises.is_empty());
}

#[test]
fn timestamps_are_copied_verbatim() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);
    assert_eq!(alice.created_at, "2026-01-01 09:00:00", "a backup that reformats its timestamps is not a backup");
    assert_eq!(alice.sessions[0].started_at, "2026-02-01 17:00:00");
    assert_eq!(alice.body_metrics[0].measured_at, "2026-01-10 07:00:00");
}

#[test]
fn free_text_survives_quoting_punctuation_and_non_ascii() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);

    assert_eq!(session(alice, 25).intent.as_deref(), Some(r"unicode check: 100kg — pesado, ~5 reps <ok> \ done"));
    let philosophy = alice.philosophies.iter().find(|philosophy| philosophy.id == 8).unwrap();
    assert_eq!(philosophy.content, r#"It's the last set that counts — "leave one in the tank" is for cowards."#);

    // And it survives the JSON trip, which is where a hand-rolled escape would break.
    assert_eq!(from_json(&to_json(&dump).unwrap()).unwrap(), dump);
}

#[test]
fn per_user_scoping_keeps_one_users_data_out_of_anothers_tree() {
    let dump = export(&seeded_v1_db()).unwrap();

    let bob = user(&dump, "Bob");
    assert!(bob.sessions.is_empty());
    assert!(bob.goals.is_empty());
    assert!(bob.session_rosters.is_empty());
    assert!(bob.programmes.is_empty());
    assert!(bob.legacy.schedules.is_empty());
    assert_eq!(bob.legacy.signal_id, None);

    // Carol is populated, so an unscoped query shows up as Alice's rows landing in Carol's tree
    // rather than as an empty tree that happens to look correct.
    let carol = user(&dump, "Carol");
    assert_eq!(carol.sessions.iter().map(|session| session.id).collect::<Vec<_>>(), vec![26]);
    assert_eq!(carol.goals.iter().map(|goal| goal.id).collect::<Vec<_>>(), vec![16]);
    assert_eq!(carol.programmes.iter().map(|programme| programme.title.as_str()).collect::<Vec<_>>(), vec!["Couch to Squat"]);
    assert_eq!(carol.group_memberships.iter().map(|membership| membership.level.as_str()).collect::<Vec<_>>(), vec!["write"]);
    assert!(carol.legacy.schedules.is_empty(), "Alice's schedules must not reach Carol");
}

#[test]
fn conversation_history_and_health_entries_survive_intact() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);

    assert_eq!(alice.conversation_history.len(), 4);
    assert_eq!(alice.conversation_history[0].platform, "confide");
    assert!(alice.conversation_history[1].exclude_from_context);

    assert_eq!(alice.health_entries.len(), 3);
    assert_eq!(alice.health_entries[0].severity, "severe");
    assert_eq!(alice.health_entries[0].body_part.as_deref(), Some("shoulder"));
    assert_eq!(alice.health_entries[1].body_part, None);
}

#[test]
fn every_platform_role_severity_and_entry_type_reaches_the_dump() {
    let dump = export(&seeded_v1_db()).unwrap();
    // v2 drops the platform CHECK, so these pass through as text — including `confide`, which
    // migration 05 added and an exporter written against migration 01 alone would not expect.
    let messages = || dump.users.iter().flat_map(|user| &user.conversation_history);
    assert_eq!(distinct(messages().map(|message| message.platform.as_str())), distinct(["telegram", "signal", "web", "confide"]));
    assert_eq!(distinct(messages().map(|message| message.role.as_str())), distinct(["user", "assistant", "system"]));

    let health = || dump.users.iter().flat_map(|user| &user.health_entries);
    assert_eq!(distinct(health().map(|entry| entry.entry_type.as_str())), distinct(["injury", "illness", "wellbeing"]));
    assert_eq!(distinct(health().map(|entry| entry.severity.as_str())), distinct(["mild", "moderate", "severe"]));

    let philosophies = dump.users.iter().flat_map(|user| &user.philosophies);
    assert_eq!(distinct(philosophies.map(|philosophy| philosophy.source.as_str())), distinct(["interview", "note", "import"]));

    let memberships = dump.users.iter().flat_map(|user| &user.group_memberships);
    assert_eq!(distinct(memberships.map(|membership| membership.level.as_str())), distinct(["read", "write", "admin"]));
}

#[test]
fn body_metrics_keep_their_metric_names_and_series() {
    let dump = export(&seeded_v1_db()).unwrap();
    let metrics = &alice(&dump).body_metrics;
    assert_eq!(metrics.len(), 5);
    assert_eq!(
        distinct(metrics.iter().map(|metric| metric.metric.as_str())),
        distinct(["bodyweight_kg", "body_fat_pct", "waist_cm", "resting_hr_bpm"]),
        "the long shape means several metrics per user, not a column each"
    );

    let series = metrics.iter().filter(|metric| metric.metric == "bodyweight_kg").collect::<Vec<_>>();
    assert_eq!(series.len(), 2, "a metric's history is a series, and every point of it is data");
    assert_eq!(series[0].value, 82.5);

    // The same names goals use, which is what lets a metric-denominated goal join onto its series.
    alice(&dump).goals.iter().filter_map(|goal| goal.metric.as_deref()).for_each(|name| {
        assert!(metrics.iter().any(|metric| metric.metric == name), "goal metric `{name}` has no matching measurement series");
    });
}

// -----------------------------------------------------------------------------------------------
// JSON envelope.
// -----------------------------------------------------------------------------------------------

#[test]
fn json_round_trips_exactly() {
    let dump = export(&seeded_v1_db()).unwrap();
    let parsed = from_json(&to_json(&dump).unwrap()).unwrap();
    assert_eq!(parsed, dump, "serialising and parsing must be lossless");
}

#[test]
fn from_json_rejects_a_foreign_format() {
    let error = from_json(r#"{"format":"something-else","dump_version":1,"source_schema":{"generation":1,"user_version":13},"exported_at":"now","users":[]}"#)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a GymBuddy dump"), "unexpected error: {error}");
}

#[test]
fn from_json_rejects_an_unreadable_dump_version() {
    let json = format!(
        r#"{{"format":"{DUMP_FORMAT}","dump_version":99,"source_schema":{{"generation":1,"user_version":13}},"exported_at":"now","users":[]}}"#
    );
    let error = from_json(&json).unwrap_err().to_string();
    assert!(error.contains("unsupported dump version 99"), "unexpected error: {error}");
}

#[test]
fn export_refuses_a_database_that_is_not_gymbuddy() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE notes (id INTEGER PRIMARY KEY);").unwrap();
    assert!(export(&conn).unwrap_err().to_string().contains("not a GymBuddy database"));
}

#[test]
fn export_path_opens_read_only_and_leaves_a_legacy_database_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");
    fixtures::seeded_v1_db_at(&path);
    let before = std::fs::metadata(&path).unwrap().len();

    let dump = export_path(&path).unwrap();
    assert_eq!(dump.users.len(), 3);

    let after = Connection::open(&path).unwrap();
    let user_version: i64 = after.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(user_version, V1_USER_VERSION, "export must not migrate the database it reads");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before, "export must not write to its source");
}

// -----------------------------------------------------------------------------------------------
// Snapshot.
// -----------------------------------------------------------------------------------------------

/// The whole envelope, byte for byte against a committed file.
///
/// Every assertion above encodes an *intention*; this one encodes the **output**. A renamed field,
/// a reordered collection, a NULL that became `""` — all show up in the diff whether or not anyone
/// thought to assert about them. That makes the snapshot the review artefact for the schema v2
/// work: when the reader changes, the diff is the change.
///
/// Regenerate with `UPDATE_SNAPSHOT=1 cargo test -p gymbuddy-backend export_matches_the_committed`
/// and read the diff before committing it. A snapshot updated without being read tests nothing.
#[test]
fn export_matches_the_committed_snapshot() {
    let mut dump = export(&seeded_v1_db()).unwrap();
    // The one field that can never match: it is the wall clock at export time.
    dump.exported_at = "<exported_at>".to_string();
    let actual = to_json(&dump).unwrap() + "\n";

    if std::env::var_os("UPDATE_SNAPSHOT").is_some() {
        std::fs::write(SNAPSHOT_PATH, &actual).expect("writing the snapshot");
        return;
    }

    let expected = std::fs::read_to_string(SNAPSHOT_PATH).expect("reading the snapshot; regenerate it with UPDATE_SNAPSHOT=1");
    assert!(actual == expected, "{}", snapshot_mismatch(&expected, &actual));
}

const SNAPSHOT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v1_export.snapshot.json");

/// Point at the change rather than dumping two thousand lines of JSON into the test output.
fn snapshot_mismatch(expected: &str, actual: &str) -> String {
    let scratch = std::env::temp_dir().join("gymbuddy-v1_export.actual.json");
    std::fs::write(&scratch, actual).ok();
    let at = expected.lines().zip(actual.lines()).position(|(left, right)| left != right);
    let detail = match at {
        Some(line) => format!("first difference at line {}:\n  expected: {}\n  actual:   {}", line + 1, expected.lines().nth(line).unwrap_or(""), actual.lines().nth(line).unwrap_or("")),
        None => format!("identical for {} lines, then one ran out (expected {}, actual {})", expected.lines().count().min(actual.lines().count()), expected.lines().count(), actual.lines().count()),
    };
    format!(
        "export no longer matches tests/fixtures/v1_export.snapshot.json\n{detail}\nfull actual output written to {}\nif the change is intended, regenerate with UPDATE_SNAPSHOT=1",
        scratch.display()
    )
}

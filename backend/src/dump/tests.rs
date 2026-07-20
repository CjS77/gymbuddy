//! Export tests, driven by a real v1 database.
//!
//! The fixture is built by applying the actual v1 migration set and seeding rows that exercise
//! every translation in the module's mapping table — a hand-written `CREATE TABLE` approximation
//! would drift from the schema it claims to test.

use rusqlite::Connection;

use super::*;
use crate::db::migrations::MIGRATIONS;

/// A v1 database seeded with one row of everything the mapping table mentions.
///
/// Catalogue ids used here are the seeded ones: 10000 = Flat Barbell Bench Press (parent Bench
/// Press), 1000 = Bench Press (parent Pectoral), 5000 = Squat (parent Quadriceps), 6020 = Plank
/// (parent Transverse Abdominis, measurement type 2 = time_based).
fn seeded_v1_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    MIGRATIONS.to_latest(&mut conn).expect("v1 migrations failed");
    conn.execute_batch(SEED).expect("seeding v1 fixture failed");
    conn
}

const SEED: &str = r#"
INSERT INTO users (id, name, telegram_id, signal_id, timezone, created_at, updated_at, beta_tester, pubkey, timers_enabled)
VALUES (1, 'Alice', 'tg-alice', 'signal-alice', 'Europe/Lisbon', '2026-01-01 09:00:00', '2026-02-01 09:00:00', 1, 'pk-alice', 0),
       (2, 'Bob', 'tg-bob', NULL, 'UTC', '2026-01-02 09:00:00', '2026-01-02 09:00:00', 0, NULL, 1);

INSERT INTO groups (id, name, description, created_at) VALUES (1, 'beta', 'Beta testers', '2026-01-01 08:00:00');
INSERT INTO group_members (user_id, group_id, level, granted_at) VALUES (1, 1, 'admin', '2026-01-01 08:30:00');

INSERT INTO workout_philosophy (id, user_id, content, source, created_at)
VALUES (7, 1, 'Compound lifts first.', 'interview', '2026-01-03 10:00:00');

INSERT INTO interview_state (user_id, platform, mode, draft, turns, started_at)
VALUES (1, 'telegram', 'philosophy', 'half a philosophy', 2, '2026-01-04 10:00:00');

INSERT INTO goals (id, user_id, kind, exercise_type_id, metric, target_value, direction, priority,
                   start_date, target_date, achieved, notes, created_at, updated_at)
VALUES (11, 1, 'strength', 1000, NULL, 120.0, 'increase', 5, '2026-01-01', '2026-06-01', 0, 'bench 120', '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (12, 1, 'bodyweight', NULL, 'bodyweight_kg', 78.0, 'decrease', 3, '2026-01-01', NULL, 1, NULL, '2026-01-01 09:00:00', '2026-01-01 09:00:00');

-- Session 21 carries the `plan:` sentinel; session 22 carries plain notes.
INSERT INTO sessions (id, user_id, started_at, ended_at, notes, overall_effort, felt, cut_short, cut_short_reason)
VALUES (21, 1, '2026-02-01 17:00:00', '2026-02-01 18:15:00', 'plan:Push Day' || char(10) || 'felt strong', 'hard', 'great', 0, NULL),
       (22, 1, '2026-02-03 17:00:00', NULL, 'no sentinel here', NULL, NULL, 1, 'gym closed');

INSERT INTO exercise_entry (id, user_id, session_id, start_timestamp, end_timestamp, comment)
VALUES (31, 1, 21, '2026-02-01 17:05:00', '2026-02-01 17:30:00', 'warmed up'),
       (32, 1, 21, '2026-02-01 17:35:00', NULL, NULL),
       (33, 1, NULL, '2026-01-15 12:00:00', NULL, 'logged outside a session');

INSERT INTO sets (id, exercise_entry_id, exercise_type_id, order_idx, measurement_type_id, count, value, perceived_difficulty, comment, logged_at)
VALUES (41, 31, 10000, 0, 1, 8, 80.0, 'medium', 'smooth', '2026-02-01 17:10:00'),
       (42, 31, 10000, 1, 1, 6, 90.0, 'hard', NULL, '2026-02-01 17:20:00'),
       (43, 32, 6020, 0, 2, NULL, 60.0, 'easy', NULL, '2026-02-01 17:40:00'),
       (44, 33, 5000, 0, 1, 5, 100.0, 'medium', NULL, '2026-01-15 12:05:00');

INSERT INTO programs (id, user_id, title, start_date, target_end_date, days_per_week, split, progression_policy, status, created_at, updated_at)
VALUES (51, 1, 'Winter Strength', '2026-01-06', '2026-03-30', 4, 'upper/lower', 'double progression', 'active', '2026-01-05 09:00:00', '2026-01-05 09:00:00');

INSERT INTO program_goals (program_id, goal_id) VALUES (51, 11);
INSERT INTO program_blocks (id, program_id, start_week, end_week, focus, notes)
VALUES (61, 51, 1, 4, 'hypertrophy', 'build volume'), (62, 51, 5, 6, 'deload', NULL);
INSERT INTO program_slots (id, program_id, week_idx, day_idx, focus, status, updated_at)
VALUES (71, 51, 1, 1, 'push', 'filled', '2026-02-01 18:15:00'),
       (72, 51, 1, 2, 'pull', 'pending', '2026-01-05 09:00:00');

-- Plan 81 is still 'proposed' (→ draft); plan 82 ran as session 21 and filled slot 71.
INSERT INTO workout_plans (id, user_id, title, rationale, philosophy_id, status, session_id, created_at, updated_at, override_note, program_slot_id)
VALUES (81, 1, 'Next Pull', 'lats need volume', 7, 'proposed', NULL, '2026-02-04 08:00:00', '2026-02-04 08:00:00', NULL, NULL),
       (82, 1, 'Push Day', 'chest focus', 7, 'completed', 21, '2026-02-01 16:00:00', '2026-02-01 18:15:00', 'no dips today', 71);

INSERT INTO workout_plan_exercises (id, plan_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes)
VALUES (91, 82, 10000, 0, 3, 8, 80.0, NULL, 'controlled tempo'),
       (92, 82, 6020, 1, 3, NULL, NULL, 60, NULL);

INSERT INTO schedules (id, user_id, name, cron_expr, reminder_type, reminder_notice_mins, enabled, created_at, updated_at)
VALUES (101, 1, 'Push Day', '0 17 * * 1', 'voice', 45, 1, '2026-01-01 09:00:00', '2026-01-01 09:00:00');
INSERT INTO schedule_exercises (schedule_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg)
VALUES (101, 10000, 0, 3, 8, 80.0);

INSERT INTO health_entries (id, user_id, entry_type, body_part, severity, description, started_at, resolved_at, notes, updated_at)
VALUES (111, 1, 'injury', 'shoulder', 'moderate', 'tweaked left shoulder', '2026-01-20 09:00:00', NULL, 'avoid overhead', '2026-01-20 09:00:00');

INSERT INTO body_metrics (id, user_id, metric, value, measured_at)
VALUES (121, 1, 'bodyweight_kg', 82.5, '2026-01-10 07:00:00'),
       (122, 1, 'bodyweight_kg', 81.9, '2026-02-10 07:00:00');

INSERT INTO conversation_history (id, user_id, platform, role, content, timestamp, exclude_from_context)
VALUES (131, 1, 'confide', 'user', 'logged 8 at 80', '2026-02-01 17:10:00', 0),
       (132, 1, 'telegram', 'assistant', 'nice work', '2026-02-01 17:10:05', 1);
"#;

fn alice(dump: &Dump) -> &model::User {
    dump.users.iter().find(|user| user.name == "Alice").expect("Alice missing from dump")
}

#[test]
fn envelope_carries_format_version_and_provenance() {
    let dump = export(&seeded_v1_db()).unwrap();
    assert_eq!(dump.format, DUMP_FORMAT);
    assert_eq!(dump.dump_version, DUMP_VERSION);
    assert_eq!(dump.source_schema.generation, 1);
    assert_eq!(dump.source_schema.user_version, 13);
    assert!(dump.exported_at.contains('T'), "exported_at should be RFC 3339, got {}", dump.exported_at);
}

#[test]
fn every_user_gets_a_tree_and_groups_stay_global() {
    let dump = export(&seeded_v1_db()).unwrap();
    assert_eq!(dump.users.len(), 2);
    assert_eq!(dump.groups.len(), 1);
    assert_eq!(dump.groups[0].name, "beta");

    let alice = alice(&dump);
    assert_eq!(alice.group_memberships.len(), 1);
    assert_eq!(alice.group_memberships[0].group, "beta", "membership references the group by name, not id");
    assert_eq!(alice.group_memberships[0].level, "admin");

    let bob = dump.users.iter().find(|user| user.name == "Bob").unwrap();
    assert!(bob.sessions.is_empty(), "a user with no data still gets a tree");
}

// -----------------------------------------------------------------------------------------------
// Vocabulary: the v1 reader must emit v2 names and values, never legacy ones.
// -----------------------------------------------------------------------------------------------

#[test]
fn workout_plans_export_as_session_rosters_with_proposed_renamed_to_draft() {
    let dump = export(&seeded_v1_db()).unwrap();
    let rosters = &alice(&dump).session_rosters;
    assert_eq!(rosters.len(), 2);

    let draft = rosters.iter().find(|roster| roster.title == "Next Pull").unwrap();
    assert_eq!(draft.status, "draft", "v1 'proposed' must land as v2 'draft'");
    assert_eq!(draft.philosophy_id, Some(7));

    let completed = rosters.iter().find(|roster| roster.title == "Push Day").unwrap();
    assert_eq!(completed.status, "completed", "statuses outside the rename pass through");
    assert_eq!(completed.session_id, Some(21), "the roster still points at the session it ran as");
    assert_eq!(completed.programme_slot_id, Some(71), "program_slot_id is renamed, and the reference survives");
    assert_eq!(completed.override_note.as_deref(), Some("no dips today"));
}

#[test]
fn workout_plan_exercises_export_as_roster_exercises_in_order() {
    let dump = export(&seeded_v1_db()).unwrap();
    let roster = alice(&dump).session_rosters.iter().find(|roster| roster.title == "Push Day").unwrap();
    assert_eq!(roster.exercises.len(), 2);

    let first = &roster.exercises[0];
    assert_eq!(first.exercise.name, "Flat Barbell Bench Press");
    assert_eq!(first.target_sets, Some(3));
    assert_eq!(first.target_weight_kg, Some(80.0));
    assert_eq!(first.notes.as_deref(), Some("controlled tempo"));

    assert_eq!(roster.exercises[1].exercise.name, "Plank");
    assert_eq!(roster.exercises[1].target_secs, Some(60));
}

#[test]
fn programs_export_as_programmes_with_blocks_slots_and_goal_links() {
    let dump = export(&seeded_v1_db()).unwrap();
    let programmes = &alice(&dump).programmes;
    assert_eq!(programmes.len(), 1);

    let programme = &programmes[0];
    assert_eq!(programme.title, "Winter Strength");
    assert_eq!(programme.days_per_week, 4);
    assert_eq!(programme.goal_ids, vec![11], "program_goals becomes goal_ids into this user's goals");
    assert_eq!(programme.blocks.len(), 2);
    assert_eq!(programme.blocks[0].focus, "hypertrophy");
    assert_eq!(programme.slots.len(), 2);
    assert_eq!(programme.slots[0].id, 71);
    assert_eq!(programme.slots[0].status, "filled");
}

#[test]
fn session_notes_become_intent_with_the_plan_sentinel_stripped_into_legacy() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);

    let sentinel_session = alice.sessions.iter().find(|session| session.id == 21).unwrap();
    assert_eq!(sentinel_session.intent.as_deref(), Some("felt strong"), "the `plan:` prefix must not survive into intent");

    let plain_session = alice.sessions.iter().find(|session| session.id == 22).unwrap();
    assert_eq!(plain_session.intent.as_deref(), Some("no sentinel here"), "notes without a sentinel pass through verbatim");
    assert!(plain_session.cut_short);
    assert_eq!(plain_session.cut_short_reason.as_deref(), Some("gym closed"));

    assert_eq!(alice.legacy.session_plan_names.len(), 1, "the stripped schedule name is archived, not discarded");
    assert_eq!(alice.legacy.session_plan_names[0].session_id, 21);
    assert_eq!(alice.legacy.session_plan_names[0].plan, "Push Day");
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
    let alice = alice(&dump);
    let entry = alice.sessions.iter().find(|s| s.id == 21).unwrap().entries.iter().find(|e| e.id == 31).unwrap();

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
    let plank = alice(&dump).sessions.iter().find(|s| s.id == 21).unwrap().entries.iter().find(|e| e.id == 32).unwrap();
    assert_eq!(plank.sets[0].measurement_type, "time_based");
    assert_eq!(plank.sets[0].count, None, "the (count, value) polymorphism is preserved verbatim");
    assert_eq!(plank.sets[0].value, 60.0);
}

#[test]
fn goals_reference_their_exercise_by_name_and_keep_metric_names_as_text() {
    let dump = export(&seeded_v1_db()).unwrap();
    let goals = &alice(&dump).goals;

    let strength = goals.iter().find(|goal| goal.id == 11).unwrap();
    assert_eq!(strength.exercise.as_ref().unwrap().name, "Bench Press");
    assert_eq!(strength.exercise.as_ref().unwrap().parent.as_deref(), Some("Pectoral"));
    assert_eq!(strength.metric, None);

    let bodyweight = goals.iter().find(|goal| goal.id == 12).unwrap();
    assert_eq!(bodyweight.exercise, None);
    assert_eq!(bodyweight.metric.as_deref(), Some("bodyweight_kg"), "v2 resolves this name to a metrics row on import");
    assert!(bodyweight.achieved);
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
    assert_eq!(alice.unsessioned_entries.len(), 1, "a NULL session_id entry has no session to nest under");
    assert_eq!(alice.unsessioned_entries[0].id, 33);
    assert_eq!(alice.unsessioned_entries[0].sets[0].exercise.name, "Squat");
}

#[test]
fn set_and_entry_counts_match_the_source_database() {
    let conn = seeded_v1_db();
    let dump = export(&conn).unwrap();

    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
    let exported_entries: usize =
        dump.users.iter().map(|user| user.sessions.iter().map(|s| s.entries.len()).sum::<usize>() + user.unsessioned_entries.len()).sum();
    let exported_sets: usize = dump
        .users
        .iter()
        .map(|user| {
            let in_sessions: usize = user.sessions.iter().flat_map(|s| &s.entries).map(|entry| entry.sets.len()).sum();
            in_sessions + user.unsessioned_entries.iter().map(|entry| entry.sets.len()).sum::<usize>()
        })
        .sum();

    assert_eq!(exported_entries as i64, count("SELECT COUNT(*) FROM exercise_entry"));
    assert_eq!(exported_sets as i64, count("SELECT COUNT(*) FROM sets"));
    assert_eq!(dump.users.len() as i64, count("SELECT COUNT(*) FROM users"));
    assert_eq!(
        dump.users.iter().map(|user| user.session_rosters.len()).sum::<usize>() as i64,
        count("SELECT COUNT(*) FROM workout_plans")
    );
    assert_eq!(dump.users.iter().map(|user| user.body_metrics.len()).sum::<usize>() as i64, count("SELECT COUNT(*) FROM body_metrics"));
}

#[test]
fn dropped_v2_data_is_archived_under_legacy() {
    let dump = export(&seeded_v1_db()).unwrap();
    let legacy = &alice(&dump).legacy;

    assert_eq!(legacy.signal_id.as_deref(), Some("signal-alice"), "v2 drops users.signal_id, so it is archived");
    assert_eq!(legacy.schedules.len(), 1);
    assert_eq!(legacy.schedules[0].cron_expr, "0 17 * * 1");
    assert_eq!(legacy.schedules[0].reminder_type, "voice");
    assert!(legacy.schedules[0].enabled);
    assert_eq!(legacy.schedules[0].exercises[0].exercise.name, "Flat Barbell Bench Press");
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
fn per_user_scoping_keeps_one_users_data_out_of_anothers_tree() {
    let dump = export(&seeded_v1_db()).unwrap();
    let bob = dump.users.iter().find(|user| user.name == "Bob").unwrap();
    assert!(bob.sessions.is_empty());
    assert!(bob.goals.is_empty());
    assert!(bob.session_rosters.is_empty());
    assert!(bob.programmes.is_empty());
    assert!(bob.legacy.schedules.is_empty());
    assert_eq!(bob.legacy.signal_id, None);
}

#[test]
fn conversation_history_and_health_entries_survive_intact() {
    let dump = export(&seeded_v1_db()).unwrap();
    let alice = alice(&dump);

    assert_eq!(alice.conversation_history.len(), 2);
    assert_eq!(alice.conversation_history[0].platform, "confide");
    assert!(alice.conversation_history[1].exclude_from_context);

    assert_eq!(alice.health_entries.len(), 1);
    assert_eq!(alice.health_entries[0].severity, "moderate");
    assert_eq!(alice.health_entries[0].body_part.as_deref(), Some("shoulder"));
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
    {
        let mut conn = Connection::open(&path).unwrap();
        MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.execute_batch(SEED).unwrap();
    }
    let before = std::fs::metadata(&path).unwrap().len();

    let dump = export_path(&path).unwrap();
    assert_eq!(dump.users.len(), 2);

    let after = Connection::open(&path).unwrap();
    let user_version: i64 = after.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(user_version, 13, "export must not migrate the database it reads");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before, "export must not write to its source");
}

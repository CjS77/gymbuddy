mod helpers;

use gymbuddy_backend::db::*;
use helpers::build_fixture;

#[test]
fn fixture_loads_completely() {
    let f = build_fixture();
    let db = &f.db;

    assert_eq!(db.list_users().unwrap().len(), 2);

    let session_count = [f.alice_id, f.bob_id].iter().map(|id| db.count_sessions_for_user(*id).unwrap()).sum::<i64>();
    assert!(session_count >= 30, "expected ≥30 sessions across both users, got {session_count}");

    let set_count: usize = [f.alice_id, f.bob_id].iter().map(|id| db.list_sets_for_user(*id, None, None).unwrap().len()).sum();
    assert!(set_count >= 100);

    // A window wide enough to catch every seeded goal, achieved ones included —
    // `list_active_goals` would hide Alice's completed bench goal.
    let goal_count: usize =
        [f.alice_id, f.bob_id].iter().map(|id| db.list_goals_in_period(*id, "2000-01-01", "2100-01-01").unwrap().len()).sum();
    assert_eq!(goal_count, 3);
}

#[test]
fn invariant_closed_sessions_have_no_open_entries() {
    let f = build_fixture();
    let leaks: usize = [f.alice_id, f.bob_id]
        .iter()
        .flat_map(|id| f.db.list_sessions(*id, None, None).unwrap())
        .filter(|session| session.ended_at.is_some())
        .map(|session| f.db.list_open_entries_for_session(session.id).unwrap().len())
        .sum();
    assert_eq!(leaks, 0, "ended sessions must have no open exercise_entry");
}

#[test]
fn exercise_time_series_shows_progression() {
    let f = build_fixture();
    let points = f.db.exercise_time_series(f.alice_id, f.bench_press_id, Some("2025-01-01"), Some("2026-06-30"), false).unwrap();
    assert!(points.len() >= 10, "expected ≥10 bench data points, got {}", points.len());
    let first = points.first().unwrap().value;
    let last = points.last().unwrap().value;
    assert!(first < last, "expected progression: first {first} < last {last}");
}

#[test]
fn exercise_time_series_time_based() {
    let f = build_fixture();
    let points = f.db.exercise_time_series(f.alice_id, f.plank_id, Some("2025-01-01"), Some("2026-06-30"), false).unwrap();
    assert!(points.len() >= 20, "got {}", points.len());
}

#[test]
fn exercise_time_series_distance_based() {
    let f = build_fixture();
    let points = f.db.exercise_time_series(f.alice_id, f.running_id, Some("2025-01-01"), Some("2026-06-30"), false).unwrap();
    assert!(points.len() >= 20, "got {}", points.len());
}

#[test]
fn muscle_group_time_series_returns_multiple_exercises() {
    let f = build_fixture();
    let series = f.db.muscle_group_time_series(f.alice_id, "Chest", Some("2025-01-01"), Some("2026-06-30")).unwrap();
    assert!(!series.is_empty(), "expected ≥1 chest series");
}

#[test]
fn goal_progress_shows_achieved_and_failed() {
    let f = build_fixture();
    let report_alice = f.db.goal_progress_report(f.alice_id, Some("2024-01-01"), Some("2026-12-31")).unwrap();
    let report_bob = f.db.goal_progress_report(f.bob_id, Some("2024-01-01"), Some("2026-12-31")).unwrap();

    let all: Vec<_> = report_alice.iter().chain(report_bob.iter()).collect();
    assert!(!all.is_empty());
    assert!(all.iter().any(|r| r.status == GoalStatus::Achieved));
    assert!(all.iter().any(|r| r.status == GoalStatus::Failed));
}

#[test]
fn goal_progress_percentages_are_reasonable() {
    let f = build_fixture();
    let report = f.db.goal_progress_report(f.alice_id, Some("2025-01-01"), Some("2026-12-31")).unwrap();
    for gp in &report {
        match gp.status {
            GoalStatus::Achieved => {
                assert!(gp.percentage >= 100.0 || gp.goal.achieved);
            }
            GoalStatus::Failed => {
                assert!(!gp.goal.achieved);
            }
            GoalStatus::Active => {}
        }
    }
}

#[test]
fn injury_gap_visible_in_sessions() {
    let f = build_fixture();
    let injury_sessions = f.db.list_sessions(f.alice_id, Some("2025-07-01"), Some("2025-07-14")).unwrap();
    let normal_sessions = f.db.list_sessions(f.alice_id, Some("2025-02-01"), Some("2025-02-14")).unwrap();
    assert!(injury_sessions.len() < normal_sessions.len());
}

#[test]
fn health_entries_have_resolved_dates() {
    let f = build_fixture();
    let history = f.db.list_health_history(f.alice_id, 10).unwrap();
    let resolved_count = history.iter().filter(|h| h.resolved_at.is_some()).count();
    assert!(resolved_count > 0);
}

#[test]
fn access_control_across_seeded_groups() {
    let f = build_fixture();
    assert!(f.db.can_read(f.alice_id, f.bob_id).unwrap());
    assert!(f.db.can_read(f.bob_id, f.alice_id).unwrap());
    assert!(f.db.can_write(f.alice_id, f.bob_id).unwrap());
    assert!(f.db.can_write(f.bob_id, f.alice_id).unwrap());
    assert!(f.db.can_admin_group(f.alice_id, f.group_id).unwrap());
    assert!(!f.db.can_admin_group(f.bob_id, f.group_id).unwrap());
}

#[test]
fn conversation_history_present() {
    let f = build_fixture();
    assert!(f.db.count_messages_for_user(f.alice_id).unwrap() >= 20);
}

#[test]
fn personal_records() {
    let f = build_fixture();
    let pr = f.db.personal_record(f.alice_id, f.bench_press_id, false).unwrap();
    assert!(pr.is_some(), "expected a bench press PR for alice");
    let pr = pr.unwrap();
    assert!(pr.value >= 90.0, "bench PR should be ≥90 kg, got {}", pr.value);
}

#[test]
fn personal_records_with_descendants_rolls_up() {
    let f = build_fixture();
    // Roll up from Chest (muscle_group level) — should pick up Bench Press sets.
    let chest = f.db.get_exercise_type_by_name("Chest").unwrap().unwrap();
    let pr = f.db.personal_record(f.alice_id, chest.id, true).unwrap();
    assert!(pr.is_some(), "rollup should find a chest PR");
}

#[test]
fn concurrent_read_write_wal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    let db1 = Database::open(&db_path).unwrap();
    let db2 = Database::open(&db_path).unwrap();

    // Both DBs share the same migration-seeded taxonomy.
    let exercises = db2.list_exercise_types(None).unwrap();
    assert!(!exercises.is_empty(), "db2 should see migration-seeded exercise_types");

    let user_id = db1.insert_user(&new_user("WAL Test", None, "UTC")).unwrap();
    let fetched = db2.get_user(user_id).unwrap();
    assert!(fetched.is_some(), "db2 should see user written by db1 via WAL");
}

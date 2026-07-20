mod helpers;

use gymbuddy_backend::db::*;
use helpers::build_fixture;

#[test]
fn fixture_loads_completely() {
    let f = build_fixture();
    let db = &f.db;

    let user_count: i64 = db.conn().query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap();
    assert_eq!(user_count, 2);

    let session_count: i64 = db.conn().query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)).unwrap();
    assert!(session_count >= 30, "expected ≥30 sessions across both users, got {session_count}");

    let set_count: i64 = db.conn().query_row("SELECT COUNT(*) FROM sets", [], |r| r.get(0)).unwrap();
    assert!(set_count >= 100);

    let goal_count: i64 = db.conn().query_row("SELECT COUNT(*) FROM goals", [], |r| r.get(0)).unwrap();
    assert_eq!(goal_count, 3);
}

#[test]
fn invariant_closed_sessions_have_no_open_entries() {
    let f = build_fixture();
    let leaks: i64 =
        f.db.conn()
            .query_row(
                "SELECT COUNT(*) FROM exercise_entries ee \
             JOIN sessions s ON ee.session_id = s.id \
             WHERE s.ended_at IS NOT NULL AND ee.end_timestamp IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
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
    let msg_count: i64 =
        f.db.conn()
            .query_row("SELECT COUNT(*) FROM conversation_history WHERE user_id = ?1", rusqlite::params![f.alice_id], |r| r.get(0))
            .unwrap();
    assert!(msg_count >= 20);
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

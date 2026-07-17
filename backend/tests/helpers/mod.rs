//! Programmatic test fixture for corre-gym integration tests.
//!
//! Builds a small but realistic dataset on top of the migration-seeded
//! exercise_types taxonomy: two users, three groups, sessions/entries/sets
//! over a six-month period, a few goals (achieved + failed), and health entries.

use chrono::{Duration, NaiveDate};
use gymbuddy_backend::db::*;

#[allow(dead_code)]
pub struct Fixture {
    pub db: Database,
    pub alice_id: i64,
    pub bob_id: i64,
    pub group_id: i64,
    pub bench_press_id: i64,
    pub deadlift_id: i64,
    pub plank_id: i64,
    pub running_id: i64,
}

/// Build the fixture. Returns an in-memory DB pre-populated with everything the
/// integration tests need.
#[allow(dead_code)]
pub fn build_fixture() -> Fixture {
    let db = Database::open_in_memory().unwrap();
    let alice_id = db.insert_user(&new_user("Alice", Some("alice_tg"), "Europe/London")).unwrap();
    let bob_id = db.insert_user(&new_user("Bob", Some("bob_tg"), "America/New_York")).unwrap();

    let group = Group { id: 0, name: "Training Buddies".into(), description: None, created_at: String::new() };
    let group_id = db.insert_group(&group).unwrap();
    db.add_member(alice_id, group_id, AccessLevel::Admin).unwrap();
    db.add_member(bob_id, group_id, AccessLevel::Write).unwrap();

    let bench_press_id = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap().id;
    let deadlift_id = db.get_exercise_type_by_name("Deadlift").unwrap().unwrap().id;
    let plank_id = db.get_exercise_type_by_name("Plank").unwrap().unwrap().id;
    let running_id = db.get_exercise_type_by_name("Running").unwrap().unwrap().id;

    seed_alice_history(&db, alice_id, bench_press_id, deadlift_id, plank_id, running_id);
    seed_bob_history(&db, bob_id, bench_press_id);
    seed_goals(&db, alice_id, bob_id, bench_press_id, deadlift_id);
    seed_health(&db, alice_id);

    Fixture { db, alice_id, bob_id, group_id, bench_press_id, deadlift_id, plank_id, running_id }
}

fn seed_alice_history(db: &Database, user_id: i64, bench: i64, deadlift: i64, plank: i64, running: i64) {
    // 26 weekly sessions starting 2025-01-06 (Mon) — gap during 2025-07-01..2025-07-14 (injury).
    let start = NaiveDate::from_ymd_opt(2025, 1, 6).unwrap();
    for week in 0..26 {
        let date = start + Duration::days((week * 7) as i64);

        if date >= NaiveDate::from_ymd_opt(2025, 7, 1).unwrap() && date <= NaiveDate::from_ymd_opt(2025, 7, 14).unwrap() {
            continue; // injury gap
        }

        let started = format!("{date} 09:00:00");
        let ended = format!("{date} 10:30:00");
        db.conn()
            .execute(
                "INSERT INTO sessions (user_id, started_at, ended_at, notes) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![user_id, started, ended, format!("Week {}", week + 1)],
            )
            .unwrap();
        let session_id = db.conn().last_insert_rowid();

        let mut bench_entry = new_exercise_entry(user_id, Some(session_id), None);
        bench_entry.start_timestamp = format!("{date} 09:05:00");
        bench_entry.end_timestamp = Some(ended.clone());
        let bench_entry_id = db.insert_entry(&bench_entry).unwrap();

        let progressing_weight = 70.0 + (week as f64) * 1.0;
        for set_idx in 0..3 {
            let mut s = new_exercise_set(bench_entry_id, bench, MeasurementType::WeightReps, progressing_weight);
            s.count = Some(8);
            s.order_idx = set_idx;
            s.logged_at = format!("{date} 09:05:00");
            db.insert_set(&s).unwrap();
        }

        if week % 2 == 0 {
            let mut dl_entry = new_exercise_entry(user_id, Some(session_id), None);
            dl_entry.start_timestamp = format!("{date} 09:30:00");
            dl_entry.end_timestamp = Some(ended.clone());
            let dl_entry_id = db.insert_entry(&dl_entry).unwrap();
            let dl_weight = 100.0 + (week as f64) * 1.5;
            for set_idx in 0..3 {
                let mut s = new_exercise_set(dl_entry_id, deadlift, MeasurementType::WeightReps, dl_weight);
                s.count = Some(5);
                s.order_idx = set_idx;
                s.logged_at = format!("{date} 09:30:00");
                db.insert_set(&s).unwrap();
            }
        }

        // Plank (time-based)
        let mut plank_entry = new_exercise_entry(user_id, Some(session_id), None);
        plank_entry.start_timestamp = format!("{date} 09:55:00");
        plank_entry.end_timestamp = Some(ended.clone());
        let plank_entry_id = db.insert_entry(&plank_entry).unwrap();
        let mut plank_set = new_exercise_set(plank_entry_id, plank, MeasurementType::TimeBased, 60.0 + week as f64);
        plank_set.logged_at = format!("{date} 09:55:00");
        db.insert_set(&plank_set).unwrap();

        // Running (distance-based)
        let mut run_entry = new_exercise_entry(user_id, Some(session_id), None);
        run_entry.start_timestamp = format!("{date} 10:05:00");
        run_entry.end_timestamp = Some(ended.clone());
        let run_entry_id = db.insert_entry(&run_entry).unwrap();
        let mut run_set = new_exercise_set(run_entry_id, running, MeasurementType::DistanceBased, 3000.0 + (week as f64) * 100.0);
        run_set.logged_at = format!("{date} 10:05:00");
        db.insert_set(&run_set).unwrap();

        let prompt = new_conversation_message(user_id, "telegram", ConversationRole::User, &format!("Logged my session on {date}"));
        db.insert_message(&prompt).unwrap();
        let reply = new_conversation_message(user_id, "telegram", ConversationRole::Assistant, "Nice work! Keep it up.");
        db.insert_message(&reply).unwrap();
    }
}

fn seed_bob_history(db: &Database, user_id: i64, bench: i64) {
    for day in 0..10 {
        let date = NaiveDate::from_ymd_opt(2025, 3, 1).unwrap() + Duration::days(day * 3);
        let ended = format!("{date} 19:00:00");
        db.conn()
            .execute(
                "INSERT INTO sessions (user_id, started_at, ended_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![user_id, format!("{date} 18:00:00"), ended],
            )
            .unwrap();
        let session_id = db.conn().last_insert_rowid();
        let mut entry = new_exercise_entry(user_id, Some(session_id), None);
        entry.end_timestamp = Some(ended.clone());
        let entry_id = db.insert_entry(&entry).unwrap();
        for _ in 0..3 {
            let mut s = new_exercise_set(entry_id, bench, MeasurementType::WeightReps, 60.0);
            s.count = Some(10);
            s.logged_at = format!("{date} 18:05:00");
            db.insert_set(&s).unwrap();
        }
    }
}

fn seed_goals(db: &Database, alice: i64, bob: i64, bench: i64, deadlift: i64) {
    // Alice: achieved bench goal (90 kg)
    let mut achieved = new_exercise_goal(alice, bench, 90.0);
    achieved.start_date = "2025-01-01".into();
    achieved.target_date = Some("2025-12-31".into());
    achieved.achieved = true;
    db.insert_goal(&achieved).unwrap();

    // Alice: failed deadlift goal (target way too high, target_date in past)
    let mut failed = new_exercise_goal(alice, deadlift, 500.0);
    failed.start_date = "2024-01-01".into();
    failed.target_date = Some("2024-06-01".into());
    db.insert_goal(&failed).unwrap();

    // Bob: active goal
    let mut active = new_exercise_goal(bob, bench, 100.0);
    active.start_date = "2025-01-01".into();
    active.target_date = Some("2026-12-31".into());
    db.insert_goal(&active).unwrap();
}

fn seed_health(db: &Database, user_id: i64) {
    // Resolved injury (the "gap" in Alice's training)
    let mut injury = new_health_entry(user_id, HealthEntryType::Injury, "Lower back strain");
    injury.body_part = Some("lower back".into());
    injury.severity = "moderate".into();
    injury.started_at = "2025-07-01".into();
    injury.resolved_at = Some("2025-07-14".into());
    db.insert_health_entry(&injury).unwrap();

    // Active wellbeing entry
    let mut active = new_health_entry(user_id, HealthEntryType::Wellbeing, "Sleeping well");
    active.severity = "mild".into();
    active.started_at = "2025-08-01".into();
    db.insert_health_entry(&active).unwrap();
}

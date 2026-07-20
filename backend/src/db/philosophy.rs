//! The user's training philosophy and the interview that builds it.
//!
//! `philosophies` is append-only: the latest row *is* the philosophy, and every
//! earlier one stays as history. `interview_states` holds the in-progress
//! interview for a `(user, platform)` pair — presence means one is running.
//!
//! Split out of the old `planner.rs`, which mixed this with roster persistence.
//! The two share nothing but a foreign key: a roster records a philosophy id so a
//! later reader knows which philosophy it was designed under.

use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{InterviewState, WorkoutPhilosophy};

fn row_to_philosophy(row: &rusqlite::Row) -> rusqlite::Result<WorkoutPhilosophy> {
    Ok(WorkoutPhilosophy {
        id: row.get(0)?,
        user_id: row.get(1)?,
        content: row.get(2)?,
        source: row.get(3)?,
        created_at: row.get(4)?,
    })
}

fn row_to_interview_state(row: &rusqlite::Row) -> rusqlite::Result<InterviewState> {
    Ok(InterviewState {
        user_id: row.get(0)?,
        platform: row.get(1)?,
        mode: row.get(2)?,
        draft: row.get(3)?,
        turns: row.get(4)?,
        started_at: row.get(5)?,
    })
}

const SELECT_PHILOSOPHY: &str = "SELECT id, user_id, content, source, created_at FROM philosophies";

impl Database {
    // ── Philosophy ──────────────────────────────────────────────────────────────

    /// Append a philosophy entry (the table is append-only). Returns its row id.
    pub fn insert_philosophy(&self, user_id: i64, content: &str, source: &str) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO philosophies (user_id, content, source) VALUES (?1, ?2, ?3)",
            params![user_id, content, source],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// The user's current philosophy: the most recently inserted entry.
    pub fn latest_philosophy(&self, user_id: i64) -> anyhow::Result<Option<WorkoutPhilosophy>> {
        let sql = format!("{SELECT_PHILOSOPHY} WHERE user_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![user_id], row_to_philosophy)?;
        rows.next().transpose().context("Failed to read philosophy row")
    }

    /// Append a durable preference captured mid-workout: read the latest content,
    /// append `"- {note}"` as a new bullet, and insert it as a fresh `note` entry
    /// so the table stays append-only and `latest_philosophy` keeps the full text.
    pub fn append_philosophy_note(&self, user_id: i64, note: &str) -> anyhow::Result<i64> {
        let base = self.latest_philosophy(user_id)?.map(|p| p.content).unwrap_or_default();
        let content = if base.trim().is_empty() { format!("- {note}") } else { format!("{base}\n- {note}") };
        self.insert_philosophy(user_id, &content, "note")
    }

    // ── Interview state ───────────────────────────────────────────────────────────

    pub fn get_interview_state(&self, user_id: i64, platform: &str) -> anyhow::Result<Option<InterviewState>> {
        let mut stmt = self.conn().prepare(
            "SELECT user_id, platform, mode, draft, turns, started_at \
             FROM interview_states WHERE user_id = ?1 AND platform = ?2",
        )?;
        let mut rows = stmt.query_map(params![user_id, platform], row_to_interview_state)?;
        rows.next().transpose().context("Failed to read interview state")
    }

    pub fn set_interview_state(&self, user_id: i64, platform: &str, mode: &str, draft: &str, turns: i32) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO interview_states (user_id, platform, mode, draft, turns) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_id, platform) DO UPDATE SET mode = excluded.mode, draft = excluded.draft, turns = excluded.turns",
            params![user_id, platform, mode, draft, turns],
        )?;
        Ok(())
    }

    pub fn clear_interview_state(&self, user_id: i64, platform: &str) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM interview_states WHERE user_id = ?1 AND platform = ?2", params![user_id, platform])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::new_user;
    use super::*;

    fn test_db() -> (Database, i64) {
        let db = Database::open_in_memory().unwrap();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();
        (db, user_id)
    }

    #[test]
    fn philosophy_insert_and_latest() {
        let (db, user_id) = test_db();
        assert!(db.latest_philosophy(user_id).unwrap().is_none());

        db.insert_philosophy(user_id, "v1: hypertrophy", "interview").unwrap();
        db.insert_philosophy(user_id, "v2: 5x5 strength, home gym squat rack 120kg", "interview").unwrap();

        let latest = db.latest_philosophy(user_id).unwrap().unwrap();
        assert!(latest.content.contains("v2"));
        assert_eq!(latest.source, "interview");
    }

    #[test]
    fn append_note_adds_bullet_and_row() {
        let (db, user_id) = test_db();
        db.insert_philosophy(user_id, "5x5 strength", "interview").unwrap();

        db.append_philosophy_note(user_id, "dislikes barbell squats, prefer goblet").unwrap();

        let latest = db.latest_philosophy(user_id).unwrap().unwrap();
        assert_eq!(latest.source, "note");
        assert!(latest.content.starts_with("5x5 strength"));
        assert!(latest.content.contains("- dislikes barbell squats"));
    }

    #[test]
    fn append_note_without_existing_philosophy() {
        let (db, user_id) = test_db();
        db.append_philosophy_note(user_id, "trains mornings only").unwrap();
        let latest = db.latest_philosophy(user_id).unwrap().unwrap();
        assert_eq!(latest.content, "- trains mornings only");
    }

    #[test]
    fn interview_state_upsert_and_clear() {
        let (db, user_id) = test_db();
        assert!(db.get_interview_state(user_id, "telegram").unwrap().is_none());

        db.set_interview_state(user_id, "telegram", "philosophy", "", 0).unwrap();
        db.set_interview_state(user_id, "telegram", "philosophy", "goal=hypertrophy", 1).unwrap();
        let state = db.get_interview_state(user_id, "telegram").unwrap().unwrap();
        assert_eq!(state.turns, 1);
        assert_eq!(state.draft, "goal=hypertrophy");

        // Per-platform isolation: confide unaffected.
        assert!(db.get_interview_state(user_id, "confide").unwrap().is_none());

        db.clear_interview_state(user_id, "telegram").unwrap();
        assert!(db.get_interview_state(user_id, "telegram").unwrap().is_none());
    }
}

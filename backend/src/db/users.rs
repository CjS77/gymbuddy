use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::User;

pub(super) fn row_to_user(row: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        name: row.get(1)?,
        telegram_id: row.get(2)?,
        signal_id: row.get(3)?,
        pubkey: row.get(4)?,
        timezone: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        beta_tester: row.get::<_, i64>(8)? != 0,
        timers_enabled: row.get::<_, i64>(9)? != 0,
    })
}

const SELECT_USER: &str =
    "SELECT id, name, telegram_id, signal_id, pubkey, timezone, created_at, updated_at, beta_tester, timers_enabled FROM users";

impl Database {
    /// Insert a user. Returns the generated id.
    pub fn insert_user(&self, user: &User) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO users (name, telegram_id, signal_id, pubkey, timezone, beta_tester, timers_enabled) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![user.name, user.telegram_id, user.signal_id, user.pubkey, user.timezone, user.beta_tester as i64, user.timers_enabled as i64],
        )?;
        let id = self.conn().last_insert_rowid();
        tracing::debug!(id, name = %user.name, telegram_id = ?user.telegram_id, "DB: inserted user");
        Ok(id)
    }

    pub fn get_user(&self, id: i64) -> anyhow::Result<Option<User>> {
        let sql = format!("{SELECT_USER} WHERE id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_user)?;
        rows.next().transpose().context("Failed to read user row")
    }

    pub fn get_user_by_telegram_id(&self, telegram_id: &str) -> anyhow::Result<Option<User>> {
        let sql = format!("{SELECT_USER} WHERE telegram_id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![telegram_id], row_to_user)?;
        rows.next().transpose().context("Failed to read user row")
    }

    pub fn get_user_by_signal_id(&self, signal_id: &str) -> anyhow::Result<Option<User>> {
        let sql = format!("{SELECT_USER} WHERE signal_id = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![signal_id], row_to_user)?;
        rows.next().transpose().context("Failed to read user row")
    }

    /// Look up a user by their confide ed25519 public key (hex). Used by the
    /// confide transport to resolve the connecting peer to a registered user.
    pub fn get_user_by_pubkey(&self, pubkey: &str) -> anyhow::Result<Option<User>> {
        let sql = format!("{SELECT_USER} WHERE pubkey = ?1");
        let mut stmt = self.conn().prepare(&sql)?;
        let mut rows = stmt.query_map(params![pubkey], row_to_user)?;
        rows.next().transpose().context("Failed to read user row")
    }

    pub fn update_user(&self, user: &User) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE users SET name = ?1, telegram_id = ?2, signal_id = ?3, pubkey = ?4, timezone = ?5, beta_tester = ?6, \
             updated_at = datetime('now') WHERE id = ?7",
            params![user.name, user.telegram_id, user.signal_id, user.pubkey, user.timezone, user.beta_tester as i64, user.id],
        )?;
        anyhow::ensure!(rows > 0, "User with id {} not found", user.id);
        tracing::debug!(id = user.id, "DB: updated user");
        Ok(())
    }

    /// Flip the `beta_tester` flag on a user. Intended for operator/CLI use to
    /// grant or revoke access to beta-only commands such as `/feedback`.
    pub fn set_beta_tester(&self, user_id: i64, is_beta: bool) -> anyhow::Result<()> {
        let rows = self.conn().execute(
            "UPDATE users SET beta_tester = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![is_beta as i64, user_id],
        )?;
        anyhow::ensure!(rows > 0, "User with id {user_id} not found");
        tracing::debug!(user_id, is_beta, "DB: set beta_tester");
        Ok(())
    }

    /// Set the user's rest-timer preference. Returns the new state so callers can
    /// report it without a re-read.
    pub fn set_user_timers(&self, user_id: i64, enabled: bool) -> anyhow::Result<bool> {
        let rows = self.conn().execute(
            "UPDATE users SET timers_enabled = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![enabled as i64, user_id],
        )?;
        anyhow::ensure!(rows > 0, "User with id {user_id} not found");
        tracing::debug!(user_id, enabled, "DB: set timers_enabled");
        Ok(enabled)
    }

    pub fn delete_user(&self, id: i64) -> anyhow::Result<()> {
        let rows = self.conn().execute("DELETE FROM users WHERE id = ?1", params![id])?;
        anyhow::ensure!(rows > 0, "User with id {id} not found");
        tracing::debug!(id, "DB: deleted user");
        Ok(())
    }

    pub fn list_users(&self) -> anyhow::Result<Vec<User>> {
        let sql = format!("{SELECT_USER} ORDER BY name");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map([], row_to_user)?;
        rows.collect::<Result<Vec<_>, _>>().context("Failed to list users")
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{MeasurementType, new_exercise_entry, new_exercise_set, new_user, new_user_with_pubkey};
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get_user() {
        let db = test_db();
        let user = new_user("Alice", Some("alice_tg"), "Europe/London");
        let id = db.insert_user(&user).unwrap();

        let fetched = db.get_user(id).unwrap().unwrap();
        assert_eq!(fetched.name, "Alice");
        assert_eq!(fetched.telegram_id.as_deref(), Some("alice_tg"));
        assert_eq!(fetched.timezone, "Europe/London");
    }

    #[test]
    fn get_by_telegram_id() {
        let db = test_db();
        let user = new_user("Bob", Some("bob_tg"), "UTC");
        let id = db.insert_user(&user).unwrap();

        let fetched = db.get_user_by_telegram_id("bob_tg").unwrap().unwrap();
        assert_eq!(fetched.id, id);
    }

    #[test]
    fn duplicate_telegram_id_fails() {
        let db = test_db();
        let u1 = new_user("Alice", Some("same_tg"), "UTC");
        let u2 = new_user("Bob", Some("same_tg"), "UTC");
        db.insert_user(&u1).unwrap();
        assert!(db.insert_user(&u2).is_err());
    }

    #[test]
    fn get_by_pubkey() {
        let db = test_db();
        let user = new_user_with_pubkey("Bob", "abcd1234", "UTC");
        let id = db.insert_user(&user).unwrap();

        let fetched = db.get_user_by_pubkey("abcd1234").unwrap().unwrap();
        assert_eq!(fetched.id, id);
        assert_eq!(fetched.pubkey.as_deref(), Some("abcd1234"));
        assert!(fetched.telegram_id.is_none());
    }

    #[test]
    fn duplicate_pubkey_fails() {
        let db = test_db();
        let u1 = new_user_with_pubkey("Alice", "same_key", "UTC");
        let u2 = new_user_with_pubkey("Bob", "same_key", "UTC");
        db.insert_user(&u1).unwrap();
        assert!(db.insert_user(&u2).is_err());
    }

    #[test]
    fn null_pubkeys_coexist() {
        // The unique index is partial (WHERE pubkey IS NOT NULL), so multiple
        // Telegram-only users (NULL pubkey) must insert without conflict.
        let db = test_db();
        db.insert_user(&new_user("Alice", Some("alice_tg"), "UTC")).unwrap();
        db.insert_user(&new_user("Bob", Some("bob_tg"), "UTC")).unwrap();
        assert_eq!(db.list_users().unwrap().len(), 2);
    }

    #[test]
    fn update_user() {
        let db = test_db();
        let user = new_user("Alice", None, "UTC");
        let id = db.insert_user(&user).unwrap();

        let mut fetched = db.get_user(id).unwrap().unwrap();
        fetched.name = "Alicia".into();
        fetched.telegram_id = Some("alicia_tg".into());
        db.update_user(&fetched).unwrap();

        let after = db.get_user(id).unwrap().unwrap();
        assert_eq!(after.name, "Alicia");
        assert_eq!(after.telegram_id.as_deref(), Some("alicia_tg"));
    }

    #[test]
    fn beta_tester_defaults_false() {
        let db = test_db();
        let user = new_user("Alice", Some("alice_tg"), "UTC");
        let id = db.insert_user(&user).unwrap();
        let fetched = db.get_user(id).unwrap().unwrap();
        assert!(!fetched.beta_tester);
    }

    #[test]
    fn set_beta_tester_toggles_flag() {
        let db = test_db();
        let user = new_user("Alice", Some("alice_tg"), "UTC");
        let id = db.insert_user(&user).unwrap();

        db.set_beta_tester(id, true).unwrap();
        assert!(db.get_user(id).unwrap().unwrap().beta_tester);

        db.set_beta_tester(id, false).unwrap();
        assert!(!db.get_user(id).unwrap().unwrap().beta_tester);
    }

    #[test]
    fn delete_user_cascades() {
        let db = test_db();
        let user = new_user("Alice", None, "UTC");
        let user_id = db.insert_user(&user).unwrap();

        let bp = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let session = db.start_session(user_id, None).unwrap();
        let entry_id = db.insert_entry(&new_exercise_entry(user_id, Some(session.id), None)).unwrap();
        let mut s = new_exercise_set(entry_id, bp.id, MeasurementType::WeightReps, 60.0);
        s.count = Some(10);
        db.insert_set(&s).unwrap();

        db.delete_user(user_id).unwrap();
        assert!(db.get_user(user_id).unwrap().is_none());
        assert!(db.get_session(session.id).unwrap().is_none());
        assert!(db.get_entry(entry_id).unwrap().is_none());
    }
}

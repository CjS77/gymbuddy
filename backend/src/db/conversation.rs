use anyhow::Context as _;
use rusqlite::params;

use super::database::Database;
use super::models::{ConversationMessage, ConversationRole};

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<ConversationMessage> {
    Ok(ConversationMessage {
        id: row.get(0)?,
        user_id: row.get(1)?,
        platform: row.get(2)?,
        role: ConversationRole::from_str_loose(&row.get::<_, String>(3)?),
        content: row.get(4)?,
        timestamp: row.get(5)?,
        exclude_from_context: row.get::<_, i32>(6)? != 0,
    })
}

const SELECT_MSG: &str = "SELECT id, user_id, platform, role, content, timestamp, exclude_from_context FROM conversation_history";

impl Database {
    pub fn insert_message(&self, msg: &ConversationMessage) -> anyhow::Result<i64> {
        self.conn().execute(
            "INSERT INTO conversation_history (user_id, platform, role, content, timestamp, exclude_from_context) \
             VALUES (?1, ?2, ?3, ?4, COALESCE(?5, datetime('now')), ?6)",
            params![
                msg.user_id,
                msg.platform,
                msg.role.as_str(),
                msg.content,
                if msg.timestamp.is_empty() { None } else { Some(&msg.timestamp) },
                msg.exclude_from_context as i32,
            ],
        )?;
        let id = self.conn().last_insert_rowid();
        tracing::debug!(id, role = %msg.role.as_str(), platform = %msg.platform, exclude = %msg.exclude_from_context, "DB: inserted message");
        Ok(id)
    }

    /// Every message stored for the user, including the ones excluded from LLM
    /// context. `get_recent_messages*` deliberately hide exclusions; this counts the
    /// table, which is what "was it persisted at all?" needs to ask.
    pub fn count_messages_for_user(&self, user_id: i64) -> anyhow::Result<i64> {
        self.conn()
            .query_row("SELECT COUNT(*) FROM conversation_history WHERE user_id = ?1", params![user_id], |row| row.get(0))
            .context("Failed to count messages for user")
    }

    pub fn get_recent_messages(&self, user_id: i64, limit: usize) -> anyhow::Result<Vec<ConversationMessage>> {
        let sql = format!("{SELECT_MSG} WHERE user_id = ?1 AND exclude_from_context = 0 ORDER BY timestamp DESC LIMIT ?2");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, limit as i64], row_to_message)?;
        let mut msgs: Vec<_> = rows.collect::<Result<Vec<_>, _>>().context("Failed to get recent messages")?;
        msgs.reverse();
        Ok(msgs)
    }

    pub fn get_recent_messages_for_platform(&self, user_id: i64, platform: &str, limit: usize) -> anyhow::Result<Vec<ConversationMessage>> {
        let sql =
            format!("{SELECT_MSG} WHERE user_id = ?1 AND platform = ?2 AND exclude_from_context = 0 ORDER BY timestamp DESC LIMIT ?3");
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![user_id, platform, limit as i64], row_to_message)?;
        let mut msgs: Vec<_> = rows.collect::<Result<Vec<_>, _>>().context("Failed to get recent messages for platform")?;
        msgs.reverse();
        Ok(msgs)
    }

    pub fn exclude_all_messages_for_platform(&self, user_id: i64, platform: &str) -> anyhow::Result<usize> {
        let updated = self.conn().execute(
            "UPDATE conversation_history SET exclude_from_context = 1 WHERE user_id = ?1 AND platform = ?2 AND exclude_from_context = 0",
            params![user_id, platform],
        )?;
        tracing::info!(user_id, %platform, updated, "DB: excluded all messages from context");
        Ok(updated)
    }

    /// Trim this user's history on a single platform down to its most recent
    /// `keep_last` rows. Scoped to `(user, platform)` to mirror the read path
    /// ([`Self::get_recent_messages_for_platform`]): a chatty session on one
    /// platform must not evict another platform's context.
    pub fn prune_old_messages_for_platform(&self, user_id: i64, platform: &str, keep_last: usize) -> anyhow::Result<usize> {
        let deleted = self.conn().execute(
            "DELETE FROM conversation_history WHERE user_id = ?1 AND platform = ?2 AND id NOT IN \
             (SELECT id FROM conversation_history WHERE user_id = ?1 AND platform = ?2 ORDER BY timestamp DESC LIMIT ?3)",
            params![user_id, platform, keep_last as i64],
        )?;
        if deleted > 0 {
            tracing::debug!(user_id, %platform, deleted, "DB: pruned old messages");
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{new_conversation_message, new_user};
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get_recent() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::User, "Hello")).unwrap();
        db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::Assistant, "Hi there!")).unwrap();

        let msgs = db.get_recent_messages(user_id, 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().any(|m| m.role == ConversationRole::User));
        assert!(msgs.iter().any(|m| m.role == ConversationRole::Assistant));
    }

    #[test]
    fn get_recent_by_platform() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::User, "From TG")).unwrap();
        db.insert_message(&new_conversation_message(user_id, "web", ConversationRole::User, "From Web")).unwrap();

        let msgs = db.get_recent_messages_for_platform(user_id, "telegram", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "From TG");
    }

    #[test]
    fn prune_old_messages_for_platform() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        for i in 0..10 {
            db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::User, &format!("Message {i}"))).unwrap();
        }

        let deleted = db.prune_old_messages_for_platform(user_id, "telegram", 3).unwrap();
        assert_eq!(deleted, 7);

        let remaining = db.get_recent_messages(user_id, 100).unwrap();
        assert_eq!(remaining.len(), 3);
    }

    #[test]
    fn prune_is_scoped_per_platform() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        // A quiet confide (TUI) session: two messages the user still expects to see.
        db.insert_message(&new_conversation_message(user_id, "confide", ConversationRole::User, "tui first")).unwrap();
        db.insert_message(&new_conversation_message(user_id, "confide", ConversationRole::Assistant, "tui second")).unwrap();

        // A chatty Telegram session: far more than the pruning window.
        (0..20).for_each(|i| {
            db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::User, &format!("tg {i}"))).unwrap();
        });

        // Prune telegram down to its 3 most recent, as the handler does after a turn.
        let deleted = db.prune_old_messages_for_platform(user_id, "telegram", 3).unwrap();
        assert_eq!(deleted, 17);

        // Telegram is trimmed...
        let tg = db.get_recent_messages_for_platform(user_id, "telegram", 100).unwrap();
        assert_eq!(tg.len(), 3);

        // ...but the confide history is untouched — the chatty platform did not evict it.
        let tui = db.get_recent_messages_for_platform(user_id, "confide", 100).unwrap();
        assert_eq!(tui.len(), 2);
        assert_eq!(tui[0].content, "tui first");
        assert_eq!(tui[1].content, "tui second");
    }

    #[test]
    fn exclude_all_messages_for_platform() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::User, "Hello")).unwrap();
        db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::Assistant, "Hi!")).unwrap();
        db.insert_message(&new_conversation_message(user_id, "web", ConversationRole::User, "Web msg")).unwrap();

        let excluded = db.exclude_all_messages_for_platform(user_id, "telegram").unwrap();
        assert_eq!(excluded, 2);

        let tg_msgs = db.get_recent_messages_for_platform(user_id, "telegram", 100).unwrap();
        assert_eq!(tg_msgs.len(), 0);

        let web_msgs = db.get_recent_messages_for_platform(user_id, "web", 100).unwrap();
        assert_eq!(web_msgs.len(), 1);

        let excluded = db.exclude_all_messages_for_platform(user_id, "telegram").unwrap();
        assert_eq!(excluded, 0);
    }

    #[test]
    fn excluded_messages_hidden_from_context() {
        let db = test_db();
        let user_id = db.insert_user(&new_user("Tester", None, "UTC")).unwrap();

        let mut m1 = new_conversation_message(user_id, "telegram", ConversationRole::User, "bad request");
        m1.exclude_from_context = true;
        db.insert_message(&m1).unwrap();

        db.insert_message(&new_conversation_message(user_id, "telegram", ConversationRole::User, "good request")).unwrap();

        let msgs = db.get_recent_messages_for_platform(user_id, "telegram", 100).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "good request");

        let msgs = db.get_recent_messages(user_id, 100).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "good request");
    }
}

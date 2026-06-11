use rusqlite::params;

use super::database::Database;

impl Database {
    /// True if actor == target, or actor has read/write/admin on any group containing target.
    pub fn can_read(&self, actor_id: i64, target_id: i64) -> anyhow::Result<bool> {
        if actor_id == target_id {
            return Ok(true);
        }
        let mut stmt = self.conn().prepare(
            "SELECT 1 FROM group_members gm1 \
             JOIN group_members gm2 ON gm1.group_id = gm2.group_id \
             WHERE gm1.user_id = ?1 AND gm2.user_id = ?2 \
               AND gm1.level IN ('read', 'write', 'admin') \
             LIMIT 1",
        )?;
        let exists = stmt.query_map(params![actor_id, target_id], |row| row.get::<_, i32>(0))?.next().is_some();
        Ok(exists)
    }

    /// True if actor == target, or actor has write/admin on any group containing target.
    pub fn can_write(&self, actor_id: i64, target_id: i64) -> anyhow::Result<bool> {
        if actor_id == target_id {
            return Ok(true);
        }
        let mut stmt = self.conn().prepare(
            "SELECT 1 FROM group_members gm1 \
             JOIN group_members gm2 ON gm1.group_id = gm2.group_id \
             WHERE gm1.user_id = ?1 AND gm2.user_id = ?2 \
               AND gm1.level IN ('write', 'admin') \
             LIMIT 1",
        )?;
        let exists = stmt.query_map(params![actor_id, target_id], |row| row.get::<_, i32>(0))?.next().is_some();
        Ok(exists)
    }

    /// True if actor has admin level on the specified group.
    pub fn can_admin_group(&self, actor_id: i64, group_id: i64) -> anyhow::Result<bool> {
        let mut stmt = self.conn().prepare(
            "SELECT 1 FROM group_members \
             WHERE user_id = ?1 AND group_id = ?2 AND level = 'admin' \
             LIMIT 1",
        )?;
        let exists = stmt.query_map(params![actor_id, group_id], |row| row.get::<_, i32>(0))?.next().is_some();
        Ok(exists)
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{AccessLevel, Group, new_user};
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn make_group(name: &str) -> Group {
        Group { id: 0, name: name.to_string(), description: None, created_at: "2025-01-01 00:00:00".into() }
    }

    fn setup_group_with_users(db: &Database, actor_level: AccessLevel) -> (i64, i64, i64) {
        let actor = db.insert_user(&new_user("Actor", None, "UTC")).unwrap();
        let target = db.insert_user(&new_user("Target", None, "UTC")).unwrap();
        let group = db.insert_group(&make_group("Test Group")).unwrap();
        db.add_member(actor, group, actor_level).unwrap();
        db.add_member(target, group, AccessLevel::Read).unwrap();
        (actor, target, group)
    }

    #[test]
    fn user_can_read_own_data() {
        let db = test_db();
        let user = db.insert_user(&new_user("Self", None, "UTC")).unwrap();
        assert!(db.can_read(user, user).unwrap());
    }

    #[test]
    fn user_cannot_read_other_data() {
        let db = test_db();
        let u1 = db.insert_user(&new_user("Alice", None, "UTC")).unwrap();
        let u2 = db.insert_user(&new_user("Bob", None, "UTC")).unwrap();
        assert!(!db.can_read(u1, u2).unwrap());
    }

    #[test]
    fn group_read_access_works() {
        let db = test_db();
        let (actor, target, _) = setup_group_with_users(&db, AccessLevel::Read);
        assert!(db.can_read(actor, target).unwrap());
    }

    #[test]
    fn group_write_access_works() {
        let db = test_db();
        let (actor, target, _) = setup_group_with_users(&db, AccessLevel::Write);
        assert!(db.can_write(actor, target).unwrap());
    }

    #[test]
    fn group_admin_access_works() {
        let db = test_db();
        let (actor, _, group_id) = setup_group_with_users(&db, AccessLevel::Admin);
        assert!(db.can_admin_group(actor, group_id).unwrap());
    }

    #[test]
    fn write_implies_read() {
        let db = test_db();
        let (actor, target, _) = setup_group_with_users(&db, AccessLevel::Write);
        assert!(db.can_read(actor, target).unwrap());
    }

    #[test]
    fn admin_implies_write_and_read() {
        let db = test_db();
        let (actor, target, _) = setup_group_with_users(&db, AccessLevel::Admin);
        assert!(db.can_read(actor, target).unwrap());
        assert!(db.can_write(actor, target).unwrap());
    }

    #[test]
    fn non_member_cannot_read() {
        let db = test_db();
        let actor = db.insert_user(&new_user("Outsider", None, "UTC")).unwrap();
        let target = db.insert_user(&new_user("Member", None, "UTC")).unwrap();
        let group = db.insert_group(&make_group("Private Group")).unwrap();
        db.add_member(target, group, AccessLevel::Read).unwrap();
        assert!(!db.can_read(actor, target).unwrap());
    }

    #[test]
    fn nonexistent_actor_returns_false() {
        let db = test_db();
        let target = db.insert_user(&new_user("Target", None, "UTC")).unwrap();
        assert!(!db.can_read(99999, target).unwrap());
    }

    #[test]
    fn nonexistent_target_returns_false() {
        let db = test_db();
        let actor = db.insert_user(&new_user("Actor", None, "UTC")).unwrap();
        assert!(!db.can_read(actor, 99999).unwrap());
    }

    #[test]
    fn deleted_group_revokes_access() {
        let db = test_db();
        let (actor, target, group_id) = setup_group_with_users(&db, AccessLevel::Admin);
        assert!(db.can_read(actor, target).unwrap());

        db.delete_group(group_id).unwrap();
        assert!(!db.can_read(actor, target).unwrap());
    }
}

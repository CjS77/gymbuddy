-- Schema support for the confide (encrypted p2p) transport.

-- 1. Public-key identity for confide clients (TUI, Android).
--
--    Existing Telegram-only users keep a NULL pubkey. The unique index is partial
--    (WHERE pubkey IS NOT NULL) so multiple NULLs coexist while each registered
--    pubkey maps to at most one user.
ALTER TABLE users ADD COLUMN pubkey TEXT;
CREATE UNIQUE INDEX idx_users_pubkey ON users(pubkey) WHERE pubkey IS NOT NULL;

-- 2. Allow 'confide' as a conversation platform.
--
--    SQLite cannot alter a CHECK constraint in place, so rebuild the table. 'web'
--    is kept in the allowed set (even though the dashboard was removed) so any
--    existing 'web' rows copy across cleanly.
CREATE TABLE conversation_history_new (
    id                   INTEGER PRIMARY KEY,
    user_id              INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform             TEXT NOT NULL DEFAULT 'telegram'
                             CHECK (platform IN ('telegram','signal','web','confide')),
    role                 TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
    content              TEXT NOT NULL,
    timestamp            TEXT NOT NULL DEFAULT (datetime('now')),
    exclude_from_context INTEGER NOT NULL DEFAULT 0
);
INSERT INTO conversation_history_new SELECT * FROM conversation_history;
DROP TABLE conversation_history;
ALTER TABLE conversation_history_new RENAME TO conversation_history;
CREATE INDEX idx_conversation_user_time ON conversation_history(user_id, timestamp);

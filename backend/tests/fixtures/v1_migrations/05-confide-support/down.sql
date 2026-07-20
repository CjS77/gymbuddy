-- Reverse the conversation platform CHECK back to its original set.
CREATE TABLE conversation_history_old (
    id                   INTEGER PRIMARY KEY,
    user_id              INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform             TEXT NOT NULL DEFAULT 'telegram'
                             CHECK (platform IN ('telegram','signal','web')),
    role                 TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
    content              TEXT NOT NULL,
    timestamp            TEXT NOT NULL DEFAULT (datetime('now')),
    exclude_from_context INTEGER NOT NULL DEFAULT 0
);
INSERT INTO conversation_history_old SELECT * FROM conversation_history;
DROP TABLE conversation_history;
ALTER TABLE conversation_history_old RENAME TO conversation_history;
CREATE INDEX idx_conversation_user_time ON conversation_history(user_id, timestamp);

-- Drop the pubkey index before the column it references.
DROP INDEX IF EXISTS idx_users_pubkey;
ALTER TABLE users DROP COLUMN pubkey;

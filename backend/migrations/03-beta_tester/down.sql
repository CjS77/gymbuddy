-- SQLite < 3.35 cannot DROP COLUMN, so rebuild the table without beta_tester.
CREATE TABLE users_new (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    telegram_id TEXT UNIQUE,
    signal_id   TEXT UNIQUE,
    timezone    TEXT NOT NULL DEFAULT 'UTC',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT INTO users_new (id, name, telegram_id, signal_id, timezone, created_at, updated_at)
SELECT id, name, telegram_id, signal_id, timezone, created_at, updated_at FROM users;
DROP TABLE users;
ALTER TABLE users_new RENAME TO users;

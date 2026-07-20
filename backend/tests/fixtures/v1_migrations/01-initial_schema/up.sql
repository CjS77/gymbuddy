-- =============================================================================
-- corre-gym — initial schema (v1)
--
-- All primary keys are auto-incrementing INTEGERs (rowid alias). Foreign keys
-- are INTEGERs throughout.

-- -----------------------------------------------------------------------------
-- Reference: measurement types (how a set is recorded)
-- -----------------------------------------------------------------------------
CREATE TABLE measurement_types (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

INSERT INTO measurement_types (id, name) VALUES
    (1, 'weight_reps'),
    (2, 'time_based'),
    (3, 'distance_based'),
    (4, 'level_based'),
    (5, 'score_based');

-- -----------------------------------------------------------------------------
-- Hierarchical exercise taxonomy
-- -----------------------------------------------------------------------------
CREATE TABLE exercise_types (
    id                  INTEGER PRIMARY KEY,
    name                TEXT NOT NULL COLLATE NOCASE,
    parent_id           INTEGER REFERENCES exercise_types(id) ON DELETE RESTRICT,
    level               TEXT NOT NULL CHECK (level IN
                            ('muscle_group','specific_muscle','exercise','variation')),
    aliases             TEXT,
    purpose             TEXT,
    measurement_type_id INTEGER REFERENCES measurement_types(id),
    description         TEXT,
    url                 TEXT,  -- relative path to an illustrative image/video; populated for muscle_group / specific_muscle, NULL for exercises and variations until per-exercise media is sourced
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (parent_id, name),
    CHECK ((level = 'muscle_group' AND parent_id IS NULL)
        OR (level <> 'muscle_group' AND parent_id IS NOT NULL))
);

CREATE INDEX idx_exercise_types_parent ON exercise_types(parent_id);
CREATE INDEX idx_exercise_types_level  ON exercise_types(level);

-- -----------------------------------------------------------------------------
-- Users (telegram_id / signal_id are external identifiers; users.id is internal)
-- -----------------------------------------------------------------------------
CREATE TABLE users (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    telegram_id TEXT UNIQUE,
    signal_id   TEXT UNIQUE,
    timezone    TEXT NOT NULL DEFAULT 'UTC',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- -----------------------------------------------------------------------------
-- Access groups
-- -----------------------------------------------------------------------------
CREATE TABLE groups (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE group_members (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_id   INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    level      TEXT NOT NULL DEFAULT 'read'
                   CHECK (level IN ('read','write','admin')),
    granted_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, group_id)
);

-- -----------------------------------------------------------------------------
-- Per-user goals (target a specific exercise_type at any level)
-- -----------------------------------------------------------------------------
CREATE TABLE exercise_goals (
    id               INTEGER PRIMARY KEY,
    user_id          INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    exercise_type_id INTEGER NOT NULL REFERENCES exercise_types(id),
    target_value     REAL NOT NULL,
    start_date       TEXT NOT NULL,
    end_date         TEXT,
    achieved         INTEGER NOT NULL DEFAULT 0,
    notes            TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_goals_user ON exercise_goals(user_id, achieved);

-- -----------------------------------------------------------------------------
-- Workout sessions (a whole training session)
-- -----------------------------------------------------------------------------
CREATE TABLE sessions (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    ended_at   TEXT,
    notes      TEXT
);

CREATE INDEX idx_sessions_user ON sessions(user_id, started_at);

-- -----------------------------------------------------------------------------
-- Exercise entries (a block of related sets within a session)
-- -----------------------------------------------------------------------------
CREATE TABLE exercise_entry (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_id      INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
    start_timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    end_timestamp   TEXT,
    comment         TEXT
);

CREATE INDEX idx_exercise_entry_user    ON exercise_entry(user_id, start_timestamp);
CREATE INDEX idx_exercise_entry_session ON exercise_entry(session_id);

-- -----------------------------------------------------------------------------
-- Sets (individual recorded efforts)
--
-- The (count, value) pair is interpreted via measurement_type_id:
--     weight_reps    -> count = reps, value = weight_kg
--     time_based     -> count = NULL,  value = duration_secs
--     distance_based -> count = NULL,  value = distance_m
--     level_based    -> count = NULL,  value = level
--     score_based    -> count = NULL,  value = score
-- -----------------------------------------------------------------------------
CREATE TABLE sets (
    id                   INTEGER PRIMARY KEY,
    exercise_entry_id    INTEGER NOT NULL REFERENCES exercise_entry(id) ON DELETE CASCADE,
    exercise_type_id     INTEGER NOT NULL REFERENCES exercise_types(id),
    order_idx            INTEGER NOT NULL DEFAULT 0,
    measurement_type_id  INTEGER NOT NULL REFERENCES measurement_types(id),
    count                INTEGER,
    value                REAL NOT NULL,
    perceived_difficulty TEXT NOT NULL DEFAULT 'medium'
                             CHECK (perceived_difficulty IN ('easy','medium','hard','failure')),
    comment              TEXT,
    logged_at            TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_sets_entry        ON sets(exercise_entry_id);
CREATE INDEX idx_sets_type_logged  ON sets(exercise_type_id, logged_at);

-- -----------------------------------------------------------------------------
-- Schedules (cron-driven workout reminders)
-- -----------------------------------------------------------------------------
CREATE TABLE schedules (
    id                   INTEGER PRIMARY KEY,
    user_id              INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name                 TEXT NOT NULL,
    cron_expr            TEXT NOT NULL,
    reminder_type        TEXT NOT NULL DEFAULT 'text'
                             CHECK (reminder_type IN ('text','voice')),
    reminder_notice_mins INTEGER NOT NULL DEFAULT 30,
    enabled              INTEGER NOT NULL DEFAULT 1,
    created_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at           TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_schedules_user ON schedules(user_id);

CREATE TABLE schedule_exercises (
    schedule_id      INTEGER NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    exercise_type_id INTEGER NOT NULL REFERENCES exercise_types(id),
    order_idx        INTEGER NOT NULL DEFAULT 0,
    target_sets      INTEGER,
    target_reps      INTEGER,
    target_weight_kg REAL,
    PRIMARY KEY (schedule_id, exercise_type_id)
);

-- -----------------------------------------------------------------------------
-- Health tracking (injuries, illnesses, wellbeing notes)
-- -----------------------------------------------------------------------------
CREATE TABLE health_entries (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    entry_type  TEXT NOT NULL CHECK (entry_type IN ('injury','illness','wellbeing')),
    body_part   TEXT,
    severity    TEXT NOT NULL DEFAULT 'mild'
                    CHECK (severity IN ('mild','moderate','severe')),
    description TEXT NOT NULL,
    started_at  TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT,
    notes       TEXT,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_health_user_date ON health_entries(user_id, started_at);

-- -----------------------------------------------------------------------------
-- Conversation history (LLM context per platform)
-- -----------------------------------------------------------------------------
CREATE TABLE conversation_history (
    id                   INTEGER PRIMARY KEY,
    user_id              INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform             TEXT NOT NULL DEFAULT 'telegram'
                             CHECK (platform IN ('telegram','signal','web')),
    role                 TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
    content              TEXT NOT NULL,
    timestamp            TEXT NOT NULL DEFAULT (datetime('now')),
    exclude_from_context INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_conversation_user_time ON conversation_history(user_id, timestamp);

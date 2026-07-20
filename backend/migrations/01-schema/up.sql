-- =============================================================================
-- GymBuddy — schema v2 baseline
--
-- This is a fresh baseline, not an evolution of schema v1. The 13 v1 migrations
-- live on as a test fixture in `backend/tests/fixtures/v1_migrations/`, and a v1
-- database reaches this schema through `gymbuddy migrate` (export → create v2 →
-- import), never by running migrations in place.
--
-- Vocabulary: one name per concept. The built session artefact is a
-- **session roster** (`session_rosters` / `roster_exercises`) — "plan" is gone
-- from the schema. Long-term structure is a **programme** (British spelling
-- everywhere, including columns and CHECK values).
--
-- All primary keys are auto-incrementing INTEGERs (rowid alias). Foreign keys
-- are INTEGERs throughout and are enforced (`PRAGMA foreign_keys = ON` is set
-- when the connection is opened).
--
-- Tables are declared in FK dependency order so the whole file applies as one
-- batch against an empty database.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- Reference: measurement types (how a set is recorded)
--
-- Seeded here rather than from the startup catalogue because the ids are load-
-- bearing: `exercise_types.measurement_type_id` in the catalogue seed refers to
-- them by number, and the Rust layer matches on the names.
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
-- Hierarchical exercise taxonomy (the catalogue)
--
-- Rows are reference data, not user data: the baseline arrives with migration
-- 02, and later additions are applied at startup from `backend/catalogue/`
-- (`INSERT OR IGNORE`, idempotent on `UNIQUE (parent_id, name)`). Adding an
-- exercise is therefore a data change, never a migration.
--
-- v1's `description` column is dropped: it was never populated, and `purpose`
-- plus `aliases` carry everything the matcher and the prompts read.
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
    url                 TEXT,  -- relative path to an illustrative image/video; populated for muscle_group / specific_muscle, NULL for exercises and variations until per-exercise media is sourced
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (parent_id, name),
    CHECK ((level = 'muscle_group' AND parent_id IS NULL)
        OR (level <> 'muscle_group' AND parent_id IS NOT NULL))
);

CREATE INDEX idx_exercise_types_parent ON exercise_types(parent_id);
CREATE INDEX idx_exercise_types_level  ON exercise_types(level);

-- -----------------------------------------------------------------------------
-- Users
--
-- `telegram_id` and `pubkey` (confide clients) are external identifiers;
-- `users.id` is internal and the only id any other table references. v1's
-- `signal_id` is dropped — no Signal transport was ever built.
--
-- The `pubkey` unique index is partial so Telegram-only users can share a NULL
-- while each registered key maps to at most one user.
-- -----------------------------------------------------------------------------
CREATE TABLE users (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    telegram_id    TEXT UNIQUE,
    pubkey         TEXT,
    timezone       TEXT NOT NULL DEFAULT 'UTC',
    beta_tester    INTEGER NOT NULL DEFAULT 0,
    timers_enabled INTEGER NOT NULL DEFAULT 1,
    created_at     TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX idx_users_pubkey ON users(pubkey) WHERE pubkey IS NOT NULL;

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
-- Measurable quantities (bodyweight, body fat, girths, resting heart rate)
--
-- v1 joined `goals.metric` to `body_metrics.metric` on a naming convention
-- alone: two free-text columns that agreed by discipline and failed silently
-- when they did not. Both now reference this table, so a metric goal and its
-- measurement series cannot drift apart.
--
-- `name` is the canonical, unit-suffixed form (`bodyweight_kg`, `body_fat_pct`)
-- and `unit` names the unit that suffix implies. There is deliberately no CHECK
-- on `name`: a new metric is a row insert, never a migration. The canonical
-- names are seeded from `backend/catalogue/` at startup.
-- -----------------------------------------------------------------------------
CREATE TABLE metrics (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    unit TEXT
);

-- -----------------------------------------------------------------------------
-- Per-user goals
--
-- A goal is denominated either by an exercise (`exercise_type_id`) or by a
-- metric (`metric_id`) — the CHECK enforces at least one. `direction` says
-- which way is better, so a weightloss goal and a strength goal are judged by
-- the same code. `achieved_at` records *when* the goal was met; `achieved`
-- stays the cheap boolean the indexes and the read-side `GoalStatus` use.
-- -----------------------------------------------------------------------------
CREATE TABLE goals (
    id               INTEGER PRIMARY KEY,
    user_id          INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind             TEXT NOT NULL DEFAULT 'strength'
                         CHECK (kind IN ('strength','endurance','bodyweight','body_composition','habit')),
    exercise_type_id INTEGER REFERENCES exercise_types(id),
    metric_id        INTEGER REFERENCES metrics(id),
    target_value     REAL NOT NULL,
    direction        TEXT NOT NULL DEFAULT 'increase'
                         CHECK (direction IN ('increase','decrease')),
    priority         INTEGER NOT NULL DEFAULT 0,
    start_date       TEXT NOT NULL,
    target_date      TEXT,
    achieved         INTEGER NOT NULL DEFAULT 0,
    achieved_at      TEXT,
    notes            TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK (exercise_type_id IS NOT NULL OR metric_id IS NOT NULL)
);

CREATE INDEX idx_goals_user     ON goals(user_id, achieved);
CREATE INDEX idx_goals_priority ON goals(user_id, priority);

-- -----------------------------------------------------------------------------
-- Sessions (a whole training session)
--
-- `intent` is v1's `notes` renamed and cleaned up: free text describing what
-- the session was for, with no sentinel encoding of any kind (v1 smuggled a
-- schedule name in as a `plan:` prefix; schedules are gone and so is that).
-- The review generator reads it.
--
-- `overall_effort` / `felt` / `cut_short*` are the session verdict.
-- `effort_source` says where the verdict came from: 'derived' when it was
-- distilled from the sets at auto-close (the user was not there to ask), and
-- 'confirmed' once the user agreed or corrected it. NULL means no verdict yet.
-- -----------------------------------------------------------------------------
CREATE TABLE sessions (
    id               INTEGER PRIMARY KEY,
    user_id          INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    started_at       TEXT NOT NULL DEFAULT (datetime('now')),
    ended_at         TEXT,
    intent           TEXT,
    overall_effort   TEXT CHECK (overall_effort IN ('easy','medium','hard','failure')),
    effort_source    TEXT CHECK (effort_source IN ('derived','confirmed')),
    felt             TEXT CHECK (felt IN ('great','good','ok','rough')),
    cut_short        INTEGER NOT NULL DEFAULT 0,
    cut_short_reason TEXT
);

CREATE INDEX idx_sessions_user ON sessions(user_id, started_at);

-- -----------------------------------------------------------------------------
-- Exercise entries (a block of related sets within a session)
--
-- `session_id` is nullable: a set logged outside a session is first-class.
-- -----------------------------------------------------------------------------
CREATE TABLE exercise_entries (
    id              INTEGER PRIMARY KEY,
    user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_id      INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
    start_timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    end_timestamp   TEXT,
    comment         TEXT
);

CREATE INDEX idx_exercise_entries_user    ON exercise_entries(user_id, start_timestamp);
CREATE INDEX idx_exercise_entries_session ON exercise_entries(session_id);

-- -----------------------------------------------------------------------------
-- Sets (individual recorded efforts)
--
-- POLYMORPHIC BY DESIGN. The (count, value) pair is interpreted through
-- `measurement_type_id`:
--
--     weight_reps    -> count = reps, value = weight_kg
--     time_based     -> count = NULL, value = duration_secs
--     distance_based -> count = NULL, value = distance_m
--     level_based    -> count = NULL, value = level
--     score_based    -> count = NULL, value = score
--
-- This encoding was kept over per-type columns on purpose: it is load-bearing
-- across the DAOs, the prompts and the wire format (`SetLine`), and splitting
-- it would trade one documented convention for four mostly-NULL columns.
-- `value` is NOT NULL for every type; `count` is meaningful only for
-- weight_reps. Any code reading a set must consult the measurement type first.
-- -----------------------------------------------------------------------------
CREATE TABLE sets (
    id                   INTEGER PRIMARY KEY,
    exercise_entry_id    INTEGER NOT NULL REFERENCES exercise_entries(id) ON DELETE CASCADE,
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

CREATE INDEX idx_sets_entry       ON sets(exercise_entry_id);
CREATE INDEX idx_sets_type_logged ON sets(exercise_type_id, logged_at);

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
--
-- `platform` carries no CHECK: v1 constrained it to a fixed set and adding the
-- confide transport therefore cost a table rebuild. A new client must not be a
-- schema migration, so the column is free text.
-- -----------------------------------------------------------------------------
CREATE TABLE conversation_history (
    id                   INTEGER PRIMARY KEY,
    user_id              INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform             TEXT NOT NULL DEFAULT 'telegram',
    role                 TEXT NOT NULL CHECK (role IN ('user','assistant','system')),
    content              TEXT NOT NULL,
    timestamp            TEXT NOT NULL DEFAULT (datetime('now')),
    exclude_from_context INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_conversation_user_time ON conversation_history(user_id, timestamp);

-- -----------------------------------------------------------------------------
-- Body measurements
--
-- Long-shaped on purpose: one row per (user, metric, moment), never a column
-- per metric. `value` is always in the unit `metrics.unit` names.
--
-- This is the most sensitive data in the schema; retention and exposure policy
-- is documented in backend/src/db/body_metrics.rs.
-- -----------------------------------------------------------------------------
CREATE TABLE body_metrics (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    metric_id   INTEGER NOT NULL REFERENCES metrics(id),
    value       REAL NOT NULL,
    measured_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_body_metrics_user_metric ON body_metrics(user_id, metric_id, measured_at);

-- -----------------------------------------------------------------------------
-- Distilled training philosophy (was `workout_philosophy`)
--
-- Append-only: the most recent row per user is the active one. Equipment is
-- captured as free text inside `content` (no equipment table). `source`:
-- 'interview' (built via /philosophy), 'note' (a durable preference appended
-- mid-workout), 'import' (a migrated or restored row).
-- -----------------------------------------------------------------------------
CREATE TABLE philosophies (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    content    TEXT NOT NULL,
    source     TEXT NOT NULL DEFAULT 'interview' CHECK (source IN ('interview','note','import')),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_philosophies_user ON philosophies(user_id, created_at);

-- -----------------------------------------------------------------------------
-- Multi-turn interview state (was `interview_state`)
--
-- A row's presence means the user is mid-interview; its absence means normal
-- operation. `mode` says which interview: 'philosophy' distils training
-- philosophy, 'programme' builds a long-term programme. `draft` accumulates the
-- answer-so-far across turns; `turns` bounds the interview length. Kept in a
-- table (not smuggled into message text) so it survives history pruning and
-- /clear.
-- -----------------------------------------------------------------------------
CREATE TABLE interview_states (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform   TEXT NOT NULL,
    mode       TEXT NOT NULL CHECK (mode IN ('philosophy','programme')),
    draft      TEXT NOT NULL DEFAULT '',
    turns      INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, platform)
);

-- -----------------------------------------------------------------------------
-- Programmes: the long-term skeleton ad-hoc sessions slot into (was `programs`)
--
-- A programme is a skeleton, not a script: it persists the goals served, the
-- dates, the split, the mesocycle blocks and a progression policy, while each
-- session is still designed on demand against it. `split` and
-- `progression_policy` are free text — the LLM reads them; no query looks
-- inside. At most one 'active' programme per user, enforced in the DAO.
-- -----------------------------------------------------------------------------
CREATE TABLE programmes (
    id                 INTEGER PRIMARY KEY,
    user_id            INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title              TEXT NOT NULL,
    start_date         TEXT NOT NULL,
    target_end_date    TEXT,
    days_per_week      INTEGER NOT NULL CHECK (days_per_week BETWEEN 1 AND 7),
    split              TEXT NOT NULL,
    progression_policy TEXT NOT NULL,
    status             TEXT NOT NULL DEFAULT 'draft'
                           CHECK (status IN ('draft','active','completed','abandoned')),
    created_at         TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at         TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_programmes_user   ON programmes(user_id, created_at);
CREATE INDEX idx_programmes_status ON programmes(user_id, status);

-- The goals a programme serves. A goal outlives any one programme, so deleting
-- either side just unlinks.
CREATE TABLE programme_goals (
    programme_id INTEGER NOT NULL REFERENCES programmes(id) ON DELETE CASCADE,
    goal_id      INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    PRIMARY KEY (programme_id, goal_id)
);

-- Mesocycle blocks: an inclusive 1-based week range with an intent, e.g. weeks
-- 1-4 'hypertrophy', weeks 5-6 'deload'. This is what makes sessions build on
-- one another rather than repeat — the designer reads the block the current
-- week falls in and progresses within it.
CREATE TABLE programme_blocks (
    id           INTEGER PRIMARY KEY,
    programme_id INTEGER NOT NULL REFERENCES programmes(id) ON DELETE CASCADE,
    start_week   INTEGER NOT NULL CHECK (start_week >= 1),
    end_week     INTEGER NOT NULL CHECK (end_week >= start_week),
    focus        TEXT NOT NULL,
    notes        TEXT
);

CREATE INDEX idx_programme_blocks_programme ON programme_blocks(programme_id, start_week);

-- The week/day grid. `week_idx` is 1-based from the programme start; `day_idx`
-- is the 1-based ordinal training day within the week (not a calendar weekday),
-- bounded by days_per_week. A slot starts 'pending', becomes 'filled' when a
-- roster binds to it, 'missed' when its week passes untouched, or 'skipped'
-- when deliberately dropped.
--
-- Slots are skeleton-only: `focus` is a text intent, never an exercise list.
-- The one per-exercise prescription table in this schema is `roster_exercises`.
CREATE TABLE programme_slots (
    id           INTEGER PRIMARY KEY,
    programme_id INTEGER NOT NULL REFERENCES programmes(id) ON DELETE CASCADE,
    week_idx     INTEGER NOT NULL CHECK (week_idx >= 1),
    day_idx      INTEGER NOT NULL CHECK (day_idx >= 1),
    focus        TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending','filled','missed','skipped')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (programme_id, week_idx, day_idx)
);

-- -----------------------------------------------------------------------------
-- Session rosters: the built session artefact (was `workout_plans`)
--
-- Output of /nextworkout as 'draft'; becomes 'active' and bound to a session
-- when the user starts executing it (guided mode), then 'completed' on session
-- end. Prescription only — never a log. v1's 'proposed' status is spelled
-- 'draft' here so rosters and programmes share one `LifecycleStatus`.
--
-- `programme_slot_id` NULL is the ad-hoc case and stays first-class: designing
-- a roster never sets it, and binding to a slot is a separate, optional step.
--
-- `override_note` is a one-off the user voiced mid-workout ("no bench today").
-- It lives on the roster so it expires when the roster completes, is abandoned
-- or is superseded — durable preferences go to `philosophies` instead.
-- -----------------------------------------------------------------------------
CREATE TABLE session_rosters (
    id                INTEGER PRIMARY KEY,
    user_id           INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title             TEXT NOT NULL,
    rationale         TEXT,
    philosophy_id     INTEGER REFERENCES philosophies(id),
    status            TEXT NOT NULL DEFAULT 'draft'
                          CHECK (status IN ('draft','active','completed','abandoned')),
    session_id        INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    programme_slot_id INTEGER REFERENCES programme_slots(id) ON DELETE SET NULL,
    override_note     TEXT,
    created_at        TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_session_rosters_user   ON session_rosters(user_id, created_at);
CREATE INDEX idx_session_rosters_active ON session_rosters(user_id, status);
CREATE INDEX idx_session_rosters_slot   ON session_rosters(programme_slot_id) WHERE programme_slot_id IS NOT NULL;

-- -----------------------------------------------------------------------------
-- Prescribed exercises within a roster, ordered (was `workout_plan_exercises`)
--
-- The only per-exercise prescription table in the schema. (target_count,
-- target_value) style fields mirror `sets`: use target_reps + target_weight_kg
-- for the common weight_reps case, target_secs for timed work.
-- -----------------------------------------------------------------------------
CREATE TABLE roster_exercises (
    id               INTEGER PRIMARY KEY,
    roster_id        INTEGER NOT NULL REFERENCES session_rosters(id) ON DELETE CASCADE,
    exercise_type_id INTEGER NOT NULL REFERENCES exercise_types(id),
    order_idx        INTEGER NOT NULL DEFAULT 0,
    target_sets      INTEGER,
    target_reps      INTEGER,
    target_weight_kg REAL,
    target_secs      INTEGER,
    notes            TEXT
);

CREATE INDEX idx_roster_exercises_roster ON roster_exercises(roster_id, order_idx);

-- -----------------------------------------------------------------------------
-- Session reviews
--
-- One review per session (`session_id` is UNIQUE); regenerating replaces the
-- row. `kind` is 'summary' for the deterministic ad-hoc review (no LLM call) or
-- 'report' for the programme-mode report with LLM commentary. `roster_id` is
-- NULL for a session logged without a roster.
--
-- `body` is the schema's ONLY JSON column, and deliberately so: a review is a
-- snapshot of what was true when it was generated — a later edit to a set must
-- not rewrite history — and no SQL query needs its internals. Chart series are
-- recomputed live from the underlying tables rather than read out of here.
-- -----------------------------------------------------------------------------
CREATE TABLE session_reviews (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    roster_id  INTEGER REFERENCES session_rosters(id) ON DELETE SET NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('summary','report')),
    body       TEXT NOT NULL,  -- serde-JSON SessionReview snapshot
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_session_reviews_user ON session_reviews(user_id, created_at);

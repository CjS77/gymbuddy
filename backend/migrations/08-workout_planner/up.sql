-- =============================================================================
-- Workout planner: distilled training philosophy, the multi-turn interview that
-- builds it, and generated (but unlogged) workout plans.
--
-- The planner leans on the LLM's intelligence rather than stored workout
-- variations: a plan is designed on demand from the philosophy + recent history
-- + injuries + goals. These tables only persist the philosophy and the designed
-- plan; logging sets stays on the existing sessions/exercise_entry/sets path.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- Append-only distilled philosophy. The most recent row per user is the active
-- one. Equipment is captured as free text inside `content` (no equipment table).
-- `source`: 'interview' (built via /philosophy), 'note' (a durable preference
-- appended mid-workout), 'import' (reserved).
-- -----------------------------------------------------------------------------
CREATE TABLE workout_philosophy (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    content    TEXT NOT NULL,
    source     TEXT NOT NULL DEFAULT 'interview' CHECK (source IN ('interview', 'note', 'import')),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_philosophy_user ON workout_philosophy(user_id, created_at);

-- -----------------------------------------------------------------------------
-- Per-(user, platform) interview mode. A row's presence means the user is in a
-- multi-turn interview; its absence means normal operation. `draft` accumulates
-- the philosophy-so-far across turns; `turns` bounds the interview length. Kept
-- in a table (not smuggled into message text) so it survives history pruning and
-- /clear.
-- -----------------------------------------------------------------------------
CREATE TABLE interview_state (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform   TEXT NOT NULL,
    mode       TEXT NOT NULL CHECK (mode IN ('philosophy')),
    draft      TEXT NOT NULL DEFAULT '',
    turns      INTEGER NOT NULL DEFAULT 0,
    started_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, platform)
);

-- -----------------------------------------------------------------------------
-- A generated workout plan. Output of /nextworkout (status 'proposed'); becomes
-- 'active' and bound to a session when the user starts executing it (guided
-- mode), then 'completed' on session end. Prescription only -- never a log.
-- -----------------------------------------------------------------------------
CREATE TABLE workout_plans (
    id            INTEGER PRIMARY KEY,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    rationale     TEXT,
    philosophy_id INTEGER REFERENCES workout_philosophy(id),
    status        TEXT NOT NULL DEFAULT 'proposed'
                      CHECK (status IN ('proposed', 'active', 'completed', 'abandoned')),
    session_id    INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_workout_plans_user   ON workout_plans(user_id, created_at);
CREATE INDEX idx_workout_plans_active ON workout_plans(user_id, status);

-- -----------------------------------------------------------------------------
-- Prescribed exercises within a plan, ordered. (target_count, target_value) is
-- interpreted via the exercise's measurement type, mirroring `sets`; for the
-- common weight_reps case use target_reps + target_weight_kg, with target_secs
-- for timed work.
-- -----------------------------------------------------------------------------
CREATE TABLE workout_plan_exercises (
    id               INTEGER PRIMARY KEY,
    plan_id          INTEGER NOT NULL REFERENCES workout_plans(id) ON DELETE CASCADE,
    exercise_type_id INTEGER NOT NULL REFERENCES exercise_types(id),
    order_idx        INTEGER NOT NULL DEFAULT 0,
    target_sets      INTEGER,
    target_reps      INTEGER,
    target_weight_kg REAL,
    target_secs      INTEGER,
    notes            TEXT
);

CREATE INDEX idx_workout_plan_exercises_plan ON workout_plan_exercises(plan_id, order_idx);

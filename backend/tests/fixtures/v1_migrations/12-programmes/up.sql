-- =============================================================================
-- Long-term training programmes: the skeleton ad-hoc sessions can slot into.
--
-- A programme is a skeleton, not a script: it persists the goals served, dates,
-- split, mesocycle block structure and a progression policy, while each session
-- keeps being designed on demand against it. `split` and `progression_policy`
-- are free text -- the LLM reads them; no query looks inside. The join back
-- runs from a designed workout_plan to the slot it filled; plans with
-- program_slot_id NULL are the ad-hoc case and stay first-class.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- The programme itself. `status` starts at 'draft' (being designed), becomes
-- 'active' when the user commits to it -- at most one active programme per
-- user, enforced in the DAO the way create_plan supersedes proposals -- and
-- ends 'completed' or 'abandoned'.
-- -----------------------------------------------------------------------------
CREATE TABLE programs (
    id                 INTEGER PRIMARY KEY,
    user_id            INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title              TEXT NOT NULL,
    start_date         TEXT NOT NULL,
    target_end_date    TEXT,
    days_per_week      INTEGER NOT NULL CHECK (days_per_week BETWEEN 1 AND 7),
    split              TEXT NOT NULL,
    progression_policy TEXT NOT NULL,
    status             TEXT NOT NULL DEFAULT 'draft'
                           CHECK (status IN ('draft', 'active', 'completed', 'abandoned')),
    created_at         TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at         TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_programs_user   ON programs(user_id, created_at);
CREATE INDEX idx_programs_status ON programs(user_id, status);

-- -----------------------------------------------------------------------------
-- The goals a programme serves: a many-to-many join onto the generalised
-- goals table. A goal outlives any one programme, so deleting either side
-- just unlinks.
-- -----------------------------------------------------------------------------
CREATE TABLE program_goals (
    program_id INTEGER NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    goal_id    INTEGER NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    PRIMARY KEY (program_id, goal_id)
);

-- -----------------------------------------------------------------------------
-- Mesocycle blocks: an inclusive 1-based week range with an intent, e.g.
-- weeks 1-4 'hypertrophy', weeks 5-6 'deload'. This is what makes sessions
-- build on one another rather than repeat: the designer reads the block the
-- current week falls in and progresses within it.
-- -----------------------------------------------------------------------------
CREATE TABLE program_blocks (
    id         INTEGER PRIMARY KEY,
    program_id INTEGER NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    start_week INTEGER NOT NULL CHECK (start_week >= 1),
    end_week   INTEGER NOT NULL CHECK (end_week >= start_week),
    focus      TEXT NOT NULL,
    notes      TEXT
);

CREATE INDEX idx_program_blocks_program ON program_blocks(program_id, start_week);

-- -----------------------------------------------------------------------------
-- The week/day grid. `week_idx` is 1-based from the programme start;
-- `day_idx` is the 1-based ordinal training day within the week (not a
-- calendar weekday), bounded by days_per_week. A slot starts 'pending',
-- becomes 'filled' when a designed plan binds to it, 'missed' when its week
-- passes untouched, or 'skipped' when deliberately dropped.
-- -----------------------------------------------------------------------------
CREATE TABLE program_slots (
    id         INTEGER PRIMARY KEY,
    program_id INTEGER NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    week_idx   INTEGER NOT NULL CHECK (week_idx >= 1),
    day_idx    INTEGER NOT NULL CHECK (day_idx >= 1),
    focus      TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending'
                   CHECK (status IN ('pending', 'filled', 'missed', 'skipped')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (program_id, week_idx, day_idx)
);

-- -----------------------------------------------------------------------------
-- The join from a designed session back to the programme slot it filled.
-- NULL is the ad-hoc case and stays first-class: create_plan never sets it,
-- binding to a slot is a separate, optional step.
-- -----------------------------------------------------------------------------
ALTER TABLE workout_plans ADD COLUMN program_slot_id INTEGER REFERENCES program_slots(id) ON DELETE SET NULL;

CREATE INDEX idx_workout_plans_slot ON workout_plans(program_slot_id) WHERE program_slot_id IS NOT NULL;

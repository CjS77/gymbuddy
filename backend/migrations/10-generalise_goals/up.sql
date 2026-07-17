-- Generalise per-user goals beyond a single exercise's single number.
--
-- exercise_goals bound every goal to one exercise_type (exercise_type_id NOT NULL)
-- and one target number, which cannot express weightloss, "train 4x a week", or
-- "run 5k under 25 min". Rebuild it as `goals`:
--   * kind             — strength / endurance / bodyweight / body_composition / habit
--   * exercise_type_id — now NULLable (metric goals aren't tied to one exercise)
--   * metric           — free-text quantity for non-exercise goals (e.g. bodyweight_kg)
--   * direction        — increase (bigger is better) / decrease (weightloss, faster time)
--   * priority         — ranks competing goals; higher wins
--   * target_date      — the deadline the user aims for (renamed from end_date)
--
-- SQLite cannot drop a NOT NULL or add CHECK constraints in place, so rebuild the
-- table and copy across. Existing rows migrate as strength / increase, preserving
-- their ids so any referencing data stays valid.

CREATE TABLE goals (
    id               INTEGER PRIMARY KEY,
    user_id          INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind             TEXT NOT NULL DEFAULT 'strength'
                         CHECK (kind IN ('strength','endurance','bodyweight','body_composition','habit')),
    exercise_type_id INTEGER REFERENCES exercise_types(id),
    metric           TEXT,
    target_value     REAL NOT NULL,
    direction        TEXT NOT NULL DEFAULT 'increase'
                         CHECK (direction IN ('increase','decrease')),
    priority         INTEGER NOT NULL DEFAULT 0,
    start_date       TEXT NOT NULL,
    target_date      TEXT,
    achieved         INTEGER NOT NULL DEFAULT 0,
    notes            TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now')),
    -- A goal is denominated either by an exercise or by a free-text metric.
    CHECK (exercise_type_id IS NOT NULL OR metric IS NOT NULL)
);

INSERT INTO goals (id, user_id, kind, exercise_type_id, metric, target_value, direction, priority,
                   start_date, target_date, achieved, notes, created_at, updated_at)
SELECT id, user_id, 'strength', exercise_type_id, NULL, target_value, 'increase', 0,
       start_date, end_date, achieved, notes, created_at, updated_at
FROM exercise_goals;

DROP TABLE exercise_goals;

CREATE INDEX idx_goals_user     ON goals(user_id, achieved);
CREATE INDEX idx_goals_priority ON goals(user_id, priority);

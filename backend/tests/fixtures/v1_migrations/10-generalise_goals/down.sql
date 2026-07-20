-- Revert `goals` back to the exercise-only `exercise_goals` table. Only goals that
-- name an exercise survive the round-trip; metric-only goals (which the old schema
-- could not represent) are dropped. target_date maps back to end_date.

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

INSERT INTO exercise_goals (id, user_id, exercise_type_id, target_value, start_date, end_date,
                            achieved, notes, created_at, updated_at)
SELECT id, user_id, exercise_type_id, target_value, start_date, target_date,
       achieved, notes, created_at, updated_at
FROM goals
WHERE exercise_type_id IS NOT NULL;

DROP TABLE goals;

CREATE INDEX idx_goals_user ON exercise_goals(user_id, achieved);

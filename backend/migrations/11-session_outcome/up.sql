-- =============================================================================
-- Session-level outcome: a structured verdict on how the whole session went,
-- distinct from free-form `notes` (which no query reads).
--
-- `overall_effort` is proposed at session end by distilling the last set of
-- each exercise (the same easy/medium/hard/failure vocabulary as
-- `sets.perceived_difficulty`), so the user can simply agree with it -- or
-- override it, add how the session felt, and flag that it was cut short and
-- why. Read by the workout designer's feedback loop and the weekly digest.
-- =============================================================================

ALTER TABLE sessions ADD COLUMN overall_effort TEXT
    CHECK (overall_effort IN ('easy', 'medium', 'hard', 'failure'));
ALTER TABLE sessions ADD COLUMN felt TEXT
    CHECK (felt IN ('great', 'good', 'ok', 'rough'));
ALTER TABLE sessions ADD COLUMN cut_short INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN cut_short_reason TEXT;

-- Temporary, single-session workout overrides.
--
-- A one-off the user voices mid-workout ("no bench today, let's do flys instead")
-- applies ONLY to the plan in flight. It must never leak into the philosophy
-- (which would ban the movement forever) nor survive into the next design. It
-- lives on the plan row itself, so it expires naturally when the plan completes,
-- is abandoned, or is superseded by a fresh /nextworkout design. Durable
-- preferences keep going to workout_philosophy via append_philosophy_note.
ALTER TABLE workout_plans ADD COLUMN override_note TEXT;

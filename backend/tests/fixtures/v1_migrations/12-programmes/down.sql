-- Remove the programme skeleton. The plan-side column goes first (its index
-- would block the DROP COLUMN, so it goes before that); every plan degrades to
-- the ad-hoc case, which loses only the slot join, never the plan.
DROP INDEX IF EXISTS idx_workout_plans_slot;
ALTER TABLE workout_plans DROP COLUMN program_slot_id;

DROP TABLE IF EXISTS program_slots;
DROP TABLE IF EXISTS program_blocks;
DROP TABLE IF EXISTS program_goals;
DROP TABLE IF EXISTS programs;

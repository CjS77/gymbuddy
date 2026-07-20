-- Unseed the taxonomy. Deepest level first: `exercise_types.parent_id` is
-- ON DELETE RESTRICT, so a parent cannot go before its children.
DELETE FROM exercise_types WHERE level = 'variation';
DELETE FROM exercise_types WHERE level = 'exercise';
DELETE FROM exercise_types WHERE level = 'specific_muscle';
DELETE FROM exercise_types WHERE level = 'muscle_group';

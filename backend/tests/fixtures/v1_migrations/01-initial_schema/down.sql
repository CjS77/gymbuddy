-- exercise_types has a self-referencing FK (ON DELETE RESTRICT). To drop the table
-- cleanly we first delete its rows in reverse-level order so no row refers to a row
-- being deleted in the same step.
DELETE FROM exercise_types WHERE level = 'variation';
DELETE FROM exercise_types WHERE level = 'exercise';
DELETE FROM exercise_types WHERE level = 'specific_muscle';
DELETE FROM exercise_types WHERE level = 'muscle_group';

DROP TABLE IF EXISTS conversation_history;
DROP TABLE IF EXISTS health_entries;
DROP TABLE IF EXISTS schedule_exercises;
DROP TABLE IF EXISTS schedules;
DROP TABLE IF EXISTS sets;
DROP TABLE IF EXISTS exercise_entry;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS exercise_goals;
DROP TABLE IF EXISTS group_members;
DROP TABLE IF EXISTS groups;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS exercise_types;
DROP TABLE IF EXISTS measurement_types;

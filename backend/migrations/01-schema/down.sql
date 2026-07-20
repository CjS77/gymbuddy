-- Tear the v2 baseline down. Reverse dependency order, so foreign keys never
-- block a drop.
DROP TABLE IF EXISTS session_reviews;
DROP TABLE IF EXISTS roster_exercises;
DROP TABLE IF EXISTS session_rosters;
DROP TABLE IF EXISTS programme_slots;
DROP TABLE IF EXISTS programme_blocks;
DROP TABLE IF EXISTS programme_goals;
DROP TABLE IF EXISTS programmes;
DROP TABLE IF EXISTS interview_states;
DROP TABLE IF EXISTS philosophies;
DROP TABLE IF EXISTS body_metrics;
DROP TABLE IF EXISTS conversation_history;
DROP TABLE IF EXISTS health_entries;
DROP TABLE IF EXISTS sets;
DROP TABLE IF EXISTS exercise_entries;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS goals;
DROP TABLE IF EXISTS metrics;
DROP TABLE IF EXISTS group_members;
DROP TABLE IF EXISTS groups;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS exercise_types;
DROP TABLE IF EXISTS measurement_types;

-- ============================================================================
-- The canonical seeded schema v1 database.
--
-- Applied on top of `v1_migrations/`, this fills every table the exporter reads
-- and every CHECK-constrained value those tables accept. It is the input to the
-- export snapshot and to the per-table count invariants, so it is deliberately
-- exhaustive rather than realistic: an arm nobody seeds is an arm the exporter
-- can drop in silence.
--
-- Ids are explicit throughout. The snapshot compares exported ids verbatim, and
-- autoincrement would make the expected output depend on insertion order.
--
-- Three users, each proving something different:
--   1 Alice — the maximal tree; every table, every enum value, every NULL arm.
--   2 Bob   — rows in `users` and nowhere else; an empty tree is still a tree.
--   3 Carol — a second populated tree, so per-user scoping is tested in both
--             directions (Alice's rows must not reach Carol's tree, nor Carol's
--             Alice's). A single populated user cannot catch an unscoped query.
--
-- Catalogue ids referenced here, with the name each resolves to:
--     7 Cardio (muscle_group, NO PARENT — the only ExerciseRef::parent = None arm)
--  1000 Bench Press (parent Pectoral)
--  5000 Squat (parent Quadriceps)
--  6020 Plank (parent Transverse Abdominis)
--  7000 Running (parent Cardiovascular)
--  7005 Padel (parent Cardiovascular)
-- 10000 Flat Barbell Bench Press (parent Bench Press)
-- 20000 Overhand Pull-Up (parent Pull-Up)
-- 60030 Front Plank (parent Plank)
--
-- Measurement type ids: 1 weight_reps, 2 time_based, 3 distance_based,
-- 4 level_based, 5 score_based. All five are used by some set below.
-- ============================================================================

-- ----------------------------------------------------------------------------
-- users. Alice carries every optional column; Bob carries none of them.
-- ----------------------------------------------------------------------------
INSERT INTO users (id, name, telegram_id, signal_id, timezone, created_at, updated_at, beta_tester, pubkey, timers_enabled)
VALUES (1, 'Alice', 'tg-alice', 'signal-alice', 'Europe/Lisbon', '2026-01-01 09:00:00', '2026-02-01 09:00:00', 1, 'pk-alice', 0),
       (2, 'Bob', NULL, NULL, 'UTC', '2026-01-02 09:00:00', '2026-01-02 09:00:00', 0, NULL, 1),
       (3, 'Carol', 'tg-carol', NULL, 'America/New_York', '2026-01-03 09:00:00', '2026-01-03 09:00:00', 0, 'pk-carol', 1);

-- ----------------------------------------------------------------------------
-- groups + memberships. Groups are global; membership is per user, and every
-- `level` value appears.
-- ----------------------------------------------------------------------------
INSERT INTO groups (id, name, description, created_at)
VALUES (1, 'beta', 'Beta testers', '2026-01-01 08:00:00'),
       (2, 'coaches', NULL, '2026-01-01 08:05:00');

INSERT INTO group_members (user_id, group_id, level, granted_at)
VALUES (1, 1, 'admin', '2026-01-01 08:30:00'),
       (1, 2, 'read', '2026-01-01 08:31:00'),
       (3, 2, 'write', '2026-01-03 09:30:00');

-- ----------------------------------------------------------------------------
-- workout_philosophy → philosophies. All three `source` values.
-- ----------------------------------------------------------------------------
INSERT INTO workout_philosophy (id, user_id, content, source, created_at)
VALUES (7, 1, 'Compound lifts first.', 'interview', '2026-01-03 10:00:00'),
       (8, 1, 'It''s the last set that counts — "leave one in the tank" is for cowards.', 'note', '2026-01-06 10:00:00'),
       (9, 3, 'Volume over intensity.', 'import', '2026-01-04 10:00:00');

-- ----------------------------------------------------------------------------
-- interview_state → interview_states. Keyed (user_id, platform); `mode` has a
-- single legal value.
-- ----------------------------------------------------------------------------
INSERT INTO interview_state (user_id, platform, mode, draft, turns, started_at)
VALUES (1, 'confide', 'philosophy', '', 0, '2026-01-04 10:05:00'),
       (1, 'telegram', 'philosophy', 'half a philosophy', 2, '2026-01-04 10:00:00'),
       (3, 'web', 'philosophy', 'starting out', 1, '2026-01-05 10:00:00');

-- ----------------------------------------------------------------------------
-- goals. Every `kind`, both `direction`s, and all three denominations:
-- exercise-bound (11, 13, 16), metric-bound (12, 14), and exercise-bound on a
-- parentless muscle_group (15).
-- ----------------------------------------------------------------------------
INSERT INTO goals (id, user_id, kind, exercise_type_id, metric, target_value, direction, priority,
                   start_date, target_date, achieved, notes, created_at, updated_at)
VALUES (11, 1, 'strength', 1000, NULL, 120.0, 'increase', 5, '2026-01-01', '2026-06-01', 0, 'bench 120', '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (12, 1, 'bodyweight', NULL, 'bodyweight_kg', 78.0, 'decrease', 3, '2026-01-01', NULL, 1, NULL, '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (13, 1, 'endurance', 7000, NULL, 10.0, 'increase', 2, '2026-01-01', '2026-09-01', 0, '10k without walking', '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (14, 1, 'body_composition', NULL, 'body_fat_pct', 15.0, 'decrease', 1, '2026-01-01', '2026-12-01', 0, NULL, '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (15, 1, 'habit', 7, NULL, 3.0, 'increase', 0, '2026-01-01', NULL, 0, 'three cardio sessions a week', '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (16, 3, 'strength', 5000, NULL, 140.0, 'increase', 4, '2026-01-05', NULL, 0, NULL, '2026-01-05 09:00:00', '2026-01-05 09:00:00');

-- ----------------------------------------------------------------------------
-- sessions. Between them: every `overall_effort` and `felt` value plus the NULL
-- arm of each, both `cut_short` states, and all three shapes of `notes` —
--   21  `plan:<name>` + body   → intent is the body, name archived
--   22  free text, no sentinel → intent verbatim
--   23  `plan:<name>` alone    → intent NULL, name archived
--   24  NULL                   → intent NULL, nothing archived
-- ----------------------------------------------------------------------------
INSERT INTO sessions (id, user_id, started_at, ended_at, notes, overall_effort, felt, cut_short, cut_short_reason)
VALUES (21, 1, '2026-02-01 17:00:00', '2026-02-01 18:15:00', 'plan:Push Day' || char(10) || 'felt strong', 'hard', 'great', 0, NULL),
       (22, 1, '2026-02-03 17:00:00', NULL, 'no sentinel here', NULL, NULL, 1, 'gym closed'),
       (23, 1, '2026-02-05 17:00:00', '2026-02-05 18:30:00', 'plan:Leg Day', 'failure', 'rough', 0, NULL),
       (24, 1, '2026-02-07 17:00:00', '2026-02-07 17:45:00', NULL, 'easy', 'good', 0, NULL),
       (25, 1, '2026-02-09 17:00:00', '2026-02-09 18:00:00', 'unicode check: 100kg — pesado, ~5 reps <ok> \ done', 'medium', 'ok', 0, NULL),
       (26, 3, '2026-02-02 07:00:00', '2026-02-02 08:00:00', 'morning squats', 'medium', 'good', 0, NULL);

-- ----------------------------------------------------------------------------
-- exercise_entry. Entries 33, 36 and 38 have session_id NULL — sets logged
-- outside any session. They hang off the user, not a session, and are the arm
-- most easily dropped without anyone noticing.
-- Entry 35 deliberately has no sets at all.
-- Session 24 deliberately has no entries at all.
-- ----------------------------------------------------------------------------
INSERT INTO exercise_entry (id, user_id, session_id, start_timestamp, end_timestamp, comment)
VALUES (31, 1, 21, '2026-02-01 17:05:00', '2026-02-01 17:30:00', 'warmed up'),
       (32, 1, 21, '2026-02-01 17:35:00', NULL, NULL),
       (33, 1, NULL, '2026-01-15 12:00:00', NULL, 'logged outside a session'),
       (34, 1, 23, '2026-02-05 17:10:00', '2026-02-05 17:50:00', NULL),
       (35, 1, 23, '2026-02-05 18:00:00', NULL, 'started, logged nothing'),
       (36, 1, NULL, '2026-01-22 12:00:00', '2026-01-22 12:20:00', NULL),
       (37, 3, 26, '2026-02-02 07:05:00', '2026-02-02 07:40:00', NULL),
       (38, 3, NULL, '2026-01-28 18:00:00', NULL, 'carol logged a set on her own');

-- ----------------------------------------------------------------------------
-- sets. All five measurement types and all four `perceived_difficulty` values.
-- The (count, value) polymorphism: weight_reps fills both, everything else
-- leaves count NULL.
-- ----------------------------------------------------------------------------
INSERT INTO sets (id, exercise_entry_id, exercise_type_id, order_idx, measurement_type_id, count, value, perceived_difficulty, comment, logged_at)
VALUES (41, 31, 10000, 0, 1, 8, 80.0, 'medium', 'smooth', '2026-02-01 17:10:00'),
       (42, 31, 10000, 1, 1, 6, 90.0, 'hard', NULL, '2026-02-01 17:20:00'),
       (43, 32, 6020, 0, 2, NULL, 60.0, 'easy', NULL, '2026-02-01 17:40:00'),
       (44, 33, 5000, 0, 1, 5, 100.0, 'failure', NULL, '2026-01-15 12:05:00'),
       (45, 34, 7000, 0, 3, NULL, 5.2, 'medium', '5.2 km', '2026-02-05 17:15:00'),
       (46, 34, 7005, 1, 5, NULL, 21.0, 'hard', 'won 21-19', '2026-02-05 17:45:00'),
       (47, 36, 60030, 0, 4, NULL, 3.0, 'easy', 'level 3 hold', '2026-01-22 12:10:00'),
       (48, 37, 5000, 0, 1, 10, 60.0, 'medium', NULL, '2026-02-02 07:10:00'),
       (49, 38, 20000, 0, 1, 4, 0.0, 'hard', 'bodyweight only', '2026-01-28 18:05:00');

-- ----------------------------------------------------------------------------
-- programs → programmes. All four `status` values across the four rows; 51 is
-- the fully-populated one (goals + blocks + slots).
-- ----------------------------------------------------------------------------
INSERT INTO programs (id, user_id, title, start_date, target_end_date, days_per_week, split, progression_policy, status, created_at, updated_at)
VALUES (51, 1, 'Winter Strength', '2026-01-06', '2026-03-30', 4, 'upper/lower', 'double progression', 'active', '2026-01-05 09:00:00', '2026-01-05 09:00:00'),
       (52, 1, 'Autumn Base', '2025-09-01', '2025-11-30', 3, 'full body', 'linear', 'completed', '2025-08-25 09:00:00', '2025-12-01 09:00:00'),
       (53, 1, 'Abandoned Experiment', '2025-06-01', NULL, 6, 'bro split', 'none', 'abandoned', '2025-05-25 09:00:00', '2025-06-20 09:00:00'),
       (54, 3, 'Couch to Squat', '2026-01-10', NULL, 2, 'full body', 'linear', 'draft', '2026-01-05 09:00:00', '2026-01-05 09:00:00');

INSERT INTO program_goals (program_id, goal_id)
VALUES (51, 11), (51, 13), (52, 12), (54, 16);

INSERT INTO program_blocks (id, program_id, start_week, end_week, focus, notes)
VALUES (61, 51, 1, 4, 'hypertrophy', 'build volume'),
       (62, 51, 5, 6, 'deload', NULL),
       (63, 51, 7, 12, 'strength', 'heavy triples'),
       (64, 52, 1, 12, 'base', NULL);

-- Every `status` value a slot can hold.
INSERT INTO program_slots (id, program_id, week_idx, day_idx, focus, status, updated_at)
VALUES (71, 51, 1, 1, 'push', 'filled', '2026-02-01 18:15:00'),
       (72, 51, 1, 2, 'pull', 'pending', '2026-01-05 09:00:00'),
       (73, 51, 1, 3, 'legs', 'missed', '2026-01-12 09:00:00'),
       (74, 51, 2, 1, 'push', 'skipped', '2026-01-13 09:00:00'),
       (75, 54, 1, 1, 'full body', 'pending', '2026-01-05 09:00:00');

-- ----------------------------------------------------------------------------
-- workout_plans → session_rosters. Every `status` value, including the
-- `proposed` → `draft` rename. 81 is a roster with no exercises; 82 is bound to
-- both a session and a programme slot; 84 carries an override note.
-- ----------------------------------------------------------------------------
INSERT INTO workout_plans (id, user_id, title, rationale, philosophy_id, status, session_id, created_at, updated_at, override_note, program_slot_id)
VALUES (81, 1, 'Next Pull', 'lats need volume', 7, 'proposed', NULL, '2026-02-04 08:00:00', '2026-02-04 08:00:00', NULL, NULL),
       (82, 1, 'Push Day', 'chest focus', 7, 'completed', 21, '2026-02-01 16:00:00', '2026-02-01 18:15:00', 'no dips today', 71),
       (83, 1, 'Leg Day', NULL, 8, 'active', 23, '2026-02-05 16:00:00', '2026-02-05 18:30:00', NULL, NULL),
       (84, 1, 'Deload Week', 'too much fatigue', NULL, 'abandoned', NULL, '2026-01-12 08:00:00', '2026-01-14 08:00:00', 'shoulder still sore', NULL),
       (85, 3, 'First Session', 'start light', 9, 'proposed', NULL, '2026-01-06 08:00:00', '2026-01-06 08:00:00', NULL, NULL);

INSERT INTO workout_plan_exercises (id, plan_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg, target_secs, notes)
VALUES (91, 82, 10000, 0, 3, 8, 80.0, NULL, 'controlled tempo'),
       (92, 82, 6020, 1, 3, NULL, NULL, 60, NULL),
       (93, 82, 20000, 2, 4, 6, NULL, NULL, NULL),
       (94, 83, 5000, 0, 5, 5, 110.0, NULL, 'belt on the last two'),
       (95, 85, 5000, 0, 3, 10, 20.0, NULL, NULL);

-- ----------------------------------------------------------------------------
-- schedules + schedule_exercises. Dropped by schema v2 and archived under
-- `legacy`; both `reminder_type` values and both `enabled` states appear.
-- Schedule 102 has no exercises.
-- ----------------------------------------------------------------------------
INSERT INTO schedules (id, user_id, name, cron_expr, reminder_type, reminder_notice_mins, enabled, created_at, updated_at)
VALUES (101, 1, 'Push Day', '0 17 * * 1', 'voice', 45, 1, '2026-01-01 09:00:00', '2026-01-01 09:00:00'),
       (102, 1, 'Leg Day', '0 17 * * 4', 'text', 30, 0, '2026-01-01 09:00:00', '2026-01-08 09:00:00');

INSERT INTO schedule_exercises (schedule_id, exercise_type_id, order_idx, target_sets, target_reps, target_weight_kg)
VALUES (101, 10000, 0, 3, 8, 80.0),
       (101, 6020, 1, 3, NULL, NULL);

-- ----------------------------------------------------------------------------
-- health_entries. Every `entry_type` and every `severity`.
-- ----------------------------------------------------------------------------
INSERT INTO health_entries (id, user_id, entry_type, body_part, severity, description, started_at, resolved_at, notes, updated_at)
VALUES (111, 1, 'injury', 'shoulder', 'severe', 'tweaked left shoulder', '2026-01-20 09:00:00', NULL, 'avoid overhead', '2026-01-20 09:00:00'),
       (112, 1, 'illness', NULL, 'moderate', 'flu', '2026-01-25 09:00:00', '2026-01-29 09:00:00', NULL, '2026-01-29 09:00:00'),
       (113, 1, 'wellbeing', NULL, 'mild', 'sleeping badly', '2026-02-02 09:00:00', NULL, 'four hours a night', '2026-02-02 09:00:00'),
       (114, 3, 'injury', 'knee', 'mild', 'sore knee after squats', '2026-02-03 09:00:00', '2026-02-06 09:00:00', NULL, '2026-02-06 09:00:00');

-- ----------------------------------------------------------------------------
-- body_metrics. Long-shaped: several metrics per user, a series for one of
-- them, and the same metric names goals use so the two join.
-- ----------------------------------------------------------------------------
INSERT INTO body_metrics (id, user_id, metric, value, measured_at)
VALUES (121, 1, 'bodyweight_kg', 82.5, '2026-01-10 07:00:00'),
       (122, 1, 'bodyweight_kg', 81.9, '2026-02-10 07:00:00'),
       (123, 1, 'body_fat_pct', 19.4, '2026-01-10 07:00:00'),
       (124, 1, 'waist_cm', 86.0, '2026-01-10 07:00:00'),
       (125, 1, 'resting_hr_bpm', 58.0, '2026-01-10 07:00:00'),
       (126, 3, 'bodyweight_kg', 64.2, '2026-01-11 07:00:00');

-- ----------------------------------------------------------------------------
-- conversation_history. Every `platform` (including 'confide', added by
-- migration 05) and every `role`; both `exclude_from_context` states.
-- ----------------------------------------------------------------------------
INSERT INTO conversation_history (id, user_id, platform, role, content, timestamp, exclude_from_context)
VALUES (131, 1, 'confide', 'user', 'logged 8 at 80', '2026-02-01 17:10:00', 0),
       (132, 1, 'telegram', 'assistant', 'nice work', '2026-02-01 17:10:05', 1),
       (133, 1, 'signal', 'system', 'session summary generated', '2026-02-01 18:16:00', 0),
       (134, 1, 'web', 'user', 'quotes "inside" and a backslash \ survive', '2026-02-03 17:00:00', 0),
       (135, 3, 'telegram', 'user', 'how heavy should I squat?', '2026-02-02 07:00:00', 0);

-- =============================================================================
-- Catalogue additions: exercises added since the baseline taxonomy
-- =============================================================================
-- Applied at every startup, after the migrations, by `db::catalogue`. Adding an
-- exercise is therefore a one-line data change that ships with the next deploy
-- and needs no migration, no version bump, and no thought about which databases
-- have already seen it.
--
-- RULES FOR THIS FILE
--
--  * Every statement is `INSERT OR IGNORE`. Re-running must be a no-op, because
--    it runs again on every single startup.
--  * Never write an explicit `id`. Idempotency comes from
--    `UNIQUE (parent_id, name)` on `exercise_types`; ids are assigned by SQLite
--    and differ between databases, which is exactly why the dump format refers
--    to exercises by name.
--  * `parent_id` IS explicit, taken from the stable ranges in migration
--    02-catalogue-seed. A typo there fails loudly on the foreign key rather
--    than silently inserting nothing, which is why this is not a subselect.
--  * `aliases` is a comma-separated list of the spoken and typed variants, so
--    the assistant's matcher resolves them in its alias stage instead of
--    falling through to fuzzy matching.
-- =============================================================================

-- Latissimus Dorsi (parent 200). Both rows arrived in schema v1 as a migration
-- apiece — mig 04 and mig 06 — which is the pattern this file exists to end.
INSERT OR IGNORE INTO exercise_types (name, parent_id, level, measurement_type_id, purpose, aliases) VALUES
    ('Bent Over Barbell Row', 200, 'exercise', 1, 'strength',
     'barbell row,bent over row,bent-over row,bent-over barbell row,bb row'),
    ('One Arm Dumbbell Row', 200, 'exercise', 1, 'strength',
     'one arm row,single arm row,single arm dumbbell row,db row,one arm dumbell row');

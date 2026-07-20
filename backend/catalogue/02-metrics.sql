-- =============================================================================
-- Catalogue additions: the canonical body metrics
-- =============================================================================
-- `metrics` is reference data on the same terms as the exercise taxonomy, and
-- for the same reason: v1 made every new metric a schema decision by spelling it
-- as free text in two places. These are the names `canonical_body_metric`
-- normalises to, seeded with their units so a renderer never has to guess one.
--
-- A metric the user invents is still first-class: `get_or_create_metric` inserts
-- it on demand (with a NULL unit, since the name carries no known suffix). This
-- file only pre-declares the ones the code knows how to canonicalise, so their
-- units are right from the first weigh-in.
--
-- Idempotency comes from `metrics.name UNIQUE`; ids are never written here.
-- =============================================================================

INSERT OR IGNORE INTO metrics (name, unit) VALUES
    ('bodyweight_kg',  'kg'),
    ('body_fat_pct',   '%'),
    ('waist_cm',       'cm'),
    ('resting_hr_bpm', 'bpm');

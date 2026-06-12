-- Add Bent Over Barbell Row as a Latissimus Dorsi exercise (parent 200).
-- Aliases cover the common spoken/typed variants so the assistant matcher
-- resolves them via the alias stage without relying on Levenshtein.
INSERT INTO exercise_types (id, name, parent_id, level, measurement_type_id, purpose, aliases) VALUES
    (2003, 'Bent Over Barbell Row', 200, 'exercise', 1, 'strength',
     'barbell row,bent over row,bent-over row,bent-over barbell row,bb row');

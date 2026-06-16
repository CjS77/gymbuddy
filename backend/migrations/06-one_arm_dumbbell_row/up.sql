-- Add One Arm Dumbbell Row as a Latissimus Dorsi exercise (parent 200).
-- Aliases cover the common spoken/typed variants so the assistant matcher
-- resolves them via the alias stage without relying on Levenshtein.
INSERT INTO exercise_types (id, name, parent_id, level, measurement_type_id, purpose, aliases) VALUES
    (2004, 'One Arm Dumbbell Row', 200, 'exercise', 1, 'strength',
     'one arm row,single arm row,single arm dumbbell row,db row,one arm dumbell row');

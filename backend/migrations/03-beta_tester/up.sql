-- Gates the /feedback slash command and any future beta-only features.
ALTER TABLE users ADD COLUMN beta_tester INTEGER NOT NULL DEFAULT 0;

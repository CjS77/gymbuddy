-- Per-user rest-timer preference.
--
-- When on (the default), the assistant arms an inter-set rest countdown after each
-- logged set. Lives on the user (not the session) so it persists across workouts;
-- toggled at runtime with /timers (Telegram) or the TUI sidebar switch (Ctrl+T).
-- New users inherit the [rest_timer] default_enabled config value at registration.
ALTER TABLE users ADD COLUMN timers_enabled INTEGER NOT NULL DEFAULT 1;

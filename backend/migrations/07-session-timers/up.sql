-- Per-session rest-timer toggle.
--
-- When on (the default), the assistant arms an inter-set rest countdown after each
-- logged set. Toggled at runtime with /timers (Telegram) or the TUI sidebar switch.
ALTER TABLE sessions ADD COLUMN timers_enabled INTEGER NOT NULL DEFAULT 1;

//! Prompt recall: an in-memory ring of submitted chat lines, plus its on-disk form.
//!
//! The ring itself is pure so [`crate::app::App`] stays free of I/O; `main` loads
//! it at startup and writes it back on the way out.

use std::path::{Path, PathBuf};

/// Most lines kept, in memory and on disk. Old entries fall off the front.
const MAX_ENTRIES: usize = 500;

/// Submitted chat lines and the cursor walking them.
///
/// `entries` runs oldest-first. `cursor` is `None` while the user is typing a
/// fresh line and `Some(i)` once they start recalling, so stepping forward past
/// the newest entry can restore what they had been typing.
#[derive(Default)]
pub struct History {
    entries: Vec<String>,
    cursor: Option<usize>,
    draft: String,
}

impl History {
    pub fn new(entries: Vec<String>) -> Self {
        Self { entries, cursor: None, draft: String::new() }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Record a submitted line, skipping blanks and consecutive duplicates.
    pub fn record(&mut self, line: &str) {
        if line.trim().is_empty() || self.entries.last().is_some_and(|last| last == line) {
            return;
        }
        self.entries.push(line.to_string());
        if self.entries.len() > MAX_ENTRIES {
            let excess = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..excess);
        }
    }

    /// Step back through the ring, stashing `draft` on the first step so the line
    /// in progress survives. `None` when there is nothing older to show.
    pub fn prev(&mut self, draft: String) -> Option<String> {
        let next = match self.cursor {
            None => self.entries.len().checked_sub(1)?,
            Some(0) => return None,
            Some(i) => i - 1,
        };
        if self.cursor.is_none() {
            self.draft = draft;
        }
        self.cursor = Some(next);
        self.entries.get(next).cloned()
    }

    /// Step forward through the ring. Stepping past the newest entry leaves recall
    /// and hands back the stashed draft.
    pub fn next(&mut self) -> Option<String> {
        match self.cursor {
            None => None,
            Some(i) if i + 1 < self.entries.len() => {
                self.cursor = Some(i + 1);
                self.entries.get(i + 1).cloned()
            }
            Some(_) => {
                self.cursor = None;
                Some(std::mem::take(&mut self.draft))
            }
        }
    }

    /// Leave recall mode and drop the stashed draft. Called on submit.
    pub fn reset_cursor(&mut self) {
        self.cursor = None;
        self.draft.clear();
    }
}

/// Where a given client's history lives: `data_dir()/gymbuddy/history/<pubkey>.txt`.
///
/// Keyed by pubkey so separate identities keep separate rings. This sits beside
/// the identity key itself (see `gymbuddy_client::default_identity_path`) — recall
/// history is state, not configuration. `None` when the platform has no data dir,
/// which simply means history won't persist.
pub fn path_for(pubkey: &str) -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("gymbuddy").join("history").join(format!("{pubkey}.txt")))
}

/// Read a history file, newest last. A missing, unreadable, or corrupt file is
/// empty history — recall is a convenience and must never hold up startup.
pub fn load(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<String> = text.lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect();
    let excess = lines.len().saturating_sub(MAX_ENTRIES);
    lines.into_iter().skip(excess).collect()
}

/// Write the ring back, newest last, creating the directory if needed.
pub fn save(path: &Path, entries: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let excess = entries.len().saturating_sub(MAX_ENTRIES);
    let body: String = entries[excess..].iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(path, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(lines: &[&str]) -> History {
        History::new(lines.iter().map(|l| l.to_string()).collect())
    }

    #[test]
    fn record_skips_blanks_and_consecutive_duplicates() {
        let mut h = History::default();
        h.record("squats");
        h.record("squats");
        h.record("");
        h.record("   ");
        h.record("bench");
        h.record("squats");
        assert_eq!(h.entries(), ["squats", "bench", "squats"]);
    }

    #[test]
    fn record_caps_the_ring() {
        let mut h = History::default();
        (0..MAX_ENTRIES + 10).for_each(|i| h.record(&format!("line {i}")));
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert_eq!(h.entries()[0], "line 10");
    }

    #[test]
    fn prev_walks_back_and_stops_at_oldest() {
        let mut h = history(&["one", "two"]);
        assert_eq!(h.prev(String::new()).as_deref(), Some("two"));
        assert_eq!(h.prev(String::new()).as_deref(), Some("one"));
        assert_eq!(h.prev(String::new()), None);
    }

    #[test]
    fn prev_on_empty_history_does_nothing() {
        let mut h = History::default();
        assert_eq!(h.prev("draft".into()), None);
    }

    #[test]
    fn down_past_newest_restores_the_draft() {
        let mut h = history(&["one", "two"]);
        assert_eq!(h.prev("half typed".into()).as_deref(), Some("two"));
        assert_eq!(h.next().as_deref(), Some("half typed"));
        // Past the newest we are back on the draft, so there is nothing further.
        assert_eq!(h.next(), None);
    }

    #[test]
    fn draft_is_stashed_only_on_the_first_step_back() {
        let mut h = history(&["one", "two"]);
        h.prev("half typed".into());
        // The second step back passes the recalled line, which must not overwrite
        // the stashed draft.
        h.prev("two".into());
        assert_eq!(h.next().as_deref(), Some("two"));
        assert_eq!(h.next().as_deref(), Some("half typed"));
    }

    #[test]
    fn recall_does_not_mutate_stored_entries() {
        let mut h = history(&["squats"]);
        h.prev(String::new());
        h.reset_cursor();
        assert_eq!(h.entries(), ["squats"]);
    }

    #[test]
    fn missing_file_is_empty_history() {
        assert!(load(Path::new("/nonexistent/gymbuddy/history.txt")).is_empty());
    }

    #[test]
    fn corrupt_file_is_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.txt");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(load(&path).is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        // A nested path also proves save creates the directory.
        let path = dir.path().join("history").join("abc.txt");
        let entries = vec!["squats 100kg".to_string(), "bench 80kg".to_string()];
        save(&path, &entries).unwrap();
        assert_eq!(load(&path), entries);
    }

    #[test]
    fn load_keeps_only_the_newest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.txt");
        let entries: Vec<String> = (0..MAX_ENTRIES + 10).map(|i| format!("line {i}")).collect();
        std::fs::write(&path, entries.join("\n")).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), MAX_ENTRIES);
        assert_eq!(loaded[0], "line 10");
    }
}

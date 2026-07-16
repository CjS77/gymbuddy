//! Tab completion for slash commands at the prompt.
//!
//! The candidate set is whatever the server advertised ([C2.1]) — this client has
//! no built-in list, because the set is per-user and only the server knows which
//! commands a given user may run.
//!
//! Pure state: [`crate::app::App`] feeds it the line and cursor and applies what
//! comes back.

/// Command names from the server, plus any in-flight cycle through them.
#[derive(Default)]
pub struct Completion {
    /// Advertised command names, in the order the server sent them.
    names: Vec<String>,
    /// The candidates a Tab turned up, once completing to their common prefix has
    /// stopped making progress. `None` until Tab hits an ambiguous prefix, and
    /// cleared by any other key.
    cycle: Option<Cycle>,
}

/// A walk through candidates that share the typed prefix.
struct Cycle {
    candidates: Vec<String>,
    /// Which candidate is in the line. `None` while the line holds their common
    /// prefix rather than any one of them.
    index: Option<usize>,
}

impl Completion {
    /// Adopt the server's advertised command set, dropping any cycle built from
    /// the previous one.
    pub fn set_commands(&mut self, names: impl IntoIterator<Item = String>) {
        self.names = names.into_iter().collect();
        self.cycle = None;
    }

    /// End the in-flight cycle. Called for every key except Tab, so the next Tab
    /// reads the line as it now stands rather than continuing a stale walk.
    pub fn reset(&mut self) {
        self.cycle = None;
    }

    /// Complete the command at the prompt, returning the new `(line, cursor)`.
    ///
    /// `None` when there is nothing to do: the line isn't a command, the cursor
    /// has left the first word, or nothing matches. A first Tab completes to the
    /// candidates' longest common prefix; once that adds nothing, repeated Tabs
    /// cycle the candidates.
    pub fn complete(&mut self, line: &str, cursor: usize) -> Option<(String, usize)> {
        let word = self.completable_word(line, cursor)?;
        let replacement = match self.cycle.as_mut() {
            Some(cycle) => cycle.advance(),
            None => self.begin(word)?,
        };
        let rest = &line[word.len()..];
        let cursor = replacement.chars().count();
        Some((format!("{replacement}{rest}"), cursor))
    }

    /// The first word, when it is a command prefix the cursor is still inside.
    ///
    /// Completion is deliberately confined to the command itself; argument values
    /// are a separate concern the server doesn't advertise.
    fn completable_word<'a>(&self, line: &'a str, cursor: usize) -> Option<&'a str> {
        if !line.starts_with('/') {
            return None;
        }
        // The line starts with `/`, so the first word starts at byte 0.
        let word = line.split_whitespace().next()?;
        (cursor <= word.chars().count()).then_some(word)
    }

    /// Open a cycle for `word`, returning what to put in the line.
    ///
    /// Matching ignores case — the server dispatches case-insensitively — and
    /// always inserts the server's spelling, so `/ST` completes to `/status`.
    fn begin(&mut self, word: &str) -> Option<String> {
        let lower = word.to_lowercase();
        let candidates: Vec<String> = self.names.iter().filter(|name| name.to_lowercase().starts_with(&lower)).cloned().collect();
        match candidates.len() {
            0 => None,
            // Unambiguous: finish the word and leave no cycle behind.
            1 => candidates.into_iter().next(),
            _ => {
                let prefix = longest_common_prefix(&candidates);
                if prefix.chars().count() > word.chars().count() {
                    self.cycle = Some(Cycle { candidates, index: None });
                    Some(prefix)
                } else {
                    // The prefix is already all the user typed, so there is nothing
                    // left to narrow — start offering the candidates themselves.
                    let first = candidates.first().cloned();
                    self.cycle = Some(Cycle { candidates, index: Some(0) });
                    first
                }
            }
        }
    }
}

impl Cycle {
    /// Offer the next candidate, wrapping at the end.
    fn advance(&mut self) -> String {
        let next = match self.index {
            None => 0,
            Some(i) => (i + 1) % self.candidates.len(),
        };
        self.index = Some(next);
        self.candidates[next].clone()
    }
}

/// The longest prefix every candidate shares.
fn longest_common_prefix(candidates: &[String]) -> String {
    let first: Vec<char> = candidates.first().map(|c| c.chars().collect()).unwrap_or_default();
    first
        .iter()
        .enumerate()
        .take_while(|(i, c)| candidates.iter().all(|cand| cand.chars().nth(*i) == Some(**c)))
        .map(|(_, c)| *c)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real set the server advertises, in its order.
    fn completion() -> Completion {
        let mut c = Completion::default();
        c.set_commands(
            ["/start", "/status", "/history", "/exercises", "/philosophy", "/nextworkout", "/cancel", "/clear", "/timers", "/help"]
                .into_iter()
                .map(str::to_string),
        );
        c
    }

    /// Tab at the end of `line`, repeated `times`, reporting the resulting line.
    fn tab(c: &mut Completion, line: &str, times: usize) -> String {
        (0..times).fold(line.to_string(), |line, _| {
            let cursor = line.chars().count();
            c.complete(&line, cursor).map(|(l, _)| l).unwrap_or(line)
        })
    }

    #[test]
    fn unique_prefix_completes_the_whole_command() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/hi", 1), "/history");
        assert_eq!(tab(&mut c, "/e", 1), "/exercises");
    }

    #[test]
    fn cursor_lands_after_the_completed_command() {
        let mut c = completion();
        let (line, cursor) = c.complete("/hi", 3).unwrap();
        assert_eq!((line.as_str(), cursor), ("/history", 8));
    }

    #[test]
    fn ambiguous_prefix_completes_to_the_longest_common_prefix() {
        let mut c = completion();
        // /start, /status → "/sta"
        assert_eq!(tab(&mut c, "/s", 1), "/sta");
        // /cancel, /clear → "/c"; already there, so it goes straight to cycling.
        assert_eq!(tab(&mut completion(), "/c", 1), "/cancel");
    }

    #[test]
    fn repeated_tab_cycles_the_candidates_and_wraps() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/s", 1), "/sta");
        assert_eq!(tab(&mut c, "/sta", 1), "/start");
        assert_eq!(tab(&mut c, "/start", 1), "/status");
        // Two candidates, so the third Tab comes back around.
        assert_eq!(tab(&mut c, "/status", 1), "/start");
    }

    #[test]
    fn a_unique_completion_leaves_no_cycle_to_continue() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/hi", 1), "/history");
        // Tab again: "/history" still matches only itself, so it stands.
        assert_eq!(tab(&mut c, "/history", 1), "/history");
    }

    #[test]
    fn reset_makes_the_next_tab_read_the_line_afresh() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/s", 1), "/sta");
        // Simulates the user typing after a Tab: the cycle must not resume.
        c.reset();
        assert_eq!(tab(&mut c, "/star", 1), "/start");
    }

    #[test]
    fn no_match_leaves_the_line_alone() {
        let mut c = completion();
        assert_eq!(c.complete("/zzz", 4), None);
        assert_eq!(c.complete("/nosuch", 7), None);
    }

    #[test]
    fn only_fires_on_a_slash_command() {
        let mut c = completion();
        assert_eq!(c.complete("hello", 5), None);
        assert_eq!(c.complete("", 0), None);
        assert_eq!(c.complete("bench /s", 8), None);
        // Leading whitespace means the line does not start with a command.
        assert_eq!(c.complete(" /s", 3), None);
    }

    #[test]
    fn only_fires_while_the_cursor_is_in_the_first_word() {
        let mut c = completion();
        // "/s| 100kg" — still in the command.
        assert_eq!(c.complete("/s 100kg", 2).map(|(l, _)| l), Some("/sta 100kg".to_string()));
        // "/s 100kg|" — the cursor has moved on to the argument.
        assert_eq!(c.complete("/s 100kg", 8), None);
    }

    #[test]
    fn completing_preserves_the_rest_of_the_line() {
        let mut c = completion();
        let (line, cursor) = c.complete("/hi yesterday", 3).unwrap();
        assert_eq!((line.as_str(), cursor), ("/history yesterday", 8));
    }

    #[test]
    fn matching_ignores_case_but_inserts_the_servers_spelling() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/HI", 1), "/history");
    }

    /// With no advertised set — before the server answers, or if it never does —
    /// Tab must simply do nothing.
    #[test]
    fn without_an_advertised_set_nothing_completes() {
        let mut c = Completion::default();
        assert_eq!(c.complete("/s", 2), None);
    }

    #[test]
    fn a_bare_slash_offers_every_command_in_turn() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/", 1), "/start");
        assert_eq!(tab(&mut c, "/start", 1), "/status");
    }

    #[test]
    fn a_new_advertised_set_replaces_the_old_one() {
        let mut c = completion();
        assert_eq!(tab(&mut c, "/hi", 1), "/history");
        c.set_commands(["/hello".to_string()]);
        assert_eq!(tab(&mut c, "/hi", 1), "/hi");
        assert_eq!(tab(&mut c, "/he", 1), "/hello");
    }

    #[test]
    fn longest_common_prefix_of_disjoint_candidates_is_empty() {
        assert_eq!(longest_common_prefix(&["/abc".into(), "xyz".into()]), "");
        assert_eq!(longest_common_prefix(&[]), "");
        assert_eq!(longest_common_prefix(&["/only".into()]), "/only");
    }
}

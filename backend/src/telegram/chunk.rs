//! HTML-aware splitter for messages that exceed Telegram's 4096-character ceiling.
//!
//! Telegram rejects messages whose HTML parse mode leaves tags unclosed at the
//! chunk boundary (e.g. a `<pre>` that ends mid-block with
//! `Can't find end of the entity starting at byte offset ...`). The splitter
//! therefore prefers boundaries that fall outside `<pre>` regions, falls back
//! to whitespace, and as a last resort hard-cuts at a UTF-8 char boundary —
//! closing and reopening `<pre>` tags across the split so every emitted chunk
//! is independently well-formed.

/// Hard upper bound enforced by Telegram for a single message payload.
pub const MAX_LEN: usize = 4096;

const PRE_OPEN: &str = "<pre>";
const PRE_CLOSE: &str = "</pre>";

/// Split `text` into chunks no longer than [`MAX_LEN`] bytes, preferring
/// boundaries that keep `<pre>` blocks intact. Empty input yields an empty
/// vector.
pub fn split_for_telegram(text: &str) -> Vec<String> {
    split_with_limit(text, MAX_LEN)
}

/// Same as [`split_for_telegram`] but with a configurable byte limit; exposed so
/// the unit tests can exercise the boundary logic on small inputs.
pub fn split_with_limit(text: &str, max_len: usize) -> Vec<String> {
    assert!(max_len > PRE_OPEN.len() + PRE_CLOSE.len() + 2, "max_len too small to fit reopened tags");

    let mut chunks = Vec::new();
    let mut remaining = text;
    let mut carry_open_pre = false;

    while !remaining.is_empty() {
        let start_depth = i32::from(carry_open_pre);
        let prefix_len = if carry_open_pre { PRE_OPEN.len() } else { 0 };

        // Easy case: the whole tail fits in one chunk. If we're still inside a
        // carried-open <pre>, we must reopen it AND ensure the tail itself
        // already closes that block (otherwise we'd emit an unbalanced final
        // chunk). The tail begins with the carried-pre content from the same
        // source block, so it always contains the matching </pre> somewhere.
        if remaining.len() + prefix_len <= max_len {
            chunks.push(prefix(carry_open_pre) + remaining);
            break;
        }

        let (cut, close_pre_at_end) = choose_split_point(remaining, max_len - prefix_len, start_depth);

        let mut chunk = prefix(carry_open_pre);
        chunk.push_str(&remaining[..cut]);
        if close_pre_at_end {
            chunk.push_str(PRE_CLOSE);
        }
        chunks.push(chunk);

        carry_open_pre = close_pre_at_end;
        remaining = remaining[cut..].trim_start();
    }

    chunks
}

fn prefix(carry: bool) -> String {
    if carry { PRE_OPEN.to_string() } else { String::new() }
}

/// Choose where to cut. Returns the byte offset (always a UTF-8 boundary) and a
/// flag indicating whether the cut falls inside an open `<pre>` block, in which
/// case the caller must append `</pre>` to the emitted chunk and prepend
/// `<pre>` to the next one. `budget` is the byte space available in this chunk
/// AFTER any leading `<pre>` reopen; `start_depth` is the `<pre>` nesting level
/// that the chunk inherits from the previous one (1 if carrying, else 0).
///
/// Preference ladder:
/// 1. byte immediately after the last `</pre>\n` within budget (clean close);
/// 2. last newline that lies outside any open `<pre>`;
/// 3. last space that lies outside any open `<pre>`;
/// 4. UTF-8 safe hard cut, with `</pre>`/`<pre>` carried over if mid-block.
fn choose_split_point(text: &str, budget: usize, start_depth: i32) -> (usize, bool) {
    let window_end = safe_boundary(text, budget.min(text.len()));

    if start_depth == 0
        && let Some(idx) = find_pre_close(text, window_end)
    {
        return (idx, false);
    }
    if let Some(idx) = find_outside_pre(text, window_end, '\n', start_depth) {
        return (idx, false);
    }
    if let Some(idx) = find_outside_pre(text, window_end, ' ', start_depth) {
        return (idx, false);
    }

    // Forced hard cut. If we're inside a <pre> at the cut point we must close
    // it now and reopen on the next chunk — that requires budget for `</pre>`.
    let hard_budget = budget.saturating_sub(PRE_CLOSE.len()).max(1);
    let hard_end = safe_boundary(text, hard_budget.min(text.len()));
    let inside_pre = depth_at(text, hard_end, start_depth) > 0;
    (hard_end, inside_pre)
}

/// Walk `idx` left to the nearest UTF-8 char boundary.
fn safe_boundary(text: &str, mut idx: usize) -> usize {
    if idx > text.len() {
        idx = text.len();
    }
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Return the byte index immediately after the last `</pre>\n` in `text[..end]`,
/// or `None` if no such sequence exists.
fn find_pre_close(text: &str, end: usize) -> Option<usize> {
    const NEEDLE: &str = "</pre>\n";
    text[..end].rfind(NEEDLE).map(|idx| idx + NEEDLE.len())
}

/// Find the last occurrence of ASCII `delim` in `text[..end]` that lies outside
/// any open `<pre>` block, starting from `start_depth`. Returns the byte index
/// after the delimiter so it stays with the preceding chunk.
fn find_outside_pre(text: &str, end: usize, delim: char, start_depth: i32) -> Option<usize> {
    assert!(delim.is_ascii(), "delim must be ASCII");
    let delim_byte = delim as u32 as u8;
    let window = &text[..end];
    let bytes = window.as_bytes();
    let mut depth = start_depth;
    let mut last_safe: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if window[i..].starts_with(PRE_OPEN) {
                depth += 1;
                i += PRE_OPEN.len();
                continue;
            }
            if window[i..].starts_with(PRE_CLOSE) {
                depth = depth.saturating_sub(1);
                i += PRE_CLOSE.len();
                continue;
            }
        }
        if depth == 0 && bytes[i] == delim_byte {
            last_safe = Some(i + 1);
        }
        i += 1;
    }
    last_safe
}

/// `<pre>` nesting depth at byte offset `end`, starting from `start_depth`.
/// Positive means a cut at `end` would land inside an unclosed `<pre>` block.
fn depth_at(text: &str, end: usize, start_depth: i32) -> i32 {
    let window = &text[..end];
    let bytes = window.as_bytes();
    let mut depth = start_depth;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if window[i..].starts_with(PRE_OPEN) {
                depth += 1;
                i += PRE_OPEN.len();
                continue;
            }
            if window[i..].starts_with(PRE_CLOSE) {
                depth = depth.saturating_sub(1);
                i += PRE_CLOSE.len();
                continue;
            }
        }
        i += 1;
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returns_single_chunk() {
        let chunks = split_with_limit("hello world", 4096);
        assert_eq!(chunks, vec!["hello world".to_string()]);
    }

    #[test]
    fn prefers_pre_close_boundary_over_inner_whitespace() {
        let block = "<b>One</b>\n<pre>aaa\nbbb\nccc\n</pre>\n<b>Two</b>\n<pre>ddd\neee\n</pre>";
        let chunks = split_with_limit(block, 40);
        assert!(chunks.len() >= 2, "input should split, got: {chunks:?}");
        // First chunk must end at the `</pre>\n` boundary, never inside a block.
        assert!(chunks[0].ends_with("</pre>\n"), "first chunk: {:?}", chunks[0]);
        for chunk in &chunks {
            assert_eq!(chunk.matches(PRE_OPEN).count(), chunk.matches(PRE_CLOSE).count(), "unbalanced: {chunk:?}");
        }
    }

    #[test]
    fn no_chunk_leaves_pre_unbalanced_even_when_block_is_huge() {
        // Build one fat <pre> block followed by another — far larger than MAX_LEN
        // so a single block cannot fit in one chunk; the splitter must close
        // and reopen <pre> across the cut.
        let mut block = String::from("<b>Header</b>\n<pre>");
        for i in 0..200 {
            block.push_str(&format!("row {i:03} of the catalogue\n"));
        }
        block.push_str("</pre>\n<b>Next</b>\n<pre>");
        for i in 0..200 {
            block.push_str(&format!("more {i:03}\n"));
        }
        block.push_str("</pre>");

        let chunks = split_with_limit(&block, 4096);
        assert!(chunks.len() >= 2, "input should exceed one chunk");
        for chunk in &chunks {
            assert!(chunk.len() <= 4096, "chunk over limit: {}", chunk.len());
            assert_eq!(chunk.matches(PRE_OPEN).count(), chunk.matches(PRE_CLOSE).count(), "unbalanced: {chunk:?}");
        }
    }

    #[test]
    fn whitespace_split_used_when_no_pre_close() {
        let text = "line one\nline two\nline three\nline four";
        let chunks = split_with_limit(text, 20);
        assert!(chunks.len() >= 2, "expected split, got {chunks:?}");
        assert!(chunks[0].ends_with('\n'), "first chunk should end at newline: {:?}", chunks[0]);
    }

    #[test]
    fn hard_cut_respects_utf8_boundary() {
        // Each emoji is a 4-byte UTF-8 sequence; no whitespace anywhere.
        let text: String = "🦀".repeat(20);
        let chunks = split_with_limit(&text, 20);
        assert!(chunks.len() >= 2, "expected multiple chunks");
        for chunk in &chunks {
            assert!(chunk.len() <= 20, "chunk exceeds limit: {}", chunk.len());
            // No tags carried over because input contains no <pre>.
            assert!(chunk.chars().all(|c| c == '🦀'), "chunk: {chunk:?}");
        }
        let rejoined: String = chunks.concat();
        assert_eq!(rejoined, text);
    }

    #[test]
    fn no_whitespace_input_falls_back_to_hard_cut() {
        let text = "x".repeat(50);
        let chunks = split_with_limit(&text, 20);
        for chunk in &chunks {
            assert!(chunk.len() <= 20);
        }
        let rejoined: String = chunks.concat();
        assert_eq!(rejoined, text);
    }

    #[test]
    fn forced_cut_inside_pre_closes_and_reopens() {
        // Single oversized <pre> with no whitespace — must close/reopen tags.
        let text = format!("<pre>{}</pre>", "y".repeat(100));
        let chunks = split_with_limit(&text, 50);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 50, "chunk over limit: {}", chunk.len());
            assert_eq!(chunk.matches(PRE_OPEN).count(), chunk.matches(PRE_CLOSE).count(), "unbalanced: {chunk:?}");
        }
    }
}

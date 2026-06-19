//! Small text-normalisation helpers shared across output paths.

/// Borrow at most `max_bytes` of `text`, truncated at the nearest UTF-8 character
/// boundary at or below the limit. A plain `&text[..max_bytes]` panics when the
/// cut falls inside a multibyte character (emoji, accents, CJK …); this walks back
/// to the preceding boundary instead, so the result is always ≤ `max_bytes` and
/// never splits a `char`.
pub(crate) fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let end = (0..=max_bytes).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    &text[..end]
}

/// Strip the markdown the LLM might emit so no raw markup reaches a client chat
/// box or the TTS engine: bold/italic/code (`*`, `_`, `` ` ``), ATX headings,
/// list markers (`- `, `* `, `1. `), and link syntax (keeping the link text).
pub(crate) fn strip_markdown(text: &str) -> String {
    let link_re = regex::Regex::new(r"\[([^\]]+)\]\([^)]+\)").unwrap();
    let text = link_re.replace_all(text, "$1");

    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            // Strip list markers (- item, * item, 1. item, 10. item).
            let line = if let Some(rest) = trimmed.strip_prefix("- ") {
                rest
            } else if let Some(rest) = trimmed.strip_prefix("* ") {
                rest
            } else if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                // Multi-digit numbered lists (1. item, 10. item, 100. item).
                let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                rest.strip_prefix(". ").unwrap_or(trimmed)
            } else {
                trimmed
            };
            // Strip heading markers.
            line.trim_start_matches('#').trim_start()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace(['*', '_', '`'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_bold_italic_code() {
        assert_eq!(strip_markdown("**bold** and *italic* and `code`"), "bold and italic and code");
    }

    #[test]
    fn preserves_plain_text() {
        assert_eq!(strip_markdown("hello world"), "hello world");
    }

    #[test]
    fn strips_links() {
        assert_eq!(strip_markdown("[click here](https://example.com)"), "click here");
    }

    #[test]
    fn strips_headings() {
        assert_eq!(strip_markdown("## Heading\nsome text"), "Heading\nsome text");
    }

    #[test]
    fn strips_list_markers() {
        assert_eq!(strip_markdown("- first\n* second\n1. third"), "first\nsecond\nthird");
    }

    #[test]
    fn strips_multi_digit_lists() {
        assert_eq!(strip_markdown("10. tenth item"), "tenth item");
        assert_eq!(strip_markdown("100. hundredth item"), "hundredth item");
    }

    #[test]
    fn truncate_keeps_short_text_whole() {
        assert_eq!(truncate_on_char_boundary("hello", 10), "hello");
        assert_eq!(truncate_on_char_boundary("hello", 5), "hello");
    }

    #[test]
    fn truncate_ascii_cuts_at_limit() {
        assert_eq!(truncate_on_char_boundary("hello world", 5), "hello");
    }

    #[test]
    fn truncate_never_splits_a_multibyte_char() {
        // "héllo": 'é' is two bytes (1..3), so a byte limit of 2 lands mid-char.
        // A naive &s[..2] would panic; we must fall back to "h".
        let s = "héllo";
        assert_eq!(truncate_on_char_boundary(s, 2), "h");
        // Limit exactly on the boundary after 'é' keeps "hé".
        assert_eq!(truncate_on_char_boundary(s, 3), "hé");
    }

    #[test]
    fn truncate_handles_emoji_and_cjk() {
        // 4-byte emoji: any limit inside it walks back to empty.
        assert_eq!(truncate_on_char_boundary("😀tail", 3), "");
        assert_eq!(truncate_on_char_boundary("😀tail", 4), "😀");
        // 3-byte CJK characters.
        assert_eq!(truncate_on_char_boundary("日本語", 4), "日");
    }
}

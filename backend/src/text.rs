//! Small text-normalisation helpers shared across output paths.

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
}

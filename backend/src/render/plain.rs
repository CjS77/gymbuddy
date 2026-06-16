//! Render a [`View`] to plain text — used by the voice pipeline (transcript echo
//! and TTS), which cannot speak HTML.
//!
//! Built on the Telegram renderer: take its output and, when it is HTML, strip
//! the tags and unescape the entities. This keeps a single source of truth for
//! the textual layout.

use gymbuddy_proto::{Render, View};

use super::telegram::Telegram;

/// Plain-text rendering of a view.
pub fn to_plain(view: &View) -> String {
    match Telegram.render(view) {
        (text, Some("HTML")) => html_to_plain(&text),
        (text, _) => text,
    }
}

/// Strip HTML tags and unescape the entities `escape_html` produces (`&amp;`,
/// `&lt;`, `&gt;`). `&amp;` is reversed last so an escaped literal entity such as
/// `&amp;lt;` decodes back to `&lt;` rather than `<`.
fn html_to_plain(html: &str) -> String {
    strip_tags(html).replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Remove `<...>` tags, keeping the text between them.
fn strip_tags(s: &str) -> String {
    s.chars()
        .fold((String::with_capacity(s.len()), false), |(mut out, in_tag), c| match c {
            '<' => (out, true),
            '>' => (out, false),
            _ if in_tag => (out, in_tag),
            _ => {
                out.push(c);
                (out, in_tag)
            }
        })
        .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use gymbuddy_proto::{CatalogEntry, CatalogGroup, CatalogView};

    #[test]
    fn message_passes_through() {
        assert_eq!(to_plain(&View::message("logged 3 sets")), "logged 3 sets");
    }

    #[test]
    fn html_view_is_stripped_and_unescaped() {
        let catalog = CatalogView {
            groups: vec![CatalogGroup {
                muscle_group: "Arms & Co".into(),
                exercises: vec![CatalogEntry { name: "Curl".into(), aliases: "".into(), kind: "weight_reps".into() }],
            }],
        };
        let plain = to_plain(&View::Catalog(catalog));
        assert!(plain.contains("Arms & Co"));
        assert!(!plain.contains('<'));
        assert!(!plain.contains("&amp;"));
    }
}

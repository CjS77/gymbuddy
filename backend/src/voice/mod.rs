mod stt;
mod tts;

pub use stt::SttClient;
pub use tts::TtsClient;

use crate::config::{ResponseMode, VoiceConfig};

pub struct VoicePipeline {
    stt: SttClient,
    tts: Option<TtsClient>,
    response_mode: ResponseMode,
    max_voice_duration_secs: u32,
}

impl VoicePipeline {
    pub fn new(config: &VoiceConfig) -> Self {
        let tts = if config.tts_enabled { Some(TtsClient::new(&config.tts_url, &config.tts_speaker, config.tts_speed)) } else { None };
        Self {
            stt: SttClient::new(&config.stt_url),
            tts,
            response_mode: config.response_mode.clone(),
            max_voice_duration_secs: config.max_voice_duration_secs,
        }
    }

    pub async fn speech_to_text(&self, audio_bytes: &[u8]) -> anyhow::Result<String> {
        self.stt.transcribe(audio_bytes).await
    }

    /// Returns None if TTS is disabled.
    pub async fn text_to_speech(&self, text: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match &self.tts {
            Some(tts) => {
                let clean = strip_markdown(text);
                Ok(Some(tts.synthesize(&clean).await?))
            }
            None => Ok(None),
        }
    }

    pub fn should_send_text(&self) -> bool {
        matches!(self.response_mode, ResponseMode::Text | ResponseMode::Both)
    }

    pub fn should_send_voice(&self) -> bool {
        matches!(self.response_mode, ResponseMode::Voice | ResponseMode::Both)
    }

    pub fn max_duration_secs(&self) -> u32 {
        self.max_voice_duration_secs
    }

    /// Health check both services. Called at startup.
    pub async fn verify(&self) -> anyhow::Result<()> {
        self.stt.health_check().await?;
        if let Some(ref tts) = self.tts {
            tts.health_check().await?;
        }
        Ok(())
    }
}

/// Strip markdown formatting for cleaner TTS output.
/// Removes *, _, `, #, list markers, and link syntax.
fn strip_markdown(text: &str) -> String {
    let link_re = regex::Regex::new(r"\[([^\]]+)\]\([^)]+\)").unwrap();
    let text = link_re.replace_all(text, "$1");

    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            // Strip list markers (- item, * item, 1. item, 10. item)
            let line = if let Some(rest) = trimmed.strip_prefix("- ") {
                rest
            } else if let Some(rest) = trimmed.strip_prefix("* ") {
                rest
            } else if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                // Handle multi-digit numbered lists (1. item, 10. item, 100. item)
                let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                if let Some(stripped) = rest.strip_prefix(". ") { stripped } else { trimmed }
            } else {
                trimmed
            };
            // Strip heading markers
            line.trim_start_matches('#').trim_start()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("**", "")
        .replace('*', "")
        .replace('_', "")
        .replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_markdown_removes_bold_italic_code() {
        assert_eq!(strip_markdown("**bold** and *italic* and `code`"), "bold and italic and code");
    }

    #[test]
    fn strip_markdown_preserves_plain_text() {
        assert_eq!(strip_markdown("hello world"), "hello world");
    }

    #[test]
    fn strip_markdown_strips_links() {
        assert_eq!(strip_markdown("[click here](https://example.com)"), "click here");
    }

    #[test]
    fn strip_markdown_strips_headings() {
        assert_eq!(strip_markdown("## Heading\nsome text"), "Heading\nsome text");
    }

    #[test]
    fn strip_markdown_strips_list_markers() {
        let input = "- first\n* second\n1. third";
        assert_eq!(strip_markdown(input), "first\nsecond\nthird");
    }

    #[test]
    fn strip_markdown_multi_digit_lists() {
        assert_eq!(strip_markdown("10. tenth item"), "tenth item");
        assert_eq!(strip_markdown("100. hundredth item"), "hundredth item");
    }

    #[test]
    fn should_send_text_per_mode() {
        assert!(matches!(ResponseMode::Text, ResponseMode::Text | ResponseMode::Both));
        assert!(matches!(ResponseMode::Both, ResponseMode::Text | ResponseMode::Both));
        assert!(!matches!(ResponseMode::Voice, ResponseMode::Text | ResponseMode::Both));
    }

    #[test]
    fn should_send_voice_per_mode() {
        assert!(matches!(ResponseMode::Voice, ResponseMode::Voice | ResponseMode::Both));
        assert!(matches!(ResponseMode::Both, ResponseMode::Voice | ResponseMode::Both));
        assert!(!matches!(ResponseMode::Text, ResponseMode::Voice | ResponseMode::Both));
    }
}

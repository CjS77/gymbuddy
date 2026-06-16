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
                let clean = crate::text::strip_markdown(text);
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

#[cfg(test)]
mod tests {
    use super::*;

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

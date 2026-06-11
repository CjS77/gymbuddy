use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct WhisperResponse {
    text: Option<String>,
    #[serde(default)]
    segments: Vec<WhisperSegment>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    text: String,
}

impl WhisperResponse {
    fn transcript(&self) -> String {
        if let Some(ref text) = self.text {
            text.trim().to_string()
        } else {
            // Fallback: join segment texts
            self.segments.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ")
        }
    }
}

pub struct SttClient {
    url: String,
    client: reqwest::Client,
}

impl SttClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("failed to build HTTP client"),
        }
    }

    /// Transcribe audio bytes (OGG/Opus from Telegram) to text.
    /// The whisper server handles format conversion internally via ffmpeg.
    /// Retries once on 5xx responses with a 2-second delay.
    pub async fn transcribe(&self, audio_bytes: &[u8]) -> anyhow::Result<String> {
        let mut last_err = None;

        for attempt in 0..2u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }

            let part = reqwest::multipart::Part::bytes(audio_bytes.to_vec()).file_name("voice.ogg").mime_str("audio/ogg")?;
            let form = reqwest::multipart::Form::new().part("file", part);

            let resp = match self.client.post(format!("{}/inference", self.url)).multipart(form).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("whisper request failed: {e:#}"));
                    continue;
                }
            };

            if resp.status().is_server_error() && attempt == 0 {
                let status = resp.status();
                tracing::warn!("whisper returned {status}, retrying");
                last_err = Some(anyhow::anyhow!("whisper server returned {status}"));
                continue;
            }

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("whisper server returned {status}: {body}");
            }

            let whisper_resp: WhisperResponse = resp.json().await?;
            return Ok(whisper_resp.transcript());
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("whisper transcription failed")))
    }

    pub async fn health_check(&self) -> anyhow::Result<()> {
        let resp = self.client.get(format!("{}/health", self.url)).send().await.context("whisper health check failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("whisper server unhealthy: {}", resp.status());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_response_text_field() {
        let resp: WhisperResponse = serde_json::from_str(r#"{"text": " hello world "}"#).unwrap();
        assert_eq!(resp.transcript(), "hello world");
    }

    #[test]
    fn whisper_response_segment_fallback() {
        let resp: WhisperResponse = serde_json::from_str(r#"{"segments": [{"text": " hello "}, {"text": " world "}]}"#).unwrap();
        assert_eq!(resp.transcript(), "hello world");
    }

    #[test]
    fn whisper_response_empty_text() {
        let resp: WhisperResponse = serde_json::from_str(r#"{"text": ""}"#).unwrap();
        assert_eq!(resp.transcript(), "");
    }

    #[test]
    fn whisper_response_no_text_no_segments() {
        let resp: WhisperResponse = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(resp.transcript(), "");
    }
}

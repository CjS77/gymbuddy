use std::time::Duration;

use anyhow::Context as _;

pub struct TtsClient {
    url: String,
    speaker: Option<String>,
    speed: f32,
    client: reqwest::Client,
}

impl TtsClient {
    pub fn new(url: &str, speaker: &str, speed: f32) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            speaker: if speaker.is_empty() { None } else { Some(speaker.to_string()) },
            speed,
            client: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("failed to build HTTP client"),
        }
    }

    /// Synthesize text to OGG/Opus audio bytes.
    /// The piper server handles WAV->OGG conversion internally via ffmpeg.
    /// Retries once on 5xx responses with a 1-second delay.
    pub async fn synthesize(&self, text: &str) -> anyhow::Result<Vec<u8>> {
        let mut body = serde_json::json!({"text": text});
        if let Some(ref speaker) = self.speaker {
            body["speaker"] = serde_json::json!(speaker);
        }
        if (self.speed - 1.0).abs() > f32::EPSILON {
            body["speed"] = serde_json::json!(self.speed);
        }

        let mut last_err = None;

        for attempt in 0..2u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            let resp = match self.client.post(format!("{}/synthesize", self.url)).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("piper request failed: {e:#}"));
                    continue;
                }
            };

            if resp.status().is_server_error() && attempt == 0 {
                let status = resp.status();
                tracing::warn!("piper returned {status}, retrying");
                last_err = Some(anyhow::anyhow!("piper server returned {status}"));
                continue;
            }

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("piper server returned {status}: {body}");
            }

            return Ok(resp.bytes().await?.to_vec());
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("piper synthesis failed")))
    }

    pub async fn health_check(&self) -> anyhow::Result<()> {
        let resp = self.client.get(format!("{}/health", self.url)).send().await.context("piper health check failed")?;
        if !resp.status().is_success() {
            anyhow::bail!("piper server unhealthy: {}", resp.status());
        }
        Ok(())
    }
}

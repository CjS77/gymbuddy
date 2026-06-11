use std::time::Duration;

use anyhow::{Context as _, bail};
use serde_json::json;

use super::types::{BotUser, Message, TelegramFile, TelegramResponse, Update};

#[derive(Clone)]
pub struct TelegramClient {
    client: reqwest::Client,
    base_url: String,
    file_base_url: String,
}

impl TelegramClient {
    pub fn new(token: &str) -> anyhow::Result<Self> {
        // Validate token format: {digits}:{alphanumeric+special}
        let parts: Vec<&str> = token.splitn(2, ':').collect();
        anyhow::ensure!(
            parts.len() == 2 && !parts[0].is_empty() && parts[0].chars().all(|c| c.is_ascii_digit()) && !parts[1].is_empty(),
            "invalid Telegram bot token format (expected digits:alphanumeric)"
        );

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .context("building HTTP client")?;

        let base_url = format!("https://api.telegram.org/bot{token}");
        let file_base_url = format!("https://api.telegram.org/file/bot{token}");
        Ok(Self { client, base_url, file_base_url })
    }

    /// Get bot info (startup verification).
    pub async fn get_me(&self) -> anyhow::Result<BotUser> {
        self.post("getMe", &json!({})).await
    }

    /// Long-poll for updates. Blocks server-side for up to `timeout` seconds.
    pub async fn get_updates(&self, offset: i64, timeout: u32) -> anyhow::Result<Vec<Update>> {
        let body = json!({
            "offset": offset,
            "timeout": timeout,
            "allowed_updates": ["message"],
        });

        // Use a longer client timeout to account for network overhead beyond
        // the Telegram server-side long-poll window.
        let request_timeout = Duration::from_secs(u64::from(timeout) + 10);

        let resp = self
            .client
            .post(format!("{}/getUpdates", self.base_url))
            .timeout(request_timeout)
            .json(&body)
            .send()
            .await
            .context("getUpdates request failed")?;

        let tg: TelegramResponse<Vec<Update>> = resp.json().await.context("parsing getUpdates response")?;
        self.unwrap_response(tg, "getUpdates")
    }

    /// Send a text message. Returns the sent Message.
    pub async fn send_message(&self, chat_id: i64, text: &str, parse_mode: Option<&str>, reply_to: Option<i64>) -> anyhow::Result<Message> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            body["parse_mode"] = json!(pm);
        }
        if let Some(rt) = reply_to {
            body["reply_to_message_id"] = json!(rt);
        }

        self.post_with_retry("sendMessage", &body).await
    }

    /// Send a "typing..." indicator.
    pub async fn send_chat_action(&self, chat_id: i64, action: &str) -> anyhow::Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "action": action,
        });
        let _: bool = self.post("sendChatAction", &body).await?;
        Ok(())
    }

    /// Get the file metadata for a file_id (needed to construct download URL).
    pub async fn get_file(&self, file_id: &str) -> anyhow::Result<TelegramFile> {
        self.post("getFile", &json!({"file_id": file_id})).await
    }

    /// Download file bytes directly into memory.
    pub async fn download_file_bytes(&self, file_path: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/{file_path}", self.file_base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("file download failed: {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Send a voice message (OGG/Opus bytes).
    pub async fn send_voice(&self, chat_id: i64, ogg_bytes: &[u8], reply_to: Option<i64>) -> anyhow::Result<Message> {
        let url = format!("{}/sendVoice", self.base_url);
        let part = reqwest::multipart::Part::bytes(ogg_bytes.to_vec()).file_name("voice.ogg").mime_str("audio/ogg")?;
        let mut form = reqwest::multipart::Form::new().text("chat_id", chat_id.to_string()).part("voice", part);
        if let Some(reply_id) = reply_to {
            form = form.text("reply_to_message_id", reply_id.to_string());
        }
        let resp: TelegramResponse<Message> = self.client.post(&url).multipart(form).send().await?.json().await?;
        match resp.result {
            Some(msg) if resp.ok => Ok(msg),
            _ => bail!("sendVoice failed: {}", resp.description.unwrap_or_else(|| "unknown error".into())),
        }
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, method: &str, body: &serde_json::Value) -> anyhow::Result<T> {
        let resp = self
            .client
            .post(format!("{}/{method}", self.base_url))
            .json(body)
            .send()
            .await
            .with_context(|| format!("{method} request failed"))?;

        let tg: TelegramResponse<T> = resp.json().await.with_context(|| format!("parsing {method} response"))?;
        self.unwrap_response(tg, method)
    }

    /// Post with automatic retry on 429 rate limiting.
    async fn post_with_retry<T: serde::de::DeserializeOwned>(&self, method: &str, body: &serde_json::Value) -> anyhow::Result<T> {
        for _ in 0..3 {
            let resp = self
                .client
                .post(format!("{}/{method}", self.base_url))
                .json(body)
                .send()
                .await
                .with_context(|| format!("{method} request failed"))?;

            let status = resp.status();
            let tg: TelegramResponse<T> = resp.json().await.with_context(|| format!("parsing {method} response"))?;

            if status == 429 {
                let wait = tg.parameters.as_ref().and_then(|p| p.retry_after).unwrap_or(5);
                tracing::warn!("Telegram rate limited, waiting {wait}s");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }

            return self.unwrap_response(tg, method);
        }

        bail!("Telegram {method}: rate limited after 3 retries")
    }

    fn unwrap_response<T>(&self, resp: TelegramResponse<T>, method: &str) -> anyhow::Result<T> {
        if resp.ok {
            resp.result.ok_or_else(|| anyhow::anyhow!("Telegram {method}: ok=true but result is null"))
        } else {
            let code = resp.error_code.unwrap_or(0);
            let desc = resp.description.unwrap_or_else(|| "unknown error".into());
            bail!("Telegram API {code}: {desc}")
        }
    }
}

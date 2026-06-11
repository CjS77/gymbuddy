use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub error_code: Option<i32>,
    pub description: Option<String>,
    /// Seconds to wait before retrying (present on 429 responses).
    #[serde(default)]
    pub parameters: Option<ResponseParameters>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseParameters {
    pub retry_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<TelegramUser>,
    pub chat: Chat,
    pub date: i64,
    pub text: Option<String>,
    pub voice: Option<Voice>,
    pub audio: Option<Audio>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub duration: i32,
}

#[derive(Debug, Deserialize)]
pub struct BotUser {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: String,
}

#[derive(Debug, Deserialize)]
pub struct TelegramFile {
    pub file_id: String,
    pub file_unique_id: Option<String>,
    pub file_size: Option<i64>,
    pub file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Audio {
    pub file_id: String,
    pub duration: i32,
}

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use corre_core::config::CorreConfig;
use gymbuddy_backend::assistant::AssistantHandler;
use gymbuddy_backend::config::GymConfig;
use gymbuddy_backend::db::Database;
use gymbuddy_backend::github::{GithubIssueReporter, IssueReporter};
use gymbuddy_backend::telegram::{Message, TelegramClient, Voice};
use gymbuddy_backend::voice::VoicePipeline;
use corre_llm::OpenAiCompatProvider;

#[derive(Parser)]
#[command(name = "gymbuddy", about = "You friendly AI personal trainer")]
struct Cli {
    /// Path to corre.toml config file
    #[arg(short, long, default_value_os_t = default_config_path())]
    config: PathBuf,
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir().map(|d| d.join("corre")).unwrap_or_else(|| PathBuf::from("."))
}

fn default_config_path() -> PathBuf {
    default_data_dir().join("gymbuddy.toml")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (telegram, handler, allowed_ids, voice_pipeline, _gym_config, _db) = setup().await?;
    let handler = Arc::new(handler);

    tokio::select! {
        result = run_polling_loop(&telegram, &handler, &allowed_ids, voice_pipeline.as_ref()) => {
            result
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, shutting down");
            Ok(())
        }
    }
}

async fn setup()
-> anyhow::Result<(TelegramClient, AssistantHandler, Vec<i64>, Option<VoicePipeline>, GymConfig, Arc<Mutex<Database>>)> {
    // 1. Load .env from data dir (best-effort, same as corre-news)
    let default_data_dir = default_data_dir();
    tracing::info!("Loading environment from {}", default_data_dir.display());
    let _ = dotenvy::from_filename(default_data_dir.join(".env")).ok();

    // 2. Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();

    // 3. Parse CLI args
    let cli = Cli::parse();

    // 4. Load config
    tracing::info!(path = %cli.config.display(), "Loading config");
    let config = CorreConfig::load(&cli.config).context("loading config")?;
    let data_dir = config.data_dir();
    tracing::debug!(raw_gym = %config.gym, "Raw [gym] table from config");
    let mut gym_config = GymConfig::from_toml_table(Some(&config.gym))?;
    gym_config.resolve_secrets()?;
    gym_config.resolve_endpoints()?;

    // 5. Build LLM provider (with optional [gym.llm] overrides)
    tracing::debug!(gym_llm = ?gym_config.llm, "Gym LLM override");
    let effective_llm = match gym_config.llm.as_ref() {
        Some(overrides) => config.llm.with_overrides(overrides),
        None => config.llm.clone(),
    };
    tracing::info!(model = %effective_llm.model, base_url = %effective_llm.base_url, "LLM config loaded");
    let raw_llm: Box<dyn corre_core::app::LlmProvider> = Box::new(OpenAiCompatProvider::from_config(&effective_llm)?);
    let llm: Box<dyn corre_core::app::LlmProvider> = if config.safety.enabled {
        tracing::info!("Safety layer enabled — wrapping LLM provider");
        Box::new(corre_safety::SafeLlmProvider::new(raw_llm, &config.safety))
    } else {
        raw_llm
    };

    // 6. Open database
    let db_path = data_dir.join(&gym_config.db_path);
    tracing::info!("Loading database from {}", db_path.display());
    let db = Database::open(&db_path)?;
    let db = Arc::new(Mutex::new(db));
    tracing::info!("Database ready!");

    // 7. Create Telegram client, verify connection
    let telegram = TelegramClient::new(&gym_config.telegram_bot_token)?;
    let me = telegram.get_me().await?;
    tracing::info!("Bot connected (id: {})", me.id);

    let allowed_ids = gym_config.telegram_allowed_ids.clone();

    // 8. Optional feedback reporter (gated by [gym.github] config + per-user beta flag)
    let issue_reporter: Option<Arc<dyn IssueReporter>> = match &gym_config.github {
        Some(github_cfg) => {
            let reporter = GithubIssueReporter::new(github_cfg).context("constructing GitHub issue reporter")?;
            tracing::info!(repo = %github_cfg.repo, "Feedback target configured");
            Some(Arc::new(reporter))
        }
        None => None,
    };

    // 9. Create handler
    let handler = AssistantHandler::new_with_reporter(db.clone(), llm, gym_config.clone(), issue_reporter).await?;

    // 10. Voice pipeline (optional)
    let voice_pipeline = match &gym_config.voice {
        Some(voice_config) if voice_config.stt_enabled => {
            voice_config.validate()?;
            let pipeline = VoicePipeline::new(voice_config);
            match pipeline.verify().await {
                Ok(()) => {
                    tracing::info!(
                        stt_url = %voice_config.stt_url,
                        tts = if voice_config.tts_enabled { &voice_config.tts_url } else { "disabled" },
                        "Voice pipeline active"
                    );
                    Some(pipeline)
                }
                Err(e) => {
                    tracing::warn!("Voice services unreachable, voice disabled: {e:#}");
                    None
                }
            }
        }
        _ => {
            tracing::info!("Voice pipeline not configured");
            None
        }
    };

    Ok((telegram, handler, allowed_ids, voice_pipeline, gym_config, db))
}

async fn run_polling_loop(
    telegram: &TelegramClient,
    handler: &Arc<AssistantHandler>,
    allowed_ids: &[i64],
    voice_pipeline: Option<&VoicePipeline>,
) -> anyhow::Result<()> {
    let mut offset = 0i64;

    loop {
        match telegram.get_updates(offset, 30).await {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(ref message) = update.message {
                        process_message(telegram, handler, voice_pipeline, message, allowed_ids).await;
                    }
                }
            }
            Err(e) => {
                tracing::error!("get_updates failed: {e:#}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn process_message(
    telegram: &TelegramClient,
    handler: &Arc<AssistantHandler>,
    voice_pipeline: Option<&VoicePipeline>,
    message: &Message,
    allowed_ids: &[i64],
) {
    if message.chat.chat_type != "private" {
        return;
    }
    let Some(ref from) = message.from else { return };
    if !allowed_ids.is_empty() && !allowed_ids.contains(&from.id) {
        tracing::debug!("Ignoring message from unauthorized user {}", from.id);
        return;
    }

    if let Some(ref text) = message.text {
        process_text_message(telegram, handler, message, text).await;
    } else if let Some(ref voice) = message.voice {
        process_voice_message(telegram, handler, voice_pipeline, message, voice).await;
    } else if message.audio.is_some() {
        if let Err(e) =
            telegram.send_message(message.chat.id, "Please use the microphone button to record voice messages directly.", None, None).await
        {
            tracing::warn!("Failed to send audio guidance: {e:#}");
        }
    }
}

async fn process_text_message(telegram: &TelegramClient, handler: &Arc<AssistantHandler>, message: &Message, text: &str) {
    let _ = telegram.send_chat_action(message.chat.id, "typing").await;

    let (reply_text, parse_mode) = match handler.handle_text_message(message, text).await {
        Ok(reply) => (reply.text, reply.parse_mode),
        Err(e) => {
            tracing::error!("Handler error: {e:#}");
            ("Something went wrong -- please try again later.".to_string(), None)
        }
    };

    if let Err(e) = send_long_message(telegram, message.chat.id, &reply_text, parse_mode).await {
        tracing::error!("Failed to send reply: {e:#}");
    }
}

async fn process_voice_message(
    telegram: &TelegramClient,
    handler: &Arc<AssistantHandler>,
    voice_pipeline: Option<&VoicePipeline>,
    message: &Message,
    voice: &Voice,
) {
    // 0. Check if voice is enabled
    let Some(pipeline) = voice_pipeline else {
        if let Err(e) =
            telegram.send_message(message.chat.id, "Voice messages are not enabled. Please type your message instead.", None, None).await
        {
            tracing::warn!("Failed to send voice-disabled notice: {e:#}");
        }
        return;
    };

    // 1. Reject overly long messages
    if voice.duration as u32 > pipeline.max_duration_secs() {
        if let Err(e) = telegram
            .send_message(
                message.chat.id,
                "That voice message is too long. Please keep it under 60 seconds, or type your message.",
                None,
                None,
            )
            .await
        {
            tracing::warn!("Failed to send duration-limit notice: {e:#}");
        }
        return;
    }

    // 2. Start chat action refresh loop (re-sends every 4s to avoid 5s expiry)
    let stop_action = spawn_chat_action_loop(telegram, message.chat.id, "record_voice");

    // 3. Download OGG from Telegram
    let ogg_bytes = match download_voice(telegram, &voice.file_id).await {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = stop_action.send(());
            tracing::error!("Failed to download voice: {e:#}");
            if let Err(e) =
                telegram.send_message(message.chat.id, "I couldn't download that voice message. Could you try again?", None, None).await
            {
                tracing::warn!("Failed to send download-error notice: {e:#}");
            }
            return;
        }
    };

    // 4. Transcribe via whisper
    let transcript = match pipeline.speech_to_text(&ogg_bytes).await {
        Ok(text) if !text.trim().is_empty() => text,
        Ok(_) => {
            let _ = stop_action.send(());
            if let Err(e) = telegram
                .send_message(message.chat.id, "I couldn't make out what you said. Could you try again, or type your message?", None, None)
                .await
            {
                tracing::warn!("Failed to send empty-transcript notice: {e:#}");
            }
            return;
        }
        Err(e) => {
            let _ = stop_action.send(());
            tracing::error!("STT failed: {e:#}");
            if let Err(e) = telegram
                .send_message(message.chat.id, "I had trouble understanding that voice message. Could you type it instead?", None, None)
                .await
            {
                tracing::warn!("Failed to send STT-error notice: {e:#}");
            }
            return;
        }
    };

    tracing::info!(duration = voice.duration, transcript = %transcript, "Voice transcribed");

    // 5. Switch chat action to "typing" for the LLM call
    let _ = stop_action.send(());
    let stop_action = spawn_chat_action_loop(telegram, message.chat.id, "typing");

    // 6. Process transcript through handler (identical to text messages)
    let reply = match handler.handle_text_message(message, &transcript).await {
        Ok(r) => r.text,
        Err(e) => {
            tracing::error!("Handler error: {e:#}");
            "I had trouble processing that -- could you try again?".to_string()
        }
    };

    let _ = stop_action.send(());

    // 7. Send text reply with transcript echo (if configured)
    if pipeline.should_send_text() {
        let text_with_echo = format!("_Heard: \"{transcript}\"_\n\n{reply}");
        if let Err(e) = send_long_message(telegram, message.chat.id, &text_with_echo, None).await {
            tracing::error!("Failed to send text reply: {e:#}");
        }
    }

    // 8. Synthesize and send voice reply (if configured)
    if pipeline.should_send_voice() {
        match pipeline.text_to_speech(&reply).await {
            Ok(Some(ogg_bytes)) => {
                let _ = telegram.send_chat_action(message.chat.id, "upload_voice").await;
                if let Err(e) = telegram.send_voice(message.chat.id, &ogg_bytes, None).await {
                    tracing::error!("Failed to send voice reply: {e:#}");
                    // Fallback: send text if we haven't already
                    if !pipeline.should_send_text() {
                        let text_with_echo = format!("_Heard: \"{transcript}\"_\n\n{reply}");
                        if let Err(e) = send_long_message(telegram, message.chat.id, &text_with_echo, None).await {
                            tracing::warn!("Failed to send fallback text: {e:#}");
                        }
                    }
                }
            }
            Ok(None) => {} // TTS disabled
            Err(e) => {
                tracing::warn!("TTS synthesis failed: {e:#}");
                // Graceful degradation: send text if we haven't already
                if !pipeline.should_send_text() {
                    let text_with_echo = format!("_Heard: \"{transcript}\"_\n\n{reply}");
                    if let Err(e) = send_long_message(telegram, message.chat.id, &text_with_echo, None).await {
                        tracing::warn!("Failed to send fallback text: {e:#}");
                    }
                }
            }
        }
    }
}

async fn download_voice(telegram: &TelegramClient, file_id: &str) -> anyhow::Result<Vec<u8>> {
    let file = telegram.get_file(file_id).await?;
    let file_path = file.file_path.context("Telegram returned no file_path")?;
    telegram.download_file_bytes(&file_path).await
}

/// Re-sends a chat action every 4 seconds until the returned sender is dropped or signalled.
/// Telegram chat actions expire after 5 seconds, so this keeps the UI responsive during
/// long operations (transcription, LLM calls, synthesis).
fn spawn_chat_action_loop(telegram: &TelegramClient, chat_id: i64, action: &str) -> tokio::sync::oneshot::Sender<()> {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let telegram = telegram.clone();
    let action = action.to_string();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                _ = tokio::time::sleep(Duration::from_secs(4)) => {
                    let _ = telegram.send_chat_action(chat_id, &action).await;
                }
            }
        }
    });
    tx
}

/// Splits messages exceeding Telegram's 4096 character limit.
async fn send_long_message(telegram: &TelegramClient, chat_id: i64, text: &str, parse_mode: Option<&str>) -> anyhow::Result<()> {
    const MAX_LEN: usize = 4096;

    if text.len() <= MAX_LEN {
        telegram.send_message(chat_id, text, parse_mode, None).await?;
        return Ok(());
    }

    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= MAX_LEN {
            telegram.send_message(chat_id, remaining, parse_mode, None).await?;
            break;
        }

        let chunk = &remaining[..MAX_LEN];
        // Try splitting at the last newline
        let split_at = chunk.rfind('\n').or_else(|| chunk.rfind(' ')).unwrap_or(MAX_LEN);

        telegram.send_message(chat_id, &remaining[..split_at], parse_mode, None).await?;
        remaining = remaining[split_at..].trim_start();
    }

    Ok(())
}

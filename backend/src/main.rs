use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use corre_core::config::CorreConfig;
use gymbuddy_backend::assistant::AssistantHandler;
use gymbuddy_backend::config::GymConfig;
use gymbuddy_backend::db::Database;
use gymbuddy_backend::dump;
use gymbuddy_backend::github::{GithubIssueReporter, IssueReporter};
use gymbuddy_backend::render::{Telegram, to_plain};
use gymbuddy_backend::telegram::chunk::split_for_telegram;
use gymbuddy_backend::telegram::{Message, TelegramClient, Voice};
use gymbuddy_backend::transport;
use gymbuddy_backend::voice::VoicePipeline;
use corre_llm::OpenAiCompatProvider;
use gymbuddy_proto::{Render, TimerSignal};
use gymbuddy_timer_core::{Cue, run_timer};

#[derive(Parser)]
#[command(name = "gymbuddy", about = "You friendly AI personal trainer")]
struct Cli {
    /// Path to corre.toml config file
    #[arg(short, long, default_value_os_t = default_config_path(), global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands are additive: omitting one runs [`Command::Serve`], so the deployment invocation
/// (`gymbuddy --config …`) is unchanged.
#[derive(Subcommand)]
enum Command {
    /// Run the bot. The default when no subcommand is given.
    Serve,

    /// Export a database to a JSON dump — the backup tool, and the first half of a migration.
    ///
    /// Version-aware: reads either schema generation and emits one format. The database is opened
    /// read-only and is never written to.
    Export {
        /// Database to read. Not taken from the config: exporting a backup copy is the common case.
        #[arg(long)]
        db: PathBuf,
        /// File to write the dump to. Overwritten if it exists.
        #[arg(long)]
        out: PathBuf,
    },

    /// Load a dump into a fresh schema v2 database. Refuses a database that already holds data.
    Import {
        /// Database to write. Must be empty.
        #[arg(long)]
        db: PathBuf,
        /// Dump to read.
        #[arg(long = "in")]
        input: PathBuf,
    },

    /// Migrate a legacy database to schema v2: export, build v2, import, then verify.
    ///
    /// Writes a new file and never touches the old one, which is therefore its own rollback.
    Migrate {
        /// Legacy database to read. Never written to.
        #[arg(long)]
        db: PathBuf,
        /// Schema v2 database to create.
        #[arg(long)]
        out: PathBuf,
        /// Re-export the result and compare it against the source dump. On by default.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        verify: bool,
    },
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir().map(|d| d.join("corre")).unwrap_or_else(|| PathBuf::from("."))
}

fn default_config_path() -> PathBuf {
    default_data_dir().join("gymbuddy.toml")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_observability();
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(&cli.config).await,
        Command::Export { db, out } => run_export(&db, &out),
        Command::Import { .. } => anyhow::bail!("`gymbuddy import` is not implemented yet — it lands with the schema v2 importer"),
        Command::Migrate { .. } => anyhow::bail!("`gymbuddy migrate` is not implemented yet — it lands with the schema v2 importer"),
    }
}

/// Load `.env` from the data dir (best-effort) and start tracing.
///
/// Shared by every subcommand, so the one-shot data tools report through the same filter the
/// server does — `RUST_LOG=debug gymbuddy export …` works as expected.
fn init_observability() {
    let data_dir = default_data_dir();
    let _ = dotenvy::from_filename(data_dir.join(".env")).ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();
    tracing::debug!("Loaded environment from {}", data_dir.display());
}

/// `gymbuddy export --db <path> --out dump.json`.
fn run_export(db: &Path, out: &Path) -> anyhow::Result<()> {
    tracing::info!(db = %db.display(), "Exporting (read-only)");
    let dump = dump::export_path(db)?;
    let json = dump::to_json(&dump)?;
    std::fs::write(out, &json).with_context(|| format!("writing dump to {}", out.display()))?;

    // Per-collection counts, not just a total: they are what an operator reconciles against the
    // source database to convince themselves nothing was left behind, and the only way a dropped
    // table shows up in a run that otherwise reports success.
    let counts = dump.row_counts();
    tracing::info!(
        out = %out.display(),
        source_schema = dump.source_schema.generation,
        rows = counts.total(),
        bytes = json.len(),
        "Export complete"
    );
    tracing::info!(%counts, "Rows exported per collection");
    Ok(())
}

async fn serve(config_path: &Path) -> anyhow::Result<()> {
    let (telegram, handler, allowed_ids, voice_pipeline, gym_config) = setup(config_path).await?;
    let handler = Arc::new(handler);

    anyhow::ensure!(
        telegram.is_some() || gym_config.confide.is_some(),
        "no transport configured: set telegram_bot_token and/or [gym.confide] in the config"
    );

    // Each transport runs only when configured; the absent one parks forever so it
    // never wins the select!.
    let telegram_loop = async {
        match telegram.as_ref() {
            Some(tg) => run_polling_loop(tg, &handler, &allowed_ids, voice_pipeline.as_ref()).await,
            None => std::future::pending::<anyhow::Result<()>>().await,
        }
    };
    let confide_loop = async {
        match gym_config.confide.clone() {
            Some(cfg) => transport::confide::serve(handler.clone(), cfg).await,
            None => std::future::pending::<anyhow::Result<()>>().await,
        }
    };

    tokio::select! {
        result = telegram_loop => result,
        result = confide_loop => result,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Ctrl+C received, shutting down");
            Ok(())
        }
    }
}

async fn setup(
    config_path: &Path,
) -> anyhow::Result<(Option<TelegramClient>, AssistantHandler, Vec<i64>, Option<VoicePipeline>, GymConfig)> {
    // 1. Load config
    tracing::info!(path = %config_path.display(), "Loading config");
    let config = CorreConfig::load(config_path).context("loading config")?;
    let data_dir = config.data_dir();
    // corre 0.22 dropped the hardcoded `[gym]` table from `CorreConfig` when it
    // removed the corre-gym app. This repo is that app now, so it reads its own
    // table out of the same file rather than expecting the host to surface it.
    let raw_config: toml::Value = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?
        .parse()
        .with_context(|| format!("parsing {} as TOML", config_path.display()))?;
    tracing::debug!(raw_gym = ?raw_config.get("gym"), "Raw [gym] table from config");
    let mut gym_config = GymConfig::from_toml_table(raw_config.get("gym"))?;
    gym_config.resolve_secrets()?;
    gym_config.resolve_endpoints()?;

    // 2. Build LLM provider (with optional [gym.llm] overrides)
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

    // 3. Open database
    let db_path = data_dir.join(&gym_config.db_path);
    tracing::info!("Loading database from {}", db_path.display());
    let db = Database::open(&db_path)?;
    let db = Arc::new(Mutex::new(db));
    tracing::info!("Database ready!");

    // 4. Create Telegram client only when a token is configured (a confide-only
    //    server runs with no Telegram token); verify the connection if present.
    let telegram = match gym_config.telegram_bot_token.as_deref() {
        Some(token) => {
            let client = TelegramClient::new(token)?;
            let me = client.get_me().await?;
            tracing::info!("Bot connected (id: {})", me.id);
            Some(client)
        }
        None => {
            tracing::info!("No Telegram token configured; Telegram transport disabled");
            None
        }
    };

    let allowed_ids = gym_config.telegram_allowed_ids.clone();

    // 5. Optional feedback reporter (gated by [gym.github] config + per-user beta flag)
    let issue_reporter: Option<Arc<dyn IssueReporter>> = match &gym_config.github {
        Some(github_cfg) => {
            let reporter = GithubIssueReporter::new(github_cfg).context("constructing GitHub issue reporter")?;
            tracing::info!(repo = %github_cfg.repo, "Feedback target configured");
            Some(Arc::new(reporter))
        }
        None => None,
    };

    // 6. Create handler
    let handler = AssistantHandler::new_with_reporter(db.clone(), llm, gym_config.clone(), issue_reporter).await?;

    // 7. Voice pipeline (optional)
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

    Ok((telegram, handler, allowed_ids, voice_pipeline, gym_config))
}

async fn run_polling_loop(
    telegram: &TelegramClient,
    handler: &Arc<AssistantHandler>,
    allowed_ids: &[i64],
    voice_pipeline: Option<&VoicePipeline>,
) -> anyhow::Result<()> {
    let mut offset = 0i64;
    let mut rest_timers = RestTimerRegistry::default();

    loop {
        match telegram.get_updates(offset, 30).await {
            Ok(updates) => {
                for update in updates {
                    offset = update.update_id + 1;
                    if let Some(ref message) = update.message {
                        process_message(telegram, handler, voice_pipeline, message, allowed_ids, &mut rest_timers).await;
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
    rest_timers: &mut RestTimerRegistry,
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
        process_text_message(telegram, handler, message, text, rest_timers).await;
    } else if let Some(ref voice) = message.voice {
        process_voice_message(telegram, handler, voice_pipeline, message, voice, rest_timers).await;
    } else if message.audio.is_some() {
        if let Err(e) =
            telegram.send_message(message.chat.id, "Please use the microphone button to record voice messages directly.", None, None).await
        {
            tracing::warn!("Failed to send audio guidance: {e:#}");
        }
    }
}

async fn process_text_message(
    telegram: &TelegramClient,
    handler: &Arc<AssistantHandler>,
    message: &Message,
    text: &str,
    rest_timers: &mut RestTimerRegistry,
) {
    let _ = telegram.send_chat_action(message.chat.id, "typing").await;

    let (reply_text, parse_mode, timer) = match handler.handle_text_message(message, text).await {
        Ok(reply) => {
            let (rendered, parse_mode) = Telegram.render(&reply.view);
            (rendered, parse_mode, reply.timer)
        }
        Err(e) => {
            tracing::error!("Handler error: {e:#}");
            ("Something went wrong -- please try again later.".to_string(), None, None)
        }
    };

    if let Err(e) = send_long_message(telegram, message.chat.id, &reply_text, parse_mode).await {
        tracing::error!("Failed to send reply: {e:#}");
    }

    rest_timers.apply(telegram, message.chat.id, timer);
}

async fn process_voice_message(
    telegram: &TelegramClient,
    handler: &Arc<AssistantHandler>,
    voice_pipeline: Option<&VoicePipeline>,
    message: &Message,
    voice: &Voice,
    rest_timers: &mut RestTimerRegistry,
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
    let (reply, timer) = match handler.handle_text_message(message, &transcript).await {
        Ok(r) => (to_plain(&r.view), r.timer),
        Err(e) => {
            tracing::error!("Handler error: {e:#}");
            ("I had trouble processing that -- could you try again?".to_string(), None)
        }
    };

    let _ = stop_action.send(());
    rest_timers.apply(telegram, message.chat.id, timer);

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

/// Server-side rest timers for the Telegram transport: one in-flight countdown per
/// chat. Telegram has no persistent client loop, so (unlike the TUI/Android clients,
/// which run the countdown locally) the server runs it and sends the cue messages.
#[derive(Default)]
struct RestTimerRegistry {
    timers: HashMap<i64, tokio::task::JoinHandle<()>>,
}

impl RestTimerRegistry {
    /// Apply a [`TimerSignal`] from a handled reply: arm a fresh countdown, cancel
    /// the running one, or do nothing.
    fn apply(&mut self, telegram: &TelegramClient, chat_id: i64, signal: Option<TimerSignal>) {
        match signal {
            Some(TimerSignal::Arm { duration_secs, exercise }) => self.arm(telegram, chat_id, duration_secs, exercise),
            Some(TimerSignal::Cancel) => self.cancel(chat_id),
            None => {}
        }
    }

    /// Start a countdown for `chat_id`, replacing (and aborting) any in-flight one —
    /// logging a new set restarts the rest.
    fn arm(&mut self, telegram: &TelegramClient, chat_id: i64, duration_secs: u32, exercise: String) {
        self.cancel(chat_id);
        self.timers.insert(chat_id, spawn_telegram_rest_timer(telegram, chat_id, duration_secs, exercise));
    }

    fn cancel(&mut self, chat_id: i64) {
        if let Some(handle) = self.timers.remove(&chat_id) {
            handle.abort();
        }
    }
}

/// Spawn a Telegram rest countdown: run the shared timer engine and turn its cues
/// into chat messages. Telegram gets the 10-seconds warning and the "Go!" message
/// only — the silent 3-2-1 ticks are for clients that beep. Aborting the returned
/// handle drops the cue receiver, which stops the engine task too.
fn spawn_telegram_rest_timer(telegram: &TelegramClient, chat_id: i64, duration_secs: u32, exercise: String) -> tokio::task::JoinHandle<()> {
    let telegram = telegram.clone();
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = tokio::spawn(run_timer(duration_secs, exercise, tx));
        while let Some(cue) = rx.recv().await {
            let message = match cue {
                Cue::Ready { exercise } => Some(format!("Get ready for your next set of {exercise}.")),
                Cue::Countdown(_) => None,
                Cue::Go => Some("Go!".to_string()),
            };
            if let Some(message) = message
                && let Err(e) = telegram.send_message(chat_id, &message, None, None).await
            {
                tracing::warn!("Failed to send rest-timer message: {e:#}");
            }
        }
        let _ = engine.await;
    })
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

/// Splits messages exceeding Telegram's 4096 character limit, taking care to
/// keep `<pre>` blocks balanced when `parse_mode` is `HTML` so Telegram doesn't
/// reject the chunk with "Can't find end of the entity starting at byte offset".
async fn send_long_message(telegram: &TelegramClient, chat_id: i64, text: &str, parse_mode: Option<&str>) -> anyhow::Result<()> {
    for chunk in split_for_telegram(text) {
        telegram.send_message(chat_id, &chunk, parse_mode, None).await?;
    }
    Ok(())
}

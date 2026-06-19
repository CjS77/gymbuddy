use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Context as _;
use corre_core::config::AppLlmConfig;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GymConfig {
    /// Telegram bot token. Absent = Telegram transport disabled (confide-only server).
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
    /// Telegram user IDs allowed to use the bot. Empty = allow all (dev mode).
    #[serde(default)]
    pub telegram_allowed_ids: Vec<i64>,
    #[serde(default = "default_timezone")]
    pub default_timezone: String,
    #[serde(default = "default_history_limit")]
    pub conversation_history_limit: usize,
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_max_message_length")]
    pub max_message_length: usize,
    #[serde(default = "default_session_timeout_hours")]
    pub session_timeout_hours: u32,
    #[serde(default)]
    pub llm: Option<AppLlmConfig>,
    #[serde(default)]
    pub voice: Option<VoiceConfig>,
    #[serde(default)]
    pub github: Option<GithubConfig>,
    /// Encrypted p2p transport for the TUI/Android clients. Absent = disabled.
    #[serde(default)]
    pub confide: Option<ConfideConfig>,
    /// Inter-set rest timer. Always present (defaults applied when the block is
    /// omitted) so the feature is on out of the box.
    #[serde(default)]
    pub rest_timer: RestTimerConfig,
}

/// Rest-timer durations (seconds) keyed by the perceived difficulty of the last
/// set, plus a flat override for supersets and the default toggle for new users.
///
/// Defaults mirror the corre rest-timer PR: easy 120 / medium 180 / hard &
/// failure 300, supersets a flat 60.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct RestTimerConfig {
    /// Rest-timer preference seeded onto each newly-registered user. Existing users
    /// keep their stored choice; `/timers` flips it per user thereafter.
    #[serde(default = "default_true")]
    pub default_enabled: bool,
    #[serde(default = "default_easy_secs")]
    pub easy_secs: u32,
    #[serde(default = "default_medium_secs")]
    pub medium_secs: u32,
    #[serde(default = "default_hard_secs")]
    pub hard_secs: u32,
    #[serde(default = "default_failure_secs")]
    pub failure_secs: u32,
    /// Flat rest while supersetting (≥2 open entries), overriding difficulty.
    #[serde(default = "default_superset_secs")]
    pub superset_secs: u32,
}

impl Default for RestTimerConfig {
    fn default() -> Self {
        Self {
            default_enabled: true,
            easy_secs: default_easy_secs(),
            medium_secs: default_medium_secs(),
            hard_secs: default_hard_secs(),
            failure_secs: default_failure_secs(),
            superset_secs: default_superset_secs(),
        }
    }
}

impl RestTimerConfig {
    /// Rest duration after a set: supersets get the flat `superset_secs` override
    /// regardless of difficulty; otherwise the difficulty-keyed duration applies.
    /// A set logged without a perceived difficulty is treated as `Medium`.
    pub fn rest_secs_for(&self, difficulty: Option<crate::db::Difficulty>, is_superset: bool) -> u32 {
        use crate::db::Difficulty;
        if is_superset {
            return self.superset_secs;
        }
        match difficulty.unwrap_or(Difficulty::Medium) {
            Difficulty::Easy => self.easy_secs,
            Difficulty::Medium => self.medium_secs,
            Difficulty::Hard => self.hard_secs,
            Difficulty::Failure => self.failure_secs,
        }
    }
}

/// Configuration for the confide (encrypted p2p) transport.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfideConfig {
    /// File holding the server's ed25519 identity keypair. Created on first run;
    /// its public key is what clients dial. Defaults next to the database.
    #[serde(default = "default_keystore_path")]
    pub keystore_path: PathBuf,
    /// Hex public keys allowed to connect. Empty = allow all (dev mode), mirroring
    /// `telegram_allowed_ids`.
    #[serde(default)]
    pub allowed_pubkeys: Vec<String>,
    /// Use the public n0 relay + discovery (true) or direct/LAN connections only
    /// (false, e.g. for tests). Defaults to true.
    #[serde(default = "default_true")]
    pub relay: bool,
}

/// Configuration for filing GitHub issues via the `/feedback` slash command.
///
/// The endpoint host is hardcoded to `https://api.github.com`; only the
/// `owner/repo` slug is configurable. The token must be passed as a `${VAR}`
/// reference and is resolved through the secret resolver at startup.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GithubConfig {
    /// "owner/repo" — restricted by allowlist to alphanumeric + `._-`.
    pub repo: String,
    pub token: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

static REPO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$").unwrap());

impl GithubConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(REPO_RE.is_match(&self.repo), "invalid github.repo (expected 'owner/repo'): {}", self.repo);
        let (owner, name) = self.repo.split_once('/').expect("regex guarantees exactly one slash");
        for component in [owner, name] {
            anyhow::ensure!(component != "." && component != "..", "invalid github.repo component: {component:?}");
        }
        anyhow::ensure!(!self.token.is_empty(), "github.token is empty");
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VoiceConfig {
    #[serde(default = "default_true")]
    pub stt_enabled: bool,
    #[serde(default = "default_stt_url")]
    pub stt_url: String,
    #[serde(default = "default_stt_language")]
    pub stt_language: String,
    #[serde(default = "default_true")]
    pub tts_enabled: bool,
    #[serde(default = "default_tts_url")]
    pub tts_url: String,
    #[serde(default = "default_tts_voice")]
    pub tts_voice: String,
    /// Piper speaker name for multi-speaker models (e.g. "prudence", "spike", "obadiah", "poppy").
    /// Empty string = use model default.
    #[serde(default)]
    pub tts_speaker: String,
    /// Speaking speed multiplier. 1.0 = normal, 1.5 = 50% faster, 0.75 = 25% slower.
    /// Range: 0.25 to 4.0.
    #[serde(default = "default_tts_speed")]
    pub tts_speed: f32,
    #[serde(default)]
    pub response_mode: ResponseMode,
    #[serde(default = "default_max_voice_duration")]
    pub max_voice_duration_secs: u32,
}

impl VoiceConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        url::Url::parse(&self.stt_url).with_context(|| format!("invalid stt_url: {}", self.stt_url))?;
        url::Url::parse(&self.tts_url).with_context(|| format!("invalid tts_url: {}", self.tts_url))?;
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseMode {
    Voice,
    Text,
    #[default]
    Both,
}

fn default_true() -> bool {
    true
}

fn default_stt_url() -> String {
    "http://whisper:5005".into()
}

fn default_stt_language() -> String {
    "en".into()
}

fn default_tts_url() -> String {
    "http://piper:5000".into()
}

fn default_tts_voice() -> String {
    "en_GB-semaine-medium".into()
}

fn default_tts_speed() -> f32 {
    1.0
}

fn default_max_voice_duration() -> u32 {
    60
}

fn default_timezone() -> String {
    "Europe/London".into()
}

fn default_keystore_path() -> PathBuf {
    PathBuf::from("confide-identity.key")
}

fn default_history_limit() -> usize {
    20
}

fn default_db_path() -> String {
    "gym-tracker.db".into()
}

fn default_max_message_length() -> usize {
    2000
}

fn default_session_timeout_hours() -> u32 {
    4
}

fn default_easy_secs() -> u32 {
    120
}

fn default_medium_secs() -> u32 {
    180
}

fn default_hard_secs() -> u32 {
    300
}

fn default_failure_secs() -> u32 {
    300
}

fn default_superset_secs() -> u32 {
    60
}

impl GymConfig {
    pub fn from_toml_table(table: Option<&toml::Value>) -> anyhow::Result<Self> {
        table.cloned().ok_or_else(|| anyhow::anyhow!("missing [gym] section in corre.toml")).and_then(|v| v.try_into().map_err(Into::into))
    }

    /// Resolve `${VAR}` references in secret fields (the Telegram bot token,
    /// and the GitHub PAT used by `/feedback` when configured).
    pub fn resolve_secrets(&mut self) -> anyhow::Result<()> {
        if let Some(token) = &self.telegram_bot_token {
            self.telegram_bot_token = Some(corre_core::secret::resolve_value(token).context("resolving TELEGRAM_GYM_BOT_TOKEN")?);
        }
        if let Some(ref mut github) = self.github {
            github.token = corre_core::secret::resolve_value(&github.token).context("resolving github.token")?;
            github.validate()?;
        }
        Ok(())
    }

    /// Resolve `${VAR}` references in non-secret voice endpoint URLs.
    ///
    /// Service URLs are not secrets, so they are resolved through the plain
    /// `resolve_env_ref` path rather than the secret resolver.
    pub fn resolve_endpoints(&mut self) -> anyhow::Result<()> {
        if let Some(ref mut voice) = self.voice {
            voice.stt_url = corre_core::config::resolve_env_ref(&voice.stt_url).context("resolving stt_url")?;
            voice.tts_url = corre_core::config::resolve_env_ref(&voice.tts_url).context("resolving tts_url")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_gym_toml(voice_section: &str) -> toml::Value {
        let s = format!(
            r#"
            telegram_bot_token = "123456:ABC"
            {voice_section}
            "#
        );
        toml::from_str(&s).unwrap()
    }

    #[test]
    fn voice_config_absent() {
        let val = minimal_gym_toml("");
        let config: GymConfig = val.try_into().unwrap();
        assert!(config.voice.is_none());
    }

    #[test]
    fn voice_config_defaults() {
        let val = minimal_gym_toml("[voice]");
        let config: GymConfig = val.try_into().unwrap();
        let voice = config.voice.unwrap();
        assert!(voice.stt_enabled);
        assert_eq!(voice.stt_url, "http://whisper:5005");
        assert_eq!(voice.stt_language, "en");
        assert!(voice.tts_enabled);
        assert_eq!(voice.tts_url, "http://piper:5000");
        assert_eq!(voice.tts_voice, "en_GB-semaine-medium");
        assert_eq!(voice.tts_speaker, "");
        assert!((voice.tts_speed - 1.0).abs() < f32::EPSILON);
        assert_eq!(voice.response_mode, ResponseMode::Both);
        assert_eq!(voice.max_voice_duration_secs, 60);
    }

    #[test]
    fn voice_config_custom() {
        let val = minimal_gym_toml(
            r#"
            [voice]
            stt_enabled = false
            stt_url = "http://localhost:9090"
            stt_language = "de"
            tts_enabled = false
            tts_url = "http://localhost:9091"
            tts_voice = "de_DE-thorsten-medium"
            tts_speaker = "spike"
            tts_speed = 1.3
            response_mode = "voice"
            max_voice_duration_secs = 30
            "#,
        );
        let config: GymConfig = val.try_into().unwrap();
        let voice = config.voice.unwrap();
        assert!(!voice.stt_enabled);
        assert_eq!(voice.stt_url, "http://localhost:9090");
        assert_eq!(voice.stt_language, "de");
        assert!(!voice.tts_enabled);
        assert_eq!(voice.tts_url, "http://localhost:9091");
        assert_eq!(voice.tts_voice, "de_DE-thorsten-medium");
        assert_eq!(voice.tts_speaker, "spike");
        assert!((voice.tts_speed - 1.3).abs() < 0.01);
        assert_eq!(voice.response_mode, ResponseMode::Voice);
        assert_eq!(voice.max_voice_duration_secs, 30);
    }

    #[test]
    fn response_mode_variants() {
        let v: ResponseMode = serde_json::from_str(r#""voice""#).unwrap();
        assert_eq!(v, ResponseMode::Voice);
        let v: ResponseMode = serde_json::from_str(r#""text""#).unwrap();
        assert_eq!(v, ResponseMode::Text);
        let v: ResponseMode = serde_json::from_str(r#""both""#).unwrap();
        assert_eq!(v, ResponseMode::Both);
    }

    #[test]
    fn voice_config_invalid_url() {
        let voice = VoiceConfig {
            stt_enabled: true,
            stt_url: "not-a-url".into(),
            stt_language: "en".into(),
            tts_enabled: true,
            tts_url: "http://piper:5000".into(),
            tts_voice: "en_GB-semaine-medium".into(),
            tts_speaker: String::new(),
            tts_speed: 1.0,
            response_mode: ResponseMode::Both,
            max_voice_duration_secs: 60,
        };
        assert!(voice.validate().is_err());
    }

    #[test]
    fn resolve_endpoints_expands_env_refs_and_keeps_literals() {
        unsafe { std::env::set_var("TEST_GYM_STT_URL", "https://stt.example.com") };
        let val = minimal_gym_toml(
            r#"
            [voice]
            stt_url = "${TEST_GYM_STT_URL}"
            tts_url = "http://piper:5000"
            "#,
        );
        let mut config: GymConfig = val.try_into().unwrap();
        config.resolve_endpoints().unwrap();
        let voice = config.voice.unwrap();
        assert_eq!(voice.stt_url, "https://stt.example.com");
        assert_eq!(voice.tts_url, "http://piper:5000");
        unsafe { std::env::remove_var("TEST_GYM_STT_URL") };
    }

    #[test]
    fn resolve_endpoints_missing_env_ref_errors() {
        let val = minimal_gym_toml(
            r#"
            [voice]
            stt_url = "${DEFINITELY_NOT_SET_GYM_XYZ_999}"
            "#,
        );
        let mut config: GymConfig = val.try_into().unwrap();
        assert!(config.resolve_endpoints().is_err());
    }

    #[test]
    fn resolve_endpoints_noop_without_voice() {
        let val = minimal_gym_toml("");
        let mut config: GymConfig = val.try_into().unwrap();
        assert!(config.resolve_endpoints().is_ok());
    }

    #[test]
    fn github_config_parses_and_resolves_secret() {
        unsafe { std::env::set_var("TEST_GH_TOKEN_XYZ", "ghp_resolved") };
        let val = minimal_gym_toml(
            r#"
            [github]
            repo = "corre-pa/corre"
            token = "${TEST_GH_TOKEN_XYZ}"
            labels = ["beta-feedback", "triage"]
            "#,
        );
        let mut config: GymConfig = val.try_into().unwrap();
        config.resolve_secrets().unwrap();
        let gh = config.github.unwrap();
        assert_eq!(gh.repo, "corre-pa/corre");
        assert_eq!(gh.token, "ghp_resolved");
        assert_eq!(gh.labels, vec!["beta-feedback".to_string(), "triage".to_string()]);
        unsafe { std::env::remove_var("TEST_GH_TOKEN_XYZ") };
    }

    #[test]
    fn github_config_rejects_invalid_repo() {
        for repo in &["no-slash", "../etc", "owner/repo/extra", "owner /repo", ""] {
            let cfg = GithubConfig { repo: (*repo).to_string(), token: "x".into(), labels: vec![] };
            assert!(cfg.validate().is_err(), "repo {repo:?} should be rejected");
        }
    }

    #[test]
    fn github_config_accepts_valid_repos() {
        for repo in &["owner/repo", "Owner.Name/repo_1", "a-b/c.d-e_f", "Org/Project.Name"] {
            let cfg = GithubConfig { repo: (*repo).to_string(), token: "x".into(), labels: vec![] };
            assert!(cfg.validate().is_ok(), "repo {repo:?} should be accepted");
        }
    }

    #[test]
    fn github_config_absent_by_default() {
        let val = minimal_gym_toml("");
        let config: GymConfig = val.try_into().unwrap();
        assert!(config.github.is_none());
    }

    #[test]
    fn telegram_only_config_parses() {
        let val = minimal_gym_toml("");
        let config: GymConfig = val.try_into().unwrap();
        assert_eq!(config.telegram_bot_token.as_deref(), Some("123456:ABC"));
        assert!(config.confide.is_none());
    }

    #[test]
    fn confide_only_config_parses_without_telegram_token() {
        let val: toml::Value = toml::from_str(
            r#"
            [confide]
            keystore_path = "/tmp/gym-key"
            allowed_pubkeys = ["abc123"]
            relay = false
            "#,
        )
        .unwrap();
        let config: GymConfig = val.try_into().unwrap();
        assert!(config.telegram_bot_token.is_none());
        let confide = config.confide.unwrap();
        assert_eq!(confide.keystore_path, std::path::PathBuf::from("/tmp/gym-key"));
        assert_eq!(confide.allowed_pubkeys, vec!["abc123".to_string()]);
        assert!(!confide.relay);
    }

    #[test]
    fn confide_config_defaults() {
        let val = minimal_gym_toml("[confide]");
        let config: GymConfig = val.try_into().unwrap();
        let confide = config.confide.unwrap();
        assert!(confide.relay, "relay defaults to true");
        assert!(confide.allowed_pubkeys.is_empty());
        assert_eq!(confide.keystore_path, default_keystore_path());
    }

    #[test]
    fn rest_timer_defaults_when_absent() {
        let val = minimal_gym_toml("");
        let config: GymConfig = val.try_into().unwrap();
        assert_eq!(config.rest_timer, RestTimerConfig::default());
        assert!(config.rest_timer.default_enabled);
    }

    #[test]
    fn rest_timer_custom_values_parse() {
        let val = minimal_gym_toml(
            r#"
            [rest_timer]
            default_enabled = false
            easy_secs = 90
            superset_secs = 45
            "#,
        );
        let config: GymConfig = val.try_into().unwrap();
        assert!(!config.rest_timer.default_enabled);
        assert_eq!(config.rest_timer.easy_secs, 90);
        assert_eq!(config.rest_timer.superset_secs, 45);
        // Unspecified fields keep their defaults.
        assert_eq!(config.rest_timer.medium_secs, 180);
        assert_eq!(config.rest_timer.hard_secs, 300);
    }

    #[test]
    fn rest_secs_maps_difficulty_and_superset() {
        use crate::db::Difficulty;
        let cfg = RestTimerConfig::default();
        assert_eq!(cfg.rest_secs_for(Some(Difficulty::Easy), false), 120);
        assert_eq!(cfg.rest_secs_for(Some(Difficulty::Medium), false), 180);
        assert_eq!(cfg.rest_secs_for(Some(Difficulty::Hard), false), 300);
        assert_eq!(cfg.rest_secs_for(Some(Difficulty::Failure), false), 300);
        // No difficulty given => treated as Medium.
        assert_eq!(cfg.rest_secs_for(None, false), 180);
        // Superset overrides difficulty entirely.
        assert_eq!(cfg.rest_secs_for(Some(Difficulty::Hard), true), 60);
        assert_eq!(cfg.rest_secs_for(None, true), 60);
    }

    #[test]
    fn voice_config_valid_urls() {
        let voice = VoiceConfig {
            stt_enabled: true,
            stt_url: "http://whisper:5005".into(),
            stt_language: "en".into(),
            tts_enabled: true,
            tts_url: "http://piper:5000".into(),
            tts_voice: "en_GB-semaine-medium".into(),
            tts_speaker: "prudence".into(),
            tts_speed: 1.2,
            response_mode: ResponseMode::Both,
            max_voice_duration_secs: 60,
        };
        assert!(voice.validate().is_ok());
    }
}

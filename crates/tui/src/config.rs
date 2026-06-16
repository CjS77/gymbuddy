//! Configuration resolution: CLI args layered over an optional `gymbuddy-tui.toml`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use gymbuddy_client::{ConnectOptions, default_identity_path};
use serde::Deserialize;

/// Seconds to wait for the server connection before giving up.
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Command-line arguments. Anything omitted falls back to `--config` file values.
#[derive(Parser, Debug)]
#[command(name = "gymbuddy-tui", about = "GymBuddy terminal client")]
pub struct Cli {
    /// Server's ed25519 public key (64-char hex) to connect to.
    #[arg(long)]
    server: Option<String>,
    /// Path to a gymbuddy-tui.toml providing defaults.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Disable the public relay and connect directly (requires --addr). For LAN/tests.
    #[arg(long)]
    no_relay: bool,
    /// Direct socket address(es) of the server, used with --no-relay. Repeatable.
    #[arg(long = "addr")]
    addrs: Vec<SocketAddr>,
    /// Override the identity keystore path.
    #[arg(long)]
    keystore: Option<PathBuf>,
    /// Pre-fill the registration name (skips the name prompt).
    #[arg(long)]
    pub name: Option<String>,
    /// Pre-fill the registration timezone (skips the timezone prompt).
    #[arg(long)]
    pub timezone: Option<String>,
    /// Seconds to wait for the server connection before giving up (default 10).
    #[arg(long)]
    connect_timeout: Option<u64>,
}

#[derive(Deserialize, Default)]
struct FileConfig {
    server_pubkey: Option<String>,
    relay: Option<bool>,
    keystore_path: Option<PathBuf>,
    #[serde(default)]
    server_addrs: Vec<SocketAddr>,
    name: Option<String>,
    timezone: Option<String>,
    connect_timeout_secs: Option<u64>,
}

/// Fully resolved client configuration.
pub struct ResolvedConfig {
    pub connect: ConnectOptions,
    pub name: Option<String>,
    pub timezone: Option<String>,
    pub connect_timeout: Duration,
}

impl Cli {
    /// Merge CLI args over an optional config file into [`ConnectOptions`].
    pub fn resolve(self) -> anyhow::Result<ResolvedConfig> {
        let file = match &self.config {
            Some(path) => {
                let text = std::fs::read_to_string(path).with_context(|| format!("reading config {}", path.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?
            }
            None => FileConfig::default(),
        };

        let server_pubkey_hex = self
            .server
            .or(file.server_pubkey)
            .context("server pubkey required: pass --server <hex> or set server_pubkey in --config")?;
        let relay = if self.no_relay { false } else { file.relay.unwrap_or(true) };
        let keystore_path = self.keystore.or(file.keystore_path).unwrap_or_else(default_identity_path);
        let server_addrs = if self.addrs.is_empty() { file.server_addrs } else { self.addrs };
        let connect_timeout =
            Duration::from_secs(self.connect_timeout.or(file.connect_timeout_secs).unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS));

        Ok(ResolvedConfig {
            connect: ConnectOptions { server_pubkey_hex, relay, server_addrs, keystore_path },
            name: self.name.or(file.name),
            timezone: self.timezone.or(file.timezone),
            connect_timeout,
        })
    }
}

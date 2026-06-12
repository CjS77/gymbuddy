//! Reusable, UI-agnostic GymBuddy client core.
//!
//! This is the shared layer the TUI sits on today and that Android will bind to
//! (via UniFFI) tomorrow — there is no rendering code here. It owns the confide
//! connection and the on-disk ed25519 identity, and exposes the wire protocol
//! ([`gymbuddy_proto`]) as a small request/response API.
//!
//! The connection is split into two halves so a UI event loop can `select!` over
//! both without borrow conflicts:
//! - [`GymClient`] — send requests (`&self`) and read your own public key;
//! - [`Responses`] — an inbound stream of decoded [`ServerResponse`]s, fed by a
//!   background reader task.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use confide::{Config, FileKeyStore, Identity, Message, Node, Relay, Session, peer_id_from_hex, peer_id_to_hex};
use gymbuddy_proto::{ClientRequest, ServerResponse, decode_response, encode_request};
use tokio::sync::mpsc;

/// Default on-disk location of the client's ed25519 identity keypair.
///
/// `dirs::data_dir()/gymbuddy/identity.key`, falling back to the current directory
/// if no data dir is available. The pubkey derived from this key *is* the user's
/// identity, so persisting it keeps the same account across runs.
pub fn default_identity_path() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("gymbuddy").join("identity.key"))
        .unwrap_or_else(|| PathBuf::from("gymbuddy-identity.key"))
}

/// How to reach the server.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Server's ed25519 public key (64-char hex) — what you dial.
    pub server_pubkey_hex: String,
    /// Use the public n0 relay + discovery (true) or direct connections only
    /// (false). When false, `server_addrs` must be supplied.
    pub relay: bool,
    /// Direct socket addresses of the server, used only when `relay == false`
    /// (LAN / tests). Ignored when relaying.
    pub server_addrs: Vec<SocketAddr>,
    /// Where to load/generate this client's identity keypair.
    pub keystore_path: PathBuf,
}

impl ConnectOptions {
    /// Relayed connection (the normal case) to `server_pubkey_hex`, using the
    /// default identity path.
    pub fn relayed(server_pubkey_hex: impl Into<String>) -> Self {
        Self { server_pubkey_hex: server_pubkey_hex.into(), relay: true, server_addrs: Vec::new(), keystore_path: default_identity_path() }
    }
}

/// The send half of a confide connection plus the local identity.
pub struct GymClient {
    // Kept alive for the lifetime of the connection: dropping the node closes the
    // iroh endpoint and tears down the session.
    _node: Node,
    session: Arc<Session>,
    my_pubkey: String,
}

/// The inbound half: decoded [`ServerResponse`]s as they arrive.
pub struct Responses {
    rx: mpsc::UnboundedReceiver<anyhow::Result<ServerResponse>>,
}

impl GymClient {
    /// Connect to the server, returning the send half and the inbound stream.
    ///
    /// Loads (or generates, on first run) the identity at `opts.keystore_path`,
    /// spawns a confide node, dials the server, and starts a background task that
    /// forwards every inbound GymBuddy response onto [`Responses`].
    pub async fn connect(opts: ConnectOptions) -> anyhow::Result<(GymClient, Responses)> {
        ensure_parent_dir(&opts.keystore_path)?;
        let identity = Identity::load_or_generate(&FileKeyStore::new(&opts.keystore_path)).context("loading client identity")?;

        let config = if opts.relay { Config::client() } else { Config::client().with_relay(Relay::Disabled) };
        let node = Node::spawn(identity, config).await.context("spawning confide node")?;
        let my_pubkey = peer_id_to_hex(&node.peer_id());

        let server = peer_id_from_hex(&opts.server_pubkey_hex).context("invalid server pubkey")?;
        let session = if opts.relay {
            node.connect(server).await
        } else {
            anyhow::ensure!(!opts.server_addrs.is_empty(), "direct connection requires server_addrs");
            node.connect_direct(server, &opts.server_addrs).await
        }
        .context("connecting to server")?;
        let session = Arc::new(session);

        let (tx, rx) = mpsc::unbounded_channel();
        spawn_reader(session.clone(), tx);

        Ok((GymClient { _node: node, session, my_pubkey }, Responses { rx }))
    }

    /// This client's own public key (64-char hex) — show it so the user can
    /// whitelist it on the server.
    pub fn my_pubkey_hex(&self) -> &str {
        &self.my_pubkey
    }

    /// Send a request without waiting for the reply. The reply (if any) arrives on
    /// [`Responses`].
    pub async fn send(&self, req: &ClientRequest) -> anyhow::Result<()> {
        let data = encode_request(req).context("encoding request")?;
        self.session
            .send(Message::Custom { kind: gymbuddy_proto::KIND.to_string(), data: Bytes::from(data) })
            .await
            .context("sending request")?;
        Ok(())
    }

    /// Send a request and await the next response. Convenient for the sequential
    /// handshake (`Hello`, `Register`); during the main loop prefer [`Self::send`]
    /// plus selecting on [`Responses`] so the UI stays responsive.
    pub async fn request(&self, responses: &mut Responses, req: &ClientRequest) -> anyhow::Result<ServerResponse> {
        self.send(req).await?;
        responses.next().await.context("connection closed before a response arrived")?
    }
}

impl Responses {
    /// Await the next decoded response, or `None` once the connection closes.
    pub async fn next(&mut self) -> Option<anyhow::Result<ServerResponse>> {
        self.rx.recv().await
    }
}

/// Background task: read inbound messages, decode GymBuddy responses, forward them.
fn spawn_reader(session: Arc<Session>, tx: mpsc::UnboundedSender<anyhow::Result<ServerResponse>>) {
    tokio::spawn(async move {
        loop {
            match session.recv().await {
                Ok(Some(Message::Custom { kind, data })) if kind == gymbuddy_proto::KIND => {
                    let decoded = decode_response(&data).context("decoding response");
                    if tx.send(decoded).is_err() {
                        break; // receiver dropped
                    }
                }
                Ok(Some(_)) => continue, // non-GymBuddy message; ignore
                Ok(None) => break,       // peer closed cleanly
                Err(e) => {
                    let _ = tx.send(Err(anyhow::Error::from(e).context("receiving from server")));
                    break;
                }
            }
        }
    });
}

fn ensure_parent_dir(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("creating identity dir {}", parent.display()))?;
    }
    Ok(())
}

//! Confide (encrypted p2p) transport for the TUI / Android clients.
//!
//! Modeled on `confide/examples/echo_server.rs`: spawn a node, accept inbound
//! sessions, and dispatch each decoded [`ClientRequest`] to the shared
//! [`AssistantHandler`]. The connecting peer's ed25519 public key (hex) *is* the
//! user identity — there are no passwords.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use bytes::Bytes;
use confide::{Config, FileKeyStore, Identity, Message, Node, Relay, Session, peer_id_to_hex};
use futures::StreamExt as _;
use gymbuddy_proto::{ClientRequest, ServerResponse, decode_request, encode_response};

use crate::assistant::AssistantHandler;
use crate::config::ConfideConfig;

/// A bound confide endpoint, ready to accept sessions.
///
/// Splitting bind from run lets the operator log how to dial the server — and lets
/// integration tests learn the public key and direct addresses for a relay-less
/// connection.
pub struct ConfideServer {
    node: Node,
    allowed: Arc<Vec<String>>,
}

impl ConfideServer {
    /// Bind the endpoint described by `cfg`, loading or generating its identity.
    pub async fn bind(cfg: &ConfideConfig) -> anyhow::Result<Self> {
        let identity = Identity::load_or_generate(&FileKeyStore::new(&cfg.keystore_path)).context("loading confide identity")?;
        let node_config = if cfg.relay { Config::server() } else { Config::server().with_relay(Relay::Disabled) };
        let node = Node::spawn(identity, node_config).await.context("spawning confide node")?;
        Ok(Self { node, allowed: Arc::new(cfg.allowed_pubkeys.clone()) })
    }

    /// The server's public key (hex) — what clients dial.
    pub fn pubkey_hex(&self) -> String {
        peer_id_to_hex(&self.node.peer_id())
    }

    /// Socket addresses the endpoint is bound to, for direct (relay-less) dialing.
    pub fn direct_addresses(&self) -> Vec<SocketAddr> {
        self.node.direct_addresses()
    }

    /// Accept sessions until the endpoint closes, handling each on its own task.
    pub async fn run(self, handler: Arc<AssistantHandler>) -> anyhow::Result<()> {
        let mut incoming = self.node.incoming();
        while let Some(session) = incoming.next().await {
            let handler = handler.clone();
            let allowed = self.allowed.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_session(session, handler, allowed).await {
                    tracing::warn!("confide session ended with error: {e:#}");
                }
            });
        }
        Ok(())
    }
}

/// Run the confide transport until the node's incoming stream ends.
///
/// Loads (or generates, on first run) the server identity from `cfg.keystore_path`
/// and logs the public key clients must dial.
pub async fn serve(handler: Arc<AssistantHandler>, cfg: ConfideConfig) -> anyhow::Result<()> {
    let server = ConfideServer::bind(&cfg).await?;
    tracing::info!("confide transport listening; server key: {}", server.pubkey_hex());
    server.run(handler).await
}

/// Handle one authenticated session: enforce the allow-list, then loop over
/// inbound GymBuddy envelopes and answer each.
async fn handle_session(session: Session, handler: Arc<AssistantHandler>, allowed: Arc<Vec<String>>) -> anyhow::Result<()> {
    let pubkey = peer_id_to_hex(&session.peer());

    // Empty allow-list ⇒ allow all (dev mode), mirroring `telegram_allowed_ids`.
    if !allowed.is_empty() && !allowed.iter().any(|k| k == &pubkey) {
        tracing::warn!("rejecting confide session from unauthorized pubkey {pubkey}");
        let _ = send_response(&session, &ServerResponse::Error { message: "not authorized".into() }).await;
        return Ok(());
    }

    tracing::info!("confide session opened: {pubkey}");
    while let Some(msg) = session.recv().await? {
        let Message::Custom { kind, data } = msg else {
            tracing::debug!("ignoring non-Custom confide message");
            continue;
        };
        if kind != gymbuddy_proto::KIND {
            tracing::debug!(%kind, "ignoring confide message of unknown kind");
            continue;
        }
        let response = match decode_request(&data) {
            Ok(req) => dispatch(&handler, &pubkey, req).await,
            Err(e) => ServerResponse::Error { message: format!("malformed request: {e}") },
        };
        send_response(&session, &response).await?;
    }
    tracing::info!("confide session closed: {pubkey}");
    Ok(())
}

/// Map a decoded request to a response, reusing the existing assistant for chat.
async fn dispatch(handler: &AssistantHandler, pubkey: &str, req: ClientRequest) -> ServerResponse {
    match req {
        ClientRequest::Hello => match handler.ensure_user_by_pubkey(pubkey).await {
            Ok(Some(user)) => ServerResponse::Welcome { name: user.name },
            Ok(None) => ServerResponse::NeedsRegistration,
            Err(e) => error_response(e),
        },
        ClientRequest::Register { name, timezone } => match handler.register_user(pubkey, &name, &timezone).await {
            Ok(user) => ServerResponse::Welcome { name: user.name },
            Err(e) => error_response(e),
        },
        ClientRequest::Chat { text } => match handler.ensure_user_by_pubkey(pubkey).await {
            Ok(Some(user)) => match handler.handle_message_for_user(&user, &text, "confide").await {
                // Send the domain view straight over the wire; the client renders it.
                // The optional rest-timer directive rides along — the client runs the
                // countdown locally.
                Ok(reply) => ServerResponse::Reply { view: reply.view, timer: reply.timer },
                Err(e) => error_response(e),
            },
            Ok(None) => ServerResponse::NeedsRegistration,
            Err(e) => error_response(e),
        },
    }
}

fn error_response(e: anyhow::Error) -> ServerResponse {
    tracing::error!("confide request failed: {e:#}");
    ServerResponse::Error { message: e.to_string() }
}

async fn send_response(session: &Session, resp: &ServerResponse) -> anyhow::Result<()> {
    let data = encode_response(resp).context("encoding response")?;
    session.send(Message::Custom { kind: gymbuddy_proto::KIND.to_string(), data: Bytes::from(data) }).await?;
    Ok(())
}


//! Wire protocol shared by the GymBuddy server and all clients (TUI, Android, …).
//!
//! One envelope per direction ([`ClientRequest`] / [`ServerResponse`]), serialized
//! with [`postcard`] for compactness and carried inside confide's
//! `Message::Custom { kind: `[`KIND`]`, data }`. The crate is pure data — no I/O —
//! so every client and the server share exactly one definition of the wire format.

use serde::{Deserialize, Serialize};

/// Discriminator placed in confide's `Message::Custom { kind, .. }` so the peer
/// knows the payload is a GymBuddy v1 envelope.
pub const KIND: &str = "gymbuddy/v1";

/// A request sent from a client to the server.
///
/// Chat is sequential, so no request id is carried yet; later structured queries
/// (progress, goals, …) can add one when correlation becomes necessary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientRequest {
    /// "Who am I?" — resolves this peer's pubkey to a [`ServerResponse::Welcome`]
    /// or, if unknown, [`ServerResponse::NeedsRegistration`].
    Hello,
    /// Explicitly register the connecting pubkey as a new user.
    Register { name: String, timezone: String },
    /// Free-form chat text — the main path into the assistant. Server-side slash
    /// commands (`/status`, `/history`, …) travel as plain chat text here too.
    Chat { text: String },
}

/// A response sent from the server to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerResponse {
    /// The pubkey is a known, registered user.
    Welcome { name: String },
    /// The pubkey is unknown; the client should collect a name/timezone and send
    /// [`ClientRequest::Register`].
    NeedsRegistration,
    /// Plain-text assistant reply (mirrors the server's `handler::Reply` text).
    Reply { text: String },
    /// The server could not process the request.
    Error { message: String },
}

/// Serialize a [`ClientRequest`] to postcard bytes for a `Message::Custom` payload.
pub fn encode_request(req: &ClientRequest) -> postcard::Result<Vec<u8>> {
    postcard::to_allocvec(req)
}

/// Deserialize a [`ClientRequest`] from a `Message::Custom` payload.
pub fn decode_request(data: &[u8]) -> postcard::Result<ClientRequest> {
    postcard::from_bytes(data)
}

/// Serialize a [`ServerResponse`] to postcard bytes for a `Message::Custom` payload.
pub fn encode_response(resp: &ServerResponse) -> postcard::Result<Vec<u8>> {
    postcard::to_allocvec(resp)
}

/// Deserialize a [`ServerResponse`] from a `Message::Custom` payload.
pub fn decode_response(data: &[u8]) -> postcard::Result<ServerResponse> {
    postcard::from_bytes(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_request(req: ClientRequest) {
        let bytes = encode_request(&req).expect("encode");
        let decoded = decode_request(&bytes).expect("decode");
        assert_eq!(req, decoded);
    }

    fn roundtrip_response(resp: ServerResponse) {
        let bytes = encode_response(&resp).expect("encode");
        let decoded = decode_response(&bytes).expect("decode");
        assert_eq!(resp, decoded);
    }

    #[test]
    fn client_request_variants_roundtrip() {
        roundtrip_request(ClientRequest::Hello);
        roundtrip_request(ClientRequest::Register { name: "Alice".into(), timezone: "Europe/London".into() });
        roundtrip_request(ClientRequest::Chat { text: "3 sets of bench press, 80kg, 8 reps".into() });
    }

    #[test]
    fn server_response_variants_roundtrip() {
        roundtrip_response(ServerResponse::Welcome { name: "Alice".into() });
        roundtrip_response(ServerResponse::NeedsRegistration);
        roundtrip_response(ServerResponse::Reply { text: "Logged 3 sets of bench press.".into() });
        roundtrip_response(ServerResponse::Error { message: "unknown user".into() });
    }

    #[test]
    fn decode_rejects_garbage() {
        // Postcard enum discriminants are varints; 0xFF.. is not a valid 3-variant tag.
        assert!(decode_request(&[0xFF, 0xFF, 0xFF]).is_err());
    }
}

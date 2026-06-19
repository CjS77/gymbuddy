//! Wire protocol shared by the GymBuddy server and all clients (TUI, Android, …).
//!
//! One envelope per direction ([`ClientRequest`] / [`ServerResponse`]), serialized
//! with [`postcard`] for compactness and carried inside confide's
//! `Message::Custom { kind: `[`KIND`]`, data }`. The crate is pure data — no I/O —
//! so every client and the server share exactly one definition of the wire format.

use serde::{Deserialize, Serialize};

pub mod view;
pub use view::{
    CatalogEntry, CatalogGroup, CatalogView, ExerciseLog, HealthNote, HistoryView, Measurement, Render, SessionSummaryView, SessionView,
    SetLine, StatusView, View,
};

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
///
/// Not `Eq`: [`View`] carries floating-point set values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerResponse {
    /// The pubkey is a known, registered user.
    Welcome { name: String },
    /// The pubkey is unknown; the client should collect a name/timezone and send
    /// [`ClientRequest::Register`].
    NeedsRegistration,
    /// An assistant reply as a domain [`View`]; the client renders it natively.
    ///
    /// `timer` optionally carries a rest-timer directive that rides along with the
    /// reply: the client (TUI/Android) runs the countdown locally, rendering cues
    /// however it likes (the Telegram path arms its timer server-side instead, so it
    /// never reads this field). Defaults to `None` for replies that don't touch the
    /// timer.
    Reply { view: View, timer: Option<TimerSignal> },
    /// The server could not process the request.
    Error { message: String },
}

/// A rest-timer directive attached to a [`ServerResponse::Reply`].
///
/// The server decides the rest *duration* (from the perceived difficulty of the
/// last set and whether the user is supersetting); the client runs the countdown
/// and renders the cues. UI-agnostic: the same directive drives the TUI, a future
/// Android client, and (server-side) Telegram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimerSignal {
    /// Start (or restart) a rest countdown of `duration_secs`, after which the user
    /// does another set of `exercise`.
    Arm { duration_secs: u32, exercise: String },
    /// Cancel any in-flight rest countdown (session ended, entry closed, …).
    Cancel,
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
        roundtrip_response(ServerResponse::Reply { view: View::message("Logged 3 sets of bench press."), timer: None });
        roundtrip_response(ServerResponse::Reply {
            view: View::message("Hard set logged."),
            timer: Some(TimerSignal::Arm { duration_secs: 300, exercise: "Bench Press".into() }),
        });
        roundtrip_response(ServerResponse::Error { message: "unknown user".into() });
    }

    #[test]
    fn view_variants_roundtrip() {
        for view in [
            View::Message { text: "Nice work!".into(), notes: vec!["3 sets logged".into()], failures: vec!["oops".into()] },
            View::notice("Conversation cleared."),
            View::Status(view::StatusView {
                user_name: "Alice".into(),
                session: Some(view::SessionView {
                    started_at: "2026-06-16 10:00:00".into(),
                    completed: vec![view::ExerciseLog {
                        name: "Bench Press".into(),
                        sets: vec![view::SetLine { measurement: view::Measurement::WeightReps, count: Some(8), value: 80.0 }],
                    }],
                    in_progress: vec![],
                }),
                health: vec![view::HealthNote { kind: "injury".into(), body_part: "shoulder".into(), description: "sore".into() }],
            }),
            View::Catalog(view::CatalogView {
                groups: vec![view::CatalogGroup {
                    muscle_group: "Chest".into(),
                    exercises: vec![view::CatalogEntry { name: "Bench Press".into(), aliases: "bench".into(), kind: "weight_reps".into() }],
                }],
            }),
            View::History(view::HistoryView {
                sessions: vec![view::SessionSummaryView { started_at: "2026-06-16 10:00:00".into(), status: "done".into(), entries: 3, minutes: Some(45) }],
            }),
        ] {
            roundtrip_response(ServerResponse::Reply { view, timer: None });
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        // Postcard enum discriminants are varints; 0xFF.. is not a valid 3-variant tag.
        assert!(decode_request(&[0xFF, 0xFF, 0xFF]).is_err());
    }
}

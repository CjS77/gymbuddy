//! Wire protocol shared by the GymBuddy server and all clients (TUI, Android, …).
//!
//! One envelope per direction ([`ClientRequest`] / [`ServerResponse`]), serialized
//! with [`postcard`] for compactness and carried inside confide's
//! `Message::Custom { kind: `[`KIND`]`, data }`. The crate is pure data — no I/O —
//! so every client and the server share exactly one definition of the wire format.

use serde::{Deserialize, Serialize};

pub mod view;
pub use view::{
    CatalogEntry, CatalogGroup, CatalogView, ExerciseLog, HealthNote, HistoryView, Measurement, Render, RosterExerciseView,
    SessionRosterView, SessionSummaryView, SessionView, SetLine, StatusView, TrainingModeView, View,
};

/// Discriminator placed in confide's `Message::Custom { kind, .. }` so the peer
/// knows the payload is a GymBuddy v1 envelope.
pub const KIND: &str = "gymbuddy/v1";

/// A request sent from a client to the server.
///
/// Chat is sequential, so no request id is carried yet; later structured queries
/// (progress, goals, …) can add one when correlation becomes necessary.
///
/// **Append new variants; never reorder or extend an existing one.** postcard is
/// non-self-describing and positional: enum tags are varint discriminants assigned
/// by declaration order, and struct fields carry no names on the wire. Appending a
/// variant is safe in both directions — an old decoder fails cleanly on an unknown
/// tag — whereas adding a field to an existing variant silently misparses.
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
    /// "What can I run?" — asks for the slash commands available to this user,
    /// answered with [`ServerResponse::Commands`].
    ///
    /// A request rather than a connect-time snapshot because the answer is
    /// per-user and can change mid-session (a user can be granted beta access
    /// while connected), so a client that cares may re-issue it.
    ListCommands,
}

/// A response sent from the server to a client.
///
/// Not `Eq`: [`View`] carries floating-point set values.
///
/// Append-only, for the postcard reasons spelled out on [`ClientRequest`].
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
    /// The slash commands this user may run, in help order — the answer to
    /// [`ClientRequest::ListCommands`].
    ///
    /// The set is per-user and lists only what the user can actually run, so a
    /// client may offer every entry without disclosing a command that isn't
    /// theirs.
    Commands { commands: Vec<CommandInfo> },
}

/// One slash command as advertised to a client.
///
/// Enough to complete the word at a prompt and to say what it does; the server
/// keeps the handler and the help text, and clients never hardcode the set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// The command word, leading slash included, e.g. `/status`.
    pub name: String,
    /// One-line description of what the command does.
    pub description: String,
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
        roundtrip_request(ClientRequest::ListCommands);
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
        roundtrip_response(ServerResponse::Commands {
            commands: vec![CommandInfo { name: "/status".into(), description: "Current session and today's stats".into() }],
        });
        roundtrip_response(ServerResponse::Commands { commands: vec![] });
    }

    /// The variants added for [C2.1] must sit at the end of their enums: postcard
    /// tags are declaration-order varints, so inserting one ahead of an existing
    /// variant silently reinterprets every old message that carries it.
    #[test]
    fn appended_variants_did_not_shift_existing_discriminants() {
        assert_eq!(encode_request(&ClientRequest::Hello).unwrap()[0], 0);
        assert_eq!(encode_request(&ClientRequest::ListCommands).unwrap()[0], 3);
        assert_eq!(encode_response(&ServerResponse::NeedsRegistration).unwrap()[0], 1);
        assert_eq!(encode_response(&ServerResponse::Commands { commands: vec![] }).unwrap()[0], 4);
    }

    /// A server that predates [C2.1] has no tag 3, so its decoder rejects the
    /// request outright instead of misreading it as a `Chat`.
    #[test]
    fn an_old_decoder_rejects_a_new_variant_rather_than_misparsing_it() {
        let bytes = encode_request(&ClientRequest::ListCommands).unwrap();
        #[derive(Debug, Serialize, Deserialize)]
        enum OldClientRequest {
            Hello,
            Register { name: String, timezone: String },
            Chat { text: String },
        }
        assert!(postcard::from_bytes::<OldClientRequest>(&bytes).is_err());
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
            View::ProgrammeSessionRoster {
                roster: view::SessionRosterView {
                    title: "Upper".into(),
                    rationale: Some("push it".into()),
                    exercises: vec![],
                    notes: vec![],
                },
                mode: view::TrainingModeView::AdHoc { programme_title: "12-week hypertrophy".into() },
            },
            View::ProgrammeSessionRoster {
                roster: view::SessionRosterView { title: "Upper".into(), rationale: None, exercises: vec![], notes: vec![] },
                mode: view::TrainingModeView::Programme { programme_title: "12-week".into(), week: 1, day: 2, focus: "upper".into() },
            },
        ] {
            roundtrip_response(ServerResponse::Reply { view, timer: None });
        }
    }

    /// Every `View` discriminant, pinned to its number.
    ///
    /// postcard tags are declaration-order varints, so variant *order* is the wire
    /// format and variant *names* are not: the [R1.5] renames (`Workout` →
    /// `SessionRoster`, `ProgramWorkout` → `ProgrammeSessionRoster`) had to leave
    /// these bytes untouched. Pinning the whole enum rather than only the two renamed
    /// tags is what makes an accidental reorder or insertion fail here instead of in
    /// the field, where an older peer would silently misparse the shifted variants.
    ///
    /// Adding a variant means appending it and adding a line below — never inserting.
    #[test]
    fn view_discriminants_are_pinned_to_their_wire_tags() {
        let roster = || view::SessionRosterView { title: "Push".into(), rationale: None, exercises: vec![], notes: vec![] };
        let tag = |view: &View| postcard::to_allocvec(view).unwrap()[0];

        assert_eq!(tag(&View::message("hi")), 0, "Message");
        assert_eq!(tag(&View::Status(view::StatusView { user_name: "Al".into(), session: None, health: vec![] })), 1, "Status");
        assert_eq!(tag(&View::Catalog(view::CatalogView { groups: vec![] })), 2, "Catalog");
        assert_eq!(tag(&View::History(view::HistoryView { sessions: vec![] })), 3, "History");
        assert_eq!(tag(&View::notice("ok")), 4, "Notice");
        assert_eq!(tag(&View::Timers { enabled: true }), 5, "Timers");
        assert_eq!(tag(&View::SessionRoster(roster())), 6, "SessionRoster keeps the pre-[R1.5] `Workout` discriminant");
        assert_eq!(
            tag(&View::ProgrammeSessionRoster {
                roster: roster(),
                mode: view::TrainingModeView::AdHoc { programme_title: "12-week".into() },
            }),
            7,
            "ProgrammeSessionRoster keeps the pre-[R1.5] `ProgramWorkout` discriminant",
        );
    }

    /// `TrainingModeView` rides inside tag 7, so its own variant order is wire format
    /// too — the [R1.5] `Program` → `Programme` rename must not have moved it.
    #[test]
    fn training_mode_discriminants_are_pinned_to_their_wire_tags() {
        let tag = |mode: &view::TrainingModeView| postcard::to_allocvec(mode).unwrap()[0];
        assert_eq!(tag(&view::TrainingModeView::AdHoc { programme_title: "p".into() }), 0, "AdHoc");
        let slot = view::TrainingModeView::Programme { programme_title: "p".into(), week: 1, day: 1, focus: "upper".into() };
        assert_eq!(tag(&slot), 1, "Programme keeps the pre-[R1.5] `Program` discriminant");
    }

    #[test]
    fn decode_rejects_garbage() {
        // Postcard enum discriminants are varints; 0xFF.. is not a valid 3-variant tag.
        assert!(decode_request(&[0xFF, 0xFF, 0xFF]).is_err());
    }
}

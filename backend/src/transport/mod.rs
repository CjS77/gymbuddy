//! Network transports that bridge external clients to the assistant.
//!
//! Telegram lives in [`crate::telegram`] and `main.rs`; this module hosts the
//! confide (encrypted p2p) transport used by the TUI and the future Android
//! client. Both transports funnel into the same
//! [`AssistantHandler::handle_message_for_user`](crate::assistant::AssistantHandler::handle_message_for_user).
pub mod confide;

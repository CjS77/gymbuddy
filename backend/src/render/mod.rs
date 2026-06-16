//! Server-side renderers for the assistant [`View`](gymbuddy_proto::View).
//!
//! The Telegram renderer runs in-process because the Telegram "client" lives in
//! this backend. Confide clients (TUI, Android) receive the `View` over the wire
//! and render it themselves — see `gymbuddy-tui`'s `render` module.

pub mod plain;
pub mod telegram;

pub use plain::to_plain;
pub use telegram::Telegram;

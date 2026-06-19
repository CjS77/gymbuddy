//! UI-independent application state and the rules that update it.
//!
//! Kept free of ratatui/crossterm types beyond the key event it interprets, so the
//! state transitions are easy to follow and test.

use gymbuddy_proto::{ClientRequest, ServerResponse, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Who produced a transcript line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    You,
    Buddy,
    System,
}

/// The payload of a transcript entry: either flat text (the user's own input,
/// system notices) or a domain [`View`] from the assistant, rendered at draw time.
pub enum EntryBody {
    Text(String),
    View(View),
}

/// One transcript entry.
pub struct Entry {
    pub speaker: Speaker,
    pub body: EntryBody,
}

/// What the client should do in response to an input event.
pub enum Action {
    /// Nothing to do.
    None,
    /// Quit the application.
    Quit,
    /// Send a request to the server.
    Send(ClientRequest),
}

/// The current input expectation.
pub enum Mode {
    /// Awaiting the server's answer to `Hello`.
    Connecting,
    /// Collecting the registration name.
    AskName,
    /// Collecting the registration timezone (name already captured).
    AskTimezone { name: String },
    /// Normal chat.
    Chat,
}

/// Whole application state.
pub struct App {
    pub my_pubkey: String,
    pub mode: Mode,
    pub transcript: Vec<Entry>,
    pub input: String,
    pub connected: bool,
    pub should_quit: bool,
    /// Lines scrolled up from the bottom (0 = following the latest).
    pub scroll_back: u16,
    /// Mirror of the server's per-session rest-timer toggle, shown in the sidebar.
    /// Optimistic default until the first `/timers` reply confirms it.
    pub timers_enabled: bool,
    default_name: Option<String>,
    default_timezone: Option<String>,
}

impl App {
    pub fn new(my_pubkey: String, default_name: Option<String>, default_timezone: Option<String>) -> Self {
        Self {
            my_pubkey,
            mode: Mode::Connecting,
            transcript: Vec::new(),
            input: String::new(),
            connected: true,
            should_quit: false,
            scroll_back: 0,
            timers_enabled: true,
            default_name,
            default_timezone,
        }
    }

    /// Append a rest-timer cue line to the transcript (driven by the local timer).
    pub fn push_cue_line(&mut self, text: impl Into<String>) {
        self.push(Speaker::System, text);
    }

    fn push(&mut self, speaker: Speaker, text: impl Into<String>) {
        self.transcript.push(Entry { speaker, body: EntryBody::Text(text.into()) });
        self.scroll_back = 0; // jump back to the latest on any new line
    }

    /// Append an assistant view to the transcript (rendered when drawn).
    fn push_view(&mut self, view: View) {
        self.transcript.push(Entry { speaker: Speaker::Buddy, body: EntryBody::View(view) });
        self.scroll_back = 0;
    }

    /// Apply a key press and report what the client should do.
    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return Action::Quit;
        }
        // Ctrl+T flips the sidebar timer switch — it travels as the same `/timers`
        // command Telegram uses, so the server stays the single source of truth.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('t')) {
            return Action::Send(ClientRequest::Chat { text: "/timers".to_string() });
        }
        match key.code {
            KeyCode::Esc => Action::Quit,
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input.pop();
                Action::None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                Action::None
            }
            KeyCode::PageUp => {
                self.scroll_back = self.scroll_back.saturating_add(5);
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll_back = self.scroll_back.saturating_sub(5);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Handle the Enter key according to the current [`Mode`].
    fn submit(&mut self) -> Action {
        let text = std::mem::take(&mut self.input).trim().to_string();
        match std::mem::replace(&mut self.mode, Mode::Connecting) {
            Mode::Chat => {
                self.mode = Mode::Chat;
                if text.is_empty() {
                    return Action::None;
                }
                self.push(Speaker::You, text.clone());
                Action::Send(ClientRequest::Chat { text })
            }
            Mode::AskName => {
                if text.is_empty() {
                    self.mode = Mode::AskName;
                    return Action::None;
                }
                // Skip the timezone prompt when a default was supplied.
                if let Some(tz) = self.default_timezone.clone() {
                    self.mode = Mode::Connecting;
                    self.push(Speaker::System, format!("Registering as {text} ({tz})…"));
                    return Action::Send(ClientRequest::Register { name: text, timezone: tz });
                }
                self.push(Speaker::System, "Enter your timezone (e.g. Europe/London):");
                self.mode = Mode::AskTimezone { name: text };
                Action::None
            }
            Mode::AskTimezone { name } => {
                let timezone = if text.is_empty() { "UTC".to_string() } else { text };
                self.mode = Mode::Connecting;
                self.push(Speaker::System, format!("Registering as {name} ({timezone})…"));
                Action::Send(ClientRequest::Register { name, timezone })
            }
            Mode::Connecting => {
                self.mode = Mode::Connecting;
                Action::None
            }
        }
    }

    /// Handle a decoded response. Returns a follow-up request to send, if any.
    pub fn on_response(&mut self, resp: ServerResponse) -> Option<ClientRequest> {
        match resp {
            ServerResponse::Welcome { name } => {
                self.mode = Mode::Chat;
                self.push(Speaker::System, format!("Connected as {name}. Type a message; /help for commands."));
                None
            }
            ServerResponse::NeedsRegistration => self.begin_registration(),
            // The optional `timer` directive is handled by the event loop (which
            // owns the local countdown task); here we only render the view and keep
            // the sidebar switch in sync with a `/timers` reply.
            ServerResponse::Reply { view, .. } => {
                if let View::Timers { enabled } = &view {
                    self.timers_enabled = *enabled;
                }
                self.push_view(view);
                None
            }
            ServerResponse::Error { message } => {
                self.push(Speaker::System, format!("Error: {message}"));
                None
            }
        }
    }

    /// Start registration — auto-register if both name and timezone were preset,
    /// otherwise prompt for the name.
    fn begin_registration(&mut self) -> Option<ClientRequest> {
        match (self.default_name.clone(), self.default_timezone.clone()) {
            (Some(name), Some(timezone)) => {
                self.push(Speaker::System, format!("Registering as {name} ({timezone})…"));
                Some(ClientRequest::Register { name, timezone })
            }
            _ => {
                self.input = self.default_name.clone().unwrap_or_default();
                self.push(Speaker::System, "This key isn't registered yet. Enter your name:");
                self.mode = Mode::AskName;
                None
            }
        }
    }

    pub fn mark_disconnected(&mut self, reason: impl Into<String>) {
        self.connected = false;
        self.push(Speaker::System, reason);
    }

    /// Prompt label for the input box, depending on mode.
    pub fn input_label(&self) -> &'static str {
        match self.mode {
            Mode::Connecting => "Connecting…",
            Mode::AskName => "Your name",
            Mode::AskTimezone { .. } => "Your timezone",
            Mode::Chat => "Message",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn registration_flow_two_steps() {
        let mut app = App::new("pk".into(), None, None);
        assert!(app.begin_registration().is_none());
        assert!(matches!(app.mode, Mode::AskName));

        type_text(&mut app, "Alice");
        assert!(matches!(app.on_key(key(KeyCode::Enter)), Action::None));
        assert!(matches!(app.mode, Mode::AskTimezone { .. }));

        type_text(&mut app, "Europe/London");
        match app.on_key(key(KeyCode::Enter)) {
            Action::Send(ClientRequest::Register { name, timezone }) => {
                assert_eq!(name, "Alice");
                assert_eq!(timezone, "Europe/London");
            }
            _ => panic!("expected Register"),
        }
    }

    #[test]
    fn auto_register_with_defaults() {
        let mut app = App::new("pk".into(), Some("Bob".into()), Some("UTC".into()));
        match app.begin_registration() {
            Some(ClientRequest::Register { name, timezone }) => {
                assert_eq!(name, "Bob");
                assert_eq!(timezone, "UTC");
            }
            _ => panic!("expected auto Register"),
        }
    }

    #[test]
    fn chat_enter_sends_and_echoes() {
        let mut app = App::new("pk".into(), None, None);
        app.on_response(ServerResponse::Welcome { name: "Alice".into() });
        assert!(matches!(app.mode, Mode::Chat));

        type_text(&mut app, "hi there");
        match app.on_key(key(KeyCode::Enter)) {
            Action::Send(ClientRequest::Chat { text }) => assert_eq!(text, "hi there"),
            _ => panic!("expected Chat send"),
        }
        // The user's line is echoed into the transcript.
        assert!(
            app.transcript
                .iter()
                .any(|e| e.speaker == Speaker::You && matches!(&e.body, EntryBody::Text(t) if t == "hi there"))
        );
    }

    #[test]
    fn empty_chat_enter_is_noop() {
        let mut app = App::new("pk".into(), None, None);
        app.mode = Mode::Chat;
        assert!(matches!(app.on_key(key(KeyCode::Enter)), Action::None));
    }
}

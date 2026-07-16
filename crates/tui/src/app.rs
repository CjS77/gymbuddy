//! UI-independent application state and the rules that update it.
//!
//! Kept free of ratatui/crossterm types beyond the key event it interprets, so the
//! state transitions are easy to follow and test.

use gymbuddy_proto::{ClientRequest, ServerResponse, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::{Input, InputRequest};

use crate::history::History;

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

/// Translate a key press into a `tui-input` edit request, binding the readline
/// set. `None` for keys the prompt doesn't act on.
///
/// Modifiers are matched explicitly because crossterm still reports the bare
/// letter in `KeyCode::Char` when Ctrl or Alt is held — so the `InsertChar` arm
/// must come last and only fire unmodified, or Ctrl-B would type a `b`.
fn to_input_request(key: &KeyEvent) -> Option<InputRequest> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match (key.code, ctrl, alt) {
        (KeyCode::Char('b'), true, _) => Some(InputRequest::GoToPrevChar),
        (KeyCode::Char('f'), true, _) => Some(InputRequest::GoToNextChar),
        (KeyCode::Char('a'), true, _) => Some(InputRequest::GoToStart),
        (KeyCode::Char('e'), true, _) => Some(InputRequest::GoToEnd),
        (KeyCode::Char('w'), true, _) => Some(InputRequest::DeletePrevWord),
        (KeyCode::Char('k'), true, _) => Some(InputRequest::DeleteTillEnd),
        (KeyCode::Char('u'), true, _) => Some(InputRequest::DeleteLine),
        (KeyCode::Char('y'), true, _) => Some(InputRequest::Yank),
        (KeyCode::Char('b'), _, true) => Some(InputRequest::GoToPrevWord),
        (KeyCode::Char('f'), _, true) => Some(InputRequest::GoToNextWord),
        (KeyCode::Backspace, _, true) => Some(InputRequest::DeletePrevWord),
        (KeyCode::Left, _, _) => Some(InputRequest::GoToPrevChar),
        (KeyCode::Right, _, _) => Some(InputRequest::GoToNextChar),
        (KeyCode::Home, _, _) => Some(InputRequest::GoToStart),
        (KeyCode::End, _, _) => Some(InputRequest::GoToEnd),
        (KeyCode::Backspace, _, _) => Some(InputRequest::DeletePrevChar),
        (KeyCode::Delete, _, _) => Some(InputRequest::DeleteNextChar),
        (KeyCode::Char(c), false, false) => Some(InputRequest::InsertChar(c)),
        _ => None,
    }
}

/// Whole application state.
pub struct App {
    pub my_pubkey: String,
    pub mode: Mode,
    pub transcript: Vec<Entry>,
    pub input: Input,
    /// Recall ring for previously submitted chat lines. Loaded from and saved to
    /// disk by `main`, so the state transitions here stay free of I/O.
    pub history: History,
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
            input: Input::default(),
            history: History::default(),
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
            KeyCode::Up => {
                self.recall_prev();
                Action::None
            }
            KeyCode::Down => {
                self.recall_next();
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
            _ => {
                if let Some(req) = to_input_request(&key) {
                    self.input.handle(req);
                }
                Action::None
            }
        }
    }

    /// Replace the prompt with the previous history entry, stashing the line in
    /// progress on the first step back.
    fn recall_prev(&mut self) {
        if let Some(line) = self.history.prev(self.input.value().to_string()) {
            self.input = Input::new(line);
        }
    }

    /// Replace the prompt with the next history entry, restoring the stashed
    /// draft once we step past the newest.
    fn recall_next(&mut self) {
        if let Some(line) = self.history.next() {
            self.input = Input::new(line);
        }
    }

    /// Handle the Enter key according to the current [`Mode`].
    fn submit(&mut self) -> Action {
        let text = self.input.value_and_reset().trim().to_string();
        self.history.reset_cursor();
        match std::mem::replace(&mut self.mode, Mode::Connecting) {
            Mode::Chat => {
                self.mode = Mode::Chat;
                if text.is_empty() {
                    return Action::None;
                }
                // Only chat lines are recalled. The registration arms below
                // deliberately never record, so a name or timezone can't reach the
                // ring or the history file.
                self.history.record(&text);
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
            // Advertised by the server ([C2.1]) but not yet asked for or used —
            // [T1.3] is what turns this into tab completion.
            ServerResponse::Commands { .. } => None,
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
                self.input = Input::new(self.default_name.clone().unwrap_or_default());
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

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    /// An app already past registration, with a prompt ready for input.
    fn chatting() -> App {
        let mut app = App::new("pk".into(), None, None);
        app.on_response(ServerResponse::Welcome { name: "Alice".into() });
        app
    }

    fn submit(app: &mut App, text: &str) {
        type_text(app, text);
        app.on_key(key(KeyCode::Enter));
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

    /// Crossterm reports the bare letter in `KeyCode::Char` even with Ctrl or Alt
    /// held, so a careless `Char(c) => insert` arm types the chord's letter.
    #[test]
    fn chords_do_not_insert_their_letter() {
        let mut app = chatting();
        [ctrl('a'), ctrl('e'), ctrl('b'), ctrl('f'), ctrl('k'), ctrl('u'), ctrl('w'), ctrl('y')]
            .into_iter()
            .for_each(|k| {
                app.on_key(k);
            });
        app.on_key(alt(KeyCode::Char('b')));
        app.on_key(alt(KeyCode::Char('f')));
        assert_eq!(app.input.value(), "");
    }

    #[test]
    fn ctrl_a_and_e_jump_to_line_ends() {
        let mut app = chatting();
        type_text(&mut app, "bench");
        app.on_key(ctrl('a'));
        type_text(&mut app, ">");
        assert_eq!(app.input.value(), ">bench");
        app.on_key(ctrl('e'));
        type_text(&mut app, "<");
        assert_eq!(app.input.value(), ">bench<");
    }

    #[test]
    fn ctrl_w_kills_the_previous_word_and_ctrl_y_yanks_it_back() {
        let mut app = chatting();
        type_text(&mut app, "squats 100kg");
        app.on_key(ctrl('w'));
        assert_eq!(app.input.value(), "squats ");
        app.on_key(ctrl('y'));
        assert_eq!(app.input.value(), "squats 100kg");
    }

    #[test]
    fn ctrl_u_kills_the_line() {
        let mut app = chatting();
        type_text(&mut app, "bench 80kg");
        app.on_key(ctrl('u'));
        assert_eq!(app.input.value(), "");
    }

    #[test]
    fn alt_b_moves_by_word() {
        let mut app = chatting();
        type_text(&mut app, "squats 100kg");
        app.on_key(alt(KeyCode::Char('b')));
        type_text(&mut app, "@");
        assert_eq!(app.input.value(), "squats @100kg");
    }

    #[test]
    fn arrows_recall_history_and_restore_the_draft() {
        let mut app = chatting();
        submit(&mut app, "squats 100kg");
        submit(&mut app, "bench 80kg");

        type_text(&mut app, "half typed");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input.value(), "bench 80kg");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input.value(), "squats 100kg");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.input.value(), "bench 80kg");
        // Past the newest entry the line in progress comes back intact.
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.input.value(), "half typed");
    }

    #[test]
    fn editing_a_recalled_line_leaves_the_stored_entry_alone() {
        let mut app = chatting();
        submit(&mut app, "squats 100kg");
        app.on_key(key(KeyCode::Up));
        app.on_key(ctrl('w'));
        type_text(&mut app, "120kg");
        assert_eq!(app.input.value(), "squats 120kg");
        assert_eq!(app.history.entries(), ["squats 100kg"]);
    }

    /// The name and timezone must never reach the recall ring — or, through it,
    /// the history file on disk.
    #[test]
    fn registration_answers_never_enter_history() {
        let mut app = App::new("pk".into(), None, None);
        app.begin_registration();
        submit(&mut app, "Alice");
        submit(&mut app, "Europe/London");
        assert!(app.history.entries().is_empty());

        // …while ordinary chat afterwards is recorded.
        app.on_response(ServerResponse::Welcome { name: "Alice".into() });
        submit(&mut app, "squats 100kg");
        assert_eq!(app.history.entries(), ["squats 100kg"]);
    }

    #[test]
    fn ctrl_t_still_toggles_timers_and_ctrl_c_still_quits() {
        let mut app = chatting();
        match app.on_key(ctrl('t')) {
            Action::Send(ClientRequest::Chat { text }) => assert_eq!(text, "/timers"),
            _ => panic!("expected /timers"),
        }
        assert!(matches!(app.on_key(ctrl('c')), Action::Quit));
    }
}

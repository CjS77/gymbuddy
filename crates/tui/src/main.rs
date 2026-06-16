//! GymBuddy terminal client: a ratatui text-chat UI over [`gymbuddy_client`].
//!
//! The event loop is a `tokio::select!` over crossterm key events and the inbound
//! response stream from the client core, so the UI stays responsive while the
//! server thinks. All domain logic lives server-side; this binary only renders.

mod app;
mod config;
mod ui;

use std::io::Stdout;

use anyhow::Context as _;
use clap::Parser;
use futures::StreamExt as _;
use gymbuddy_client::GymClient;
use gymbuddy_proto::ClientRequest;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, EventStream, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

use crate::app::{Action, App};
use crate::config::{Cli, ResolvedConfig};

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ResolvedConfig { connect, name, timezone } = Cli::parse().resolve()?;
    let (client, mut responses) = GymClient::connect(connect).await.context("connecting to server")?;

    install_panic_hook();
    let mut terminal = init_terminal()?;
    let result = run(&mut terminal, &client, &mut responses, name, timezone).await;
    restore_terminal(&mut terminal)?;
    result
}

async fn run(
    terminal: &mut Tui,
    client: &GymClient,
    responses: &mut gymbuddy_client::Responses,
    name: Option<String>,
    timezone: Option<String>,
) -> anyhow::Result<()> {
    let mut app = App::new(client.my_pubkey_hex().to_string(), name, timezone);
    let mut events = EventStream::new();

    // Identify ourselves; the answer drives registration vs. welcome.
    client.send(&ClientRequest::Hello).await?;

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => match app.on_key(key) {
                    Action::None => {}
                    Action::Quit => app.should_quit = true,
                    Action::Send(req) => client.send(&req).await?,
                },
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(anyhow::Error::from(e).context("reading terminal events")),
                None => app.should_quit = true,
            },
            maybe_resp = responses.next() => match maybe_resp {
                Some(Ok(resp)) => {
                    if let Some(req) = app.on_response(resp) {
                        client.send(&req).await?;
                    }
                }
                Some(Err(e)) => app.mark_disconnected(format!("connection error: {e:#}")),
                None => app.mark_disconnected("disconnected from server"),
            },
        }
    }
    Ok(())
}

fn init_terminal() -> anyhow::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout)).context("creating terminal")
}

fn restore_terminal(terminal: &mut Tui) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal on panic so the user isn't left in raw/alternate mode.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

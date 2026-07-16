# TUI

Terminal client product area. Covers everything the user touches directly in the
terminal: the command prompt, rendering, and session flow.

Parent: [product_areas.md](product_areas.md)

## Epic 1: UI

Direct interaction surface of the terminal client — how input is entered and how
output is presented.

### [T1.1] Adopt tui-input for prompt line editing

#### Description
Replace the ad-hoc input buffer in `App` (`crates/tui/src/app.rs`) with
[`tui-input`](https://crates.io/crates/tui-input), giving the prompt real
readline-style editing. Today `on_key` handles only `Char` and `Backspace`
(push/pop on a `String`) and has no cursor position at all.

- Swap `App::input: String` for `tui_input::Input`; translate crossterm
  `KeyEvent`s into `InputRequest`s in `on_key`.
- Bind the readline set: left/right and Ctrl-B/F by character, Alt-B/F by word,
  Ctrl-A/E to line ends, Ctrl-W and Alt-Backspace to kill the previous word,
  Ctrl-K to kill to end, Ctrl-U to kill the line, Ctrl-Y to yank.
- Fix the cursor in `crates/tui/src/ui.rs:65`, which places it with
  `input.chars().count()` — wrong once the cursor can move, and wrong for wide
  glyphs. Use the `Input`'s visual cursor offset.
- Depend with `default-features = false`: we supply the key mapping, which
  `on_key` already is, so the crate's crossterm event helper is dead weight. That
  also keeps widget types out of `app.rs`, which its module doc asks for.
- Correction — `default-features = false` does **not** dodge the version conflict.
  The clash isn't the optional ratatui/crossterm deps, it's `unicode-width`, which
  is unconditional: ratatui 0.29 hard-pins `=0.2.0` while tui-input 0.15 needs
  `^0.2.2`, so cargo cannot unify them at any feature setting. The last tui-input
  that resolves against ratatui 0.29 is 0.14, which predates `InputRequest::Yank`
  and so can't do the Ctrl-Y this task asks for. Resolved by upgrading the
  workspace to ratatui 0.30 / crossterm 0.29, which unblocks tui-input 0.15.3 —
  the upgrade turned out to be a no-op for our source.
- Keep `on_key`'s existing contract: it returns an `Action`, and Enter still
  routes through `submit()` per `Mode`.

#### Metadata
- Priority: P1
- Progress: In progress

### [T1.2] Command history with cross-session persistence

#### Description
Recall of previously entered lines at the prompt, surviving restarts (per user)

- Up/down walk history; down past the newest returns to the line in progress.
  Stash the draft on first recall so arrowing up never destroys unsent text.
- Record only lines submitted in `Mode::Chat`. Registration answers
  (`Mode::AskName`, `Mode::AskTimezone`) must never enter history — the user's
  name and timezone don't belong in a recall ring or on disk.
- Drop consecutive duplicates and blank lines; cap the ring at a fixed size.
- Persist to a local file under `dirs::data_dir()/gymbuddy/`, keyed by the
  client's own pubkey — which is what separates histories by user, and is known
  at startup since it comes from the local keystore, not the server. Treat a
  missing, unreadable, or corrupt file as empty history — never fail startup over
  it.
- Editing a recalled line changes only the working copy, not the stored entry.
- Resolved — history is a local file, not the DB. The server does already hold
  every TUI line (`conversation_history`, tagged `platform = 'confide'`, which is
  also where transcribed Telegram voice prompts land), but that table is the wrong
  substrate: it is hard-`DELETE` pruned after every turn
  (`backend/src/assistant/handler.rs:237`), `/clear` excludes it, and it mixes in
  assistant rows. A recall ring wants the opposite retention policy, and a local
  file needs no proto change and no round-trip at prompt startup.
- Resolved — `data_dir()`, not the config dir. Recall history is state, not
  config, and `data_dir()/gymbuddy/` is where the identity keyed to it already
  lives (`default_identity_path()`, `crates/client/src/lib.rs:24-33`). The repo
  has no `config_dir()` call anywhere.

#### Metadata
- Priority: P1
- Progress: In progress
- Depends on: [T1.1]

### [T1.3] Tab completion for slash commands

#### Description
Tab completes `/` commands at the prompt.

- Complete to the longest common prefix; on a repeated Tab with the candidate
  set still ambiguous, cycle the candidates. Only fires when the line starts
  with `/` and the cursor is in the first word.
- **Resolved — the candidate list comes from the server, via [C2.1].** The
  commands are defined server-side in `handle_command`
  (`backend/src/assistant/handler.rs:305`) and the TUI has no knowledge of them.
  The set is per-user: `/feedback` is beta-gated, so a hardcoded client list would
  both drift from the server and offer commands the user can't run. The server
  must therefore advertise its set — that is a proto and backend change, filed as
  [C2.1]. This task consumes the result; it stays blocked until C2.1 lands.
- **Not on `Welcome`.** The original suggestion here was to carry the set on
  `ServerResponse::Welcome`. That does not work: the proto is postcard-encoded
  (`crates/proto/src/lib.rs:4`), which is non-self-describing and positional, so
  appending a field to an existing variant is a hard break in both directions
  rather than a graceful degrade. [C2.1] specifies a separate request/response
  pair instead.
- Completing argument values (exercise names for `/nextworkout`) is out of scope
  here; file separately if wanted.

#### Metadata
- Priority: P2
- Progress: Blocked — needs [C2.1] (server-advertised command set) before it can start
- Depends on: [T1.1], [C2.1]

## Epic 2: Planning and progress surfaces

Surface the Core planning, programme and feedback work in the terminal. Each task
renders a `View` variant Core defines; none of them own domain logic.

`render` is an exhaustive `match` over `View` (`crates/tui/src/render.rs:21-27`),
so every new Core variant breaks this crate's build until it is handled — rollout
is compiler-enforced rather than remembered. The same is true of the Telegram
renderer, which is why each Core view task has a sibling in both areas.

### [T2.1] Render programme status

#### Description
Render the programme status view from [C4.6]: position in the programme, what is
done, what is next, adherence, and goal trajectory.

- Follow `render_workout` (`crates/tui/src/render.rs`) as the model — the closest
  existing analogue, and it already handles the prescription shape.
- Reuse the shared formatters rather than growing a local copy. `SetLine::compact()`
  and `PlannedExerciseView::target_line()` live in `gymbuddy-proto` as the single
  source of truth for set and prescription text (commit `cfc82d0`) precisely so a
  client inherits the formatting.
- Unlike Telegram, the TUI has a persistent frame and a sidebar. A programme is
  long-lived context, not a one-off reply — worth deciding whether it belongs in
  the scrollback at all, or in the sidebar beside the rest-timer switch.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C4.6]

### [T2.2] Render charts

#### Description
Render the chartable series from [C6.2].

- ratatui ships chart widgets (`Chart`, `Sparkline`, `BarChart`), so this needs no
  new dependency — but `app.rs` deliberately keeps widget types out of itself (see
  [T1.1]), so the widget choice belongs in `render.rs`.
- Must respect the direction-aware "better" that [C6.2] carries: for a weightloss
  goal, down is progress, and a chart that renders it as decline is actively wrong.
- Terminal width and colour support vary. A chart that only reads correctly in
  truecolor is a chart that misleads in a 16-colour terminal; prefer shape over
  hue for anything load-bearing.
- Diverges from [G1.2], which can render a real image. Both consume the same Core
  series — the divergence is the point of shipping data rather than pixels.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C6.2]

### [T2.3] Render the post-session report

#### Description
Render the post-session report from [C6.5] — what was done, how it compared to the
prescription, PRs, effort, and goal movement.

- Arrives unprompted at session end rather than in reply to a submitted line,
  which is new for this client: every view it renders today answers something the
  user just sent. Confirm it lands correctly for a session ended by timeout
  (`close_stale_session`) rather than by the user.
- Composes with [T2.2] if the report carries series; degrades to text if not.
- Honest assessment is Core's job ([C6.5]); do not soften it in the rendering.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C6.5]

# TUI

Terminal client product area. Covers everything the user touches directly in the
terminal: the command prompt, rendering, and session flow.

Parent: [product_areas.md](product_areas.md)

## Epic 1: UI

Direct interaction surface of the terminal client — how input is entered and how
output is presented.

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

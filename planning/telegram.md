# Telegram

Telegram client product area. Everything the user touches in Telegram: how views
are rendered to HTML, how long replies are chunked, voice in and out, and the
command menu.

Parent: [product_areas.md](product_areas.md)

The renderer lives in the backend tree (`backend/src/render/telegram.rs`) for
build reasons, but it is client work and belongs here, not to `C`. It implements
the same UI-agnostic `View` contract the TUI does — `Renderer::render(&View)`
returns HTML plus a parse mode, and `backend/src/telegram/chunk.rs` splits it to
Telegram's 4096-character limit.

`render` is an exhaustive `match` over `View` (`render/telegram.rs:17-23`), so
every new Core variant breaks this crate's build until it is handled. That is the
intended pressure and the reason each Core view task has a task here: rollout is
compiler-enforced rather than remembered.

## Epic 1: Planning and progress surfaces

Surface the Core planning, programme and feedback work in Telegram. Each task
here renders a `View` variant Core defines; none of them own domain logic.

### [G1.1] Render programme status

#### Description
Render the programme status view from [C4.6]: where the user is in the programme,
what is done, what is next, adherence, and goal trajectory.

- Follow `render_workout` (`render/telegram.rs:125`) as the model — it is the
  closest existing analogue and already handles the prescription shape.
- Reuse the shared formatters rather than growing a local copy. `SetLine::compact()`
  and `PlannedExerciseView::target_line()` were deliberately pushed into
  `gymbuddy-proto` as the single source of truth for set and prescription text
  (commit `cfc82d0`), precisely so a new client inherits the formatting.
- A programme is much longer than a session and will collide with the 4096-char
  limit that `split_for_telegram` handles. Chunking mid-programme is worse than
  summarising: prefer a compact status with detail on request.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C4.6]

### [G1.2] Render charts

#### Description
Render the chartable series from [C6.2]. Telegram is the one client that can show
a real plot, which makes this the most divergent renderer task in the area.

- **Open decision: image or text.** Telegram can display a rendered PNG via
  `sendPhoto`; a Unicode/block-character sparkline needs no new dependency and
  survives in any client. Images are better here and worse everywhere else, and
  the answer decides whether this area takes on a plotting dependency.
- `TelegramClient` has no `send_photo` today (`telegram/client.rs`) — only
  `send_message`, `send_voice` and `send_chat_action`. `send_voice` (`:107`)
  already does a multipart upload, so the client work is small and precedented.
- Whatever is chosen must respect the direction-aware "better" that [C6.2] carries:
  for a weightloss goal, down is progress, and a chart that colours it as decline
  is actively wrong.
- The `View` carries data, not pixels. If plotting lands here it stays a rendering
  concern and does not leak back into Core.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C6.2]

### [G1.3] Render the post-session report

#### Description
Render the post-session report from [C6.5] — what was done, how it compared to the
prescription, PRs, effort, and goal movement.

- Arrives unprompted at session end rather than in reply to a message, which is
  new: every other Telegram view today answers something the user just sent.
  Confirm it routes correctly for a session ended by timeout (`close_stale_session`)
  rather than by the user.
- Composes with [G1.2] if the report carries series; degrades to text if not.
- Honest assessment is Core's job ([C6.5]); this task must not soften it in the
  rendering.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C6.5]

### [G1.4] Native command menu from the advertised set

#### Description
Feed the per-user command set from [C2.1] into Telegram's native command menu, so
commands autocomplete in the client the way `/` completion will in the TUI
([T1.3]).

- Telegram exposes this via `setMyCommands`; `TelegramClient` has no such method
  today, so this adds one alongside `send_message`.
- **The per-user gating is the catch.** [C2.1] computes the set per user
  specifically to preserve `/feedback`'s non-disclosure for non-beta users, but
  `setMyCommands` is scoped per bot or per chat, not per message. Use a chat scope
  so a non-beta user's menu never lists `/feedback` — a bot-wide default would leak
  exactly what [C2.1] is careful not to.
- Refresh on change, not only at startup: `set_beta_tester` (`db/users.rs:81`) can
  flip mid-session, which is the same reason [C2.1] prefers a re-issuable request
  over a connect-time snapshot.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C2.1]

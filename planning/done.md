# Done

Archive of completed tasks, cut from their area files once finished. This is the
historical record — append only, newest first. Nothing here is active work.

Parent: [product_areas.md](product_areas.md)

A task keeps its tag when archived, so references to it still resolve. Entries
also record their origin and the date they landed, since the task no longer lives
in the file that gave it context:

```
### [<tag>] <Task title>
#### Description
What the task delivered.

#### Metadata
- Area / Epic: <area> / <epic>
- Completed: YYYY-MM-DD
```

Tags here are still live: they are never reused, and this file counts when
working out the next free tag in an epic.

---

### [T1.2] Command history with cross-session persistence

#### Description
Prompt recall of previously submitted lines, surviving restarts, per user. Up and
down walk the ring; the line in progress is stashed on the first step back and
handed back on stepping past the newest entry, so arrowing up never destroys
unsent text. Editing a recalled line changes only the working copy. Blank lines
and consecutive duplicates are dropped, and the ring caps at 500 entries.

Only lines submitted in `Mode::Chat` are recorded — the registration answers
(`Mode::AskName`, `Mode::AskTimezone`) never reach the ring or the disk, so a
user's name and timezone stay out of recall.

Persisted to `data_dir()/gymbuddy/history/<pubkey>.txt`, keyed by the client's own
pubkey from the local keystore, which is what separates histories by user and is
known at startup without a round-trip. A missing, unreadable, or corrupt file
reads as empty history and never holds up startup. The ring itself
(`crates/tui/src/history.rs`) is pure; `main` loads it at startup and writes it
back on the way out.

Chose a local file over the server's `conversation_history` table: that table is
hard-`DELETE` pruned after every turn, is excluded from `/clear`, and mixes in
assistant rows — the opposite retention policy to a recall ring.

#### Metadata
- Area / Epic: TUI / Epic 1: UI
- Completed: 2026-07-16

### [T1.1] Adopt tui-input for prompt line editing

#### Description
Replaced the ad-hoc `String` input buffer in `App` with `tui_input::Input`, giving
the prompt readline-style editing where `on_key` had previously handled only
`Char` and `Backspace` with no cursor position at all. `on_key` now translates
crossterm key events into `InputRequest`s, binding left/right and Ctrl-B/F by
character, Alt-B/F by word, Ctrl-A/E to the line ends, Ctrl-W and Alt-Backspace to
kill the previous word, Ctrl-K to kill to end, Ctrl-U to kill the line, and Ctrl-Y
to yank. Its contract is unchanged: it still returns an `Action`, and Enter still
routes through `submit()` per `Mode`.

The cursor in `crates/tui/src/ui.rs` had been placed with `input.chars().count()`;
it now uses the `Input`'s visual cursor offset and scroll, which fixes placement
both for a moved cursor and for wide glyphs.

Depended on upgrading the workspace to ratatui 0.30 / crossterm 0.29. The blocker
was not the optional ratatui/crossterm features but `unicode-width`, which is an
unconditional dependency: ratatui 0.29 hard-pinned `=0.2.0` while tui-input 0.15
needs `^0.2.2`, so cargo could not unify them at any feature setting, and the last
tui-input that resolved against ratatui 0.29 predates `InputRequest::Yank`. The
upgrade turned out to be a no-op for our source.

#### Metadata
- Area / Epic: TUI / Epic 1: UI
- Completed: 2026-07-16

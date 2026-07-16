# Product Areas

Top-level index of Gymbuddy product areas. Each area has its own file containing
epics, and each epic is subdivided into tasks.

| Tag | Area | File | Scope |
|-----|------|------|-------|
| `C` | Core | [core.md](core.md) | Domain model, backend services, storage, planner, timers |
| `T` | TUI | [tui.md](tui.md) | Terminal client: input handling, rendering, session flow |
| `G` | Telegram | [telegram.md](telegram.md) | Telegram client: rendering, chunking, voice, command menu |

Core is UI-agnostic and owns the `View` contract (`crates/proto/src/view.rs`); the
client areas render it. A feature is therefore normally two tasks — one in `C`,
one per client that surfaces it — and the client task depends on the Core one.
Telegram's renderer physically lives in the backend tree
(`backend/src/render/telegram.rs`) but is client work and belongs to `G`, not `C`.

Completed work lives in [done.md](done.md), not in the area files.

## Workflow

Area files hold only live work. A task moves through three steps:

1. **Start it.** Set its **Progress** to `In progress` in the area file. Epics are
   worked in their own git worktree — see [Epic worktrees](#epic-worktrees).
2. **Finish it.** Do the work; get it merged and verified. Set **Progress** to
   `Done`.
3. **Archive it.** Cut the task out of the area file entirely and paste it into
   [done.md](done.md), adding its area, epic, and completion date. A task should
   exist in exactly one file — never both.

Steps 2 and 3 don't have to happen together. Marking a task `Done` and leaving it
in place is fine: it parks the task until the next housekeeping pass, and it lets
work be closed out by hand without also doing the archiving. The area files stay
short enough to read in full, and the history stays intact in `done.md` where it
can be skimmed when needed but doesn't crowd out current work.

### Epic worktrees

The worktree is the unit of isolation; the task is the unit of commit.

1. **Add a git worktree for the epic and switch into it.** One per epic, not one
   per task — the epic's tasks share a branch, and their dependencies
   (`Depends on:`) mean they are meant to land in order on top of each other.
2. **Work the epic's tasks there, committing each task as it is done.** A task is
   a commit. Don't batch an epic into one commit; don't split a task across
   several unless it genuinely stands alone.
3. **Stop when the epic is done, and hand it to the user for testing.** The user
   merges the branch back into `main` and removes the worktree. Whoever wrote the
   epic does neither — no self-merge, no cleanup of a worktree still awaiting
   review.

The handover in step 3 is the point of the whole arrangement: the epic sits intact
and testable in its own directory until someone who didn't write it has run it.

### Housekeeping

Periodically — on request, or whenever the planning files are being worked on —
sweep the area files for tasks marked `Done` and archive them per step 3. This is
also the recovery path if archiving is ever interrupted or missed: the `Done`
marker is what makes a stranded task findable, so a sweep can always reconstruct
what should have moved.

## Conventions

Every area file follows the same shape:

```
# <Area>
## Epic <n>: <name>
### [<tag>] <Task title>
#### Description
What the task delivers — prose, or a bullet per deliverable.

#### Metadata
- Priority: P0 | P1 | P2 | P3
- Progress: Not started | In progress | Blocked | Done
- Depends on: [<tag>], [<tag>]     (optional; omit when nothing blocks it)
```

The epic number in its heading is what the task tags are built from, so every
epic needs one.

### Task IDs

Every task carries a short tag before its title, used to refer to it from
anywhere else in the planning docs. The tag is `{area}{epic}.{task}`:

```
[T1.1]   TUI area, epic 1, task 1
[C12.2]  Core area, epic 12, task 2
```

Area letters are assigned in the table above; epics are numbered within their
area, tasks within their epic. Cross-reference a task by its tag alone —
`blocked on [T1.1]` — since the tag names exactly one task across all files.

Tags are permanent:

- **Never reuse a tag,** even after its task is archived or dropped. A stale
  cross-reference should fail to resolve, not silently point at unrelated work.
- **Never renumber** to close gaps. Gaps are free; broken references are not.
- **An archived task keeps its tag** in [done.md](done.md), so a reference to
  finished work still resolves.
- Next tag in an epic = highest ever used in that epic + 1. Check `done.md`
  alongside the area file, since the highest may already have been archived.

### Priority levels

- **P0** — Blocking. Nothing else in the area ships until this lands.
- **P1** — Important. Scheduled for the current cycle.
- **P2** — Wanted. Picked up once P0/P1 work is clear.
- **P3** — Someday. Recorded so it isn't lost, not scheduled.

### Progress indicators

- **Not started** — no work begun.
- **In progress** — actively being worked.
- **Blocked** — waiting on something; note the blocker in the description.
- **Done** — merged and verified.

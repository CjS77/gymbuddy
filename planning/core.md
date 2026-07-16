# Core

Core product area. Domain model, backend services, storage, planner, and timers —
everything that is independent of a particular client.

Parent: [product_areas.md](product_areas.md)

## Epics

## Epic 1: Workout flow

Gymbuddy aims to be a virtual Personal Trainer that is AS GOOD AS any human PT short of being able to be their to spot you!
Workout storage, retrival, execution and planning is currently very finicky and broken.
GymBuddy must be able to work in adhoc mode (this workout is not part of a long-term health schedule) and plan mode (a long-term series 
of workouts that build on one-another towards some set of fitness goals).

A complete workout plan feature set looks like this:
1. Workout plan setting
- Based on medical science (so we let the LLM do some research), research-backed exercise<>goal pairing (e.g. high-weight<>low reps for 
  strength etc, weightloss<>high reps + cardio)
- Works towards goals
- Takes real feedback (actual gym session results, perceived effort levels, injury reports etc) and works that into defining the next 
  workout session
- tries to push the user out of their comfort zone every session whilst still being safe.
2. Progress and feedback
- We can see all sessions related to the workout plan, how far we are from our goals, and projected likelihood for success.
- We can plot charts (or get chartable data) related to each exercise and goal
- Intelligent commentary on where we might be falling short and suggested remediation.
3. Planning the Next workout session
- Load all goals, history, conversations and recommendations (i.e. the entire health-medical file on record) and pass it to the LLM in 
  order to plan the next session (which may be right now)
- Choice of specific exercises for this next session should be chosen roughly according to this decreasing priority list:
  - Exercise directly contributes towards the target goals
  - Exercises that we've done before
  - Focus on muscles that have had the longest rest period
  - Fits into the workout philosophy defined by the user
  - Takes recent injuries, or medical issues into account
  - Temporary overrides (e.g. user says, I don't feel like bench press today, let's try flys)
4. Post session feedback. Give an honest assessment of how the session went. Give data, charts etc (same as #2)
5. Storage - is the current DB schema suitable for providing such a comprehensive and flexible exercise regime?

---

**Decomposition.** The spec above is the North Star for the whole area, not one
epic's worth of work. The [planner arc](done.md) (commit `cfc82d0`) delivered a
real foundation for it — the `/philosophy` interview, `/nextworkout` designing
from philosophy + last three sessions + goals + injuries, guided set-by-set
execution, and the `workout_plans` / `workout_philosophy` tables. Four gaps
separate that from the spec, and they set the epic structure:

1. **A "plan" is one session.** `workout_plans` is a single designed workout.
   Nothing models a long-term programme that sessions build toward, so nothing can
   answer "how far through am I" or "am I on track". → Epic 4.
2. **Goals can only be one exercise and one number.** `exercise_goals` is
   `exercise_type_id + target_value`. Weightloss — named in §1 — cannot be
   expressed, and no body metric exists to measure it against. → Epic 3.
3. **The analytics already exist and are unreachable.** `progress.rs` and
   `dashboard.rs` implement time series, PRs, streaks, weekly volume and goal
   progress. Only `goal_progress_report` has a production caller; the rest is live
   code reachable only from tests. → Epic 6.
4. **No exercise science is encoded anywhere.** The designer relies entirely on
   the LLM's internal knowledge. Nothing versioned or testable states which
   prescription serves which goal. → Epic 5.

| Spec item | Epic |
|-----------|------|
| §1 science, goal pairing, comfort zone | Epic 5, Epic 3 |
| §2 progress, charts, projections, commentary | Epic 6 |
| §3 planning the next session, priority order | **Epic 1**, Epic 4 |
| §4 post-session feedback | Epic 6 |
| §5 storage — is the schema suitable? | Epic 3 — answer: **no**, see [C3.1]–[C3.3] |
| ad-hoc *and* plan mode | Epic 4, [C1.4] |

Decisions taken up front, so the tasks don't re-litigate them:

- **Programmes are a skeleton, not a script.** Persist goals, dates, split, block
  structure and a progression policy; keep designing each session on demand
  against that skeleton. Preserves the LLM's adaptivity while giving adherence
  and projection something concrete to measure against.
- **Goals get a `kind`; body metrics get a table.** That is what makes weightloss,
  bodyweight and habit goals expressible at all.
- **"Research" means a curated, in-repo knowledge base**, not live web search.
  Beyond being deterministic and reviewable, it is the only option that doesn't
  start with framework work: `corre_core::app::LlmRequest` has no `tools` field,
  so real tool-calling doesn't exist to build on today (see Epic 5).

Client rollout is out of scope here: Core defines UI-agnostic `View` variants;
[tui.md](tui.md) and [telegram.md](telegram.md) render them.

**Scope of Epic 1**, with the rest decomposed into Epics 3–6: the runtime loop —
what the session designer sees, how it chooses, and how execution is tracked
against what was prescribed. Spec §3.

### [C1.1] Rank exercise selection by the spec's priority order

#### Description
`build_designer_prompt` (`backend/src/assistant/prompts.rs:411`) tells the LLM to
"reason like a real trainer" but never states the decreasing priority order §3
lays down. Make that order explicit and testable.

- Encode the list, in order, in the designer prompt: goal contribution, then
  exercises done before, then longest-rested muscles, then philosophy fit, then
  injuries/medical, then temporary overrides.
- It is a *decreasing priority*, not a filter — say so. A lower item never vetoes
  a higher one; it breaks ties. Injuries are the exception and stay a hard
  constraint (they already are, via `format_health_entries`).
- Require the `rationale` to name which priorities drove the picks, so the
  ordering is visible in the output and assertable in tests.
- Consumes the recovery signal from [C1.2] and the history window from [C1.3];
  without those the "longest rest period" and "done before" rungs have no data to
  stand on.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C1.2], [C1.3]

### [C1.2] Per-muscle recovery signal for the designer

#### Description
"Focus on muscles that have had the longest rest period" is third on §3's list
and there is nothing to compute it from. The designer sees a flat prose dump of
recent sessions (`format_designer_history`, `handler.rs:533`) and would have to
infer recovery itself.

- Add a query returning, per muscle group, time since last trained and the volume
  it last took. Roll variations up to their muscle group via the existing ancestry
  helpers (`descendant_ids_inclusive`, `backend/src/db/exercise_types.rs`).
- Render it as an explicit prompt section (`Chest — last trained 5d ago, 12 sets`)
  rather than leaving it implicit in the history block.
- Muscle groups untrained in the window must appear, not be omitted — "never
  trained" is the strongest possible rest signal and the most useful thing to
  surface.
- Belongs in `progress.rs` beside the other aggregates, and must have a production
  caller from day one — unlike its neighbours, see [C6.1].

#### Metadata
- Priority: P1
- Progress: Not started

### [C1.3] Goal-aware history window

#### Description
`cmd_next_workout` (`handler.rs:446`) hardcodes `recent_sessions_with_sets(user.id, 3)`.
Three sessions is roughly one week — too short to see progression on a lift
trained weekly, and it silently caps how well the designer can honour "exercises
we've done before".

- Replace the constant with a window driven by what the design needs: enough
  history to cover each goal-relevant exercise's recent trend, bounded by a token
  budget rather than a session count.
- Prefer depth on goal-relevant exercises over breadth. A lift central to a goal
  wants its last several sessions; an incidental accessory does not.
- §3 asks for "the entire health-medical file on record". That is the intent, but
  it cannot be literal at scale — and the designer already runs at a raised 2048
  token cap (`handler.rs:466`) precisely because it overruns the default.
  Summarise rather than drop: recent sessions in full, older ones as per-exercise
  trend lines.
- Make the window and budget config-driven, not literals in the handler.

#### Metadata
- Priority: P1
- Progress: Not started

### [C1.4] First-class training mode: ad-hoc vs programme

#### Description
The spec opens by demanding both modes; the code has only one. Every session
today is implicitly ad-hoc — `/nextworkout` designs in a vacuum and the resulting
plan belongs to nothing.

- Introduce the mode explicitly, so the designer, the renderers and the user can
  all tell which is in play.
- Ad-hoc stays fully supported and must never require a programme: a user with no
  programme gets exactly today's behaviour.
- With an active programme, `/nextworkout` designs against the current slot
  ([C4.3]); without one, it designs as it does now.
- An ad-hoc session inside an active programme is legitimate ("I'm travelling,
  dumbbells only") and must not corrupt adherence. Record it as ad-hoc rather than
  as a missed or completed slot.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C4.1]

### [C1.5] Track prescribed vs actual

#### Description
A plan prescribes; sets get logged; nothing compares them. Without this the loop
§1 asks for — "takes real feedback ... and works that into defining the next
workout session" — cannot close, and the post-session assessment ([C6.5]) has
nothing to assess against.

- Given a session bound to a plan, produce the deltas: prescribed vs performed
  sets, reps, weight; exercises skipped; exercises added that were never planned.
- Lives in Core and is consumed by [C6.5], the designer's next run, and
  progression ([C5.3]). Not a rendering concern.
- Deviation is signal, not failure. Someone who beats every prescription is
  under-prescribed; someone who consistently misses is over-prescribed. Both feed
  progression — say so in the model, so consumers don't treat delta as error.
- `workout_plans.session_id` already binds the two sides (`bind_plan_to_session`);
  this is the query and the model over that binding.

#### Metadata
- Priority: P1
- Progress: Not started

### [C1.6] Temporary, single-session overrides

#### Description
§3's last rung: "user says, I don't feel like bench press today, let's try flys".
Durable preferences have a home — `AppendPhilosophyNote` folds them into the
philosophy — but a one-off has nowhere to live, and the distinction is never
drawn.

- Distinguish *today only* from *from now on*. Writing "I don't feel like bench
  today" into the philosophy is a real bug: it silently bans bench press forever.
- `/nextworkout <text>` already forwards guidance to the designer as the user turn
  (`handler.rs:461`). Extend the same notion to overrides raised mid-session,
  which currently reach the chat prompt with no route into the design.
- An override applies to the plan in flight and expires with it. It must not
  survive into the next design, and must not touch the philosophy.
- Ambiguity resolves by asking, not guessing. "I hate bench press" is plausibly
  either, and the cost of guessing wrong is asymmetric.

#### Metadata
- Priority: P2
- Progress: Not started

### [C1.7] Make structured LLM output reliable

#### Description
Every prompt in `prompts.rs` ends by demanding "ONLY a JSON object", and
`call_llm_with` (`handler.rs:897`) duly sets `LlmRequest.json_mode = true` — but
`corre-llm`'s `OpenAiCompatProvider` never reads that field and hardcodes
`response_format: None`. **No JSON mode is being sent to the provider.** The
contract is enforced by prompt text alone, against `qwen3-next-80b`
(`gymbuddy.toml:35`).

- The failure is already visible and silent: when the designer doesn't emit
  `propose_workout`, `cmd_next_workout` falls back to showing prose and saves no
  plan (`handler.rs:475`). The user sees a workout that was never persisted.
- Either honour `json_mode` in the provider (an upstream `corre` change) or stop
  setting a flag that does nothing and make the parser's tolerance the deliberate
  strategy. Silently passing an ignored flag is the worst of both.
- Getting worse, not stable: [C4.2]'s programme design is a strictly larger and
  more nested structured output than a single session, and [C6.2] adds more.
  `parse_assistant_response` already tolerates markdown fences — that tolerance is
  load-bearing and undocumented as such.
- Whatever the outcome, a design that fails to parse must fail loudly rather than
  degrade to prose that looks like success.

#### Metadata
- Priority: P1
- Progress: Not started

### [C1.8] Split the assistant handler

#### Description
`backend/src/assistant/handler.rs` is 3157 lines and is the common blast radius of
nearly every task in Epics 1, 4 and 6 — command dispatch, LLM calls, action
execution, session continuity, the philosophy interview and the designer all live
in it. Hygiene, not a feature: filed because the work queued behind it will
otherwise land on top of itself.

- Separate the concerns already distinct in everything but file layout: command
  dispatch, action execution, the designer, the interview, LLM plumbing.
- Pure refactor. The existing tests are substantial and in-file; they must pass
  untouched, moving with the code they cover.
- Best done *before* Epics 4 and 6 add commands and actions. If it slips it should
  be dropped rather than attempted late — a big refactor underneath in-flight
  feature work is worse than a large file.

#### Metadata
- Priority: P2
- Progress: Not started

## Epic 2: Client-server contract

The wire protocol between the backend and its clients, and what the server tells
clients about itself.

### [C2.1] Advertise the per-user slash-command set to clients

#### Description
Clients have no machine-readable knowledge of the server's slash commands. The
commands live in `handle_command` (`backend/src/assistant/handler.rs:305`) and the
TUI just forwards everything as `ClientRequest::Chat`. [T1.3] needs the set to
complete `/` commands at the prompt, and is blocked until this lands.

- Carry it on a **new request/response pair**, not a field on an existing variant.
  The proto is postcard-encoded (`crates/proto/src/lib.rs:4`) — non-self-describing
  and positional, with no field names on the wire — so appending a field to
  `ServerResponse::Welcome` breaks both directions rather than degrading (old
  client → `TrailingBytes`; new client vs old server → `DeserializeUnexpectedEnd`).
  Appending a new *variant* is discriminant-safe, and an old decoder fails cleanly
  on an unknown tag instead of misparsing. The TUI is the only client today
  (`android/` is empty), but it ships as its own binary and can lag the server, so
  the asymmetry is real.
- Compute the set **per user**, and preserve `/feedback`'s non-disclosure.
  `cmd_feedback` returns `Ok(None)` for non-beta users
  (`backend/src/assistant/handler.rs:1442`) specifically so the command is
  indistinguishable from one that doesn't exist — see the doc comment above it.
  Never advertise a command the user can't run.
- Prefer a request the client can re-issue over a connect-time snapshot:
  `set_beta_tester` (`backend/src/db/users.rs:81`) can flip mid-session.
- Fold the duplicate lists in while here. The set is currently written out three
  times — the dispatcher (`handler.rs:307`), `cmd_help` (`:570`), and `cmd_start`
  (`:550`, which omits `/cancel`). Derive them from one table rather than adding a
  fourth.
- Resolved — the table is `backend/src/assistant/commands.rs`, and the dispatcher
  matches a parsed `Command` enum rather than a raw string. That makes the link
  compiler-enforced: a row added to the table with no handler fails to build. The
  `/start` list is now derived too, so it gained the `/cancel` it had drifted into
  omitting, and both lists inherit `/help`'s wording.

#### Metadata
- Priority: P2
- Progress: In progress

## Epic 3: Domain model & storage

Spec §5 asks: is the current DB schema suitable for a comprehensive and flexible
exercise regime? **No** — in the specific ways below. This epic is the foundation
the rest of the North Star work stands on, which is why [C3.1] is the only P0 in
the area.

### [C3.1] Generalise the goal model

#### Description
`exercise_goals` binds every goal to one exercise and one number
(`exercise_type_id INTEGER NOT NULL`, `target_value REAL NOT NULL`;
`backend/migrations/01-initial_schema/up.sql:80`). Spec §1 names weightloss as a
first-class goal in its very first bullet — that shape cannot express it. Nor can
it express "train 4x a week", "run 5k under 25 minutes", or any goal not
denominated in a single exercise's single number.

- Generalise to a `goals` table: add `kind` (strength / endurance / bodyweight /
  body_composition / habit), make `exercise_type_id` nullable, add `metric` for
  non-exercise goals, plus `direction` (increase / decrease), `target_date` and
  `priority`.
- `direction` is not cosmetic. Every query today assumes bigger is better
  (`best_value_for_exercise_type`, `progress.rs`); a weightloss goal inverts that,
  and progress and projection ([C6.4]) invert silently with it if this is skipped.
- `priority` is what lets the designer rank "contributes toward target goals"
  ([C1.1]) when goals compete — a strength goal and a weightloss goal pull in
  opposite directions and something has to break the tie.
- Migrate existing `exercise_goals` rows: they become `kind = strength`,
  `direction = increase`. Keep `GoalStatus` derived at read time as it is now
  (`progress.rs:126`) — it is a function of the data, not a state to maintain.
- Update the `SetGoal` action (`actions.rs:96`) and its prompt so the LLM can set
  the new kinds. The model is useless if only the old shape is reachable.
- Blocks the goal-driven half of the North Star: [C4.1], [C5.1], [C6.1].

#### Metadata
- Priority: P0
- Progress: Not started

### [C3.2] Body metrics

#### Description
Nothing in the schema records bodyweight, body fat or measurements. A weightloss
goal ([C3.1]) with nothing to measure against is a goal that can never report
progress or be marked achieved.

- Add a `body_metrics` table: user, metric, value, measured_at. Long-shaped rather
  than a wide column per metric — the metric set will grow (waist, resting HR) and
  each addition should be a row, not a migration.
- Add an LLM action so "I weighed 82.5 this morning" logs without a command, the
  way health entries already do via `LogHealth`.
- Feeds the weight/composition series in [C6.2] and goal progress in [C6.4].
- This is health data about a real person's body, and the most sensitive data in
  the schema. Worth a deliberate call on retention and on what appears unprompted
  in summaries, rather than defaulting to "render it because we have it".

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C3.1]

### [C3.3] Session-level outcome and effort

#### Description
`sets.perceived_difficulty` captures effort per set (easy / medium / hard /
failure). `sessions` carries only `started_at`, `ended_at`, `notes` — so "how did
that session go" has no home. §1 wants perceived effort worked into the next
design and §4 wants an honest post-session assessment; both need a session-level
verdict, not just a pile of per-set buckets.

- Add session outcome fields: overall effort, how it felt, whether it was cut
  short and why. Distinct from `notes`, which is free-form and read by no query.
- Populate at session end — the natural moment, and where the bound plan is
  already marked completed (`handler.rs:1006`).
- Feeds [C6.5] and the next design's feedback loop.
- **Open question, deliberately not answered here:** whether
  `perceived_difficulty`'s four buckets should become numeric RPE/RIR. RIR is the
  standard instrument for progressive overload ([C5.3]) and "failure" is really
  RIR 0 — but the four buckets are cheap for a user to give and map onto RPE bands
  well enough. Revisit when [C5.3] makes the requirement concrete; do not
  pre-emptively migrate.

#### Metadata
- Priority: P1
- Progress: Not started

### [C3.4] Reconcile the competing plan mechanisms

#### Description
There are already **two** notions of "what I'm training", and both are injected
into the system prompt on every turn:

1. `active_plan` — the pre-planner mechanism. A `plan:<name>` sentinel smuggled
   into `sessions.notes`, resolved against `schedules` / `schedule_exercises`, and
   rendered by `format_active_plan` (`prompts.rs:556`).
2. `active_workout_plan` — the planner's own `workout_plans`, rendered by
   `format_active_workout_plan` (`prompts.rs:584`).

Programmes ([C4.1]) would make three. Reconcile before adding, not after.

- Decide the split and enforce it: `schedules` becomes purely *when to remind*,
  programmes own *what to train*. `schedule_exercises` — a fixed, non-adaptive
  list — is then redundant with programme slots and should go.
- The sentinel in `sessions.notes` is the part to kill outright. Encoding a
  foreign key as a string prefix inside a free-text column is exactly the kind of
  thing [C3.3] is otherwise trying to give a real home.
- A programme with a weekly frequency implies reminders. Driving them off the
  programme rather than a parallel cron table is the point of doing this.
- Left alone, a user can have a schedule and a programme prescribing different
  things on the same day, with both in the prompt and nothing reconciling them.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C4.1]

### [C3.5] Scope conversation pruning by platform

#### Description
A latent bug, found while mapping storage. Conversation history is **fetched** per
`(user, platform)` — `get_recent_messages_for_platform` (`conversation.rs:49`) —
but **pruned** per user across all platforms: `prune_old_messages(user.id, limit * 2)`
(`handler.rs:237`) has no platform predicate (`conversation.rs:68`).

- The consequence: a chatty Telegram session evicts the TUI's history, and vice
  versa. The user loses context on a client they weren't even using, with no
  signal that it happened.
- Prune per `(user, platform)` so retention matches the read path.
- Small and self-contained, but it silently degrades the LLM's context — the whole
  substrate Epics 1 and 4 build on — and gets more visible as more clients land
  ([G1], `android/` is empty but planned).
- Worth checking whether the `* 2` factor is still the right retention rule once
  it means what it appears to mean.

#### Metadata
- Priority: P2
- Progress: Not started

## Epic 4: Long-term programmes

Plan mode: the long-term series of workouts that build on one another toward a
set of goals. The single largest gap between the code and the North Star — today
a "plan" is one session and nothing joins them up.

The shape is a **skeleton, not a script**: persist goals, dates, split, block
structure and a progression policy; keep designing each session on demand against
it. A fully pre-generated programme would be more predictable and entirely
chartable, but every piece of real feedback would invalidate it — and reacting to
feedback is what makes this a PT rather than a spreadsheet.

### [C4.1] Programme schema and DAO

#### Description
The skeleton, and the join from a designed session back to the programme slot it
filled.

- `programs`: user, title, goals served, start / target-end date, days per week,
  split (free text — the LLM reads it), progression policy, status
  (draft / active / completed / abandoned).
- `program_blocks`: the mesocycle structure (weeks 1–4 hypertrophy, 5–6 deload).
  What makes sessions "build on one another" rather than repeat.
- `program_slots`: the week/day grid — week index, day index, focus, status
  (pending / filled / missed / skipped).
- `workout_plans` gains a nullable `program_slot_id`. **Nullable is the ad-hoc
  case** ([C1.4]) and stays first-class, not an afterthought.
- Follow the existing DAO shape: hand-written SQL in a `backend/src/db/` module
  with in-memory tests, alongside `planner.rs`.
- One active programme per user. `create_plan` already precedents this by
  auto-abandoning prior proposals (`planner.rs:135`); mirror it rather than
  inventing a second convention.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C3.1]

### [C4.2] Design a programme

#### Description
The flow that produces a skeleton: goals plus philosophy plus history in, a
structured multi-week programme out.

- New command. It overlaps genuinely with `/philosophy` — both interview, both
  distil — and should reuse `interview_state`, which is already keyed by
  `(user, platform)` and already survives pruning and `/clear`. Its `mode` CHECK
  currently allows only `'philosophy'` and needs widening.
- The philosophy is an input, not a duplicate: it already holds frequency,
  preferred programs and equipment. Do not re-interview for what is on file —
  confirm it and move on. Ask only for what a programme needs and a philosophy
  lacks: the target date, and which goals this programme serves.
- Requires goals ([C3.1]). A programme with no goal to build toward is just a
  calendar — refuse and offer to set one, the way `/nextworkout` already refuses
  without a philosophy (`handler.rs:452`).
- Grounded in [C5.1] — block structure and progression are exactly where the
  science belongs.
- Emits a `propose_program` action mirroring `propose_workout` (`actions.rs:138`),
  so structured output stays on the existing rails. Note this is a much larger
  output than a session design, which already needs a raised 2048-token cap — the
  budget and [C1.7]'s reliability question both bite harder here.
- The philosophy interview's `turns >= 4` hard wrap-up (`prompts.rs:352`) exists
  because a small model otherwise interviews forever. Expect to need the same
  guard.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C4.1], [C5.1]

### [C4.3] Programme-aware session design

#### Description
The join that makes a programme mean anything: `/nextworkout` designs against the
current slot instead of in a vacuum.

- Extend `build_designer_prompt` (`prompts.rs:411`) with programme context: where
  the user is, the current block's intent, the slot's focus, adherence so far.
- Concretely, the designer should know it is "week 3 of 8, block = hypertrophy,
  slot focus = push, 7 of 9 sessions completed". That is what turns a standalone
  design into one that builds on the last.
- Block intent must beat generic progression. A deload week means backing off, and
  a designer that doesn't know it's in one will cheerfully add weight.
- Fills the slot on completion; leaves it pending on abandonment.
- Degrades to today's behaviour with no active programme — the ad-hoc path
  ([C1.4]) is not a special case to bolt on later.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C4.1], [C1.4]

### [C4.4] Adherence and drift

#### Description
Real users miss sessions. A programme that cannot absorb that is a programme users
abandon — and adherence is most of what §2's "projected likelihood for success"
([C6.4]) is actually made of.

- Track slots filled, missed and skipped against the plan.
- Detect drift: consistently missing a slot is information about the user, not an
  error to report at them. A PT would move leg day, not scold.
- Decide the reschedule policy — shift, drop, or compress — and make it explicit
  rather than emergent.
- Ad-hoc sessions inside an active programme count toward training but not toward
  slots ([C1.4]).
- Feeds [C6.4] and [C4.6].

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C4.1], [C4.3]

### [C4.5] Programme lifecycle

#### Description
Getting into a programme is [C4.2]; this is everything after — pause, resume,
complete, abandon, regenerate the remainder.

- Pause/resume matters more than it sounds: injury and travel are the common
  cases, and without it the only exit is abandonment, which loses the history.
- Completion is worth marking — goals met or missed, what changed. It is the
  natural trigger for the next programme's design.
- Regenerating the remainder is the escape hatch when reality has diverged too far
  for [C4.4] to absorb. It must preserve completed history rather than rewrite it.
- An injury that contraindicates a programme's core lift ([C5.4]) should surface as
  a prompt to regenerate, not silently produce substituted sessions until the
  programme quietly means nothing.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C4.1]

### [C4.6] Programme status view

#### Description
"We can see all sessions related to the workout plan, how far we are from our
goals" — §2, the programme half. The goal half is [C6.3].

- A new `View` variant carrying programme progress: where you are, what's done,
  what's next, adherence, goal trajectory.
- Append the variant; never extend an existing one — see the postcard reasoning
  spelled out in [C2.1].
- Core builds the view model; [T2.1] and [G1.1] render it.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C4.1], [C4.4]

## Epic 5: Training science knowledge base

"Based on medical science (so we let the LLM do some research), research-backed
exercise<>goal pairing" — §1. Today nothing in the repo encodes any of it: the
designer prompt says draw on "your own expertise" (`prompts.rs:423`) and hopes.

**Curated, in-repo, versioned — not live web search.** Three reasons, in order of
weight: the science is stable and small enough to get right once (rep ranges and
rest intervals have not moved in years); curation keeps unvetted claims out of a
health context and makes prescriptions reproducible and testable; and live
retrieval isn't actually available to build on — `corre_core::app::LlmRequest`
carries no `tools` field and `AssistantHandler` has never used `McpCaller`, so
tool-calling would be framework work before it was product work. A research tool
that enriches the KB *offline*, with the KB still the runtime source of truth, is
future work worth filing separately.

### [C5.1] Curated knowledge base

#### Description
The content and the loader.

- Goal kind → prescription: rep range, intensity, volume, rest interval,
  frequency. Strength is low reps at high load with long rests; hypertrophy is
  moderate both with short rests; endurance is high reps at low load; weightloss
  is compounds plus cardio against a deficit.
- Keyed to the goal `kind` from [C3.1] — that is what makes the pairing mechanical
  rather than a prompt-engineering exercise.
- Reviewable as prose by a human, loadable as data by the designer. It gets
  reviewed *once*, by someone who can judge it; that is the whole argument for
  curation over retrieval.
- Versioned in-repo and unit-testable. A change to the science is a diff, with the
  reasoning in the commit message.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C3.1]

### [C5.2] Ground the designer in the knowledge base

#### Description
Inject [C5.1] into `build_designer_prompt` so prescriptions follow from the user's
goal kinds rather than from whatever the model recalls that day.

- The KB narrows the prescription; the LLM still picks the exercises. It is a
  constraint on rep ranges and intensity, not a lookup table of workouts — the
  adaptivity is the product.
- Competing goals resolve by `priority` ([C3.1]). A strength goal and a weightloss
  goal genuinely conflict, and the KB should state how they combine rather than
  leave the model to average them into something that serves neither.
- Testable: given a goal kind, assert the prescription lands in the KB's band.
  This is the first point where "is the science right" becomes a question with an
  answer.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C5.1]

### [C5.3] Progressive overload policy

#### Description
"Tries to push the user out of their comfort zone every session whilst still being
safe" — §1, and the hardest sentence in it. Today the designer is told "if the
last sets of an exercise were easy, progress the load; if they were hard or to
failure, hold or back off" (`prompts.rs:434`). That is the entire progression
model.

- Encode a real policy: how much to add, how often, per exercise class, driven by
  logged performance and perceived effort.
- Both halves are requirements. Never progressing is useless; always progressing
  injures people. This is where "safely" is actually enforced, and the deload and
  back-off rules need to be as explicit as the increments.
- Consumes prescribed-vs-actual ([C1.5]) and per-set effort: someone who beat the
  prescription at "easy" gets a different answer from someone who missed it at
  "failure".
- Respects block intent ([C4.3]) — a deload week overrides progression.
- The likely forcing function for the RPE/RIR question parked in [C3.3]. Answer it
  here, where the requirement is concrete, rather than guessing earlier.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C5.1], [C1.5]

### [C5.4] Injury contraindication rails

#### Description
Injuries reach the designer as prose — `format_health_entries` dumps them and the
prompt asks the model to "substitute away from movements that load an injured
area", with one worked example (`prompts.rs:436`). That is guidance, not a rail,
and this is the one place in the system where a wrong answer hurts someone.

- Encode contraindications as data: body part → movement patterns to avoid →
  substitutions that keep the session's intent.
- Deterministic, not persuasive. The current prompt happens to name the lower-back
  case; nothing covers a shoulder, and nothing fails loudly when the model ignores
  the advice.
- Severity graduates the response. `health_entries.severity` is already
  mild/moderate/severe and is currently used for nothing: mild means work around
  it, severe means do not train it.
- Testable: given an active injury, assert the design contains no contraindicated
  movement. The one place in this epic where the test *is* the point.
- This is decision support, not medical advice, and the boundary should be explicit
  in what the user sees. A PT refers out; so should this.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C5.1]

## Epic 6: Progress, analytics & feedback

Spec §2 and §4. The good news: most of the SQL already exists. `progress.rs` and
`dashboard.rs` implement exercise / muscle-group / goal time series, goal progress
reports, personal records, workout streaks and weekly volume by muscle group.

The bad news: **almost none of it is reachable.** `goal_progress_report` has one
production caller (the designer, `handler.rs:447`); `personal_records`,
`workout_streak`, `week_summary`, `volume_by_muscle_group_weekly`,
`exercise_time_series`, `muscle_group_time_series` and `goal_time_series` are
called only from `backend/tests/integration_tests.rs`. The dashboard that used
them was removed (see `05-confide-support/up.sql:9`) and nothing replaced it. This
epic is mostly *surfacing* work, not query work.

### [C6.1] Reconnect the analytics layer

#### Description
Audit what `progress.rs` and `dashboard.rs` already compute, verify it against the
generalised goal model, and give it a production path. Do this before building
views on top — the answer to "how much of §2 already works" changes the size of
everything below it.

- Inventory each function: correct, needs work, or obsolete. They were written for
  a dashboard that no longer exists and have not been exercised in anger since;
  test-only code drifts quietly.
- `direction` from [C3.1] is the known break. Every "best" and "progress"
  computation assumes bigger is better (`best_value_for_exercise_type`), which is
  wrong for weightloss the moment [C3.1] lands.
- Test-only code is not a free asset. Either it earns a caller here or it is
  deleted — leaving it in place is what produced this situation.
- Delete rather than preserve whatever the new model obsoletes.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C3.1]

### [C6.2] Chartable series in the View contract

#### Description
"We can plot charts (or get chartable data) related to each exercise and goal" —
the parenthetical is the actual requirement for Core. Ship the data; clients plot
it.

- A `View` variant carrying labelled series with units and a direction-aware sense
  of "better". A rendered chart is a client concern ([T2.2], [G1.2]).
- Append the variant; never extend an existing one — postcard, see [C2.1].
- Covers exercise progression, muscle-group volume, goal trajectory and body
  metrics ([C3.2]).
- The series model should be general enough that a client renders one chart type
  per series *shape*, rather than special-casing each metric. Every metric needing
  bespoke client code is how this ends up rebuilt once per client.
- `TimeSeries` / `TimeSeriesPoint` already exist as DB-side aggregates
  (`models.rs:496`); this is the wire-side view model, deliberately decoupled per
  `crates/proto/src/view.rs:1-10`.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C6.1]

### [C6.3] Progress command

#### Description
The user-facing way to ask "how am I doing" — §2's "how far we are from our
goals". The programme half is [C4.6].

- Per goal: current value, target, trend, time remaining, and whether the trend
  gets there.
- Direction-aware throughout ([C3.1]) — for a weightloss goal, down is progress.
- Composes the series from [C6.2] rather than growing a second query path.
- Registered through whatever [C2.1] settles on for the command table, so it
  appears in `/help` and the advertised set for free.

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C6.2]

### [C6.4] Goal projection and likelihood

#### Description
"Projected likelihood for success" — §2. The most quantitative claim in the spec
and the one most able to mislead.

- Extrapolate each goal's trend against its target date and express how likely it
  is to land.
- Honesty over precision. This is a trend line over noisy, sparse, self-reported
  data; a false "87%" is worse than an honest "on track / at risk / off track",
  and the temptation to invent the number should be resisted in the model, not
  patched in the wording afterwards.
- Say when there isn't enough data. Two points do not make a projection, and "too
  early to say" is a real answer a PT would give.
- Adherence ([C4.4]) dominates when a programme is active — the best predictor of
  hitting a goal is turning up.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C6.2], [C4.4]

### [C6.5] Post-session report

#### Description
"Post session feedback. Give an honest assessment of how the session went" — §4.
Nothing exists today: a session ends, the bound plan is marked completed
(`handler.rs:1006`), and that is the last the user hears about it.

- Triggered on session end: what was done, how it compared to the prescription
  ([C1.5]), PRs hit, effort, and how it moved each goal.
- **Honest** is the operative word and the reason this isn't just a summary.
  Congratulating someone for a session they visibly phoned in is exactly what a
  human PT would not do, and it is the failure mode an LLM defaults to.
- Data first, then commentary — grounded in the deltas from [C1.5] rather than
  composed from vibes.
- A new `View` variant, appended ([C2.1]); rendered by [T2.3] and [G1.3].

#### Metadata
- Priority: P1
- Progress: Not started
- Depends on: [C1.5], [C6.2], [C3.3]

### [C6.6] Shortfall commentary and remediation

#### Description
"Intelligent commentary on where we might be falling short and suggested
remediation" — §2. The step from reporting numbers to actually coaching.

- Identify where progress has stalled, which goals are at risk, and what to change
  — the diagnosis and the prescription, not just the chart.
- Grounded in the projections ([C6.4]) and adherence ([C4.4]), not free-form LLM
  opinion over a transcript.
- Remediation must be actionable: "your bench hasn't moved in five weeks and you
  hit every prescribed rep — the load is too light" beats "try harder".
- Distinguish the causes, because the remedies differ: not turning up, turning up
  and under-performing, and a badly-set goal are three different problems and only
  the middle one is about training.

#### Metadata
- Priority: P2
- Progress: Not started
- Depends on: [C6.4]


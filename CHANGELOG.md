# Changelog

All notable changes to GymBuddy are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This log starts at 0.22.0; earlier releases are not covered.

## [0.22.0] - 2026-07-18

> Terminology note: this release shipped before the 2026-07-20 vocabulary realignment. Entries below are written in
> current terms — `SessionRoster` for the built session artefact, `programme` throughout — so the log reads against
> today's codebase. Behaviour is described as it shipped; only the names have moved. See the Nomenclature section of
> `README.md`.

### Added

#### Long-term programmes

- Programme skeleton in the schema and a `programmes.rs` DAO (migration `12-programmes`): programmes carry a title,
  start and target-end dates, days per week, free-text split and progression policy, and a
  draft/active/completed/abandoned status. Mesocycles are inclusive week ranges with a focus, and the week/day grid
  is a table of slots with pending/filled/missed/skipped status. Each user holds at most one live draft and one
  active programme.
- Programmes serve goals through a many-to-many join, so the goals a programme is built around are explicit.
- `/nextworkout` resolves an explicit training mode before designing. Under an active programme the design targets
  the current slot — the earliest unresolved cell — and the reply names the programme, week, day and focus. A
  redesign re-targets the same slot rather than burning the next one.
- A leading `adhoc` (also `ad-hoc`, `oneoff`, `one-off`) in the command guidance forces a deliberate one-off under
  an active programme: the roster stays slotless and no slot status moves through design, execution or session end.
- The training mode is carried on the wire as a new `View::ProgrammeSessionRoster` variant and rendered as a single
  line under the roster title in both the Telegram and TUI clients. Ad-hoc designs with no programme in play still
  travel as plain `View::SessionRoster`, byte-identical to the pre-programme protocol.

#### Goals and body metrics

- The exercise-only goal model is now general: goals have a kind (strength, endurance, bodyweight, body
  composition, habit), a direction, a priority, and either an exercise or a metric (migration
  `10-generalise_goals`). Goals that a single exercise's single number cannot express — weight loss, weekly
  frequency habits, faster-time targets — are first-class.
- Direction is honoured end to end: decrease goals take the lowest value as best and invert their progress
  calculation, so weight-loss and faster-time goals report correctly. Priority orders the progress report so the
  designer can rank competing goals.
- Body measurements are stored long-shaped, one row per user/metric/moment (migration `13-body_metrics`). Metric
  names are canonical and unit-suffixed (`bodyweight_kg`, `body_fat_pct`, `waist_cm`, `resting_hr_bpm`) and are
  normalised on write and on read, so a new metric is a new row value rather than a migration.
- Metric-denominated goals now read from the latest measurement on or before the goal window's end, so a weight-loss
  goal can report progress and derive an achieved status.
- A `log_body_metric` action lets free-text weigh-ins log without a command: "I weighed 82.5 this morning" inserts
  the row directly, with no session and no confirmation ritual. Values are metric-unit, and the assistant is
  instructed to convert imperial input first.
- Measurements are never raised unprompted — only when the user asks or an active goal reports on them. They are
  kept indefinitely and erased completely on request.

#### Session designer

- A per-muscle recovery signal: for each top-level muscle group, when it was last trained (rolled up over its
  subtree) and how many sets that day took. Every group is reported, including those never trained, and the
  designer prompt renders them longest-rested first.
- An explicit six-rung selection priority — goal contribution, done before, longest-rested muscles, philosophy fit,
  health issues, temporary requests — replacing the previous loose guidance. Lower rungs break ties rather than
  veto, active health issues stay a hard constraint, and the design rationale must name which priorities drove the
  picks.
- A goal-aware, token-budgeted history window replaces the fixed three-session lookback. Recent sessions render in
  full while older ones collapse to per-exercise trend lines, filled in priority order — goal-relevant lifts and
  their taxonomy descendants before incidental accessories — until the token budget is reached. Goal lifts keep
  more history than accessories. Configurable under a new `[gym.designer_history]` block.
- Prescribed-vs-actual deltas for a rostered session.

#### Session feedback

- Sessions gain a structured verdict distinct from free-form notes: overall effort, how it felt, and whether it was
  cut short with a reason (migration `11-session_outcome`).
- Ending a session proposes an overall effort by distilling the last set of each exercise, so auto-closed and
  continuity-ended sessions get a verdict too. The reply appends a confirm-or-override question, and a
  `record_session_outcome` action applies the answer as a partial update — a bare agreement or a single-field
  correction leaves the rest intact.
- The verdict feeds back into the `/nextworkout` designer history and the recent-history prompt section, closing the
  per-session feedback loop.

#### Coaching

- A `set_session_override` action gives one-off requests voiced mid-workout ("I don't feel like bench today, let's
  do flys") their own home, separate from durable preferences (migration `09-session_overrides`). The override is
  honoured for the rest of the session, expires when the roster completes or is superseded, and never reaches the
  philosophy — so it cannot silently ban a movement from future designs.
- The prompt now draws the one-off vs durable distinction explicitly and, when the scope is genuinely ambiguous
  ("I hate bench press"), instructs the assistant to ask "just today or from now on?" rather than guess.

#### Clients and protocol

- The server answers a new `ListCommands` request with the slash commands available to the asking user, each with a
  one-line description. It is computed per user and per request, so mid-session beta grants are picked up and
  commands a user cannot run are never named.
- The TUI completes slash commands with Tab. The first Tab completes to the longest common prefix; once that adds
  nothing, repeated Tabs cycle the candidates and wrap. It only fires in the first word of a line starting with `/`,
  and the candidate set comes from the server rather than a hardcoded list.

### Changed

- The slash-command set lives in one table instead of being written out in three places, where it had already
  drifted. `/help` and `/start` both render from it, and `/start` consequently gained `/cancel`.
- The assistant handler was split into per-concern modules (dispatch, execution, designer, interview, LLM,
  continuity, context) — a pure move, with tests travelling with the code they cover.
- GymBuddy reads its own `[gym]` config table rather than expecting the host to surface it, following the removal
  of the `corre-gym` app upstream. The config format is unchanged.

### Fixed

- JSON mode now reaches the provider as `response_format: {"type": "json_object"}`. The flag had always been set on
  the request but the provider dropped it, leaving every prompt's JSON contract resting on prompt text alone. The
  parser's fence tolerance stays as the guard for a model or provider that ignores the setting.
- `/nextworkout` fails loudly with a retry notice when the designer returns no valid roster, instead of rendering
  fallback prose as if a session had been designed. No phantom roster is persisted.
- Conversation history is pruned per user *and* platform, matching how it is read. A chatty session on one client no
  longer silently evicts another client's context.

### Dependency updates

- corre-core: 0.21 => 0.22
- corre-llm: 0.21 => 0.22
- corre-safety: 0.21 => 0.22
- corre-sdk: 0.21 => 0.22

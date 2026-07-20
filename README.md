# GymBuddy

An LLM-backed gym training coach that logs what you actually did and designs what you should do next.

You talk to it in plain language — "bench 3x8 at 60", "what did I squat last time?", "/nextworkout" — and it keeps the
books: sessions, sets, goals, injuries, body measurements. Session design is grounded in a curated, in-repo training
science corpus rather than the model's unaided recall, and in a long-term programme when you have one.

## Layout

| Path              | What it is                                                                              |
| ----------------- | --------------------------------------------------------------------------------------- |
| `backend/`        | The server: SQLite store (`db/`), the assistant loop (`assistant/`), the training science corpus and index (`science/`), the progressive-overload policy (`progression/`), the v1→v2 import path (`dump/`), and the Telegram client (`telegram/`) |
| `backend/migrations/` | The schema. `01-schema/up.sql` is the v2 baseline and the best single description of the domain |
| `backend/science/`| The curated science corpus: markdown with YAML front-matter, compiled into the binary     |
| `crates/proto/`   | Wire types shared by server and clients. postcard-encoded, so variant *order* is the format |
| `crates/client/`  | UI-agnostic client core: confide transport and identity, no rendering                     |
| `crates/tui/`     | ratatui terminal client, sitting on `crates/client`                                       |
| `crates/timer/`   | Rest-timer core                                                                           |
| `services/`       | Piper (TTS) and Whisper (ASR) sidecars                                                    |

Clients reach the server over [confide](https://github.com/CjS77/confide), an encrypted p2p transport.

## Nomenclature

**One term, one meaning.** This section is the reference for that rule: every domain term below has exactly one
sense, and the codebase is expected to match. If you find a name used two ways, that is a bug — fix the code or fix
this list, but do not let both readings live.

Where a term maps to a table or a Rust type, both are named. Types live in `backend/src/db/models.rs` unless stated.

### What you did

**Session** — one training session, `started_at` to `ended_at`. Table `sessions`, type `Session`. It also carries the
session-level verdict — `overall_effort`, `felt`, `cut_short` — proposed at session end and confirmed or overridden by
the user.
*Not to be confused with SessionRoster.* A Session is what happened. A SessionRoster is what was proposed. A Session
can exist with no roster (you just started lifting) and a roster can exist with no session (designed, never done).

**ExerciseEntry** — a block of related sets for one exercise, within a session. Table `exercise_entries`, type
`ExerciseEntry`. `session_id` is nullable on purpose: a set logged outside any session is first-class.

**Set** — one recorded effort. Table `sets`, type `ExerciseSet` (note the type name; `Set` alone would collide with
the standard library). Polymorphic by design: the `(count, value)` pair is read through the entry's measurement type —
`weight_reps` uses both, `time_based`/`distance_based`/`level_based`/`score_based` use `value` only. Any code reading a
set must consult the measurement type first.

### What was designed

**SessionRoster** — the built session artefact: a titled, ordered list of prescribed exercises with a rationale,
produced by the session designer (`/nextworkout`). Table `session_rosters`, type `SessionRoster`. It moves through
`LifecycleStatus` (`draft` → `active` → `completed`/`abandoned`) and binds to a Session via `session_id` when the user
starts training against it. A one-off override the user voices mid-session ("no bench today") lives here, on
`override_note`, so it expires with the roster — durable preferences go to Philosophy instead.

**RosterExercise** — one prescribed exercise inside a roster: targets (`target_sets`, `target_reps`,
`target_weight_kg`, `target_secs`) plus notes, in `order_idx` order. Table `roster_exercises`, type `RosterExercise`.
This is the **only** per-exercise prescription table in the schema; ProgrammeSlots deliberately do not hold one.

### The long game

**Programme** — a long-term training plan-of-record: goals served, dates, days per week, split, progression policy.
Table `programmes`, type `Programme`. **A skeleton, not a script** — it does not contain sessions; each session is
still designed on demand against it. `split` and `progression_policy` are free text the LLM reads; no query looks
inside. At most one Programme per user is `active`.

**ProgrammeBlock** — a mesocycle within a Programme: an inclusive, 1-based week range with a focus (weeks 1–4
"hypertrophy", 5–6 "deload"). Table `programme_blocks`, type `ProgrammeBlock`. The designer reads the block the current
week falls in, which is what makes sessions build on one another rather than repeat.

**ProgrammeSlot** — one cell of a Programme's week/day grid, with a `focus` and a `SlotStatus`
(`pending`/`filled`/`missed`/`skipped`). Table `programme_slots`, type `ProgrammeSlot`. `week_idx` is 1-based from the
programme start; `day_idx` is the ordinal *training day within the week*, not a calendar weekday.
*Not to be confused with Programme, and not a roster.* The Programme is the whole skeleton; a Slot is one cell of it.
A Slot's `focus` is a text intent ("upper") — never an exercise list. Exercises appear only when a SessionRoster is
designed for that slot and stamped with its id.

**TrainingMode** — the mode a session design runs in. Type `TrainingMode`; resolved per design, never persisted.
- `AdHoc` — a one-off. The first-class default: it never requires a Programme, and it stays available *while* one is
  active as a deliberate one-off that leaves every slot untouched.
- `Programme` — the design fills a specific slot of the active Programme; persisting it stamps the roster's
  `programme_slot_id` and marks the slot filled.

**LifecycleStatus** — the four states `draft`/`active`/`completed`/`abandoned`, shared by SessionRoster and Programme
because they are genuinely the same four states over the same stored strings. Distinct from **SlotStatus**, which is
the ProgrammeSlot's own `pending`/`filled`/`missed`/`skipped`.

### Deciding what comes next

**ProgressionDirective** — one exercise's instruction for the next session: `PROGRESS` / `HOLD` / `BACK OFF` /
`DELOAD`, the load it applies to, and the reason it was reached. Type `ProgressionDirective` in
`backend/src/progression/`. Never persisted — recomputed per design from the log, so it can never disagree with the
sets it was derived from. The designer prompt carries it as a **binding** instruction, not a suggestion.

**ProgressionPolicy** — a whole design's worth of directives, plus the week's BlockIntent and an optional
unplanned-deload recommendation for when back-off signals accumulate across several exercises at once. Type
`ProgressionPolicy`, same module.

**BlockIntent** — what a ProgrammeBlock says this week is for, as the progression policy reads it: `Ordinary`, or
`Deload` when the block's focus text says so. A deload overrides ordinary progression outright. Only programme mode
resolves one, and *no* block is `Ordinary` — never an implied deload.

**ExerciseClass** — how big a load step a movement takes: `LowerBodyCompound`, `UpperBodyCompound`, `Isolation`,
`Conditioning`. Derived from catalogue data (top-level muscle group plus the exercise-level `purpose`, which
variations inherit), never stored. It exists because the same 5kg is a routine step on a squat and a 25% jump on a
lateral raise.

*On effort:* sets are logged on the four-point `Difficulty` scale (`easy`/`medium`/`hard`/`failure`) because that is
how people describe a set out loud. There is deliberately **no** RPE or RIR field anywhere in the schema: the scale
is *read* as repetitions in reserve (easy = 4+, medium = 2-3, hard = 1-2, failure = 0) by
`progression::reps_in_reserve`, and that reading is how the log is held against the science corpus's
RIR-denominated prescriptions.

### What the coach knows about you

**Philosophy** — the user's distilled training philosophy: goals, frequency, preferred programmes and styles, and
equipment with limits, as one compact free-text paragraph. Table `philosophies`, type `Philosophy`. Append-only — the
most recent row is the active one, so the history of how someone's training outlook changed is never overwritten.

**Goal** — a target, denominated either by an exercise or by a Metric (at least one is required). Table `goals`, type
`Goal`. Carries a `kind` (strength, endurance, bodyweight, body composition, habit), a `direction` so that "lose 5kg"
and "add 20kg to my squat" are judged by the same code, and a `priority` that orders competing goals for the designer.

**Metric** — a canonical, unit-suffixed name for something measurable about the body: `bodyweight_kg`, `body_fat_pct`,
`waist_cm`, `resting_hr_bpm`. Table `metrics` (a lookup table; no Rust struct of its own). Deliberately unconstrained —
a new metric is a row insert, never a migration.

**BodyMetric** — one measurement of one Metric at one moment. Table `body_metrics`, type `BodyMetric`. Stored
long-shaped, one row per user/metric/moment. The most sensitive data in the schema: never raised unprompted, kept
indefinitely, erased completely on request.

**HealthEntry** — an injury, illness or wellbeing note, with a type and a severity. Table `health_entries`, type
`HealthEntry`. Active entries are a **hard constraint** on session design, not a tie-breaker: the designer substitutes
away from a movement that loads an injured area rather than ranking it lower. **Severity**
(`mild`/`moderate`/`severe`, type `Severity`) graduates that response — mild works around the pattern, moderate removes
it, severe does not load the area at all — and the type is *ordered*, so `Mild < Moderate < Severe` is load-bearing
rather than incidental.

**MovementPattern** — a class of movement an injured body part may not tolerate ("loaded spinal flexion", "overhead
pressing"). Type `MovementPattern` in `backend/src/science/contraindications.rs`. Deliberately coarser than an exercise
and finer than a muscle group: the corpus reasons in patterns, and a muscle group is too blunt to be a rail — "no back
work" would bar the chest-supported row that `injury-lower-back` actually prescribes.

**Contraindication** — one rule pairing a MovementPattern with the Severity at which it stops being a modification and
becomes a bar, plus the substitutions that keep the session's intent. Type `Contraindication`; the table is `RAILS`,
one entry per body part the corpus has a document for. It is a **rail, not a preference**: `violations()` runs after the
designer's response is parsed and before anything is persisted, and a roster containing a barred movement is rejected
outright rather than softened. Every rule is transcribed from the curated document it cites, so a change here without a
matching change to `backend/science/injury-*.md` is a bug. This is decision support, not medical advice, and nothing in
it diagnoses: the rules key off the body part and severity the *user* reported.

### Looking back

**SessionReview** — the durable review of one finished session. Table `session_reviews`, type `SessionReview` (in
`backend/src/dump/model.rs`). One per session; regenerating replaces it. `kind` is `summary` for the deterministic
ad-hoc review or `report` for the programme-mode report with LLM commentary. Its `body` is the schema's only JSON
column, and deliberately so: a review is a snapshot of what was true when it was written, and a later edit to a set
must not rewrite it.
*Not to be confused with the Session verdict* (`overall_effort`, `felt`, `cut_short`), which is a handful of
structured fields on the Session itself, captured at session end.

### Reference data

**ExerciseType** — one exercise in the taxonomy ("Barbell Bench Press"), with a parent, a level and a measurement
type. Table `exercise_types`, types `ExerciseType` and `ExerciseTypeWithAncestry`. The tree matters: queries roll up
over descendants, so "chest" resolves through its children.

**Catalogue** — the whole seeded set of ExerciseTypes, plus the measurement types and canonical metric names that come
with it. Seeded from `backend/catalogue/` at startup and reached through `db/catalogue.rs`. "The catalogue" means this
reference data; it is not a user-owned table.

**ScienceChunk** — one heading-level chunk of the curated training science corpus, carrying its text and a rendered
`[S:doc-id]` citation. Type `ScienceChunk` in `backend/src/science/`. Sourced from `backend/science/*.md`, compiled
into the binary, retrieved per design and injected into the designer prompt to narrow the *prescription* (rep ranges,
intensity, rest, volume) while the model still chooses the exercises. Not persisted, not user data.

### Words that are not domain terms

**workout** — informal English for a Session, or for the act of training. It is **not** a domain term: no table, type
or field is named `workout`. It survives deliberately on user-facing surfaces where it reads naturally — the
`/nextworkout` command, "Recent workouts", "no workout history yet" — because those are the user's words. In code,
doc comments, commit messages and this README, say **Session** for what was done and **SessionRoster** for what was
designed. Do not introduce `workout` as an identifier.

**plan** — retired. It was v1's word for what is now a SessionRoster, and its ambiguity (one session? the long-term
programme?) is why the vocabulary was realigned. It survives *only* as a frozen storage contract in the v1 import path
— the `workout_plans`/`workout_plan_exercises` tables, the `plan:` sentinel in v1 `sessions.notes`, and the dropped
`plan` field still present in replayed conversation history. Those spellings must never be "modernised": they read
data that already exists. Everywhere else, use SessionRoster or Programme, whichever you actually mean.

**program** — the spelling is **programme**, everywhere. `program*` appears only in v1 fixtures and the v1 reader,
where it is likewise a frozen contract. ("Programming", as in "training programming", is ordinary English and is fine
in the science corpus.)

---

Keep this section current: a phase that adds a domain concept adds its term here in the same change.

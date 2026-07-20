//! The `/programme` interview ([C4.2]): goals, philosophy and history in, a
//! multi-week programme skeleton out.
//!
//! Structurally this is [`super::interview`]'s philosophy interview with a different
//! prompt and a different terminal action — same `interview_states` row, same generic
//! `/cancel`, same turn-bounded wrap-up. It differs in two places, both deliberate:
//!
//! * It has **preconditions**, mirroring `/nextworkout`'s refusals. No philosophy and
//!   there is nothing to shape a programme from; no active goal and a programme is
//!   just a calendar. Both refusals name the command that fixes them.
//! * It has a **second phase**. The philosophy interview ends the moment the model
//!   saves; a programme is proposed as a draft, shown to the user, and only becomes
//!   live when they say so. The draft's id is parked in the interview state's `draft`
//!   column, which is what arms the lock-in turn — no scraping of message history is
//!   needed, unlike the continuity resume, because the state itself records the ask.

use anyhow::Context as _;

use crate::assistant::actions::{AssistantAction, ProposedProgrammeBlock, ProposedProgrammeDay};
use crate::assistant::parser::parse_assistant_response;
use crate::assistant::prompts::build_programme_prompt;
use crate::db::{
    Database, GoalProgress, InterviewState, Programme, User, new_programme, new_programme_block, new_programme_slot,
};

use super::AssistantHandler;
use super::affirmative::is_affirmative;
use gymbuddy_proto::{ProgrammeBlockView, ProgrammeDayView, ProgrammeView, View};

/// The `interview_states.mode` value this interview runs under. The column's CHECK
/// constraint accepts exactly `'philosophy'` and `'programme'`.
const PROGRAMME_MODE: &str = "programme";

/// The design overruns the default token cap — a programme carries blocks, a week
/// template and a progression policy in one envelope. Matches the session designer.
const PROGRAMME_MAX_TOKENS: u32 = 2048;

/// Upper bound on programme length. A year of training is already far beyond what
/// anyone should commit to in one skeleton, and the value drives a slot-insert loop,
/// so an unbounded number from the model must not reach the database.
const MAX_PROGRAMME_WEEKS: i32 = 52;

/// Upper bound on training days in the repeating week.
const MAX_DAYS_PER_WEEK: usize = 7;

impl AssistantHandler {
    /// Enter the multi-turn `/programme` interview and return the opening question.
    ///
    /// Refuses before arming anything when the inputs a programme needs are missing,
    /// so the user is never walked through an interview that cannot conclude.
    pub(super) async fn cmd_programme_start(&self, user: &User, platform: &str) -> anyhow::Result<View> {
        let (philosophy, goals, active) = {
            let db = self.db.lock().await;
            (db.latest_philosophy(user.id)?, db.goal_progress_report(user.id, None, None)?, db.active_programme_for_user(user.id)?)
        };

        // Same refusal shape as `/nextworkout`: name what is missing and the command
        // that supplies it, rather than guessing a programme out of nothing.
        let Some(philosophy) = philosophy else {
            return Ok(View::notice(
                "I don't have a training philosophy for you yet, so I can't build a programme around it. \
                 Run /philosophy and we'll sort that out first.",
            ));
        };
        if goals.is_empty() {
            return Ok(View::notice(
                "A programme needs a goal to serve -- without one it's just a calendar. \
                 Tell me one target you want to aim at (a lift number, a bodyweight figure, or a weekly \
                 training habit) and I'll set it up, then run /programme again.",
            ));
        }

        self.db.lock().await.set_interview_state(user.id, platform, PROGRAMME_MODE, "", 0)?;

        let opener = programme_opener(&philosophy.content, &goals, active.as_ref());
        self.store_conversation_on_platform(user.id, platform, "/programme", &opener).await?;
        Ok(View::message(opener))
    }

    /// One turn of the `/programme` interview.
    ///
    /// Two phases, told apart by whether `state.draft` holds a proposed programme's id:
    /// before a proposal it is an ordinary interview turn; after one, an affirmative
    /// locks the draft in and anything else falls back to another interview turn, so
    /// "make it 8 weeks instead" simply produces a revised proposal (which supersedes
    /// the earlier draft, because `create_programme` abandons it).
    pub(super) async fn programme_interview_turn(
        &self,
        user: &User,
        text: &str,
        platform: &str,
        state: &InterviewState,
    ) -> anyhow::Result<View> {
        if let Some(programme_id) = state.draft.parse::<i64>().ok().filter(|_| is_affirmative(text)) {
            return self.lock_in_programme(user, programme_id, text, platform).await;
        }

        let (system_prompt, history) = self.build_programme_turn_prompt(user, platform, state.turns).await?;

        let llm_response = match self.call_llm_with(&system_prompt, &history, text, PROGRAMME_MAX_TOKENS, 0.2).await {
            Ok(response) => response,
            Err(e) => {
                tracing::error!("Programme interview LLM call failed: {e:#}");
                self.store_excluded_conversation_on_platform(user.id, platform, text, "interview error").await?;
                return Ok(View::notice("I had trouble with that -- could you say it again? (or /cancel to stop)"));
            }
        };

        let parsed = parse_assistant_response(&llm_response);
        let proposal = parsed.actions.into_iter().find_map(|action| match action {
            AssistantAction::ProposeProgramme { title, weeks, days_per_week, split, progression_policy, blocks, week_template, goal_ids } => {
                Some(Proposal { title, weeks, days_per_week, split, progression_policy, blocks, week_template, goal_ids })
            }
            _ => None,
        });

        self.store_conversation_on_platform(user.id, platform, text, &llm_response).await?;

        let Some(proposal) = proposal else {
            self.db.lock().await.set_interview_state(user.id, platform, PROGRAMME_MODE, &state.draft, state.turns + 1)?;
            return Ok(View::message(crate::text::strip_markdown(&parsed.message)));
        };

        // A proposal that cannot be turned into a grid is not a programme. Fail loud and
        // persist nothing rather than saving a skeleton with no slots, which would leave
        // `/nextworkout` silently falling back to ad-hoc for a user who thinks they have
        // a programme. Same rule as the session designer's invalid-design path.
        let Some(shape) = ProgrammeShape::from_proposal(&proposal) else {
            tracing::warn!(user_id = user.id, weeks = proposal.weeks, days = proposal.week_template.len(), "invalid propose_programme");
            self.db.lock().await.set_interview_state(user.id, platform, PROGRAMME_MODE, &state.draft, state.turns + 1)?;
            return Ok(View::notice(
                "I couldn't turn that into a workable programme -- I need a length in weeks and what each \
                 training day of the week is for. Shall we try again? (or /cancel to stop)",
            ));
        };

        let (programme_id, view) = self.persist_programme(user, &proposal, &shape).await?;
        // Park the draft's id: the next turn's affirmative locks in THIS programme.
        self.db.lock().await.set_interview_state(user.id, platform, PROGRAMME_MODE, &programme_id.to_string(), state.turns + 1)?;
        Ok(View::Programme(Box::new(view)))
    }

    /// The programme interview's system prompt and conversation history for one turn.
    async fn build_programme_turn_prompt(
        &self,
        user: &User,
        platform: &str,
        turns: i32,
    ) -> anyhow::Result<(String, Vec<crate::db::ConversationMessage>)> {
        let db = self.db.lock().await;
        let philosophy = db.latest_philosophy(user.id)?.map(|p| p.content).unwrap_or_default();
        let goals = db.goal_progress_report(user.id, None, None)?;
        let goal_ids = super::designer::goal_relevant_exercise_ids(&db, &goals)?;
        let sessions = db.recent_sessions_with_sets(user.id, self.config.designer_history.max_sessions)?;
        let injuries = db.list_active_health_entries(user.id)?;
        let history = db.get_recent_messages_for_platform(user.id, platform, self.config.conversation_history_limit)?;
        drop(db);

        let history_block = self.format_designer_history(&sessions, &goal_ids);
        Ok((build_programme_prompt(&philosophy, &goals, &history_block, &injuries, turns), history))
    }

    /// Persist a proposed programme as a draft: the programme row, its blocks, the
    /// `week_template × weeks` slot grid, and the `programme_goals` links. Returns the
    /// new programme's id alongside the view of it.
    ///
    /// Nothing here activates: [`Self::lock_in_programme`] is the only path to that.
    async fn persist_programme(
        &self,
        user: &User,
        proposal: &Proposal,
        shape: &ProgrammeShape,
    ) -> anyhow::Result<(i64, ProgrammeView)> {
        let db = self.db.lock().await;

        let mut draft = new_programme(user.id, &proposal.title, shape.days_per_week as i32, &proposal.split, &proposal.progression_policy);
        draft.target_end_date = Some(shape.target_end_date());
        let programme_id = db.create_programme(&draft).context("creating the draft programme")?;

        write_blocks_and_grid(&db, programme_id, &shape.blocks(proposal), shape)?;
        let (goal_labels, goal_notes) = link_goals(&db, user.id, programme_id, &proposal.goal_ids)?;

        let notes = shape.notes.iter().cloned().chain(goal_notes).collect();
        let programme = db.get_programme(programme_id)?.context("draft programme disappeared after insert")?;
        let view = stored_programme_view(&db, &programme, goal_labels, notes, false)?;
        Ok((programme_id, view))
    }

    /// The user said yes: activate the draft (superseding any programme already active)
    /// and leave the interview. Rendered back as an active [`View::Programme`], which is
    /// the same artefact minus the lock-in ask.
    async fn lock_in_programme(&self, user: &User, programme_id: i64, text: &str, platform: &str) -> anyhow::Result<View> {
        let view = {
            let db = self.db.lock().await;
            db.activate_programme(programme_id).context("activating the programme")?;
            db.clear_interview_state(user.id, platform)?;
            let programme = db.get_programme(programme_id)?.context("programme disappeared after activation")?;
            let goals = linked_goal_labels(&db, user.id, programme_id)?;
            let note = "Locked in -- this is now your active programme. /nextworkout will design each session against it.";
            stored_programme_view(&db, &programme, goals, vec![note.to_string()], true)?
        };

        tracing::info!(user_id = user.id, programme_id, "programme locked in and activated");
        self.store_conversation_on_platform(user.id, platform, text, "Locked in -- your programme is now active.").await?;
        Ok(View::Programme(Box::new(view)))
    }
}

/// The opening turn: confirm what is already on file, then ask only what a programme
/// adds. Names the goals by label so the user can answer "the bench one" straight away.
fn programme_opener(philosophy: &str, goals: &[GoalProgress], active: Option<&Programme>) -> String {
    let goal_list = goals.iter().map(|gp| format!("- {}", goal_label(gp))).collect::<Vec<_>>().join("\n");
    let superseding = match active {
        Some(p) => format!(
            "\n\nHeads up: you already have an active programme (\"{}\"). If you lock a new one in, it replaces that one.",
            p.title
        ),
        None => String::new(),
    };
    format!(
        "Let's build your multi-week programme -- the skeleton your sessions then get designed against.\n\n\
         I'll use the philosophy you already have on file, so I won't ask about that again:\n\
         \"{philosophy}\"\n\n\
         Your active goals are:\n{goal_list}\n\n\
         Two things I need: roughly how long should this run, or is there a date you're training toward? \
         And which of those goals is it for?{superseding}\n\n\
         (/cancel at any point to stop.)"
    )
}

/// One `propose_programme` action, lifted out of the parsed response.
struct Proposal {
    title: String,
    weeks: i32,
    days_per_week: i32,
    split: String,
    progression_policy: String,
    blocks: Vec<ProposedProgrammeBlock>,
    week_template: Vec<ProposedProgrammeDay>,
    goal_ids: Vec<i64>,
}

/// The validated, bounded shape of a proposal — what actually reaches the database.
///
/// Built by [`Self::from_proposal`], which is where every value the model supplied is
/// forced into a range the schema and the slot-expansion loop can survive. Anything it
/// had to change is recorded in `notes`, so a silently reshaped programme is never
/// presented as the one that was proposed.
struct ProgrammeShape {
    weeks: u32,
    days_per_week: usize,
    week_template: Vec<ProgrammeDayView>,
    notes: Vec<String>,
}

impl ProgrammeShape {
    /// Validate and bound a proposal, or `None` when it cannot be a programme at all —
    /// no positive length, or no training days to repeat.
    fn from_proposal(proposal: &Proposal) -> Option<Self> {
        if proposal.weeks < 1 || proposal.week_template.is_empty() {
            return None;
        }
        let mut notes = Vec::new();

        let weeks = if proposal.weeks > MAX_PROGRAMME_WEEKS {
            notes.push(format!("Capped the programme at {MAX_PROGRAMME_WEEKS} weeks."));
            MAX_PROGRAMME_WEEKS as u32
        } else {
            proposal.weeks as u32
        };

        // `day_idx` is the ordinal training day within the week, so the grid is
        // renumbered 1..n from the template's order rather than trusting the model's
        // indices, which arrive duplicated or 0-based often enough to matter.
        let mut week_template: Vec<ProgrammeDayView> = proposal
            .week_template
            .iter()
            .enumerate()
            .map(|(i, day)| ProgrammeDayView { day_idx: i as u32 + 1, focus: day.focus.clone() })
            .collect();
        if week_template.len() > MAX_DAYS_PER_WEEK {
            notes.push(format!("Trimmed the week to {MAX_DAYS_PER_WEEK} training days."));
            week_template.truncate(MAX_DAYS_PER_WEEK);
        }

        // The grid is built from the template, so the template's length *is* how many
        // days a week this programme trains. When the model's own `days_per_week`
        // disagrees, the template wins and the user is told — silently storing a number
        // the grid contradicts would make every later adherence reading wrong.
        let days_per_week = week_template.len();
        if proposal.days_per_week as usize != days_per_week {
            notes.push(format!(
                "You'll train {days_per_week} days a week -- that's what the week below lays out, though I'd said {}.",
                proposal.days_per_week
            ));
        }
        Some(Self { weeks, days_per_week, week_template, notes })
    }

    /// The date the programme aims to conclude by: `weeks` whole weeks from today.
    fn target_end_date(&self) -> String {
        (chrono::Utc::now() + chrono::Duration::weeks(self.weeks as i64)).format("%Y-%m-%d").to_string()
    }

    /// The proposal's blocks, clamped into the programme's week range. Blocks that
    /// start beyond the last week describe nothing and are dropped; an inverted range
    /// is repaired rather than stored, since `end_week < start_week` covers no weeks.
    fn blocks(&self, proposal: &Proposal) -> Vec<ProgrammeBlockView> {
        proposal
            .blocks
            .iter()
            .filter(|b| b.start_week >= 1 && (b.start_week as u32) <= self.weeks)
            .map(|b| {
                let start_week = b.start_week as u32;
                ProgrammeBlockView { start_week, end_week: (b.end_week.max(b.start_week) as u32).min(self.weeks), focus: b.focus.clone() }
            })
            .collect()
    }
}

/// Write a programme's mesocycle blocks and expand its repeating week across every
/// week into the slot grid. The grid is what [`Database::next_design_slot`] walks, so
/// this is the step that actually makes `/nextworkout` programme-aware.
fn write_blocks_and_grid(db: &Database, programme_id: i64, blocks: &[ProgrammeBlockView], shape: &ProgrammeShape) -> anyhow::Result<()> {
    blocks
        .iter()
        .try_for_each(|b| {
            db.add_programme_block(&new_programme_block(programme_id, b.start_week as i32, b.end_week as i32, &b.focus)).map(|_| ())
        })
        .context("adding programme blocks")?;

    (1..=shape.weeks)
        .flat_map(|week| shape.week_template.iter().map(move |day| (week, day)))
        .try_for_each(|(week, day)| {
            db.add_programme_slot(&new_programme_slot(programme_id, week as i32, day.day_idx as i32, &day.focus)).map(|_| ())
        })
        .context("expanding the programme slot grid")
}

/// Link the goals a proposal claims to serve, and report what was linked plus any
/// caveat the user should see.
///
/// `goal_ids` is model output, so it is filtered against the user's own goals: an id
/// that is not theirs would otherwise silently attach a stranger's goal to their
/// programme. An unmatched id is dropped with a note rather than failing the design.
fn link_goals(db: &Database, user_id: i64, programme_id: i64, goal_ids: &[i64]) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let owned = db.goal_progress_report(user_id, None, None)?;
    let (linked, unknown): (Vec<i64>, Vec<i64>) = goal_ids.iter().partition(|id| owned.iter().any(|gp| gp.goal.id == **id));
    linked.iter().try_for_each(|id| db.add_programme_goal(programme_id, *id)).context("linking programme goals")?;

    let labels = linked_goal_labels(db, user_id, programme_id)?;
    let mut notes = Vec::new();
    if !unknown.is_empty() {
        notes.push(format!("Ignored {} goal reference(s) I couldn't match to your goals.", unknown.len()));
    }
    if labels.is_empty() {
        notes.push("No goal is linked to this programme yet -- tell me which one it serves and I'll attach it.".to_string());
    }
    Ok((labels, notes))
}

/// The view of a stored programme, read back from the grid it was expanded into.
///
/// `weeks` and the week template both come from the slots rather than from anything
/// remembered: the grid is the programme's actual shape, so a view built from it cannot
/// disagree with what `next_design_slot` will walk.
fn stored_programme_view(db: &Database, programme: &Programme, goals: Vec<String>, notes: Vec<String>, active: bool) -> anyhow::Result<ProgrammeView> {
    let slots = db.list_programme_slots(programme.id)?;
    let weeks = slots.iter().map(|slot| slot.week_idx.max(0) as u32).max().unwrap_or(0);
    // Every week was expanded from the same template, so week 1 *is* the template.
    let week_template = slots
        .iter()
        .filter(|slot| slot.week_idx == 1)
        .map(|slot| ProgrammeDayView { day_idx: slot.day_idx.max(0) as u32, focus: slot.focus.clone() })
        .collect();
    let blocks = db
        .list_programme_blocks(programme.id)?
        .iter()
        .map(|b| ProgrammeBlockView {
            start_week: b.start_week.max(0) as u32,
            end_week: b.end_week.max(0) as u32,
            focus: b.focus.clone(),
        })
        .collect();

    Ok(ProgrammeView {
        title: programme.title.clone(),
        start_date: date_part(&programme.start_date),
        target_end_date: programme.target_end_date.clone(),
        weeks,
        days_per_week: programme.days_per_week.max(0) as u32,
        split: programme.split.clone(),
        progression_policy: programme.progression_policy.clone(),
        blocks,
        week_template,
        goals,
        notes,
        active,
    })
}

/// The date half of a stored `YYYY-MM-DD HH:MM:SS` timestamp.
fn date_part(timestamp: &str) -> String {
    timestamp.get(..10).unwrap_or(timestamp).to_string()
}

/// One goal as the display line the programme view lists under "goals served", e.g.
/// "Bench Press to 100.0". Built from [`GoalProgress`] because that is what resolves an
/// exercise goal's name, which the raw [`crate::db::Goal`] holds only an id for.
fn goal_label(gp: &GoalProgress) -> String {
    format!("{} to {:.1}", gp.exercise_name, gp.goal.target_value)
}

/// The display labels for the goals `programme_id` serves, in the DAO's priority order.
fn linked_goal_labels(db: &Database, user_id: i64, programme_id: i64) -> anyhow::Result<Vec<String>> {
    let progress = db.goal_progress_report(user_id, None, None)?;
    Ok(db
        .list_programme_goals(programme_id)?
        .iter()
        .filter_map(|goal| progress.iter().find(|gp| gp.goal.id == goal.id))
        .map(goal_label)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::AssistantHandler;
    use crate::db::{Database, LifecycleStatus, User, new_exercise_goal};
    use crate::telegram::Message as TgMessage;

    /// A valid three-day, six-week proposal — the happy path most tests start from.
    const PROPOSAL: &str = r#"{"message": "Here's the shape I'd suggest.", "actions": [
        {"type": "propose_programme", "title": "6-week base", "weeks": 6, "days_per_week": 3,
         "split": "upper/lower/full", "progression_policy": "add 2.5kg when all reps land",
         "blocks": [{"start_week": 1, "end_week": 4, "focus": "accumulation"},
                    {"start_week": 5, "end_week": 6, "focus": "intensification"}],
         "week_template": [{"day_idx": 1, "focus": "upper push"}, {"day_idx": 2, "focus": "lower"},
                           {"day_idx": 3, "focus": "upper pull"}],
         "goal_ids": [1]}
    ]}"#;

    /// Register a user and give them the two things `/programme` requires.
    async fn setup_ready_user(handler: &AssistantHandler, msg: &TgMessage) -> User {
        let _ = handler.handle_text_message(msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        let db = handler.db.lock().await;
        db.insert_philosophy(user.id, "3x/week, dumbbells to 24kg, likes 5x5", "interview").unwrap();
        seed_goal(&db, user.id);
        user
    }

    /// An active bench-press goal, back-dated so the date-windowed report returns it.
    fn seed_goal(db: &Database, user_id: i64) -> i64 {
        let bench = db.get_exercise_type_by_name("Bench Press").unwrap().unwrap();
        let mut goal = new_exercise_goal(user_id, bench.id, 100.0);
        goal.start_date = "2026-01-01".into();
        db.insert_goal(&goal).unwrap()
    }

    // ── Preconditions ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn programme_without_philosophy_points_at_philosophy() {
        let (handler, _) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/programme").await.unwrap();
        assert!(shown(&reply).contains("/philosophy"), "should point at /philosophy first: {}", shown(&reply));

        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        assert!(
            handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_none(),
            "a refused precondition must not arm the interview"
        );
    }

    /// A programme with no goal is just a calendar, so the refusal offers to set one.
    #[tokio::test]
    async fn programme_without_a_goal_refuses_and_offers_to_set_one() {
        let (handler, _) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let _ = handler.handle_text_message(&msg, "hello").await.unwrap();
        let user = { handler.db.lock().await.get_user_by_telegram_id("12345").unwrap().unwrap() };
        {
            handler.db.lock().await.insert_philosophy(user.id, "3x/week", "interview").unwrap();
        }

        let reply = handler.handle_text_message(&msg, "/programme").await.unwrap();
        let text = shown(&reply).to_lowercase();
        assert!(text.contains("goal"), "the refusal must name what is missing: {text}");
        assert!(text.contains("calendar"), "and say why a goal is required: {text}");
        assert!(handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().is_none());
    }

    /// The philosophy is an input, not a question: the opener confirms what is on file
    /// and spends its questions on what a programme actually adds.
    #[tokio::test]
    async fn opener_confirms_the_philosophy_rather_than_re_interviewing_it() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let before = llm.recorded_requests().len();

        let reply = handler.handle_text_message(&msg, "/programme").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("dumbbells to 24kg"), "the opener must read back the philosophy on file: {text}");
        assert!(text.contains("Bench Press"), "and list the goals it could serve: {text}");
        assert_eq!(llm.recorded_requests().len(), before, "the opener is canned — no LLM round-trip");

        let state = handler.db.lock().await.get_interview_state(user.id, "telegram").unwrap().unwrap();
        assert_eq!(state.mode, "programme", "the interview must be armed in programme mode");
    }

    // ── The interview and its proposal ────────────────────────────────────────

    #[tokio::test]
    async fn a_first_answer_keeps_interviewing_without_proposing() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        llm.set_response(r#"{"message": "Which goal is it for?", "actions": []}"#);
        let reply = handler.handle_text_message(&msg, "about six weeks").await.unwrap();
        assert!(shown(&reply).contains("Which goal"));

        let db = handler.db.lock().await;
        let state = db.get_interview_state(user.id, "telegram").unwrap().unwrap();
        assert_eq!(state.turns, 1, "the turn counter must advance");
        assert_eq!(state.draft, "", "no proposal yet, so no draft id is parked");
        assert!(db.latest_draft_programme(user.id).unwrap().is_none(), "nothing may be persisted before a proposal");
    }

    /// The heart of the ticket: one `propose_programme` becomes a draft programme, a
    /// fully expanded slot grid and the goal links — and activates nothing.
    #[tokio::test]
    async fn a_proposal_persists_a_draft_with_an_expanded_grid() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        llm.set_response(PROPOSAL);
        let reply = handler.handle_text_message(&msg, "six weeks, for the bench goal").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("6-week base"));
        assert!(text.contains("6 weeks × 3 days/week"), "the shape line must reach the user: {text}");
        assert!(text.contains("Weeks 1-4") && text.contains("accumulation"), "blocks must be shown: {text}");
        assert!(text.contains("Day 2: lower"), "the week template must be shown: {text}");
        assert!(text.contains("Bench Press to 100.0"), "the goals served must be shown: {text}");
        assert!(text.contains("Lock it in?"), "a draft must ask for confirmation: {text}");

        let db = handler.db.lock().await;
        let programme = db.latest_draft_programme(user.id).unwrap().expect("a draft programme must be persisted");
        assert_eq!(programme.title, "6-week base");
        assert_eq!(programme.status, LifecycleStatus::Draft, "proposing must never activate");
        assert!(db.active_programme_for_user(user.id).unwrap().is_none(), "nothing is active until the user says so");
        assert_eq!(programme.days_per_week, 3);
        assert_eq!(programme.split, "upper/lower/full");

        // The grid is `week_template × weeks`, which is what `next_design_slot` walks.
        let slots = db.list_programme_slots(programme.id).unwrap();
        assert_eq!(slots.len(), 18, "6 weeks x 3 days");
        assert_eq!(slots.iter().filter(|s| s.week_idx == 6).count(), 3, "the last week is expanded too");
        assert_eq!(slots[0].focus, "upper push");
        assert_eq!(db.next_design_slot(programme.id).unwrap().unwrap().week_idx, 1);

        assert_eq!(db.list_programme_blocks(programme.id).unwrap().len(), 2);
        assert_eq!(db.list_programme_goals(programme.id).unwrap().len(), 1, "the named goal is linked");

        // Parked so the next affirmative knows which programme it is locking in.
        let state = db.get_interview_state(user.id, "telegram").unwrap().unwrap();
        assert_eq!(state.draft, programme.id.to_string());
    }

    /// A proposal the host cannot turn into a grid must persist nothing rather than
    /// leave a slot-less programme that `/nextworkout` would silently ignore.
    #[tokio::test]
    async fn a_proposal_with_no_week_template_fails_loud_and_persists_nothing() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        llm.set_response(r#"{"message": "Done!", "actions": [
            {"type": "propose_programme", "title": "Empty", "weeks": 6, "days_per_week": 3}
        ]}"#);
        let reply = handler.handle_text_message(&msg, "go on").await.unwrap();
        assert!(shown(&reply).to_lowercase().contains("couldn't turn that into a workable programme"), "got: {}", shown(&reply));

        let db = handler.db.lock().await;
        assert!(db.latest_draft_programme(user.id).unwrap().is_none(), "no phantom programme may be persisted");
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_some(), "the interview stays open to retry");
    }

    /// `goal_ids` is model output: an id that is not this user's must never be linked.
    #[tokio::test]
    async fn goal_ids_that_are_not_the_users_are_ignored_with_a_note() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        // A second user's goal, whose id the model will name.
        let stranger_goal = {
            let db = handler.db.lock().await;
            let stranger = db.insert_user(&crate::db::new_user("Stranger", Some("999"), "UTC")).unwrap();
            seed_goal(&db, stranger)
        };
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        llm.set_response(&PROPOSAL.replace("\"goal_ids\": [1]", &format!("\"goal_ids\": [{stranger_goal}]")));
        let reply = handler.handle_text_message(&msg, "go").await.unwrap();
        assert!(shown(&reply).contains("couldn't match"), "the ignored reference must be surfaced: {}", shown(&reply));

        let db = handler.db.lock().await;
        let programme = db.latest_draft_programme(user.id).unwrap().unwrap();
        assert!(db.list_programme_goals(programme.id).unwrap().is_empty(), "another user's goal must never be linked");
    }

    /// The model's `days_per_week` is not authoritative — the template it actually gave
    /// is, because that is what the grid was built from.
    #[tokio::test]
    async fn the_week_template_wins_over_a_contradictory_days_per_week() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        llm.set_response(r#"{"message": "Here.", "actions": [
            {"type": "propose_programme", "title": "Mismatched", "weeks": 2, "days_per_week": 5,
             "week_template": [{"day_idx": 1, "focus": "full body"}, {"day_idx": 2, "focus": "full body"}]}
        ]}"#);
        let reply = handler.handle_text_message(&msg, "go").await.unwrap();
        assert!(shown(&reply).contains("2 days a week"), "the correction must be surfaced: {}", shown(&reply));

        let db = handler.db.lock().await;
        let programme = db.latest_draft_programme(user.id).unwrap().unwrap();
        assert_eq!(programme.days_per_week, 2, "the stored value must match the grid");
        assert_eq!(db.list_programme_slots(programme.id).unwrap().len(), 4);
    }

    // ── Lock-in ───────────────────────────────────────────────────────────────

    /// The whole second phase: a plain "yes" after a proposal activates it, decided
    /// server-side with no LLM round-trip.
    #[tokio::test]
    async fn an_affirmative_after_the_proposal_activates_it_without_an_llm_call() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();
        llm.set_response(PROPOSAL);
        let _ = handler.handle_text_message(&msg, "six weeks").await.unwrap();
        let before = llm.recorded_requests().len();

        let reply = handler.handle_text_message(&msg, "yes").await.unwrap();
        let text = shown(&reply);
        assert!(text.contains("Locked in"), "the activation must be confirmed: {text}");
        assert!(text.contains("/nextworkout"), "and say what happens next: {text}");
        assert!(!text.contains("Lock it in?"), "an active programme must not still be asking: {text}");
        assert_eq!(llm.recorded_requests().len(), before, "the yes must be decided server-side");

        let db = handler.db.lock().await;
        let active = db.active_programme_for_user(user.id).unwrap().expect("the programme must be active");
        assert_eq!(active.title, "6-week base");
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_none(), "the interview must close");
    }

    /// Activation supersedes: one active programme per user, so an older one is stood down.
    #[tokio::test]
    async fn locking_in_supersedes_a_previously_active_programme() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let old = {
            let db = handler.db.lock().await;
            let id = db.create_programme(&crate::db::new_programme(user.id, "Old plan-of-record", 3, "full body", "linear")).unwrap();
            db.activate_programme(id).unwrap();
            id
        };

        // The opener warns that locking a new one in replaces it.
        let opener = handler.handle_text_message(&msg, "/programme").await.unwrap();
        assert!(shown(&opener).contains("Old plan-of-record"), "the user must be told what would be replaced: {}", shown(&opener));

        llm.set_response(PROPOSAL);
        let _ = handler.handle_text_message(&msg, "six weeks").await.unwrap();
        let _ = handler.handle_text_message(&msg, "yes please").await.unwrap();

        let db = handler.db.lock().await;
        assert_eq!(db.active_programme_for_user(user.id).unwrap().unwrap().title, "6-week base");
        assert_eq!(db.get_programme(old).unwrap().unwrap().status, LifecycleStatus::Abandoned);
    }

    /// Not every reply to "lock it in?" is a yes. "Make it 8 weeks" must revise, which
    /// means another interview turn and a fresh proposal superseding the first draft.
    #[tokio::test]
    async fn a_revision_after_the_proposal_replaces_the_draft_rather_than_activating() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();
        llm.set_response(PROPOSAL);
        let _ = handler.handle_text_message(&msg, "six weeks").await.unwrap();
        let first = handler.db.lock().await.latest_draft_programme(user.id).unwrap().unwrap().id;

        llm.set_response(&PROPOSAL.replace("6-week base", "8-week base").replace("\"weeks\": 6", "\"weeks\": 8"));
        let reply = handler.handle_text_message(&msg, "make it 8 weeks instead").await.unwrap();
        assert!(shown(&reply).contains("8-week base"));

        let db = handler.db.lock().await;
        assert!(db.active_programme_for_user(user.id).unwrap().is_none(), "a revision must not activate anything");
        let draft = db.latest_draft_programme(user.id).unwrap().unwrap();
        assert_ne!(draft.id, first, "the revision is a new draft");
        assert_eq!(db.get_programme(first).unwrap().unwrap().status, LifecycleStatus::Abandoned, "the superseded draft is abandoned");
        assert_eq!(db.list_programme_slots(draft.id).unwrap().len(), 24, "8 weeks x 3 days");
    }

    /// `/cancel` mid-interview leaves the proposal as a draft — saved, but not live.
    #[tokio::test]
    async fn cancel_after_a_proposal_leaves_it_a_draft() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();
        llm.set_response(PROPOSAL);
        let _ = handler.handle_text_message(&msg, "six weeks").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/cancel").await.unwrap();
        assert!(shown(&reply).contains("draft"), "cancelling must say the draft was kept: {}", shown(&reply));

        let db = handler.db.lock().await;
        assert!(db.active_programme_for_user(user.id).unwrap().is_none(), "cancel must never activate");
        assert!(db.latest_draft_programme(user.id).unwrap().is_some(), "the draft survives the cancel");
        assert!(db.get_interview_state(user.id, "telegram").unwrap().is_none());
    }

    /// Cancelling before anything was proposed must not claim a draft was kept.
    #[tokio::test]
    async fn cancel_before_a_proposal_says_nothing_was_created() {
        let (handler, _llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        let reply = handler.handle_text_message(&msg, "/cancel").await.unwrap();
        assert!(shown(&reply).contains("no programme was created"), "got: {}", shown(&reply));
        assert!(handler.db.lock().await.latest_draft_programme(user.id).unwrap().is_none());
    }

    // ── The prompt ────────────────────────────────────────────────────────────

    /// The prompt must carry goal *ids*, because `propose_programme` names goals by id
    /// and a model can only return an id it was shown.
    #[tokio::test]
    async fn the_prompt_lists_goal_ids_and_advertises_only_propose_programme() {
        let (handler, llm) = setup_handler("").await;
        let msg = make_message(12345, "hello");
        let user = setup_ready_user(&handler, &msg).await;
        let goal_id = handler.db.lock().await.goal_progress_report(user.id, None, None).unwrap()[0].goal.id;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        llm.set_response(r#"{"message": "Which goal?", "actions": []}"#);
        let _ = handler.handle_text_message(&msg, "six weeks").await.unwrap();

        let prompt = llm.recorded_requests().pop().unwrap().messages[0].content.clone();
        assert!(prompt.contains(&format!("id={goal_id} Bench Press")), "goal ids must reach the model:\n{prompt}");
        assert!(prompt.contains("dumbbells to 24kg"), "the philosophy is an input to the prompt:\n{prompt}");
        assert!(prompt.contains("propose_programme"), "the terminal action must be advertised");
        assert!(!prompt.contains("propose_session_roster"), "the programme interview designs no sessions");
        assert!(!prompt.contains("log_exercise"), "and logs nothing");
    }

    /// The wrap-up guard: a small model that keeps interviewing must be told to converge.
    #[tokio::test]
    async fn the_prompt_pushes_for_a_proposal_once_the_interview_has_run_long() {
        let (handler, llm) = setup_handler(r#"{"message": "And another thing?", "actions": []}"#).await;
        let msg = make_message(12345, "hello");
        let _ = setup_ready_user(&handler, &msg).await;
        let _ = handler.handle_text_message(&msg, "/programme").await.unwrap();

        let prompt_after = |llm: &MockLlm| llm.recorded_requests().pop().unwrap().messages[0].content.clone();
        for _ in 0..4 {
            let _ = handler.handle_text_message(&msg, "sure").await.unwrap();
        }
        assert!(!prompt_after(&llm).contains("WRAP UP"), "four turns in, the guard has not fired yet");

        let _ = handler.handle_text_message(&msg, "ok").await.unwrap();
        assert!(prompt_after(&llm).contains("WRAP UP"), "the guard must fire once the interview has run several turns");
    }
}

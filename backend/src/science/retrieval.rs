//! Selecting the handful of corpus chunks a prompt can afford to carry.
//!
//! # The K-50 seam
//!
//! [`ScienceIndex`] is the boundary between *choosing* science and *using* it. Everything above it —
//! the `TRAINING SCIENCE` section of the designer prompt, the programme prompt ([C4.2]), the review
//! commentary ([C6.5]) — talks to exactly these three items and nothing else:
//!
//! - [`ScienceQuery`], composed by the caller from goal kinds, injury body parts, focus tags and free
//!   guidance text;
//! - [`ScienceIndex::build`], called once at startup and held alongside the exercise catalogue;
//! - [`ScienceIndex::search`], returning at most `k` chunks, best first.
//!
//! **[R5.1] / K-50 replaces the bodies of `build` and `search` with an in-memory tantivy index and
//! must not need to change those signatures, [`ScienceQuery`], or a single line of prompt code.**
//! What lives here today is a deliberate placeholder: a tag filter over [`crate::science::CORPUS`],
//! which the corpus already supports without an index because every document carries validated goal
//! kind, muscle group and `injury:<part>` tags. Its known weakness is exactly the one BM25 over chunk
//! bodies fixes — tags live on the *document*, so every chunk of a matching document scores alike and
//! the tie is broken structurally (see [`chunk_bonus`]) rather than by relevance.
//!
//! The one part of the contract that is *not* a ranking detail, and that K-50 must preserve:
//! [`ScienceQuery::pinned_docs`] names documents whose best chunk is emitted whatever the scores say.
//! Rails — the competing-goal resolution when goals genuinely compete ([C5.2]), the contraindications
//! for an active injury ([C5.4]) — cannot be left to a relevance score that might rank them fifth.

use anyhow::Result;

use super::{CORPUS, ScienceChunk, ScienceDoc, goal_kind_tag};
use crate::db::GoalKind;

/// What a prompt wants science about. Composed by the caller; see the module docs for why this type
/// is the stable half of the [R5.1] seam.
#[derive(Debug, Clone, Default)]
pub struct ScienceQuery {
    /// The goal kinds this session serves, **highest priority first** ([C3.1]). Order is meaning, not
    /// presentation: the first kind sets the session's core prescription and outranks the rest.
    pub goal_kinds: Vec<GoalKind>,
    /// Body parts with an active injury, in [`super::INJURY_BODY_PARTS`] spelling. Use
    /// [`normalise_body_part`] to get there from a user-supplied `health_entries.body_part`.
    pub injuries: Vec<&'static str>,
    /// Topic or muscle-group tags from a programme block or slot focus ("hypertrophy", "deload").
    pub focus: Vec<String>,
    /// The user's free guidance for this session ("something lighter today").
    pub guidance: String,
    /// Documents whose best chunk is returned regardless of rank — see the module docs. Ids, as in
    /// `competing-goals`.
    pub pinned_docs: Vec<String>,
}

impl ScienceQuery {
    /// The goal kinds as corpus tags, highest priority first.
    fn goal_tags(&self) -> impl Iterator<Item = &'static str> {
        self.goal_kinds.iter().copied().map(goal_kind_tag)
    }

    /// Focus tags plus whatever words of the free guidance happen to be corpus vocabulary. Crude by
    /// construction — matching prose against a tag list is the placeholder's job, not tantivy's.
    fn focus_tags(&self) -> impl Iterator<Item = String> {
        let from_guidance = self
            .guidance
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|word| super::TOPIC_TAGS.contains(word) || super::MUSCLE_GROUP_TAGS.contains(word))
            .map(str::to_string)
            .collect::<Vec<_>>();
        self.focus.iter().map(|f| f.to_lowercase()).chain(from_guidance)
    }
}

/// A body part as the corpus spells it, from whatever the user or the LLM wrote. `health_entries`
/// stores free text ("lower back", "Lower-Back"); the corpus vocabulary is `lower_back`.
pub fn normalise_body_part(raw: &str) -> Option<&'static str> {
    let normalised = raw.trim().to_lowercase().replace([' ', '-'], "_");
    super::INJURY_BODY_PARTS.into_iter().find(|part| *part == normalised)
}

/// An active injury outranks everything else combined: it is a hard constraint on the session, not a
/// preference, and a contraindication ranked fifth is a contraindication the prompt never carries.
const INJURY_WEIGHT: i32 = 30;
/// Weight of the highest-priority goal kind's tag. Each further rank drops by [`RANK_STEP`] so a
/// second goal genuinely counts for less rather than averaging with the first.
const TOP_GOAL_WEIGHT: i32 = 8;
const RANK_STEP: i32 = 2;
/// Floor for the weight of a low-ranked goal kind — still relevant, never decisive.
const MIN_GOAL_WEIGHT: i32 = 4;
/// A block/slot focus or guidance word that lands on corpus vocabulary. As heavy as the top goal:
/// "this is a deload week" says as much about today's prescription as the goal it serves does.
const FOCUS_WEIGHT: i32 = 8;
/// Awarded to a document that prescribes at all — one of its chunks names a prescription. Heavier
/// than a single tag match on purpose: the tag vocabulary cannot tell a document *about* strength
/// from one that merely mentions it, and of the two, the prescribing document is the one worth the
/// prompt's budget. This is the crudest part of the placeholder; BM25 over chunk text is the answer.
///
/// Document-level and uniform: *whether* a document prescribes decides its rank, *which* of its
/// chunks prescribes best decides only what is quoted from it ([`chunk_bonus`]). Letting the second
/// question leak into the first made a document's rank depend on how its author phrased a heading.
const PRESCRIPTION_BONUS: i32 = 10;
/// Chunk-level preference for a heading that *is* a prescription ("Prescription", "Prescription
/// summary") over one that merely mentions the word ("Two goal kinds, one prescription", which
/// introduces the bands rather than stating them).
const PRESCRIPTION_HEADING_BONUS: i32 = 10;
const PRESCRIPTION_MENTION_BONUS: i32 = 7;
/// A chunk of evidence caveats. Worth reading, worth curating, worth spending prompt budget on last.
const CAVEAT_PENALTY: i32 = 6;

/// Marks a chunk as carrying prescribed bands. The corpus writes these headings as "Prescription",
/// "Prescription summary", "Muscular endurance prescription" — hence a substring test.
const PRESCRIPTION_HEADING: &str = "prescription";
const CAVEAT_HEADING: &str = "where the evidence is thinner";

/// Retrieval over the curated corpus. Built once and held for the process's life; see the module
/// docs for what [R5.1] swaps out and what it must keep.
#[derive(Debug, Clone)]
pub struct ScienceIndex {
    docs: &'static [ScienceDoc],
}

impl ScienceIndex {
    /// Build the index. Cheap today (the corpus is already parsed and compiled in) and cheap after
    /// K-50 too — tens of documents, milliseconds to index in memory. Fallible so that a tantivy
    /// build failure has somewhere to go without a signature change.
    pub fn build() -> Result<Self> {
        Ok(Self { docs: CORPUS.as_slice() })
    }

    /// The best `k` chunks for `query`, best first.
    ///
    /// Pinned documents ([`ScienceQuery::pinned_docs`]) contribute their best chunk first and are
    /// never dropped — if a caller pins more documents than `k`, it gets all of them, because `k` is
    /// a budget for *ranked* results and a rail is not a ranked result. Ranked chunks then fill the
    /// remainder, at most one per document, so four slots buy four perspectives rather than four
    /// paragraphs of the same paper.
    pub fn search(&self, query: &ScienceQuery, k: usize) -> Vec<ScienceChunk> {
        // Two goal kinds can name one document (`bodyweight` and `body_composition` share a
        // prescription), so duplicates are dropped here rather than pushed onto every caller.
        let pinned = &query.pinned_docs;
        let mut chosen: Vec<ScienceChunk> = pinned
            .iter()
            .enumerate()
            .filter(|(idx, id)| !pinned[..*idx].contains(id))
            .filter_map(|(_, id)| self.best_chunk(id))
            .collect();

        let mut ranked: Vec<(i32, &ScienceDoc, &ScienceChunk)> = self
            .docs
            .iter()
            .map(|doc| (doc_score(doc, query), doc))
            .filter(|(score, _)| *score > 0)
            .filter(|(_, doc)| !query.pinned_docs.contains(&doc.id))
            .filter_map(|(score, doc)| best_chunk_of(doc).map(|chunk| (score + prescribes(doc), doc, chunk)))
            .collect();
        // Highest score first; ties fall back to corpus order, which is filename order — stable
        // across runs, which matters because a prompt that changes shape run to run cannot be tested.
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));

        chosen.extend(ranked.into_iter().take(k.saturating_sub(chosen.len())).map(|(_, _, chunk)| chunk.clone()));
        chosen
    }

    /// The highest-scoring chunk of a document named by id, if the id exists.
    fn best_chunk(&self, doc_id: &str) -> Option<ScienceChunk> {
        let doc = self.docs.iter().find(|doc| doc.id == doc_id)?;
        best_chunk_of(doc).cloned()
    }

}

/// How well a document's tags answer the query. Document-level because that is where tags live —
/// the flatness this produces within a document is the placeholder's central weakness.
fn doc_score(doc: &ScienceDoc, query: &ScienceQuery) -> i32 {
    let injury = doc.injury_body_parts().filter(|part| query.injuries.contains(part)).count() as i32 * INJURY_WEIGHT;
    let goals: i32 = query.goal_tags().enumerate().filter(|(_, tag)| doc.has_tag(tag)).map(|(rank, _)| goal_weight(rank)).sum();
    let focus = query.focus_tags().filter(|tag| doc.has_tag(tag)).count() as i32 * FOCUS_WEIGHT;
    injury + goals + focus
}

/// The weight of the goal kind at `rank` (0 = highest priority). Decreasing, floored: a third goal
/// still pulls its document in, but never at the expense of the goal the user ranked first.
fn goal_weight(rank: usize) -> i32 {
    (TOP_GOAL_WEIGHT - RANK_STEP * rank as i32).max(MIN_GOAL_WEIGHT)
}

/// Whether a document prescribes at all — the document-level half of the prescription signal.
fn prescribes(doc: &ScienceDoc) -> i32 {
    match doc.chunks.iter().any(|chunk| chunk.heading.to_lowercase().contains(PRESCRIPTION_HEADING)) {
        true => PRESCRIPTION_BONUS,
        false => 0,
    }
}

/// The structural preference between chunks of one document, standing in for the relevance score a
/// real index computes over chunk text: the chunk that states the bands first, evidence caveats last.
fn chunk_bonus(chunk: &ScienceChunk) -> i32 {
    let heading = chunk.heading.to_lowercase();
    match heading {
        h if h.starts_with(PRESCRIPTION_HEADING) => PRESCRIPTION_HEADING_BONUS,
        h if h.contains(PRESCRIPTION_HEADING) => PRESCRIPTION_MENTION_BONUS,
        h if h.contains(CAVEAT_HEADING) => -CAVEAT_PENALTY,
        _ => 0,
    }
}

/// A document's most useful chunk. Ties keep the earliest, so a document with no prescription
/// heading reads from the top.
fn best_chunk_of(doc: &ScienceDoc) -> Option<&ScienceChunk> {
    doc.chunks
        .iter()
        .map(|chunk| (chunk_bonus(chunk), chunk))
        .reduce(|best, next| if next.0 > best.0 { next } else { best })
        .map(|(_, chunk)| chunk)
}

#[cfg(test)]
mod tests;

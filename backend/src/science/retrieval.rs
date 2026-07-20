//! Selecting the handful of corpus chunks a prompt can afford to carry.
//!
//! # The seam
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
//! # What is behind it ([R5.1])
//!
//! An in-memory tantivy index, built at startup from [`crate::science::CORPUS`], one indexed document
//! per `##`-heading chunk. The corpus is tens of files, so the build costs milliseconds and buys no
//! on-disk staleness to manage.
//!
//! Scoring answers two different questions, and conflating them is how retrieval goes wrong:
//!
//! - **Which documents?** Decided by [`doc_score`] from the caller's own weights — an active injury
//!   outranks everything, goal kinds count by priority, a focus tag counts as much as the top goal.
//!   These are *contracts*, not estimates: [C3.1] says the user's first-priority goal governs the
//!   session, and BM25 cannot know that. Worse, it actively contradicts it — a term's score rises
//!   with its rarity, so `endurance` (four tagged documents) outscores `strength` (seven) and the
//!   priority ordering inverts. Rarity in a hand-curated corpus of fourteen files is an accident of
//!   what has been written so far, not evidence about a user's goals.
//! - **Which chunk of a document, and which of two documents the tags cannot separate?** Decided by
//!   BM25 over chunk text, which is what this ticket buys. Tags live on the document, so tag-only
//!   scoring made every chunk of a matching document score alike and left the choice between them to
//!   structure alone. Now the query's words meet the chunk's words, so "let's go lighter today"
//!   reaches the deload section of a document rather than its first paragraph.
//!
//! The prose score is normalised against the best-matching chunk in the corpus and folded into the
//! document score with a weight below [`RANK_STEP`] ([`PROSE_WEIGHT`]), which is what keeps it a
//! tie-break between comparable documents rather than something that can reorder priorities. Where
//! the caller names no tags at all — free guidance and nothing else — every tag weight is zero and
//! BM25 decides alone, which is a query the tag-only placeholder could not answer at all.
//!
//! Structure survives as [`chunk_factor`], a multiplier on a chunk's BM25 score: a stated
//! prescription up, evidence caveats down. The corpus's own headings know something word frequencies
//! cannot express — which section carries the numbers a session has to land inside — and multiplying
//! rather than adding keeps that judgement scale-free.
//!
//! # The part of the contract that is not a ranking detail
//!
//! [`ScienceQuery::pinned_docs`] names documents whose best chunk is emitted whatever the scores say.
//! Rails — the competing-goal resolution when goals genuinely compete ([C5.2]), the contraindications
//! for an active injury ([C5.4]) — cannot be left to a relevance score that might rank them fifth.
//! BM25 can bury a rail exactly as easily as tag matching could, so the pin survives the swap: a
//! pinned document skips document ranking entirely and still gets its best chunk chosen by relevance.

use std::cmp::Ordering;

use anyhow::{Context, Result};
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, Score, Searcher, TantivyDocument, Term,
    collector::TopDocs,
    doc,
    query::{BooleanQuery, BoostQuery, Occur, Query, TermQuery},
    schema::{Field, IndexRecordOption, STORED, STRING, Schema, TEXT, Value},
};

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

    /// Focus tags, plus the words of the free guidance that are corpus vocabulary.
    ///
    /// The vocabulary filter is no longer the whole of what guidance contributes — the full text is
    /// searched as prose as well ([`ScienceIndex::prose_query`]). It survives the swap because a
    /// guidance word that *is* a tag is a precise claim about the document, where the same word in
    /// prose is only a hint about a chunk, and the two deserve different weights.
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
const INJURY_WEIGHT: f32 = 30.0;
/// Weight of the highest-priority goal kind's tag. Each further rank drops by [`RANK_STEP`] so a
/// second goal genuinely counts for less rather than averaging with the first.
const TOP_GOAL_WEIGHT: f32 = 8.0;
const RANK_STEP: f32 = 2.0;
/// Floor for the weight of a low-ranked goal kind — still relevant, never decisive.
const MIN_GOAL_WEIGHT: f32 = 4.0;
/// A block/slot focus or guidance word that lands on corpus vocabulary. As heavy as the top goal:
/// "this is a deload week" says as much about today's prescription as the goal it serves does.
const FOCUS_WEIGHT: f32 = 8.0;
/// Awarded to a document that prescribes at all — one of its chunks names a prescription. Heavier
/// than a single tag match on purpose: tags cannot tell a document *about* strength from one that
/// merely mentions it, and of the two, the prescribing document is the one worth the prompt's
/// budget.
///
/// Document-level and uniform: *whether* a document prescribes decides its rank, *which* of its
/// chunks prescribes best decides only what is quoted from it ([`chunk_factor`]). Letting the second
/// question leak into the first made a document's rank depend on how its author phrased a heading.
const PRESCRIPTION_BONUS: f32 = 10.0;
/// How much the best possible prose match adds to a document's score. Below [`RANK_STEP`] by
/// construction: BM25 must be able to separate two documents the tags rank alike, and must never be
/// able to reorder two goal kinds the user ranked. See the module docs.
const PROSE_WEIGHT: f32 = 1.5;

/// Relative weight of the free guidance text when matching chunk prose. Light against the goal and
/// injury weights: the user's phrasing is evidence about which chunk answers them, not a claim about
/// which document is relevant.
const GUIDANCE_WEIGHT: f32 = 2.0;

/// Chunk-level preference for a heading that *is* a prescription ("Prescription", "Prescription
/// summary") over one that merely mentions the word ("Two goal kinds, one prescription", which
/// introduces the bands rather than stating them). Multiplied into the BM25 score, so it decides
/// between chunks of comparable relevance without overriding a genuinely better match.
const PRESCRIPTION_HEADING_FACTOR: f32 = 1.5;
const PRESCRIPTION_MENTION_FACTOR: f32 = 1.2;
/// A chunk of evidence caveats. Worth reading, worth curating, worth spending prompt budget on last.
const CAVEAT_FACTOR: f32 = 0.5;

/// Marks a chunk as carrying prescribed bands. The corpus writes these headings as "Prescription",
/// "Prescription summary", "Muscular endurance prescription" — hence a substring test.
const PRESCRIPTION_HEADING: &str = "prescription";
const CAVEAT_HEADING: &str = "where the evidence is thinner";

/// tantivy's per-thread indexing arena floor. The corpus needs a fraction of it; this is simply the
/// smallest value the writer accepts.
const INDEX_WRITER_HEAP: usize = 15_000_000;

/// The searchable and stored fields of one indexed chunk.
#[derive(Debug, Clone, Copy)]
struct Fields {
    /// The parent document's id, indexed raw so a pinned document can be searched within itself,
    /// and stored so a hit resolves back to the corpus.
    id: Field,
    /// The parent document's title and tags, repeated on every chunk of it — that repetition is
    /// what lets a chunk be found by what its document is about.
    title: Field,
    tags: Field,
    /// The chunk's own heading and prose. The heading is indexed as well as stored: "Deloads" is as
    /// much a statement of what a chunk answers as its paragraphs are.
    heading: Field,
    body: Field,
    /// The chunk's position in its document, stored so a hit resolves to the parsed
    /// [`ScienceChunk`] rather than to a second copy of its text.
    ord: Field,
}

/// Retrieval over the curated corpus. Built once and held for the process's life.
#[derive(Clone)]
pub struct ScienceIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
    docs: &'static [ScienceDoc],
    /// Every chunk in the corpus: the ceiling on how many hits a search can return, and so the
    /// limit that makes `TopDocs` exhaustive rather than a sample.
    chunk_count: usize,
}

impl std::fmt::Debug for ScienceIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScienceIndex").field("docs", &self.docs.len()).field("chunks", &self.chunk_count).finish()
    }
}

impl ScienceIndex {
    /// Build the index: one in-memory tantivy document per corpus chunk. Fallible because a tantivy
    /// write can fail; in practice the corpus is compiled in and already parsed, so this succeeds or
    /// the process has nothing useful to say.
    pub fn build() -> Result<Self> {
        let (schema, fields) = schema();
        let index = Index::create_in_ram(schema);
        let mut writer: IndexWriter = index.writer_with_num_threads(1, INDEX_WRITER_HEAP).context("creating the index writer")?;
        CORPUS.iter().try_for_each(|document| index_document(&writer, &fields, document))?;
        writer.commit().context("committing the science index")?;
        let reader = index.reader().context("opening the science index reader")?;
        Ok(Self { index, reader, fields, docs: CORPUS.as_slice(), chunk_count: super::all_chunks().count() })
    }

    /// The best `k` chunks for `query`, best first.
    ///
    /// Pinned documents ([`ScienceQuery::pinned_docs`]) contribute their best chunk first and are
    /// never dropped — if a caller pins more documents than `k`, it gets all of them, because `k` is
    /// a budget for *ranked* results and a rail is not a ranked result. Ranked chunks then fill the
    /// remainder, at most one per document, so four slots buy four perspectives rather than four
    /// paragraphs of the same paper.
    pub fn search(&self, query: &ScienceQuery, k: usize) -> Vec<ScienceChunk> {
        let hits = self.rank(query);
        let pinned = pinned_hits(&hits, query);
        let ranked = ranked_hits(&hits, query, k.saturating_sub(pinned.len()));
        pinned.into_iter().chain(ranked).map(|hit| hit.chunk.clone()).collect()
    }

    /// Every corpus document's standing for `query`: which chunk to quote from it, and how well it
    /// answers. Ordering and budgeting are left to the callers of this — a pinned document is
    /// scored exactly like any other and simply does not have to earn its place.
    fn rank(&self, query: &ScienceQuery) -> Vec<DocHit> {
        let aboutness = self.scores(self.aboutness_clauses(query));
        let situation = self.scores(self.situation_clauses(query));
        // Normalising against the corpus's best match is what makes [`PROSE_WEIGHT`] mean "what the
        // best possible prose match is worth", independent of the absolute scale BM25 happens to
        // produce for this query.
        let best = aboutness.iter().map(|hit| hit.score).fold(0.0, f32::max);
        self.docs
            .iter()
            .enumerate()
            .map(|(idx, document)| {
                let relevance = aboutness.iter().filter(|h| h.document == idx).map(|h| h.score).fold(0.0, f32::max);
                let prose = if best > 0.0 { PROSE_WEIGHT * relevance / best } else { 0.0 };
                DocHit { chunk: best_chunk(idx, document, &situation), score: doc_score(document, query) + prose }
            })
            .collect()
    }

    /// BM25 for every chunk `clauses` reaches, unnormalised and ungrouped.
    fn scores(&self, clauses: Vec<Clause>) -> Vec<ProseHit> {
        if clauses.is_empty() {
            return Vec::new();
        }
        let searcher = self.reader.searcher();
        let assembled = BooleanQuery::new(clauses);
        let Ok(hits) = searcher.search(&assembled, &TopDocs::with_limit(self.chunk_count)) else {
            return Vec::new();
        };
        hits.iter().filter_map(|(score, addr)| self.resolve(&searcher, *addr, *score)).collect()
    }

    /// Where an index hit sits in the corpus. Ids and ordinals are stored rather than text, so what
    /// a caller finally receives is the corpus's own chunk and cannot drift from it.
    fn resolve(&self, searcher: &Searcher, addr: DocAddress, score: Score) -> Option<ProseHit> {
        let stored: TantivyDocument = searcher.doc(addr).ok()?;
        let id = stored.get_first(self.fields.id)?.as_str()?;
        let ord = stored.get_first(self.fields.ord)?.as_u64()? as usize;
        let document = self.docs.iter().position(|d| d.id == id)?;
        Some(ProseHit { document, ord, score })
    }

    /// Everything the caller's words say — what the session is *about*. Scores documents, not
    /// chunks: how much a document dwells on strength is evidence about the document.
    ///
    /// Note what is *not* here: the tags as a boolean filter. A document's tags are scored by
    /// [`doc_score`], not by BM25 — see the module docs. They are still searched as text, so a
    /// guidance word that happens to be a tag reaches the document carrying it.
    fn aboutness_clauses(&self, query: &ScienceQuery) -> Vec<Clause> {
        let injuries = query.injuries.iter().filter_map(|part| self.prose_query(part, INJURY_WEIGHT));
        let goals = query.goal_tags().enumerate().filter_map(|(rank, tag)| self.prose_query(tag, goal_weight(rank)));
        let situation = self.situation_clauses(query).into_iter().map(|(_, q)| q);
        injuries.chain(goals).chain(situation).map(|q| (Occur::Should, q)).collect()
    }

    /// Only what is particular to *this* session: the block or slot focus, and the user's own words.
    /// These choose the chunk within a document.
    ///
    /// The goal kinds and injuries deliberately sit this out, and the corpus shows why. A goal's
    /// name is densest in the chunks that *discuss* the goal — `goal-habit` says "habit" throughout
    /// its opening and its interactions section, and barely at all under "Prescription", which
    /// spends its words on frequencies and cues instead. Scoring chunks by the goal name therefore
    /// ranks the essay above the numbers, which is precisely backwards: term frequency of a
    /// document-level topic is anti-correlated with the chunk worth quoting. What genuinely picks a
    /// section out is the situational language — "deload", "something lighter today", "my shoulder"
    /// — and where the caller supplies none, [`chunk_factor`] decides on the corpus's own structure.
    fn situation_clauses(&self, query: &ScienceQuery) -> Vec<Clause> {
        let focus = query.focus_tags().filter_map(|tag| self.prose_query(&tag, FOCUS_WEIGHT)).collect::<Vec<_>>();
        let guidance = self.prose_query(&query.guidance, GUIDANCE_WEIGHT);
        focus.into_iter().chain(guidance).map(|q| (Occur::Should, q)).collect()
    }

    /// Free text over everything a chunk reads as: its document's title and tags, its own heading,
    /// its prose. Any term may match — BM25 rewards the chunk that matches most of them, most often.
    /// This is the clause that makes retrieval sensitive to what a chunk actually says.
    fn prose_query(&self, text: &str, weight: f32) -> Option<Box<dyn Query>> {
        let fields = [self.fields.title, self.fields.tags, self.fields.heading, self.fields.body];
        let clauses: Vec<Clause> = fields
            .iter()
            .flat_map(|&field| {
                self.tokens(field, text)
                    .into_iter()
                    .map(move |token| Term::from_field_text(field, &token))
                    .map(|term| (Occur::Should, Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)) as Box<dyn Query>))
            })
            .collect();
        (!clauses.is_empty()).then(|| boosted(Box::new(BooleanQuery::new(clauses)), weight))
    }

    /// `text` as the field's own tokeniser sees it, so a query term is spelled the way the indexed
    /// term is. Done by hand rather than through `QueryParser` to keep free user text out of query
    /// syntax, where a stray `:` or `"` would be an error or, worse, an operator.
    fn tokens(&self, field: Field, text: &str) -> Vec<String> {
        let Ok(mut analyzer) = self.index.tokenizer_for_field(field) else { return Vec::new() };
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        stream.process(&mut |token| tokens.push(token.text.clone()));
        tokens
    }
}

/// One weighted clause of the assembled query.
type Clause = (Occur, Box<dyn Query>);

/// One chunk's BM25 score, located by its document's place in the corpus and its own place in that
/// document.
struct ProseHit {
    document: usize,
    ord: usize,
    score: f32,
}

/// A document's standing for one query: the chunk worth quoting from it, and what the whole document
/// scored.
struct DocHit {
    chunk: &'static ScienceChunk,
    score: f32,
}

/// The best chunk of each pinned document, in the order the caller pinned them.
///
/// Two goal kinds can name one document (`bodyweight` and `body_composition` share a prescription),
/// so duplicates are dropped here rather than pushed onto every caller. An id naming no document is
/// skipped: a caller's stale pin should cost it that rail, not the whole retrieval.
fn pinned_hits<'a>(hits: &'a [DocHit], query: &ScienceQuery) -> Vec<&'a DocHit> {
    let pinned = &query.pinned_docs;
    pinned
        .iter()
        .enumerate()
        .filter(|(idx, id)| !pinned[..*idx].contains(id))
        .filter_map(|(_, id)| hits.iter().find(|hit| hit.chunk.doc_id == *id))
        .collect()
}

/// The best chunks of the top `k` unpinned documents, best first.
fn ranked_hits<'a>(hits: &'a [DocHit], query: &ScienceQuery, k: usize) -> Vec<&'a DocHit> {
    let mut ranked: Vec<&DocHit> = hits
        .iter()
        // A document nothing in the query touches is not a weak answer, it is not an answer. An
        // empty query therefore retrieves nothing: a section of arbitrary science is worse for the
        // prompt than no section at all.
        .filter(|hit| hit.score > 0.0)
        .filter(|hit| !query.pinned_docs.contains(&hit.chunk.doc_id))
        .collect();
    // Highest score first; ties fall back to document id, which is filename order — stable across
    // runs, which matters because a prompt that changes shape run to run cannot be tested.
    ranked.sort_by(|a, b| score_order(b.score, a.score).then_with(|| a.chunk.doc_id.cmp(&b.chunk.doc_id)));
    ranked.into_iter().take(k).collect()
}

/// The chunk of `document` worth quoting.
///
/// Situational BM25 scaled by [`chunk_factor`] decides. When nothing situational reached this
/// document's chunks — including a query with no situational words at all, which is the common case
/// of a plain goal — every score is zero and the factor decides alone. That is what makes an
/// unqueried document yield its prescription rather than its opening paragraph. Ties keep the
/// earliest chunk, so a document with nothing to prefer reads from the top.
fn best_chunk(idx: usize, document: &'static ScienceDoc, situation: &[ProseHit]) -> &'static ScienceChunk {
    let score_of = |ord: usize| situation.iter().find(|h| h.document == idx && h.ord == ord).map_or(0.0, |h| h.score);
    document
        .chunks
        .iter()
        .enumerate()
        .map(|(ord, chunk)| {
            let factor = chunk_factor(chunk);
            (score_of(ord) * factor, factor, chunk)
        })
        .reduce(|best, next| match score_order(next.0, best.0).then_with(|| score_order(next.1, best.1)) {
            Ordering::Greater => next,
            _ => best,
        })
        .map(|(_, _, chunk)| chunk)
        .expect("a parsed science document always has at least one chunk")
}

/// How well a document's tags answer the query — the caller's own weights, not an estimate. See the
/// module docs for why this is deliberately not BM25.
fn doc_score(document: &ScienceDoc, query: &ScienceQuery) -> f32 {
    let injury = document.injury_body_parts().filter(|part| query.injuries.contains(part)).count() as f32 * INJURY_WEIGHT;
    let goals: f32 = query.goal_tags().enumerate().filter(|(_, tag)| document.has_tag(tag)).map(|(rank, _)| goal_weight(rank)).sum();
    let focus = query.focus_tags().filter(|tag| document.has_tag(tag)).count() as f32 * FOCUS_WEIGHT;
    let relevant = injury + goals + focus;
    match relevant > 0.0 {
        true => relevant + prescribes(document),
        // A document nothing was asked about earns no prescription bonus — the bonus says "of the
        // relevant documents, prefer the one that prescribes", not "prefer prescriptions always".
        false => 0.0,
    }
}

/// Whether a document prescribes at all — the document-level half of the prescription signal.
fn prescribes(document: &ScienceDoc) -> f32 {
    match document.chunks.iter().any(|chunk| chunk.heading.to_lowercase().contains(PRESCRIPTION_HEADING)) {
        true => PRESCRIPTION_BONUS,
        false => 0.0,
    }
}

fn boosted(query: Box<dyn Query>, weight: f32) -> Box<dyn Query> {
    Box::new(BoostQuery::new(query, weight))
}

/// Ordering for BM25 scores, which are floats and never NaN in practice.
fn score_order(a: f32, b: f32) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// The index schema: what is searched, and what is kept to resolve a hit back to the corpus.
fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let fields = Fields {
        id: builder.add_text_field("id", STRING | STORED),
        title: builder.add_text_field("title", TEXT),
        tags: builder.add_text_field("tags", TEXT),
        heading: builder.add_text_field("heading", TEXT | STORED),
        body: builder.add_text_field("body", TEXT),
        ord: builder.add_u64_field("ord", STORED),
    };
    (builder.build(), fields)
}

/// Index one corpus document as one tantivy document per chunk, each carrying its document's title
/// and tags so a chunk is findable by what its document is about as well as by what it says.
fn index_document(writer: &IndexWriter, fields: &Fields, document: &ScienceDoc) -> Result<()> {
    let tags = document.tags.join(" ");
    document.chunks.iter().enumerate().try_for_each(|(ord, chunk)| {
        writer
            .add_document(doc!(
                fields.id => document.id.as_str(),
                fields.title => document.title.as_str(),
                fields.tags => tags.as_str(),
                fields.heading => chunk.heading.as_str(),
                fields.body => chunk.text.as_str(),
                fields.ord => ord as u64,
            ))
            .with_context(|| format!("indexing chunk `{}` of `{}`", chunk.heading, document.id))
            .map(|_| ())
    })
}

/// The weight of the goal kind at `rank` (0 = highest priority). Decreasing, floored: a third goal
/// still pulls its document in, but never at the expense of the goal the user ranked first.
fn goal_weight(rank: usize) -> f32 {
    (TOP_GOAL_WEIGHT - RANK_STEP * rank as f32).max(MIN_GOAL_WEIGHT)
}

/// What the corpus's own structure says about a chunk, as a multiplier on its relevance score: the
/// chunk that states the bands first, evidence caveats last. See the module docs for why BM25 does
/// not decide this by itself.
fn chunk_factor(chunk: &ScienceChunk) -> f32 {
    let heading = chunk.heading.to_lowercase();
    match heading {
        h if h.starts_with(PRESCRIPTION_HEADING) => PRESCRIPTION_HEADING_FACTOR,
        h if h.contains(PRESCRIPTION_HEADING) => PRESCRIPTION_MENTION_FACTOR,
        h if h.contains(CAVEAT_HEADING) => CAVEAT_FACTOR,
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests;

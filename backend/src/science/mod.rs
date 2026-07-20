//! The curated training-science corpus.
//!
//! `backend/science/*.md` holds hand-written, human-reviewed exercise-science notes, embedded into the
//! binary with `include_dir!` exactly like `backend/migrations`. The whole argument for curating this rather
//! than retrieving it live is that a person who can judge it reads it once; a change to the science is a
//! diff with the reasoning in the commit message.
//!
//! This module owns the corpus and its parsing only. Building a search index over the chunks, and injecting
//! retrieved chunks into a prompt, both belong to its consumers.
//!
//! # Document format
//!
//! ```text
//! ---
//! id: goal-strength                       # must equal the filename stem; unique across the corpus
//! title: Training for maximal strength
//! tags: [strength, chest, volume]         # see the tag vocabulary below
//! sources:                                # human citations, at least one
//!   - "ACSM Position Stand: ... Med Sci Sports Exerc. 2009;41(3):687-708."
//! ---
//!
//! ## First heading
//!
//! Prose. Every `##` heading starts a new independently retrievable chunk, cited as `[S:goal-strength]`.
//! `###` and deeper headings stay inside their parent chunk, and `##` inside a fenced block is not a
//! heading. All prose must live under a `##` heading — a document with content before the first one is
//! rejected, so nothing can be silently dropped.
//! ```
//!
//! # Tag vocabulary
//!
//! Tags are validated at parse time against a closed vocabulary, in four categories:
//!
//! - **Goal kinds** — exactly the [`GoalKind`] variants. [`goal_kind_tag`] is an exhaustive match, so a new
//!   variant fails to compile here until the corpus accounts for it.
//! - **Muscle groups** — the seven `muscle_group` rows the exercise catalogue seeds, lowercased.
//! - **Injuries** — `injury:<body_part>` over [`INJURY_BODY_PARTS`].
//! - **Topics** — [`TOPIC_TAGS`], for emphases that are not goal kinds (`hypertrophy`, `deload`, …) so that
//!   a programme block's focus text has something to match against.

use std::sync::LazyLock;

use anyhow::{Context, Result, bail, ensure};
use include_dir::{Dir, include_dir};
use serde::Deserialize;

use crate::db::GoalKind;

static SCIENCE_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/science");

/// Prefix marking a tag as an injury body part, as in `injury:lower_back`.
pub const INJURY_TAG_PREFIX: &str = "injury:";

/// Body parts an `injury:<body_part>` tag may name. Not every part has a document yet; this is the
/// vocabulary a document may draw on, and the list injury-aware retrieval matches a user's
/// `health_entries.body_part` against.
pub const INJURY_BODY_PARTS: [&str; 9] =
    ["shoulder", "lower_back", "upper_back", "neck", "elbow", "wrist", "hip", "knee", "ankle"];

/// The exercise catalogue's seven `muscle_group` rows, lowercased.
pub const MUSCLE_GROUP_TAGS: [&str; 7] = ["chest", "back", "shoulders", "arms", "legs", "core", "cardio"];

/// Training emphases that are not goal kinds, so a slot or block focus has something to match on.
pub const TOPIC_TAGS: [&str; 16] = [
    "hypertrophy",
    "conditioning",
    "priority",
    "progression",
    "deload",
    "autoregulation",
    "volume",
    "frequency",
    "rest",
    "recovery",
    "warmup",
    "technique",
    "nutrition",
    "adherence",
    "safety",
    "novice",
];

/// Every [`GoalKind`] usable as a tag.
///
/// Deliberately routed through [`goal_kind_tag`] rather than written out: the exhaustive match there is
/// what makes a new `GoalKind` variant a compile error in this module.
pub const GOAL_KIND_TAGS: [&str; 5] = [
    goal_kind_tag(GoalKind::Strength),
    goal_kind_tag(GoalKind::Endurance),
    goal_kind_tag(GoalKind::Bodyweight),
    goal_kind_tag(GoalKind::BodyComposition),
    goal_kind_tag(GoalKind::Habit),
];

/// The tag spelling of a goal kind. Exhaustive by construction — adding a `GoalKind` variant fails to
/// compile here until the corpus vocabulary accounts for it.
pub const fn goal_kind_tag(kind: GoalKind) -> &'static str {
    match kind {
        GoalKind::Strength => "strength",
        GoalKind::Endurance => "endurance",
        GoalKind::Bodyweight => "bodyweight",
        GoalKind::BodyComposition => "body_composition",
        GoalKind::Habit => "habit",
    }
}

/// One curated document: its front matter plus the chunks its `##` headings carve it into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScienceDoc {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub sources: Vec<String>,
    pub chunks: Vec<ScienceChunk>,
}

impl ScienceDoc {
    /// Whether this document carries `tag`, case-sensitively — the vocabulary is all lowercase.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// The injury body parts this document is tagged for.
    pub fn injury_body_parts(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().filter_map(|tag| injury_body_part(tag))
    }
}

/// One `##`-delimited section, the unit of retrieval and of citation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScienceChunk {
    pub doc_id: String,
    pub heading: String,
    pub text: String,
    /// Rendered `[S:doc-id]`, the form a prompt asks the model to cite.
    pub citation: String,
}

/// The citation marker for a document id.
pub fn citation_for(doc_id: &str) -> String {
    format!("[S:{doc_id}]")
}

/// If `tag` names an injury, the body part it names.
pub fn injury_body_part(tag: &str) -> Option<&str> {
    tag.strip_prefix(INJURY_TAG_PREFIX).filter(|part| INJURY_BODY_PARTS.contains(part))
}

/// The parsed corpus, in filename order. Panics on a malformed document, as `MIGRATIONS` does — the corpus
/// is compiled in, so a failure here is a build-time mistake that should never reach a user.
pub static CORPUS: LazyLock<Vec<ScienceDoc>> = LazyLock::new(|| load_corpus().expect("invalid science corpus"));

/// The document with this id, if any.
pub fn doc(id: &str) -> Option<&'static ScienceDoc> {
    CORPUS.iter().find(|doc| doc.id == id)
}

/// Every chunk in the corpus, paired with the document it came from — the shape an index build wants,
/// since the searchable tags live on the document and the searchable text on the chunk.
pub fn all_chunks() -> impl Iterator<Item = (&'static ScienceDoc, &'static ScienceChunk)> {
    CORPUS.iter().flat_map(|doc| doc.chunks.iter().map(move |chunk| (doc, chunk)))
}

fn load_corpus() -> Result<Vec<ScienceDoc>> {
    let docs = SCIENCE_DIR
        .files()
        .filter(|file| file.path().extension().is_some_and(|ext| ext == "md"))
        .map(parse_file)
        .collect::<Result<Vec<_>>>()?;
    ensure!(!docs.is_empty(), "the science corpus is empty");
    ensure_unique_ids(&docs)?;
    Ok(docs)
}

fn ensure_unique_ids(docs: &[ScienceDoc]) -> Result<()> {
    let duplicate = docs.iter().enumerate().find_map(|(idx, doc)| docs[..idx].iter().any(|d| d.id == doc.id).then_some(&doc.id));
    match duplicate {
        Some(id) => bail!("duplicate science document id `{id}`"),
        None => Ok(()),
    }
}

fn parse_file(file: &include_dir::File<'_>) -> Result<ScienceDoc> {
    let name = file.path().display().to_string();
    let stem = file.path().file_stem().context("science document has no filename")?.to_string_lossy().into_owned();
    let raw = std::str::from_utf8(file.contents()).with_context(|| format!("{name}: not valid UTF-8"))?;
    let doc = parse_document(raw).with_context(|| format!("{name}: malformed science document"))?;
    ensure!(doc.id == stem, "{name}: front-matter id `{}` does not match the filename stem `{stem}`", doc.id);
    Ok(doc)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontMatter {
    id: String,
    title: String,
    tags: Vec<String>,
    sources: Vec<String>,
}

/// Parse one document's raw text: `---`-fenced YAML front matter, then `##`-chunked markdown.
fn parse_document(raw: &str) -> Result<ScienceDoc> {
    let (yaml, body) = split_front_matter(raw)?;
    let front: FrontMatter = serde_yaml_ng::from_str(yaml).context("front matter is not valid YAML")?;
    validate_front_matter(&front)?;
    let chunks = chunk_body(&front.id, body)?;
    Ok(ScienceDoc { id: front.id, title: front.title, tags: front.tags, sources: front.sources, chunks })
}

fn validate_front_matter(front: &FrontMatter) -> Result<()> {
    ensure!(!front.id.trim().is_empty(), "front matter `id` is empty");
    ensure!(!front.title.trim().is_empty(), "front matter `title` is empty");
    ensure!(!front.tags.is_empty(), "front matter `tags` is empty");
    ensure!(!front.sources.is_empty(), "front matter `sources` is empty — curated claims must be attributable");
    match front.tags.iter().find(|tag| !is_known_tag(tag)) {
        Some(tag) => bail!("tag `{tag}` is not in the corpus vocabulary"),
        None => Ok(()),
    }
}

fn is_known_tag(tag: &str) -> bool {
    GOAL_KIND_TAGS.contains(&tag) || MUSCLE_GROUP_TAGS.contains(&tag) || TOPIC_TAGS.contains(&tag) || injury_body_part(tag).is_some()
}

const FENCE: &str = "---";

/// Every line's byte offset paired with its content, terminator stripped.
fn line_spans(raw: &str) -> impl Iterator<Item = (usize, &str)> {
    raw.split_inclusive('\n').scan(0usize, |offset, line| {
        let at = *offset;
        *offset += line.len();
        Some((at, line.trim_end_matches(['\n', '\r'])))
    })
}

/// Split `---`-fenced YAML front matter from the markdown body that follows it.
fn split_front_matter(raw: &str) -> Result<(&str, &str)> {
    let spans: Vec<(usize, &str)> = line_spans(raw).collect();
    ensure!(spans.first().is_some_and(|(_, line)| line.trim() == FENCE), "document must open with a `---` front-matter fence");
    let (close_at, _) =
        *spans.iter().skip(1).find(|(_, line)| line.trim() == FENCE).context("front matter is not closed by a `---` line")?;
    // A closing fence was found beyond line 0, so a second line exists.
    let yaml_start = spans[1].0;
    let body_start = spans.iter().find(|(at, _)| *at > close_at).map_or(raw.len(), |(at, _)| *at);
    Ok((&raw[yaml_start..close_at], &raw[body_start..]))
}

/// Carve the body into one chunk per `##` heading.
fn chunk_body(doc_id: &str, body: &str) -> Result<Vec<ScienceChunk>> {
    let lines: Vec<&str> = body.lines().collect();
    let starts = heading_lines(&lines);
    let first = *starts.first().context("document has no `##` headings")?;
    ensure!(
        lines[..first].iter().all(|line| line.trim().is_empty()),
        "document has prose before its first `##` heading — every chunk must be reachable"
    );
    let ends = starts.iter().skip(1).copied().chain(std::iter::once(lines.len()));
    starts.iter().zip(ends).map(|(&start, end)| build_chunk(doc_id, &lines[start..end])).collect()
}

/// Line indices of `##` headings, ignoring anything inside a fenced code block.
fn heading_lines(lines: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .scan(false, |in_fence, line| {
            let is_fence = line.trim_start().starts_with("```");
            *in_fence ^= is_fence;
            Some(!*in_fence && !is_fence && line.starts_with("## "))
        })
        .enumerate()
        .filter_map(|(idx, is_heading)| is_heading.then_some(idx))
        .collect()
}

fn build_chunk(doc_id: &str, lines: &[&str]) -> Result<ScienceChunk> {
    let heading = lines[0].trim_start_matches('#').trim().to_string();
    ensure!(!heading.is_empty(), "a `##` heading is empty");
    let text = lines[1..].join("\n").trim().to_string();
    ensure!(!text.is_empty(), "section `{heading}` has no content");
    Ok(ScienceChunk { doc_id: doc_id.to_string(), heading, text, citation: citation_for(doc_id) })
}

#[cfg(test)]
mod tests;

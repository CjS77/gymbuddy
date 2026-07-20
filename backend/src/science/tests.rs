use super::*;

// ---------------------------------------------------------------------------
// Parser unit tests — inline documents, so a failure points at the parser
// rather than at the corpus.
// ---------------------------------------------------------------------------

const MINIMAL: &str = r#"---
id: sample
title: A sample
tags: [strength]
sources:
  - "Some Book, 2nd ed."
---

## First

One.

## Second

Two.
"#;

#[test]
fn parses_front_matter_and_chunks() {
    let doc = parse_document(MINIMAL).expect("should parse");
    assert_eq!(doc.id, "sample");
    assert_eq!(doc.title, "A sample");
    assert_eq!(doc.tags, ["strength"]);
    assert_eq!(doc.sources, ["Some Book, 2nd ed."]);
    let headings: Vec<&str> = doc.chunks.iter().map(|c| c.heading.as_str()).collect();
    assert_eq!(headings, ["First", "Second"]);
    assert_eq!(doc.chunks[0].text, "One.");
    assert_eq!(doc.chunks[1].text, "Two.");
}

#[test]
fn every_chunk_carries_its_document_citation() {
    let doc = parse_document(MINIMAL).expect("should parse");
    assert!(doc.chunks.iter().all(|c| c.doc_id == "sample" && c.citation == "[S:sample]"));
}

#[test]
fn deeper_headings_stay_inside_their_chunk() {
    let raw = MINIMAL.replace("## Second", "### Second");
    let doc = parse_document(&raw).expect("should parse");
    assert_eq!(doc.chunks.len(), 1, "only `##` splits");
    assert!(doc.chunks[0].text.contains("### Second"));
}

#[test]
fn hashes_inside_a_code_fence_are_not_headings() {
    let raw = MINIMAL.replace("## Second\n\nTwo.", "```\n## Second\n```\n\nTwo.");
    let doc = parse_document(&raw).expect("should parse");
    assert_eq!(doc.chunks.len(), 1);
    assert!(doc.chunks[0].text.contains("## Second"));
}

#[test]
fn prose_before_the_first_heading_is_rejected() {
    let raw = MINIMAL.replace("\n## First", "\nStray prose nothing can retrieve.\n\n## First");
    let err = parse_document(&raw).expect_err("stray prose should be rejected");
    assert!(format!("{err}").contains("before its first `##` heading"), "unexpected error: {err}");
}

#[test]
fn missing_front_matter_is_rejected() {
    let err = parse_document("## First\n\nOne.\n").expect_err("should be rejected");
    assert!(format!("{err}").contains("front-matter fence"), "unexpected error: {err}");
}

#[test]
fn unclosed_front_matter_is_rejected() {
    let err = parse_document("---\nid: sample\n\n## First\n\nOne.\n").expect_err("should be rejected");
    assert!(format!("{err}").contains("not closed"), "unexpected error: {err}");
}

#[test]
fn unknown_tags_are_rejected() {
    let raw = MINIMAL.replace("tags: [strength]", "tags: [strength, functional-fitness]");
    let err = parse_document(&raw).expect_err("an off-vocabulary tag should be rejected");
    assert!(format!("{err}").contains("functional-fitness"), "unexpected error: {err}");
}

#[test]
fn unknown_injury_body_parts_are_rejected() {
    let raw = MINIMAL.replace("tags: [strength]", "tags: [injury:soul]");
    assert!(parse_document(&raw).is_err());
    let known = MINIMAL.replace("tags: [strength]", "tags: [injury:knee]");
    assert!(parse_document(&known).is_ok());
}

#[test]
fn documents_without_sources_are_rejected() {
    let raw = MINIMAL.replace("sources:\n  - \"Some Book, 2nd ed.\"", "sources: []");
    let err = parse_document(&raw).expect_err("an unattributed document should be rejected");
    assert!(format!("{err}").contains("sources"), "unexpected error: {err}");
}

#[test]
fn documents_without_headings_are_rejected() {
    let raw = "---\nid: s\ntitle: T\ntags: [strength]\nsources: [\"x\"]\n---\n";
    assert!(parse_document(raw).is_err());
}

#[test]
fn empty_sections_are_rejected() {
    let raw = MINIMAL.replace("## Second\n\nTwo.\n", "## Second\n");
    let err = parse_document(&raw).expect_err("an empty section should be rejected");
    assert!(format!("{err}").contains("no content"), "unexpected error: {err}");
}

#[test]
fn unknown_front_matter_keys_are_rejected() {
    let raw = MINIMAL.replace("title: A sample", "title: A sample\nauthor: nobody");
    assert!(parse_document(&raw).is_err(), "a typo'd key must not be silently ignored");
}

#[test]
fn windows_line_endings_parse() {
    let raw = MINIMAL.replace('\n', "\r\n");
    let doc = parse_document(&raw).expect("CRLF should parse");
    assert_eq!(doc.chunks.len(), 2);
    assert_eq!(doc.chunks[0].text, "One.");
}

// ---------------------------------------------------------------------------
// Vocabulary — the tags must be the application's own vocabulary, not a
// parallel one invented for the corpus.
// ---------------------------------------------------------------------------

#[test]
fn goal_kind_tags_are_exactly_the_goal_kind_variants() {
    let kinds = [GoalKind::Strength, GoalKind::Endurance, GoalKind::Bodyweight, GoalKind::BodyComposition, GoalKind::Habit];
    assert!(kinds.iter().all(|k| goal_kind_tag(*k) == k.as_str()), "tag spelling must match GoalKind::as_str");
    assert!(GOAL_KIND_TAGS.iter().all(|tag| GoalKind::from_str_loose(tag).as_str() == *tag), "tags must round-trip");
    assert_eq!(GOAL_KIND_TAGS.len(), kinds.len());
}

#[test]
fn muscle_group_tags_are_the_catalogue_groups() {
    // The seven `muscle_group` rows seeded by migration 02-exercises, lowercased.
    let seeded = ["Chest", "Back", "Shoulders", "Arms", "Legs", "Core", "Cardio"];
    let expected: Vec<String> = seeded.iter().map(|name| name.to_lowercase()).collect();
    assert_eq!(MUSCLE_GROUP_TAGS.to_vec(), expected);
}

#[test]
fn injury_tags_resolve_to_body_parts() {
    assert_eq!(injury_body_part("injury:lower_back"), Some("lower_back"));
    assert_eq!(injury_body_part("injury:unmapped"), None);
    assert_eq!(injury_body_part("strength"), None);
}

// ---------------------------------------------------------------------------
// Corpus tests — these run against the real embedded documents.
// ---------------------------------------------------------------------------

#[test]
fn corpus_loads() {
    assert!(CORPUS.len() >= 10, "expected a substantive corpus, got {}", CORPUS.len());
    assert!(CORPUS.iter().all(|doc| !doc.chunks.is_empty()));
}

#[test]
fn corpus_ids_are_unique() {
    let mut ids: Vec<&str> = CORPUS.iter().map(|doc| doc.id.as_str()).collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "document ids must be unique — they are the citation key");
}

#[test]
fn every_goal_kind_is_covered() {
    let uncovered: Vec<&str> = GOAL_KIND_TAGS.iter().copied().filter(|tag| !CORPUS.iter().any(|doc| doc.has_tag(tag))).collect();
    assert!(uncovered.is_empty(), "goal kinds with no science document: {uncovered:?}");
}

#[test]
fn every_muscle_group_is_covered() {
    let uncovered: Vec<&str> = MUSCLE_GROUP_TAGS.iter().copied().filter(|tag| !CORPUS.iter().any(|doc| doc.has_tag(tag))).collect();
    assert!(uncovered.is_empty(), "muscle groups with no science document: {uncovered:?}");
}

#[test]
fn injury_guidance_exists_for_the_common_body_parts() {
    let covered: Vec<&str> = CORPUS.iter().flat_map(ScienceDoc::injury_body_parts).collect();
    assert!(["shoulder", "lower_back", "knee"].iter().all(|part| covered.contains(part)), "covered: {covered:?}");
}

#[test]
fn every_chunk_is_citable_and_substantive() {
    let thin: Vec<(&str, &str)> = all_chunks()
        .filter(|(_, chunk)| chunk.text.len() < 80)
        .map(|(doc, chunk)| (doc.id.as_str(), chunk.heading.as_str()))
        .collect();
    assert!(thin.is_empty(), "chunks too thin to be worth retrieving: {thin:?}");
    assert!(all_chunks().all(|(doc, chunk)| chunk.citation == citation_for(&doc.id)));
}

#[test]
fn lookup_by_id_works() {
    let id = CORPUS[0].id.clone();
    assert_eq!(doc(&id).map(|d| d.id.as_str()), Some(id.as_str()));
    assert!(doc("no-such-document").is_none());
}

#[test]
fn headings_are_unique_within_a_document() {
    let clashing: Vec<&str> = CORPUS
        .iter()
        .filter(|doc| {
            let mut headings: Vec<&str> = doc.chunks.iter().map(|c| c.heading.as_str()).collect();
            headings.sort_unstable();
            let total = headings.len();
            headings.dedup();
            headings.len() != total
        })
        .map(|doc| doc.id.as_str())
        .collect();
    assert!(clashing.is_empty(), "documents with duplicate headings: {clashing:?}");
}

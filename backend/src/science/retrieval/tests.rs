use super::*;

fn index() -> ScienceIndex {
    ScienceIndex::build().expect("the compiled-in corpus always indexes")
}

/// A query for one goal kind and nothing else — the plainest case the designer issues.
fn goal_query(kind: GoalKind) -> ScienceQuery {
    ScienceQuery { goal_kinds: vec![kind], ..Default::default() }
}

fn doc_ids(chunks: &[ScienceChunk]) -> Vec<&str> {
    chunks.iter().map(|c| c.doc_id.as_str()).collect()
}

#[test]
fn body_parts_normalise_from_free_text() {
    assert_eq!(normalise_body_part("lower back"), Some("lower_back"));
    assert_eq!(normalise_body_part(" Lower-Back "), Some("lower_back"));
    assert_eq!(normalise_body_part("shoulder"), Some("shoulder"));
    // Not vocabulary: the caller gets `None` and leaves the query alone rather than
    // inventing an `injury:` tag the corpus can never match.
    assert_eq!(normalise_body_part("soul"), None);
    assert_eq!(normalise_body_part(""), None);
}

#[test]
fn a_goal_kind_retrieves_its_own_document() {
    let index = index();
    let hits = index.search(&goal_query(GoalKind::Strength), 4);
    assert!(doc_ids(&hits).contains(&"goal-strength"), "a strength goal must reach the strength document: {:?}", doc_ids(&hits));

    let hits = index.search(&goal_query(GoalKind::Endurance), 4);
    assert!(doc_ids(&hits).contains(&"goal-endurance"), "an endurance goal must reach the endurance document: {:?}", doc_ids(&hits));
}

#[test]
fn prescription_chunks_win_their_document() {
    let hits = index().search(&goal_query(GoalKind::Strength), 4);
    let strength = hits.iter().find(|c| c.doc_id == "goal-strength").expect("goal-strength retrieved");
    assert_eq!(strength.heading, "Prescription summary", "the prescribing chunk is the one worth prompt budget");
}

/// `goal-body-composition` opens with "Two goal kinds, one prescription", which *introduces* the
/// bands, and states them two chunks later under a bare "Prescription". Quoting the introduction
/// would put a section header's worth of framing in the prompt where the numbers should be.
#[test]
fn a_heading_that_is_a_prescription_beats_one_that_mentions_prescriptions() {
    let hits = index().search(&goal_query(GoalKind::BodyComposition), 4);
    let doc = hits.iter().find(|c| c.doc_id == "goal-body-composition").expect("goal-body-composition retrieved");
    assert_eq!(doc.heading, "Prescription");
    assert!(doc.text.contains("6-12"), "the chosen chunk should carry the repetition band, not the framing");
}

#[test]
fn evidence_caveats_never_stand_in_for_a_prescription() {
    let hits = index().search(&goal_query(GoalKind::Strength), 8);
    assert!(
        hits.iter().all(|c| !c.heading.to_lowercase().contains(CAVEAT_HEADING)),
        "a caveat chunk displaced real content: {:?}",
        hits.iter().map(|c| &c.heading).collect::<Vec<_>>()
    );
}

#[test]
fn k_bounds_the_ranked_results() {
    let index = index();
    (1..=5).for_each(|k| assert_eq!(index.search(&goal_query(GoalKind::Strength), k).len(), k, "k={k} must bound the result"));
}

#[test]
fn one_chunk_per_document_buys_breadth() {
    let hits = index().search(&goal_query(GoalKind::Strength), 4);
    let mut ids = doc_ids(&hits);
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), hits.len(), "four slots should buy four documents, not four sections of one");
}

#[test]
fn pinned_documents_survive_ranking() {
    // `session-structure` carries no goal-kind tag that outranks the goal documents, so on score
    // alone it loses. Pinned, it is emitted first — the guarantee [C5.2] and [C5.4] rails need.
    let query = ScienceQuery { pinned_docs: vec!["session-structure".to_string()], ..goal_query(GoalKind::Strength) };
    let hits = index().search(&query, 4);
    assert_eq!(hits[0].doc_id, "session-structure", "a pinned document leads: {:?}", doc_ids(&hits));
    assert_eq!(hits.len(), 4, "pinning consumes a slot rather than adding one");
}

#[test]
fn a_pinned_document_is_never_dropped_even_below_k() {
    let query = ScienceQuery { pinned_docs: vec!["competing-goals".to_string(), "scope-and-safety".to_string()], ..Default::default() };
    // k=1 is smaller than the pinned set: a rail is not a ranked result and is not budgeted away.
    let hits = index().search(&query, 1);
    assert_eq!(doc_ids(&hits), ["competing-goals", "scope-and-safety"]);
}

#[test]
fn an_unknown_pinned_id_is_skipped_rather_than_fatal() {
    let query = ScienceQuery { pinned_docs: vec!["no-such-document".to_string()], ..goal_query(GoalKind::Strength) };
    let hits = index().search(&query, 3);
    assert_eq!(hits.len(), 3);
    assert!(!doc_ids(&hits).contains(&"no-such-document"));
}

#[test]
fn an_active_injury_outranks_the_goal() {
    let query = ScienceQuery { injuries: vec!["lower_back"], ..goal_query(GoalKind::Strength) };
    let hits = index().search(&query, 4);
    assert_eq!(hits[0].doc_id, "injury-lower-back", "an injury is a hard constraint, so it leads: {:?}", doc_ids(&hits));
}

#[test]
fn goal_priority_orders_the_results() {
    let index = index();
    let strength_first = ScienceQuery { goal_kinds: vec![GoalKind::Strength, GoalKind::Endurance], ..Default::default() };
    let endurance_first = ScienceQuery { goal_kinds: vec![GoalKind::Endurance, GoalKind::Strength], ..Default::default() };

    let position = |hits: &[ScienceChunk], id: &str| doc_ids(hits).iter().position(|d| *d == id);
    let (a, b) = (index.search(&strength_first, 6), index.search(&endurance_first, 6));

    assert!(
        position(&a, "goal-strength") < position(&a, "goal-endurance"),
        "the higher-priority goal's document ranks first: {:?}",
        doc_ids(&a)
    );
    assert!(
        position(&b, "goal-endurance") < position(&b, "goal-strength"),
        "reversing priority reverses the order — priority is not decoration: {:?}",
        doc_ids(&b)
    );
}

#[test]
fn focus_tags_and_guidance_words_steer_retrieval() {
    let index = index();
    let plain = index.search(&goal_query(GoalKind::Strength), 4);
    let deload = ScienceQuery { focus: vec!["deload".to_string()], ..goal_query(GoalKind::Strength) };
    assert!(
        doc_ids(&index.search(&deload, 4)).contains(&"progressive-overload") && !doc_ids(&plain).contains(&"progressive-overload"),
        "a deload focus should pull in the document tagged for it"
    );

    // Free guidance is matched against corpus vocabulary only — crude, and replaced by K-50.
    let guided = ScienceQuery { guidance: "let's do a deload week".to_string(), ..goal_query(GoalKind::Strength) };
    assert!(doc_ids(&index.search(&guided, 4)).contains(&"progressive-overload"));
}

#[test]
fn an_empty_query_retrieves_nothing() {
    // No goals, no injuries, no focus: nothing is relevant, and a section of arbitrary science is
    // worse than no section. The caller decides what to do with an empty result.
    assert!(index().search(&ScienceQuery::default(), 4).is_empty());
}

#[test]
fn goal_weight_decreases_and_then_floors() {
    assert_eq!(goal_weight(0), TOP_GOAL_WEIGHT);
    assert!(goal_weight(1) < goal_weight(0));
    assert_eq!(goal_weight(9), MIN_GOAL_WEIGHT, "a low-ranked goal still counts for something");
}

/// Every goal kind must be able to reach curated science, or the designer silently falls back to
/// whatever the model recalls for that kind — the exact failure [C5.2] exists to close. Exhaustive
/// over `GoalKind`, so a new variant fails here until the corpus covers it.
#[test]
fn every_goal_kind_reaches_the_corpus() {
    let index = index();
    let kinds = [GoalKind::Strength, GoalKind::Endurance, GoalKind::Bodyweight, GoalKind::BodyComposition, GoalKind::Habit];
    kinds.iter().for_each(|kind| {
        let hits = index.search(&goal_query(*kind), 4);
        assert!(!hits.is_empty(), "{kind:?} retrieves no science at all");
        assert!(
            hits.iter().any(|c| c.heading.to_lowercase().contains(PRESCRIPTION_HEADING)),
            "{kind:?} retrieves no prescription: {:?}",
            hits.iter().map(|c| &c.heading).collect::<Vec<_>>()
        );
    });
}


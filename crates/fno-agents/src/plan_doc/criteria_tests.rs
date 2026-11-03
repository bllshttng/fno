use super::*;

fn codes(criteria: &[Criterion]) -> Vec<&str> {
    criteria.iter().map(|c| c.code.as_str()).collect()
}

fn kinds(criteria: &[Criterion]) -> Vec<&str> {
    criteria.iter().map(|c| c.kind.as_str()).collect()
}

#[test]
fn an_empty_section_compiles_to_nothing_and_unshaped_prose_is_refused_with_guidance() {
    assert!(compile_criteria("", "Acceptance Criteria")
        .unwrap()
        .is_empty());
    assert!(compile_criteria("   \n\n  \n", "Acceptance Criteria")
        .unwrap()
        .is_empty());

    let err = compile_criteria(
        "This is just prose sentences with no criterion shape.\nMore prose here.\n",
        "Acceptance Criteria",
    )
    .unwrap_err();
    assert!(err.0.contains("Given/When/Then"), "{err}");
    assert!(
        err.0.starts_with("Acceptance Criteria has content"),
        "{err}"
    );
}

#[test]
fn legacy_bold_codes_keep_tag_suffix_metadata_drop_bare_markers_and_accept_letter_suffixes() {
    let tagged = compile_criteria(
        "**AC1-HP (api):** Given valid input, the module returns the result.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&tagged), ["AC1-HP"]);
    assert_eq!(kinds(&tagged), ["HP"]);
    assert_eq!(
        tagged[0].text,
        "Given valid input, the module returns the result."
    );

    let bare = compile_criteria(
        "**AC1-HP:** Real behavior.\n**AC2-ERR:**\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&bare), ["AC1-HP"]);
    assert_eq!(bare[0].text, "Real behavior.");

    let lettered = compile_criteria(
        "**AC3-HP:** Base criterion.\n\n**AC3b-HP:** Sub-criterion.\n\n**AC7a:** A bare letter-suffixed criterion.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&lettered), ["AC3-HP", "AC3b-HP", "AC7a"]);
    assert_eq!(kinds(&lettered), ["HP", "HP", "GENERAL"]);
}

#[test]
fn unlabeled_items_get_document_order_codes_that_skip_explicit_numbers() {
    let numbered = compile_criteria(
        "1. First behavior.\n2. Second behavior.\n3. Third behavior.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&numbered), ["AC1", "AC2", "AC3"]);
    assert_eq!(numbered[2].text, "Third behavior.");
    assert_eq!(numbered[0].source_ref, "line 1");

    let mixed = compile_criteria(
        "**AC5-HP:** Explicit named criterion.\n\n- An unlabeled bulleted criterion.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&mixed), ["AC5-HP", "AC1"]);
    assert_eq!(kinds(&mixed), ["HP", "GENERAL"]);
}

#[test]
fn a_duplicate_explicit_code_is_refused_naming_both_lines() {
    let err = compile_criteria(
        "**AC1-HP:** First use.\n\n**AC1-HP:** Second use of the same code.\n",
        "Acceptance Criteria",
    )
    .unwrap_err();
    assert!(err.0.contains("AC1-HP"), "{err}");
    assert!(err.0.contains("first at line 1"), "{err}");
    assert!(err.0.contains("again at line 3"), "{err}");
}

#[test]
fn table_header_gwt_blocks_and_fenced_lines_follow_their_boundaries() {
    let table = compile_criteria(
        "| Given | When | Then |\n|---|---|---|\n| a | b | c |\n| d | e | f |\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(table.len(), 2);

    let gwt = compile_criteria(
        "First scenario\nGiven a\nWhen b\nThen c\n\nSecond scenario\nGiven d\nWhen e\nThen f\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(gwt.len(), 2);
    assert!(gwt[0].text.contains("First scenario"));
    assert!(gwt[0].text.contains("Then c"));
    assert!(gwt[1].text.contains("Second scenario"));

    let fenced = compile_criteria(
        "1. First behavior.\n   ```\n   - --verbose\n   2. also not a criterion\n   ```\n2. Second behavior.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&fenced), ["AC1", "AC2"]);
    assert_eq!(fenced[0].text, "First behavior.");
    assert_eq!(fenced[1].text, "Second behavior.");
}

use super::*;

#[test]
fn nested_headings_stay_in_their_section_and_an_empty_section_reads_as_empty_text() {
    let text = "---\nstatus: design\n---\n\n## Top Level Section\n\nTop body.\n\n### Sub Heading\n\nSub content.\n\n#### Deep Heading\n\nDeep content.\n\n## Empty One\n\n## Second Section\n\nSecond body.\n";
    let doc = load_plan_text(text).unwrap();
    let names: Vec<&str> = doc.sections.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(names, ["Top Level Section", "Empty One", "Second Section"]);
    assert!(doc
        .get_section("Top Level Section")
        .unwrap()
        .contains("### Sub Heading"));
    assert_eq!(doc.get_section("Empty One").unwrap(), "");
}

#[test]
fn several_fences_in_one_section_each_hide_their_headings() {
    let text = "---\nstatus: design\n---\n\n## Setup\n\nBefore code.\n\n```python\n## Not a heading in code\ndef foo():\n    pass\n```\n\nAfter code.\n\n```yaml\n## Also not a heading\nkey: value\n```\n\nFinal text.\n\n## Teardown\n\nTeardown body.\n";
    let doc = load_plan_text(text).unwrap();
    let names: Vec<&str> = doc.sections.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(names, ["Setup", "Teardown"]);
    let setup = doc.get_section("Setup").unwrap();
    assert!(setup.contains("## Not a heading in code"));
    assert!(setup.contains("## Also not a heading"));
}

#[test]
fn malformed_frontmatter_yaml_reports_a_line_number() {
    let err = load_plan_text(
        "---\nstatus: [invalid: yaml: here\ntitle: blah\n---\n\n## Overview\n\nbody\n",
    )
    .unwrap_err();
    assert!(err.line.is_some(), "{err:?}");
}

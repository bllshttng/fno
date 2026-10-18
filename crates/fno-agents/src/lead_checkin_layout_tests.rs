// template_from_tree reads the ACTUAL wire tree, never count+aspect
// alone: a manually-applied row-thirds/col-thirds, an overflow-skipped
// reflow, or a plain stack must each report their real shape, never
// whatever auto-reflow would have picked for that leaf count.
#[test]
fn template_from_tree_rows() {
    let leaf = |id: u64| json!({"Leaf": id});
    let main = |axis: &str, first: Value, rest: Value| json!({"Branch": {"axis": axis, "children": [[0.5, first], [0.5, rest]]}});
    let flat = |axis: &str, children: Vec<Value>| {
        let weight = 1.0 / children.len() as f64;
        json!({"Branch": {"axis": axis, "children": children.into_iter().map(|c| json!([weight, c])).collect::<Vec<_>>()}})
    };

    // 2 leaves: main-left / main-top by axis.
    assert_eq!(
        layout::template_from_tree(&main("Horizontal", leaf(1), leaf(2))),
        "main-left"
    );
    assert_eq!(
        layout::template_from_tree(&main("Vertical", leaf(1), leaf(2))),
        "main-top"
    );

    // 3 leaves: a manually-applied row-thirds/col-thirds must read as
    // itself, not get folded into main-left/main-top by count alone.
    assert_eq!(
        layout::template_from_tree(&flat("Horizontal", vec![leaf(1), leaf(2), leaf(3)])),
        "row-thirds"
    );
    assert_eq!(
        layout::template_from_tree(&flat("Vertical", vec![leaf(1), leaf(2), leaf(3)])),
        "col-thirds"
    );
    assert_eq!(
        layout::template_from_tree(&main(
            "Horizontal",
            leaf(1),
            flat("Vertical", vec![leaf(2), leaf(3)])
        )),
        "main-left"
    );

    // 4 leaves: grid-2x2's real nested shape.
    let grid = json!({"Branch": {"axis": "Vertical", "children": [
        [0.5, flat("Horizontal", vec![leaf(1), leaf(2)])],
        [0.5, flat("Horizontal", vec![leaf(3), leaf(4)])],
    ]}});
    assert_eq!(layout::template_from_tree(&grid), "grid-2x2");

    // A plain 1-column stack (naive repeated splits, nested same-axis,
    // never flattened) is not grid-2x2 just because it has 4 leaves.
    let stack = json!({"Branch": {"axis": "Vertical", "children": [
        [0.25, leaf(1)],
        [0.75, {"Branch": {"axis": "Vertical", "children": [
            [0.33, leaf(2)],
            [0.67, {"Branch": {"axis": "Vertical", "children": [
                [0.5, leaf(3)], [0.5, leaf(4)],
            ]}}],
        ]}}],
    ]}});
    assert_eq!(layout::template_from_tree(&stack), "unshaped");

    // A count outside 2/3/4 is always unshaped, regardless of shape.
    assert_eq!(layout::template_from_tree(&leaf(1)), "unshaped");
}

// A layout: line is present and non-empty whenever at least one
// tab exists, naming each tab's pane count and the template it reflowed
// to (or "unshaped" when the count isn't 2/3/4).
#[test]
fn layout_rows() {
    let mut readings = sample_readings(board7(), org4(), cap_ok(), workers3());
    set_reading(
        &mut readings,
        Reading::took(
            "layout",
            json!({"tabs": [
                {"name": "main", "panes": 4, "template": "grid-2x2"},
                {"name": "scratch", "panes": 5, "template": "unshaped"},
            ]}),
        ),
    );
    let data = build_data(&readings, "x-bbbb");
    let lines = render_lines("x-bbbb", &readings, &data, &None, "", "no change");
    let layout_line = lines.iter().find(|l| l.starts_with("layout:")).unwrap();
    assert!(
        layout_line.contains("main 4p/grid-2x2"),
        "line: {layout_line}"
    );
    assert!(
        layout_line.contains("scratch 5p/unshaped"),
        "line: {layout_line}"
    );

    // A failed reading prints the failure line, not a silent drop.
    let mut readings = sample_readings(board7(), org4(), cap_ok(), workers3());
    set_reading(
        &mut readings,
        Reading::failed(
            "layout",
            "layout unavailable: mux server not running".into(),
        ),
    );
    let data = build_data(&readings, "x-bbbb");
    let lines = render_lines("x-bbbb", &readings, &data, &None, "", "no change");
    assert!(
        lines.iter().any(|l| l.starts_with("READER FAILED layout:")),
        "lines: {lines:?}"
    );
}

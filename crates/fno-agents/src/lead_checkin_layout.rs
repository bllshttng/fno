// The check-in `layout:` reading - moved out of lead_checkin.rs (file budget
// shrink) via include!, so it shares lead_checkin's own module scope.

/// One row per live mux tab: shells to `fno mux layout get --json`
/// with no scope flags, which reads the WHOLE session (every squad, every
/// tab) in one call. `fno-agents` cannot link the `fno` crate in a real
/// build (it is a `[dev-dependencies]`-only path; `fno` shells the `fno`
/// binary at runtime instead - see `crates/fno-agents/Cargo.toml`), so the
/// template is read back structurally off the wire `root` tree in
/// [`template_from_tree`] rather than inferred from count and aspect: a
/// manually-applied template, an overflow-skipped reflow, or an exempt
/// shape (diff pane, portal) must never be misreported as whatever
/// auto-reflow would have picked.
fn r_layout() -> Result<Value, String> {
    let (_, out, err) = fno_command(&["mux", "layout", "get", "--json"])?;
    if out.trim().is_empty() {
        return Err(format!("layout unavailable: {}", stderr_cause(&err)));
    }
    let squads: Value = serde_json::from_str(out.trim())
        .map_err(|e| format!("layout payload did not parse: {e}"))?;
    let squads = squads
        .as_array()
        .ok_or_else(|| "layout payload is not an array".to_string())?;
    let mut tabs = Vec::new();
    for squad in squads {
        let squad_name = squad.get("squad_name").and_then(Value::as_str);
        let Some(tab_rows) = squad.get("tabs").and_then(Value::as_array) else {
            continue;
        };
        for tab in tab_rows {
            let name = tab
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| squad_name.unwrap_or("-").to_string());
            let panes = tab
                .get("panes")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let template = tab
                .get("root")
                .map(template_from_tree)
                .unwrap_or("unshaped");
            tabs.push(json!({
                "name": name,
                "panes": panes,
                "template": template,
            }));
        }
    }
    Ok(json!({ "tabs": tabs }))
}

/// A wire-JSON `Node` (`crates/fno/src/tree.rs`): externally-tagged as
/// `{"Leaf": id}` or `{"Branch": {"axis": "Horizontal"|"Vertical",
/// "children": [[ratio, Node], ...]}}`. Read generically off the JSON
/// payload since `fno-agents` cannot import the real type (see
/// [`r_layout`]'s doc comment).
fn tree_leaf_count(node: &Value) -> usize {
    if node.get("Leaf").is_some() {
        return 1;
    }
    node.get("Branch")
        .and_then(|b| b.get("children"))
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .filter_map(|c| c.as_array().and_then(|p| p.get(1)))
                .map(tree_leaf_count)
                .sum()
        })
        .unwrap_or(0)
}

fn branch_of(node: &Value) -> Option<(&str, &Vec<Value>)> {
    let b = node.get("Branch")?;
    Some((b.get("axis")?.as_str()?, b.get("children")?.as_array()?))
}

fn is_leaf(node: &Value) -> bool {
    node.get("Leaf").is_some()
}

fn nth_child(children: &[Value], i: usize) -> Option<&Value> {
    children.get(i)?.as_array()?.get(1)
}

/// `Branch{want_axis, children: k flat leaves}` - `row-thirds`/`col-thirds`
/// and the "rest" arm of a `main-*` template beyond k=2.
fn matches_flat(node: &Value, want_axis: &str, k: usize) -> bool {
    let Some((axis, children)) = branch_of(node) else {
        return false;
    };
    axis == want_axis
        && children.len() == k
        && (0..k).all(|i| nth_child(children, i).is_some_and(is_leaf))
}

/// `Branch{want_axis, [leaf, rest]}` - `main-left`/`main-top`'s shape for
/// any k >= 2, matching [`crate::templates::topology`]'s `rest()` exactly.
fn matches_main(node: &Value, want_axis: &str, k: usize) -> bool {
    let Some((axis, children)) = branch_of(node) else {
        return false;
    };
    if axis != want_axis || children.len() != 2 {
        return false;
    }
    let Some(first) = nth_child(children, 0) else {
        return false;
    };
    let Some(rest) = nth_child(children, 1) else {
        return false;
    };
    if !is_leaf(first) {
        return false;
    }
    if k == 2 {
        return is_leaf(rest);
    }
    let other_axis = if want_axis == "Horizontal" {
        "Vertical"
    } else {
        "Horizontal"
    };
    matches_flat(rest, other_axis, k - 1)
}

/// `Branch{Vertical, [row, row]}` where each row is `Branch{Horizontal, [leaf, leaf]}`.
fn matches_grid2x2(node: &Value) -> bool {
    let Some((axis, children)) = branch_of(node) else {
        return false;
    };
    axis == "Vertical"
        && children.len() == 2
        && nth_child(children, 0).is_some_and(|c| matches_flat(c, "Horizontal", 2))
        && nth_child(children, 1).is_some_and(|c| matches_flat(c, "Horizontal", 2))
}

/// The template name a wire-JSON tree structurally matches, ignoring leaf
/// identity - mirrors every shape [`crate::templates`] (`crates/fno/src`)
/// can produce for 2/3/4 leaves. Anything else - a stack, a manual drag, an
/// overflow-skipped reflow, a diff-pane or portal shape - reads `unshaped`.
fn template_from_tree(root: &Value) -> &'static str {
    match tree_leaf_count(root) {
        2 => {
            if matches_main(root, "Horizontal", 2) {
                "main-left"
            } else if matches_main(root, "Vertical", 2) {
                "main-top"
            } else {
                "unshaped"
            }
        }
        3 => {
            if matches_flat(root, "Horizontal", 3) {
                "row-thirds"
            } else if matches_flat(root, "Vertical", 3) {
                "col-thirds"
            } else if matches_main(root, "Horizontal", 3) {
                "main-left"
            } else if matches_main(root, "Vertical", 3) {
                "main-top"
            } else {
                "unshaped"
            }
        }
        4 => {
            if matches_grid2x2(root) {
                "grid-2x2"
            } else if matches_main(root, "Horizontal", 4) {
                "main-left"
            } else if matches_main(root, "Vertical", 4) {
                "main-top"
            } else {
                "unshaped"
            }
        }
        _ => "unshaped",
    }
}

fn layout_lines(readings: &[Reading]) -> Vec<String> {
    let reading = readings.iter().find(|r| r.name == "layout");
    let Some(reading) = reading else {
        return Vec::new();
    };
    if !reading.ok {
        return vec![format!("READER FAILED layout: {}", reading.error)];
    }
    let tabs = reading.value.get("tabs").and_then(Value::as_array);
    match tabs {
        Some(tabs) if !tabs.is_empty() => {
            let parts: Vec<String> = tabs
                .iter()
                .map(|t| {
                    format!(
                        "{} {}p/{}",
                        dash(t.get("name")),
                        dash(t.get("panes")),
                        dash(t.get("template")),
                    )
                })
                .collect();
            vec![format!("layout: {}", parts.join(", "))]
        }
        _ => vec!["layout: no tabs".to_string()],
    }
}

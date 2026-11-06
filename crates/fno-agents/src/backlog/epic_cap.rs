//! The epic child cap. `config.backlog.epic_max_open_children`, read from
//! the `config.toml` beside the graph, bounds the open direct children one
//! epic may hold: a write that parents one more open child under a full
//! epic is refused, so new findings start a new small epic instead of
//! growing one that never finishes. The check runs at both publication
//! seams, so every door (update parent=, idea --parent, contain,
//! decompose, the rollup auto-link, api node_create) meets the same
//! refusal. Unset means no cap, which is the OSS default.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// `config.backlog.epic_max_open_children`, beside the graph. `None` = no
/// cap. Only positive integers count: a missing file, `0`, `-1` and a
/// quoted `"15"` all read as no cap.
pub fn configured_cap(graph: &Path) -> Option<usize> {
    let dir = graph.parent().unwrap_or_else(|| Path::new(""));
    crate::backlog_ready::backlog_config_int(dir, "epic_max_open_children")
        .filter(|n| *n > 0)
        .map(|n| n as usize)
}

/// A row holds its parent open: not terminal and not deferred (a lead does
/// not work parked rows, so a wont-do child never holds its epic open).
fn row_is_open(row: &Value) -> bool {
    !crate::graph_store::is_terminal_entry(row)
        && row.get("status").and_then(Value::as_str) != Some("deferred")
}

fn parent_of(row: &Value) -> Option<&str> {
    row.get("parent")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
}

/// Open direct children one epic holds: the same count the refusal judges
/// with, lifted here so the read-time view and the write-time refusal
/// cannot disagree.
pub fn open_child_count(rows: &[Value], epic: &str) -> usize {
    rows.iter()
        .filter(|r| row_is_open(r) && parent_of(r) == Some(epic))
        .count()
}

/// Refuse a write that leaves an epic it grew with more open children than
/// the cap. `pre` is the begin snapshot, `post` the candidate state. With
/// no cap configured this returns `Ok` at once.
pub fn enforce(pre: &[Value], post: &[Value], cap: Option<usize>) -> Result<(), String> {
    let Some(cap) = cap else {
        return Ok(());
    };
    // Borrowed id indexes, not a linear scan per row: every graph write
    // reaches this seam, and a `find()` inside the row loop is quadratic
    // over the whole graph.
    let post_by_id = crate::graph_store::index_by_id(post);
    let pre_by_id = crate::graph_store::index_by_id(pre);
    // Rows this write newly parents under an epic: open, parent non-empty
    // and not self, parent typed epic, and the edge is new (the row was
    // absent, or its parent before the write differed).
    let mut grown: Vec<(&str, &Value, Vec<&str>)> = Vec::new();
    for row in post.iter() {
        let (Some(id), true) = (crate::graph_store::entry_id(row), row.is_object()) else {
            continue;
        };
        if !row_is_open(row) {
            continue;
        }
        let Some(epic) = parent_of(row).filter(|p| *p != id) else {
            continue;
        };
        let Some(epic_row) = post_by_id.get(epic).copied() else {
            continue;
        };
        if epic_row.get("type").and_then(Value::as_str) != Some("epic") {
            continue;
        }
        let pre_row = pre_by_id.get(id).copied();
        // The edge is unchanged AND the row was already open, so this write
        // adds no open child. A child coming back from closed (undefer,
        // reopen) does add one, even under an unchanged edge, so it is
        // judged like a fresh edge rather than skipped here.
        // A type update can turn a populated feature into an epic. Its
        // children keep both their edge and their status, so judging the
        // edge alone skips every one of them and the epic is born over
        // cap. The parent must ALSO have been an epic before the write.
        let was_epic = pre_by_id
            .get(epic)
            .map(|r| r.get("type").and_then(Value::as_str) == Some("epic"))
            .unwrap_or(false);
        let already_counted = was_epic
            && pre_row
                .map(|r| parent_of(r) == Some(epic) && row_is_open(r))
                .unwrap_or(false);
        if already_counted {
            continue;
        }
        // Hand-up exemption: the old parent is terminal AFTER the write, so
        // this edge moves existing work (the done/maintain hand-up) and
        // adds none. It covers an ALREADY-OPEN row only, for the same
        // reason the unchanged edge above does: a row coming back from
        // closed is new open work wherever it lands.
        if let Some(old) = pre_row.filter(|r| row_is_open(r)).and_then(parent_of) {
            if post_by_id
                .get(old)
                .copied()
                .map(crate::graph_store::is_terminal_entry)
                .unwrap_or(false)
            {
                continue;
            }
        }
        let entry = match grown.iter_mut().find(|(e, _, _)| *e == epic) {
            Some(entry) => entry,
            None => {
                grown.push((epic, epic_row, Vec::new()));
                grown.last_mut().unwrap()
            }
        };
        entry.2.push(id);
    }
    for (epic, epic_row, children) in grown {
        let open = open_child_count(post, epic);
        if open <= cap {
            continue;
        }
        let title = epic_row
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title: String = title.chars().take(80).collect();
        let kids = children
            .iter()
            .map(|id| format!("'{}'", id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "epic cap: refusing to add {kids} under epic '{epic}' ('{title}'): it would hold \
             {open} open children and the cap is {cap} (config backlog.epic_max_open_children). \
             An epic stays small enough to finish, so new work starts a new epic. Next: \
             fno backlog idea \"EPIC: <theme>\" --type epic --difficulty <low|medium|high>, \
             then point this write at the new epic id. If a lead leads '{epic}', it adds the \
             new epic to its own team: fno agents org promote <its handle> --scope <each epic \
             it \
             holds> --scope '<new-epic-id>'. That works for an epic the lead's own session \
             created. Any other epic needs a team that contains both: ask that lead."
        ));
    }
    Ok(())
}

/// Read-time view of the cap: for the epic ids given, one row per epic that
/// holds at least one open direct child, fullest first. `full` means the
/// next open child is refused: `enforce` refuses once the count after a
/// write passes the cap, so a count already at the cap cannot take one
/// more. An epic's own status is ignored, as `enforce` ignores it.
pub fn epic_load(rows: &[Value], ids: &BTreeSet<String>, cap: Option<usize>) -> Vec<Value> {
    let by_id = crate::graph_store::index_by_id(rows);
    let mut load: Vec<(String, usize)> = ids
        .iter()
        .filter(|id| {
            by_id
                .get(id.as_str())
                .map(|r| r.get("type").and_then(Value::as_str) == Some("epic"))
                .unwrap_or(false)
        })
        .map(|id| (id.clone(), open_child_count(rows, id)))
        .filter(|(_, n)| *n > 0)
        .collect();
    load.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    load.into_iter()
        .map(|(id, n)| json!({"id": id, "open_children": n, "full": cap.is_some_and(|c| n >= c)}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn epic(id: &str) -> Value {
        json!({"id": id, "slug": id, "title": "the full epic", "type": "epic",
               "status": "in_progress", "priority": "p1", "domain": "code"})
    }

    fn child(id: &str, parent: &str) -> Value {
        json!({"id": id, "slug": id, "title": id, "type": "feature",
               "status": "idea", "priority": "p2", "domain": "code", "parent": parent})
    }

    fn full_epic_rows() -> Vec<Value> {
        let mut rows = vec![epic("e-1")];
        for i in 1..=15 {
            rows.push(child(&format!("c-{i:02}"), "e-1"));
        }
        rows
    }

    #[test]
    fn a_sixteenth_open_child_is_refused_and_names_the_cap() {
        // AC1-HP, first half: the message names the epic, both counts, the
        // config key and the new-epic command.
        let pre = full_epic_rows();
        let mut post = pre.clone();
        post.push(child("c-16", "e-1"));
        let error = enforce(&pre, &post, Some(15)).unwrap_err();
        assert!(error.contains("epic cap: refusing to add"), "{error}");
        assert!(error.contains("'c-16'"), "{error}");
        assert!(error.contains("'e-1'"), "{error}");
        assert!(error.contains("16 open children"), "{error}");
        assert!(error.contains("the cap is 15"), "{error}");
        assert!(error.contains("backlog.epic_max_open_children"), "{error}");
        assert!(error.contains("--type epic"), "{error}");
    }

    #[test]
    fn configured_cap_reads_only_positive_integers() {
        // AC3-EDGE, last line: missing file, 0, -1 and "15" all read as None.
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        crate::graph_store::seed_rows(&graph, &[]).unwrap();
        assert_eq!(
            configured_cap(&graph),
            None,
            "no config.toml beside the graph"
        );
        for body in [
            "[backlog]\nepic_max_open_children = 0\n",
            "[backlog]\nepic_max_open_children = -1\n",
            "[backlog]\nepic_max_open_children = \"15\"\n",
            "[backlog]\nstaleness_days = 15\n",
        ] {
            std::fs::write(dir.path().join("config.toml"), body).unwrap();
            assert_eq!(configured_cap(&graph), None, "body: {body}");
        }
        std::fs::write(
            dir.path().join("config.toml"),
            "[backlog]\nepic_max_open_children = 15\n",
        )
        .unwrap();
        assert_eq!(configured_cap(&graph), Some(15));
    }
}

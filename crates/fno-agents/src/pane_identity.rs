//! `fno agents pane-identity`: cross-check one mux server's panes against the
//! registry, in both directions. A read leaf with no action of its own: the
//! router rewrites it into `status --pane-identity`, because the binary's
//! action list is shrink-only.
//!
//! Direction 1 finds a registry row whose mux ref points at a pane the listing
//! cannot resolve to that row (the stale pane a resume re-homed). Direction 2
//! finds a pane whose process tree carries fno's spawn signature with no row
//! at all. Direction 2 reads argv only to RAISE a mismatch for an operator; it
//! never mints an identity from argv, which can outlive the process it names.
//! A mismatch is a reading, never a repair: nothing here writes.

use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// The argv flags fno's own spawn posture renders (codex's yolo sandbox arm,
/// claude's permission-mode else-arm), restated so the check never has to
/// build an argv to learn them.
const SPAWN_SIGNATURE_FLAGS: [&str; 2] = [
    "--dangerously-bypass-approvals-and-sandbox",
    "--dangerously-skip-permissions",
];

const USAGE: &str = "usage: fno agents pane-identity [--server NAME] [--json]";

/// Python truthiness for a JSON value: null, false, 0, "", [] and {} are false.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A listing from an older daemon carries no `fno_id_state`; derive it from
/// the raw id so the check stays honest against both.
fn pane_state(pane: &Value) -> String {
    match pane.get("fno_id_state") {
        state if truthy(state) => text_of(state.unwrap_or(&Value::Null)),
        _ if truthy(pane.get("fno_id")) => "resolved".to_string(),
        _ => "unresolved:spawned-name".to_string(),
    }
}

/// Compare one server's pane listing against the registry rows. `argv_of`
/// answers a pid's process-tree argv; it is a parameter so both directions
/// test on constructed rows.
pub(crate) fn crosscheck(
    panes: &[Value],
    rows: &[Value],
    session: &str,
    argv_of: &dyn Fn(u64) -> Option<String>,
) -> Value {
    let null = Value::Null;
    let pane_by_id: HashMap<String, &Value> = panes
        .iter()
        .map(|p| (p.get("pane_id").unwrap_or(&null).to_string(), p))
        .collect();
    let rows_with_mux: Vec<&Value> = rows.iter().filter(|r| truthy(r.get("mux"))).collect();
    let rows_here: Vec<&Value> = rows_with_mux
        .iter()
        .copied()
        .filter(|r| r["mux"].get("session").and_then(Value::as_str) == Some(session))
        .collect();
    let rows_other_session = rows_with_mux.len() - rows_here.len();

    let mut row_mismatches = Vec::new();
    for r in &rows_here {
        let name = r.get("name").cloned().unwrap_or(Value::Null);
        let pane_ref = r["mux"].get("pane_id").cloned().unwrap_or(Value::Null);
        let expected = r
            .get("harness_session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let Some(pane) = pane_by_id.get(&pane_ref.to_string()) else {
            row_mismatches.push(json!({
                "row": name, "pane": pane_ref, "reason": "pane missing from session listing",
            }));
            continue;
        };
        if expected.is_empty() {
            row_mismatches.push(json!({
                "row": name, "pane": pane_ref, "reason": "row carries a mux ref but no session id",
            }));
            continue;
        }
        // State only: a pane's fno_id is the row's own mint, never the harness
        // id the row records, so equality would flag every healthy row.
        let state = pane_state(pane);
        if state != "resolved" {
            let shown = match pane.get("fno_id") {
                actual if truthy(actual) => text_of(actual.unwrap_or(&Value::Null)),
                _ => state,
            };
            row_mismatches.push(json!({
                "row": name,
                "pane": pane_ref,
                "reason": format!(
                    "pane id is {}, row expects {expected}",
                    crate::client_commands::py_repr_str(&shown)
                ),
            }));
        }
    }

    // Pane ids are server-local, so only a row of THIS server spares a pane.
    let referenced: HashSet<String> = rows_here
        .iter()
        .map(|r| r["mux"].get("pane_id").unwrap_or(&null).to_string())
        .collect();
    let mut pane_mismatches = Vec::new();
    for p in panes {
        if pane_state(p) == "resolved" || truthy(p.get("pristine_idle_shell")) {
            continue;
        }
        let Some(pid) = p.get("child_pid").and_then(Value::as_u64) else {
            continue;
        };
        let Some(argv) = argv_of(pid).filter(|a| !a.is_empty()) else {
            continue;
        };
        let pane_ref = p.get("pane_id").cloned().unwrap_or(Value::Null);
        if referenced.contains(&pane_ref.to_string()) {
            continue;
        }
        if SPAWN_SIGNATURE_FLAGS.iter().any(|flag| argv.contains(flag)) {
            pane_mismatches.push(json!({
                "pane": pane_ref, "pid": pid, "reason": "fno spawn signature in argv, no registry row",
            }));
        }
    }

    json!({
        "session": session,
        "panes_compared": panes.len(),
        "rows_with_mux_compared": rows_here.len(),
        "rows_other_session": rows_other_session,
        "row_mismatches": row_mismatches,
        "pane_mismatches": pane_mismatches,
    })
}

/// The human reading. The counts line prints on every run, so a clean result
/// is told apart from a check that never enumerated anything.
pub(crate) fn render(result: &Value) -> String {
    let count = |key: &str| result[key].as_u64().unwrap_or(0);
    let mut head = format!(
        "panes compared: {}; rows with mux ref compared: {}",
        count("panes_compared"),
        count("rows_with_mux_compared")
    );
    if count("rows_other_session") > 0 {
        head.push_str(&format!(
            "; rows in other sessions skipped: {}",
            count("rows_other_session")
        ));
    }
    let empty = Vec::new();
    let rows = result["row_mismatches"].as_array().unwrap_or(&empty);
    let panes = result["pane_mismatches"].as_array().unwrap_or(&empty);
    let mut lines = vec![head, format!("row -> pane mismatches: {}", rows.len())];
    for m in rows {
        lines.push(format!(
            "  row {} pane {}: {}",
            text_of(&m["row"]),
            text_of(&m["pane"]),
            text_of(&m["reason"])
        ));
    }
    lines.push(format!("pane -> row mismatches: {}", panes.len()));
    for m in panes {
        lines.push(format!(
            "  pane {} pid {}: {}",
            text_of(&m["pane"]),
            text_of(&m["pid"]),
            text_of(&m["reason"])
        ));
    }
    if rows.is_empty() && panes.is_empty() {
        lines.push("clean: no mismatch in either direction".to_string());
    }
    lines.join("\n")
}

/// One `ps` snapshot: pid -> its argv joined with every descendant's argv.
/// The tree, not the child: a pane's child is usually the shell, and the
/// harness runs under it, maybe behind a launcher. Empty when ps cannot answer.
fn process_tree_from(ps_output: &str) -> HashMap<u64, String> {
    let mut args_by_pid: HashMap<u64, String> = HashMap::new();
    let mut children_of: HashMap<u64, Vec<u64>> = HashMap::new();
    for line in ps_output.lines() {
        let mut parts = line.split_whitespace();
        let (Some(ppid), Some(pid)) = (parts.next(), parts.next()) else {
            continue;
        };
        let args = parts.collect::<Vec<_>>().join(" ");
        let (Ok(ppid), Ok(pid)) = (ppid.parse::<u64>(), pid.parse::<u64>()) else {
            continue;
        };
        if args.is_empty() {
            continue;
        }
        args_by_pid.insert(pid, args);
        children_of.entry(ppid).or_default().push(pid);
    }
    let mut trees = HashMap::new();
    for (pid, own) in &args_by_pid {
        let mut lines = vec![own.clone()];
        let mut stack: Vec<u64> = children_of.get(pid).cloned().unwrap_or_default();
        let mut seen = HashSet::new();
        while let Some(descendant) = stack.pop() {
            if !seen.insert(descendant) {
                continue;
            }
            if let Some(args) = args_by_pid.get(&descendant) {
                lines.push(args.clone());
            }
            stack.extend(children_of.get(&descendant).cloned().unwrap_or_default());
        }
        trees.insert(*pid, lines.join("\n").trim().to_string());
    }
    trees
}

fn process_tree() -> HashMap<u64, String> {
    std::process::Command::new("ps")
        .args(["-axo", "ppid=,pid=,args="])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| process_tree_from(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

/// Fold a deprecated alias into the canonical flag: the alias alone warns,
/// both together refuse.
fn merge_alias(
    canonical: Option<String>,
    legacy: Option<String>,
    canonical_flag: &str,
    legacy_flag: &str,
) -> Result<Option<String>, String> {
    let Some(legacy) = legacy.filter(|v| !v.is_empty()) else {
        return Ok(canonical);
    };
    if canonical.as_deref().is_some_and(|v| !v.is_empty()) {
        return Err(format!(
            "{legacy_flag} is a deprecated spelling of {canonical_flag}; pass one, not both"
        ));
    }
    eprintln!(
        "warning: {legacy_flag} is deprecated; use {canonical_flag} instead. \
         The alias will be removed in a future release."
    );
    Ok(Some(legacy))
}

/// Exit 0 clean, 1 a mismatch, 2 an unreadable instrument or a usage error.
pub fn run_pane_identity(args: &[String]) -> i32 {
    let (mut server, mut session_id, mut session_legacy) = (None, None, None);
    let mut as_json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let slot = match flag {
            "--json" | "-J" => {
                as_json = true;
                continue;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--server" => &mut server,
            "--session-id" => &mut session_id,
            "--session" => &mut session_legacy,
            other => {
                eprintln!("pane-identity: no such option: {other}\n{USAGE}");
                return 2;
            }
        };
        match inline.or_else(|| it.next().cloned()) {
            Some(value) => *slot = Some(value),
            None => {
                eprintln!("pane-identity: option {flag} requires a value\n{USAGE}");
                return 2;
            }
        }
    }
    let explicit = merge_alias(server, session_id, "--server", "--session-id")
        .and_then(|v| merge_alias(v, session_legacy, "--server", "--session"));
    let explicit = match explicit {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pane-identity: {e}");
            return 2;
        }
    };
    let session = crate::pane_spawn::resolve_mux_session(explicit.as_deref());

    let listing =
        match crate::pane_stop::run_fno(&["mux", "pane", "ls", "--server", &session, "--json"]) {
            Ok(run) => run,
            Err(e) => {
                eprintln!("pane-identity: mux listing unavailable: {e}");
                return 2;
            }
        };
    let stdout = String::from_utf8_lossy(&listing.stdout);
    if !listing.ok || stdout.trim().is_empty() {
        let detail = if listing.stderr.is_empty() {
            "pane ls returned non-zero".to_string()
        } else {
            listing.stderr.clone()
        };
        eprintln!("pane-identity: mux listing unavailable: {detail}");
        return 2;
    }
    let panes = match serde_json::from_str::<Value>(&stdout) {
        Ok(Value::Array(panes)) => panes,
        Ok(_) => {
            eprintln!("pane-identity: pane ls JSON was not a list");
            return 2;
        }
        Err(e) => {
            eprintln!("pane-identity: unparseable pane ls JSON: {e}");
            return 2;
        }
    };
    let rows =
        match crate::client_commands::load_registry_entries(&AgentsHome::shared_registry_json()) {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("pane-identity: registry unavailable: {e}");
                return 2;
            }
        };
    let tree = process_tree();
    let result = crosscheck(&panes, &rows, &session, &|pid| tree.get(&pid).cloned());
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).unwrap_or_default()
        );
    } else {
        println!("{}", render(&result));
    }
    let mismatched = |key: &str| result[key].as_array().is_some_and(|a| !a.is_empty());
    if mismatched("row_mismatches") || mismatched("pane_mismatches") {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(pane_id: u64, fno_id: Option<&str>, child_pid: Option<u64>, pristine: bool) -> Value {
        json!({
            "pane_id": pane_id,
            "fno_id": fno_id,
            "fno_id_state": if fno_id.is_some() { "resolved" } else { "unresolved:spawned-name" },
            "child_pid": child_pid,
            "pristine_idle_shell": pristine,
        })
    }

    fn row(name: &str, pane_id: u64, sid: &str) -> Value {
        json!({"name": name, "mux": {"session": "main", "pane_id": pane_id}, "harness_session_id": sid})
    }

    const SIGNED: &str = "codex --model gpt --dangerously-bypass-approvals-and-sandbox";

    #[test]
    fn direction_one_flags_a_row_whose_pane_vanished() {
        let out = crosscheck(&[], &[row("gone", 999, &"a".repeat(36))], "main", &|_| None);
        assert_eq!(
            out["row_mismatches"],
            json!([{"row": "gone", "pane": 999, "reason": "pane missing from session listing"}])
        );
        assert_eq!(
            (
                out["panes_compared"].as_u64(),
                out["rows_with_mux_compared"].as_u64()
            ),
            (Some(0), Some(1))
        );
    }

    #[test]
    fn direction_two_flags_signed_argv_with_no_row() {
        let out = crosscheck(&[pane(9, None, Some(11258), false)], &[], "main", &|_| {
            Some(SIGNED.to_string())
        });
        assert_eq!(
            out["pane_mismatches"],
            json!([{"pane": 9, "pid": 11258, "reason": "fno spawn signature in argv, no registry row"}])
        );
    }

    #[test]
    fn direction_two_spares_referenced_plain_pristine_and_resolved_panes() {
        let panes = [
            pane(1, None, Some(101), false),
            pane(2, None, Some(102), false),
            pane(3, None, Some(103), true),
            pane(4, None, None, false),
            pane(
                5,
                Some("119e3c52-a4b3-4f7e-8a1c-2d3e4f5a6b7c"),
                Some(105),
                false,
            ),
        ];
        let argv = |pid: u64| match pid {
            102 => Some("/bin/zsh".to_string()),
            _ => Some(SIGNED.to_string()),
        };
        let out = crosscheck(&panes, &[row("held", 1, "x")], "main", &argv);
        assert_eq!(out["pane_mismatches"], json!([]));
    }

    #[test]
    fn the_counts_print_even_when_clean() {
        let text = render(&crosscheck(&[], &[], "main", &|_| None));
        for needle in [
            "panes compared: 0",
            "rows with mux ref compared: 0",
            "clean: no mismatch in either direction",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }

    #[test]
    fn the_process_tree_walks_transitive_descendants() {
        let tree = process_tree_from(
            "    1  100 /bin/zsh\n  100  101 /bin/sh -c launcher\n  101  102 codex --dangerously-bypass-approvals-and-sandbox\n",
        );
        assert!(
            tree[&100].contains("--dangerously-bypass-approvals-and-sandbox")
                && tree[&100].contains("codex")
        );
        assert!(tree[&102].starts_with("codex"));
    }
}

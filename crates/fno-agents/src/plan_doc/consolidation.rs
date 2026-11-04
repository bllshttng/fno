//! `fno do plan consolidation <plan> [<node>]` (hidden): the consolidation
//! block check `scripts/validate-plan.sh` renders. One tab-separated line
//! per finding, the shell's own channel:
//!
//!   E<TAB>msg            a defect          O<TAB>outcome    the block's outcome
//!   U<TAB>why            not checked       M<TAB>id<TAB>ts<TAB>text  a live
//!   W<TAB>why            index unreadable  decision with no acknowledgment
//!   T<TAB>why            the read timed out  D<TAB>n  live decisions checked
//!
//! The shape is the plan-schema door's (validate_block); this adds only the
//! live read, the node's decisions, which the door has no access to.

use serde_json::Value;
use std::path::Path;

const ENUM: &str = "(absorb | append | proceed_alone)";
/// The decision read takes about 3s of CPU; past this the machine had none.
const DECISIONS_TIMEOUT_SECS: u64 = 60;

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, n: usize) -> String {
    one_line(s).chars().take(n).collect()
}

/// The text between the first two `---` lines: the same rule as the `/^---/`
/// awk every other check in the validator uses, so a stricter reader never
/// says NOT CHECKED on a plan the presence check accepted.
fn frontmatter(doc: &str) -> Option<String> {
    let lines: Vec<&str> = doc.lines().collect();
    let open = lines.iter().position(|l| l.starts_with("---"))?;
    let close = lines
        .iter()
        .skip(open + 1)
        .position(|l| l.starts_with("---"))?
        + open
        + 1;
    Some(lines[open + 1..close].join("\n"))
}

/// One plan-schema door error in the channel's own words.
fn render(err: &Value, block: &Value) -> String {
    let field = err.get("field").and_then(Value::as_str).unwrap_or("");
    let message = err.get("message").and_then(Value::as_str).unwrap_or("");
    if field == "consolidation.outcome" {
        if message == "Field required" {
            return format!(
                "consolidation block present but has no outcome: line (expected {ENUM})"
            );
        }
        return format!(
            "consolidation outcome `{}` is not in the enum {ENUM}",
            text(block.get("outcome"))
        );
    }
    let parts: Vec<&str> = field.split('.').collect();
    let index = parts.get(2).and_then(|p| p.parse::<usize>().ok());
    if parts.len() == 3 && parts[0] == "consolidation" {
        if let Some(i) = index {
            return format!(
                "consolidation section `{}` entry {} is not an id/reason mapping",
                parts[1],
                i + 1
            );
        }
    }
    if let (4, "consolidation", Some(i)) = (parts.len(), parts[0], index) {
        let (section, leaf) = (parts[1], parts[3]);
        let entry = block
            .get(section)
            .and_then(Value::as_array)
            .and_then(|a| a.get(i));
        let key = |k: &str| entry.and_then(|e| if e.is_object() { e.get(k) } else { Some(e) });
        match leaf {
            "id" if message == "Field required" => return format!("consolidation section `{section}` has an entry with no id"),
            "id" => return format!("consolidation entry id `{}` ({section}) is not a node id (expected <prefix>-<hex>, e.g. x-aaaa)", text(key("id"))),
            "decision_id" if message == "Field required" => return format!("consolidation section `{section}` has an entry with no decision_id"),
            "decision_id" => return format!("consolidation entry decision_id `{}` ({section}) is not a decision id (expected d-<hex>)", text(key("decision_id"))),
            "reason" => {
                let named = entry
                    .and_then(|e| if e.is_object() { e.get("id").or_else(|| e.get("decision_id")) } else { Some(e) });
                return format!("consolidation entry `{}` ({section}) has an empty reason - the recorded decision must be checkable by a later reader", text(named));
            }
            _ => return format!("consolidation section `{section}` entry {} is not an id/reason mapping", i + 1),
        }
    }
    let message = message.strip_prefix("Value error, ").unwrap_or(message);
    for (outcome, key) in [("absorb", "absorbed"), ("append", "appended_to")] {
        if message == format!("outcome {outcome} requires at least one {key} entry") {
            return format!("consolidation outcome is {outcome} but the {key}: list is empty");
        }
    }
    let at = field.strip_prefix("consolidation.").unwrap_or(field);
    format!(
        "consolidation block{}: {message}",
        if at.is_empty() {
            String::new()
        } else {
            format!(" {at}")
        }
    )
}

fn valid_expiry_ref(r: &Value) -> bool {
    let nonblank = |k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    match r.get("kind").and_then(Value::as_str) {
        Some("node") => nonblank("node_id"),
        Some("pr") => {
            nonblank("repository")
                && r.get("number")
                    .and_then(Value::as_i64)
                    .is_some_and(|n| n > 0)
                && r.get("number").is_some_and(Value::is_i64)
        }
        _ => false,
    }
}

/// The node's decisions as the check reads them. `Err` carries the finished
/// T or W line: an unread index is never an empty one.
fn decisions(node: &str) -> Result<Value, String> {
    let mut cmd = std::process::Command::new("fno");
    cmd.args([
        "backlog",
        "decisions",
        node,
        "--limit",
        "0",
        "--state",
        "all",
        "--json",
    ]);
    let started = std::time::Instant::now();
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, DECISIONS_TIMEOUT_SECS)
        .map_err(|e| format!("W\tthe graph could not be read ({}), so coord lifecycles and slug subjects are unknown", clip(&e.to_string(), 160)))?;
    if !out.status.success() {
        if started.elapsed().as_secs() >= DECISIONS_TIMEOUT_SECS {
            return Err(format!(
                "T\tthe decisions read passed its {DECISIONS_TIMEOUT_SECS}s bound"
            ));
        }
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("W\tthe graph could not be read ({}), so coord lifecycles and slug subjects are unknown", clip(&why, 160)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("W\tthe graph could not be read ({}), so coord lifecycles and slug subjects are unknown", clip(&e.to_string(), 160)))
}

fn live_lines(answer: &Value, block: &Value, out: &mut Vec<String>) {
    let empty = Vec::new();
    let rows = answer
        .get("decisions")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let damaged = answer.get("damaged").and_then(Value::as_i64).unwrap_or(0);
    if damaged > 0 {
        out.push(format!("W\t{damaged} damaged row(s) in the decision index - run `fno backlog decide-reindex` and re-validate"));
        return;
    }
    // A graph the door could not read degrades to `lifecycle: unknown` rows
    // carrying the reason: fail closed, never a clean read.
    if let Some(row) = rows.iter().find(|r| {
        r.get("lifecycle").and_then(Value::as_str) == Some("unknown")
            && r.get("lifecycle_reason")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("could not be read")
    }) {
        let why = row
            .get("lifecycle_reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        out.push(format!("W\tthe graph could not be read ({}), so coord lifecycles and slug subjects are unknown", clip(why, 160)));
        return;
    }
    for row in rows.iter().filter(|r| {
        r.get("lane").and_then(Value::as_str) == Some("coord") && r.get("expiry_ref").is_some()
    }) {
        let did = row
            .get("decision_id")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        if !valid_expiry_ref(&row["expiry_ref"]) {
            out.push(format!("E\tcoord decision {did} has invalid expiry_ref shape; use a node ref with node_id or a PR ref with repository and positive number"));
        } else if row.get("lifecycle").and_then(Value::as_str) == Some("unscoped") {
            out.push(format!("E\tcoord decision {did} has explicit expiry_ref but no positive closure evidence; repair the graph evidence and re-validate"));
        }
    }
    // Withdrawn rulings carry a non-live lifecycle from the superseded_by map;
    // ids compare casefolded because a hand-typed acknowledgment need not be.
    let live: Vec<&Value> = rows
        .iter()
        .filter(|r| r.get("lifecycle").and_then(Value::as_str) == Some("live"))
        .collect();
    let acked: std::collections::BTreeSet<String> = block
        .get("decisions_acknowledged")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|e| {
            e.get("decision_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
        })
        .collect();
    for row in &live {
        let did = row.get("decision_id").and_then(Value::as_str).unwrap_or("");
        if !did.is_empty() && !acked.contains(&did.to_lowercase()) {
            let decision = clip(
                row.get("decision").and_then(Value::as_str).unwrap_or(""),
                80,
            );
            let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
            out.push(format!("M\t{did}\t{ts}\t{decision}"));
        }
    }
    out.push(format!("D\t{}", live.len()));
}

pub(crate) fn check(
    doc: &str,
    node: Option<&str>,
    read: &dyn Fn(&str) -> Result<Value, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    let Some(fm) = frontmatter(doc) else {
        return vec!["U\tno closed --- frontmatter block".into()];
    };
    let loaded: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&fm) {
        Ok(v) => v,
        // A repeated key would silently discard the earlier value; YAML 1.2
        // refuses the document, so name it as the defect it is.
        Err(e) if e.to_string().starts_with("duplicate entry with key") => {
            let key = e.to_string().split('"').nth(1).unwrap_or("?").to_string();
            return vec![format!("E\tfrontmatter has more than one `{key}:` line - a repeated key silently discards the earlier value")];
        }
        // The frontmatter check already errors on this; never double-report.
        Err(e) => return vec![format!("U\t{}", clip(&e.to_string(), 160))],
    };
    let loaded: Value = serde_json::to_value(&loaded).unwrap_or(Value::Null);
    let block = loaded.get("consolidation").cloned().unwrap_or(Value::Null);
    if !block.is_object() {
        return vec![format!(
            "E\tconsolidation: must be a block of keys with an outcome: line {ENUM}, not `{}`",
            text(Some(&block))
        )];
    }
    let errors = crate::plan_schema::validate_block("consolidation", &block);
    if !errors.is_empty() {
        return errors
            .iter()
            .map(|e| format!("E\t{}", one_line(&render(e, &block))))
            .collect();
    }
    out.push(format!("O\t{}", text(block.get("outcome"))));
    if let Some(node) = node.map(str::trim).filter(|n| !n.is_empty()) {
        match read(node) {
            Ok(answer) => live_lines(&answer, &block, &mut out),
            Err(line) => out.push(line),
        }
    }
    out
}

pub fn run(rest: &[String]) -> i32 {
    let Some(plan) = rest.first() else {
        eprintln!("usage: fno do plan consolidation <plan> [<node>]");
        return 2;
    };
    let doc = match std::fs::read(Path::new(plan)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            println!("U\tcannot read {plan}: {e}");
            return 0;
        }
    };
    for line in check(&doc, rest.get(1).map(String::as_str), &decisions) {
        println!("{line}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fm(block: &str) -> String {
        format!("---\nstatus: ready\n{block}\n---\nbody\n")
    }

    #[test]
    fn shape_defects_read_in_the_channel_words() {
        let none = |_: &str| -> Result<Value, String> { unreachable!() };
        assert_eq!(
            check("no fence", None, &none),
            ["U\tno closed --- frontmatter block"]
        );
        assert!(check(&fm("consolidation: x"), None, &none)[0]
            .starts_with("E\tconsolidation: must be a block of keys"));
        let lines = check(&fm("consolidation:\n  outcome: merge"), None, &none);
        assert!(
            lines[0].contains("consolidation outcome `merge` is not in the enum"),
            "{lines:?}"
        );
        let lines = check(
            &fm("consolidation:\n  outcome: absorb\n  absorbed: []"),
            None,
            &none,
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("outcome is absorb but the absorbed: list is empty")),
            "{lines:?}"
        );
        let lines = check(
            &fm("consolidation:\n  outcome: proceed_alone\n  outcome: absorb"),
            None,
            &none,
        );
        assert!(
            lines[0].contains("more than one `outcome:` line"),
            "{lines:?}"
        );
    }

    #[test]
    fn live_decisions_need_an_acknowledgment() {
        let doc = fm("consolidation:\n  outcome: proceed_alone\n  decisions_acknowledged:\n    - decision_id: D-AAAA1111\n      reason: covered");
        let read = |node: &str| -> Result<Value, String> {
            assert_eq!(node, "x-1");
            Ok(json!({"decisions": [
                {"decision_id": "d-aaaa1111", "lifecycle": "live", "decision": "old"},
                {"decision_id": "d-bbbb2222", "lifecycle": "live", "ts": "2026-11-01T00:00:00Z", "decision": "new\nruling"},
                {"decision_id": "d-cccc3333", "lifecycle": "superseded"},
                {"decision_id": "d-dddd4444", "lane": "coord", "expiry_ref": {"kind": "pr", "number": 0}}
            ]}))
        };
        let lines = check(&doc, Some("x-1"), &read);
        assert_eq!(lines[0], "O\tproceed_alone");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("E\tcoord decision d-dddd4444 has invalid expiry_ref")),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"M\td-bbbb2222\t2026-11-01T00:00:00Z\tnew ruling".to_string()),
            "{lines:?}"
        );
        assert_eq!(lines.last().unwrap(), "D\t2");
        let unread =
            |_: &str| -> Result<Value, String> { Ok(json!({"decisions": [], "damaged": 2})) };
        assert!(check(&doc, Some("x-1"), &unread)[1].starts_with("W\t2 damaged row(s)"));
    }
}

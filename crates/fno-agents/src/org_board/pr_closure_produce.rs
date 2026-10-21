//! The produce half of the `pr-closure-*` command family: branch-ids, ensure,
//! trailer, bind-created, and context. Each command reads one JSON payload on
//! stdin (trailer is argv-driven; bind-created takes its payload as one
//! JSON argv element, matching its CLI door) and answers with the exact
//! bytes the Python commands printed, pinned by `tests/golden/pr_closure_produce/`.
//!
//! Envelopes are hand-built strings, not `serde_json::json!`, because key
//! order is part of the parity contract (`json!` sorts keys; Python dicts
//! keep insertion order).

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::budget::{run_with_timeout_full, RunFailure};
use super::pr_closure::{parse as parse_closure, render as render_closure};
use crate::backlog::binding::{bind_pr_rows, PrRowBindResult};
use crate::backlog::node_ref::is_wellformed_node_id;
use crate::backlog::pr_link::{pr_number_from_url, repo_slug_from_url, stamp_ship_on_link};
use crate::backlog::settings::graph_path;
use crate::graph_get::external_backend_selected;
use crate::graph_store::{mutate_rows, read_rows_strict, StoreError};
use crate::node_branch::node_ids;

type Leg = fn(&serde_json::Value) -> (i32, String, String);

pub(crate) fn run(args: &[String]) -> i32 {
    let command = args.first().map(String::as_str).unwrap_or("");
    match command {
        "pr-closure-trailer" => leg_trailer(&args[1..]),
        "pr-closure-branch-ids" => run_case_command(command, leg_branch_ids),
        "pr-closure-ensure" => run_case_command(command, leg_ensure),
        "pr-closure-bind-created" => run_argv_command(
            command,
            leg_bind,
            args.get(1).map(String::as_str).unwrap_or(""),
        ),
        "pr-closure-context" => run_case_command(command, leg_context),
        _ => {
            eprint!(
                "usage: fno-agents pr-closure-<parse|render|branch-ids|ensure|trailer|bind-created|context>\n"
            );
            2
        }
    }
}

/// One JSON payload on stdin, one leg, streams printed verbatim. Every
/// golden stream ends in a newline, so the line-buffered writers flush.
fn run_case_command(command: &str, leg: Leg) -> i32 {
    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        eprint!("{command}: cannot read stdin\n");
        return 2;
    }
    let parsed: serde_json::Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprint!("{command}: stdin was not JSON: {e}\n");
            return 2;
        }
    };
    run_leg(leg, &parsed)
}

/// bind-created's door: its CLI caller forwards the flags as one JSON argv
/// element (the authorized-merge pattern), because the shared
/// `_forward_to_binary` passthrough inherits the caller's stdin and a TTY
/// there would hang a stdin read.
fn run_argv_command(command: &str, leg: Leg, payload: &str) -> i32 {
    let parsed: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            eprint!("{command}: argv payload was not JSON: {e}\n");
            return 2;
        }
    };
    run_leg(leg, &parsed)
}

fn run_leg(leg: Leg, parsed: &serde_json::Value) -> i32 {
    let (code, out, err) = leg(parsed);
    if !out.is_empty() {
        print!("{out}");
    }
    if !err.is_empty() {
        eprint!("{err}");
    }
    code
}

/// Python truthiness for a JSON value: only None, false, 0, "", [], and {}
/// are false.
fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// `json.dumps(..., ensure_ascii=True)` for one string: raw ASCII printable,
/// short escapes for the five control characters JSON names, `\\u00xx` for
/// the rest, and UTF-16 code units (surrogate pairs included) above BMP.
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            _ if (0x20..0x7f).contains(&(ch as u32)) => out.push(ch),
            _ => {
                let mut utf16 = [0u16; 2];
                for unit in ch.encode_utf16(&mut utf16) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
    out
}

fn str_array_json(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| escape_json_string(s)).collect();
    format!("[{}]", inner.join(","))
}

/// Pass a JSON value through to text the way `json.dumps` would: strings
/// re-escaped, everything else rendered by serde (null, numbers, bools).
fn json_raw(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => escape_json_string(s),
        other => other.to_string(),
    }
}

fn refused(refusal: Option<String>) -> PrRowBindResult {
    PrRowBindResult {
        outcome: "refused",
        bindings: Vec::new(),
        refusal,
    }
}

fn bind_envelope(result: &PrRowBindResult, claimed: &[String]) -> (i32, String, String) {
    let refusal = match &result.refusal {
        Some(r) => escape_json_string(r),
        None => "null".to_string(),
    };
    let out = format!(
        "{{\"outcome\":\"{}\",\"claimed_ids\":{},\"refusal\":{}}}\n",
        result.outcome,
        str_array_json(claimed),
        refusal
    );
    let code = if result.outcome == "bound" { 0 } else { 1 };
    (code, out, String::new())
}

/// The known-ids read behind `verify` and the ensure candidates: empty
/// without the graph backend (never a read), strict otherwise.
fn known_node_ids() -> Result<HashSet<String>, StoreError> {
    if external_backend_selected() {
        return Ok(HashSet::new());
    }
    let rows = read_rows_strict(&graph_path())?;
    Ok(rows
        .iter()
        .filter_map(|e| e.get("id"))
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect())
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn read_current_branch(cwd: &Path) -> Result<String, String> {
    let cmd = vec![
        "git".to_string(),
        "branch".to_string(),
        "--show-current".to_string(),
    ];
    match run_with_timeout_full(&cmd, cwd, Duration::from_secs(5)) {
        Ok(out) => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        Err(f) => Err(format!("branch lookup failed: {}", f.message())),
    }
}

fn resolve_branch_node_id(known: &HashSet<String>, cwd: &Path) -> Result<String, String> {
    let head_ref = read_current_branch(cwd)?;
    if head_ref.is_empty() {
        return Err("current branch is unknown (detached HEAD?)".to_string());
    }
    let mut real: Vec<String> = Vec::new();
    for id in node_ids(&head_ref) {
        if known.contains(&id) && !real.iter().any(|r| r == &id) {
            real.push(id);
        }
    }
    if real.len() != 1 {
        let named = if real.is_empty() {
            String::new()
        } else {
            format!(" ({})", real.join(", "))
        };
        return Err(format!(
            "branch '{}' names {} real node(s){}; bare resolution needs exactly one - pass the node explicitly instead",
            head_ref,
            real.len(),
            named
        ));
    }
    Ok(real.remove(0))
}

/// The bind decision on a snapshot: URL scope, claim matching against the
/// row ids, and the probe bind. Returns the matched claim ids alongside so
/// the envelope can carry them on every path, refusals included.
fn bind_created_pr(
    entries: &mut [serde_json::Value],
    head_ref: &str,
    url: &str,
    node_id: Option<&str>,
) -> (PrRowBindResult, Vec<String>) {
    let pr_number = pr_number_from_url(Some(url));
    let repo = repo_slug_from_url(Some(url));
    if pr_number.is_none() || repo.is_none() {
        return (
            refused(Some("created PR URL is malformed or unscoped".to_string())),
            Vec::new(),
        );
    }
    let real_ids: HashSet<String> = entries
        .iter()
        .filter_map(|e| e.get("id"))
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect();
    let trimmed = node_id.map(str::trim).filter(|s| !s.is_empty());
    let mut matched: Vec<String> = Vec::new();
    match trimmed.filter(|s| real_ids.contains(*s)) {
        Some(auth) => matched.push(auth.to_string()),
        None => {
            for id in node_ids(head_ref) {
                if real_ids.contains(&id) && !matched.iter().any(|m| m == &id) {
                    matched.push(id);
                }
            }
        }
    }
    if matched.len() != 1 {
        let refusal = if matched.is_empty() {
            "branch does not resolve to exactly one real node".to_string()
        } else {
            format!("branch ambiguously names {} real nodes", matched.len())
        };
        return (refused(Some(refusal)), matched);
    }
    let result = bind_pr_rows(
        entries,
        &matched,
        pr_number.unwrap(),
        Some(url),
        repo.as_deref(),
        false,
    );
    (result, matched)
}

fn leg_branch_ids(payload: &serde_json::Value) -> (i32, String, String) {
    let head_ref = payload
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let ids = node_ids(head_ref);
    if !truthy(payload.get("verify").unwrap_or(&serde_json::Value::Null)) {
        return (
            0,
            format!("{{\"ids\":{}}}\n", str_array_json(&ids)),
            String::new(),
        );
    }
    let known = match known_node_ids() {
        Ok(k) => k,
        Err(e) => {
            return (
                4,
                String::new(),
                format!("pr-closure-branch-ids: the graph read failed ({e})\n"),
            )
        }
    };
    let real: Vec<String> = ids.iter().filter(|i| known.contains(*i)).cloned().collect();
    (
        0,
        format!(
            "{{\"ids\":{},\"real\":{}}}\n",
            str_array_json(&ids),
            str_array_json(&real)
        ),
        String::new(),
    )
}

fn leg_ensure(payload: &serde_json::Value) -> (i32, String, String) {
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let head_ref = payload
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let extra_ids: Vec<String> = payload
        .get("extra_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let candidates = node_ids(head_ref);
    let known = if candidates.is_empty() {
        Ok(HashSet::new())
    } else {
        known_node_ids()
    };
    let known = match known {
        Ok(k) => k,
        Err(e) => {
            return (
                4,
                String::new(),
                format!("pr-closure-ensure: the graph read failed ({e})\n"),
            )
        }
    };
    let mut wanted: Vec<String> = Vec::new();
    let mut push_wanted = |id: &str| {
        if !wanted.iter().any(|w| w == id) {
            wanted.push(id.to_string());
        }
    };
    for id in candidates.iter().filter(|c| known.contains(*c)) {
        push_wanted(id);
    }
    for id in extra_ids.iter().filter(|e| is_wellformed_node_id(e)) {
        push_wanted(id);
    }
    let new_body = if wanted.is_empty() {
        body.to_string()
    } else {
        let claimed = parse_closure(body);
        if wanted.iter().all(|w| claimed.iter().any(|c| c == w)) {
            body.to_string()
        } else {
            let mut line_input = claimed.clone();
            line_input.extend(wanted.iter().cloned());
            let line = render_closure(&line_input);
            if line.is_empty() {
                body.to_string()
            } else if !body.trim().is_empty() {
                format!("{}\n\n{}\n", body.trim_end(), line)
            } else {
                format!("{line}\n")
            }
        }
    };
    (
        0,
        format!("{{\"body\":{}}}\n", escape_json_string(&new_body)),
        String::new(),
    )
}

fn leg_bind(payload: &serde_json::Value) -> (i32, String, String) {
    if external_backend_selected() {
        return bind_envelope(
            &refused(Some("graph backend is not active".to_string())),
            &[],
        );
    }
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let node = payload.get("node").and_then(|v| v.as_str());
    let trimmed = node.map(str::trim).filter(|s| !s.is_empty());
    // The CLI door advertises --repo; an empty or absent key keeps the
    // process cwd (the historical behavior every existing caller relies on).
    let work_dir = match payload
        .get("repo")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(repo) => PathBuf::from(repo),
        None => current_dir(),
    };
    let head_ref = if trimmed.is_none() {
        match read_current_branch(&work_dir) {
            Ok(b) => b,
            Err(e) => return bind_envelope(&refused(Some(e)), &[]),
        }
    } else {
        String::new()
    };
    if head_ref.is_empty() && trimmed.is_none() {
        return bind_envelope(&refused(Some("current branch is unknown".to_string())), &[]);
    }
    let path = graph_path();
    let mut rows = match read_rows_strict(&path) {
        Ok(r) => r,
        Err(e) => return bind_envelope(&refused(Some(format!("graph read failed: {e}"))), &[]),
    };
    let (probe, matched) = bind_created_pr(&mut rows, &head_ref, url, node);
    if probe.outcome != "bound" {
        return bind_envelope(&probe, &matched);
    }
    let pr_number = pr_number_from_url(Some(url)).unwrap();
    let repo = repo_slug_from_url(Some(url));
    let mut boxed: Option<PrRowBindResult> = None;
    let live = mutate_rows(&path, Duration::from_secs(30), None, None, |entries| {
        boxed = Some(bind_pr_rows(
            entries,
            &matched,
            pr_number,
            Some(url),
            repo.as_deref(),
            false,
        ));
        Ok(true)
    });
    let bound = match live {
        Ok(_) => boxed.unwrap_or_else(|| refused(Some("the graph write failed".to_string()))),
        Err(e) => refused(Some(format!("the graph write failed: {e}"))),
    };
    if bound.outcome == "bound" {
        for id in bound.bound_ids() {
            stamp_ship_on_link(&path, &id);
        }
    }
    bind_envelope(&bound, &matched)
}

fn leg_trailer(argv: &[String]) -> i32 {
    let mut node: Option<&str> = None;
    let mut extra: Vec<String> = Vec::new();
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        if arg == "--extra" {
            if let Some(v) = it.next() {
                extra.push(v.clone());
            }
        } else if node.is_none() {
            node = Some(arg.as_str());
        }
    }
    let external = external_backend_selected();
    if node.is_none() && external {
        eprint!("closure-trailer: branch resolution needs the graph backend; pass the node explicitly instead\n");
        return 1;
    }
    if node.is_some() && external {
        return 0;
    }
    let path = graph_path();
    let entries = match read_rows_strict(&path) {
        Ok(e) => e,
        Err(e) => {
            eprint!("closure-trailer: the graph read failed ({e}); exit 4 names a dead reader, not an unresolvable node\n");
            return 4;
        }
    };
    let node_id: String = match node {
        None => {
            let known: HashSet<String> = entries
                .iter()
                .filter_map(|e| e.get("id"))
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect();
            match resolve_branch_node_id(&known, &current_dir()) {
                Ok(n) => n,
                Err(e) => {
                    eprint!("closure-trailer: {e}\n");
                    return 1;
                }
            }
        }
        Some(n) => n.to_string(),
    };
    let dropped: Vec<String> = extra
        .iter()
        .filter(|e| !is_wellformed_node_id(e))
        .cloned()
        .collect();
    if !dropped.is_empty() {
        eprint!(
            "warning: dropping malformed --extra id(s) from the trailer: {} (need the full <prefix>-<hex> form)\n",
            dropped.join(", ")
        );
    }
    let mut seed: Vec<String> = Vec::new();
    if is_wellformed_node_id(&node_id) {
        seed.push(node_id.clone());
        seed.extend(contained_descendant_ids(&entries, &node_id));
    }
    seed.extend(extra.iter().filter(|e| is_wellformed_node_id(e)).cloned());
    let line = render_closure(&seed);
    if !line.is_empty() {
        print!("{line}\n");
    }
    0
}

fn contained_descendant_ids(entries: &[serde_json::Value], node_id: &str) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.get("contained_in").and_then(|v| v.as_str()) == Some(node_id))
        .filter_map(|e| e.get("id"))
        .filter_map(|v| v.as_str())
        .map(str::to_string)
        .collect()
}

fn find_gh_on_path() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("gh");
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// The gh lookup is a live-process surface with no goldens; the parity test
/// pins it with a stub gh script instead of a byte contract.
fn leg_context(payload: &serde_json::Value) -> (i32, String, String) {
    let pr_value = payload
        .get("pr")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let pr_str = match &pr_value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    };
    let repo = payload.get("repo").and_then(|v| v.as_str());
    let cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
    if !cwd.is_empty() {
        let _ = std::env::set_current_dir(cwd);
    }
    let gh = match find_gh_on_path() {
        Some(p) => p,
        None => {
            return (
                3,
                String::new(),
                "pr-closure-context: gh CLI not found on PATH\n".to_string(),
            )
        }
    };
    let mut cmd: Vec<String> = vec![gh, "pr".to_string(), "view".to_string(), pr_str.clone()];
    if let Some(r) = repo {
        cmd.push("--repo".to_string());
        cmd.push(r.to_string());
    }
    cmd.push("--json".to_string());
    cmd.push("number,body,url,state,mergedAt,files".to_string());
    let out = match run_with_timeout_full(&cmd, Path::new("."), Duration::from_secs_f64(30.0)) {
        Ok(o) => o,
        Err(RunFailure::KilledAtSlice(_)) => {
            return (
                3,
                String::new(),
                format!("pr-closure-context: gh pr view #{pr_str} timed out after 30.0s\n"),
            )
        }
        Err(RunFailure::Failed(msg)) => {
            if let Some(rest) = msg.strip_prefix("exit ") {
                if let Some((rc, detail)) = rest.split_once(": ") {
                    return (
                        3,
                        String::new(),
                        format!(
                            "pr-closure-context: gh pr view #{pr_str} failed (rc={rc}): {detail}\n"
                        ),
                    );
                }
            }
            return (
                3,
                String::new(),
                format!("pr-closure-context: gh subprocess failed to launch: {msg}\n"),
            );
        }
    };
    if String::from_utf8_lossy(&out.stdout).trim().is_empty() {
        return (
            3,
            String::new(),
            format!("pr-closure-context: gh pr view #{pr_str} returned no output (exit 0)\n"),
        );
    }
    let row: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(e) => {
            return (
                3,
                String::new(),
                format!("pr-closure-context: gh stdout was not JSON: {e}\n"),
            )
        }
    };
    let number_json = match row.get("number") {
        Some(v) => json_raw(v),
        None => {
            let display = match &pr_value {
                serde_json::Value::Number(n) => n
                    .as_i64()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| n.to_string()),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            escape_json_string(&display)
        }
    };
    let body_json = match row.get("body") {
        Some(serde_json::Value::String(s)) if !s.is_empty() => escape_json_string(s),
        _ => "\"\"".to_string(),
    };
    let url_json = row
        .get("url")
        .map(json_raw)
        .unwrap_or_else(|| "null".to_string());
    let state_json = match row.get("state") {
        Some(v) => json_raw(v),
        None => "\"UNKNOWN\"".to_string(),
    };
    let merged_json = row
        .get("mergedAt")
        .map(json_raw)
        .unwrap_or_else(|| "null".to_string());
    let mut files: Vec<String> = Vec::new();
    if let Some(arr) = row.get("files").and_then(|v| v.as_array()) {
        for item in arr {
            let s: String = match item {
                serde_json::Value::Object(o) => o
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string(),
                serde_json::Value::String(s) => s.clone(),
                // The Python leg keeps only str and dict items; a bare
                // number or bool entry drops rather than stringifying.
                _ => String::new(),
            };
            if !s.is_empty() {
                files.push(s);
            }
        }
    }
    let out = format!(
        "{{\"number\":{},\"body\":{},\"url\":{},\"state\":{},\"merged_at\":{},\"changed_files\":{}}}\n",
        number_json,
        body_json,
        url_json,
        state_json,
        merged_json,
        str_array_json(&files)
    );
    (0, out, String::new())
}

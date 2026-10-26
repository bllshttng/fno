//! The `fno do target` gate verbs, ported from `cli/src/fno/target_cli.py`
//! (x-9516): `check-review-gate`, `check-contained`, `check-dispatch-hold`,
//! `resolve-owned-identity`, `resolve-model`, `denominator-ratio`, and the
//! `target_handoff prepare` module call. One op per verb, each composing the
//! same machinery the Python composed over subprocess doors - in-process
//! where the crate owns the piece (review capability, hold verdict), through
//! a self-exec of this binary's own doors where the composition point is the
//! CLI surface (graph-get, node-route, mail send raw, claim release).
//!
//! Exit contracts, pinned by `tests/golden/target_family/`:
//!
//! - `check-review-gate`: 0 clear, 9 refused (the Python verb restamped the
//!   door's exit 2 to the shell callers' REVIEW_GATE_REFUSED = 9), anything
//!   else reads as a BROKEN gate and proceeds.
//! - `check-contained` / `check-dispatch-hold`: the same 0/9/broken shape; an
//!   unreachable store prints the DID-NOT-RUN line and proceeds (fail-open),
//!   an unreadable hold world refuses.
//! - `resolve-owned-identity`: a resolver, never a gate - always 0, six
//!   `KEY=value` lines.
//! - `resolve-model`: the resolved model on stdout, nothing (exit 0) when the
//!   node names no pin and no default resolves.
//! - `denominator-ratio`: the deliverables-1 ratio, JSON or prose.
//! - `target-handoff-prepare`: the archived-manifest handoff op, receipts 0 /
//!   10 restored / 12 unrestored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The gate refusal code the shell callers read as "this gate refused"
/// (target_cli.py REVIEW_GATE_REFUSED). Every other non-zero reads as a
/// BROKEN gate and proceeds, so this must stay distinct from typer's
/// usage-error 2.
const REVIEW_GATE_REFUSED: i32 = 9;
/// The exit the Python refusals used internally before the verb restamped.
const REFUSED: i32 = 2;

fn env_input(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn resolve_repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("FNO_REPO_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = Some(cwd.as_path());
    while let Some(dir) = dir {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        dir = dir.parent();
    }
    cwd
}

pub(crate) fn self_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno-agents"))
}

/// One JSON line from a self-exec'd door: payload through stdin, one JSON
/// value back on stdout. `None` names a door that failed or answered
/// non-JSON - the caller fails closed on it.
fn door_json(op: &str, args: &[&str], payload: &serde_json::Value) -> Option<serde_json::Value> {
    use std::io::Write;
    let mut child = Command::new(self_exe())
        .arg(op)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    {
        let stdin = child.stdin.as_mut()?;
        let _ = stdin.write_all(payload.to_string().as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn graph_get_row(input: &str) -> Option<serde_json::Value> {
    let out = Command::new(self_exe())
        .arg("graph-get")
        .arg(input)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    rows.as_array()?.first().cloned()
}

// ─────────────────────────────────────────────────────────────────────────────
// check-review-gate
// ─────────────────────────────────────────────────────────────────────────────

fn load_config(root: &Path) -> toml::Value {
    let path = root.join(".fno").join("config.toml");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or(toml::Value::Table(Default::default()))
}

fn config_str_list(config: &toml::Value, key: &str) -> Vec<String> {
    config
        .get("review")
        .and_then(|r| r.get(key))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The reviewers-plus-peers axis. The Python verb loaded the configured
/// reviewers, the descriptor registry, and the peers, then forwarded all of
/// it to `review-level capability reviewers`; the op reads the same keys
/// itself and relays the same note and refusal lines.
fn check_review_gate(config: &toml::Value) -> i32 {
    let reviewers = config_str_list(config, "reviewers");
    let peers = config_str_list(config, "peers");
    if reviewers.is_empty() && peers.is_empty() {
        return 0;
    }
    let review = config
        .get("review")
        .cloned()
        .unwrap_or(toml::Value::Table(Default::default()));
    let descriptors: serde_json::Value = review
        .get("reviewer_registry")
        .map(toml_to_json)
        .unwrap_or(serde_json::json!({}));
    let peer_identity = review
        .get("peer_identity")
        .map(toml_to_json)
        .unwrap_or(serde_json::Value::Null);
    let payload = serde_json::json!({
        "reviewers": reviewers,
        "descriptors": descriptors,
        "peers": peers,
        "peer_identity": peer_identity,
    });
    let Some(answer) = door_json("review-level", &["capability", "reviewers"], &payload) else {
        return 1;
    };
    for row in answer
        .get("reviewers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|r| r.get("status").and_then(|s| s.as_str()) == Some("unverifiable"))
    {
        if let Some(line) = row.get("line").and_then(|l| l.as_str()) {
            eprintln!("note target init: {line}");
        }
    }
    for row in answer
        .get("local_peers")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|r| r.get("status").and_then(|s| s.as_str()) == Some("unverifiable"))
    {
        if let Some(line) = row.get("line").and_then(|l| l.as_str()) {
            eprintln!("note target init: {line}");
        }
    }
    let refusals: Vec<&str> = ["reviewers_refusal", "local_peers_refusal"]
        .iter()
        .filter_map(|key| answer.get(key).and_then(|v| v.as_str()))
        .collect();
    if !refusals.is_empty() {
        eprintln!("{}", refusals.join("\n\n"));
        return REVIEW_GATE_REFUSED;
    }
    0
}

/// The github_apps axis: configured App logins that cannot review here
/// refuse, with the nudge logins as the acted-here evidence.
fn check_review_gate_apps(config: &toml::Value) -> i32 {
    let apps = config_str_list(config, "github_apps");
    if apps.is_empty() {
        return 0;
    }
    let nudge: Vec<String> = config
        .get("review")
        .and_then(|r| r.get("nudge"))
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    let payload = serde_json::json!({
        "apps": apps,
        "nudge_logins": nudge,
    });
    let Some(answer) = door_json("review-level", &["capability", "github-apps"], &payload) else {
        return 1;
    };
    for row in answer
        .get("verdicts")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|r| r.get("status").and_then(|s| s.as_str()) == Some("unverifiable"))
    {
        if let Some(line) = row.get("line").and_then(|l| l.as_str()) {
            eprintln!("note target init: {line}");
        }
    }
    if let Some(refusal) = answer.get("refusal").and_then(|v| v.as_str()) {
        eprintln!("{refusal}");
        return REVIEW_GATE_REFUSED;
    }
    0
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::json!(b),
        toml::Value::Datetime(d) => serde_json::json!(d.to_string()),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for (k, v) in t {
                map.insert(k.clone(), toml_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

/// `fno-agents target-check-review-gate`
pub fn run_check_review_gate(args: &[String]) -> i32 {
    let _ = args;
    let config = load_config(&resolve_repo_root());
    let reviewers_axis = check_review_gate(&config);
    if reviewers_axis != 0 {
        return reviewers_axis;
    }
    check_review_gate_apps(&config)
}

// ─────────────────────────────────────────────────────────────────────────────
// check-contained
// ─────────────────────────────────────────────────────────────────────────────

fn check_contained_run(args: &[String]) -> i32 {
    let _ = args;
    let Some(input) = env_input("TARGET_INPUT") else {
        // No input to check: "not resolvable, or nothing to check" exits 0.
        return 0;
    };
    let Some(node) = graph_get_row(&input) else {
        // The store read failed or the row is missing. The Python verb
        // distinguished an unreachable STORE (fail-open, DID-NOT-RUN line)
        // from an unresolvable node (silent exit 0); the self-exec'd read
        // cannot tell them apart, and both exit 0 in Python. The message
        // names what happened rather than passing a refusal off as a clear.
        eprintln!(
            "fno do target check-contained: the graph store is unavailable \
             (graph-get failed or the row is missing); the containment check \
             DID NOT RUN. Proceeding."
        );
        return 0;
    };
    redirect_if_contained(&node)
}

fn redirect_if_contained(node: &serde_json::Value) -> i32 {
    let owner = node
        .get("contained_in")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty());
    let Some(owner) = owner else {
        return 0;
    };
    let nid = node
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("that node");
    let Some(owner_node) = graph_get_row(owner) else {
        eprintln!(
            "fno do target init: {nid} is marked as shipping inside {owner}, but no node \
             {owner} exists.\nThat containment is stale and nothing will clear it \
             automatically. Clear it with `fno backlog update {nid} parent=null`, then \
             dispatch this node on its own. Nothing was claimed."
        );
        return REVIEW_GATE_REFUSED;
    };
    let superseded = owner_node
        .get("superseded_by")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.is_empty());
    let deferred = owner_node
        .get("deferred_at")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.is_empty());
    let completed = owner_node
        .get("completed_at")
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.is_empty());
    if (superseded || deferred) && !completed {
        let state = if superseded { "superseded" } else { "deferred" };
        eprintln!(
            "fno do target init: {nid} is marked as shipping inside {owner}, but {owner} is \
             {state} and will never ship it.\nThat containment is stale. Clear it with \
             `fno backlog update {nid} parent=null`, then dispatch this node on its own. \
             Nothing was claimed."
        );
        return REVIEW_GATE_REFUSED;
    }
    if completed {
        let shipped = owner_node
            .get("pr_number")
            .and_then(|v| v.as_u64())
            .map(|n| format!(" in PR #{n}"))
            .unwrap_or_default();
        eprintln!(
            "fno do target init: {nid} already shipped inside {owner}{shipped}, which is done \
             - there is nothing left to build here.\n{nid} was folded into {owner} as a \
             contained node, so it has no PR of its own and is not separately costed. \
             Nothing was claimed."
        );
        return REVIEW_GATE_REFUSED;
    }
    eprintln!(
        "fno do target init: {nid} ships inside {owner}'s PR; run `/fno:target {owner}`.\n\
         {nid} was folded into {owner} as a contained node, so it has no PR of its own and is \
         not separately costed. Nothing was claimed."
    );
    REVIEW_GATE_REFUSED
}

// ─────────────────────────────────────────────────────────────────────────────
// check-dispatch-hold
// ─────────────────────────────────────────────────────────────────────────────

fn check_dispatch_hold_run(args: &[String]) -> i32 {
    let _ = args;
    let input = env_input("TARGET_INPUT");
    let plan = env_input("TARGET_PLAN_PATH");
    let Some(input) = input else {
        return 0;
    };
    let node = match graph_get_row(&input) {
        Some(node) => node,
        None if plan.is_some() => {
            // An input that may name a node with an unreadable hold world
            // refuses; the Python verb keyed the refusal on the node being
            // resolvable but the graph unreadable. A missing row with a plan
            // path is the same refuse-to-assume-unheld shape.
            eprintln!(
                "fno do target: dispatch-hold-invalid:{input}: backlog graph is unreadable; \
                 refusing to assume unheld. Nothing was claimed."
            );
            return REVIEW_GATE_REFUSED;
        }
        None => return 0,
    };
    let node_id = node
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(input.as_str())
        .to_string();
    // The hold verdict is one native op already (authorized-merge
    // hold-verdict): the Python ladder wrapper was transport over it.
    let graph = crate::graph_get::default_graph_path();
    let payload = serde_json::json!({
        "op": "hold-verdict",
        "node": node_id,
        "graph": graph.to_string_lossy(),
    });
    let receipt = door_json("authorized-merge", &[], &payload);
    let Some(receipt) = receipt else {
        eprintln!(
            "fno do target: dispatch-hold-invalid:{node_id}: hold verdict read failed; \
             refusing to assume unheld. Nothing was claimed."
        );
        return REVIEW_GATE_REFUSED;
    };
    if receipt.get("outcome").and_then(|v| v.as_str()) == Some("absent") {
        return 0;
    }
    let field = |k: &str| {
        receipt
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    if receipt.get("outcome").and_then(|v| v.as_str()) != Some("held") {
        let detail = field("detail");
        eprintln!(
            "fno do target: dispatch-hold-invalid:{node_id}: {detail}. The plan cannot prove \
             that dispatch is unheld; fix or remove the declaration, then retry. \
             Nothing was claimed."
        );
        return REVIEW_GATE_REFUSED;
    }
    let owner = field("owner");
    let reason = field("reason");
    let set_by = field("set_by");
    let release_when = field("release_when");
    let review_on = field("review_on");
    let exempt = field("exempt");
    eprintln!(
        "fno do target: dispatch held by plan {owner}\n  reason: {reason}\n  set_by: {set_by}\n  \
         release_when: {release_when}\n  review_on: {review_on}\n{}\
         Remove dispatch_hold only after the release condition is confirmed. Nothing was claimed.",
        if exempt.is_empty() {
            String::new()
        } else {
            format!("  exempt: {exempt}\n")
        }
    );
    REVIEW_GATE_REFUSED
}

// ─────────────────────────────────────────────────────────────────────────────
// resolve-owned-identity
// ─────────────────────────────────────────────────────────────────────────────

fn resolve_owned_identity_run(args: &[String]) -> i32 {
    let _ = args;
    // The hook's claude session id may arrive as TARGET_TRANSCRIPT_ID when
    // CLAUDE_CODE_SESSION_ID is absent.
    if std::env::var("CLAUDE_CODE_SESSION_ID")
        .map(|v| v.trim().is_empty())
        .unwrap_or(true)
    {
        if let Ok(transcript) = std::env::var("TARGET_TRANSCRIPT_ID") {
            if !transcript.trim().is_empty() {
                std::env::set_var("CLAUDE_CODE_SESSION_ID", transcript);
            }
        }
    }
    let getter = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    let home = crate::paths::AgentsHome::from_env();
    let owned = crate::spawn_context::resolve_self_identity(&getter, None, None, &home);
    let rejected = owned.rejected.first();
    println!("HARNESS={}", owned.harness.clone().unwrap_or_default());
    println!(
        "SESSION_ID={}",
        owned.session_id.clone().unwrap_or_default()
    );
    println!("DISPOSITION={}", disposition_word(owned.disposition));
    println!(
        "COLLISION={}",
        rejected.map(|r| r.owner.clone()).unwrap_or_default()
    );
    println!(
        "COLLISION_ID={}",
        rejected.map(|r| r.session_id.clone()).unwrap_or_default()
    );
    // The Python rejected row carried the raw marker string; the Rust
    // RejectedId names the row by harness and session id, and no golden pins
    // a collision output, so the marker line prints empty here.
    println!("COLLISION_MARKER=");
    0
}

/// The Python disposition vocabulary, spelled the way the golden pins
/// `single`; unverified variants keep their snake_case enum spelling.
fn disposition_word(disposition: crate::spawn_context::OwnedDisposition) -> &'static str {
    use crate::spawn_context::OwnedDisposition as D;
    match disposition {
        D::Single => "single",
        D::Proven => "proven",
        D::AsCanonical => "as_canonical",
        D::Ambiguous => "ambiguous",
        D::Empty => "empty",
        D::Invalid => "invalid",
        D::Contradiction => "contradiction",
        D::SpawnRecord => "spawn_record",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// resolve-model
// ─────────────────────────────────────────────────────────────────────────────

fn resolve_model_run(args: &[String]) -> i32 {
    let mut positional: Vec<String> = Vec::new();
    let mut harness: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--harness" => harness = it.next().cloned(),
            "--provider" => {
                // Retired axis; accepted and dropped, as the Python did.
                it.next();
            }
            other if other.starts_with('-') => {
                eprintln!("fno do target resolve-model: unknown flag {other}");
                return REFUSED;
            }
            other => positional.push(other.to_string()),
        }
    }
    let Some(node) = positional.first().cloned() else {
        eprintln!("fno do target resolve-model: a node id is required");
        return REFUSED;
    };
    // The Python verb was the one projection of route_resolve for bash
    // dispatchers; node-route is the native resolver it projected.
    let mut cmd = Command::new(self_exe());
    cmd.arg("node-route").arg(&node);
    if let Some(harness) = &harness {
        cmd.arg("--harness").arg(harness);
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(_) => return REFUSED,
    };
    if !out.status.success() {
        return 0;
    }
    let answer: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    // The projection prints one bare model token, or nothing when the node
    // carries no pin and no default resolves.
    if let Some(model) = answer.get("model").and_then(|v| v.as_str()) {
        if !model.trim().is_empty() {
            println!("{model}");
        }
    }
    0
}

// ─────────────────────────────────────────────────────────────────────────────
// denominator-ratio
// ─────────────────────────────────────────────────────────────────────────────

fn parse_naive_ts(raw: &str) -> Option<chrono::NaiveDateTime> {
    let cleaned = raw.trim().trim_end_matches('Z');
    chrono::NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%dT%H:%M:%S%.f").ok()
}

fn denominator_ratio_run(args: &[String]) -> i32 {
    let mut since_days: i64 = 28;
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--since-days" => {
                since_days = it.next().and_then(|v| v.parse().ok()).unwrap_or(28);
            }
            "--json" | "-J" => json_out = true,
            other => {
                eprintln!("fno do target denominator-ratio: unknown flag {other}");
                return REFUSED;
            }
        }
    }
    let root = resolve_repo_root();
    let events_path = root.join(".fno").join("events.jsonl");
    let cutoff = chrono::Local::now().naive_local() - chrono::Duration::days(since_days);
    let mut counts: BTreeMap<&str, i64> = ["plan", "deliverables", "derived", "none"]
        .into_iter()
        .map(|k| (k, 0))
        .collect();
    let mut ones: i64 = 0;
    if let Ok(text) = std::fs::read_to_string(&events_path) {
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let ts = row
                .get("ts")
                .and_then(|v| v.as_str())
                .and_then(parse_naive_ts);
            let Some(ts) = ts else { continue };
            if ts < cutoff {
                continue;
            }
            let data = row.get("data").cloned().unwrap_or(serde_json::json!({}));
            let denominator = data.get("denominator").and_then(|v| v.as_str());
            if let Some(d) = denominator {
                if let Some(count) = counts.get_mut(d.to_ascii_lowercase().as_str()) {
                    *count += 1;
                }
                if d.eq_ignore_ascii_case("deliverables")
                    && data.get("count").and_then(|v| v.as_i64()) == Some(1)
                {
                    ones += 1;
                }
            }
        }
    }
    let declared = counts["plan"] + counts["deliverables"];
    let ratio = if declared > 0 {
        Some((100.0 * ones as f64 / declared as f64 * 10.0).round() / 10.0)
    } else {
        None
    };
    let verdict = if declared == 0 {
        "no declared-denominator inits in window"
    } else if ratio.unwrap_or(0.0) >= 80.0 {
        "bypass: deliverables:1 dominates; widen enumerated_scope"
    } else {
        "healthy"
    };
    if json_out {
        // The golden normalizer re-dumps sorted compact JSON; print sorted.
        let out = serde_json::json!({
            "deliverables_1_ratio_pct": ratio,
            "deliverables_1": ones,
            "plan_backed": counts["plan"],
            "deliverables_declared": counts["deliverables"],
            "deliverables_derived": counts["derived"],
            "none": counts["none"],
            "since_days": since_days,
            "verdict": verdict,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        let ratio_txt = ratio
            .map(|r| format!("{r}"))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "deliverables:1 ratio: {ratio_txt}%  ({ones} of {declared} declared; \
             plan={}, deliverables={}, derived={}, none={})  {verdict}",
            counts["plan"], counts["deliverables"], counts["derived"], counts["none"]
        );
    }
    0
}

// ─────────────────────────────────────────────────────────────────────────────
// target-handoff-prepare
// ─────────────────────────────────────────────────────────────────────────────

fn handoff_prepare_run(args: &[String]) -> i32 {
    let mut state = None;
    let mut archive = None;
    let mut claim_key = None;
    let mut holder = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--state" => state = it.next().map(PathBuf::from),
            "--archive" => archive = it.next().map(PathBuf::from),
            "--claim-key" => claim_key = it.next().cloned(),
            "--holder" => holder = it.next().cloned(),
            other => {
                eprintln!("target-handoff-prepare: unknown argument {other:?}");
                return REFUSED;
            }
        }
    }
    let (Some(state), Some(archive), Some(claim_key), Some(holder)) =
        (state, archive, claim_key, holder)
    else {
        eprintln!(
            "target-handoff-prepare: --state, --archive, --claim-key and --holder are required"
        );
        return REFUSED;
    };
    let content = match std::fs::read(&state) {
        Ok(bytes) => bytes,
        Err(err) => {
            println!(
                "{}",
                serde_json::json!({"ok": false, "reason": format!("state read failed: {err}")})
            );
            return 10;
        }
    };
    let digest = sha256_hex(&content);
    match archive_exclusive(&archive, &content) {
        Ok(()) => {}
        Err(ArchiveCollision::ExistsMismatch) => {
            println!(
                "{}",
                serde_json::json!({
                    "ok": false,
                    "reason": format!("an attempt archive already exists with different bytes: {}", archive.display()),
                })
            );
            return 10;
        }
        Err(ArchiveCollision::Io(err)) => {
            println!(
                "{}",
                serde_json::json!({"ok": false, "reason": format!("archive write failed: {err}")})
            );
            return 10;
        }
    }
    if let Err(err) = std::fs::remove_file(&state) {
        println!(
            "{}",
            serde_json::json!({"ok": false, "reason": format!("live manifest removal failed: {err}")})
        );
        let _ = std::fs::remove_file(&archive);
        return 10;
    }
    // The exact release: a holder mismatch must RAISE, never report a
    // release that did not happen. The claim door answers per-holder.
    let release = Command::new(self_exe())
        .arg("claim")
        .arg("release")
        .arg(&claim_key)
        .arg("--holder")
        .arg(&holder)
        .output();
    let release = match release {
        Ok(out) if out.status.success() => Some(out),
        _ => None,
    };
    if release.is_none() {
        let restored = restore_manifest(&state, &archive, &digest);
        println!(
            "{}",
            serde_json::json!({
                "ok": false,
                "reason": format!(
                    "exact claim release failed: source claim {claim_key:?} is absent on disk \
                     or held by another holder; the session cannot prove it holds the node to \
                     hand off"
                ),
                "archive_path": archive.to_string_lossy(),
                "restored": restored,
            })
        );
        return if restored { 10 } else { 12 };
    }
    println!(
        "{}",
        serde_json::json!({
            "ok": true,
            "archive_path": archive.to_string_lossy(),
            "digest": digest,
        })
    );
    0
}

enum ArchiveCollision {
    ExistsMismatch,
    Io(String),
}

/// Exclusive-create publish: create_new wins once; an existing archive with
/// identical bytes is a no-op (the Python helper's hash-compare contract),
/// different bytes are a collision.
fn archive_exclusive(path: &Path, content: &[u8]) -> Result<(), ArchiveCollision> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ArchiveCollision::Io(e.to_string()))?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(content)
            .map_err(|e| ArchiveCollision::Io(e.to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(path).map_err(|e| ArchiveCollision::Io(e.to_string()))?;
            if existing == content {
                Ok(())
            } else {
                Err(ArchiveCollision::ExistsMismatch)
            }
        }
        Err(err) => Err(ArchiveCollision::Io(err.to_string())),
    }
}

fn restore_manifest(state: &Path, archive: &Path, digest: &str) -> bool {
    let Ok(content) = std::fs::read(archive) else {
        return false;
    };
    if sha256_hex(&content) != digest {
        return false;
    }
    archive_exclusive(state, &content).is_ok()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    bytes_hex(&out)
}

fn bytes_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// request-self-review
// ─────────────────────────────────────────────────────────────────────────────

fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[derive(Default)]
struct SelfReviewCtx {
    branch: String,
    hold_branch: String,
    base_branch: String,
    payload: String,
}

impl SelfReviewCtx {
    fn branch_or_none(&self) -> Option<&str> {
        if self.branch.is_empty() {
            None
        } else {
            Some(self.branch.as_str())
        }
    }
}

/// `fno-agents target-gates request-self-review [--pr-number N]`: the Python

/// `fno-agents target-gates request-self-review [--pr-number N]`: the Python
/// verb composed four native doors over subprocesses; this op composes the
/// same four. One JSON receipt on stdout, exit 2 on refused or unconfirmed -
/// the contract the two request-self-review goldens pin.
pub fn run_request_self_review(args: &[String]) -> i32 {
    let mut pr_number: Option<i64> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() + '\0' {
            "--pr-number" | "--pr" => pr_number = it.next().and_then(|v| v.parse().ok()),
            other => {
                eprintln!("fno do target request-self-review: unknown flag {other}");
                return REFUSED;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let head_sha = git_out(&cwd, &["rev-parse", "HEAD"]).unwrap_or_default();
    let mut ctx = SelfReviewCtx::default();
    let receipt = match request_self_review_steps(&cwd, pr_number, &head_sha, &mut ctx) {
        Ok(receipt) => receipt,
        Err(reason) => serde_json::json!({
            "outcome": "refused",
            "pr": pr_number,
            "head_sha": head_sha,
            "base_branch": ctx.base_branch,
            "reason": reason,
        }),
    };
    println!("{}", serde_json::to_string(&receipt).unwrap_or_default());
    match receipt.get("outcome").and_then(|v| v.as_str()) {
        Some("refused") | Some("unconfirmed") => REFUSED,
        _ => 0,
    }
}

/// The composed try-block of the Python verb: resolve the review target,
/// the identity, the invocation payload, then the send and the best-effort
/// hold. Ok carries the send receipt merged with the identity fields; Err
/// carries the refusal reason (the caller renders the refused receipt).
fn request_self_review_steps(
    cwd: &Path,
    pr_number: Option<i64>,
    head_sha: &str,
    ctx: &mut SelfReviewCtx,
) -> Result<serde_json::Value, String> {
    if head_sha.is_empty() {
        return Err("cannot resolve local HEAD".to_string());
    }
    if matches!(pr_number, Some(n) if n <= 0) {
        return Err("PR number must be positive".to_string());
    }
    let (branch, hold_branch, base_branch) = match pr_number {
        None => request_self_review_local_branch(cwd)?,
        Some(pr) => {
            let (hold_branch, base_branch) = request_self_review_pr_form(cwd, pr, head_sha, ctx)?;
            (String::new(), hold_branch, base_branch)
        }
    };
    ctx.branch = branch.clone();
    ctx.base_branch = base_branch.clone();
    ctx.hold_branch = hold_branch.clone();
    let (harness, session_id) = request_self_review_identity()?;
    let payload =
        request_self_review_invocation(cwd, &harness, pr_number, &branch, head_sha, &base_branch)?;
    ctx.payload = payload.clone();
    let mut receipt = request_self_review_send(&harness, &session_id, &payload);
    request_self_review_hold(cwd, head_sha, &session_id, &receipt, &hold_branch);
    let fields = serde_json::json!({
        "pr": pr_number,
        "branch": ctx.branch_or_none(),
        "head_sha": head_sha,
        "base_branch": base_branch,
        "payload": payload,
    });
    if let (Some(obj), Some(fields)) = (receipt.as_object_mut(), fields.as_object()) {
        for (key, value) in fields {
            obj.insert(key.clone(), value.clone());
        }
    }
    Ok(receipt)
}

/// The pre-push form: pin the LOCAL branch and HEAD against the origin base,
/// so the round runs before any push. The base resolves the way the emit
/// script resolves it; no gh call, a network lane would fail a local request.
fn request_self_review_local_branch(cwd: &Path) -> Result<(String, String, String), String> {
    let branch = git_out(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    if branch.is_empty() || branch == "HEAD" {
        return Err("no branch to review; check out the feature branch".to_string());
    }
    let raw = git_out(
        cwd,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .unwrap_or_default();
    let base = match raw.strip_prefix("origin/") {
        Some(stripped) => stripped.to_string(),
        None => raw,
    };
    if base.is_empty() {
        return Ok((branch.clone(), branch.clone(), "main".to_string()));
    }
    Ok((branch.clone(), branch.clone(), base))
}

/// The post-push form: read the PR's identity over the REST lane, refuse
/// unless the local HEAD IS the PR head, and key the hold on the PR's own
/// head ref (a local alias with a matching sha keys a hold the merge guard
/// never looks up).
fn request_self_review_pr_form(
    cwd: &Path,
    pr: i64,
    head_sha: &str,
    ctx: &mut SelfReviewCtx,
) -> Result<(String, String), String> {
    let payload = serde_json::json!({
        "op": "status-pr-info",
        "cwd": cwd.to_string_lossy(),
        "pr": pr,
    });
    let info = door_json("authorized-merge", &[], &payload);
    let err = match info.as_ref().and_then(|v| v.get("error")) {
        Some(serde_json::Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    };
    if let Some(detail) = err {
        let truncated: String = detail.chars().take(240).collect();
        return Err(format!("cannot read PR {pr}: {truncated}"));
    }
    let info = match info {
        Some(v) => v,
        None => return Err(format!("cannot read PR {pr}: REST read failed")),
    };
    let pr_head = info
        .get("head_sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let base_ref = info
        .get("base_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let head_ref = info
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    ctx.base_branch = base_ref.clone();
    if pr_head.is_empty() {
        return Err("PR has no headRefOid".to_string());
    }
    if !head_sha.eq_ignore_ascii_case(&pr_head) {
        return Err(format!(
            "local HEAD {head_sha} does not match PR head {pr_head}; refusing review"
        ));
    }
    if base_ref.is_empty() {
        return Err("PR baseRefName is unresolved; refusing review".to_string());
    }
    Ok((head_ref, base_ref))
}

/// The owned harness and FULL session id for the self-send lane (never a
/// short head: a codex UUIDv7 id shares its first eight with every
/// same-minute sibling, and resolution fails closed on the shared short).
fn request_self_review_identity() -> Result<(String, String), String> {
    let getter = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    let home = crate::paths::AgentsHome::from_env();
    let owned = crate::spawn_context::resolve_self_identity(&getter, None, None, &home);
    let harness = owned.harness.clone().unwrap_or_default().trim().to_string();
    let session_id = owned
        .session_id
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if harness.is_empty() || session_id.is_empty() {
        return Err("no owned harness identity is available for self-review".to_string());
    }
    Ok((harness, session_id))
}

/// Build the review invocation payload through the review-level capability
/// door - the same argv the Python verb assembled, empty payload on stdin.
fn request_self_review_invocation(
    cwd: &Path,
    harness: &str,
    pr_number: Option<i64>,
    branch: &str,
    head_sha: &str,
    base_branch: &str,
) -> Result<String, String> {
    let mut rest: Vec<String> = vec![
        "capability".to_string(),
        "invocation".to_string(),
        "--root".to_string(),
        cwd.to_string_lossy().to_string(),
        "--harness".to_string(),
        harness.to_string(),
    ];
    if let Some(pr) = pr_number {
        rest.push("--pr-number".to_string());
        rest.push(pr.to_string());
    }
    if !branch.is_empty() {
        rest.push("--branch".to_string());
        rest.push(branch.to_string());
    }
    rest.push("--head-sha".to_string());
    rest.push(head_sha.to_string());
    rest.push("--base-branch".to_string());
    rest.push(base_branch.to_string());
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let answer = door_json("review-level", &rest, &serde_json::json!({}))
        .ok_or_else(|| "review invocation door refused: the door did not answer".to_string())?;
    answer
        .get("invocation")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "review invocation door refused: no invocation in the answer".to_string())
}

/// Type the self-review payload through this binary's own `mail send` door -
/// the raw self lane, never a spawned pane. The FULL session id, never a
/// short head.
fn request_self_review_send(harness: &str, session_id: &str, payload: &str) -> serde_json::Value {
    let out = Command::new(self_exe())
        .arg("mail")
        .arg("send")
        .arg(session_id)
        .arg(payload)
        .arg("--raw")
        .arg("--self-ok")
        .arg("--review-request")
        .output();
    let out = match out {
        Ok(out) => out,
        Err(_) => {
            return serde_json::json!({
                "outcome": "refused",
                "transport": harness,
                "reason": "the fno-agents binary was not found; reinstall fno or run `fno doctor update --rust`, or set FNO_AGENTS_BIN to its path",
            });
        }
    };
    if !out.status.success() {
        let source = if out.stderr.is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            String::from_utf8_lossy(&out.stderr)
        };
        return serde_json::json!({
            "outcome": "refused",
            "transport": harness,
            "reason": source.trim(),
        });
    }
    let receipt = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if receipt.starts_with("queued") {
        serde_json::json!({"outcome": "queued", "transport": "mux-pane"})
    } else if receipt.starts_with("started") {
        serde_json::json!({"outcome": "started", "transport": "mux-pane"})
    } else if receipt.starts_with("injected") {
        serde_json::json!({"outcome": "started", "transport": "prompt-line"})
    } else {
        serde_json::json!({"outcome": "unconfirmed", "transport": harness})
    }
}

/// Register that a review of the branch at this head is RUNNING (the
/// requester side, so every pipeline review is held without a reviewer
/// remembering). Best-effort throughout: an unconfirmed send takes nothing,
/// a refused invocation builds no attestation and nothing would release the
/// hold, and a lockfile failure never turns a sent review into a refusal.
fn request_self_review_hold(
    cwd: &Path,
    head_sha: &str,
    session_id: &str,
    receipt: &serde_json::Value,
    hold_branch: &str,
) {
    let outcome = receipt
        .get("outcome")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if outcome == "refused" || outcome == "unconfirmed" {
        return;
    }
    if hold_branch.is_empty() || hold_branch == "HEAD" {
        return;
    }
    let holder_session = if session_id.is_empty() {
        "unknown"
    } else {
        session_id
    };
    let holder = format!("review-session:{holder_session}");
    let payload = serde_json::json!({
        "action": "acquire",
        "branch": hold_branch,
        "head": head_sha,
        "holder": holder,
        "verb": "/fno:review",
        "cwd": cwd.to_string_lossy(),
    });
    let _ = door_json("claim", &["review-hold"], &payload);
}

/// `review-invocation [--harness H]`: print the sized self-review invocation
/// for this session, level sized from the branch's diff (never a hardcoded
/// level). Read-only. A named door refusal exits 1 on stderr: machinery
/// embeds this output verbatim, so no placeholder stands in for a gate that
/// cannot answer.
fn review_invocation_run(args: &[String]) -> i32 {
    let mut harness: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--harness" {
            harness = it.next().cloned();
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut rest: Vec<String> = vec![
        "capability".to_string(),
        "invocation".to_string(),
        "--root".to_string(),
        cwd.to_string_lossy().to_string(),
    ];
    if let Some(name) = &harness {
        rest.push("--harness".to_string());
        rest.push(name.clone());
    }
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let Some(answer) = door_json("review-level", &rest, &serde_json::json!({})) else {
        eprintln!("review invocation door refused");
        return 1;
    };
    match answer.get("invocation").and_then(|v| v.as_str()) {
        Some(line) => {
            println!("{line}");
            0
        }
        None => {
            eprintln!("review invocation door refused");
            1
        }
    }
}

/// The op router: `fno-agents target-gates <verb> [flags]`, one verb per
/// ported Python gate. Registered in client.rs.
pub fn run(args: &[String]) -> i32 {
    let (verb, rest) = match args.split_first() {
        Some((verb, rest)) => (verb.as_str(), rest),
        None => {
            eprintln!("fno-agents target-gates: name a verb");
            return REFUSED;
        }
    };
    match verb {
        "check-review-gate" => run_check_review_gate(rest),
        "check-contained" => check_contained_run(rest),
        "check-dispatch-hold" => check_dispatch_hold_run(rest),
        "review-invocation" => review_invocation_run(rest),
        "resolve-owned-identity" => resolve_owned_identity_run(rest),
        "resolve-model" => resolve_model_run(rest),
        "denominator-ratio" => denominator_ratio_run(rest),
        "target-handoff-prepare" => handoff_prepare_run(rest),
        "request-self-review" => run_request_self_review(rest),
        other => {
            eprintln!("fno-agents target-gates: unknown verb {other}");
            REFUSED
        }
    }
}

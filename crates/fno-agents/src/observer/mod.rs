//! The observer family's native ops door, reached as
//! `fno doctor observer <op>` (the front classifier execs
//! `fno-agents spawn-overlay observer <op> ...`, the roles/accounts
//! precedent: no new root command on either binary).
//!
//! Ported from `cli/src/fno/observer/cli.py` (deleted in the same change).
//! Three ops over the pure fold: `sweep` (retrospective, read-only, ~$0
//! spend), `judge` (advisory five-question judge, never blocking), and
//! `replay` (active, isolated, real spend, blueprint-only). The scoring is
//! pure in [`fold`]; the digests render in [`digest`]; the detective scan
//! is [`isolation`].

mod digest;
mod fold;
mod isolation;

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::events::EventEmitter;
use crate::observer::isolation::IsolationResult;

/// The op door: `fno doctor observer <sweep|judge|replay> [flags]` execs
/// here through `fno-agents spawn-overlay observer <op> [flags]`.
pub fn run(args: &[String]) -> i32 {
    let Some(cmd) = args.first() else {
        eprintln!("usage: fno doctor observer <sweep|judge|replay> [flags]");
        return 2;
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "sweep" => run_sweep(rest),
        "judge" => run_judge(rest),
        "replay" => run_replay(rest),
        sub => {
            eprintln!("observer: unknown op {sub} (sweep|judge|replay)");
            2
        }
    }
}

/// `datetime.now()` on the fold's naive-local timeline.
fn now_local() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// `_mint_run_id` (cli.py:299): `obs-<skill_id>-<UTC timestamp>`, minted once
/// per sweep/replay batch.
fn mint_run_id(skill_id: &str) -> String {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("obs-{skill_id}-{stamp}")
}

/// `_events_paths` (cli.py:196): project events.jsonl (trigger source) first
/// (the CANONICAL log - the run_complete contract reads its append), then
/// the global log. The Python pair was `[project, global]` de-duplicated.
fn events_paths(cwd: &Path) -> Vec<PathBuf> {
    let project = crate::paths::events_path(cwd);
    let global = crate::paths_cli::global_events_json(cwd);
    if project == global {
        return vec![project];
    }
    vec![project, global]
}

/// `_emit_finding` (cli.py:205): one `skill_eval_finding` per (item,
/// dimension) verdict. Evidence capped at 500 chars by the schema.
struct Finding<'a> {
    run_id: &'a str,
    item: &'a Value,
    dimension: &str,
    verdict: &str,
    evidence: String,
    cost_usd: f64,
    skill_ref: Option<&'a str>,
    tool_fault: bool,
}

/// Append one finding to each journal (best-effort per journal; the caller
/// surfaces the canonical-log failure where the contract needs it).
fn emit_finding(f: &Finding<'_>, events_paths: &[PathBuf]) {
    let skill_id = f.item.get("skill_id").cloned().unwrap_or(json!("unknown"));
    let sv = f
        .item
        .get("skill_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let corpus_item_id = f
        .item
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut data = json!({
        "run_id": f.run_id,
        "skill_id": skill_id,
        "skill_version": sv,
        "corpus_item_id": corpus_item_id,
        "dimension": f.dimension,
        "verdict": f.verdict,
        "evidence": f.evidence.chars().take(500).collect::<String>(),
        "cost_usd": f.cost_usd,
    });
    if let Some(r) = f.skill_ref {
        data["skill_ref"] = json!(r);
    }
    if f.tool_fault {
        data["tool_fault"] = json!(true);
    }
    for p in events_paths {
        let emitter = EventEmitter::new(p, "observer");
        if let Err(err) = emitter.emit("skill_eval_finding", &data) {
            eprintln!("observer: finding emit to {} failed: {err}", p.display());
        }
    }
}

/// `_emit_run_complete` (cli.py:241): emit the terminal run event; returns
/// whether the CANONICAL (first, project) append succeeded - the caller
/// fails the command if not, so the CLI never reports `ok` while the machine
/// contract has no terminal event (codex P2). The global log stays
/// best-effort. The summary's `state` key is dropped (the event carries the
/// fields, never the state word).
fn emit_run_complete(summary: &Value, events_paths: &[PathBuf]) -> bool {
    let mut data = summary.as_object().cloned().unwrap_or_default();
    data.remove("state");
    let Some(canonical) = events_paths.first() else {
        return false;
    };
    let canonical_ok = EventEmitter::new(canonical, "observer")
        .emit("skill_eval_run_complete", &data)
        .is_ok();
    for p in events_paths.iter().skip(1) {
        let _ = EventEmitter::new(p, "observer").emit("skill_eval_run_complete", &data);
    }

    canonical_ok
}

/// `_default_gh` (cli.py:146): one gh round-trip, 60s timeout; any spawn
/// fault reads (1, "", <error>).
type GhRunner<'a> = &'a dyn Fn(&[String]) -> (i32, String, String);

fn default_gh(args: &[String]) -> (i32, String, String) {
    let Ok(out) = std::process::Command::new("gh").args(args).output() else {
        return (1, String::new(), "gh spawn failed".to_string());
    };
    (
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `_capped_gh` (cli.py:160): at most `cap` real gh invocations per sweep;
/// past the cap, a gh-unavailable result - the caller's existing gh-down
/// degrade counts the node as a coverage gap, never a crash. Bounds a
/// sweep's total network fan-out so N concurrent sweeps cannot stretch a
/// seconds-job into tens of minutes.
struct CappedGh<'a> {
    runner: GhRunner<'a>,
    cap: usize,
    calls: std::cell::Cell<usize>,
}

impl CappedGh<'_> {
    fn call(&self, args: &[String]) -> (i32, String, String) {
        let n = self.calls.get();
        if n >= self.cap {
            return (
                1,
                String::new(),
                "observer: gh fan-out cap reached".to_string(),
            );
        }
        self.calls.set(n + 1);
        (self.runner)(args)
    }
}

/// `_read_postmortems` (cli.py:55): `{session_id, graph_node_id,
/// blocked_reason_kind}` per postmortem markdown under the configured
/// postmortems dir. Tolerant: a missing dir or a malformed file is skipped,
/// never fatal (mirrors `harvest_postmortems`).
fn read_postmortems(cwd: &Path) -> Vec<Value> {
    let dir = crate::paths_cli::postmortems_dir(cwd);
    if !dir.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut out: Vec<Value> = Vec::new();
    for path in paths {
        if path.extension().is_some_and(|e| e != "md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // The frontmatter reader mirrors `agent.state._extract_frontmatter`:
        // the --- fenced yaml block, tolerant of an unclosed fence (an
        // unclosed fence carries no attribution keys).
        let Some(fm) = frontmatter_fields(&text) else {
            continue;
        };
        let blocked = fm.get("blocked_reason").cloned();
        let kind = blocked
            .as_ref()
            .and_then(|b| b.as_object())
            .and_then(|b| b.get("kind"))
            .and_then(Value::as_str)
            .filter(|k| !k.is_empty())
            .map(str::to_string);
        out.push(json!({
            "session_id": fm.get("session_id").cloned().unwrap_or(Value::Null),
            "graph_node_id": fm.get("graph_node_id").cloned().unwrap_or(Value::Null),
            "blocked_reason_kind": kind,
        }));
    }
    out
}

/// `_extract_frontmatter` (agent/state.py) as a tolerant yaml-fields reader:
/// the --- fenced block parsed with serde_yaml_ng; None when no fence or an
/// unclosed fence.
fn frontmatter_fields(text: &str) -> Option<Map<String, Value>> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("---")?;
    let end = rest.find("\n---").map(|i| i + 1)?;
    let block = rest[..end].trim();
    let Value::Object(fields) = serde_yaml_ng::from_str::<Value>(block).ok()? else {
        return None;
    };
    Some(fields)
}

/// `load_ledger_rows` (scoreboard/fold.py:130): retry once on a parse error
/// (a mid-append race), fail loud on a second. Missing file = empty (fresh
/// install); junk shape = empty.
fn ledger_rows(cwd: &Path) -> Result<Vec<Value>, String> {
    let path = crate::paths_cli::ledger_json(cwd);
    let read = || std::fs::read_to_string(&path);
    let parse = |raw: String| -> Result<Vec<Value>, String> {
        let data: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("{}: parse error: {e}", path.display()))?;
        match data.get("entries").unwrap_or(&data) {
            Value::Array(rows) => Ok(rows.iter().filter(|r| r.is_object()).cloned().collect()),
            _ => Ok(Vec::new()),
        }
    };
    match read() {
        Ok(raw) => match parse(raw) {
            Ok(rows) => Ok(rows),
            Err(first) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                read().ok().map_or_else(
                    || Err(format!("BrokenLedger: {first}; retry read failed")),
                    |raw2| parse(raw2),
                )
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// `_read_plan_text` (cli.py:119): plan text for a blueprint corpus item -
/// on-disk plan file first, then `gh pr diff <n>` fallback (AC1-EDGE), else
/// None (the item is excluded and counted in the coverage gap). A node's PR
/// can live in another repo (the global graph spans projects), so `--repo`
/// is pinned from its pr_url; without it gh resolves the current checkout
/// and fetches the wrong PR (codex P2).
fn read_plan_text(item: &Value, node: Option<&Value>, gh: &CappedGh<'_>) -> Option<String> {
    if let Some(plan_path) = item.get("plan_path").and_then(Value::as_str) {
        if let Ok(text) = std::fs::read_to_string(plan_path) {
            return Some(text);
        }
    }
    let node = node?;
    let pr_number = node.get("pr_number")?;
    if pr_number.is_null() {
        return None;
    }
    let mut args = vec!["pr".to_string(), "diff".to_string(), pr_number.to_string()];
    if let Some(repo) = fold::repo_from_url(node.get("pr_url").and_then(Value::as_str)) {
        args.push("--repo".to_string());
        args.push(repo);
    }
    let (rc, out, _err) = gh.call(&args);
    let out = out.trim().to_string();
    if rc == 0 && !out.is_empty() {
        return Some(out);
    }
    None
}

/// The wave-door round-trip for one plan text (fold.py `_surface_collisions`'
/// I/O half): write a temp plan file, run the in-process wave check, fold the
/// lines through [`fold::collisions_from_check`].
fn surface_collisions(plan_text: &str, repo_root: &Path) -> Option<Vec<String>> {
    if fold::plan_reaches_wave_door(plan_text).is_none() {
        return None;
    }
    if !fold::plan_reaches_wave_door(plan_text)? {
        return Some(Vec::new());
    }
    let mut path = std::env::temp_dir();
    path.push(format!(
        "observer-collision-{}-{}.md",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    std::fs::write(&path, plan_text).ok()?;
    let check = crate::wave::check_plan(&path, repo_root).ok();
    let _ = std::fs::remove_file(&path);
    fold::collisions_from_check(check.as_deref())
}

/// The sweep's argv: `sweep --skill <s> [--since N] [--repo r] [--json|-J]
/// [--judge N]`. `--judge` rides the raw argv tail (it is fno-agents' own
/// flag), read positionally like the Python's `ctx.args` scan.
fn run_sweep(args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut skill = String::new();
    let mut since: i64 = 28;
    let mut repo: Option<String> = None;
    let mut json_out = false;
    let mut judge_n: usize = 0;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--skill" => {
                i += 1;
                skill = args.get(i).cloned().unwrap_or_default();
            }
            "--since" => {
                i += 1;
                since = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(28);
            }
            "--repo" => {
                i += 1;
                repo = args.get(i).cloned();
            }
            "-J" | "--json" => json_out = true,
            "--judge" => {
                i += 1;
                judge_n = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            _ => {}
        }
        i += 1;
    }

    if skill.is_empty() {
        eprintln!("observer sweep: --skill is required (blueprint | review | target)");
        return 2;
    }
    if !matches!(skill.as_str(), "blueprint" | "review" | "target") {
        eprintln!("observer sweep: --skill must be blueprint, review, or target");
        return 2;
    }
    if judge_n > 0 && skill != "blueprint" {
        println!("--judge applies to --skill blueprint only; ignored");
    }
    if since < 1 {
        eprintln!("observer sweep: --since must be >= 1 (days)");
        return 2;
    }
    if skill == "target" {
        return sweep_target(&cwd, since, repo, json_out);
    }
    sweep_skill(&cwd, &skill, since, judge_n, json_out)
}

/// The blueprint/review sweep (cli.py `sweep` minus the target arm).
fn sweep_skill(cwd: &Path, skill: &str, since: i64, judge_n: usize, json_out: bool) -> i32 {
    let (corpus, by_id) = match load_corpus(cwd, skill, since) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let items = corpus
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let skill_id = fold::skill_id_for(skill);
    let repo_root = crate::paths::worktree_repo_root(cwd);

    // Insufficient guard first (AC1-UI): <10 attributable -> no run_complete.
    if items.len() < fold::MIN_ATTRIBUTABLE {
        println!(
            "insufficient: {} attributable {} session(s) in the last {}d, need >={}. \
             No skill_eval_run_complete emitted.",
            items.len(),
            skill_id,
            since,
            fold::MIN_ATTRIBUTABLE
        );
        return 0;
    }

    let run_id = mint_run_id(&skill_id);
    let epaths = events_paths(cwd);
    let gh = CappedGh {
        runner: &default_gh,
        cap: gh_cap(),
        calls: std::cell::Cell::new(0),
    };
    let skill_version = items[0]
        .get("skill_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let mut findings: Vec<(String, String)> = Vec::new();
    let mut scored_count = 0usize;
    // review: one review-summary --evidence round-trip; a fault leaves every
    // item a gap, digest named (cli.py `_review_evidence`).
    let review_ev = if skill == "review" {
        review_evidence(&items, &by_id)
    } else {
        None
    };
    for (n, item) in items.iter().enumerate() {
        let scores = match &review_ev {
            Some(ev) => {
                let mut m = Map::new();
                let verdict = ev
                    .get("items")
                    .and_then(Value::as_array)
                    .and_then(|list| list.get(n))
                    .and_then(|row| row.get("finding_precision").cloned())
                    .unwrap_or(Value::Null);
                m.insert("finding_precision".to_string(), verdict);
                Value::Object(m)
            }
            None => score_item(item, skill, &by_id, &gh, cwd),
        };
        let mut item_scored = false;
        let entries: Vec<(String, Value)> = scores
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        for (dimension, verdict) in &entries {
            if verdict.is_null() {
                continue; // not scorable -> coverage gap, never a fabricated verdict
            }
            item_scored = true;
            let verdict_str = verdict.as_str().unwrap_or_default().to_string();
            findings.push((dimension.clone(), verdict_str.clone()));
            emit_finding(
                &Finding {
                    run_id: &run_id,
                    item,
                    dimension,
                    verdict: &verdict_str,
                    evidence: evidence_line_for(item, dimension, &verdict_str),
                    cost_usd: 0.0,
                    skill_ref: None,
                    tool_fault: false,
                },
                &epaths,
            );
        }
        if item_scored {
            scored_count += 1;
        }
    }

    let mut summary = fold::build_run_summary(
        &run_id,
        &skill_id,
        &skill_version,
        &findings,
        items.len(),
        scored_count,
        None,
        true,
    );
    run_sweep_finish(
        &mut summary,
        skill,
        &items,
        judge_n,
        &epaths,
        json_out,
        review_ev
            .as_ref()
            .and_then(|ev| ev.get("evidence_line"))
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// The sweep's tail (cli.py 398-430): the optional judge pass, the terminal
/// run_complete (its failure is exit 5 - the digest alone is not the machine
/// contract), the evidence line for the digest, and the stdout state print.
fn run_sweep_finish(
    summary: &mut Value,
    skill: &str,
    items: &[Value],
    judge_n: usize,
    epaths: &[PathBuf],
    json_out: bool,
    evidence_line: Option<String>,
) -> i32 {
    let run_id = summary
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if judge_n > 0 && skill == "blueprint" {
        // Judge before run_complete closes the run; a judge fault never
        // loses the code record.
        let skipped = items.len().saturating_sub(judge_n);
        let mut judge_ok = true;
        for item in &items[skipped..] {
            if let JudgeOutcome::Fault = judge_one_item(item, &run_id, epaths) {
                eprintln!(
                    "judge fault (code scoring unaffected): the fno-agents judge round-trip failed"
                );
                judge_ok = false;
                break;
            }
        }
        if judge_ok {
            println!(
                "  judge: offered the model judge to {} window plan(s)",
                judge_n.min(items.len())
            );
        }
    }
    if !emit_run_complete(summary, epaths) {
        eprintln!(
            "error: could not write the canonical skill_eval_run_complete event; \
             the sweep is not recorded (the digest alone is not the machine contract)."
        );
        return 5;
    }
    if let Some(line) = evidence_line {
        if let Some(obj) = summary.as_object_mut() {
            obj.insert("evidence_line".to_string(), json!(line));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let digest = write_sweep_digest(summary, skill, "sweep", &cwd);
    let scored = summary
        .get("pass_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + summary
            .get("degraded_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
        + summary
            .get("fail_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    let state = if scored as usize == items.len() {
        "ok"
    } else {
        "partial"
    };
    if json_out {
        let mut payload = summary.as_object().cloned().unwrap_or_default();
        payload.insert("state".to_string(), json!(state));
        payload.insert("digest".to_string(), json!(digest.display().to_string()));
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(payload)).unwrap_or_default()
        );
        return 0;
    }
    let coverage_pct = summary
        .get("coverage_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let corpus = summary
        .get("corpus_size")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let pass = summary
        .get("pass_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let degraded = summary
        .get("degraded_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let fail = summary
        .get("fail_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let skill_id = summary
        .get("skill_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut out = format!(
        "{state}: {skill_id} run {run_id}\n  scored {scored}/{corpus} attributable items \
         ({coverage_pct}% coverage)\n  verdicts: pass={pass} degraded={degraded} fail={fail}"
    );
    if let Some(line) = evidence_line {
        out.push_str(&format!("\n  {line}"));
    }
    out.push_str(&format!("\n  digest: {}", digest.display()));
    println!("{out}");
    0
}

/// `_write_digest`'s write half: reports dir for the skill-id namespace's
/// owner (`fno:blueprint` -> `fno`), mkdir, `<skill>-<date>.md`.
fn write_sweep_digest(summary: &Value, skill: &str, mode: &str, cwd: &Path) -> PathBuf {
    let sid = summary
        .get("skill_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let owner = sid.split(':').next().filter(|_| sid.contains(':'));
    let dir = crate::paths_cli::observer_reports_dir(cwd, owner);
    let _ = std::fs::create_dir_all(&dir);
    let date = chrono::Local::now().format("%Y-%m-%d");
    let path = dir.join(format!("{skill}-{date}.md"));
    let body = digest::sweep_digest_body(summary, skill, mode, &date.to_string());
    let _ = std::fs::write(&path, body);
    path
}

/// The sweep's per-item evidence line (cli.py `_evidence` / `_target_evidence`):
/// one earned-specificity line per (item, dimension) verdict.
fn evidence_line_for(item: &Value, dimension: &str, verdict: &str) -> String {
    let sid = item
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let nid = item
        .get("graph_node_id")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "?".to_string());
    if dimension == "shipped_outcome" {
        let outcome = item
            .get("outcome")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let attribution = item
            .get("attribution_class")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        return format!("node {nid} outcome={outcome} attribution={attribution}");
    }
    format!("session {sid} node {nid}: {dimension}={verdict}")
}

/// `_review_evidence` (cli.py:447): one review-summary --evidence
/// round-trip; a fault leaves every item a gap, evidence line named. The
/// in-process call replaces the Python subprocess to the same
/// `fno-agents review-summary --evidence` door, reading the same
/// attestation journal (the state root's events.jsonl).
fn review_evidence(items: &[Value], by_id: &BTreeMap<String, Value>) -> Option<Value> {
    let payload: Vec<Value> = items
        .iter()
        .map(|item| {
            let node = item
                .get("graph_node_id")
                .and_then(Value::as_str)
                .and_then(|nid| by_id.get(nid));
            json!({
                "node": node.and_then(|n| n.get("id")).cloned().unwrap_or(Value::Null),
                "pr_number": node.and_then(|n| n.get("pr_number")).cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let events_path = crate::scratch::fno_state_root().join("events.jsonl");
    let events_text = crate::event_store::journal_text(&events_path, &[]);
    let out = crate::review_summary::evidence(&events_text, &payload);
    // The Python gap contract: a malformed/short answer is the unread gap
    // (every item a None verdict), never a fabricated round.
    let same_len = out
        .get("items")
        .and_then(Value::as_array)
        .is_some_and(|list| list.len() == items.len());
    if same_len {
        Some(out)
    } else {
        Some(review_gap())
    }
}

/// The `_review_evidence` gap dict (cli.py:449): every item an unscorable
/// empty row, evidence line named.
fn review_gap() -> Value {
    json!({
        "items": Vec::<Value>::new(),
        "evidence_line": "evidence: unread (fno-agents review-summary --evidence failed)"
    })
}

/// `_score_item` (cli.py:175): score one corpus item, returning
/// `{dimension: verdict|null}`. A gh/disk failure for an attributed item
/// yields all-null (a coverage gap), never a crash (AC1-ERR / AC1-EDGE).
fn score_item(
    item: &Value,
    skill: &str,
    by_id: &BTreeMap<String, Value>,
    gh: &CappedGh<'_>,
    cwd: &Path,
) -> Value {
    let node = item
        .get("graph_node_id")
        .and_then(Value::as_str)
        .and_then(|nid| by_id.get(nid));
    if skill != "blueprint" {
        let mut m = Map::new();
        m.insert("finding_precision".to_string(), Value::Null);
        return Value::Object(m);
    }
    let plan_text = read_plan_text(item, node, gh);
    match plan_text {
        Some(text) => {
            let root = crate::paths::worktree_repo_root(cwd);
            let collisions = surface_collisions(&text, &root);
            fold::score_blueprint_item(item, collisions.as_deref())
        }
        None => {
            let mut m = Map::new();
            for d in ["collision_free", "shipped_outcome"] {
                m.insert(d.to_string(), Value::Null);
            }
            Value::Object(m)
        }
    }
}

/// `_GH_FANOUT_CAP` (cli.py:157): the sweep's real gh invocation ceiling,
/// `FNO_OBSERVER_GH_CAP`, default 10.
fn gh_cap() -> usize {
    std::env::var("FNO_OBSERVER_GH_CAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
}

/// `_load_corpus` (cli.py:87): ledger + graph + postmortems + the delivery
/// classify -> the pure fold. Returns `(corpus, by_id)` with `by_id` mapping
/// graph node id -> node (PR lookup in review scoring and replay). The graph
/// read is tolerant (any store-read fault reads as no nodes, the
/// `read_graph_nodes` contract); the ledger read stays strict (a broken
/// ledger is a loud `Err`, the caller's exit 1).
fn load_corpus(
    cwd: &Path,
    skill: &str,
    since: i64,
) -> Result<(Value, BTreeMap<String, Value>), String> {
    let rows = ledger_rows(cwd)?;
    let nodes =
        crate::backlog::read_entries(&crate::paths_cli::graph_json(cwd)).unwrap_or_default();
    let postmortems = read_postmortems(cwd);
    let deliveries = crate::scoreboard::classify(&json!({"entries": nodes, "rows": rows}))
        .ok()
        .and_then(|c| c.get("by_node").cloned());
    let repo_root = crate::paths::worktree_repo_root(cwd);
    let resolve =
        |skill_id: &str, ts: Option<&str>| fold::default_skill_version(skill_id, ts, &repo_root);
    let readers = fold::CorpusReaders {
        read_transcript: &fold::default_read_transcript,
        resolve_skill_version: &resolve,
    };
    let corpus = fold::build_corpus(
        &rows,
        &nodes,
        &postmortems,
        skill,
        since,
        now_local(),
        deliveries.as_ref(),
        &readers,
    );
    let by_id: BTreeMap<String, Value> = nodes
        .iter()
        .filter_map(|n| {
            let id = n.get("id")?.as_str()?;
            if id.is_empty() {
                return None;
            }
            Some((id.to_string(), n.clone()))
        })
        .collect();
    Ok((corpus, by_id))
}

/// `_judge_via_rust` (cli.py:459): one `fno-agents judge` round-trip from the
/// running binary (`current_exe` is the twin of Python's find_dev_binary /
/// resolve_binary pair). JSON out, `None` on any fault - a coverage gap,
/// never a fabricated verdict. Python's OSError / TimeoutExpired / ValueError
/// arms collapse into the same fault line naming the detail.
fn judge_via_rust(argv: &[String]) -> Option<Value> {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("judge fault: fno-agents binary not found; run `fno doctor update --rust`");
        return None;
    };
    let timeout: u64 = if argv.iter().any(|a| a == "--labels") {
        14400
    } else {
        3600
    };
    let child = std::process::Command::new(&exe)
        .arg("judge")
        .args(argv)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn();
    let (rc, out, err) = match child {
        Ok(c) => crate::blueprint_judge::bounded_wait(c, timeout)
            .unwrap_or_else(|e| (1, String::new(), e)),
        Err(e) => (1, String::new(), e.to_string()),
    };
    let _ = rc;
    let parsed: Result<Value, _> = serde_json::from_str(out.trim());
    match parsed {
        Ok(v) => {
            if v.get("error").is_some_and(truthy) {
                let detail = v
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                eprintln!("judge fault: {detail}");
                return None;
            }
            Some(v)
        }
        Err(_) => {
            eprintln!(
                "judge fault: bad output (stdout={:?} stderr={:?})",
                out.chars().take(200).collect::<String>(),
                err.chars().take(200).collect::<String>(),
            );
            None
        }
    }
}

/// The per-item judge result: `Judged(fails)` counted, `Gap` (no Five
/// questions section - no judge call), or `Fault` (the round-trip failed).
enum JudgeOutcome {
    Judged(usize),
    Gap,
    Fault,
}

/// `_judge_one_item` (cli.py:487): offer the model judge to one plan item.
/// Emits one finding per answered dimension (cost-untracked: the judge's own
/// spend is unattributed to the session, not free) and prints the human
/// lines. Never a fabricated fail: unanswered prints `unanswered`, a fault
/// is [`JudgeOutcome::Fault`].
fn judge_one_item(item: &Value, run_id: &str, epaths: &[PathBuf]) -> JudgeOutcome {
    let pp = item.get("plan_path").and_then(Value::as_str);
    let text = pp
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let has_five = !text.is_empty() && fold::has_five_questions(&text);
    if !has_five {
        return JudgeOutcome::Gap;
    }
    let mut argv = vec!["--plan".to_string(), pp.unwrap_or_default().to_string()];
    if let Some(node) = item.get("graph_node_id").and_then(Value::as_str) {
        argv.push("--node".to_string());
        argv.push(node.to_string());
    }
    let Some(out) = judge_via_rust(&argv) else {
        return JudgeOutcome::Fault;
    };
    let mut fails = 0usize;
    let rows = out
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in &rows {
        let dimension = row.get("dimension").and_then(Value::as_str).unwrap_or("?");
        let verdict = row.get("verdict");
        let reason = row
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match verdict.and_then(Value::as_str) {
            None => {
                println!(
                    "  {dimension}: unanswered ({})",
                    reason.chars().take(120).collect::<String>()
                );
            }
            Some(v) => {
                if v == "fail" {
                    fails += 1;
                }
                println!(
                    "  {dimension}: {v} - {}",
                    reason.chars().take(160).collect::<String>()
                );
                emit_finding(
                    &Finding {
                        run_id,
                        item,
                        dimension,
                        verdict: v,
                        evidence: format!("cost-untracked; {reason}"),
                        cost_usd: 0.0,
                        skill_ref: None,
                        tool_fault: false,
                    },
                    epaths,
                );
            }
        }
    }
    JudgeOutcome::Judged(fails)
}

/// Python truthiness over a JSON value for the judge's `controls_wrong`
/// flag: null, false, empty string and zero are false; anything else true.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `_arg_value` (cli.py:426): the token after *flag*, or None.
fn arg_value(args: &[String], flag: &str) -> Option<&String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
}

/// The judge op (cli.py `judge_cmd`): the advisory five-question judge,
/// never blocking. Every flag is fno-agents'; the loop-level gate and the
/// human lines are the observer's own.
fn run_judge(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--split") && !args.iter().any(|a| a == "--labels") {
        println!("--split applies to --labels only; ignored");
    }
    if args.iter().any(|a| a == "--labels") {
        let Some(out) = judge_via_rust(args) else {
            return 1;
        };
        if let Some(dims) = out.get("dimensions").and_then(Value::as_object) {
            for (dim, s) in dims {
                println!(
                    "{dim}: n={} tp_rate={} tn_rate={}",
                    s.get("n").map(|v| v.to_string()).unwrap_or_default(),
                    s.get("tp_rate").map(|v| v.to_string()).unwrap_or_default(),
                    s.get("tn_rate").map(|v| v.to_string()).unwrap_or_default(),
                );
            }
        }
        if let Some(list) = out.get("disagreements").and_then(Value::as_array) {
            for d in list {
                println!(
                    "{}  {}  label={}  judge={}  {}",
                    d.get("plan").map(|v| v.to_string()).unwrap_or_default(),
                    d.get("dimension")
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    d.get("label").map(|v| v.to_string()).unwrap_or_default(),
                    d.get("judge").map(|v| v.to_string()).unwrap_or_default(),
                    d.get("reason")
                        .and_then(Value::as_str)
                        .map(|r| r.chars().take(120).collect::<String>())
                        .unwrap_or_default(),
                );
            }
        }
        if out.get("controls_wrong").is_some_and(truthy) {
            println!(
                "controls wrong: {}",
                out.get("controls_wrong")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            );
            return 1;
        }
        return 0;
    }
    let Some(plan) = arg_value(args, "--plan") else {
        eprintln!("judge: give --plan or --labels");
        return 2;
    };
    let forced = args.iter().any(|a| a == "--force" || a == "-F");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if !forced && loop_level(&cwd, "blueprint_judge") == "report" {
        println!("skipped level=report");
        return 0;
    }
    let plan_path = PathBuf::from(plan);
    if !plan_path.exists() {
        println!("unanswered: no plan at {}", plan_path.display());
        return 1;
    }
    let node = arg_value(args, "--node");
    println!(
        "judging {} ({})",
        plan_path.display(),
        node.map(String::as_str).unwrap_or("no node")
    );
    let item = json!({
        "skill_id": "fno:blueprint",
        "session_id": Value::Null,
        "graph_node_id": node.cloned().map(Value::String).unwrap_or(Value::Null),
        "plan_path": plan_path.display().to_string(),
    });
    let run_id = mint_run_id("fno:blueprint");
    let epaths = events_paths(&cwd);
    match judge_one_item(&item, &run_id, &epaths) {
        JudgeOutcome::Gap => {
            println!("unanswered: plan carries no ## Five questions section; no judge call");
            0
        }
        JudgeOutcome::Fault => {
            println!(
                "unanswered: the fno-agents judge round-trip failed; see the fault line above"
            );
            1
        }
        JudgeOutcome::Judged(fails) => {
            if fails > 0 {
                println!("revise the plan once, or write a one-line disposition under that question; intake proceeds either way");
            }
            0
        }
    }
}

/// Python `loops.loop_level`: the configured level for a named loop.
/// Unconfigured or malformed fails safe to "report" (observe only) - a
/// standing loop never upgrades its own autonomy from a config typo.
fn loop_level(cwd: &Path, name: &str) -> String {
    crate::agents_config::config_lookup(cwd, &["loops", name, "level"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|l| matches!(l.as_str(), "report" | "assisted" | "unattended"))
        .unwrap_or_else(|| "report".to_string())
}

/// `_replay_cost_cap` (cli.py:1188): the per-item spend ceiling - the min of
/// the configured per-run budget (`config.loops.observer_harness.
/// budget_usd_per_run`) and the hard $3 cap (never uncapped). Post-hoc
/// overage warning, not a pre-spawn gate; the 600s spawn timeout is the real
/// bound.
fn replay_cost_cap(cwd: &Path) -> f64 {
    const PER_ITEM_CAP_USD: f64 = 3.0;
    let budget = crate::agents_config::config_lookup(
        cwd,
        &["loops", "observer_harness", "budget_usd_per_run"],
    )
    .and_then(|v| v.as_f64());
    match budget {
        Some(b) => PER_ITEM_CAP_USD.min(b),
        None => PER_ITEM_CAP_USD,
    }
}

/// `_read_scratch_cost` (cli.py:1207): best-effort real spend from the
/// throwaway worktree's isolated ledger. Unknown -> 0.0.
fn read_scratch_cost(scratch: &Path) -> f64 {
    let Ok(raw) = std::fs::read_to_string(scratch.join(".fno").join("ledger.json")) else {
        return 0.0;
    };
    let Ok(data) = serde_json::from_str::<Value>(&raw) else {
        return 0.0;
    };
    let empty = Vec::new();
    let entries = data
        .get("entries")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let total: f64 = entries
        .iter()
        .filter_map(|e| e.get("cost_usd").and_then(Value::as_f64))
        .sum();
    (total * 10000.0).round() / 10000.0
}

/// `_write_workdir_settings` (cli.py:911): preventive isolation - redirect
/// all mutable fno config paths into the scratch worktree's own throwaway
/// `.fno/` and stamp the migration sentinel so the first `fno` call in the
/// spawned session does not rewrite this fragment.
fn write_workdir_settings(workdir: &Path) {
    let fno_dir = workdir.join(".fno");
    let _ = std::fs::create_dir_all(&fno_dir);
    let settings = json!({
        "schema_version": 1,
        "config": {
            "no_ship": true,
            "state_dir": format!("{}/", fno_dir.display()),
            "paths": {
                "ledger_json": fno_dir.join("ledger.json").display().to_string(),
                "briefs_dir": format!("{}/", fno_dir.join("briefs").display()),
            },
        },
    });
    let body = serde_yaml_ng::to_string(&settings).unwrap_or_default();
    let _ = std::fs::write(fno_dir.join("settings.yaml"), body);
    let _ = std::fs::File::create(fno_dir.join(".path-migration-done"));
}

/// `_already_scored` (cli.py:955): idempotent resume - has this
/// (run_id, skill_ref, corpus_item) triple already emitted a finding?
/// Tolerant read (any parse fault -> not scored, so real work re-runs).
fn already_scored(
    run_id: &str,
    skill_ref: Option<&str>,
    corpus_item: &str,
    epaths: &[PathBuf],
) -> bool {
    for p in epaths {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for line in text.lines() {
            let Ok(e) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(d) = e.get("data").filter(|d| d.is_object()) else {
                continue;
            };
            let same_ref = |a: Option<&str>, b: Option<&str>| {
                a.filter(|s| !s.is_empty()) == b.filter(|s| !s.is_empty())
            };
            if e.get("type").and_then(Value::as_str) == Some("skill_eval_finding")
                && d.get("run_id").and_then(Value::as_str) == Some(run_id)
                && d.get("corpus_item_id").and_then(Value::as_str) == Some(corpus_item)
                && same_ref(d.get("skill_ref").and_then(Value::as_str), skill_ref)
            {
                return true;
            }
        }
    }
    false
}

/// The replay op (cli.py `replay` / `_replay`): one corpus item's recorded
/// input through `/blueprint` in an isolated scratch worktree, scored on
/// structural dimensions only, hard-failed if the replay session leaks into
/// real state. `--skill review` is refused (A2: a replayed review has no
/// ground truth for finding_precision).
fn run_replay(args: &[String]) -> i32 {
    let mut skill = String::new();
    let mut corpus_item = String::new();
    let mut skill_ref: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut since: i64 = 90;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--skill" => {
                i += 1;
                skill = args.get(i).cloned().unwrap_or_default();
            }
            "--corpus-item" => {
                i += 1;
                corpus_item = args.get(i).cloned().unwrap_or_default();
            }
            "--skill-ref" => {
                i += 1;
                skill_ref = args.get(i).cloned();
            }
            "--run-id" => {
                i += 1;
                run_id = args.get(i).cloned();
            }
            "--since" => {
                i += 1;
                since = args.get(i).and_then(|v| v.parse().ok()).unwrap_or(90);
            }
            _ => {}
        }
        i += 1;
    }
    if skill.is_empty() {
        eprintln!("replay: --skill is required (blueprint only in v1)");
        return 2;
    }
    if corpus_item.is_empty() {
        eprintln!("replay: --corpus-item is required");
        return 2;
    }
    if skill == "review" {
        println!("replay not supported for review in v1 (Review Amendment A2): a replayed review has no historical thread state to score finding_precision against.");
        return 2;
    }
    if skill != "blueprint" {
        eprintln!("replay: --skill must be blueprint (v1 replay is blueprint-only)");
        return 2;
    }
    // Pause gate at the corpus-item boundary.
    if crate::loops_pause::is_paused() {
        println!("paused: loop halt in effect; stopping cleanly before replay.");
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (corpus, by_id) = match load_corpus(&cwd, &skill, since) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let item = corpus
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find(|it| {
                it.get("session_id").and_then(Value::as_str) == Some(corpus_item.as_str())
            })
        });
    let Some(item) = item else {
        println!("no attributable {skill} corpus item with session_id={corpus_item} in the last {since}d.");
        return 1;
    };
    let skill_id = item
        .get("skill_id")
        .and_then(Value::as_str)
        .unwrap_or("fno:blueprint")
        .to_string();
    let run_id = run_id.unwrap_or_else(|| mint_run_id(&skill_id));
    let epaths = events_paths(&cwd);
    if already_scored(&run_id, skill_ref.as_deref(), &corpus_item, &epaths) {
        println!(
            "already scored ({run_id}, {}, {corpus_item}); skipping (AC2-FR).",
            skill_ref.as_deref().unwrap_or("HEAD")
        );
        return 0;
    }
    replay_run(
        &cwd,
        &skill,
        &corpus_item,
        skill_ref.as_deref(),
        &run_id,
        item,
        &by_id,
        &epaths,
    )
}

/// The replay core (cli.py `_replay` body): the recorded plan text, the
/// single-flight claim, the scratch worktree, the headless spawn, the
/// detective scan FIRST (an isolation violation voids the whole run and
/// emits nothing), then structural scoring. The cleanup pair (worktree
/// remove + flight release) runs on every path past the claim.
#[allow(clippy::too_many_arguments)]
fn replay_run(
    cwd: &Path,
    skill: &str,
    corpus_item: &str,
    skill_ref: Option<&str>,
    run_id: &str,
    item: &Value,
    by_id: &BTreeMap<String, Value>,
    epaths: &[PathBuf],
) -> i32 {
    use std::os::unix::process::CommandExt;

    use crate::flight_gate::FlightGate;

    // The recorded plan text, best-effort from disk; absent is a tool fault,
    // not a skill-quality verdict. The reader gh is uncapped (one call).
    let reader_gh = CappedGh {
        runner: &default_gh,
        cap: usize::MAX,
        calls: std::cell::Cell::new(0),
    };
    let node = item
        .get("graph_node_id")
        .and_then(Value::as_str)
        .and_then(|nid| by_id.get(nid));
    let plan_text = read_plan_text(item, node, &reader_gh);
    let Some(plan_text) = plan_text else {
        emit_finding(
            &Finding {
                run_id,
                item,
                dimension: "collision_free",
                verdict: "fail",
                evidence: format!(
                    "replay tool-fault: recorded input for {corpus_item} unresolvable"
                ),
                cost_usd: 0.0,
                skill_ref,
                tool_fault: false,
            },
            epaths,
        );
        println!("tool-fault: recorded input for {corpus_item} unresolvable; emitted tool-fault finding.");
        return 1;
    };

    let repo_root = crate::paths::worktree_repo_root(cwd);
    let item12: String = corpus_item.chars().take(12).collect();
    let worktree_name = format!("observer-{skill}-{item12}");
    let scratch = crate::paths_cli::worktrees_base(cwd)
        .join(repo_root.file_name().unwrap_or_default())
        .join(&worktree_name);
    let claim_key = format!("observer:{skill}:{worktree_name}");
    let flight = crate::flight_gate::acquire_flight(
        &claim_key,
        corpus_item,
        30 * 60 * 1000,
        &format!("observer replay {run_id}"),
        Some(std::process::id()),
        None,
        None,
    );
    let FlightGate::Acquired(holder) = &flight else {
        println!("another replay holds {corpus_item}, or its claim gate is unavailable; exiting without touching its worktree.");
        return 4;
    };
    let holder: &str = holder;
    let cleanup = |holder: &str| {
        let _ = std::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&scratch)
            .current_dir(&repo_root)
            .output();
        let _ = crate::claims::release(&claim_key, holder, None, None);
    };

    let ref_word = skill_ref.unwrap_or("HEAD").to_string();
    let run = |cmd: &[&str]| -> (i32, String, String) {
        match std::process::Command::new("git")
            .args(cmd)
            .current_dir(&repo_root)
            .output()
        {
            Ok(out) => (
                out.status.code().unwrap_or(1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ),
            Err(e) => (1, String::new(), e.to_string()),
        }
    };
    run(&["worktree", "prune"]);
    run(&[
        "worktree",
        "remove",
        "--force",
        &scratch.display().to_string(),
    ]);
    let (add_rc, _add_out, add_err) = run(&[
        "worktree",
        "add",
        "--force",
        &scratch.display().to_string(),
        &ref_word,
    ]);
    if add_rc != 0 {
        eprintln!(
            "replay fault: git worktree add failed: {}",
            add_err.trim().chars().take(300).collect::<String>()
        );
        cleanup(holder);
        return 1;
    }
    write_workdir_settings(&scratch);

    // Feed the recorded input to /blueprint headless (fable-tier, sized to a
    // real blueprint run, NOT verify_advise's 90s).
    let prompt = format!(
        "Run /blueprint on the following design doc and output the resulting \
         implementation plan (with a `## Failure Modes` section and a \
         `## Execution Strategy` yaml block declaring per-task `surface` file \
         ownership). Design doc:\n\n{}",
        plan_text.chars().take(20000).collect::<String>()
    );
    let spawn_name = format!("observer-replay-{item12}");
    let scratch_str = scratch.display().to_string();
    let spawned = std::process::Command::new(crate::scrape::fno_bin())
        .args([
            "agents",
            "spawn",
            "--name",
            &spawn_name,
            &prompt,
            "--harness",
            "claude",
            "--once",
            "--cwd",
            &scratch_str,
            "--timeout",
            "600",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn();
    let (rc, out, err) = match spawned {
        Ok(child) => crate::blueprint_judge::bounded_wait(child, 630)
            .unwrap_or_else(|e| (1, String::new(), e)),
        Err(e) => (1, String::new(), e.to_string()),
    };

    let cost_usd = read_scratch_cost(&scratch);
    let cap = replay_cost_cap(cwd);
    if cost_usd > cap {
        eprintln!(
            "warning: replay of {corpus_item} spent ${cost_usd:.2}, over the ${cap:.2} cap \
             (the 600s spawn timeout is the hard pre-spawn bound; dollar caps are post-hoc in v1)."
        );
    }

    // DETECTIVE scan FIRST, before any emit: an isolation violation is the
    // ONE hard failure and voids the whole replay (a "successful" event for a
    // tainted run would be a real-state leak recorded as skill quality).
    let ids = isolation::collect_eval_session_ids(&scratch, None).0;
    let iso = isolation::check_isolation(&ids, &isolation::default_real_state_paths(&repo_root));
    if iso.verdict == "violated" {
        eprintln!("{}", isolation::format_violation_report(&iso));
        cleanup(holder);
        return 3;
    }

    let mut tool_fault_state = false;
    if rc != 0 || out.trim().is_empty() {
        // A replay TOOL fault (spawn crash/timeout/unparseable), tagged
        // tool_fault=True so downstream never counts it as a skill-quality
        // structural fail.
        emit_finding(
            &Finding {
                run_id,
                item,
                dimension: "collision_free",
                verdict: "fail",
                evidence: format!(
                    "replay spawn tool-fault rc={rc}: {}",
                    err.chars().take(200).collect::<String>()
                ),
                cost_usd,
                skill_ref,
                tool_fault: true,
            },
            epaths,
        );
        println!("tool-fault: replay spawn for {corpus_item} rc={rc}; emitted tool-fault finding.");
        tool_fault_state = true;
    } else {
        // A1: replay scores STRUCTURAL dimensions only (no shipped_outcome).
        let mut replay_item = item.as_object().cloned().unwrap_or_default();
        replay_item.insert("include_shipped_outcome".to_string(), json!(false));
        let replay_item = Value::Object(replay_item);
        let collisions = surface_collisions(&out, &repo_root);
        let scores = fold::score_blueprint_item(&replay_item, collisions.as_deref());
        let mut findings: Vec<(String, String)> = Vec::new();
        if let Some(entries) = scores.as_object() {
            for (dimension, verdict) in entries {
                let Some(v) = verdict.as_str() else {
                    continue; // coverage gap, never a fabricated verdict
                };
                findings.push((dimension.clone(), v.to_string()));
                emit_finding(
                    &Finding {
                        run_id,
                        item,
                        dimension,
                        verdict: v,
                        evidence: format!("replay {ref_word} {corpus_item}: {dimension}={v}"),
                        cost_usd,
                        skill_ref,
                        tool_fault: false,
                    },
                    epaths,
                );
            }
        }
        let scored = if findings.is_empty() { 0 } else { 1 };
        let item_version = item
            .get("skill_version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let mut summary = fold::build_run_summary(
            run_id,
            &skill_id,
            &item_version,
            &findings,
            1, // truthful: a single-item replay batch
            scored,
            skill_ref,
            false, // replay is a targeted before/after, not a >=10 trend
        );
        if let Some(obj) = summary.as_object_mut() {
            obj.insert("cost_usd".to_string(), json!(cost_usd));
            obj.insert("skill_ref".to_string(), json!(skill_ref));
        }
        if !emit_run_complete(&summary, epaths) {
            eprintln!(
                "error: could not write the canonical skill_eval_run_complete event; \
                 the replay is not recorded (the digest alone is not the machine contract)."
            );
            cleanup(holder);
            return 5;
        }
        write_sweep_digest(&summary, skill, "replay", cwd);
        println!("ok: replayed {corpus_item} under {ref_word} (cost=${cost_usd:.2})");
    }
    cleanup(holder);
    if tool_fault_state {
        return 1;
    }
    0
}

/// `_PR_LIST_LIMIT` (cli.py:600): gh pr list is generous because one call
/// per repo covers the window; the truncation caveat fires past the cap.
fn pr_list_limit() -> usize {
    std::env::var("FNO_OBSERVER_PR_LIST_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600)
}

/// `_repos_in_scope` (cli.py:608): an explicit --repo, else the graph's
/// distinct PR-url repos, sorted.
fn repos_in_scope(nodes: &[Value], repo_override: Option<&str>) -> Vec<String> {
    if let Some(repo) = repo_override {
        return vec![repo.to_string()];
    }
    let mut repos: Vec<String> = nodes
        .iter()
        .filter_map(|n| fold::repo_from_url(n.get("pr_url").and_then(Value::as_str)))
        .collect();
    repos.sort();
    repos.dedup();
    repos
}

/// `_gh_pr_list` (cli.py:624): merged+closed PRs for one repo, windowed by
/// merged/closed date on the fold's naive-local timeline. `(None, _)` is a
/// gh failure (a coverage gap for the whole repo, never a crash);
/// `truncated` fires at exactly the list limit (no silent truncation).
fn gh_pr_list(
    repo: &str,
    cutoff: chrono::NaiveDateTime,
    now: chrono::NaiveDateTime,
    gh: &CappedGh<'_>,
) -> (Option<Vec<Value>>, bool) {
    let cap = pr_list_limit();
    let limit = cap.to_string();
    let args: Vec<String> = vec![
        "pr".into(),
        "list".into(),
        "--repo".into(),
        repo.to_string(),
        "--state".into(),
        "all".into(),
        "--limit".into(),
        limit.clone(),
        "--json".into(),
        "number,headRefName,mergedAt,closedAt,url,state".into(),
    ];
    let (rc, out, _err) = gh.call(&args);
    if rc != 0 {
        return (None, false);
    }
    let Ok(raw) = serde_json::from_str::<Value>(out.trim()) else {
        return (None, false);
    };
    let Some(list) = raw.as_array() else {
        return (Some(Vec::new()), false);
    };
    let truncated = list.len() >= cap;
    let mut windowed: Vec<Value> = Vec::new();
    for pr in list {
        if !pr.is_object() {
            continue;
        }
        // mergedAt/closedAt through parse_ts only: a bare ISO parse would
        // land the window boundary off by the tz offset.
        let ts = fold::parse_ts(pr.get("mergedAt").unwrap_or(&Value::Null))
            .or_else(|| fold::parse_ts(pr.get("closedAt").unwrap_or(&Value::Null)));
        if let Some(ts) = ts {
            if cutoff <= ts && ts <= now {
                windowed.push(pr.clone());
            }
        }
    }
    (Some(windowed), truncated)
}

/// `_read_target_events` (cli.py:715): the two structured event kinds that
/// carry target signal - loop_check and termination - grouped by
/// `data.session_id`, across both journals.
fn read_target_events(cwd: &Path) -> HashMap<String, Vec<Value>> {
    let mut by_session: HashMap<String, Vec<Value>> = HashMap::new();
    for p in events_paths(cwd) {
        let text = crate::event_store::journal_text(&p, &["loop_check", "termination"]);
        for line in text.lines() {
            let Ok(e) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let sid = e
                .get("data")
                .and_then(|d| d.get("session_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !sid.is_empty() {
                by_session.entry(sid).or_default().push(e);
            }
        }
    }
    by_session
}

/// `_target_evidence` (cli.py:734): one earned-specificity line per
/// (PR, dimension) verdict.
fn target_evidence(item: &Value, dimension: &str, verdict: &str) -> String {
    // Python str() of a missing value reads "None"; a JSON null reads the
    // same way in the evidence lines.
    let py = |key: &str| -> String {
        item.get(key)
            .filter(|v| !v.is_null())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "None".to_string())
    };
    let nid = item
        .get("graph_node_id")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "?".to_string());
    let prn = py("pr_number");
    let sig = item.get("signals").cloned().unwrap_or(json!({}));
    let sig_py = |key: &str| -> String {
        sig.get(key)
            .filter(|v| !v.is_null())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "None".to_string())
    };
    if dimension == "shipped_outcome" {
        return format!(
            "pr#{prn} node {nid} outcome={} attribution={}",
            py("outcome"),
            py("attribution_class")
        );
    }
    if dimension == "first_try_green" {
        return format!(
            "pr#{prn} node {nid} ci_reds={} loop_fires={}",
            sig_py("ci_reds"),
            sig_py("loop_fires"),
        );
    }
    if dimension == "converged" {
        return format!("pr#{prn} node {nid} terminal={}", py("terminal_reasons"));
    }
    format!("pr#{prn} node {nid}: {dimension}={verdict}")
}

/// The target sweep (cli.py `_sweep_target`): enumerate real merged+closed
/// PRs per scoped repo, walk each back to its node/sessions, score the three
/// verdict dimensions, and emit the falsifier cross-tab plus the
/// survivorship-corrected no-PR class.
fn sweep_target(cwd: &Path, since: i64, repo: Option<String>, json_out: bool) -> i32 {
    let gh = CappedGh {
        runner: &default_gh,
        cap: gh_cap(),
        calls: std::cell::Cell::new(0),
    };
    let rows = match ledger_rows(cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let nodes =
        crate::backlog::read_entries(&crate::paths_cli::graph_json(cwd)).unwrap_or_default();
    let postmortems = read_postmortems(cwd);
    let events_by_session = read_target_events(cwd);
    let now = now_local();
    let cutoff = now - chrono::Duration::days(since);
    let repos = repos_in_scope(&nodes, repo.as_deref());

    let mut all_prs: Vec<Value> = Vec::new();
    let mut dropped_repos = 0usize;
    let mut truncated_repos: Vec<String> = Vec::new();
    for r in &repos {
        let (prs, truncated) = gh_pr_list(r, cutoff, now, &gh);
        match prs {
            None => dropped_repos += 1,
            Some(prs) => {
                all_prs.extend(prs);
                if truncated {
                    truncated_repos.push(r.clone());
                }
            }
        }
    }
    let corpus = fold::build_target_corpus(
        &all_prs,
        &nodes,
        &rows,
        &events_by_session,
        since,
        now,
        &postmortems,
    );
    let items = corpus
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let coverage = corpus.get("coverage").cloned().unwrap_or(json!({}));
    let meta = json!({
        "repos_scoped": repos.len(),
        "dropped_repos": dropped_repos,
        "truncated_repos": truncated_repos,
    });
    let skill_id = fold::skill_id_for("target");
    let prs_total = coverage
        .get("prs_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // Boundary: 0 merged/closed PRs. A genuine empty window exits 0; every
    // scoped repo's gh call failing means the denominator was never
    // observed, so that outage reports as a failure, not a fabricated
    // no_data.
    if prs_total == 0 {
        if !repos.is_empty() && dropped_repos >= repos.len() {
            eprintln!(
                "partial: gh unavailable for all {} scoped repo(s) \
                 (fan-out cap / rate-limit / outage); the PR denominator was never observed, \
                 NOT an empty window. No skill_eval_run_complete emitted.",
                repos.len()
            );
            return 1;
        }
        println!(
            "no_data: 0 merged/closed PR(s) across {} repo(s) in the last {since}d. \
             No skill_eval_run_complete emitted.",
            repos.len()
        );
        return 0;
    }

    // Insufficient guard: <10 attributable PRs -> a ranking would be
    // fabricated confidence. No run_complete (matches blueprint/review).
    if items.len() < fold::MIN_ATTRIBUTABLE {
        println!(
            "insufficient: {} attributable {skill_id} PR item(s) in the last {since}d, \
             need >={}. No skill_eval_run_complete emitted.",
            items.len(),
            fold::MIN_ATTRIBUTABLE
        );
        println!(
            "  (denominator was {prs_total} PR(s); no_pr_attempts={}, unattributed={})",
            coverage
                .get("no_pr_attempts")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            coverage
                .get("unattributed_pr")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        return 0;
    }

    let run_id = mint_run_id(&skill_id);
    let epaths = events_paths(cwd);
    let repo_root = crate::paths::worktree_repo_root(cwd);
    let skill_version = fold::default_skill_version(&skill_id, None, &repo_root);

    let mut findings: Vec<(String, String)> = Vec::new();
    let mut scored_pairs: Vec<(Value, Value)> = Vec::new();
    let mut scored_count = 0usize;
    for item in &items {
        let scores = fold::score_target_item(item);
        scored_pairs.push((item.clone(), scores.clone()));
        // corpus_item_id must be unique across repos: PR numbers are
        // repo-local, so the id carries the repo too.
        let mut emit_item = item.as_object().cloned().unwrap_or_default();
        emit_item.insert("skill_id".to_string(), json!(skill_id));
        emit_item.insert(
            "session_id".to_string(),
            json!(format!(
                "pr-{}#{}",
                item.get("repo").and_then(Value::as_str).unwrap_or("None"),
                item.get("pr_number")
                    .filter(|v| !v.is_null())
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "None".to_string())
            )),
        );
        emit_item.insert("skill_version".to_string(), json!(skill_version));
        let emit_item = Value::Object(emit_item);
        let mut item_scored = false;
        if let Some(entries) = scores.as_object() {
            for (dimension, verdict) in entries {
                let Some(v) = verdict.as_str() else {
                    continue; // coverage gap, never a fabricated verdict
                };
                item_scored = true;
                findings.push((dimension.clone(), v.to_string()));
                emit_finding(
                    &Finding {
                        run_id: &run_id,
                        item: &emit_item,
                        dimension,
                        verdict: v,
                        evidence: target_evidence(item, dimension, v),
                        cost_usd: 0.0,
                        skill_ref: None,
                        tool_fault: false,
                    },
                    &epaths,
                );
            }
        }
        if item_scored {
            scored_count += 1;
        }
    }

    let mut summary = fold::build_run_summary(
        &run_id,
        &skill_id,
        &skill_version,
        &findings,
        items.len(),
        scored_count,
        None,
        true,
    );
    if let Some(obj) = summary.as_object_mut() {
        obj.insert("cost_usd".to_string(), json!(0.0));
    }
    if !emit_run_complete(&summary, &epaths) {
        eprintln!(
            "error: could not write the canonical skill_eval_run_complete event; \
             the sweep is not recorded (the digest alone is not the machine contract)."
        );
        return 5;
    }

    let crosstab = digest::crosstab(&scored_pairs);
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let dir = crate::paths_cli::observer_reports_dir(cwd, Some("fno"));
    let _ = std::fs::create_dir_all(&dir);
    let digest_path = dir.join(format!("target-{date}.md"));
    let body = digest::target_digest_body(&summary, &coverage, &meta, &crosstab, &date);
    let _ = std::fs::write(&digest_path, body);

    let unattributed = coverage
        .get("unattributed_pr")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let fully_covered = scored_count == items.len()
        && unattributed == 0
        && dropped_repos == 0
        && truncated_repos.is_empty();
    let state = if fully_covered { "ok" } else { "partial" };
    let coverage_pct = summary
        .get("coverage_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let pass = summary
        .get("pass_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let degraded = summary
        .get("degraded_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let fail = summary
        .get("fail_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if json_out {
        let mut payload = summary.as_object().cloned().unwrap_or_default();
        payload.insert("state".to_string(), json!(state));
        payload.insert(
            "digest".to_string(),
            json!(digest_path.display().to_string()),
        );
        payload.insert("coverage".to_string(), coverage);
        payload.insert("meta".to_string(), meta);
        payload.insert(
            "falsifier_prs".to_string(),
            crosstab.get("falsifier_prs").cloned().unwrap_or(json!([])),
        );
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Object(payload)).unwrap_or_default()
        );
        return 0;
    }
    let fals: Vec<Value> = crosstab
        .get("falsifier_prs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let fals_word = if fals.is_empty() {
        "(none)".to_string()
    } else {
        fals.iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!(
        "{state}: {skill_id} run {run_id}\n  denominator {prs_total} PR(s); scored \
         {scored_count}/{} attributable ({coverage_pct}% coverage)\n  verdicts: \
         pass={pass} degraded={degraded} fail={fail}\n  no_pr_attempts: {}; \
         falsifier PRs: {fals_word}\n  digest: {}",
        items.len(),
        coverage
            .get("no_pr_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        digest_path.display()
    );
    0
}

/// Parity for the pure fold against the frozen oracle goldens.
///
/// parity-stage: characterization
/// parity-oracle: cli/src/fno/observer/fold.py + cli.py (deleted in this
/// change). `crates/fno-agents/tests/golden/observer/fold_golden.json` was
/// captured from the Python leg before deletion; these tests drive the same
/// pure functions and compare parsed values. The build_corpus and
/// target_corpus golden sections record outputs whose fixture rows were not
/// frozen, so they stay documentational here; the four drivable sections
/// carry the parity claim.
#[cfg(test)]
mod parity_tests {
    use super::*;

    fn golden() -> Value {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/observer/fold_golden.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("the frozen goldens exist"))
            .expect("the goldens parse")
    }

    #[test]
    fn classify_postmortem_matches_oracle() {
        let got = [
            fold::classify_postmortem(Some("missing_dependency")),
            fold::classify_postmortem(Some("test_failure")),
            fold::classify_postmortem(Some("taste")),
            fold::classify_postmortem(None),
        ];
        let want = golden()["classify_postmortem"].clone();
        assert_eq!(json!(got), want);
    }

    #[test]
    fn score_blueprint_matches_oracle() {
        let g = golden();
        let bounced = |attribution: &str| json!({"judgeable": true, "outcome": "bounced", "attribution_class": attribution});
        let collision = vec!["shared".to_string()];
        assert_eq!(
            fold::score_blueprint_item(&bounced("execution-attributable"), Some(&collision)),
            g["score_blueprint"]["exec_bounce"]
        );
        assert_eq!(
            fold::score_blueprint_item(&bounced("plan-attributable"), Some(&collision)),
            g["score_blueprint"]["plan_bounce"]
        );
        assert_eq!(
            fold::score_blueprint_item(&json!({"judgeable": false}), None),
            g["score_blueprint"]["unjudgeable"]
        );
    }

    #[test]
    fn run_summary_matches_oracle() {
        let g = golden();
        assert_eq!(
            fold::build_run_summary("r", "fno:blueprint", "v", &[], 3, 0, None, true),
            g["run_summary_insufficient"]
        );
        let findings = vec![
            ("five_questions".to_string(), "pass".to_string()),
            ("plan_shape".to_string(), "degraded".to_string()),
            ("collision_free".to_string(), "fail".to_string()),
            ("surface_fit".to_string(), "fail".to_string()),
        ];
        assert_eq!(
            fold::build_run_summary(
                "obs-fno:blueprint-x",
                "fno:blueprint",
                "abc1234",
                &findings,
                12,
                10,
                Some("abc1234"),
                true,
            ),
            g["run_summary_ok"]
        );
    }

    #[test]
    fn surface_collisions_match_oracle() {
        let g = golden();
        let colliding = vec!["E\tparallel tasks share surface 'b.py': 1.1, 1.2".to_string()];
        assert_eq!(
            fold::collisions_from_check(Some(&colliding)),
            Some(
                g["surface_collisions_colliding"]
                    .as_array()
                    .expect("oracle holds the collision line")
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<String>>()
            )
        );
        assert_eq!(fold::collisions_from_check(None), None);
        assert_eq!(
            json!(fold::collisions_from_check(None)),
            g["surface_collisions_plain"]
        );
    }
}

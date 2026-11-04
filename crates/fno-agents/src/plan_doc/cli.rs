//! `fno do plan <leaf>`: the plan command group, ported whole from the
//! deleted `cli/src/fno/plan/cli.py`. The front door claims `do plan` and
//! routes the tail here as the `plan-cli` family door. Leaves:
//!
//!   stamp | graduate | set-expected   raw flags to the plan-doc writer
//!   path              the save path for a NEW plan/design doc
//!   brief             a scoped task brief from a single-doc plan
//!   parse             one plan doc as JSON, for the skill scripts (hidden)
//!   validate          frontmatter, or --execution semantics (read-only)
//!   folder-audit      folder plans owned by a non-terminal node
//!   rung              the readiness rung and dispatch verdict (hidden)
//!   reconcile-status  normalize drifted frontmatter status in place
//!   migrate-keys      collapse synonym frontmatter keys
//!   sync              converge every doc's graph->doc mirror fields

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const USAGE: &str = "usage: fno do plan <command> [args]
  stamp|graduate|set-expected <flags>   plan-doc writer commands
  path --slug S [--node N] [--name-only]
  brief <plan> --task ID [--include-failure-modes all|relevant|none]
        [--include-locked-decisions all|relevant|none] [--format markdown|json]
  validate <plan> [--json] [--execution [--acceptance-contract C]]
  folder-audit [--non-terminal] [--plans-dir D]
  reconcile-status [--plans-dir D] [--apply]
  migrate-keys [--plans-dir D] [--apply]
  sync [--all]";

pub fn run(args: &[String]) -> i32 {
    let Some((leaf, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    match leaf.as_str() {
        "stamp" | "graduate" | "set-expected" => run_writer(leaf, rest),
        "path" => crate::plans_path::run_plan_path(rest),
        "brief" => run_brief(rest),
        "parse" => run_parse(rest),
        "validate" => run_validate(rest),
        "folder-audit" => run_folder_audit(rest),
        "rung" => run_rung(rest),
        "reconcile-status" => run_reconcile_status(rest),
        "migrate-keys" => run_migrate_keys(rest),
        "sync" => run_sync(rest),
        "-h" | "--help" => {
            println!("{USAGE}");
            0
        }
        other => {
            eprintln!("fno do plan: unknown command {other:?}\n{USAGE}");
            2
        }
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// One store method run in process, the same exec the keeper serves.
fn store(method: &str, params: Value) -> Result<Value, String> {
    let cfg = crate::store_exec::ExecConfig {
        graph: crate::backlog::settings::graph_path(),
        canonical: true,
        lock_timeout: crate::graph_store::DEFAULT_LOCK_TIMEOUT,
        events: Some(crate::paths::events_path(&cwd())),
    };
    let payload = json!({"id": 1, "method": method, "params": params}).to_string();
    let reply = crate::store_exec::exec_reply(&cfg, payload.as_bytes());
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        let err = &reply["error"];
        return Err(err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string()));
    }
    Ok(reply.get("result").cloned().unwrap_or(Value::Null))
}

/// The plan-docs call every writer leaf shares: warnings print, an
/// unreachable writer warns and reads as None.
fn plan_docs(mut params: Value) -> Option<Value> {
    params["cwd"] = json!(cwd().display().to_string());
    params["events_path"] = json!(crate::paths::project_events_path().display().to_string());
    match store("plan_docs", params) {
        Ok(result) => {
            for line in result["warnings"].as_array().into_iter().flatten() {
                eprintln!("{}", line.as_str().unwrap_or_default());
            }
            Some(result)
        }
        Err(e) => {
            eprintln!("warning: plan-doc writer unreachable ({e}); run `fno doctor`");
            None
        }
    }
}

fn run_writer(command: &str, rest: &[String]) -> i32 {
    let params = json!({"op": "argv", "verb": command, "args": rest}); // verb-wording: allow (keeper argv key)
    let reply = plan_docs(params);
    reply
        .and_then(|r| r["exit"].as_i64())
        .map(|code| code as i32)
        .unwrap_or(1)
}

/// A relative path resolves against the repo root when the file exists
/// there, else stays as given.
fn resolve_plan_arg(raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p;
    }
    if let Some(root) = crate::paths_cli::repo_root() {
        let candidate = root.join(&p);
        if candidate.exists() {
            return candidate;
        }
    }
    p
}

fn flag<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    let at = rest.iter().position(|a| a == name)?;
    rest.get(at + 1).map(String::as_str)
}

fn has(rest: &[String], names: &[&str]) -> bool {
    rest.iter().any(|a| names.contains(&a.as_str()))
}

/// The first token that is neither a flag nor a flag's value.
fn positional<'a>(rest: &'a [String], valued: &[&str]) -> Option<&'a str> {
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if valued.contains(&arg.as_str()) {
            it.next();
        } else if !arg.starts_with('-') {
            return Some(arg);
        }
    }
    None
}

fn filter_mode(rest: &[String], name: &str) -> Result<&'static str, String> {
    match flag(rest, name).unwrap_or("relevant") {
        "all" => Ok("all"),
        "relevant" => Ok("relevant"),
        "none" => Ok("none"),
        other => Err(format!("{name}: expected all|relevant|none, got {other:?}")),
    }
}

/// Exit codes: 0 success, 1 plan not found, 2 contract violation, 3
/// malformed YAML.
fn run_brief(rest: &[String]) -> i32 {
    const VALUED: [&str; 4] = [
        "--task",
        "--include-failure-modes",
        "--include-locked-decisions",
        "--format",
    ];
    let (Some(plan), Some(task)) = (positional(rest, &VALUED), flag(rest, "--task")) else {
        eprintln!("fno do plan brief: <plan> and --task are required");
        return 2;
    };
    let (fm_mode, ld_mode) = match (
        filter_mode(rest, "--include-failure-modes"),
        filter_mode(rest, "--include-locked-decisions"),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("fno do plan brief: {e}");
            return 2;
        }
    };
    let json_out = match flag(rest, "--format").unwrap_or("markdown") {
        "markdown" => false,
        "json" => true,
        other => {
            eprintln!("fno do plan brief: --format: expected markdown|json, got {other:?}");
            return 2;
        }
    };
    let resolved = resolve_plan_arg(plan);
    if !resolved.exists() {
        eprintln!("fno do plan brief: plan file not found: {plan}");
        return 1;
    }
    let doc = match super::doc::load_plan(&resolved) {
        Ok(doc) => doc,
        Err(e) => {
            eprintln!(
                "fno do plan brief: malformed frontmatter in {}: {e}",
                resolved.display()
            );
            return 3;
        }
    };
    match super::brief::build_brief(&doc, task, fm_mode, ld_mode) {
        Ok(result) if json_out => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result.to_json()).unwrap_or_default()
            );
            0
        }
        Ok(result) => {
            println!("{}", result.to_markdown());
            0
        }
        Err(e) if e.0.starts_with("exit3:") => {
            let detail = &e.0["exit3:".len()..];
            eprintln!("fno do plan brief: malformed Execution Strategy YAML: {detail}");
            3
        }
        Err(e) => {
            eprintln!("fno do plan brief: {e}");
            2
        }
    }
}

/// The sections and frontmatter keys /blueprint may write. Every other
/// section belongs to /think or /execute.
pub const BLUEPRINT_WRITE_ALLOWLIST: &[&str] = &[
    "Execution Strategy",
    "File Ownership Map",
    "Five questions",
    "Patterns to Reuse",
    // Frontmatter keys share the allowlist, so the ownership model covers
    // frontmatter writes too.
    "kill_criteria",
    "execution_mode",
    "waves",
    // The compiled-v1 stamp finalize sets on a design -> ready promotion.
    "acceptance_contract",
    // The consolidation block the validator requires.
    "consolidation",
    // The band the date-keyed difficulty gate requires on a new plan.
    "difficulty",
    // manual|auto; init-target-state.sh fires `fno backlog join` on auto.
    "join",
    // The fidelity gate refuses carve-outs on `forbidden`.
    "carveouts",
];

/// The monotonic plan axis; `done` and `superseded` sit off it.
pub const STATUS_PROGRESSION: &[&str] = &["idea", "design", "ready", "in_progress", "in_review"];

/// `parse <plan>|-`: one plan doc as JSON, for the Python skill scripts that
/// shape a doc but must not parse one. `-` reads the text from stdin. The
/// receipt carries the frontmatter, the ordered sections, the parsed
/// Execution Strategy (or its error) with its edge errors, and the status
/// ladder plus the blueprint write allowlist the scripts check against.
/// Exit 0 parsed, 1 unreadable, 3 malformed frontmatter.
fn run_parse(rest: &[String]) -> i32 {
    let Some(plan) = rest.first() else {
        eprintln!("fno do plan parse: <plan> or - is required");
        return 2;
    };
    let loaded = if plan == "-" {
        let mut text = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text) {
            println!("{}", json!({"error": format!("cannot read stdin: {e}")}));
            return 1;
        }
        super::doc::load_plan_text(&text)
    } else {
        let resolved = resolve_plan_arg(plan);
        if let Err(e) = std::fs::metadata(&resolved) {
            println!("{}", json!({"error": format!("cannot read {plan}: {e}")}));
            return 1;
        }
        super::doc::load_plan(&resolved)
    };
    let doc = match loaded {
        Ok(doc) => doc,
        Err(e) => {
            println!("{}", json!({"error": e.message, "line": e.line}));
            return 3;
        }
    };
    let (strategy, strategy_error, edge_errors) = match doc.get_section("Execution Strategy") {
        None => (Value::Null, Value::Null, Vec::new()),
        Some(section) => match super::brief::parse_execution_strategy(section) {
            Ok(parsed) => {
                let edges = super::brief::validate_task_edges(&parsed);
                (parsed, Value::Null, edges)
            }
            Err(e) => (Value::Null, json!(e.0), Vec::new()),
        },
    };
    let aliases: serde_json::Map<String, Value> = crate::graph_store::PLAN_STATUS_ALIASES
        .iter()
        .map(|(from, to)| (from.to_string(), json!(to)))
        .collect();
    println!(
        "{}",
        json!({
            "frontmatter": doc.frontmatter,
            "sections": doc.sections,
            "strategy": strategy,
            "strategy_error": strategy_error,
            "edge_errors": edge_errors,
            "status_progression": STATUS_PROGRESSION,
            "status_aliases": aliases,
            "blueprint_write_allowlist": BLUEPRINT_WRITE_ALLOWLIST,
        })
    );
    0
}

/// Epic-decomposition group nodes carry a `<doc>#group-<slug>` plan_path;
/// strip the fragment when the literal is absent and the base exists.
fn strip_group_fragment(resolved: PathBuf) -> PathBuf {
    if resolved.exists() {
        return resolved;
    }
    let name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let Some((base, _)) = name.rsplit_once("#group-") else {
        return resolved;
    };
    if base.is_empty() {
        return resolved;
    }
    let stripped = resolve_plan_arg(&resolved.with_file_name(base).to_string_lossy());
    if stripped.exists() {
        stripped
    } else {
        resolved
    }
}

fn run_validate(rest: &[String]) -> i32 {
    let Some(plan) = positional(rest, &["--acceptance-contract"]) else {
        eprintln!("fno do plan validate: <plan> is required");
        return 2;
    };
    let json_out = has(rest, &["--json", "-J"]);
    let resolved = strip_group_fragment(resolve_plan_arg(plan));
    let shown = resolved.display().to_string();
    let doc = match super::doc::load_plan(&resolved) {
        Ok(doc) => doc,
        Err(e) => {
            let msg = format!("cannot load plan {plan}: {e}");
            if json_out {
                eprintln!("{}", json!({"loaded": false, "error": msg}));
            } else {
                eprintln!("{msg}");
            }
            return 1;
        }
    };
    if has(rest, &["--execution"]) {
        return match super::exec_validate::op_validate_execution(
            &resolved,
            json_out,
            flag(rest, "--acceptance-contract"),
        ) {
            Ok(mut report) => {
                let valid = report["valid"].as_bool() == Some(true);
                if json_out {
                    println!("{report}");
                } else {
                    let text = report
                        .as_object_mut()
                        .and_then(|o| o.remove("text"))
                        .and_then(|t| t.as_str().map(str::to_string))
                        .unwrap_or_default();
                    // The pass line prints on stdout and every other line
                    // (violations, warnings) on stderr.
                    for line in text.lines() {
                        if valid && line.starts_with("valid execution:") {
                            println!("{line}");
                        } else {
                            eprintln!("{line}");
                        }
                    }
                }
                if valid {
                    0
                } else {
                    1
                }
            }
            Err(msg) => {
                eprintln!("{msg}");
                1
            }
        };
    }
    let fm = doc.frontmatter.as_object().cloned().unwrap_or_default();
    let errors = crate::plan_schema::validate_frontmatter(&fm);
    if errors.is_empty() {
        if json_out {
            println!("{}", json!({"valid": true, "path": shown}));
        } else {
            println!("valid: {shown}");
        }
        return 0;
    }
    if json_out {
        let violations: Vec<Value> = errors
            .iter()
            .map(|e| {
                json!({
                    "field": e.get("field").and_then(Value::as_str).filter(|f| !f.is_empty()).unwrap_or("<root>"),
                    "message": e.get("message").cloned().unwrap_or(json!("")),
                    "got": e.get("got").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        println!(
            "{}",
            json!({"valid": false, "path": shown, "violations": violations})
        );
    } else {
        eprintln!("invalid: {shown} ({} violation(s))", errors.len());
        for line in violation_lines(&errors) {
            eprintln!("  {line}");
        }
    }
    1
}

/// One `field: message (got X)` line per schema error. The validator omits
/// `got` on a missing field, so the value prints only when it was present.
fn violation_lines(errors: &[Value]) -> Vec<String> {
    errors
        .iter()
        .map(|e| {
            let field = e
                .get("field")
                .and_then(Value::as_str)
                .filter(|f| !f.is_empty())
                .unwrap_or("<root>");
            let message = e.get("message").and_then(Value::as_str).unwrap_or("");
            let suffix = match e.get("got") {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(got)) => {
                    format!(" (got {})", crate::client_commands::py_repr_str(got))
                }
                Some(got) => format!(" (got {got})"),
            };
            format!("{field}: {message}{suffix}")
        })
        .collect()
}

/// The frontmatter check `validate` runs, for an in-process caller: one line
/// per violation, empty when the plan is valid, Err when it cannot load.
pub(crate) fn frontmatter_violations(plan: &Path) -> Result<Vec<String>, String> {
    let resolved = strip_group_fragment(plan.to_path_buf());
    let doc = super::doc::load_plan(&resolved)
        .map_err(|e| format!("cannot load plan {}: {e}", resolved.display()))?;
    let fm = doc.frontmatter.as_object().cloned().unwrap_or_default();
    Ok(violation_lines(&crate::plan_schema::validate_frontmatter(
        &fm,
    )))
}

fn plans_dir_of(rest: &[String]) -> PathBuf {
    if let Some(dir) = flag(rest, "--plans-dir") {
        return PathBuf::from(dir);
    }
    let anchor = crate::paths_cli::repo_root().unwrap_or_else(cwd);
    crate::plans_path::plans_content_dir(&anchor).unwrap_or_else(|| anchor.join(".fno/plans"))
}

fn run_folder_audit(rest: &[String]) -> i32 {
    let plans_root = plans_dir_of(rest);
    let entries = match crate::graph_store::read_rows(&crate::backlog::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno do plan folder-audit: graph unreadable ({e}) - failing toward defer");
            return 1;
        }
    };
    let Some(owners) = super::folder_audit::scan(&plans_root, &entries) else {
        eprintln!(
            "fno do plan folder-audit: cannot scan plans dir {} - failing toward defer",
            plans_root.display()
        );
        return 1;
    };
    for o in &owners {
        println!("  {} ({}): {}", o.node_id, o.status, o.folder);
    }
    println!("non-terminal folder-plan owners: {}", owners.len());
    if has(rest, &["--non-terminal"]) && !owners.is_empty() {
        return 1;
    }
    0
}

/// Exit 0 when the plan is dispatchable, 1 when it is not (including
/// unreadable), 2 on a usage error. Output is `key=value` lines.
fn run_rung(rest: &[String]) -> i32 {
    let Some(plan) = rest.first().filter(|p| !p.is_empty()) else {
        eprintln!("fno do plan rung: a plan path is required");
        return 2;
    };
    let entry = json!({"id": "-", "plan_path": plan, "cwd": cwd()});
    let r = crate::backlog_ready::plan_rung(&entry);
    let dispatchable = matches!(r, "ready" | "in_progress" | "in_review");
    println!("rung={r}");
    println!("dispatchable={dispatchable}");
    if r == "unreadable" {
        eprintln!(
            "fno do plan rung: cannot classify {plan} (missing, undecodable, \
             malformed frontmatter, or a status this binary does not know)"
        );
    }
    if dispatchable {
        0
    } else {
        1
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

fn run_reconcile_status(rest: &[String]) -> i32 {
    let apply = has(rest, &["--apply"]);
    let archive = crate::backlog::settings::graph_path().with_file_name("graph-archive.json");
    let params = json!({
        "op": "reconcile_status",
        "plans_dir": plans_dir_of(rest).display().to_string(),
        "apply": apply,
        "cwd": cwd().display().to_string(),
        "archive_path": archive.display().to_string(),
    });
    let result = match store("plan_docs", params) {
        Ok(result) => result,
        Err(e) => {
            // A skipped sweep must not read as a clean run.
            eprintln!("  ! plan-doc writer failed ({e}); run `fno doctor`");
            return 1;
        }
    };
    let arrow = if apply { "->" } else { "would ->" };
    for change in result["changes"].as_array().into_iter().flatten() {
        let cell = |i: usize| change[i].as_str().unwrap_or_default().to_string();
        println!("  {}: {} {arrow} {}", file_name(&cell(0)), cell(1), cell(2));
    }
    for warn in result["warnings"].as_array().into_iter().flatten() {
        eprintln!("  ! {}", warn.as_str().unwrap_or_default());
    }
    let prefix = if apply { "" } else { "[dry-run] " };
    println!("{prefix}{}", result["summary"].as_str().unwrap_or_default());
    0
}

fn run_migrate_keys(rest: &[String]) -> i32 {
    let apply = has(rest, &["--apply"]);
    let res = super::migrate::migrate(&plans_dir_of(rest), apply);
    for (path, notes) in &res.changes {
        println!("  {}: {}", file_name(path), notes.join(", "));
    }
    if !res.review_files.is_empty() {
        println!("  needs manual review (kept legacy key untouched):");
        for (path, notes) in &res.review_files {
            println!("    {}: {}", file_name(path), notes.join(", "));
        }
    }
    for warn in &res.warnings {
        eprintln!("  ! {warn}");
    }
    let prefix = if apply { "" } else { "[dry-run] " };
    println!("{prefix}{}", res.summary());
    0
}

/// The sweep's gate sits beside the graph so every session shares one
/// watermark keyed to the one graph. The v2 name keeps an old mtime float
/// from reading as synced.
fn sync_watermark() -> PathBuf {
    crate::backlog::settings::graph_path().with_file_name(".plan-sync-watermark-v2")
}

fn run_sync(rest: &[String]) -> i32 {
    let all = has(rest, &["--all", "-A"]);
    let watermark = sync_watermark();
    let store_version = store("api", json!({"op": "version"}))
        .ok()
        .and_then(|r| r["version"].as_f64());
    // Gate on the store's mutation counter, never a doc mtime: a graph
    // mutation never touches a doc, so a doc gate skips the doc that needs
    // the repaint.
    if !all {
        if let Some(version) = store_version {
            let prev = std::fs::read_to_string(&watermark)
                .ok()
                .and_then(|t| t.trim().parse::<f64>().ok());
            if prev.is_some_and(|p| version <= p) {
                println!("plan sync: graph unchanged since last sync; skipped");
                return 0;
            }
        }
    }
    if crate::backlog::workflows::active_backend_name() != "graph" {
        eprintln!("plan sync: external tracker backend selected; 0 docs repainted");
        return 0;
    }
    // A failed read is a 0-doc no-op that leaves the watermark alone, so the
    // next sweep re-converges.
    let entries = match crate::graph_store::read_rows(&crate::backlog::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("plan sync: graph unreadable ({e}); 0 docs repainted");
            return 0;
        }
    };
    // Claim the sweep window before the walk: parallel sessions then walk at
    // most redundantly, with identical content and atomic writes.
    if let Some(version) = store_version {
        if let Some(parent) = watermark.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&watermark, format!("{version}\n"));
    }
    let ids: Vec<&str> = entries
        .iter()
        .filter(|e| e["plan_path"].as_str().is_some_and(|p| !p.is_empty()))
        .filter_map(|e| e["id"].as_str().filter(|id| !id.is_empty()))
        .collect();
    let repainted = if ids.is_empty() {
        0
    } else {
        plan_docs(json!({"op": "project", "ids": ids, "root": Value::Null}))
            .and_then(|r| r["rewritten"].as_u64())
            .unwrap_or(0) as usize
    };
    println!(
        "plan sync: {repainted} docs repainted, {} unchanged",
        ids.len().saturating_sub(repainted)
    );
    0
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;

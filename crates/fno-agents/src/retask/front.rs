//! The retask front, native: resolve the node row, the worker row, the
//! target coordinate, the thread viewport and the command template, then
//! run the transaction in this process. Ported from the Python `run_retask`
//! front, whose only remaining job was building the rename payload this
//! module now answers directly.
//!
//! # Surface
//!
//! `fno-agents rename retask <worker> --node <id> [--model M] [--effort E]
//! [--registry PATH] [--json]` — the Python `fno agents retask` shim execs
//! this form. `fno-agents rename` with no argv was the old payload door; it
//! is gone with its only producer.
//!
//! The coordinate probe composes `spawn --name retask-probe
//! /fno:{command} {node}` through the side-effect-free compose: the builtin
//! permission rung off, no account pick, in-process. A probe that picked an
//! account or forced the builtin would read as an explicit override and
//! force every retask to respawn.
//!
//! Exit codes: 0 with the receipt as one stdout JSON line for `retasked`;
//! 1 with the refusal receipt for `refused`; 2 for a usage or resolution
//! error (stderr names the problem). The receipt keys and refusal words are
//! the ones the Python front emitted, because the lead loop and
//! `fno backlog advance` read them.

use serde_json::{json, Value};
use std::path::Path;

use super::transport::{self, LiveSeams};
use super::{
    execute_retask, refused_receipt, refused_receipt_from_transport, RetaskRow, RetaskTarget,
};

/// The bounded subprocess seam, injected by the tests the way the Python
/// tests monkeypatched `subprocess.run`.
pub type RunSeam = Box<dyn FnMut(&[String], u64) -> Result<std::process::Output, String>>;

/// A viewport refusal or transport death. `word` is the receipt reason.
pub struct TransportError {
    pub word: String,
    pub detail: Option<String>,
}

/// The `(session, pane_id)` a viewport resolves to.
pub type Viewport = (String, u64);

/// The worker-row facts the viewport reads, narrowed from the registry row.
pub struct ViewportRow {
    pub name: String,
    pub thread_id: Option<String>,
}

/// The thread viewport resolver: open the row's dedicated pane and join it
/// by name + thread id. Ported byte-for-byte from Python
/// `resolve_thread_viewport`, which `fno mail --force` still shares.
pub struct ViewportResolver {
    pub session: String,
    pub run: RunSeam,
}

/// One `(model, value)` flag read off composed argv (`--flag v` or
/// `--flag=v`).
fn flag_value(args: &[String], names: &[&str]) -> Option<String> {
    for (index, token) in args.iter().enumerate() {
        for name in names {
            if token == name && index + 1 < args.len() {
                return Some(args[index + 1].clone());
            }
            if let Some(rest) = token.strip_prefix(&format!("{name}=")) {
                return Some(rest.to_string());
            }
        }
    }
    None
}

impl ViewportResolver {
    /// `FNO_SERVER` > legacy `FNO_SESSION` (one deprecation line) > main.
    pub fn session_from_env() -> String {
        if let Some(server) = std::env::var("FNO_SERVER")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            return server;
        }
        if let Some(legacy) = std::env::var("FNO_SESSION")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            eprintln!("warning: FNO_SESSION is deprecated; use FNO_SERVER instead. The alias will be removed in a future release.");
            return legacy;
        }
        "main".to_string()
    }

    /// The production seam: one bounded `fno` invocation per op.
    pub fn production() -> Self {
        Self {
            session: Self::session_from_env(),
            run: Box::new(|argv, secs| {
                let bin = crate::scrape::fno_bin();
                let mut cmd = std::process::Command::new(bin);
                cmd.args(argv);
                crate::bounded_cmd::output_with_timeout_result(cmd, secs)
                    .map_err(|error| error.to_string())
            }),
        }
    }

    fn invoke(
        &mut self,
        args: &[String],
        timeout: u64,
    ) -> Result<std::process::Output, TransportError> {
        (self.run)(args, timeout).map_err(|error| TransportError {
            word: "thread_view_open_timeout".to_string(),
            detail: Some(error),
        })
    }

    /// Open the row's dedicated viewport pane and join it by name + thread
    /// id. The row defect names the registry row, not a transport hiccup: a
    /// retry cannot fix it.
    pub fn resolve(&mut self, row: &ViewportRow) -> Result<Viewport, TransportError> {
        let Some(thread_id) = row.thread_id.clone().filter(|id| !id.trim().is_empty()) else {
            return Err(TransportError {
                word: "worker_has_no_thread_ref".to_string(),
                detail: Some(format!(
                    "worker_has_no_thread_ref: {} is substrate=thread and its registry row carries no thread reference, so there is no thread to open. A retry cannot fix it.",
                    row.name
                )),
            });
        };
        let door = self.invoke(
            &[
                "mux".into(),
                "thread".into(),
                row.name.clone(),
                "--server".into(),
                self.session.clone(),
            ],
            30,
        )?;
        if !door.status.success() {
            let stderr = String::from_utf8_lossy(&door.stderr).to_string();
            let stdout = String::from_utf8_lossy(&door.stdout).to_string();
            let source = if stderr.trim().is_empty() {
                stdout.as_str()
            } else {
                stderr.as_str()
            };
            let detail = source
                .trim()
                .lines()
                .next_back()
                .map(str::to_string)
                .unwrap_or_else(|| format!("exit {}", door.status.code().unwrap_or(-1)));
            return Err(TransportError {
                word: "thread_view_unavailable".to_string(),
                detail: Some(detail),
            });
        }
        // The pane opened above stays open on a join miss and its name
        // stamping can lag the open, so the join retries; the miss names
        // the opened pane.
        for _ in 0..3 {
            let listing = self.invoke(
                &[
                    "mux".into(),
                    "pane".into(),
                    "ls".into(),
                    "--server".into(),
                    self.session.clone(),
                    "--json".into(),
                ],
                10,
            )?;
            let parsed: Result<Value, _> = serde_json::from_slice(&listing.stdout);
            let rows = match parsed {
                Ok(rows) => rows,
                Err(error) => {
                    return Err(TransportError {
                        word: "thread_pane_listing_unreadable".to_string(),
                        detail: Some(error.to_string()),
                    });
                }
            };
            let matches: Vec<u64> = if listing.status.success() {
                rows.as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter(|entry| {
                                entry.get("name").and_then(Value::as_str) == Some(row.name.as_str())
                                    && entry.get("fno_id").and_then(Value::as_str)
                                        == Some(thread_id.as_str())
                                    && entry
                                        .get("pane_id")
                                        .and_then(Value::as_u64)
                                        .is_some_and(|p| p > 0)
                            })
                            .filter_map(|entry| entry.get("pane_id").and_then(Value::as_u64))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if matches.len() == 1 {
                return Ok((self.session.clone(), matches[0]));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(TransportError::new("thread_view_join_missed"))
    }
}

impl TransportError {
    fn new(word: &str) -> Self {
        Self {
            word: word.to_string(),
            detail: None,
        }
    }
}

/// The graph row for `node`, or a usage error naming the miss. Every
/// spelling resolves through the tiers; archived rows never match. A
/// wellformed id with no row lands on the stub row, and the command
/// resolution refuses there, the way Python's None row did.
fn resolve_node_row(node: &str, graph_path: &Path) -> Result<Value, String> {
    let rows = crate::graph_store::read_rows_where(
        graph_path,
        &crate::backlog::RowQuery {
            include_archived: false,
            ..Default::default()
        },
    )
    .map_err(|error| format!("retask node {node:?} does not resolve to a graph id ({error})"))?;
    if let Some(hit) = crate::backlog::node_ref::resolve_tiers(&rows, node) {
        return Ok(hit.clone());
    }
    if crate::backlog::node_ref::is_wellformed_node_id(node) {
        return Ok(json!({ "id": node }));
    }
    Err(format!(
        "retask node {node:?} does not resolve to a graph id"
    ))
}

/// The node's next lifecycle command: the lifecycle table's answer for
/// the row, or `target` on an abstain. The legacy stored key folds in
/// here, the way every reader of the pair does.
fn node_command(row: &Value) -> Result<String, String> {
    let folded = row.get("dispatch_command").is_some()
        || row
            .get(crate::backlog::model::LEGACY_DISPATCH_COMMAND_KEY)
            .is_none();
    let row = if folded {
        row.clone()
    } else {
        let mut fold = row.clone();
        fold["dispatch_command"] = row[crate::backlog::model::LEGACY_DISPATCH_COMMAND_KEY].clone();
        fold
    };
    let (command, _note) = crate::backlog_ready::effective_command(&row)?;
    // The table answers the canonical slash spelling; the probe composes
    // the bare word, the way Python lstripped it.
    Ok(command
        .map(|c| c.trim_start_matches('/').to_string())
        .unwrap_or_else(|| "target".to_string()))
}

/// The composed probe: normalize, then the side-effect-free compose with
/// the builtin permission rung off. Returns the composed argv. A compose
/// refusal surfaces as the process exit the transport would have raised.

/// A compose refusal: the exit code and stderr the Python transport
/// raised SystemExit with.
pub struct ComposeExit {
    pub code: i32,
    pub stderr: Vec<String>,
}
fn compose_probe(argv: &[String], cwd: &Path) -> Result<Vec<String>, ComposeExit> {
    let normalized = crate::spawn_admission::normalize(argv, None, cwd);
    if let Some(code) = normalized.exit {
        for line in &normalized.stderr {
            eprintln!("{line}");
        }
        return Err(ComposeExit {
            code,
            stderr: normalized.stderr,
        });
    }
    let scan = crate::spawn_admission::scan_projection(&normalized.argv[1..]);
    let payload = json!({
        "argv": normalized.argv,
        "node_command": Value::Null,
        "env_node": std::env::var("FNO_NODE").ok().filter(|v| !v.is_empty()),
        "permission_builtin": Value::Null,
        "scan": scan,
        "facts": {"role_resolves": Value::Null, "role_protected": Value::Null},
        "verbose": false,
    });
    let inputs = crate::spawn_compose::gather(&payload, cwd);
    let answer = crate::spawn_compose::compose(&inputs);
    if answer.exit != 0 {
        for line in &answer.stderr {
            eprintln!("{line}");
        }
        return Err(ComposeExit {
            code: answer.exit,
            stderr: answer.stderr,
        });
    }
    Ok(if answer.argv.is_empty() {
        normalized.argv
    } else {
        answer.argv
    })
}

/// The coordinate outcome: a resolved target, a refused receipt, or the
/// compose exit the process must surface.
pub enum CoordinateOutcome {
    Target(RetaskTarget),
    Refused(Value),
    Exit(i32),
}

/// The target coordinate: composed axes plus the command word, mirroring
/// Python `RetaskCoordinate` so the receipt words stay identical.
fn resolve_coordinate(
    node: &str,
    row: &Value,
    model: Option<&str>,
    effort: Option<&str>,
) -> CoordinateOutcome {
    let command = match node_command(row) {
        Ok(command) => command,
        Err(detail) => {
            return CoordinateOutcome::Refused(refused_with(
                "dispatch_command_unresolved",
                Some(detail),
            ))
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let probe = [
        "spawn".to_string(),
        "--name".to_string(),
        "retask-probe".to_string(),
        format!("/fno:{command} {node}"),
    ];
    let composed = match compose_probe(&probe, &cwd) {
        Ok(composed) => composed,
        Err(exit) => return CoordinateOutcome::Exit(exit.code),
    };
    // The probe argv carries no --model/--effort: both are retired at the
    // spawn door, so the operator's values overlay the composed answer.
    let mut harness = flag_value(&composed, &["--harness", "-H"]).unwrap_or_default();
    if harness.is_empty() {
        let mut env = serde_json::Map::new();
        for (key, value) in std::env::vars() {
            env.insert(key, json!(value));
        }
        harness = crate::route_slot::resolve_slot_payload(&json!({
            "mode": "resolve", "explicit": Value::Null, "env": Value::Object(env),
        }))
        .get("harness")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    }
    let route = flag_value(&composed, &["--route"]);
    let provider_flag = flag_value(&composed, &["--provider", "-P"]);
    let (provider, route_model) = match route.as_deref() {
        Some(route) => match route.replace(',', '/').split_once('/') {
            Some((p, m)) => (Some(p.to_string()), Some(m.to_string())),
            None => (Some(route.to_string()), None),
        },
        None => (provider_flag, None),
    };
    let model = model
        .map(str::to_string)
        .or_else(|| flag_value(&composed, &["--model", "-m"]))
        .or_else(|| route_model);
    let effort = effort
        .map(str::to_string)
        .or_else(|| flag_value(&composed, &["--effort"]));
    CoordinateOutcome::Target(RetaskTarget {
        harness,
        provider,
        model,
        effort,
        substrate: flag_value(&composed, &["--substrate"]),
        permission_mode: flag_value(&composed, &["--permission-mode"]),
        route,
        account: flag_value(&composed, &["--account"]),
        command,
    })
}

/// The command line the retasked worker receives: the builtin no-merge
/// target dispatch, or the normalized `<command> {id}` for every other
/// verb. An undeclared harness refuses (the irreversible direction).
fn target_command(target: &RetaskTarget, node: &str) -> Result<String, String> {
    let contract =
        crate::harness_capabilities::HarnessContract::packaged().map_err(|e| e.to_string())?;
    let rendered = if target.command == "target" {
        crate::backlog::dispatch_resolve::dispatch_command(&contract, &target.harness, false)
    } else {
        crate::backlog::dispatch_resolve::normalize_command(
            &format!("/{} {{id}}", target.command),
            &target.harness,
        )
    }?;
    Ok(rendered.replace("{id}", node))
}

/// The parsed CLI form.
struct CliArgs {
    worker: String,
    node: String,
    model: Option<String>,
    effort: Option<String>,
    registry: Option<std::path::PathBuf>,
}

fn parse_args(args: &[String]) -> Result<CliArgs, String> {
    let mut worker = String::new();
    let mut node = String::new();
    let mut model = None;
    let mut effort = None;
    let mut registry = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--node" => node = iter.next().ok_or("--node needs a value")?.clone(),
            "--model" => model = iter.next().ok_or("--model needs a value")?.clone(),
            "--effort" => effort = iter.next().ok_or("--effort needs a value")?.clone(),
            "--registry" => {
                registry = Some(std::path::PathBuf::from(
                    iter.next().ok_or("--registry needs a value")?,
                ))
            }
            "--json" | "-J" => {}
            other if other.starts_with('-') => return Err(format!("unknown flag {other:?}")),
            other if worker.is_empty() => worker = other.to_string(),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    if worker.is_empty() {
        return Err("usage: fno-agents rename retask <worker> --node <id> [--model M] [--effort E] [--registry PATH]".to_string());
    }
    if node.is_empty() {
        return Err("retask needs --node".to_string());
    }
    Ok(CliArgs {
        worker,
        node,
        model,
        effort,
        registry,
    })
}

/// The native retask front. See the module doc for the surface and exits.
pub fn run_retask_cli(args: &[String]) -> i32 {
    let fail = |message: String| {
        eprintln!("agents retask: {message}");
        2
    };
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => return fail(message),
    };
    let graph_path = crate::graph_get::default_graph_path();
    let row = match resolve_node_row(&parsed.node, &graph_path) {
        Ok(row) => row,
        Err(message) => return fail(message),
    };
    let node = row
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(parsed.node.as_str())
        .to_string();
    let registry_path = parsed
        .registry
        .clone()
        .unwrap_or_else(|| crate::paths::AgentsHome::from_env().registry_json());
    let rows = match crate::client_commands::load_registry_entries(&registry_path) {
        Ok(rows) => rows,
        Err(message) => return fail(format!("registry read failed: {message}")),
    };
    let entry_value = match crate::client_commands::find_agent_entry(&rows, &parsed.worker) {
        Ok(entry) => entry.clone(),
        Err(error) => {
            return fail(format!(
                "cannot resolve worker {:?}: {:?}",
                parsed.worker, error
            ))
        }
    };
    let entry: crate::state::RegistryEntry = match serde_json::from_value(entry_value) {
        Ok(entry) => entry,
        Err(error) => {
            return fail(format!(
                "registry row {:?} does not decode: {error}",
                parsed.worker
            ))
        }
    };
    let target = match resolve_coordinate(
        &node,
        &row,
        parsed.model.as_deref(),
        parsed.effort.as_deref(),
    ) {
        CoordinateOutcome::Target(target) => target,
        CoordinateOutcome::Refused(receipt) => return emit_receipt(&receipt, 1),
        CoordinateOutcome::Exit(code) => return code,
    };
    let target_command = match target_command(&target, &node) {
        Ok(command) => command,
        Err(detail) => {
            return emit_receipt(
                &refused_with("dispatch_command_unresolved", Some(detail)),
                1,
            )
        }
    };
    let substrate = entry.substrate.clone().unwrap_or_default();
    let mux_ref = if substrate == "thread" {
        let viewport_row = ViewportRow {
            name: entry.name.clone(),
            thread_id: entry.fno_id.clone(),
        };
        let mut resolver = ViewportResolver::production();
        match resolver.resolve(&viewport_row) {
            Ok((session, pane_id)) => Some((session, pane_id)),
            Err(error) => {
                let mut receipt = refused_receipt(&error.word);
                if let Some(obj) = receipt.as_object_mut() {
                    obj.insert("detail".into(), json!(error.detail));
                }
                return emit_receipt(&receipt, 1);
            }
        }
    } else {
        entry
            .mux
            .as_ref()
            .map(|mux| (mux.session.clone(), mux.pane_id))
    };
    let worker_row: RetaskRow = transport::retask_row_from_entry(&entry);
    let mut seams = LiveSeams::live(
        worker_row.clone(),
        mux_ref,
        node.clone(),
        registry_path,
        graph_path,
    );
    let live_mode = transport::live_permission_mode(&entry);
    let receipt = match execute_retask(
        &worker_row,
        &target,
        &node,
        &target_command,
        &mut seams,
        live_mode.as_deref(),
    ) {
        Ok(receipt) => receipt,
        Err(failure) => refused_receipt_from_transport(&worker_row, &seams, failure),
    };
    let code = if receipt.get("status").and_then(Value::as_str) == Some("retasked") {
        0
    } else {
        1
    };
    emit_receipt(&receipt, code)
}

/// One receipt line on stdout with its exit code.
fn emit_receipt(receipt: &Value, code: i32) -> i32 {
    println!(
        "{}",
        serde_json::to_string(receipt).unwrap_or_else(|_| "{}".into())
    );
    code
}

/// The shared refusal receipt with an optional detail line.
pub(crate) fn refused_with(reason: &str, detail: Option<String>) -> Value {
    let mut receipt = refused_receipt(reason);
    if let (Some(obj), Some(detail)) = (receipt.as_object_mut(), detail) {
        obj.insert("detail".into(), json!(detail));
    }
    receipt
}

#[cfg(test)]
mod tests;

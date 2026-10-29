//! `-H footnote` on the supervisor side. The loop itself is the `footnote`
//! binary, built from bllshttng/fnh; this module resolves everything it needs, hands
//! it one `LaunchSpec` on stdin, and keeps the registry row, the way
//! `zcode_ask` launches zcode. The thread lane (`thread`) holds one session
//! open over ACP stdio instead.

pub mod endpoint;
pub mod source;
pub mod thread;
pub mod transcript;

use serde_json::Value;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::claude_ask::AskOutcome;
use crate::footnote_transcript::{LaunchSpec, LAUNCH_SPEC_V};
use crate::paths::AgentsHome;
use crate::state::{load_registry, update_registry, RegistryEntry};

const INSTALL_HINT: &str =
    "build it from bllshttng/fnh: cargo install --locked --path <your fnh checkout>";

/// The `footnote` binary: `FNO_FOOTNOTE_BIN`, else a sibling of this
/// executable, else `footnote` on PATH.
pub fn resolve_footnote_bin() -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_FOOTNOTE_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("footnote")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("footnote"))
}

/// Budget caps and the claimed plan from the target manifest, when one exists.
fn manifest_fields(
    manifest: Option<&Path>,
) -> (Option<f64>, Option<u64>, Option<String>, Option<String>) {
    let Some(content) = manifest.and_then(|p| std::fs::read_to_string(p).ok()) else {
        return (None, None, None, None);
    };
    let f = |k: &str| crate::loopcheck::scan_manifest_field(&content, k);
    (
        f("budget_cost_cap_usd")
            .and_then(|v| v.parse().ok())
            .filter(|v: &f64| *v > 0.0),
        f("budget_wall_clock_cap_minutes")
            .and_then(|v| v.parse().ok())
            .filter(|v: &u64| *v > 0),
        f("plan_path"),
        f("graph_node_id"),
    )
}

/// The models.dev price cache beside the sessions root.
fn price_cache() -> PathBuf {
    transcript::sessions_root()
        .parent()
        .map(|p| p.join("cache").join("models-dev.json"))
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn launch_spec(
    mode: &str,
    fno_id: &str,
    session_dir: PathBuf,
    cwd: &Path,
    model: &str,
    message: &str,
    timeout: Option<Duration>,
    node: Option<&str>,
    permission_mode: Option<&str>,
    provider: Option<&str>,
) -> Result<LaunchSpec, String> {
    let endpoint = endpoint::resolve_endpoint(cwd, &|k| std::env::var(k).ok(), provider)?;
    let manifest = crate::state_path::resolve("target-state", cwd);
    let (cost_cap_usd, wall_cap_minutes, plan_path, manifest_node) =
        manifest_fields(manifest.as_deref());
    Ok(LaunchSpec {
        v: LAUNCH_SPEC_V,
        mode: mode.to_string(),
        fno_id: fno_id.to_string(),
        session_dir,
        cwd: cwd.to_path_buf(),
        model: model.to_string(),
        message: message.to_string(),
        timeout_secs: timeout.map(|d| d.as_secs()),
        node: node.map(str::to_string).or(manifest_node),
        plan_path,
        cost_cap_usd,
        wall_cap_minutes,
        plugin_root: crate::provider::plugin_root(),
        parent_session_id: std::env::var("FNO_HARNESS_SESSION_ID")
            .ok()
            .filter(|v| !v.is_empty()),
        permission_mode: permission_mode.map(str::to_string),
        fork_from: None,
        fork_seq: None,
        price_cache: price_cache(),
        context_window: crate::context_window::window_for_model(model),
        endpoint,
    })
}

/// Start the binary in its own process group with the spawner's identity
/// scrubbed, so its tool children carry only footnote's own.
fn spawn(cwd: &Path) -> Result<Child, AskOutcome> {
    let bin = resolve_footnote_bin();
    let mut cmd = Command::new(&bin);
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::claims::scrub_identity_env(&mut cmd);
    // SAFETY: setpgid is async-signal-safe; it runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| {
        let why = if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "footnote binary not found at {}; {INSTALL_HINT}",
                bin.display()
            )
        } else {
            format!("cannot start {}: {e}", bin.display())
        };
        outcome(2, String::new(), format!("{why}\n"))
    })
}

/// Feed the spec and wait. Ctrl-C reaches the child's group, which records
/// the interrupt in its transcript before it exits.
fn finish(mut child: Child, spec: &LaunchSpec) -> AskOutcome {
    let _sigint = crate::subprocess_ask::SigintForwarder::install(child.id());
    let body = serde_json::to_vec(spec).unwrap_or_default();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&body);
    }
    match child.wait_with_output() {
        Ok(out) => outcome(
            out.status
                .code()
                .unwrap_or(if crate::subprocess_ask::ask_interrupted() {
                    130
                } else {
                    12
                }),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(e) => outcome(12, String::new(), format!("footnote: wait failed: {e}\n")),
    }
}

fn outcome(code: i32, stdout: String, stderr: String) -> AskOutcome {
    AskOutcome {
        stdout,
        stderr,
        exit_code: code,
    }
}

/// Mark the row Exited, and write the session's spend from its newest
/// `terminal` to the ledger, where the node's cost rolls up.
fn mark_row(home: &AgentsHome, name: &str, transcript: &Path, spec: &LaunchSpec, ledger: &Path) {
    let records = transcript::read_records(transcript).unwrap_or_default();
    let reported = records
        .iter()
        .find(|r| r["type"] == "model_response")
        .and_then(|r| r["data"]["reported_model"].as_str())
        .map(str::to_string);
    if let Some(row) = execution_row(&records, spec, reported.as_deref()) {
        if let Err(e) = crate::ledger_axes::upsert_execution(ledger, row) {
            eprintln!("fno-agents: footnote spend not recorded: {e}");
        }
    }
    let _ = update_registry(&home.registry_json(), |reg| {
        let Some(e) = reg.find_mut(name) else {
            return false;
        };
        e.status = crate::AgentStatus::Exited;
        e.pid = None;
        e.pid_start_time = None;
        e.last_message_at = Some(crate::daemon::now_rfc3339_like());
        if e.model_name.is_none() {
            e.model_name = reported.clone();
        }
        true
    });
}

/// The ledger's execution row for one footnote session, or None before the
/// session wrote a `terminal`.
fn execution_row(records: &[Value], spec: &LaunchSpec, reported: Option<&str>) -> Option<Value> {
    let rec = crate::footnote_transcript::last_of(records, "terminal")?;
    let end: crate::footnote_transcript::Terminal =
        serde_json::from_value(rec["data"].clone()).ok()?;
    // A resume's spec may name no node; the header kept the first run's.
    let header = &records.first()?["data"];
    let node = spec
        .node
        .clone()
        .or_else(|| header["node"].as_str().map(str::to_string));
    // The merge backstop stamps a PR only onto a row outside NON_DELIVERY.
    let reason = match end.state.as_str() {
        "done" => "Done",
        "budget" => "Budget",
        "interrupted" => "Interrupted",
        _ => "Aborted",
    };
    Some(serde_json::json!({
        "type": "execution",
        "harness": "footnote",
        "fno_id": spec.fno_id,
        "session_id": spec.fno_id,
        "sessions": [spec.fno_id],
        "graph_node_id": node,
        "plan_path": spec.plan_path,
        "model": reported.unwrap_or(&spec.model),
        "started": records.first().map(|r| r["ts"].clone()),
        "completed": rec["ts"],
        "cost_usd": end.cost_usd,
        "termination_reason": reason,
        "footnote_terminal": end,
    }))
}

/// The base launch spec a pane's TUI reads (`footnote --spec <file>`), and
/// the thread host hands `footnote acp`: the spec the headless lane pipes on
/// stdin, written 0600 because it carries the
/// endpoint key. The TUI's first session opens under this spec's `fno_id` and
/// `session_dir`, so the caller-assigned `fno_id` is the id the pane row binds
/// to; with none, a fresh one is minted. Answers the file and the id it names.
/// The TUI copies the spec at start, so specs older than an hour are swept on
/// each write.
pub fn write_pane_spec(
    cwd: &Path,
    model: &str,
    node: Option<&str>,
    permission_mode: Option<&str>,
    fno_id: Option<&str>,
) -> Result<(PathBuf, String), String> {
    use std::os::unix::fs::OpenOptionsExt;
    if model.is_empty() {
        return Err(MODEL_REFUSAL.into());
    }
    let (provider, model) = endpoint::split_model(cwd, &|k| std::env::var(k).ok(), model);
    let sid = match fno_id {
        Some(id) if crate::resume_wake::is_uuid_shaped(id) => id.to_string(),
        Some(id) => return Err(format!("footnote pane spec: malformed fno_id {id}")),
        None => crate::identity::mint_fno_id()?,
    };
    let root = transcript::sessions_root();
    let dir = transcript::session_dir(&root, cwd, &sid);
    let spec = launch_spec(
        "create",
        &sid,
        dir,
        cwd,
        model,
        "",
        None,
        node,
        permission_mode,
        provider.as_deref(),
    )?;
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let hour = Duration::from_secs(3_600);
    for old in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let stale = old.file_name().to_string_lossy().starts_with(".pane-spec-")
            && old
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > hour));
        if stale {
            let _ = std::fs::remove_file(old.path());
        }
    }
    let path = root.join(format!(".pane-spec-{sid}.json"));
    let body = serde_json::to_vec(&spec).map_err(|e| e.to_string())?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut f| f.write_all(&body))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((path, sid))
}

/// End a headless footnote child: SIGINT its process group so the loop
/// records its terminal, SIGKILL the group if it outlives the grace, and
/// answer true only once the pid is gone. A pid the row's start token no
/// longer matches is somebody else's and is never signalled.
pub(crate) async fn stop_confirmed(pid: u32, started: Option<u64>) -> bool {
    use crate::daemon::{pid_gone_within, pid_is_ours};
    if !pid_is_ours(pid, started) {
        return pid_gone_within(pid, started, Duration::from_secs(1)).await;
    }
    // SAFETY: the child called setpgid(0, 0), so its pid is its group id, and
    // pid_is_ours rejected pid 0, 1 and anything above i32::MAX.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
    if pid_gone_within(pid, started, Duration::from_secs(5)).await {
        return true;
    }
    if pid_is_ours(pid, started) {
        // SAFETY: as above.
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    }
    pid_gone_within(pid, started, Duration::from_secs(2)).await
}

/// `fno agents spawn -H footnote --substrate headless`: mint the id, start
/// the binary, register the row with the child's pid, run, mark it Exited.
/// The refusal for a footnote spawn that names no model.
const MODEL_REFUSAL: &str =
    "-H footnote needs a model: pass it after the fence, `-- -m <provider>/<model>`";

/// The model a footnote spawn names after the `--` fence (`-m X`,
/// `--model X`, `--model=X`). The spawn door retired `--model`, so the
/// fenced harness argv is the one place a caller still names it.
pub fn fenced_model(params: &Value) -> Option<String> {
    let args: Vec<&str> = params
        .get("harness_args")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    args.iter().enumerate().find_map(|(i, a)| match *a {
        "-m" | "--model" => args.get(i + 1).map(|m| m.to_string()),
        a => a.strip_prefix("--model=").map(str::to_string),
    })
}

/// `params` is the spawn request; it supplies `node` and `permission_mode`.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_once(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    model: Option<&str>,
    timeout: Option<Duration>,
    params: &Value,
) -> AskOutcome {
    if let Err(msg) = crate::claude_ask::validate_spawn_inputs(name, from_name) {
        return outcome(2, String::new(), format!("{msg}\n"));
    }
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return outcome(2, String::new(), format!("{MODEL_REFUSAL}\n"));
    };
    let (provider, model) = endpoint::split_model(cwd, &|k| std::env::var(k).ok(), model);
    match load_registry(&home.registry_json()) {
        Ok(reg) if reg.find(name).is_some() => {
            return outcome(
                2,
                String::new(),
                format!("agent {name} already exists; use 'fno agents rm {name}' first\n"),
            );
        }
        Err(e) => return outcome(12, String::new(), format!("registry read failed: {e}\n")),
        Ok(_) => {}
    }
    let sid = match crate::identity::mint_fno_id() {
        Ok(id) => id,
        Err(e) => return outcome(2, String::new(), format!("{e}\n")),
    };
    let dir = transcript::session_dir(&transcript::sessions_root(), cwd, &sid);
    let record = transcript::transcript_file(&dir, &sid);
    let node = params.get("node").and_then(Value::as_str);
    let mode = params.get("permission_mode").and_then(Value::as_str);
    let spec = match launch_spec(
        "create",
        &sid,
        dir,
        cwd,
        model,
        message,
        timeout,
        node,
        mode,
        provider.as_deref(),
    ) {
        Ok(s) => s,
        Err(e) => return outcome(2, String::new(), format!("{e}\n")),
    };
    let mut child = match spawn(cwd) {
        Ok(c) => c,
        Err(o) => return o,
    };
    let mut entry = RegistryEntry {
        name: name.to_string(),
        short_id: sid.clone(),
        provider: Some("footnote".into()),
        harness: Some("footnote".into()),
        substrate: Some("headless".into()),
        session_id: Some(sid.clone()),
        requested_model: Some(model.to_string()),
        requested_permission_mode: spec.permission_mode.clone(),
        route_provider_id: spec.endpoint.provider_id.clone(),
        node: spec.node.clone(),
        cwd: cwd.to_string_lossy().to_string(),
        origin: Some("spawn".into()),
        status: crate::AgentStatus::Busy,
        pid: Some(child.id()),
        pid_start_time: crate::daemon::process_start_time(child.id()),
        created_at: crate::daemon::now_rfc3339_like(),
        log_path: Some(record.to_string_lossy().to_string()),
        ..RegistryEntry::new(Some(sid.clone()), crate::spawn_lineage::ambient_lineage())
    };
    entry.account_record_id = Some("default".into());
    // The session's own id is the row's fno_id; a set value is never re-minted.
    entry.fno_id = Some(sid.clone());
    let registered = update_registry(&home.registry_json(), |reg| {
        if reg.find(name).is_some() {
            return false;
        }
        reg.entries.push(entry.clone());
        true
    });
    if !matches!(registered, Ok(true)) {
        // The child is still blocked on stdin: close it and reap it.
        let _ = child.kill();
        let _ = child.wait();
        return match registered {
            Ok(_) => outcome(2, String::new(), format!("agent {name} already exists\n")),
            Err(e) => outcome(12, String::new(), format!("registry write failed: {e}\n")),
        };
    }
    let out = finish(child, &spec);
    if out.exit_code == 2 && !record.is_file() {
        // Refused before the session existed: leave no row, as before.
        let _ = update_registry(&home.registry_json(), |reg| {
            let before = reg.entries.len();
            reg.entries.retain(|e| e.name != name);
            reg.entries.len() != before
        });
    } else {
        let ledger = crate::paths_cli::ledger_json(cwd);
        mark_row(home, name, &record, &spec, &ledger);
    }
    out
}

/// `fno agents ask <name>` on a footnote row: resume by name, then the input.
/// Returns None for any other harness (fall through).
pub fn maybe_run_ask(home: &AgentsHome, params: &Value, name: &str) -> Option<i32> {
    let reg = load_registry(&home.registry_json()).ok()?;
    let entry = reg.find_name_or_full_session_id(name)?;
    if entry.harness_name() != "footnote" {
        return None;
    }
    if let Some(code) = thread::maybe_ask_host(entry, params) {
        return Some(code);
    }
    // A pane row binds the TUI's session id as `harness_session_id` and keeps
    // a birth `fno_id` that names no transcript; a headless row sets both to
    // one id.
    let fno_id = entry
        .harness_session_id
        .clone()
        .or_else(|| entry.fno_id.clone())?;
    let row_name = entry.name.clone();
    let cwd = PathBuf::from(&entry.cwd);
    let model = entry.requested_model.clone().unwrap_or_default();
    // A resume keeps the mode the session was spawned with.
    let mode = entry.requested_permission_mode.clone();
    // A resume reaches the provider the row recorded, whatever stamp the
    // asking session carries.
    let provider = entry.route_provider_id.clone();
    let message = params["message"].as_str().unwrap_or("").to_string();
    let timeout = params["timeout"].as_u64().map(Duration::from_secs);
    let Some(dir) = transcript::find_session_dir(&transcript::sessions_root(), &fno_id) else {
        eprintln!("fno-agents: footnote session {fno_id} has no transcript on disk");
        return Some(2);
    };
    let record = transcript::transcript_file(&dir, &fno_id);
    let spec = match launch_spec(
        "resume",
        &fno_id,
        dir,
        &cwd,
        &model,
        &message,
        timeout,
        None,
        mode.as_deref(),
        provider.as_deref(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fno-agents: {e}");
            return Some(2);
        }
    };
    let child = match spawn(&cwd) {
        Ok(c) => c,
        Err(o) => {
            eprint!("{}", o.stderr);
            return Some(o.exit_code);
        }
    };
    let pid = child.id();
    let prior = (entry.status, entry.pid, entry.pid_start_time);
    let _ = update_registry(&home.registry_json(), |reg| {
        let Some(e) = reg.find_mut(&row_name) else {
            return false;
        };
        e.status = crate::AgentStatus::Busy;
        e.pid = Some(pid);
        e.pid_start_time = crate::daemon::process_start_time(pid);
        true
    });
    let o = finish(child, &spec);
    if o.exit_code == 2 {
        // Refused before the session reopened (a live writer holds it, or
        // the spec was refused): the row goes back to what it said, so a
        // running session is never marked Exited by a second ask.
        let _ = update_registry(&home.registry_json(), |reg| {
            let Some(e) = reg.find_mut(&row_name) else {
                return false;
            };
            (e.status, e.pid, e.pid_start_time) = prior;
            true
        });
    } else {
        let ledger = crate::paths_cli::ledger_json(&cwd);
        mark_row(home, &row_name, &record, &spec, &ledger);
    }
    print!("{}", o.stdout);
    eprint!("{}", o.stderr);
    Some(o.exit_code)
}

/// The roster's `footnote` row. Spawn and ask never reach these argv
/// methods: the client launches the binary itself (`dispatch_once`,
/// `maybe_run_ask`), so they render the capability table's forms only.
pub struct FootnoteProvider;

impl crate::provider::Provider for FootnoteProvider {
    fn name(&self) -> &'static str {
        "footnote"
    }

    fn create_argv(&self, _ctx: &crate::provider::CreateContext) -> Vec<String> {
        crate::harness_capabilities::render_session_argv("footnote", "headless_create", None)
            .expect("embedded footnote headless-create capability")
    }

    fn resume_argv(&self, ctx: &crate::provider::ResumeContext) -> Vec<String> {
        crate::harness_capabilities::render_session_argv(
            "footnote",
            "headless_resume",
            Some(&ctx.session_id),
        )
        .expect("embedded footnote headless-resume capability")
    }

    fn parse_stream_event(&self, chunk: &str) -> crate::ParsedEvent {
        crate::ParsedEvent::Unknown {
            raw: chunk.to_string(),
        }
    }

    fn reachability(
        &self,
        _entry: &crate::provider::AgentEntry,
        _timeout: Duration,
    ) -> Result<bool, crate::provider::ReachabilityProbeError> {
        Err(crate::provider::ReachabilityProbeError::new(
            "footnote",
            "each turn is a one-shot footnote child; read the transcript's terminal record",
        ))
    }
}

#[cfg(test)]
mod tests;

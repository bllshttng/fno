//! The claude cloud lane: create a cloud session under a locally held pty,
//! read the minted session id back, and record the registry row.
//!
//! Measured on the installed CLI (the 2026-10-18 probes): `claude --cloud`
//! refuses a non-interactive invocation outright, so the create runs on a pty
//! this process holds. The CLI prints the minted id (`Created cloud session:`
//! / `Resume with: claude --teleport session_<id>`) and exits at once - the
//! session runs remotely and survives client death, so the pty is released
//! here and no local process outlives the create. Mail is queued delivery
//! (`claude -p ... --cloud <id>`, "Sent to cloud session.", exit 0); the
//! reply never returns on stdout. There is no non-interactive status source,
//! so a cloud row's status reads the honest `?` like any driverless row.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::claude_ask::AskOutcome;
use crate::paths::AgentsHome;
use crate::state::{load_registry, update_registry, RegistryEntry};
use crate::AgentStatus;

/// Bound the create read. The measured create exits in seconds; 120s covers a
/// slow VM handoff without letting a silent prompt wedge the spawn.
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);

/// Bound one queued follow-up. The measured delivery prints one line and
/// exits; 120s covers a slow enqueue without wedging the mail verb (the
/// Python transport budget bounds the whole verb besides this).
const FOLLOWUP_TIMEOUT_SECS: u64 = 120;

/// One cloud create's read-back: the parsed id plus what the CLI printed
/// beside it, enough for the receipt without a second scrape.
pub struct CloudCreate {
    pub session_id: String,
    pub title: Option<String>,
    pub view_url: Option<String>,
    pub output: String,
}

/// Run the create argv on a pty and return everything it printed. The pty is
/// required: a non-interactive `--cloud` refuses before any session exists.
fn run_create(argv: &[String], cwd: &Path) -> Result<String, (i32, String)> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| (1, format!("cloud spawn: openpty: {e}")))?;
    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.cwd(cwd);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| (1, format!("cloud spawn: launch claude: {e}")))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| (1, format!("cloud spawn: pty reader: {e}")))?;
    // The reader thread owns the byte stream; the main thread bounds the wait.
    // The TUI redraws constantly, so a partial read is normal and the marker
    // scan runs on whatever landed by the deadline.
    let reader_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let buf = reader_handle.join().unwrap_or_default();
        let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
    });
    let output = match rx.recv_timeout(CREATE_TIMEOUT) {
        Ok(text) => text,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err((
                124,
                "cloud spawn: timed out after 120s waiting for the create output; \
                 the CLI may be sitting on a prompt this lane cannot answer"
                    .to_string(),
            ));
        }
    };
    let _ = child.wait();
    Ok(output)
}

/// Parse the minted session id out of the create output. The `Resume with:`
/// line names it verbatim; the `View:` URL carries the same id. Markers
/// measured 2026-10-18 (probe 7).
pub fn parse_create_output(output: &str, id_prefix: &str) -> Option<CloudCreate> {
    let title = output
        .lines()
        .find_map(|line| line.strip_prefix("Created cloud session: "))
        .map(str::trim)
        .map(str::to_string);
    let mut session_id: Option<String> = None;
    let mut view_url: Option<String> = None;
    for line in output.lines() {
        let cleaned = strip_ansi(line);
        if let Some(rest) = cleaned.strip_prefix("Resume with: claude --teleport ") {
            if rest.starts_with(id_prefix) {
                session_id = Some(rest.trim().to_string());
            }
        }
        if let Some(url) = cleaned.strip_prefix("View: ") {
            view_url = Some(url.trim().to_string());
        }
    }
    // Belt for a CLI that changes the Resume line but still prints the URL:
    // the id rides the URL path under the same prefix.
    let session_id = session_id.or_else(|| {
        view_url.as_ref().and_then(|url| {
            url.split('/')
                .find(|part| part.starts_with(id_prefix) && part.len() > id_prefix.len())
                .map(|part| part.split('?').next().unwrap_or(part).trim().to_string())
        })
    })?;
    Some(CloudCreate {
        session_id,
        title,
        view_url,
        output: output.to_string(),
    })
}

/// Fill a capability arm's `{hole}` tokens with their values. The hole
/// vocabulary is [`crate::harness_capabilities::CLOUD_HOLES`]; a hole the
/// table declares but no caller fills would ride verbatim into the CLI, so
/// each caller fills exactly the holes its arm declares.
pub(crate) fn render_holes(argv: &[String], holes: &[(&str, &str)]) -> Vec<String> {
    argv.iter()
        .map(|token| {
            for (name, value) in holes {
                if token == &format!("{{{name}}}") {
                    return value.to_string();
                }
            }
            token.clone()
        })
        .collect()
}

/// Strip ANSI escape sequences so marker scans see prose, not cursor control.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: swallow params and the final byte.
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: swallow to BEL or ST.
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n == '\x07' {
                        break;
                    }
                    if n == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The cloud create is not free: a lost name race burns a real cloud session
/// that the registry write then refuses. The same per-agent flock the other
/// lanes hold serializes two creates before either runs one.
const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// The cloud spawn: validate, preflight the contract row, run the create on a
/// pty, parse the id, mint the row, and print the receipt. Mirror of
/// `dispatch_claude_spawn` for a lane with no local process: no daemon
/// preflight, no roster, no pid.
pub fn dispatch_claude_cloud(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    surface_cwd: bool,
) -> AskOutcome {
    if let Err(msg) = crate::claude_ask::validate_spawn_inputs(name, from_name) {
        return AskOutcome {
            stdout: String::new(),
            stderr: format!("{msg}\n"),
            exit_code: 2,
        };
    }
    let _lock = match crate::agent_lock::AgentLock::acquire(home, name, LOCK_ACQUIRE_TIMEOUT) {
        Ok(l) => l,
        Err(()) => {
            return AskOutcome {
                stdout: String::new(),
                stderr: format!(
                    "lock timeout for agent {} after {:.1}s{}",
                    crate::claude_ask::py_repr(name),
                    LOCK_ACQUIRE_TIMEOUT.as_secs_f64(),
                    crate::agent_lock::holder_note(home, name)
                ),
                exit_code: 2,
            };
        }
    };
    let contract = match crate::harness_capabilities::HarnessContract::packaged() {
        Ok(c) => c,
        Err(error) => {
            return AskOutcome {
                stdout: String::new(),
                stderr: format!("cloud spawn refused: capability table unreadable: {error}\n"),
                exit_code: 1,
            };
        }
    };
    let caps = match contract.capabilities("claude") {
        Ok(caps) => caps,
        Err(error) => {
            return AskOutcome {
                stdout: String::new(),
                stderr: format!("cloud spawn refused: {error}\n"),
                exit_code: 1,
            };
        }
    };
    let Some(arm) = caps.cloud.as_ref() else {
        return AskOutcome {
            stdout: String::new(),
            stderr: "cloud spawn refused: the capability table declares no cloud \
                     row for claude; the contract and the router disagree\n"
                .to_string(),
            exit_code: 1,
        };
    };
    let argv = render_holes(&arm.launch, &[("payload", message)]);

    let events = home.events_jsonl();
    let registry_path = home.registry_json();
    let output = match run_create(&argv, cwd) {
        Ok(text) => text,
        Err((code, stderr)) => {
            crate::claude_ask::emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "subprocess".into()),
                    ("name", name.into()),
                    ("provider", "claude".into()),
                    ("substrate", "cloud".into()),
                ],
            );
            return AskOutcome {
                stdout: String::new(),
                stderr: format!("{stderr}\n"),
                exit_code: code,
            };
        }
    };
    if output.contains("trust this folder") {
        return AskOutcome {
            stdout: String::new(),
            stderr: format!(
                "cloud spawn refused: claude sat on the workspace-trust dialog for \
                 {}. Run claude in that directory once and accept the trust \
                 prompt, then retry.\n",
                cwd.display()
            ),
            exit_code: 2,
        };
    }
    let created = match parse_create_output(&output, &arm.session_id_prefix) {
        Some(created) => created,
        None => {
            crate::claude_ask::emit_event(
                &events,
                "agent_ask_failed",
                &[
                    ("stage", "parse".into()),
                    ("name", name.into()),
                    ("provider", "claude".into()),
                    ("substrate", "cloud".into()),
                ],
            );
            let head: String = output.chars().take(200).collect();
            return AskOutcome {
                stdout: String::new(),
                stderr: format!(
                    "unable to parse the cloud session id from the create output: \
                     {head:?}\n"
                ),
                exit_code: 1,
            };
        }
    };

    // Record the create output as the row's log evidence, like the bg lane's
    // log_path: a file that exists beats a field that claims.
    let log_path = home.root().join("logs").join(format!("{name}.log"));
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_ok = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, created.output.as_bytes()))
        .is_ok();

    let spawned_by = crate::spawn_lineage::ambient_lineage();
    let (launch_account, launch_account_source) = crate::state::launch_provenance_from_env();
    let cloud_id = created.session_id.clone();
    let new_entry = RegistryEntry {
        node: std::env::var("FNO_NODE").ok().filter(|v| !v.is_empty()),
        substrate: Some("cloud".into()),
        name: name.to_string(),
        short_id: cloud_id.clone(),
        launch_account_source,
        legacy_provider: String::new(),
        provider: Some("anthropic".to_string()),
        model: None,
        model_basis: None,
        effort: None,
        requested_model: None,
        requested_provider: None,
        requested_effort: None,
        harness: Some("claude".to_string()),
        predecessor_session_ids: Vec::new(),
        forked_from_session_id: None,
        launch_account: launch_account.clone(),
        related_session_id: None,
        route_provider_id: Some("anthropic".to_string()),
        model_name: None,
        account_record_id: launch_account,
        cwd: cwd.to_string_lossy().to_string(),
        project_root: String::new(),
        // The cloud session id is the row's only handle: no local uuid, no
        // pid, no messaging socket. `harness_session_id` is the canonical
        // persisted id (the mail route's recipient); the load backfill derives
        // `claude_session_uuid` from it, which no local resume can satisfy -
        // the honest failure for a remote-only row.
        harness_session_id: Some(cloud_id.clone()),
        session_id: Some(cloud_id.clone()),
        claude_session_uuid: None,
        messaging_socket_path: None,
        codex_session_id: None,
        gemini_session_id: None,
        mcp_channel_id: None,
        host_mode: None,
        cc_session_id: None,
        status: AgentStatus::Live,
        last_message_at: None,
        created_at: crate::claude_ask::now_iso(),
        pid: None,
        pid_start_time: None,
        keeper_child_pid: None,
        log_path: log_ok.then(|| log_path.to_string_lossy().to_string()),
        last_reconciled_at: None,
        inside_leg: None,
        exited_at: None,
        mux: None,
        screen_state: None,
        role_level: None,
        role_scope: None,
        role_grantor: None,
        route_settings_path: None,
        delivery_policy: None,
        sandbox_posture: None,
        git_grant: None,
        origin: Some("spawn".to_string()),
        spawn_trigger: None,
        ..RegistryEntry::new(None, spawned_by)
    };
    match update_registry(&registry_path, |reg| {
        if reg.find(name).is_some() {
            false
        } else {
            reg.entries.push(new_entry);
            true
        }
    }) {
        Ok(true) => {}
        Ok(false) => {
            return AskOutcome {
                stdout: String::new(),
                stderr: format!(
                    "agent {} already exists; use 'fno agents rm {}' first or pick \
                     another name",
                    crate::claude_ask::py_repr(name),
                    name
                ),
                exit_code: 2,
            };
        }
        Err(e) => {
            return AskOutcome {
                stdout: String::new(),
                stderr: format!("registry write failed: {e}\n"),
                exit_code: 12,
            };
        }
    }

    // Receipt. stdout is exactly the JSON object: the Python seam parses
    // whole-stdout JSON (call_binary_json). The human create output rides
    // stderr, so a stdout-only capture still gets a machine-readable receipt.
    let mut receipt = serde_json::Map::new();
    receipt.insert("short_id".into(), cloud_id.clone().into());
    receipt.insert("session_id".into(), cloud_id.clone().into());
    receipt.insert("name".into(), name.to_string().into());
    receipt.insert("substrate".into(), "cloud".into());
    receipt.insert("harness".into(), "claude".into());
    if let Some(title) = &created.title {
        receipt.insert("title".into(), title.clone().into());
    }
    if let Some(url) = &created.view_url {
        receipt.insert("view_url".into(), url.clone().into());
    }
    let cwd_note = if surface_cwd {
        format!(
            "fno agents spawn: moved with the project to {}; the receipt's cwd names it.\n",
            cwd.display()
        )
    } else {
        String::new()
    };
    let human = format!(
        "Created cloud session: {}\n{}\nResume with: claude --teleport {cloud_id}\n\
         {cwd_note}",
        created.title.as_deref().unwrap_or(name),
        created
            .view_url
            .as_ref()
            .map(|u| format!("View: {u}"))
            .unwrap_or_default(),
    );
    AskOutcome {
        stdout: format!(
            "{}\n",
            serde_json::to_string(&serde_json::Value::Object(receipt)).unwrap_or_default()
        ),
        stderr: human,
        exit_code: 0,
    }
}

/// The registry row a mail handle resolves to, when it is a live cloud row.
/// The handle is the row's canonical `harness_session_id` (what the Python
/// send passes as the recipient) or its `short_id`; both carry the same cloud
/// id on this lane. `None` sends the caller down its own route.
pub fn cloud_row_for_session(home: &AgentsHome, session: &str) -> Option<RegistryEntry> {
    let registry = load_registry(&home.registry_json()).ok()?;
    registry.entries.into_iter().find(|e| {
        e.substrate.as_deref() == Some("cloud")
            && e.harness.as_deref() == Some("claude")
            && (e.harness_session_id.as_deref() == Some(session) || e.short_id == session)
    })
}

/// Run one argv to completion under a wall-clock bound, collecting stdout and
/// stderr. Shape mirrors `pr_draft_ready::run_bounded`; the claude lane needs
/// no fleet-budget admission (that gate is gh-specific).
fn run_bounded(argv: &[String], secs: u64) -> (i32, String, String) {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (127, String::new(), format!("launch: {e}")),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut out);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut err);
                }
                return (status.code().unwrap_or(127), out, err);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (124, String::new(), "timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return (127, String::new(), format!("wait: {e}")),
        }
    }
}

/// Deliver mail to a cloud row: render the contract row's follow-up argv with
/// the envelope and run it bounded. Measured follow-up (CLI probes): the
/// CLI prints "Sent to cloud session." and exits 0; the reply never returns
/// on stdout, so the exit code IS the delivery answer.
pub fn deliver_cloud_followup(entry: &RegistryEntry, message: &str) -> Result<(), String> {
    let session_id = entry
        .harness_session_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .unwrap_or(entry.short_id.as_str());
    let contract = crate::harness_capabilities::HarnessContract::packaged()
        .map_err(|e| format!("cloud-followup-failed: capability table unreadable: {e}"))?;
    let caps = contract
        .capabilities("claude")
        .map_err(|e| format!("cloud-followup-failed: {e}"))?;
    let arm = caps.cloud.as_ref().ok_or_else(|| {
        "cloud-followup-failed: the capability table declares no cloud row for claude".to_string()
    })?;
    let argv = render_holes(
        &arm.followup,
        &[("message", message), ("session_id", session_id)],
    );
    let (code, _out, err) = run_bounded(&argv, FOLLOWUP_TIMEOUT_SECS);
    match code {
        0 => Ok(()),
        _ => {
            eprintln!("{err}");
            Err(format!("cloud-followup-failed (exit {code})"))
        }
    }
}

/// The cloud lane carries the payload only. Measured 2026-10-18: the create
/// form has no model/effort/permission/add-dir/account spelling, and a
/// silently-dropped axis would launch a worker that does not do what was
/// asked. Every axis refuses by name; `Some(exit)` ends the spawn before the
/// gate releases anything.
pub fn refuse_cloud_axes(params: &serde_json::Value) -> Option<i32> {
    let set = |key: &str| -> Option<&str> {
        params
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let axes = [
        ("--model", set("model")),
        ("--effort", set("effort")),
        ("--permission-mode", set("permission_mode")),
        ("--add-dir", set("add_dir")),
        ("--agent", set("agent")),
        ("--tools", set("tools")),
        ("--deny-tools", set("deny_tools")),
        ("--account", set("account")),
    ];
    for (flag, value) in axes {
        if value.is_some() {
            eprintln!(
                "{flag} is not supported on the cloud substrate: the create \
                 form carries the description only. Drop it or use \
                 --substrate thread."
            );
            return Some(2);
        }
    }
    if params.get("yolo").and_then(|v| v.as_bool()) == Some(true) {
        eprintln!(
            "--yolo is not supported on the cloud substrate: the create form \
             carries the description only. Drop it or use --substrate thread."
        );
        return Some(2);
    }
    // Passthrough tokens travel to the client as harness_args (the spawn
    // verb's --harness-arg door), so the refusal lives here with the rest of
    // the lane's fail-closed matrix.
    if params
        .get("harness_args")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        eprintln!(
            "--harness-arg is not supported on the cloud substrate: the create \
             form carries the description only. Drop it or use --substrate thread."
        );
        return Some(2);
    }
    None
}

/// The client spawn verb's cloud arm. The fail-closed axis matrix for the
/// payload-only lane runs here (it moved out of the verb's pre-launch matrix
/// to keep client.rs under its line budget): every axis including passthrough
/// refuses by name before anything launches, a non-claude provider refuses,
/// and claude dispatches the pty-held create.
pub fn client_spawn_arm(
    home: &AgentsHome,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    surface_cwd: bool,
    provider: &str,
    params: &serde_json::Value,
) -> Option<i32> {
    if let Some(exit) = refuse_cloud_axes(params) {
        return Some(exit);
    }
    if provider != "claude" {
        eprintln!("{}", non_claude_cloud_refusal(provider));
        return Some(2);
    }
    let outcome = dispatch_claude_cloud(home, name, message, from_name, cwd, surface_cwd);
    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }
    Some(outcome.exit_code)
}

/// The refusal for a non-claude provider naming `--substrate cloud`.
pub fn non_claude_cloud_refusal(provider: &str) -> String {
    format!(
        "cloud spawn refused: cloud is a claude-harness substrate; \
         {provider} has no cloud session form"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The measured marker lines, with a SYNTHETIC id: the URL is assembled
    // at runtime so no real session address ships in the repo.
    fn view_url(id: &str) -> String {
        format!("View: https://claude.ai/code/{id}?from=cli&m=0")
    }

    fn sample_for(id: &str) -> String {
        format!(
            "Created cloud session: probe title\n{}\nResume with: claude --teleport {id}\n",
            view_url(id)
        )
    }

    /// One table owns every parse contract: the measured markers, the same
    /// markers ANSI-dressed the way the pty prints them, the URL-path belt
    /// for a CLI that drops the Resume line, and the refusal when no id
    /// survives at all (the spawn refuses rather than mint a phantom row).
    #[test]
    fn parse_create_output_contracts() {
        let id = "session_SYNTH0001";
        let plain = parse_create_output(&sample_for(id), "session_").expect("parses");
        assert_eq!(plain.session_id, id);
        assert_eq!(plain.title.as_deref(), Some("probe title"));
        assert_eq!(
            plain.view_url.as_deref(),
            view_url(id).strip_prefix("View: ")
        );

        let dressed = "\x1b[1mCreated cloud session:\x1b[0m probe title\n\x1b[38;2;9;9;9mResume with: claude --teleport session_SYNTH0003\x1b[0m\n";
        let dressed = parse_create_output(dressed, "session_").expect("parses");
        assert_eq!(dressed.session_id, "session_SYNTH0003");
        assert_eq!(dressed.title.as_deref(), Some("probe title"));

        let url_only = format!(
            "Created cloud session: t\n{}\n",
            view_url("session_SYNTH0002")
        );
        let fallback = parse_create_output(&url_only, "session_").expect("parses");
        assert_eq!(fallback.session_id, "session_SYNTH0002");

        assert!(parse_create_output("Created cloud session: t\n", "session_").is_none());

        // The packaged mail argv: the contract row's follow-up renders with
        // its message and session-id holes filled.
        let contract =
            crate::harness_capabilities::HarnessContract::packaged().expect("packaged contract");
        let caps = contract.capabilities("claude").expect("claude row");
        let arm = caps.cloud.as_ref().expect("the claude cloud row");
        assert_eq!(
            render_holes(
                &arm.followup,
                &[("message", "hello"), ("session_id", "session_z")],
            ),
            vec![
                "claude".to_string(),
                "-p".to_string(),
                "hello".to_string(),
                "--cloud".to_string(),
                "session_z".to_string(),
            ]
        );
    }
}

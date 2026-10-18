//! The claude cloud lane: create a cloud session under a locally held pty,
//! read the minted session id back, and record the registry row.
//!
//! Measured on the installed CLI (x-9e5b probes, 2026-10-18): `claude --cloud`
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
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::claude_ask::AskOutcome;
use crate::paths::AgentsHome;
use crate::state::{load_registry, update_registry, RegistryEntry};
use crate::AgentStatus;

/// Bound the create read. The measured create exits in seconds; 120s covers a
/// slow VM handoff without letting a silent prompt wedge the spawn.
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);

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
    let mut argv = arm.launch.clone();
    for token in &mut argv {
        if token == "{payload}" {
            *token = message.to_string();
        }
    }

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
                 {cwd}. Run claude in that directory once and accept the trust \
                 prompt, then retry.\n"
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
        // pid, no messaging socket.
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

    // Receipt. The last stdout line must stay the JSON object: the Python
    // bridge parses exactly that line.
    let mut receipt = serde_json::Map::new();
    receipt.insert("short_id".into(), cloud_id.clone().into());
    receipt.insert("session_id".into(), cloud_id.clone().into());
    receipt.insert("name".into(), name.to_string().into());
    receipt.insert("substrate".into(), "cloud".into());
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
    AskOutcome {
        stdout: format!(
            "Created cloud session: {}\n{}\nResume with: claude --teleport {cloud_id}\n\
             {cwd_note}{}\n",
            created.title.as_deref().unwrap_or(name),
            created
                .view_url
                .as_ref()
                .map(|u| format!("View: {u}"))
                .unwrap_or_default(),
            serde_json::to_string(&serde_json::Value::Object(receipt)).unwrap_or_default()
        ),
        stderr: String::new(),
        exit_code: 0,
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
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Created cloud session: Repository commit and branch probe\n\
View: https://claude.ai/code/session_01UWUSyDG4JvNYLaioWPq7i9?from=cli&m=0\n\
Resume with: claude --teleport session_01UWUSyDG4JvNYLaioWPq7i9\n";

    #[test]
    fn parse_reads_id_title_and_url() {
        let parsed = parse_create_output(SAMPLE, "session_").expect("parses");
        assert_eq!(parsed.session_id, "session_01UWUSyDG4JvNYLaioWPq7i9");
        assert_eq!(
            parsed.title.as_deref(),
            Some("Repository commit and branch probe")
        );
        assert_eq!(
            parsed.view_url.as_deref(),
            Some("https://claude.ai/code/session_01UWUSyDG4JvNYLaioWPq7i9?from=cli&m=0")
        );
    }

    #[test]
    fn parse_survives_ansi_dressing() {
        let dressed = "\x1b[1mCreated cloud session:\x1b[0m probe title\n\x1b[38;2;9;9;9mResume with: claude --teleport session_abc123def\x1b[0m\n";
        let parsed = parse_create_output(dressed, "session_").expect("parses");
        assert_eq!(parsed.session_id, "session_abc123def");
        assert_eq!(parsed.title.as_deref(), Some("probe title"));
    }

    #[test]
    fn parse_falls_back_to_the_url_path() {
        let url_only =
            "Created cloud session: t\nView: https://claude.ai/code/session_01ABC?from=cli&m=0\n";
        let parsed = parse_create_output(url_only, "session_").expect("parses");
        assert_eq!(parsed.session_id, "session_01ABC");
    }

    #[test]
    fn parse_refuses_without_a_session_id() {
        assert!(parse_create_output("Created cloud session: t\n", "session_").is_none());
    }

    #[test]
    fn strip_ansi_eats_csi_and_osc() {
        assert_eq!(
            strip_ansi("\x1b[2Ghello\x1b]0;title\x07 world"),
            "hello world"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }
}

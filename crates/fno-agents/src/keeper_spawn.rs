//! The keeper-thread spawn arm: `spawn <name> -H <keeper harness> --substrate
//! thread` for the harnesses whose `[harness.<h>.keeper]` row exists (pi,
//! cursor-agent, grok, agy).
//!
//! The Python twin (`keeper_thread.py` + `_lane_b_thread_spawn`) served every
//! mode through the seam carve-out at `rust_runtime.py`; in rust mode this
//! module is the lane and the Python leg serves python mode until the
//! dispatch_spawn port deletes it. The launch is `keeper_revival`'s: one
//! launcher for spawn and revival, as thread-lanes.md names. The refusal
//! sentences ride the capability rows verbatim, and the shapes (argv,
//! receipt, row fields) are pinned against the Python leg's goldens under
//! `tests/golden/keeper_spawn/`.

use crate::agent_lock::AgentLock;
use crate::claude_ask::AskOutcome;
use crate::daemon::KeeperProbe;
use crate::keeper_revival::{launch_and_identify, IdentifyOutcome, LaunchSpec};
use crate::state::RegistryEntry;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
/// The seed dance's deadlines and nudge cadence, ported from
/// `_keeper_seed_submit`.
const READY_WINDOW: Duration = Duration::from_secs(60);
const ECHO_WINDOW: Duration = Duration::from_secs(120);
const RESIZE_NUDGE_EVERY: Duration = Duration::from_secs(4);
/// The hosted TUI's composer-up paint when the row declares none: measured on
/// cursor-agent's status line (the Python default argument).
const DEFAULT_READY_MARKER: &str = "Plan, search, build anything";

// == Section 1: types ==

/// A refusal on the spawn path: the operator line plus the exit code the
/// Python lane exits with.
#[derive(Debug)]
pub(crate) struct SpawnRefusal {
    message: String,
    exit: i32,
}

impl SpawnRefusal {
    fn err(message: impl Into<String>, exit: i32) -> Self {
        Self {
            message: message.into(),
            exit,
        }
    }
}

/// The machine touches the arm makes, injected so the tests drive the whole
/// flow without a worker binary, a live keeper, or a cursor install.
pub(crate) struct Seams<'a> {
    pub mint: &'a dyn Fn(&str, &Path, Option<&str>) -> Result<String, SpawnRefusal>,
    pub probe: &'a dyn Fn(&Path) -> KeeperProbe,
    pub launch: &'a dyn Fn(&LaunchSpec) -> Result<u32, String>,
    pub kill: &'a dyn Fn(u32),
    /// The worker binary the arm seats the keeper on; production resolves it
    /// beside the running binary, and a test names any existing path.
    pub worker_bin: &'a Path,
}

/// True when the harness's capability contract carries a keeper row: the one
/// routing fact the client arm reads, never a name list.
pub fn carries_keeper_row(harness: &str) -> bool {
    crate::harness_capabilities::HarnessContract::packaged()
        .ok()
        .and_then(|contract| {
            contract
                .capabilities(harness)
                .ok()
                .map(|caps| caps.keeper.is_some())
        })
        .unwrap_or(false)
}

fn keeper_arm(harness: &str) -> Option<crate::harness_capabilities::KeeperArm> {
    let contract = crate::harness_capabilities::HarnessContract::packaged().ok()?;
    let caps = contract.capabilities(harness).ok()?;
    caps.keeper.clone()
}
// == Section 2: production entry ==

/// The arm the client routes keeper-row thread spawns to. `harness_args` is
/// the fenced passthrough tail; `params` carries the parsed spawn request.
pub fn dispatch(
    home: &crate::paths::AgentsHome,
    harness: &str,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    harness_args: &[String],
    params: &Value,
) -> AskOutcome {
    let worker_bin = crate::daemon::resolve_worker_bin();
    let seams = Seams {
        mint: &production_mint,
        probe: &|sock| crate::daemon::probe_keeper_socket(sock, Duration::from_secs(2)),
        launch: &launch_spec,
        kill: &crate::keeper_revival::terminate_keeper,
        worker_bin: &worker_bin,
    };
    dispatch_with(
        home,
        harness,
        name,
        message,
        from_name,
        cwd,
        harness_args,
        params,
        &seams,
    )
}

fn launch_spec(spec: &LaunchSpec) -> Result<u32, String> {
    crate::keeper_revival::launch_keeper(spec)
}

/// The production mint: cursor mints through `create-chat`, agy validates a
/// requested id and binds none at launch, any other callee-minted row refuses
/// by name, and everything else takes the UUIDv4 default.
fn production_mint(
    harness: &str,
    cwd: &Path,
    requested: Option<&str>,
) -> Result<String, SpawnRefusal> {
    production_mint_with(&cursor_mint, harness, cwd, requested)
}

/// The cursor leg is a closure so a test can fake the `create-chat` helper.
fn cursor_mint(harness: &str, cwd: &Path, requested: Option<&str>) -> Result<String, SpawnRefusal> {
    debug_assert_eq!(harness, "cursor-agent");
    match requested {
        Some(id) => match crate::cursor_agent::chat_id_error(id) {
            Some(error) => Err(SpawnRefusal::err(error, 2)),
            None => Ok(id.to_string()),
        },
        None => crate::cursor_agent::create_chat(cwd).map_err(|e| SpawnRefusal::err(e, 2)),
    }
}

fn production_mint_with(
    cursor: &dyn Fn(&str, &Path, Option<&str>) -> Result<String, SpawnRefusal>,
    harness: &str,
    cwd: &Path,
    requested: Option<&str>,
) -> Result<String, SpawnRefusal> {
    match harness {
        "cursor-agent" => cursor(harness, cwd, requested),
        "agy" => match requested {
            Some(id) => require_conversation_id(id).map_err(|e| SpawnRefusal::err(e, 2)),
            None => Ok(mint_uuid_v4()),
        },
        _ => {
            let callee_minted = crate::harness_capabilities::HarnessContract::packaged()
                .ok()
                .and_then(|contract| {
                    contract
                        .capabilities(harness)
                        .ok()
                        .map(|caps| caps.session_binding.strategy == "callee-minted-read-back")
                })
                .unwrap_or(false);
            if callee_minted {
                return Err(SpawnRefusal::err(
                    format!(
                        "{harness} declares session_binding.strategy = \
                         callee-minted-read-back but fno has no mint for it; the \
                         caller-assigned UUIDv4 fallback launches the keeper on an id the \
                         harness never adopts, and Identify reports that fabricated id"
                    ),
                    2,
                ));
            }
            Ok(mint_uuid_v4())
        }
    }
}
// == Section 3: mint helpers ==

/// A caller-assigned UUIDv4, minted from the OS CSPRNG: version and variant
/// nibbles set, dash-grouped like Python's `uuid.uuid4()`.
fn mint_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    version4(bytes)
}

fn version4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// agy resumes by exact id: a partial one silently opens a different
/// conversation, so the id validates before any process launches on it.
fn require_conversation_id(id: &str) -> Result<String, String> {
    let text = id.trim();
    if !is_full_uuid(text) {
        return Err(format!(
            "agy conversation id {id:?} is not a full UUID; \
             agy resumes by exact id and a partial one silently opens a \
             different conversation"
        ));
    }
    Ok(text.to_string())
}

fn is_full_uuid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(part, len)| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit()))
}
// == Section 4: the flow ==

/// The hermetic core. Every refusal keeps the Python lane's exit code: 2 for
/// a shape the caller got wrong, 13 for a missing runtime, 12 for a registry
/// failure, 1 for a keeper that launched and then failed proof.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_with(
    home: &crate::paths::AgentsHome,
    harness: &str,
    name: &str,
    message: &str,
    from_name: &str,
    cwd: &Path,
    harness_args: &[String],
    params: &Value,
    seams: &Seams,
) -> AskOutcome {
    let refused = |r: SpawnRefusal| AskOutcome {
        stdout: String::new(),
        stderr: format!("{}\n", r.message),
        exit_code: r.exit,
    };
    let Some(arm) = keeper_arm(harness) else {
        return refused(SpawnRefusal::err(
            format!(
                "harness {harness:?} is not a keeper-lane harness \
                 (thread_lane != 'keeper'); lane B hosts transcript-only \
                 harnesses, and lane A keeps its harness-owned path"
            ),
            2,
        ));
    };
    // Substrate-stance refusals before anything launches. The paired stance
    // answers with its once sentence; a lane with no headless sentence falls
    // back to the once line (pi names its unmeasured headless stance there).
    let headless = params.get("substrate").and_then(Value::as_str) == Some("headless");
    let once = params.get("once").and_then(Value::as_bool) == Some(true);
    if headless || once {
        let line = if once || arm.once_and_headless_together || arm.headless_refusal.is_empty() {
            arm.once_refusal.clone()
        } else {
            arm.headless_refusal.clone()
        };
        return refused(SpawnRefusal::err(line, 2));
    }
    if let Err(msg) = crate::claude_ask::validate_spawn_inputs(name, from_name) {
        return refused(SpawnRefusal::err(msg, 2));
    }
    if !seams.worker_bin.exists() {
        return refused(SpawnRefusal::err(
            "lane-B thread spawn needs the fno-agents runtime; install it \
             (cargo build --release -p fno-agents) or spawn on the pane lane (--split)",
            13,
        ));
    }
    let _lock = match AgentLock::acquire(home, name, LOCK_ACQUIRE_TIMEOUT) {
        Ok(lock) => lock,
        Err(()) => {
            return refused(SpawnRefusal::err(
                format!(
                    "timed out waiting for the agent lock on {name:?}{}",
                    crate::agent_lock::holder_note(home, name)
                ),
                13,
            ));
        }
    };
    let registry_path = home.registry_json();
    match crate::state::load_registry(&registry_path) {
        Ok(registry) => {
            if registry.entries.iter().any(|e| e.name == name) {
                return refused(SpawnRefusal::err(
                    format!(
                        "agent {name:?} already exists; \
                         use 'fno agents rm {name}' first or pick another name"
                    ),
                    2,
                ));
            }
        }
        Err(e) => {
            return refused(SpawnRefusal::err(format!("registry read failed: {e}"), 12));
        }
    }

    let session_id = match (seams.mint)(harness, cwd, None) {
        Ok(id) => id,
        Err(r) => return refused(r),
    };
    let mut argv = match crate::harness_capabilities::render_session_argv(
        harness,
        "interactive_create",
        Some(&session_id),
    ) {
        Ok(argv) => argv,
        Err(e) => {
            return refused(SpawnRefusal::err(
                format!("harness {harness:?} cannot render an interactive_create argv: {e}"),
                2,
            ));
        }
    };
    if harness == "agy" {
        crate::agy_ask::ensure_agy_folder_trusted(cwd);
    }
    match crate::agy_launch::keeper_posture(harness, "thread") {
        Ok(posture) => argv.extend(posture.tokens),
        Err(reason) => return refused(SpawnRefusal::err(reason, 2)),
    }
    if harness == "pi" {
        // Bare pi defaults to provider google; the pane lane asks the same
        // route owner.
        let route = crate::pi::pi_route("", "", "", "");
        argv.extend(route.tokens);
    }
    if !harness_args.is_empty() {
        argv.push("--".to_string());
        argv.extend(harness_args.iter().cloned());
    }
    let state_root = home.root().parent().unwrap_or(home.root());
    let sock = thread_socket(state_root, name);
    let keeper_log = home.root().join(name).join("keeper.log");
    let mut keeper_argv = vec![worker_bin_str(seams)];
    keeper_argv.extend([
        "--keeper".to_string(),
        "--sock".to_string(),
        sock.to_string_lossy().into_owned(),
        "--session".to_string(),
        name.to_string(),
        "--pane-key".to_string(),
        session_id.clone(),
        "--cwd".to_string(),
        cwd.to_string_lossy().into_owned(),
        "--".to_string(),
    ]);
    keeper_argv.extend(argv.iter().cloned());
    let spec = LaunchSpec {
        argv: keeper_argv,
        sock: sock.clone(),
        cwd: cwd.to_path_buf(),
        keeper_log: keeper_log.clone(),
        agent_name: name.to_string(),
        harness: harness.to_string(),
        session_id: session_id.clone(),
    };
    let (keeper_pid, child_pid) = match launch_and_identify(&spec, seams.probe, seams.launch) {
        Err(reason) => {
            return refused(SpawnRefusal::err(
                format!(
                    "failed to launch the keeper on {}: {reason}; see {}",
                    sock.display(),
                    keeper_log.display()
                ),
                1,
            ));
        }
        Ok((
            _,
            IdentifyOutcome::Ours {
                keeper_pid,
                child_pid,
            },
        )) => (keeper_pid, child_pid),
        Ok((launched, IdentifyOutcome::Wrong { answered_sid, .. })) => {
            (seams.kill)(launched);
            // Read liveness BEFORE judging: a collision means OUR keeper
            // already exited (it refuses to bind a socket a live keeper
            // serves), while an identity bug means the keeper answering is
            // ours and still alive.
            let answered = py_repr_opt(answered_sid.as_deref());
            if crate::agent_lock::pid_is_alive(launched as u64) {
                return refused(SpawnRefusal::err(
                    format!(
                        "keeper answered session_id {answered}, \
                         expected the minted {session_id:?}"
                    ),
                    1,
                ));
            }
            return refused(SpawnRefusal::err(
                format!(
                    "another keeper still holds {} and answered session_id {answered}; our \
                     keeper refused to bind. Stop the old keeper (or 'fno agents rm {name}') \
                     and retry",
                    sock.display()
                ),
                1,
            ));
        }
        Ok((launched, IdentifyOutcome::TimedOut)) => {
            (seams.kill)(launched);
            return refused(SpawnRefusal::err(
                format!(
                    "lane-B keeper never answered Identify on {}: timed out after 10s; \
                     see {}",
                    sock.display(),
                    keeper_log.display()
                ),
                1,
            ));
        }
    };
    let mut entry = build_keeper_thread_entry(
        name,
        harness,
        cwd,
        &session_id,
        keeper_pid,
        child_pid,
        &sock,
        &keeper_log,
    );
    entry.status = crate::AgentStatus::Live;
    // Re-check the name UNDER the registry write: the per-agent flock
    // serializes Rust-client creates, and a concurrent foreign writer is the
    // collision the closure names.
    match crate::state::update_registry(&registry_path, |reg| {
        if reg.find(name).is_some() {
            false
        } else if let Some(row) = reg.find_by_session_mut(harness, &session_id) {
            // The harness's session-start hook registered this minted session
            // first, under its short id and with no lineage. Claim that row
            // for the spawn instead of pushing a second row the identity check
            // would refuse.
            *row = adopt_self_registered(std::mem::take(row), entry.clone());
            true
        } else {
            reg.entries.push(entry.clone());
            true
        }
    }) {
        Ok(true) => {}
        Ok(false) => {
            (seams.kill)(keeper_pid);
            return refused(SpawnRefusal::err(
                format!(
                    "agent {name:?} already exists (registered concurrently); the keeper for \
                     the session was stopped"
                ),
                2,
            ));
        }
        Err(e) => {
            (seams.kill)(keeper_pid);
            return refused(SpawnRefusal::err(
                format!("registry write failed: {e}; keeper for {name:?} stopped"),
                12,
            ));
        }
    }
    // The Python lane releases the per-agent flock after the registry write
    // and pastes the seed outside it; the seed can wait out two minutes of
    // paint, and holding the name that long would queue every other
    // same-name operation behind it.
    drop(_lock);
    let rendered = crate::provider::render_verb_seed(message, harness);
    if !rendered.trim().is_empty() {
        let ready_marker: &[u8] = if arm.ready_marker.is_empty() {
            DEFAULT_READY_MARKER.as_bytes()
        } else {
            arm.ready_marker.as_bytes()
        };
        let modal = arm.clear_modal.first().map(|pattern| {
            (
                pattern.as_str(),
                arm.clear_modal
                    .get(1)
                    .map(|keys| keys.as_bytes())
                    .unwrap_or(b"\r"),
            )
        });
        if let Err(e) = seed_submit(&sock, &rendered, ready_marker, modal, name) {
            return refused(e);
        }
    }

    crate::client_verbs::append_agents_event(
        &crate::client_verbs::trace_events_path(home),
        "agent_ask_done",
        &[
            ("stage", Value::String("dispatch".into())),
            ("name", Value::String(name.to_string())),
            ("provider", Value::String(harness.to_string())),
            ("substrate", Value::String("thread".into())),
        ],
    );

    // The receipt: the created-spawn shape the CLI prints, byte-parity JSON
    // (the claude arm's `json_string_ascii` mirrors the Python `json.dumps`
    // values). A verb-rendered seed carries the effective message and the
    // pending verb-fired word; the verb-marker chain stays Python until the
    // dispatch_spawn port.
    let mut receipt = format!(
        "{{\"name\": {}, \"short_id\": {}, \"harness\": {}, \"status\": \"live\"",
        crate::claude_ask::json_string_ascii(name),
        crate::claude_ask::json_string_ascii(&session_id),
        crate::claude_ask::json_string_ascii(harness),
    );
    if rendered != message {
        receipt.push_str(&format!(
            ", \"effective_message\": {}, \"verb_fired\": \"pending\"",
            crate::claude_ask::json_string_ascii(&rendered)
        ));
    }
    receipt.push('}');
    AskOutcome {
        stdout: format!("{receipt}\n"),
        stderr: String::new(),
        exit_code: 0,
    }
}

// == Section 5: the row and small helpers ==

/// The row the keeper-hosted thread is born with: the minted id, the keeper
/// pids off the Identify proof, the socket, and the ambient parent edge (the
/// seam scrubs identity before exec, so a client-side birth records the
/// ambient disposition reason when no parent survives the scrub).
#[allow(clippy::too_many_arguments)]
fn build_keeper_thread_entry(
    name: &str,
    harness: &str,
    cwd: &Path,
    session_id: &str,
    keeper_pid: u32,
    child_pid: Option<u32>,
    sock: &Path,
    keeper_log: &Path,
) -> RegistryEntry {
    let cwd_s = cwd.to_string_lossy().into_owned();
    RegistryEntry {
        name: name.to_string(),
        short_id: String::new(),
        legacy_provider: String::new(),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        harness: Some(harness.to_string()),
        harness_session_id: Some(session_id.to_string()),
        cwd: cwd_s.clone(),
        project_root: cwd_s,
        origin: Some("spawn".into()),
        spawn_trigger: None,
        host_mode: Some(crate::state::HOST_MODE_INTERACTIVE.into()),
        status: crate::AgentStatus::Live,
        last_message_at: Some(crate::daemon::now_rfc3339_like()),
        created_at: crate::daemon::now_rfc3339_like(),
        pid: Some(keeper_pid),
        pid_start_time: crate::daemon::process_start_time(keeper_pid),
        keeper_child_pid: child_pid,
        messaging_socket_path: Some(sock.to_string_lossy().into_owned()),
        log_path: Some(keeper_log.to_string_lossy().into_owned()),
        substrate: Some("thread".into()),
        ..RegistryEntry::new(
            Some(session_id.to_string()),
            crate::spawn_lineage::ambient_lineage(),
        )
    }
}

/// The spawn's row, with what the hook-registered row for the same session
/// already knew carried over: its short id, creation time, and any model,
/// account or node the spawn does not set.
fn adopt_self_registered(existing: RegistryEntry, spawned: RegistryEntry) -> RegistryEntry {
    RegistryEntry {
        short_id: existing.short_id,
        created_at: if existing.created_at.is_empty() {
            spawned.created_at
        } else {
            existing.created_at
        },
        provider: spawned.provider.or(existing.provider),
        model: spawned.model.or(existing.model),
        model_basis: spawned.model_basis.or(existing.model_basis),
        effort: spawned.effort.or(existing.effort),
        node: spawned.node.or(existing.node),
        account_record_id: spawned.account_record_id.or(existing.account_record_id),
        ..spawned
    }
}

/// Python `repr` of an Optional[str] in the refusal lines: `None` or a
/// single-quoted string, exactly as the Python lane prints it.
fn py_repr_opt(value: Option<&str>) -> String {
    match value {
        None => "None".to_string(),
        Some(s) => format!("'{s}'"),
    }
}

fn thread_socket(state_root: &Path, name: &str) -> PathBuf {
    crate::convert::keeper_rebind::thread_socket_path(state_root, name)
}

fn worker_bin_str(seams: &Seams) -> String {
    seams.worker_bin.to_string_lossy().into_owned()
}

// == Section 6: the seed paste ==

/// Deliver a thread spawn's seed to its keeper-hosted TUI, the port of
/// `_keeper_seed_submit`: one connection taken as soon as the spawn returns,
/// a Resize nudge alternating sizes every 4s (a same-size Resize fires no
/// SIGWINCH, and the ring never replays to a late subscriber), the row's
/// composer-up paint as the ready marker, a once-only modal answer, then the
/// paste, the submit, and the repaint of the submitted line as the landing
/// proof. A miss is a refusal, never a silent drop.
fn seed_submit(
    sock: &Path,
    message: &str,
    ready_marker: &[u8],
    clear_modal: Option<(&str, &[u8])>,
    name: &str,
) -> Result<(), SpawnRefusal> {
    use crate::pane_keeper::{decode, encode, Decode, Frame};
    use std::io::{Read, Write};

    let mut conn = std::os::unix::net::UnixStream::connect(sock).map_err(|e| {
        SpawnRefusal::err(
            format!(
                "keeper thread for {name:?} never painted its idle composer within 60s; \
                 the seed was not delivered. Inspect with 'fno agents peek {name}' and \
                 send the prompt by mail. (socket: {e})"
            ),
            1,
        )
    })?;
    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
    let mut raw_pending: Vec<u8> = Vec::new();
    let mut text: Vec<u8> = Vec::new();

    let drain = |conn: &mut std::os::unix::net::UnixStream,
                 raw_pending: &mut Vec<u8>,
                 text: &mut Vec<u8>|
     -> std::io::Result<bool> {
        let mut chunk = [0u8; 65536];
        match conn.read(&mut chunk) {
            Ok(0) => Ok(false),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Ok(true)
            }
            Err(_) => Err(std::io::Error::other("read failed")),
            Ok(n) => {
                raw_pending.extend_from_slice(&chunk[..n]);
                loop {
                    match decode(raw_pending) {
                        Decode::NeedMore => break,
                        Decode::Violation(_) => {
                            raw_pending.clear();
                            break;
                        }
                        Decode::Frame(Frame::Output(payload), used) => {
                            raw_pending.drain(..used);
                            text.extend(strip_ansi(&payload));
                        }
                        Decode::Frame(_, used) => {
                            raw_pending.drain(..used);
                        }
                    }
                }
                Ok(true)
            }
        }
    };
    let modal_re = clear_modal.map(|(pattern, _)| {
        regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .ok()
    });
    let mut modal_answered = false;
    let mut flip = false;
    // The first nudge fires on the first pass, like the Python lane's zero
    // initial `last_nudge`.
    let mut first_pass = true;
    let mut last_nudge = std::time::Instant::now();
    let deadline = std::time::Instant::now() + READY_WINDOW;
    loop {
        if std::time::Instant::now() >= deadline {
            let answered = if modal_answered {
                " A blocking modal was answered and the composer still never painted."
            } else {
                ""
            };
            return Err(SpawnRefusal::err(
                format!(
                    "keeper thread for {name:?} never painted its idle composer within 60s; \
                     the seed was not delivered.{answered} Inspect with 'fno agents peek \
                     {name}' and send the prompt by mail."
                ),
                1,
            ));
        }
        if let Some((_pattern, keys)) = clear_modal {
            if !modal_answered {
                if let Some(Some(re)) = modal_re.as_ref() {
                    if re.is_match(&String::from_utf8_lossy(&text)) {
                        let _ = conn.write_all(&encode(&Frame::Input(keys.to_vec())));
                        modal_answered = true;
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }
        }
        if contains_sub(&text, ready_marker) {
            break;
        }
        if first_pass || last_nudge.elapsed() >= RESIZE_NUDGE_EVERY {
            first_pass = false;
            flip = !flip;
            let (rows, cols) = if flip { (25, 81) } else { (24, 80) };
            let _ = conn.write_all(&encode(&Frame::Resize(rows, cols)));
            last_nudge = std::time::Instant::now();
        }
        match drain(&mut conn, &mut raw_pending, &mut text) {
            Ok(true) => {}
            Ok(false) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }

    let _ = conn.write_all(&encode(&Frame::Input(message.as_bytes().to_vec())));
    std::thread::sleep(Duration::from_millis(100));
    let _ = conn.write_all(&encode(&Frame::Input(b"\r".to_vec())));

    // A composer-wide prefix: the TUI wraps long lines, and a newline
    // mid-marker would break a full-line match even after the strip. Chars,
    // not bytes - a byte cut can split a multi-byte character into a partial
    // codepoint that never appears in the complete-UTF-8 pty stream. An empty
    // first line yields an empty needle, which matches anything, so it
    // refuses instead of confirming nothing.
    let first_line = message.lines().next().unwrap_or("");
    let marker: String = first_line.chars().take(48).collect();
    if marker.is_empty() {
        return Err(SpawnRefusal::err(
            format!(
                "the seed for keeper thread {name:?} starts with an empty line, so its \
                 landing can never be confirmed; lead the seed with the payload's first \
                 real line"
            ),
            2,
        ));
    }
    let marker_bytes = marker.as_bytes();
    let base = text.len();
    let deadline = std::time::Instant::now() + ECHO_WINDOW;
    while std::time::Instant::now() < deadline {
        if text.len() >= base && contains_sub(&text[base..], marker_bytes) {
            return Ok(());
        }
        match drain(&mut conn, &mut raw_pending, &mut text) {
            Ok(true) => {}
            Ok(false) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    Err(SpawnRefusal::err(
        format!(
            "the seed for keeper thread {name:?} never repainted after submit; the worker \
             may be idle. Send it by mail: fno agents mail send {name}"
        ),
        1,
    ))
}

fn contains_sub(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Byte-level ANSI CSI strip, the port of `_strip_ansi`: the TUI paints one
/// character per escape-wrapped write, so no marker matches the raw stream.
fn strip_ansi(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == 0x1B {
            if i + 1 < raw.len() && raw[i + 1] == b'[' {
                i += 2;
                while i < raw.len() && !(0x40..=0x7E).contains(&raw[i]) {
                    i += 1;
                }
                i += 1;
            } else {
                i += 2;
            }
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    out
}

// == Section 7: tests ==

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    const SID: &str = "11111111-2222-3333-4444-555555555555";
    const WK: &str = "wk-pi";

    fn goldens() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/golden/keeper_spawn/python_leg.json"))
            .expect("the captured Python-leg goldens parse")
    }

    fn home() -> (tempfile::TempDir, crate::paths::AgentsHome) {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(dir.path().join("agents"));
        std::fs::create_dir_all(home.root()).unwrap();
        (dir, home)
    }

    fn fake_worker_bin() -> &'static str {
        "/bin/echo"
    }

    fn no_launch() -> impl Fn(&LaunchSpec) -> Result<u32, String> {
        |_spec| panic!("nothing should launch on this path")
    }

    fn read_row(home: &crate::paths::AgentsHome, name: &str) -> Option<RegistryEntry> {
        crate::state::load_registry(&home.registry_json())
            .unwrap()
            .entries
            .into_iter()
            .find(|e| e.name == name)
    }
    /// The create argv and the refusal sentences match the Python leg's
    /// capture, per keeper harness: the shapes both lanes must agree on.
    #[test]
    fn the_python_leg_goldens_hold() {
        let g = goldens();
        for harness in ["pi", "cursor-agent", "grok", "agy"] {
            let expected: Vec<String> = g[harness]["create_argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().replace(SID, "SID"))
                .collect();
            let rendered: Vec<String> = crate::harness_capabilities::render_session_argv(
                harness,
                "interactive_create",
                Some(SID),
            )
            .unwrap()
            .iter()
            .map(|t| t.replace(SID, "SID"))
            .collect();
            assert_eq!(rendered, expected, "harness {harness}");
            let arm = keeper_arm(harness).unwrap();
            let row = &g[harness]["row"];
            assert_eq!(arm.once_refusal, row["once_refusal"].as_str().unwrap());
            let headless = row["headless_refusal"].as_str().unwrap_or("");
            assert_eq!(arm.headless_refusal, headless, "harness {harness}");
        }
    }

    /// The row carries the field set the Python mint writes, and the
    /// receipt keys stay inside the Python SpawnResult vocabulary.
    #[test]
    fn the_row_and_receipt_match_the_python_leg() {
        let g = goldens();
        let entry = build_keeper_thread_entry(
            WK,
            "pi",
            Path::new("/repo"),
            SID,
            4242,
            Some(4243),
            Path::new("/state/mux/threads/wk-pi.sock"),
            Path::new("/state/agents/wk-pi/keeper.log"),
        );
        let row = serde_json::to_value(&entry).unwrap();
        for field in [
            "harness_session_id",
            "keeper_child_pid",
            "messaging_socket_path",
            "substrate",
            "origin",
            "host_mode",
            "log_path",
            "pid",
            "status",
        ] {
            assert!(row.get(field).is_some(), "row field {field} missing");
        }
        // pid_start_time skips when absent (the RegistryEntry serde contract,
        // mirrored from Python's AgentEntry): the fake keeper pid 4242 has no
        // readable start time, so the row must omit the field, not null it.
        assert!(
            row.get("pid_start_time").is_none(),
            "a fake keeper pid carries no start time; the field skips when absent"
        );
        assert_eq!(row["substrate"], "thread");
        assert_eq!(row["harness_session_id"], SID);
        assert_eq!(row["keeper_child_pid"], 4243);
        assert_eq!(row["origin"], "spawn");
        // The golden lists the SpawnResult dataclass fields; the printed
        // receipt maps "harness" <- provider, and "status" is the printer's
        // own "live" constant with no field behind it.
        let receipt_keys = g["receipt_fields"].as_array().unwrap();
        for key in ["name", "provider", "short_id", "effective_message"] {
            assert!(
                receipt_keys.iter().any(|v| v.as_str() == Some(key)),
                "SpawnResult field {key} absent from the python leg"
            );
        }
    }

    /// The mint binds ids like the Python lane: the UUIDv4 default carries
    /// its version and variant nibbles, cursor mints through the injected
    /// create-chat, agy takes no mint turn, and a callee-minted row with no
    /// mint refuses by name.
    #[test]
    fn the_mint_binds_ids_like_the_python_lane() {
        for _ in 0..16 {
            let id = mint_uuid_v4();
            assert!(is_full_uuid(&id), "{id}");
            let parts: Vec<&str> = id.split('-').collect();
            assert!(parts[2].starts_with('4'), "{id}");
            assert!(
                matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b')),
                "{id}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let minted = "74db359a-1111-4111-8111-111111111111";
        let cursor = |_h: &str, _cwd: &Path, _req: Option<&str>| Ok(minted.to_string());
        assert_eq!(
            production_mint_with(&cursor, "cursor-agent", dir.path(), None).unwrap(),
            minted
        );
        let agy = production_mint_with(&cursor, "agy", dir.path(), None).unwrap();
        assert!(is_full_uuid(&agy), "{agy}");
        let truncated = production_mint_with(&cursor, "agy", dir.path(), Some("abc"));
        assert!(truncated.is_err());
        let no_mint = production_mint_with(&cursor, "zcode", dir.path(), None);
        let err = no_mint.unwrap_err();
        assert!(
            err.message.contains("callee-minted-read-back"),
            "{}",
            err.message
        );
        assert_eq!(err.exit, 2);
    }

    /// The whole flow against a fake keeper: launch argv, Identify proof,
    /// live row, seed paste + echo proof, and the created receipt.
    #[test]
    fn a_thread_spawn_launches_proves_and_pastes() {
        let (dir, home) = home();
        let sock = dir.path().join("mux").join("threads").join("wk-pi.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
        // The paint rides escape-wrapped characters: the match proves the
        // ANSI strip, not just the frame decode.
        let keeper = Arc::new(FakeKeeper::new(
            &sock,
            b"\x1b[1m(\x1b[0msu\x1b[2mb\x1b[0m) composer ready".to_vec(),
        ));
        {
            let k = Arc::clone(&keeper);
            std::thread::spawn(move || k.serve());
        }
        let seams = Seams {
            mint: &|_h, _cwd, _req| Ok(SID.to_string()),
            probe: &|s: &Path| crate::daemon::probe_keeper_socket(s, Duration::from_millis(500)),
            launch: &|_spec| Ok(4242),
            kill: &|_pid| {},
            worker_bin: Path::new(fake_worker_bin()),
        };
        let params = json!({"substrate": "thread"});
        let outcome = dispatch_with(
            &home,
            "pi",
            WK,
            "hello world",
            "tester",
            dir.path(),
            &[],
            &params,
            &seams,
        );
        assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
        let row = read_row(&home, WK).expect("the row is written");
        assert_eq!(row.status, crate::AgentStatus::Live);
        assert_eq!(row.pid, Some(4242));
        assert_eq!(row.keeper_child_pid, Some(4243));
        assert_eq!(row.substrate.as_deref(), Some("thread"));
        assert_eq!(
            row.messaging_socket_path.as_deref(),
            Some(sock.to_str().unwrap())
        );
        let receipt: Value = serde_json::from_str(outcome.stdout.trim()).unwrap();
        assert_eq!(receipt["name"], WK);
        assert_eq!(receipt["short_id"], SID);
        assert_eq!(receipt["harness"], "pi");
        assert_eq!(receipt["status"], "live");
        // The fake keeper recorded the paste with its submit.
        assert_eq!(keeper.pasted(), b"hello world\r");
    }

    /// Nothing lands under the name when proof or uniqueness fails: a keeper
    /// that never answers Identify is killed and writes no row; an existing
    /// name refuses before the launch; a concurrent registry write of the
    /// same name stops the keeper it launched.
    #[test]
    fn a_name_that_never_lands_keeps_no_keeper() {
        let (dir, home) = home();
        // Leg 1: a keeper that never answers Identify.
        let killed: Mutex<Vec<u32>> = Mutex::new(Vec::new());
        let killed_ref = &killed;
        let seams = Seams {
            mint: &|_h, _cwd, _req| Ok(SID.to_string()),
            probe: &|_s| KeeperProbe::NoListener,
            launch: &|_spec| Ok(4242),
            kill: &|pid: u32| killed_ref.lock().unwrap().push(pid),
            worker_bin: Path::new(fake_worker_bin()),
        };
        let params = json!({"substrate": "thread"});
        let outcome = dispatch_with(
            &home,
            "pi",
            WK,
            "",
            "tester",
            dir.path(),
            &[],
            &params,
            &seams,
        );
        assert_eq!(outcome.exit_code, 1, "{}", outcome.stderr);
        assert!(killed_ref.lock().unwrap().contains(&4242));
        assert!(read_row(&home, WK).is_none());

        // Leg 2: an existing name refuses before the keeper launches. Write
        // through update_registry: the store owns the registry (a fence
        // directory stands at registry.json once the table exists), so a raw
        // file write hits EISDIR.
        let existing = RegistryEntry {
            name: WK.to_string(),
            harness: Some("pi".into()),
            harness_session_id: Some(SID.to_string()),
            cwd: "/tmp".into(),
            created_at: "2026-10-20T00:00:00Z".into(),
            ..Default::default()
        };
        crate::state::update_registry(&home.registry_json(), |reg| {
            reg.entries.push(existing);
        })
        .unwrap();
        let outcome = dispatch_with(
            &home,
            "pi",
            WK,
            "",
            "tester",
            dir.path(),
            &[],
            &params,
            &seams,
        );
        assert_eq!(outcome.exit_code, 2);
        assert!(outcome.stderr.contains("already exists"));

        // Leg 3: a concurrent registry write of the same name stops the
        // keeper it launched; no keeper runs that no row tracks.
        let seams = Seams {
            mint: &|_h, _cwd, _req| Ok(SID.to_string()),
            probe: &|_s| {
                KeeperProbe::Answered(serde_json::json!({
                    "keeper_pid": 4242,
                    "child_pid": 4243,
                    "argv": ["pi", "--session-id", SID],
                }))
            },
            launch: &|_spec| {
                let foreign = RegistryEntry {
                    name: WK.to_string(),
                    harness: Some("pi".into()),
                    harness_session_id: Some(SID.to_string()),
                    cwd: "/tmp".into(),
                    created_at: "2026-10-20T00:00:00Z".into(),
                    ..Default::default()
                };
                crate::state::update_registry(&home.registry_json(), |reg| {
                    reg.entries.push(foreign.clone());
                })
                .unwrap();
                Ok(4242)
            },
            kill: &|pid: u32| killed_ref.lock().unwrap().push(pid),
            worker_bin: Path::new(fake_worker_bin()),
        };
        let outcome = dispatch_with(
            &home,
            "pi",
            WK,
            "",
            "tester",
            dir.path(),
            &[],
            &params,
            &seams,
        );
        assert_eq!(outcome.exit_code, 2);
        assert!(outcome.stderr.contains("already exists"));
        assert!(killed_ref.lock().unwrap().contains(&4242));
        let reg = crate::state::load_registry(&home.registry_json()).unwrap();
        assert_eq!(reg.entries.len(), 1);
    }

    /// The session-start hook can register the minted session under its short
    /// id before the spawn writes its row. The spawn takes that row over: the
    /// name and lineage land on it, no second row appears, and exit is 0.
    #[test]
    fn a_spawn_takes_over_the_row_the_session_hook_registered_first() {
        let (dir, home) = home();
        let seams = Seams {
            mint: &|_h, _cwd, _req| Ok(SID.to_string()),
            probe: &|_s| {
                KeeperProbe::Answered(serde_json::json!({
                    "keeper_pid": 4242,
                    "child_pid": 4243,
                    "argv": ["pi", "--session-id", SID],
                }))
            },
            launch: &|_spec| {
                let hook_row = RegistryEntry {
                    name: SID[..8].to_string(),
                    short_id: SID[..8].to_string(),
                    harness: Some("pi".into()),
                    harness_session_id: Some(SID.to_string()),
                    model: Some("hook-model".into()),
                    cwd: "/tmp".into(),
                    created_at: "2026-10-20T00:00:00Z".into(),
                    ..Default::default()
                };
                crate::state::update_registry(&home.registry_json(), |reg| {
                    reg.entries.push(hook_row.clone());
                })
                .unwrap();
                Ok(4242)
            },
            kill: &|_pid| {},
            worker_bin: Path::new(fake_worker_bin()),
        };
        let params = json!({"substrate": "thread"});
        let outcome = dispatch_with(
            &home,
            "pi",
            WK,
            "",
            "tester",
            dir.path(),
            &[],
            &params,
            &seams,
        );
        assert_eq!(outcome.exit_code, 0, "stderr: {}", outcome.stderr);
        let reg = crate::state::load_registry(&home.registry_json()).unwrap();
        assert_eq!(reg.entries.len(), 1);
        let row = read_row(&home, WK).expect("the hook row now carries the spawn name");
        assert_eq!(row.short_id, SID[..8]);
        assert_eq!(row.origin.as_deref(), Some("spawn"));
        assert_eq!(row.model.as_deref(), Some("hook-model"));
        assert_eq!(row.created_at, "2026-10-20T00:00:00Z");
        assert_eq!(row.pid, Some(4242));
    }

    /// Headless and once refuse with the row's own sentence before anything
    /// launches.
    #[test]
    fn headless_and_once_refuse_with_the_rows_own_line() {
        let (dir, home) = home();
        let seams = Seams {
            mint: &|_h, _cwd, _req| Ok(SID.to_string()),
            probe: &|_s| KeeperProbe::NoListener,
            launch: &no_launch(),
            kill: &|_pid| {},
            worker_bin: Path::new(fake_worker_bin()),
        };
        let params = json!({"substrate": "headless"});
        let outcome = dispatch_with(
            &home,
            "pi",
            WK,
            "",
            "tester",
            dir.path(),
            &[],
            &params,
            &seams,
        );
        assert_eq!(outcome.exit_code, 2);
        // pi carries no headless sentence: the once line names its unmeasured
        // headless stance, so the fallback lands there.
        assert_eq!(
            outcome.stderr.trim(),
            keeper_arm("pi").unwrap().once_refusal
        );
    }

    /// A keeper socket that speaks the frame protocol enough for the arm:
    /// Identify gets the reply that proves the launch, a seed connection
    /// gets the composer paint, pastes are recorded and echoed back.
    struct FakeKeeper {
        path: PathBuf,
        ready: Vec<u8>,
        pasted: Mutex<Vec<u8>>,
    }

    impl FakeKeeper {
        fn new(path: &Path, ready: Vec<u8>) -> Self {
            Self {
                path: path.to_path_buf(),
                ready,
                pasted: Mutex::new(Vec::new()),
            }
        }

        fn pasted(&self) -> Vec<u8> {
            self.pasted.lock().unwrap().clone()
        }

        fn serve(&self) {
            use crate::pane_keeper::{decode, encode, Decode, Frame};
            use std::io::{Read, Write};
            let listener = std::os::unix::net::UnixListener::bind(&self.path).unwrap();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let mut buf: Vec<u8> = Vec::new();
                let mut saw_identify = false;
                let mut painted = false;
                loop {
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Err(_) => {
                            // A quiet seed connection still gets its paint.
                            if !saw_identify && !painted {
                                let _ =
                                    stream.write_all(&encode(&Frame::Output(self.ready.clone())));
                                painted = true;
                            }
                            continue;
                        }
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    loop {
                        match decode(&buf) {
                            Decode::NeedMore => break,
                            Decode::Violation(_) => return,
                            Decode::Frame(Frame::Identify, used) => {
                                buf.drain(..used);
                                saw_identify = true;
                                let reply = serde_json::json!({
                                    "keeper_pid": 4242,
                                    "child_pid": 4243,
                                    "argv": ["pi", "--session-id", SID],
                                });
                                let payload = serde_json::to_vec(&reply).unwrap();
                                let _ = stream.write_all(&encode(&Frame::IdentifyReply(payload)));
                            }
                            Decode::Frame(Frame::Input(payload), used) => {
                                buf.drain(..used);
                                self.pasted.lock().unwrap().extend(payload);
                            }
                            Decode::Frame(Frame::Resize(..), used) => {
                                buf.drain(..used);
                                if !painted {
                                    let _ = stream
                                        .write_all(&encode(&Frame::Output(self.ready.clone())));
                                    painted = true;
                                }
                            }
                            Decode::Frame(_, used) => {
                                buf.drain(..used);
                            }
                        }
                    }
                    let pasted = self.pasted.lock().unwrap().clone();
                    if pasted.ends_with(b"\r") {
                        // The submitted line repaints: the echo proof.
                        let _ = stream.write_all(&encode(&Frame::Output(pasted)));
                        break;
                    }
                }
            }
        }
    }
}

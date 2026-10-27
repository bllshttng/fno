//! The `fno whoami` family, native under `spawn-overlay whoami`: the
//! operating-stack summary and the `status` session read. The Python leg
//! (`cli/src/fno/agent/`) is deleted in the same change that lands this;
//! the goldens under `tests/golden/whoami/` freeze its bytes.
//!
//! Three layers load independently: fleet (glob of `<state>/fleet/*/00-INDEX.md`
//! naming this project), walker (`.fno/megawalk-state.md`), session
//! (`.fno/target-state.md`, else `.fno/session-state.md`). The state-file
//! dialect is YAML's block-mapping subset that manifests and mission files
//! actually carry: scalars, quoted strings, bools, ints, nulls, nested
//! mappings, block lists. Anything else is malformed and the command exits 2,
//! the same contract the Python leg had (the parser detail after the path is
//! our wording, folded in the goldens, never frozen).
//!
//! The identity enrichments (mail handle, model, context, role, agent name)
//! read the same resolvers the rest of the crate reads. The mail-unread count
//! reads the bus log through its documented store layout - segments, cursor
//! files, withdrawal tombstones, hosted/typed receipts - and degrades to 0 on
//! anything unreadable, exactly as the Python count did.

use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const GROUP_HELP: &str = "Usage: fno whoami [OPTIONS] COMMAND [ARGS]...

  Operating-stack summary: project + fleet + walker + session + harness.

Options:
  -J, --json         emit JSON to stdout
  --state-file PATH  override session state-file detection
  --no-walker        suppress walker layer in output
  --no-fleet         suppress fleet layer in output
  --help             Show this message and exit.

Commands:
  status  Show gate satisfaction, bounded events tail, inconsistencies.
";

const STATUS_HELP: &str = "Usage: fno whoami status [OPTIONS]

  Show gate satisfaction, bounded events tail, inconsistencies.

Options:
  -J, --json         emit JSON to stdout
  --state-file PATH  override session state-file detection
  --no-walker        suppress walker layer in output
  --no-fleet         suppress fleet layer in output
  --help             Show this message and exit.
";

/// The leaves still owned by other families' Python legs (`fno whoami
/// context|ledger|scoreboard|cost` forward to them). The front door routes
/// them there; a direct native call says so rather than lying with a
/// no-such-command error.
const PYTHON_LEAVES: [&str; 4] = ["context", "ledger", "scoreboard", "cost"];

pub fn run(args: &[String]) -> i32 {
    let mut opts = Options::default();
    let mut leaf: Option<String> = None;
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        match (leaf.as_deref(), arg) {
            (None, "status") => leaf = Some("status".into()),
            (None, other) if PYTHON_LEAVES.contains(&other) => {
                eprintln!("whoami {other} runs on the Python leg still; call `fno whoami {other}`");
                return 2;
            }
            (None, "--help") => {
                print!("{GROUP_HELP}");
                return 0;
            }
            (Some("status"), "--help") => {
                print!("{STATUS_HELP}");
                return 0;
            }
            (_, "--json") | (_, "-J") => opts.json_output = true,
            (_, "--no-walker") => opts.no_walker = true,
            (_, "--no-fleet") => opts.no_fleet = true,
            (_, "--state-file") => {
                i += 1;
                match args.get(i) {
                    Some(v) => opts.state_file = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("--state-file requires a path");
                        return 2;
                    }
                }
            }
            (None, other) if other.starts_with('-') => {
                eprintln!("Usage: fno whoami [OPTIONS] COMMAND [ARGS]...");
                eprintln!("Try 'fno whoami --help' for help.");
                eprintln!();
                eprintln!("Error: No such option: {other}");
                return 2;
            }
            (Some(l), other) if other.starts_with('-') => {
                eprintln!("Usage: fno whoami {l} [OPTIONS]");
                eprintln!("Try 'fno whoami {l} --help' for help.");
                eprintln!();
                eprintln!("Error: No such option: {other}");
                return 2;
            }
            (None, other) => {
                eprintln!("Usage: fno whoami [OPTIONS] COMMAND [ARGS]...");
                eprintln!("Try 'fno whoami --help' for help.");
                eprintln!();
                eprintln!("Error: No such command '{other}'.");
                return 2;
            }
            (Some(l), other) => {
                eprintln!("Usage: fno whoami {l} [OPTIONS]");
                eprintln!("Try 'fno whoami {l} --help' for help.");
                eprintln!();
                eprintln!("Error: Got unexpected extra argument ({other}).");
                return 2;
            }
        }
        i += 1;
    }
    let home = AgentsHome::from_env();
    match leaf.as_deref() {
        None => whoami_render(&opts, &home),
        Some("status") => status_render(&opts, &home),
        _ => unreachable!("only status is a native leaf"),
    }
}

#[derive(Default)]
struct Options {
    json_output: bool,
    state_file: Option<PathBuf>,
    no_walker: bool,
    no_fleet: bool,
}

/// The layer suppressions: a suppressed layer renders as absent, the same
/// replacement the Python leg applied before rendering.
fn drop_layers(mut state: AgentContext, opts: &Options) -> AgentContext {
    if opts.no_walker {
        state.walker = None;
    }
    if opts.no_fleet {
        state.fleet = None;
    }
    state
}

// -------------------------------------------------------------------------
// State layer
// -------------------------------------------------------------------------

struct FleetState {
    mission_id: String,
    title: Option<String>,
    status: String,
    wave_current: Option<i64>,
    wave_total: Option<i64>,
    path: PathBuf,
}

struct WalkerState {
    session_id: Option<String>,
    phase: Option<String>,
    in_flight: i64,
    done: i64,
    path: PathBuf,
}

struct SessionState {
    session_id: Option<String>,
    phase: Option<String>,
    status: Option<String>,
    pr_number: Option<i64>,
    raw: Map<String, Value>,
    path: PathBuf,
    kind: &'static str,
}

struct AgentContext {
    project_root: PathBuf,
    harness: String,
    fleet: Option<FleetState>,
    walker: Option<WalkerState>,
    session: Option<SessionState>,
    detected_paths: Vec<PathBuf>,
    warnings: Vec<String>,
    visitor: Option<String>,
}

enum StateError {
    Malformed { path: PathBuf, detail: String },
    MissingOverride { path: PathBuf },
}

fn load_agent_context(
    state_file_override: Option<&Path>,
    home: &AgentsHome,
) -> Result<AgentContext, StateError> {
    let mut warnings: Vec<String> = Vec::new();
    let mut detected: Vec<PathBuf> = Vec::new();
    let project_root = detect_project_root(&mut warnings);
    let harness = detect_harness(home);
    let fleet = load_fleet_state(&project_root, &mut warnings, &mut detected);
    let walker = load_walker_state(&project_root, &mut warnings, &mut detected);
    let mut session = load_session_state(
        &project_root,
        state_file_override,
        &mut warnings,
        &mut detected,
    )?;
    // A manifest naming another harness session makes this caller a visitor:
    // the run line drops and one visitor line explains why.
    let mut visitor: Option<String> = None;
    if let Some(sess) = &session {
        if sess.kind == "target" {
            let manifest_sid = sess
                .raw
                .get("harness_session_id")
                .or_else(|| sess.raw.get("claude_session_id"))
                .and_then(str_or_none);
            let caller_sid = harness_identity_session();
            if let (Some(m), Some(c)) = (manifest_sid, caller_sid) {
                if m != c {
                    visitor = Some(format!("manifest names session {m}, not this one"));
                    session = None;
                }
            }
        }
    }
    Ok(AgentContext {
        project_root,
        harness,
        fleet,
        walker,
        session,
        detected_paths: detected,
        warnings,
        visitor,
    })
}

/// `git rev-parse --show-toplevel`, else cwd with a warning. A 2s bound, the
/// same one the Python leg paid, so a wedged git cannot hang orientation.
fn detect_project_root(warnings: &mut Vec<String>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "--show-toplevel"]).current_dir(&cwd);
    if let Some(out) = crate::bounded_cmd::output_with_timeout(cmd, 2) {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !text.is_empty() {
                return PathBuf::from(text);
            }
        }
    }
    warnings.push("not a git repo, using cwd".into());
    cwd
}

/// The owned identity first, then the process-tree walk, else `claude`.
fn detect_harness(home: &AgentsHome) -> String {
    let get = |name: &str| std::env::var(name).ok();
    let ident = resolve_self_identity(&get, None, None, home);
    if let Some(h) = ident.harness {
        return h;
    }
    if let Some(walked) = crate::spawn_context::resolve_session_harness_ambient() {
        return walked.to_string();
    }
    "claude".to_string()
}

/// The caller's harness session id from env markers alone (precedence
/// order), the read the visitor check makes.
fn harness_identity_session() -> Option<String> {
    const MARKERS: [&str; 6] = [
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
    ];
    MARKERS.iter().find_map(|m| {
        std::env::var(m)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

fn read_with_retry(path: &Path) -> Result<String, StateError> {
    let mut last: Option<String> = None;
    for attempt in 0..2 {
        match std::fs::read_to_string(path) {
            Ok(text) => return Ok(text),
            Err(e) => {
                last = Some(e.to_string());
                if attempt == 0 {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    Err(StateError::Malformed {
        path: path.to_path_buf(),
        detail: last.unwrap_or_default(),
    })
}

fn parse_state_file(path: &Path) -> Result<Map<String, Value>, StateError> {
    // One retry after 50ms: a mid-write read is the failure this guards, and
    // the retry is the whole remedy. A second failure is malformed, rc=2.
    let mut last: Option<String> = None;
    for attempt in 0..2 {
        match read_with_retry(path) {
            Ok(text) => match extract_frontmatter(&text) {
                Ok(fields) => return Ok(fields),
                Err(detail) => last = Some(detail),
            },
            Err(StateError::Malformed { detail, .. }) => last = Some(detail),
            Err(other) => return Err(other),
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Err(StateError::Malformed {
        path: path.to_path_buf(),
        detail: last.unwrap_or_default(),
    })
}

/// Split the YAML frontmatter block and parse it; an absent block parses as
/// empty. Unterminated frontmatter is the caller's malformed state, and so
/// is a block our dialect cannot read.
fn extract_frontmatter(text: &str) -> Result<Map<String, Value>, String> {
    if !text.starts_with("---") {
        return Ok(Map::new());
    }
    let rest = text[3..].trim_start_matches('\n');
    let end = rest
        .find("\n---")
        .ok_or_else(|| "unterminated frontmatter (no closing ---)".to_string())?;
    let block = &rest[..end];
    if block.trim().is_empty() {
        return Ok(Map::new());
    }
    let parsed = parse_state_yaml(block)?;
    Ok(parsed)
}

/// The state-file dialect: YAML's block mappings with scalars, quoted
/// strings, bools, ints, nulls, nested mappings and block lists. A plain
/// scalar may not carry ": " (a nested mapping value), and an indented line
/// under a scalar key is malformed, the two shapes the Python YAML parser
/// refused on the same files.
fn parse_state_yaml(block: &str) -> Result<Map<String, Value>, String> {
    let lines: Vec<&str> = block
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .collect();
    let (value, used) = parse_mapping(&lines, 0, 0)?;
    if used != lines.len() {
        return Err(format!("unparsed content after line {used}"));
    }
    Ok(value)
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

fn parse_mapping(
    lines: &[&str],
    start: usize,
    indent: usize,
) -> Result<(Map<String, Value>, usize), String> {
    let mut map = Map::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        let ind = indent_of(line);
        if ind < indent {
            break;
        }
        if ind > indent {
            return Err(format!("unexpected indent at '{}'", line.trim()));
        }
        let trimmed = line.trim();
        if let Some(item) = trimmed.strip_prefix("- ") {
            return Err(format!(
                "block list item '{item}' where a mapping key was expected"
            ));
        }
        let Some((key_raw, val_raw)) = trimmed.split_once(':') else {
            return Err(format!("cannot parse '{trimmed}'"));
        };
        let key = key_raw.trim().to_string();
        let val = val_raw.trim();
        if val.contains(": ") {
            return Err(format!("mapping values are not allowed here ('{trimmed}')"));
        }
        if val.is_empty() {
            // Bare key: a nested block (deeper indent), else null.
            if i + 1 < lines.len() && indent_of(lines[i + 1]) > indent {
                let (child, used) = parse_mapping(lines, i + 1, indent_of(lines[i + 1]))?;
                map.insert(key, Value::Object(child));
                i = used;
                continue;
            }
            map.insert(key, Value::Null);
            i += 1;
            continue;
        }
        map.insert(key, parse_scalar(val)?);
        i += 1;
    }
    Ok((map, i))
}

fn parse_scalar(raw: &str) -> Result<Value, String> {
    let unquoted = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"'));
    let unquoted = match unquoted {
        Some(s) => return Ok(Value::String(s.to_string())),
        None => raw
            .strip_prefix('\'')
            .and_then(|r| r.strip_suffix('\''))
            .map(|s| Value::String(s.to_string())),
    };
    if let Some(v) = unquoted {
        return Ok(v);
    }
    match raw {
        "true" | "True" => return Ok(Value::Bool(true)),
        "false" | "False" => return Ok(Value::Bool(false)),
        "null" | "Null" | "~" => return Ok(Value::Null),
        _ => {}
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Ok(Value::Number(n.into()));
    }
    Ok(Value::String(raw.to_string()))
}

fn str_or_none(value: &Value) -> Option<String> {
    let s = match value {
        Value::String(s) => s.trim(),
        Value::Number(n) => {
            return Some(n.to_string());
        }
        Value::Bool(b) => {
            return Some(b.to_string());
        }
        _ => return None,
    };
    let lowered = s.to_ascii_lowercase();
    if s.is_empty() || lowered == "null" || lowered == "none" {
        None
    } else {
        Some(s.to_string())
    }
}

fn int_or_none(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FNO_STATE_DIR") {
        return PathBuf::from(dir);
    }
    // The fno state root, not the agents root: HOME/.fno.
    let base = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(base).join(".fno")
}

fn load_fleet_state(
    project_root: &Path,
    warnings: &mut Vec<String>,
    detected: &mut Vec<PathBuf>,
) -> Option<FleetState> {
    let fleet_root = state_dir().join("fleet");
    if !fleet_root.exists() {
        return None;
    }
    let abs_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut indexes: Vec<PathBuf> = std::fs::read_dir(&fleet_root)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .map(|d| d.join("00-INDEX.md"))
        .filter(|p| p.exists())
        .collect();
    indexes.sort();
    for index_file in indexes {
        let fields = match parse_state_file(&index_file) {
            Ok(f) => f,
            Err(StateError::Malformed { path, .. }) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                if let Some(name) = name {
                    warnings.push(format!("fleet: malformed {name}, skipping"));
                }
                continue;
            }
            Err(_) => continue,
        };
        let status = fields
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if status != "running" && status != "paused" {
            continue;
        }
        let projects = match fields.get("projects") {
            Some(Value::Object(map)) => map.clone(),
            _ => continue,
        };
        let mut matched: Option<String> = None;
        for proj in projects.values() {
            let path_str = match proj {
                Value::Object(m) => m
                    .get("cwd")
                    .or_else(|| m.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                Value::String(s) => s.clone(),
                _ => continue,
            };
            if path_str.is_empty() {
                continue;
            }
            let expanded = expand_tilde(&path_str);
            if let Ok(resolved) = Path::new(&expanded).canonicalize() {
                if resolved == abs_root {
                    matched = Some(path_str);
                    break;
                }
            }
        }
        if let Some(_) = matched {
            detected.push(index_file.clone());
            let mission_id = fields
                .get("mission_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    index_file
                        .parent()
                        .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .flatten()
                        .unwrap_or_default()
                });
            return Some(FleetState {
                mission_id,
                title: fields
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                status: if status == "paused" {
                    "paused".into()
                } else {
                    "running".into()
                },
                wave_current: fields.get("wave_current").and_then(int_or_none),
                wave_total: fields.get("wave_total").and_then(int_or_none),
                path: index_file,
            });
        }
    }
    None
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    path.to_string()
}

fn load_walker_state(
    project_root: &Path,
    warnings: &mut Vec<String>,
    detected: &mut Vec<PathBuf>,
) -> Option<WalkerState> {
    let path = project_root.join(".fno").join("megawalk-state.md");
    let ok = path.exists()
        && std::fs::metadata(&path)
            .map(|m| m.len() > 0)
            .unwrap_or(false);
    if !ok {
        return None;
    }
    let fields = match parse_state_file(&path) {
        Ok(f) => f,
        Err(StateError::Malformed { path, .. }) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
            if let Some(name) = name {
                warnings.push(format!("walker: malformed {name}"));
            }
            return None;
        }
        Err(_) => return None,
    };
    detected.push(path.clone());
    Some(WalkerState {
        session_id: fields.get("session_id").and_then(str_or_none),
        phase: fields
            .get("current_phase")
            .and_then(str_or_none)
            .or_else(|| fields.get("phase").and_then(str_or_none)),
        in_flight: fields.get("in_flight").and_then(int_or_none).unwrap_or(0),
        done: fields.get("done").and_then(int_or_none).unwrap_or(0),
        path,
    })
}

fn load_session_state(
    project_root: &Path,
    state_file_override: Option<&Path>,
    warnings: &mut Vec<String>,
    detected: &mut Vec<PathBuf>,
) -> Result<Option<SessionState>, StateError> {
    let (path, kind): (PathBuf, &str) = match state_file_override {
        Some(override_path) => {
            if !override_path.exists() {
                return Err(StateError::MissingOverride {
                    path: override_path.to_path_buf(),
                });
            }
            (override_path.to_path_buf(), "override")
        }
        None => {
            let target = project_root.join(".fno").join("target-state.md");
            let session = project_root.join(".fno").join("session-state.md");
            let size = |p: &Path| std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false);
            let target_present = target.exists() && size(&target);
            let session_present = session.exists() && size(&session);
            if target_present && session_present {
                warnings.push(
                    "both .fno/target-state.md and .fno/session-state.md present; using target"
                        .into(),
                );
                (target, "target")
            } else if target_present {
                (target, "target")
            } else if session_present {
                (session, "session")
            } else {
                return Ok(None);
            }
        }
    };
    let raw = parse_state_file(&path)?;
    detected.push(path.clone());
    let phase = raw
        .get("current_phase")
        .and_then(str_or_none)
        .or_else(|| raw.get("phase").and_then(str_or_none));
    Ok(Some(SessionState {
        session_id: raw.get("session_id").and_then(str_or_none),
        phase,
        status: raw.get("status").and_then(str_or_none),
        pr_number: raw.get("pr_number").and_then(int_or_none),
        raw,
        path,
        kind,
    }))
}

// -------------------------------------------------------------------------
// The bare summary render
// -------------------------------------------------------------------------

fn whoami_render(opts: &Options, home: &AgentsHome) -> i32 {
    let state = match load_agent_context(opts.state_file.as_deref(), home) {
        Ok(state) => state,
        Err(StateError::Malformed { path, detail }) => {
            eprintln!("error: malformed state file: {}: {detail}", path.display());
            return 2;
        }
        Err(StateError::MissingOverride { path }) => {
            eprintln!(
                "error: --state-file path does not exist: {}",
                path.display()
            );
            return 2;
        }
    };
    let state = drop_layers(state, opts);
    let get = |name: &str| std::env::var(name).ok();
    let ident = resolve_self_identity(&get, None, None, home);
    // The reply handle. An ambiguous environment is the one refusal the
    // command carries: guessing hands the operator's mail to a stranger.
    if matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!("error: {}", identity_ambiguity_message(&ident));
        return 4;
    }
    let mail = match (&ident.session_id, &ident.harness) {
        (Some(sid), Some(_)) => Some(crate::identity::canonical_handle(sid)),
        _ => None,
    };
    let agent_self = std::env::var("FNO_AGENT_SELF")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    // The registry-name pointer is the non-JSON render's enrichment; the
    // JSON payload has no agent key to fill, so a --json call skips the
    // registry read.
    let mut agent_registry_name: Option<String> = None;
    if agent_self.is_none() && !opts.json_output {
        if let (Some(sid), Some(harness)) = (&ident.session_id, &ident.harness) {
            agent_registry_name = find_by_session(&registry_rows(home), sid, harness).map(|row| {
                row.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            });
        }
    }
    let mail_unread = mail_unread_count(
        mail.as_deref(),
        agent_self.as_deref(),
        ident.session_id.as_deref(),
        &state.project_root,
    );
    let context_reading = context_reading(&ident);
    let role = role_text(&ident, home);
    let foreign_identity = foreign_identity_line(&ident);
    if opts.json_output {
        let mut payload = ctx_to_jsonable(&state);
        payload.insert("mail_handle".into(), mail.clone().into());
        payload.insert("harness_session_id".into(), ident.session_id.clone().into());
        let worker_name = task_holder(&ident, home, &state.project_root);
        if let Some(name) = worker_name {
            payload.insert("worker_name".into(), Value::String(name));
        }
        if let Some(line) = &foreign_identity {
            payload.insert("inherited_identity_env".into(), Value::String(line.clone()));
        }
        payload.insert(
            "model".into(),
            match session_model(&ident) {
                Some(m) => Value::String(m),
                None => Value::Null,
            },
        );
        let provider = std::env::var("FNO_ROUTE_PROVIDER")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        if let Some(p) = &provider {
            payload.insert("route_provider".into(), Value::String(p.clone()));
            payload.insert(
                "subagent_budget".into(),
                match crate::spawn_gate_lanes::provider_subagents_cap(&state.project_root, p) {
                    Some(n) => Value::Number((n as i64).into()),
                    None => Value::Null,
                },
            );
        }
        if let Some(text) = &role {
            payload.insert("role".into(), Value::String(text.clone()));
        }
        if let Some(reading) = &context_reading {
            payload.insert("context_used_pct".into(), json!(reading.2));
            payload.insert("context_used_tokens".into(), json!(reading.0));
            payload.insert("context_window_tokens".into(), json!(reading.1));
        }
        if mail_unread > 0 {
            payload.insert("mail_unread".into(), json!(mail_unread));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&sorted(Value::Object(payload)))
                .expect("payload serializes")
        );
        emit_warnings(&state.warnings);
        return 0;
    }
    println!("project:  {}", state.project_root.display());
    if let Some(v) = &state.visitor {
        println!("visitor: {v}");
    }
    if let Some(fleet) = &state.fleet {
        let wave = match (fleet.wave_current, fleet.wave_total) {
            (Some(c), Some(t)) => format!(" - wave {c}/{t}"),
            _ => String::new(),
        };
        let title = fleet
            .title
            .as_ref()
            .map(|t| format!(" ({t})"))
            .unwrap_or_default();
        println!("fleet:    {}{title}{wave}", fleet.mission_id);
    }
    if let Some(walker) = &state.walker {
        let phase = walker
            .phase
            .as_ref()
            .map(|p| format!(" phase={p}"))
            .unwrap_or_default();
        println!(
            "walker:   {}{phase} - {} in-flight, {} done",
            walker.session_id.as_deref().unwrap_or("(unknown)"),
            walker.in_flight,
            walker.done
        );
    }
    if let Some(session) = &state.session {
        let phase = if let Some(p) = &session.phase {
            format!(" phase={p}")
        } else if !session.raw.contains_key("current_phase") && !session.raw.contains_key("phase") {
            " phase=n/a (collapsed)".to_string()
        } else {
            String::new()
        };
        let status = session
            .status
            .as_ref()
            .map(|s| format!(" status={s}"))
            .unwrap_or_default();
        println!(
            "run:      {} ({}){phase}{status} (ledger/run id - not a mail handle)",
            session.session_id.as_deref().unwrap_or("(unknown)"),
            session.kind
        );
    }
    if let Some(handle) = &mail {
        println!("mail:     {handle}  (reply handle - pass as --from-name, or omit to self-stamp)");
    }
    if mail_unread > 0 {
        println!("mail_unread: {mail_unread}");
    }
    println!("harness:  {}", state.harness);
    if let Some(line) = &foreign_identity {
        println!("{line}");
    }
    if let Some(model) = session_model(&ident) {
        println!("model:    {model}");
    }
    let provider = std::env::var("FNO_ROUTE_PROVIDER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if let Some(p) = &provider {
        let budget = crate::spawn_gate_lanes::provider_subagents_cap(&state.project_root, p);
        match budget {
            None => println!("provider: {p} (route-bound; no subagent budget)"),
            Some(b) => {
                let plural = if b == 1 { "" } else { "s" };
                let note = if b < 2 {
                    " - reviews resolve to one inline reviewer"
                } else {
                    ""
                };
                println!("provider: {p} (route-bound; subagent budget {b} agent{plural}{note})");
            }
        }
    }
    if let Some((used, window, pct)) = &context_reading {
        println!(
            "context:  {pct}% used ({used} of {window} tokens)",
            used = with_commas(*used),
            window = with_commas(*window)
        );
    }
    if let Some(self_name) = &agent_self {
        println!("agent:    {self_name} (mesh)");
    } else if let Some(name) = &agent_registry_name {
        if !name.is_empty() {
            println!("agent:    {name} (registry)");
        }
    }
    if let Some(text) = &role {
        println!("role:    {text}");
    }
    emit_warnings(&state.warnings);
    0
}

// -------------------------------------------------------------------------
// The status render
// -------------------------------------------------------------------------

const EVENTS_TAIL_BYTES: u64 = 65536;
const EVENTS_TAIL_LINES: usize = 10;

fn status_render(opts: &Options, home: &AgentsHome) -> i32 {
    let state = match load_agent_context(opts.state_file.as_deref(), home) {
        Ok(state) => state,
        Err(StateError::Malformed { path, detail }) => {
            eprintln!("error: malformed state file: {}: {detail}", path.display());
            return 2;
        }
        Err(StateError::MissingOverride { path }) => {
            eprintln!(
                "error: --state-file path does not exist: {}",
                path.display()
            );
            return 2;
        }
    };
    let state = drop_layers(state, opts);
    let events_path = state.project_root.join(".fno").join("events.jsonl");
    let tail = tail_events(&events_path, EVENTS_TAIL_BYTES, EVENTS_TAIL_LINES);
    let events_warning: Option<String> = tail
        .failed
        .then(|| format!("events.jsonl unreadable: {}", tail.error));
    let inconsistencies = state
        .session
        .as_ref()
        .map(|s| detect_inconsistencies(&s.raw))
        .unwrap_or_default();
    if opts.json_output {
        let mut payload = ctx_to_jsonable(&state);
        payload.insert("events_tail".into(), Value::Array(tail.events.clone()));
        payload.insert(
            "inconsistencies".into(),
            Value::Array(
                inconsistencies
                    .iter()
                    .map(|l| Value::String(l.clone()))
                    .collect(),
            ),
        );
        if let Some(w) = &events_warning {
            payload.insert("events_warning".into(), Value::String(w.clone()));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&sorted(Value::Object(payload)))
                .expect("payload serializes")
        );
        emit_warnings(&state.warnings);
        if let Some(w) = &events_warning {
            eprintln!("warn: {w}");
        }
        return 0;
    }
    println!("project:  {}", state.project_root.display());
    if let Some(session) = &state.session {
        let raw_has_phase =
            session.raw.contains_key("current_phase") || session.raw.contains_key("phase");
        let phase_str = if raw_has_phase {
            session.phase.clone().unwrap_or_else(|| "(unknown)".into())
        } else {
            "n/a (collapsed)".to_string()
        };
        let status_str = if session.raw.contains_key("status") {
            session.status.clone().unwrap_or_else(|| "?".into())
        } else {
            derive_status_from_events(
                &state.project_root,
                session
                    .raw
                    .get("session_id")
                    .and_then(str_or_none)
                    .as_deref(),
            )
        };
        println!(
            "session:  {} phase={phase_str} status={status_str}",
            session.session_id.as_deref().unwrap_or("(unknown)")
        );
        let gate_keys = gate_keys(&session.raw);
        if !gate_keys.is_empty() {
            println!("gates:");
            for key in gate_keys {
                let value = &session.raw[&key];
                let mark = match value {
                    Value::Bool(true) => "y",
                    Value::Bool(false) => "n",
                    _ => "?",
                };
                println!("  [{mark}] {key}: {}", py_repr(value));
            }
        }
    }
    if let Some(walker) = &state.walker {
        println!(
            "walker:   {} in_flight={} done={}",
            walker.session_id.as_deref().unwrap_or("(unknown)"),
            walker.in_flight,
            walker.done
        );
    }
    if let Some(fleet) = &state.fleet {
        println!(
            "fleet:    {} status={} wave={}/{}",
            fleet.mission_id,
            fleet.status,
            fleet
                .wave_current
                .map(|v| v.to_string())
                .unwrap_or_else(|| "None".into()),
            fleet
                .wave_total
                .map(|v| v.to_string())
                .unwrap_or_else(|| "None".into()),
        );
    }
    if !tail.events.is_empty() {
        println!("events (last {}):", tail.events.len());
        for ev in &tail.events {
            let kind = ev
                .get("type")
                .or_else(|| ev.get("kind"))
                .or_else(|| ev.get("event"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            let ts = ev
                .get("ts")
                .or_else(|| ev.get("timestamp"))
                .and_then(Value::as_str)
                .unwrap_or("");
            println!("  - {ts} {kind}");
        }
    }
    if !inconsistencies.is_empty() {
        println!("inconsistencies:");
        for line in &inconsistencies {
            println!("  WARNING: {line}");
        }
    }
    if let Some(w) = &events_warning {
        eprintln!("warn: {w}");
    }
    emit_warnings(&state.warnings);
    0
}

struct Tail {
    events: Vec<Value>,
    failed: bool,
    error: String,
}

fn tail_events(path: &Path, max_bytes: u64, max_lines: usize) -> Tail {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        // The old leg stayed silent about an absent journal; only an
        // unreadable present file warns.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Tail {
                events: Vec::new(),
                failed: false,
                error: String::new(),
            }
        }
        Err(e) => {
            return Tail {
                events: Vec::new(),
                failed: true,
                error: e.to_string(),
            }
        }
    };
    let size = meta.len();
    if size == 0 {
        return Tail {
            events: Vec::new(),
            failed: false,
            error: String::new(),
        };
    }
    let data = match read_tail_bytes(path, size, max_bytes) {
        Ok(d) => d,
        Err(e) => {
            return Tail {
                events: Vec::new(),
                failed: true,
                error: e.to_string(),
            }
        }
    };
    let text = String::from_utf8_lossy(&data);
    let mut lines: Vec<&str> = text.split('\n').collect();
    // A start truncated mid-line drops the partial first line.
    if size > max_bytes && !lines.is_empty() {
        lines.remove(0);
    }
    let mut parsed: Vec<Value> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<Value>(line) {
            if obj.is_object() {
                parsed.push(obj);
            }
        }
    }
    let start = parsed.len().saturating_sub(max_lines);
    Tail {
        events: parsed.split_off(start),
        failed: false,
        error: String::new(),
    }
}

fn read_tail_bytes(path: &Path, size: u64, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(size.saturating_sub(max_bytes)))?;
    let mut buf = Vec::new();
    f.take(max_bytes).read_to_end(&mut buf)?;
    Ok(buf)
}

fn derive_status_from_events(project_root: &Path, session_id: Option<&str>) -> String {
    let events_path = project_root.join(".fno").join("events.jsonl");
    if !events_path.exists() {
        return "active".to_string();
    }
    let tail = tail_events(&events_path, 131072, 50);
    if tail.failed {
        return "active".to_string();
    }
    for ev in tail.events.iter().rev() {
        let ev_type = ev
            .get("type")
            .or_else(|| ev.get("kind"))
            .or_else(|| ev.get("event"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if ev_type != "termination" {
            continue;
        }
        let data = ev.get("data").cloned().unwrap_or(Value::Null);
        if let Some(sid) = session_id {
            let ev_sid = data.get("session_id").and_then(Value::as_str).unwrap_or("");
            if ev_sid != sid {
                continue;
            }
        }
        let reason = data
            .get("termination_reason")
            .or_else(|| data.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !reason.is_empty() {
            return format!("terminated ({reason})");
        }
        return "terminated".to_string();
    }
    "active".to_string()
}

fn gate_keys(raw: &Map<String, Value>) -> Vec<String> {
    const SUFFIXES: [&str; 5] = [
        "_passed",
        "_validated",
        "_shipped",
        "_generated",
        "_updated",
    ];
    let mut keys: Vec<String> = raw
        .keys()
        .filter(|k| SUFFIXES.iter().any(|s| k.ends_with(s)))
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn detect_inconsistencies(raw: &Map<String, Value>) -> Vec<String> {
    let mut findings: Vec<String> = Vec::new();
    let pr_number = raw.get("pr_number");
    let pr_absent = matches!(pr_number, None | Some(Value::Null))
        || pr_number.and_then(Value::as_str) == Some("null");
    let ext_false = matches!(raw.get("external_review_passed"), Some(Value::Bool(false)));
    if !pr_absent && ext_false {
        let shown = match pr_number {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => py_repr(other),
            None => String::new(),
        };
        findings.push(format!(
            "pr_number={shown} but external_review_passed: false -> /fno:ship pr check probably owed"
        ));
    }
    let shipped_true = matches!(raw.get("artifact_shipped"), Some(Value::Bool(true)));
    if shipped_true && pr_absent {
        findings.push(
            "artifact_shipped: true but pr_number is null -> /fno:ship pr create likely incomplete"
                .into(),
        );
    }
    findings
}

/// Python `str()` for the gate values the text render prints raw.
fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn ctx_to_jsonable(state: &AgentContext) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "project_root".into(),
        Value::String(state.project_root.to_string_lossy().into_owned()),
    );
    payload.insert("harness".into(), Value::String(state.harness.clone()));
    payload.insert(
        "fleet".into(),
        state
            .fleet
            .as_ref()
            .map(|f| {
                json!({
                    "mission_id": f.mission_id,
                    "title": f.title,
                    "status": f.status,
                    "wave_current": f.wave_current,
                    "wave_total": f.wave_total,
                    "path": f.path.to_string_lossy(),
                })
            })
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "walker".into(),
        state
            .walker
            .as_ref()
            .map(|w| {
                json!({
                    "session_id": w.session_id,
                    "phase": w.phase,
                    "in_flight": w.in_flight,
                    "done": w.done,
                    "path": w.path.to_string_lossy(),
                })
            })
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "session".into(),
        state
            .session
            .as_ref()
            .map(|s| {
                json!({
                    "session_id": s.session_id,
                    "phase": s.phase,
                    "status": s.status,
                    "pr_number": s.pr_number,
                    "raw": Value::Object(s.raw.clone()),
                    "path": s.path.to_string_lossy(),
                    "kind": s.kind,
                })
            })
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "detected_paths".into(),
        Value::Array(
            state
                .detected_paths
                .iter()
                .map(|p| Value::String(p.to_string_lossy().into_owned()))
                .collect(),
        ),
    );
    payload.insert(
        "warnings".into(),
        Value::Array(
            state
                .warnings
                .iter()
                .map(|w| Value::String(w.clone()))
                .collect(),
        ),
    );
    payload.insert("visitor".into(), state.visitor.clone().into());
    payload
}

/// Python `json.dumps(sort_keys=True)` order: every object level sorts, the
/// raw manifest block included. The crate's serde_json preserves insertion
/// order, so the sort is explicit.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, sorted(v)))
                    .collect::<Map<String, Value>>(),
            )
        }
        other => other,
    }
}

fn emit_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warn: {w}");
    }
}

fn with_commas(mut n: u64) -> String {
    let mut parts: Vec<String> = Vec::new();
    loop {
        if n < 1000 {
            parts.push(n.to_string());
            break;
        }
        parts.push(format!("{:03}", n % 1000));
        n /= 1000;
    }
    parts.reverse();
    parts.join(",")
}

// -------------------------------------------------------------------------
// Identity enrichments
// -------------------------------------------------------------------------

/// The one refusal sentence for an unproven mixed environment, byte-identical
/// to the Python message the drain-self refusal carries.
fn identity_ambiguity_message(ident: &crate::spawn_context::OwnedIdentity) -> String {
    let markers = ident
        .markers_present
        .iter()
        .map(|(m, _, _)| m.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let lookup_id = ident
        .markers_present
        .first()
        .map(|(_, _, v)| v.clone())
        .unwrap_or_else(|| "<session-id>".into());
    // The single-family ambiguous disposition has no foreign family to strip,
    // so the strip section only appears across families.
    let families: Vec<&str> = {
        let mut out: Vec<&str> = Vec::new();
        for (_, harness, _) in &ident.markers_present {
            if !out.contains(&harness.as_str()) {
                out.push(harness.as_str());
            }
        }
        out
    };
    let mut strip_lines = String::new();
    for family in &families {
        let flags = ambient_strip_flags(family);
        if !flags.is_empty() {
            strip_lines.push_str(&format!(
                "  if this is a {family} session: env {} <command>\n",
                flags.join(" ")
            ));
        }
    }
    let strip_section = if strip_lines.is_empty() {
        String::new()
    } else {
        format!(
            "or strip the foreign family's markers and retry:\n{strip_lines}keeping your own harness's markers; each harness re-mints its own for a child\n"
        )
    };
    format!(
        "cannot decide which session is 'self': multiple harness markers present (inherited env?)\nmarkers: {markers}\nresolve with: find ~/.codex/sessions ~/.claude/projects -name '*{lookup_id}*'\n{strip_section}"
    )
    .trim_end_matches('\n')
    .to_string()
}

/// The marker names a harness family reads, in the order the strip remedy
/// lists them. The fno plumbing names never strip: they are this process's
/// own plumbing, not foreign lineage.
fn ambient_strip_flags(keep_family: &str) -> Vec<String> {
    const TABLE: [(&str, &str); 15] = [
        ("CODEX_THREAD_ID", "codex"),
        ("CLAUDE_CODE_SESSION_ID", "claude"),
        ("CODEX_SESSION_ID", "codex"),
        ("GEMINI_SESSION_ID", "gemini"),
        ("OPENCODE_SESSION_ID", "opencode"),
        ("CLAUDE_SESSION_ID", "claude"),
        ("CLAUDECODE", "claude"),
        ("CLAUDECODE_SESSION_ID", "claude"),
        ("HERMES_SESSION_ID", "hermes"),
        ("TARGET_SESSION_ID", "fno"),
        ("CODEX_CI", "codex"),
        ("CODEX_INTERNAL_ORIGINATOR_OVERRIDE", "codex"),
        ("CODEX_SHELL", "codex"),
        ("CODEX_COMPANION_SESSION_ID", "codex"),
        ("CODEX_COMPANION_TRANSCRIPT_PATH", "codex"),
    ];
    let own_session = TABLE
        .iter()
        .find(|(_, family)| *family == keep_family)
        .and_then(|(name, _)| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        });
    let mut flags: Vec<String> = Vec::new();
    for (name, family) in TABLE {
        if family == keep_family || family == "fno" || family == "hermes" {
            continue;
        }
        let present = std::env::var(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let Some(value) = present else { continue };
        // A foreign name carrying our own session id names this session, not
        // foreign lineage.
        if Some(&value) == own_session.as_ref() {
            continue;
        }
        flags.push(format!("-u {name}"));
    }
    flags
}

/// One lineage line naming foreign identity env this session inherited, or
/// None when the env is clean (the normal case).
fn foreign_identity_line(ident: &crate::spawn_context::OwnedIdentity) -> Option<String> {
    let harness = ident.harness.as_deref()?;
    let flags = ambient_strip_flags(harness);
    if flags.is_empty() {
        return None;
    }
    let names: Vec<&str> = flags
        .iter()
        .filter_map(|f| f.split_once(' ').map(|x| x.1))
        .collect();
    let families: Vec<String> = {
        let mut seen: Vec<&str> = Vec::new();
        for name in &names {
            let family = family_of(name);
            if !seen.contains(&family) {
                seen.push(family);
            }
        }
        let mut out: Vec<String> = seen.into_iter().map(str::to_string).collect();
        out.sort();
        out
    };
    Some(format!(
        "identity: {} inherited {} name(s) in this {harness} session's env - lineage, not identity; clear them in the shell that started this session, or prefix a command with: env {}",
        names.len(),
        families.join("/"),
        flags.join(" ")
    ))
}

fn family_of(name: &str) -> &'static str {
    match name {
        "CODEX_THREAD_ID"
        | "CODEX_SESSION_ID"
        | "CODEX_CI"
        | "CODEX_INTERNAL_ORIGINATOR_OVERRIDE"
        | "CODEX_SHELL"
        | "CODEX_COMPANION_SESSION_ID"
        | "CODEX_COMPANION_TRANSCRIPT_PATH" => "codex",
        "CLAUDE_CODE_SESSION_ID" | "CLAUDE_SESSION_ID" | "CLAUDECODE" | "CLAUDECODE_SESSION_ID" => {
            "claude"
        }
        "GEMINI_SESSION_ID" => "gemini",
        "OPENCODE_SESSION_ID" => "opencode",
        "HERMES_SESSION_ID" => "hermes",
        "TARGET_SESSION_ID" => "fno",
        _ => "unknown",
    }
}

/// `(used_tokens, window_tokens, used_pct)` for this session's transcript,
/// or None. None omits the line: the summary is the recovery command and
/// never gains a failure mode.
fn context_reading(ident: &crate::spawn_context::OwnedIdentity) -> Option<(u64, u64, u64)> {
    let session = std::env::var("CODEX_THREAD_ID")
        .or_else(|_| std::env::var("FNO_HARNESS_SESSION_ID"))
        .unwrap_or_default();
    let sid = ident.session_id.as_deref()?;
    let harness = ident.harness.as_deref()?;
    let path = own_transcript(sid, harness)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let payload = crate::context_run::transcript_reading(&path, &session, &cwd).ok()?;
    let used = payload.get("used_tokens")?.as_u64()?;
    let window = payload.get("window_tokens")?.as_u64()?;
    let pct = payload.get("used_pct")?.as_u64()?;
    Some((used, window, pct))
}

/// This session's own transcript jsonl, harness-aware: the claude projects
/// tree glob by id, else the codex sessions tree. The env dirs seam both,
/// the same names the Python locator read.
fn own_transcript(session_id: &str, harness: &str) -> Option<PathBuf> {
    if session_id.is_empty() || harness.is_empty() {
        return None;
    }
    if harness == "claude" {
        let dir = std::env::var_os("FNO_CLAUDE_PROJECTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".claude")
                    .join("projects")
            });
        let mut best: Option<PathBuf> = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut roots: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            roots.sort();
            for root in roots {
                let candidate = root.join(format!("{session_id}.jsonl"));
                if candidate.is_file() {
                    best = Some(candidate);
                    break;
                }
            }
        }
        return best;
    }
    if harness == "codex" {
        let dir = std::env::var_os("FNO_CODEX_SESSIONS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".codex")
                    .join("sessions")
            });
        let mut best: Option<PathBuf> = None;
        let mut stack: Vec<PathBuf> = vec![dir];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> =
                entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            paths.sort();
            for p in paths {
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().map(|e| e == "jsonl").unwrap_or(false)
                    && p.file_name()
                        .map(|n| n.to_string_lossy().contains(session_id))
                        .unwrap_or(false)
                {
                    // First match in sorted order wins, one transcript per id.
                    if best.is_none() {
                        best = Some(p);
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        return best;
    }
    None
}

/// What model is actually answering, from the transcript tail, or None.
fn session_model(ident: &crate::spawn_context::OwnedIdentity) -> Option<String> {
    let sid = ident.session_id.as_deref()?;
    let harness = ident.harness.as_deref()?;
    let path = own_transcript(sid, harness)?;
    let bytes = transcript_tail_bytes(&path, 256 * 1024)?;
    for line in bytes.lines().rev() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let model = match harness {
            "claude" => {
                if record.get("type").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                if record.get("isSidechain") == Some(&Value::Bool(true)) {
                    continue;
                }
                record
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
            }
            "codex" => {
                if record.get("type").and_then(Value::as_str) != Some("turn_context") {
                    continue;
                }
                record
                    .get("payload")
                    .and_then(|p| p.get("model"))
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
            }
            _ => None,
        };
        if let Some(model) = model {
            return Some(model.to_string());
        }
    }
    None
}

fn transcript_tail_bytes(path: &Path, max_bytes: usize) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len() as usize;
    let start = size.saturating_sub(max_bytes);
    let mut f = std::fs::File::open(path).ok()?;
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(start as u64)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// The registry rows, tolerant: a missing or malformed file reads as none.
pub(crate) fn registry_rows(home: &AgentsHome) -> Vec<Value> {
    crate::registry_store::read(&home.registry_json())
        .ok()
        .and_then(|v| v.get("agents").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn row_harness(row: &Value) -> String {
    row.get("harness")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// The registry row matching this session id, scoped to its harness; the
/// claude short-id prefix is the second pass. The same match `fno agents
/// whoami` makes, so "who am I" has one answer.
fn find_by_session<'a>(rows: &'a [Value], session_uuid: &str, harness: &str) -> Option<&'a Value> {
    let harness = harness.to_ascii_lowercase();
    for row in rows {
        if row_harness(row) != harness {
            continue;
        }
        let mut fields = vec![row.get("harness_session_id").and_then(Value::as_str)];
        if harness == "claude" {
            fields.push(row.get("cc_session_id").and_then(Value::as_str));
        }
        if fields.iter().any(|f| *f == Some(session_uuid)) {
            return Some(row);
        }
    }
    if harness == "claude" {
        let norm = session_uuid.replace('-', "").to_ascii_lowercase();
        for row in rows {
            if row_harness(row) != "claude" {
                continue;
            }
            let sid = row
                .get("short_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .replace('-', "")
                .to_ascii_lowercase();
            if !sid.is_empty() && norm.starts_with(&sid) {
                return Some(row);
            }
        }
    }
    None
}

/// The holder a task-grain claim would bind: the exported roster name, else
/// the registry row's, else the ambient session id when it is provably ours
/// or at least not the worktree manifest's shared value.
fn task_holder(
    ident: &crate::spawn_context::OwnedIdentity,
    home: &AgentsHome,
    project_root: &Path,
) -> Option<String> {
    let from_env = std::env::var("FNO_WORKER_NAME")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if let Some(name) = from_env {
        return Some(name);
    }
    let (Some(sid), Some(_harness)) = (&ident.session_id, &ident.harness) else {
        return None;
    };
    let proven = matches!(
        ident.disposition,
        OwnedDisposition::Proven | OwnedDisposition::AsCanonical
    );
    if !proven
        && manifest_identity_values(project_root)
            .iter()
            .any(|v| v == sid)
    {
        return None;
    }
    let rows = registry_rows(home);
    let row = find_by_session(&rows, sid, _harness);
    match row {
        Some(row) => Some(
            row.get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .unwrap_or(sid)
                .to_string(),
        ),
        None => Some(sid.clone()),
    }
}

/// The manifest's identity values, found by walking up from the project
/// root, never climbing past a checkout root marker.
fn manifest_identity_values(project_root: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let start = project_root.to_path_buf();
    for dir in start.ancestors() {
        let manifest = dir.join(".fno").join("target-state.md");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            if dir.join(".git").exists() {
                break;
            }
            continue;
        };
        if let Ok(fields) = extract_frontmatter(&text) {
            for key in ["harness_session_id", "claude_session_id", "session_id"] {
                if let Some(v) = fields.get(key).and_then(str_or_none) {
                    out.push(v);
                }
            }
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    out
}

/// The one role rendering, or None: the row's role label plus its grantor.
fn role_text(ident: &crate::spawn_context::OwnedIdentity, home: &AgentsHome) -> Option<String> {
    let (Some(sid), Some(harness)) = (&ident.session_id, &ident.harness) else {
        return None;
    };
    let rows = registry_rows(home);
    let row = find_by_session(&rows, sid, harness)?;
    let label = row.get("role_label").and_then(Value::as_str)?;
    let grantor = row
        .get("role_grantor")
        .and_then(Value::as_str)
        .filter(|g| !g.is_empty())
        .unwrap_or("human");
    Some(format!("{label} (by {grantor})"))
}

// -------------------------------------------------------------------------
// Mail-unread count over the bus log
// -------------------------------------------------------------------------

/// Total unread bus messages across every durable address this session
/// answers to. Each address guards independently: one unreadable lane never
/// zeroes the others, and the recovery command never gains a failure mode.
fn mail_unread_count(
    handle: Option<&str>,
    agent_self: Option<&str>,
    session_id: Option<&str>,
    project_root: &Path,
) -> u64 {
    let mut total: u64 = 0;
    let mut seen: Vec<String> = Vec::new();
    for addr in [handle, agent_self] {
        let Some(addr) = addr.map(str::to_string) else {
            continue;
        };
        if seen.contains(&addr) {
            continue;
        }
        seen.push(addr.clone());
        total += scan_unread_count(&addr, None);
    }
    if let Some(project) = resolve_project(project_root) {
        if !seen.contains(&project) {
            // The project read excludes this session's own identities: a
            // plain scan would count the session's own project broadcasts
            // as unread dead letters it can never ack.
            let self_ids: Vec<String> = [handle, agent_self, session_id]
                .into_iter()
                .flatten()
                .map(str::to_string)
                .collect();
            total += scan_unread_count(&project, Some(&self_ids));
        }
    }
    total
}

/// The project lane address: the nearest `.fno/config.toml` (else legacy
/// `settings.yaml`) naming a project id, walking up from the project root.
/// A malformed or silent candidate keeps the walk going.
fn resolve_project(project_root: &Path) -> Option<String> {
    for dir in project_root.ancestors() {
        let fno_dir = dir.join(".fno");
        let candidate = if fno_dir.join("config.toml").is_file() {
            fno_dir.join("config.toml")
        } else if fno_dir.join("settings.yaml").is_file() {
            fno_dir.join("settings.yaml")
        } else {
            continue;
        };
        if let Some(pid) = project_id_from(&candidate) {
            return Some(pid);
        }
    }
    None
}

/// `config.project.id` is canonical; the top-level `project` key is the
/// deprecated alias, either a bare string or a mapping with `id`. None keeps
/// the walk going.
fn project_id_from(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    if path.extension().map(|e| e == "toml").unwrap_or(false) {
        let doc: toml::Value = toml::from_str(&text).ok()?;
        let cfg = doc.get("config").and_then(|c| c.get("project"));
        if let Some(id) = cfg.and_then(|p| p.get("id")).and_then(|v| v.as_str()) {
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
        return match doc.get("project") {
            Some(toml::Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(toml::Value::Table(t)) => t
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            _ => None,
        };
    }
    let Ok(fields) = parse_state_yaml(&text) else {
        return None;
    };
    let cfg = fields
        .get("config")
        .and_then(|c| c.get("project"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(id) = cfg.get("id").and_then(str_or_none) {
        return Some(id);
    }
    match fields.get("project") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Object(map)) => map.get("id").and_then(str_or_none),
        _ => None,
    }
}

fn bus_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FNO_BUS_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(root) = std::env::var_os("FNO_INBOX_ROOT") {
        return Some(PathBuf::from(root).join(".bus"));
    }
    let base = std::env::var_os("HOME")?;
    Some(PathBuf::from(base).join(".fno").join("bus"))
}

/// Unread messages for one address after its cursor, the count the drain
/// would print. Withdrawals, hosted/typed receipts and duplicate ids filter
/// here, the same choke point every reader of the log shares.
fn scan_unread_count(name: &str, exclude_from: Option<&[String]>) -> u64 {
    let Some(bus) = bus_dir() else { return 0 };
    let rows = match read_bus_rows(&bus) {
        Ok(rows) => rows,
        Err(_) => return 0,
    };
    let cursor = read_cursor(&bus, name);
    // Ids retracted by a same-sender tombstone, plus the tombstones.
    let mut retracted: std::collections::HashSet<String> = Default::default();
    let by_id: std::collections::HashMap<&str, &Value> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(|id| (id, r)))
        .collect();
    for m in &rows {
        if m.get("kind").and_then(Value::as_str) != Some("withdrawn") {
            continue;
        }
        if let Some(id) = m.get("id").and_then(Value::as_str) {
            retracted.insert(id.to_string());
        }
        let target_id = m
            .get("meta")
            .and_then(|meta| meta.get("withdraws"))
            .and_then(Value::as_str);
        if let Some(target_id) = target_id {
            if let Some(target) = by_id.get(target_id) {
                if target.get("from").and_then(Value::as_str)
                    == m.get("from").and_then(Value::as_str)
                    && target.get("to").and_then(Value::as_str)
                        == m.get("to").and_then(Value::as_str)
                {
                    retracted.insert(target_id.to_string());
                }
            }
        }
    }
    // One row per id, first in log order wins: a staged row and its durable
    // twin share an id and the twin must not resurface.
    let mut seen_ids: std::collections::HashSet<&str> = Default::default();
    let mut superseded: std::collections::HashSet<&str> = Default::default();
    for m in &rows {
        if m.get("delivery").and_then(Value::as_str) == Some("hosted") {
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                superseded.insert(id);
            }
        }
    }
    let mut unread: u64 = 0;
    let mut passed_cursor = cursor.is_none();
    for m in &rows {
        let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
        if !seen_ids.insert(id) {
            continue;
        }
        if cursor.as_deref() == Some(id) {
            passed_cursor = true;
        }
        if !passed_cursor {
            continue;
        }
        if m.get("to").and_then(Value::as_str) != Some(name) {
            continue;
        }
        // Control rows and delivered proofs are bookkeeping, never inbox.
        if matches!(
            m.get("kind").and_then(Value::as_str),
            Some("withdrawn") | Some("landed")
        ) {
            continue;
        }
        if matches!(
            m.get("delivery").and_then(Value::as_str),
            Some("hosted") | Some("typed") | Some("cross-session")
        ) {
            continue;
        }
        if retracted.contains(id) || superseded.contains(id) {
            continue;
        }
        if let Some(excl) = exclude_from {
            let from = m.get("from").and_then(Value::as_str).unwrap_or("");
            let from_session = m.get("from_session").and_then(Value::as_str).unwrap_or("");
            if excl
                .iter()
                .any(|e| e == from || (!from_session.is_empty() && e == from_session))
            {
                continue;
            }
        }
        unread += 1;
    }
    unread
}

/// Retained segments oldest to newest: `.N` (high N) ... `.1`, then live.
fn read_bus_rows(bus: &Path) -> Result<Vec<Value>, String> {
    let live = bus.join("messages.jsonl");
    let mut segments: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(bus) {
        let mut rotated: Vec<(u32, PathBuf)> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                let suffix = name.strip_prefix("messages.jsonl.")?;
                suffix.parse::<u32>().ok().map(|n| (n, p))
            })
            .collect();
        rotated.sort_by(|a, b| b.0.cmp(&a.0));
        segments.extend(rotated.into_iter().map(|(_, p)| p));
    }
    if live.exists() {
        segments.push(live);
    }
    let mut rows: Vec<Value> = Vec::new();
    for seg in segments {
        // Streamed line by line: the bus log can grow and the orientation
        // path reads it on every render.
        use std::io::BufRead;
        let Ok(file) = std::fs::File::open(&seg) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines() {
            let Ok(line) = line else { continue };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(obj) = serde_json::from_str::<Value>(line) {
                if obj.is_object() {
                    rows.push(obj);
                }
            }
        }
    }
    Ok(rows)
}

fn read_cursor(bus: &Path, name: &str) -> Option<String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let path = bus.join("cursors").join(format!("{name}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let obj: Value = serde_json::from_str(&text).ok()?;
    obj.get("last_seen_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(harness: &str) -> crate::spawn_context::OwnedIdentity {
        crate::spawn_context::OwnedIdentity {
            session_id: Some("879d8d26-claude-id".into()),
            harness: Some(harness.into()),
            disposition: OwnedDisposition::Proven,
            markers_present: vec![],
            rejected: vec![],
        }
    }

    /// Save-and-restore the ambient names a test touches, so sibling tests
    /// in this binary never read a stranger's stamp.
    struct CleanEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl CleanEnv {
        fn scrub(names: &[&'static str]) -> Self {
            let saved = names.iter().map(|n| (*n, std::env::var_os(n))).collect();
            for n in names {
                std::env::remove_var(n);
            }
            CleanEnv(saved)
        }
    }
    impl Drop for CleanEnv {
        fn drop(&mut self) {
            for (n, v) in self.0.drain(..) {
                match v {
                    Some(v) => std::env::set_var(n, v),
                    None => std::env::remove_var(n),
                }
            }
        }
    }

    #[test]
    fn foreign_line_names_the_inherited_family_it_cannot_unexport() {
        let _env = CleanEnv::scrub(&[
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "CODEX_COMPANION_SESSION_ID",
            "CODEX_COMPANION_TRANSCRIPT_PATH",
        ]);
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "879d8d26-claude-id");
        std::env::set_var("CODEX_THREAD_ID", "thread-x");
        std::env::set_var("CODEX_SESSION_ID", "root-y");

        let line = foreign_identity_line(&ident("claude")).expect("line renders");

        assert!(line.contains("inherited codex name"), "{line}");
        assert!(line.contains("-u CODEX_THREAD_ID"), "{line}");
        // Never offers to strip this session's own markers.
        assert!(!line.contains("CLAUDE_CODE_SESSION_ID"), "{line}");
    }

    #[test]
    fn opencode_session_is_never_told_to_delete_its_own_marker() {
        let _env = CleanEnv::scrub(&[
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "CODEX_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "CLAUDE_SESSION_ID",
        ]);
        std::env::set_var("OPENCODE_SESSION_ID", "ses_9f2a");

        assert!(foreign_identity_line(&ident("opencode")).is_none());
    }

    #[test]
    fn the_context_line_formats_thousands_with_commas() {
        assert_eq!(with_commas(1_000_000), "1,000,000");
        assert_eq!(with_commas(307_850), "307,850");
        assert_eq!(with_commas(999), "999");
    }
}

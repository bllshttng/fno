//! The managed account store: the per-account snapshot under
//! `<state_dir>/providers/<id>/` (dir 700, `blob` and `meta.json` 600), the
//! `.active-<cli>` stamp with its taint marker and proof cache, and the
//! shared credential slot a managed account materializes into (the claude
//! Keychain items or credentials file, the codex `auth.json`). Ported from
//! the store layer of `managed.py`; the switch, register and reconcile that
//! compose these primitives port in their own leaves.
//!
//! Everything outside the process (`security`, the process table, a
//! process's env and start time, `codex login status`) goes through [Host],
//! so `tests/accounts_store_parity.rs` drives the same logic from a golden.

use crate::accounts::binding::token_present;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const UNSCOPED_SERVICE: &str = "Claude Code-credentials";
const SECURITY_TIMEOUT: Duration = Duration::from_secs(5);
const CODEX_LOGIN_TIMEOUT: Duration = Duration::from_secs(5);
const CODEX_AUTH_ENV_VARS: [&str; 3] = ["CODEX_ACCESS_TOKEN", "CODEX_API_KEY", "OPENAI_API_KEY"];

/// `ManagedStoreError` and its `KeychainError` subclass, with the Python text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Store(String),
    Keychain(String),
}

impl StoreError {
    pub fn kind(&self) -> &'static str {
        match self {
            StoreError::Store(_) => "ManagedStoreError",
            StoreError::Keychain(_) => "KeychainError",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            StoreError::Store(m) | StoreError::Keychain(m) => m,
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// One finished `security` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityRun {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// A process as the pin scan sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: Option<String>,
    pub cmdline: Vec<String>,
    pub started: Option<f64>,
}

/// How `codex login status` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexRun {
    Exit(bool),
    Missing,
    Timeout,
    Failed(String),
}

/// The reads and writes the store makes outside its own files.
pub trait Host {
    /// Whether the claude slot is the Keychain (macOS) rather than a file.
    fn keychain_slot(&self) -> bool;
    /// `security <args>` under the 5 s ceiling; `Err` carries the
    /// KeychainError text for a timeout or a failed spawn.
    fn security(&self, args: &[String]) -> Result<SecurityRun, StoreError>;
    fn processes(&self) -> Vec<ProcInfo>;
    /// `Err` when the env cannot be read, `Ok(None)` when `key` is unset.
    fn env_value(&self, pid: u32, key: &str) -> Result<Option<String>, ()>;
    /// Epoch seconds the process started, `None` when gone or unreadable.
    fn started_at(&self, pid: u32) -> Option<f64>;
    fn pid_exists(&self, pid: u32) -> bool;
    fn self_pid(&self) -> u32;
    /// `codex login status` with `CODEX_HOME` set and the auth vars scrubbed.
    fn codex_login_status(&self, codex_home: &Path) -> CodexRun;
}

/// The live host.
pub struct SystemHost;

impl Host for SystemHost {
    fn keychain_slot(&self) -> bool {
        cfg!(target_os = "macos")
    }
    fn security(&self, args: &[String]) -> Result<SecurityRun, StoreError> {
        let first = args.first().map(String::as_str).unwrap_or("");
        let mut command = Command::new("security");
        command.args(args);
        match run_bounded(&mut command, SECURITY_TIMEOUT) {
            Ok(Some((code, stdout, stderr))) => Ok(SecurityRun {
                code,
                stdout,
                stderr,
            }),
            Ok(None) => Err(StoreError::Keychain(format!(
                "`security {first}` timed out after {}s",
                SECURITY_TIMEOUT.as_secs()
            ))),
            Err(e) => Err(StoreError::Keychain(format!(
                "`security {first}` failed to run: {e}"
            ))),
        }
    }
    fn processes(&self) -> Vec<ProcInfo> {
        let (rows, _unreadable) = crate::census::process_table();
        rows.into_iter()
            .map(|row| ProcInfo {
                pid: row.pid,
                name: None,
                cmdline: row.command.split_whitespace().map(str::to_string).collect(),
                // Sampled per pin in the scan, not for every row here.
                started: None,
            })
            .collect()
    }
    fn env_value(&self, pid: u32, key: &str) -> Result<Option<String>, ()> {
        crate::spawn_context::process_env_value(pid, key)
    }
    fn started_at(&self, pid: u32) -> Option<f64> {
        crate::process_probe::process_started_epoch(pid)
    }
    fn pid_exists(&self, pid: u32) -> bool {
        crate::agent_lock::pid_is_alive(u64::from(pid))
    }
    fn self_pid(&self) -> u32 {
        std::process::id()
    }
    fn codex_login_status(&self, codex_home: &Path) -> CodexRun {
        let mut command = Command::new("codex");
        command
            .args(["login", "status"])
            .env("CODEX_HOME", codex_home);
        for name in CODEX_AUTH_ENV_VARS {
            command.env_remove(name);
        }
        match run_bounded(&mut command, CODEX_LOGIN_TIMEOUT) {
            Ok(Some((code, _, _))) => CodexRun::Exit(code == 0),
            Ok(None) => CodexRun::Timeout,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => CodexRun::Missing,
            Err(e) => CodexRun::Failed(e.to_string()),
        }
    }
}

/// Run `command` with piped output under `timeout`: `Ok(None)` when it was
/// killed at the bound, else the exit code (-1 for a signal) and the output.
pub(crate) fn run_bounded(
    command: &mut Command,
    timeout: Duration,
) -> std::io::Result<Option<(i32, String, String)>> {
    use std::io::Read;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
        }
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    Ok(Some((status.code().unwrap_or(-1), stdout, stderr)))
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

pub fn account_dir(root: &Path, record_id: &str) -> PathBuf {
    root.join(record_id)
}

pub fn blob_path(root: &Path, record_id: &str) -> PathBuf {
    account_dir(root, record_id).join("blob")
}

pub fn meta_path(root: &Path, record_id: &str) -> PathBuf {
    account_dir(root, record_id).join("meta.json")
}

/// Per-CLI, so a codex switch never makes a later claude switch capture the
/// codex slot.
pub fn stamp_path(root: &Path, cli: &str) -> PathBuf {
    root.join(format!(".active-{cli}"))
}

pub fn taint_path(root: &Path, cli: &str) -> PathBuf {
    root.join(format!(".active-{cli}.tainted"))
}

pub fn principal_cache_path(root: &Path, cli: &str) -> PathBuf {
    root.join(format!(".active-{cli}.principal"))
}

pub fn switch_lock_path(root: &Path) -> PathBuf {
    root.join(".switch.lock")
}

/// The switch mutex `managed.switch` holds: an exclusive flock on
/// `<store>/.switch.lock`, polled until `timeout`.
pub fn lock_switch(root: &Path, timeout: Duration) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(switch_lock_path(root))
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: flock on a descriptor this function owns; dropping the
        // returned file releases it.
        if unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        } == 0
        {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    crate::claude_vault::atomic_write(path, content)
}

fn iso(now: f64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(now.floor() as i64, 0)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Slot backends
// ---------------------------------------------------------------------------

fn keychain_account() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The shared slot dir a managed claude account materializes into.
pub fn claude_slot_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".claude"),
    }
}

pub fn codex_slot_auth_path() -> PathBuf {
    let dir = match std::env::var_os("CODEX_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".codex"),
    };
    dir.join("auth.json")
}

fn scoped_service(cfg: &Path) -> Result<String, StoreError> {
    crate::claude_vault::scoped_service(cfg).map_err(StoreError::Keychain)
}

/// The credential the CLI reads from its slot now, `None` when no login.
pub fn read_slot_blob(
    cli: &str,
    config_dir: Option<&Path>,
    host: &dyn Host,
) -> Result<Option<String>, StoreError> {
    if cli == "codex" {
        return Ok(std::fs::read_to_string(codex_slot_auth_path()).ok());
    }
    let default_cfg = claude_slot_dir();
    let cfg = config_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_cfg.clone());
    let shared = cfg == default_cfg;
    read_claude_blob(&cfg, shared, host)
}

/// claude's credential for `cfg`; `shared` also allows the unscoped item,
/// which belongs to the shared slot only.
fn read_claude_blob(
    cfg: &Path,
    shared: bool,
    host: &dyn Host,
) -> Result<Option<String>, StoreError> {
    if host.keychain_slot() {
        let mut services = vec![scoped_service(cfg)?];
        if shared {
            services.push(UNSCOPED_SERVICE.to_string());
        }
        let account = keychain_account();
        for service in services {
            let out = host.security(&args(&[
                "find-generic-password",
                "-s",
                &service,
                "-a",
                &account,
                "-w",
            ]))?;
            let blob = out.stdout.trim();
            // A logged-out residue keeps scopes but clears the tokens; fall
            // through to the next item rather than return it as a login.
            if out.code == 0 && !blob.is_empty() && token_present(blob) {
                return Ok(Some(blob.to_string()));
            }
        }
        return Ok(None);
    }
    match std::fs::read_to_string(cfg.join(".credentials.json")) {
        Ok(blob) if token_present(&blob) => Ok(Some(blob)),
        _ => Ok(None),
    }
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

/// Materialize `blob` into the slot the CLI reads. On the Keychain the
/// unscoped item is written first and the scoped item last, so a failed
/// second write leaves the scoped item, which claude reads first, on the
/// pre-switch token.
pub fn write_slot_blob(
    cli: &str,
    blob: &str,
    config_dir: Option<&Path>,
    host: &dyn Host,
) -> Result<(), StoreError> {
    if cli == "codex" {
        return write_private(&codex_slot_auth_path(), blob).map_err(|e| {
            StoreError::Store(format!("failed to write codex credential to slot: {e}"))
        });
    }
    let cfg = config_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(claude_slot_dir);
    if host.keychain_slot() {
        let account = keychain_account();
        for service in [UNSCOPED_SERVICE.to_string(), scoped_service(&cfg)?] {
            // The blob rides argv: `security` has no stdin path for this.
            let out = host.security(&args(&[
                "add-generic-password",
                "-U",
                "-s",
                &service,
                "-a",
                &account,
                "-w",
                blob,
            ]))?;
            if out.code != 0 {
                return Err(StoreError::Keychain(format!(
                    "`security add-generic-password -s {service}` exited {}: {}",
                    out.code,
                    out.stderr.trim()
                )));
            }
        }
        return Ok(());
    }
    std::fs::create_dir_all(&cfg)
        .and_then(|_| write_private(&cfg.join(".credentials.json"), blob))
        .map_err(|e| StoreError::Store(format!("failed to write credential to slot: {e}")))
}

/// The slot reads back the blob just written and it carries a credential.
pub fn verify_slot(harness: &str, expected: &str, host: &dyn Host) -> Result<bool, StoreError> {
    let Some(got) = read_slot_blob(harness, None, host)? else {
        return Ok(false);
    };
    if got.trim() != expected.trim() {
        return Ok(false);
    }
    Ok(if harness == "codex" {
        codex_auth_present(&got)
    } else {
        token_present(&got)
    })
}

/// `codex login status` against the slot's home: `ok` is `None` when codex
/// could not answer, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexLogin {
    pub ok: Option<bool>,
    pub reason: Option<&'static str>,
}

pub fn codex_login_ok(host: &dyn Host) -> Result<CodexLogin, StoreError> {
    let auth = codex_slot_auth_path();
    let home = auth.parent().map(Path::to_path_buf).unwrap_or_default();
    match host.codex_login_status(&home) {
        CodexRun::Exit(ok) => Ok(CodexLogin {
            ok: Some(ok),
            reason: None,
        }),
        CodexRun::Missing => Ok(CodexLogin {
            ok: None,
            reason: Some("codex-login-status-missing"),
        }),
        CodexRun::Timeout => Ok(CodexLogin {
            ok: None,
            reason: Some("codex-login-status-timeout"),
        }),
        CodexRun::Failed(e) => Err(StoreError::Store(format!(
            "`codex login status` failed to run: {e}"
        ))),
    }
}

/// Credential material for Codex's effective `auth.json` mode.
pub fn codex_auth_present(blob: &str) -> bool {
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(blob) else {
        return false;
    };
    let nonempty = |v: Option<&Value>| matches!(v, Some(Value::String(s)) if !s.trim().is_empty());
    let tokens_present = |refresh_required: bool| {
        let Some(Value::Object(tokens)) = data.get("tokens") else {
            return false;
        };
        if !["access_token", "id_token"]
            .iter()
            .all(|f| nonempty(tokens.get(*f)))
        {
            return false;
        }
        let refresh = tokens.get("refresh_token");
        if refresh_required {
            nonempty(refresh)
        } else {
            matches!(refresh, Some(Value::String(_)))
        }
    };
    let identity_present = || match data.get("agent_identity") {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Object(identity)) => {
            [
                "agent_runtime_id",
                "agent_private_key",
                "account_id",
                "chatgpt_user_id",
            ]
            .iter()
            .all(|f| nonempty(identity.get(*f)))
                && nonempty(identity.get("plan_type"))
                && matches!(
                    identity.get("chatgpt_account_is_fedramp"),
                    Some(Value::Bool(_))
                )
        }
        _ => false,
    };
    let bedrock_present = || match data.get("bedrock_api_key") {
        Some(Value::Object(b)) => ["api_key", "region"].iter().all(|f| nonempty(b.get(*f))),
        _ => false,
    };
    let present = |key: &str| !matches!(data.get(key), None | Some(Value::Null));
    let mode = match data.get("auth_mode") {
        None | Some(Value::Null) => {
            if present("personal_access_token") {
                "personalAccessToken".to_string()
            } else if present("bedrock_api_key") {
                "bedrockApiKey".to_string()
            } else if present("OPENAI_API_KEY") {
                "apikey".to_string()
            } else {
                "chatgpt".to_string()
            }
        }
        Some(Value::String(s)) => s.clone(),
        Some(_) => return false,
    };
    match mode.as_str() {
        "apikey" => nonempty(data.get("OPENAI_API_KEY")),
        "chatgpt" => tokens_present(true),
        "chatgptAuthTokens" => tokens_present(false),
        "agentIdentity" => identity_present(),
        "personalAccessToken" => nonempty(data.get("personal_access_token")),
        "bedrockApiKey" => bedrock_present(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Live-pin gate
// ---------------------------------------------------------------------------

/// A live process pinning a shared slot; `started` is sampled during the
/// scan, before the pid can be recycled.
#[derive(Debug, Clone, PartialEq)]
pub struct PinningSession {
    pub pid: u32,
    pub cmdline: String,
    pub started: Option<f64>,
}

fn basename(part: &str) -> &str {
    Path::new(part)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
}

/// The standalone `claude` binary by name or by the first token of any argv
/// part.
pub fn looks_like_claude(name: Option<&str>, cmdline: &[String]) -> bool {
    if name.is_some_and(|n| basename(n) == "claude") {
        return true;
    }
    cmdline.iter().any(|part| {
        part.split_whitespace()
            .next()
            .is_some_and(|t| basename(t) == "claude")
    })
}

/// The standalone `codex` binary by name or argv[0] only, never a later arg
/// (`git commit -m "codex fix"` must not pin).
pub fn looks_like_codex(name: Option<&str>, cmdline: &[String]) -> bool {
    if name.is_some_and(|n| basename(n) == "codex") {
        return true;
    }
    cmdline
        .first()
        .and_then(|first| first.split_whitespace().next())
        .is_some_and(|t| basename(t) == "codex")
}

fn resolve(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Live processes pinning `slot_dir` through their effective `env_var`. An
/// unreadable env pins: deferring a switch is safe, rotating credentials
/// under a live session is not.
fn pinning_sessions(
    looks_like: fn(Option<&str>, &[String]) -> bool,
    env_var: &str,
    slot_dir: &Path,
    default_dir: &Path,
    host: &dyn Host,
) -> Vec<PinningSession> {
    let slot = resolve(slot_dir);
    let default_resolved = resolve(default_dir);
    let me = host.self_pid();
    let mut found = Vec::new();
    for proc in host.processes() {
        if proc.pid == me || !looks_like(proc.name.as_deref(), &proc.cmdline) {
            continue;
        }
        let pins = match host.env_value(proc.pid, env_var) {
            Err(()) => true,
            Ok(Some(dir)) if !dir.is_empty() => resolve(Path::new(&dir)) == slot,
            Ok(_) => default_resolved == slot,
        };
        if pins {
            found.push(PinningSession {
                pid: proc.pid,
                cmdline: proc.cmdline.join(" "),
                started: proc.started.or_else(|| host.started_at(proc.pid)),
            });
        }
    }
    found
}

/// The pin scan for `cli`'s slot. Only claude and codex have one; any other
/// cli is refused before a slot is read or written.
pub fn pinning_sessions_for(cli: &str, host: &dyn Host) -> Result<Vec<PinningSession>, StoreError> {
    match cli {
        "codex" => {
            let auth = codex_slot_auth_path();
            let slot_dir = auth.parent().map(Path::to_path_buf).unwrap_or_default();
            Ok(pinning_sessions(
                looks_like_codex,
                "CODEX_HOME",
                &slot_dir,
                &home().join(".codex"),
                host,
            ))
        }
        "claude" => Ok(pinning_sessions(
            looks_like_claude,
            "CLAUDE_CONFIG_DIR",
            &claude_slot_dir(),
            &home().join(".claude"),
            host,
        )),
        _ => Err(StoreError::Store(format!(
            "managed account switching is not supported for cli '{cli}' \
             (only claude and codex have a managed credential slot)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Snapshot the current slot login into the record's store. Refuses when no
/// login exists, so an empty blob is never stored.
pub fn snapshot_current(
    root: &Path,
    record_id: &str,
    harness: &str,
    account_id: &str,
    host: &dyn Host,
    now: f64,
) -> Result<PathBuf, StoreError> {
    match read_slot_blob(harness, None, host)? {
        Some(blob) if !blob.trim().is_empty() => write_snapshot(
            root,
            record_id,
            harness,
            account_id,
            &blob,
            harness == "claude" && host.keychain_slot(),
            now,
        ),
        _ => Err(StoreError::Store(format!(
            "no current {harness} login to snapshot for '{record_id}' (sign in first, then register)"
        ))),
    }
}

/// Store `blob` as the record's snapshot. A proven `principal` survives the
/// rewrite: it names the account, not the credential. `keychain` marks the
/// snapshot as taken from the Keychain rather than a file.
pub fn write_snapshot(
    root: &Path,
    record_id: &str,
    harness: &str,
    account_id: &str,
    blob: &str,
    keychain: bool,
    now: f64,
) -> Result<PathBuf, StoreError> {
    write_snapshot_with(
        root, record_id, harness, account_id, blob, keychain, now, None,
    )
}

/// A claude Keychain snapshot bound to the `principal` just proven for it,
/// in one meta write, so no crash can pair the new blob with a stale
/// principal.
pub fn write_bound_snapshot(
    root: &Path,
    record_id: &str,
    blob: &str,
    principal: &Value,
    now: f64,
) -> Result<PathBuf, StoreError> {
    write_snapshot_with(
        root,
        record_id,
        "claude",
        record_id,
        blob,
        true,
        now,
        Some(principal),
    )
}

#[allow(clippy::too_many_arguments)]
fn write_snapshot_with(
    root: &Path,
    record_id: &str,
    harness: &str,
    account_id: &str,
    blob: &str,
    keychain: bool,
    now: f64,
    principal: Option<&Value>,
) -> Result<PathBuf, StoreError> {
    let dir = account_dir(root, record_id);
    let fail = |e: std::io::Error| {
        StoreError::Store(format!("failed to write snapshot for '{record_id}': {e}"))
    };
    std::fs::create_dir_all(&dir).map_err(fail)?;
    crate::paths::set_dir_mode_0700(&dir).map_err(fail)?;
    write_private(&blob_path(root, record_id), blob).map_err(fail)?;
    let mut meta = Map::new();
    meta.insert("harness".into(), json!(harness));
    let account = if account_id.is_empty() {
        record_id
    } else {
        account_id
    };
    meta.insert("account_id".into(), json!(account));
    meta.insert("captured_at".into(), json!(iso(now)));
    meta.insert(
        "kind".into(),
        json!(if keychain { "keychain" } else { "file" }),
    );
    if let Some(principal) = principal {
        meta.insert("principal".into(), principal.clone());
        meta.insert("principal_at".into(), json!(iso(now)));
    } else if let Some(prior) = read_meta(root, record_id) {
        for key in ["principal", "principal_at"] {
            if let Some(v) = prior.get(key) {
                meta.insert(key.into(), v.clone());
            }
        }
    }
    write_private(&meta_path(root, record_id), &pretty(&Value::Object(meta))).map_err(fail)?;
    Ok(dir)
}

/// The stored blob for `record_id`, `None` when unregistered.
pub fn read_blob(root: &Path, record_id: &str) -> Option<String> {
    std::fs::read_to_string(blob_path(root, record_id)).ok()
}

/// The record's metadata, `None` when absent or not a JSON object.
pub fn read_meta(root: &Path, record_id: &str) -> Option<Map<String, Value>> {
    match crate::claude_vault::read_json(&meta_path(root, record_id))? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// The id of another account whose stored blob holds the same credential,
/// compared by digest so the secret never travels.
pub fn duplicate_credential_holder(root: &Path, blob: &str, exclude_id: &str) -> Option<String> {
    let target = crate::accounts::binding::credential_digest(blob)?;
    let mut entries: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    entries.sort();
    entries.into_iter().find(|name| {
        name != exclude_id
            && read_blob(root, name)
                .and_then(|b| crate::accounts::binding::credential_digest(&b))
                .as_deref()
                == Some(target.as_str())
    })
}

/// The `d`/`h`/`m` age ladder; a negative age floors to `0m`.
pub fn age_label_from_seconds(age_seconds: f64) -> String {
    let age = age_seconds.max(0.0);
    let days = (age / 86400.0).floor() as i64;
    if days >= 1 {
        return format!("{days}d");
    }
    let hours = (age / 3600.0).floor() as i64;
    if hours >= 1 {
        return format!("{hours}h");
    }
    format!("{}m", (age / 60.0).floor() as i64)
}

/// The credential snapshot's age for `list`: `none` when unregistered,
/// `unknown` when `captured_at` does not parse.
pub fn snapshot_age_label(root: &Path, record_id: &str, now: f64) -> String {
    let Some(captured) = read_meta(root, record_id).and_then(|m| m.get("captured_at").cloned())
    else {
        return "none".to_string();
    };
    let parsed = captured
        .as_str()
        .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ").ok());
    match parsed {
        Some(at) => age_label_from_seconds(now - at.and_utc().timestamp() as f64),
        None => "unknown".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Stamp and taint
// ---------------------------------------------------------------------------

/// Record which account is materialized in `cli`'s slot.
pub fn stamp_active_slot(root: &Path, cli: &str, record_id: &str) -> std::io::Result<()> {
    write_private(&stamp_path(root, cli), record_id)
}

/// The stamp was written under live pins, so the slot may no longer hold
/// the stamped account's credential.
pub fn slot_tainted(root: &Path, cli: &str) -> bool {
    taint_path(root, cli).exists()
}

/// One taint writer: sampled during a scan, or a bare pid whose start time
/// is read now.
#[derive(Debug, Clone, PartialEq)]
pub enum TaintWriter {
    Sampled(u32, Option<f64>),
    Pid(u32),
}

/// Write or clear the taint marker. The writers are what make it clearable:
/// each was launched under the outgoing account and can still flush that
/// credential into the slot; the start time keeps a recycled pid from
/// standing in for it.
pub fn set_slot_taint(
    root: &Path,
    cli: &str,
    tainted: bool,
    writers: &[TaintWriter],
    host: &dyn Host,
) -> std::io::Result<()> {
    let path = taint_path(root, cli);
    if !tainted {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let rows: Vec<(u32, Option<f64>)> = writers
        .iter()
        .map(|w| match w {
            TaintWriter::Sampled(pid, started) => (*pid, *started),
            TaintWriter::Pid(pid) => (*pid, host.started_at(*pid)),
        })
        .collect();
    let body = json!({
        "pids": rows.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
        "writers": rows.iter().map(|(pid, started)| json!({"pid": pid, "started": started})).collect::<Vec<_>>(),
    });
    write_private(&path, &body.to_string())
}

/// `(pid, started)` for each recorded writer; `None` when the marker cannot
/// say (absent, unparsable, or from before writers were recorded with
/// neither list). A legacy pid-only marker reads with no start times.
pub fn tainting_writers(root: &Path, cli: &str) -> Option<Vec<(u32, Option<f64>)>> {
    let raw = std::fs::read_to_string(taint_path(root, cli)).ok()?;
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&raw) else {
        return None;
    };
    let pid_of = |v: Option<&Value>| {
        v.and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
    };
    if let Some(Value::Array(writers)) = data.get("writers") {
        return Some(
            writers
                .iter()
                .filter_map(|entry| {
                    let entry = entry.as_object()?;
                    let pid = pid_of(entry.get("pid"))?;
                    Some((pid, entry.get("started").and_then(Value::as_f64)))
                })
                .collect(),
        );
    }
    let Some(Value::Array(pids)) = data.get("pids") else {
        return None;
    };
    Some(
        pids.iter()
            .filter_map(|p| pid_of(Some(p)))
            .map(|pid| (pid, None))
            .collect(),
    )
}

/// Sessions that could still rewrite the slot with the previous account's
/// credential. No taint means nothing to wait on; a marker that cannot say
/// falls back to a live pin scan.
pub fn taint_writers_still_live(
    root: &Path,
    cli: &str,
    host: &dyn Host,
) -> Result<Vec<String>, StoreError> {
    if !slot_tainted(root, cli) {
        return Ok(Vec::new());
    }
    let Some(recorded) = tainting_writers(root, cli) else {
        return Ok(pinning_sessions_for(cli, host)?
            .into_iter()
            .map(|s| format!("pid {}", s.pid))
            .collect());
    };
    let mut alive = Vec::new();
    for (pid, started) in recorded {
        match host.started_at(pid) {
            None => {
                if host.pid_exists(pid) {
                    alive.push(format!("pid {pid}"));
                }
            }
            // A pid that started at another time is a recycled number.
            Some(current) => {
                if started.is_none_or(|s| (current - s).abs() < 1.0) {
                    alive.push(format!("pid {pid}"));
                }
            }
        }
    }
    Ok(alive)
}

// ---------------------------------------------------------------------------
// Principal writes
// ---------------------------------------------------------------------------

/// Bind `principal` to `record_id` in its private metadata.
pub fn write_record_principal(
    root: &Path,
    record_id: &str,
    principal: &Value,
    now: f64,
) -> std::io::Result<()> {
    let mut meta = read_meta(root, record_id).unwrap_or_default();
    meta.insert("principal".into(), principal.clone());
    meta.insert("principal_at".into(), json!(iso(now)));
    write_private(&meta_path(root, record_id), &pretty(&Value::Object(meta)))
}

/// Drop a record's principal binding and keep the rest of its metadata.
pub fn clear_record_principal(root: &Path, record_id: &str) {
    let mut meta = read_meta(root, record_id).unwrap_or_default();
    let had_principal = meta
        .shift_remove("principal")
        .is_some_and(|v| !is_falsy(&v));
    if !had_principal && !meta.contains_key("principal_at") {
        return;
    }
    meta.shift_remove("principal_at");
    let _ = write_private(&meta_path(root, record_id), &pretty(&Value::Object(meta)));
}

fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
    }
}

pub fn clear_slot_principal_cache(root: &Path, cli: &str) {
    let _ = std::fs::remove_file(principal_cache_path(root, cli));
}

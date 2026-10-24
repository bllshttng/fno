//! `fno config accounts use <id> [--scope project|global]`: make an account
//! active. A managed record first materializes its stored credential into the
//! CLI's shared slot ([switch]); every record then moves the routing pointer
//! in the scope's own config file.
//!
//! `accounts switch` is the same switch for recovery's auto-switch sweep: the
//! id arrives as JSON on stdin, a live pin defers, no pointer is written, and
//! the receipt is JSON. The daemon's slot cutover calls [use_account] in
//! process.
//!
//! Two guards make a switch safe: the outgoing account's rotated credential is
//! captured before the slot is overwritten, and a slot a live CLI holds is
//! never rotated without a warning (claude) or at all (codex). The switch
//! mutex serializes the whole check-and-write.

use crate::accounts::add::split_flag;
use crate::accounts::records::{self, Record, Scope};
use crate::accounts::register::Run;
use crate::accounts::show::pwd;
use crate::accounts::store::{self, Host, PinningSession, StoreError, TaintWriter};
use crate::accounts::writer;
use serde_json::{json, Map, Value};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const HELP: &str = "Usage: fno config accounts use [OPTIONS] PROVIDER_ID

  Set the active account (default scope: project).

      --scope TEXT  project|global  [default: project]";

const SWITCH_USAGE: &str =
    "usage: fno-agents spawn-overlay accounts switch  (a JSON payload on stdin: {\"id\", \"repo_root\"})";

/// What a live pin on the slot does to a claude switch; codex always defers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPolicy {
    /// Proceed and name the sessions, the same rewrite a manual /login makes.
    Warn,
    Defer,
}

/// A switch that landed. `verification` is `structural` or
/// `codex-recognized`; `pinned_by` names the live sessions a warn switch
/// proceeded under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchResult {
    pub active: String,
    pub slot_changed: bool,
    pub verification: &'static str,
    pub reason: Option<String>,
    pub pinned_by: Vec<u32>,
}

impl SwitchResult {
    fn landed(active: &str) -> Self {
        SwitchResult {
            active: active.to_string(),
            slot_changed: true,
            verification: "structural",
            reason: None,
            pinned_by: Vec::new(),
        }
    }
}

/// Why a switch did not land, with the text the operator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchError {
    /// A live pin or the held mutex; nothing in the slot or the store changed.
    Deferred(String),
    /// The stored login is dead or missing; an interactive login repairs it.
    NeedsLogin(String),
    Store(String),
    /// Ctrl-C during the codex probe, after the slot was rolled back.
    Interrupted(String),
}

fn store_err(e: StoreError) -> SwitchError {
    SwitchError::Store(e.message().to_string())
}

/// Everything the switch reaches outside the store files and the slot.
pub trait Outside {
    /// `provider-cap vault <action>` under the held switch mutex: `refresh`
    /// with the record id, or `sync`. `Ok` is the receipt's verdict.
    fn vault(&self, action: &str, root: &Path, id: Option<&str>) -> Result<String, String>;
    /// The interactive vault login with inherited stdio; its exit code.
    fn login(&self, root: &Path, id: &str) -> i32;
    /// Both stdin and stdout are a terminal, so a browser login can run.
    fn terminal(&self) -> bool;
    /// One `account_switched` event with these fields.
    fn emit(&self, fields: Map<String, Value>);
    fn now(&self) -> f64;
    fn lock_timeout(&self) -> Duration;
}

/// The live side: the vault in process, the agents events log under the
/// state dir `cwd` resolves. Only an `interactive` caller may run the
/// browser login.
pub struct SystemOutside {
    pub cwd: PathBuf,
    pub interactive: bool,
}

impl Outside for SystemOutside {
    fn vault(&self, action: &str, root: &Path, id: Option<&str>) -> Result<String, String> {
        crate::claude_vault::run_action(action, root, id, true)
            .map(|(_, verdict)| verdict)
            .map_err(|reason| format!("provider-cap vault {action}: {reason}"))
    }
    fn login(&self, root: &Path, id: &str) -> i32 {
        match crate::claude_vault::run_action("login", root, Some(id), false) {
            Ok((code, verdict)) => {
                println!("provider-cap vault login: {verdict}");
                code
            }
            Err(reason) => {
                eprintln!("provider-cap vault login: {reason}");
                2
            }
        }
    }
    fn terminal(&self) -> bool {
        self.interactive && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }
    fn emit(&self, fields: Map<String, Value>) {
        let Some(path) = crate::agents_config::state_dir(&self.cwd).map(|d| d.join("events.jsonl"))
        else {
            return;
        };
        let emitter = crate::events::EventEmitter::new(&path, "rust");
        if let Err(e) = emitter.emit_fields("account_switched", fields) {
            eprintln!(
                "fno agents: warning: events.emit('account_switched') to {}: {e}",
                path.display()
            );
        }
    }
    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_default()
    }
    fn lock_timeout(&self) -> Duration {
        Duration::from_secs(10)
    }
}

fn pid_names(pins: &[PinningSession]) -> String {
    pins.iter()
        .map(|p| format!("pid {}", p.pid))
        .collect::<Vec<_>>()
        .join(", ")
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn note_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Run `probe` with Ctrl-C noted instead of fatal, so the caller can roll the
/// slot back before it reports the interrupt.
fn interruptible<T>(probe: impl FnOnce() -> T) -> (bool, T) {
    INTERRUPTED.store(false, Ordering::SeqCst);
    // SAFETY: the handler only stores to an atomic, which is
    // async-signal-safe; the prior disposition is restored below.
    let prior = unsafe {
        libc::signal(
            libc::SIGINT,
            note_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t,
        )
    };
    let out = probe();
    // SAFETY: restores the disposition saved above.
    unsafe {
        libc::signal(libc::SIGINT, prior);
    }
    (INTERRUPTED.swap(false, Ordering::SeqCst), out)
}

fn rollback_materialized_slot(
    cli: &str,
    rollback_blob: Option<&str>,
    host: &dyn Host,
) -> (String, bool) {
    let Some(blob) = rollback_blob.filter(|b| !b.is_empty()) else {
        return (
            "nothing to roll back to; slot may hold the unverified target".to_string(),
            false,
        );
    };
    match store::write_slot_blob(cli, blob, None, host) {
        Ok(()) => ("slot rolled back to the previous account".to_string(), true),
        Err(e) => (
            format!("rollback ALSO failed ({e}); slot is in an indeterminate state"),
            false,
        ),
    }
}

fn clear_unverified_codex_stamp(root: &Path) -> String {
    match store::stamp_active_slot(root, "codex", "") {
        Ok(()) => "active stamp cleared because the slot occupant is unverified".to_string(),
        Err(e) => format!("active stamp could not be cleared ({e})"),
    }
}

fn rollback_codex_slot(
    rollback_blob: Option<&str>,
    root: &Path,
    host: &dyn Host,
) -> (String, bool) {
    let (tail, rolled_back) = rollback_materialized_slot("codex", rollback_blob, host);
    if rolled_back {
        return (tail, true);
    }
    (
        format!("{tail}; {}", clear_unverified_codex_stamp(root)),
        false,
    )
}

/// After the codex probe ran, a slot a live codex now holds is not rolled
/// back under it.
fn rollback_after_codex_probe(
    target_id: &str,
    rollback_blob: Option<&str>,
    root: &Path,
    host: &dyn Host,
) -> (String, bool) {
    let pins = store::pinning_sessions_for("codex", host).unwrap_or_default();
    if !pins.is_empty() {
        return (
            format!(
                "rollback withheld because the slot is pinned by a live codex session ({}); \
                 slot may hold '{target_id}'; {}",
                pid_names(&pins),
                clear_unverified_codex_stamp(root)
            ),
            false,
        );
    }
    rollback_codex_slot(rollback_blob, root, host)
}

/// Re-snapshot the outgoing account from the slot before it is overwritten.
/// claude goes through the vault, which proves whose credential it is first.
fn capture_outgoing(
    outgoing: &Record,
    root: &Path,
    host: &dyn Host,
    outside: &dyn Outside,
) -> Result<(), SwitchError> {
    if outgoing.harness == "claude" {
        let verdict = outside
            .vault("sync", root, None)
            .map_err(SwitchError::Store)?;
        if verdict != "written" && verdict != "unchanged" {
            return Err(SwitchError::Store(
                "Claude vault sync refused before overwrite. The slot was not touched".to_string(),
            ));
        }
        return Ok(());
    }
    let blobs: Vec<String> = store::read_slot_blob(&outgoing.harness, None, host)
        .map_err(store_err)?
        .filter(|b| !b.trim().is_empty())
        .into_iter()
        .collect();
    if blobs.len() != 1 {
        return Ok(());
    }
    store::write_snapshot(
        root,
        &outgoing.id,
        &outgoing.harness,
        &outgoing.account_id,
        &blobs[0],
        outgoing.harness == "claude" && host.keychain_slot(),
        outside.now(),
    )
    .map_err(store_err)?;
    Ok(())
}

/// `managed.switch`: materialize `target` into its CLI's slot under the
/// switch mutex, rolling back on a failed verification. `records` is the
/// merged set the outgoing stamp is looked up in.
pub fn switch(
    target: &Record,
    records: &[Record],
    root: &Path,
    policy: PinPolicy,
    host: &dyn Host,
    outside: &dyn Outside,
) -> Result<SwitchResult, SwitchError> {
    std::fs::create_dir_all(root).map_err(|e| SwitchError::Store(e.to_string()))?;
    let Some(_lock) = store::lock_switch(root, outside.lock_timeout()) else {
        return Err(SwitchError::Deferred(
            "another switch is in progress (mutex held); try again".to_string(),
        ));
    };
    switch_locked(target, records, root, policy, host, outside)
}

fn switch_locked(
    target: &Record,
    records: &[Record],
    root: &Path,
    policy: PinPolicy,
    host: &dyn Host,
    outside: &dyn Outside,
) -> Result<SwitchResult, SwitchError> {
    let id = target.id.as_str();
    let harness = target.harness.as_str();
    if harness == "claude" {
        let verdict = outside
            .vault("refresh", root, Some(id))
            .map_err(SwitchError::Store)?;
        if verdict == "dead" {
            return Err(SwitchError::NeedsLogin(format!(
                "stored credential for '{id}' is spent (invalid_grant); the slot was not touched"
            )));
        }
    }
    let stored = store::blob_path(root, id);
    let target_blob = std::fs::read_to_string(&stored).map_err(|_| {
        SwitchError::NeedsLogin(format!(
            "no credential snapshot for '{id}' at {}",
            stored.display()
        ))
    })?;
    if target_blob.trim().is_empty() {
        return Err(SwitchError::Store(format!(
            "credential snapshot for '{id}' is empty; refusing to materialize"
        )));
    }

    let outgoing_id = crate::accounts::slot::active_slot_id(harness, Some(root), root)
        .map_err(|e| SwitchError::Store(e.to_string()))?;
    // The stamp alone is not proof: a slot changed out of band re-materializes.
    if outgoing_id.as_deref() == Some(id)
        && store::verify_slot(harness, &target_blob, host).map_err(store_err)?
    {
        return Ok(SwitchResult {
            slot_changed: false,
            reason: (harness == "codex").then(|| "slot-already-active".to_string()),
            ..SwitchResult::landed(id)
        });
    }

    // Codex always defers: its post-write re-scan assumes an unpinned slot.
    let pins = store::pinning_sessions_for(harness, host).map_err(store_err)?;
    let mut pinned_by = Vec::new();
    if !pins.is_empty() {
        if policy == PinPolicy::Defer || harness == "codex" {
            return Err(SwitchError::Deferred(format!(
                "slot is pinned by a live {harness} session ({}); stop it or retry",
                pid_names(&pins)
            )));
        }
        pinned_by = pins.iter().map(|p| p.pid).collect();
    }

    // A tainted stamp means the slot may hold another account by now, so
    // capturing it would poison the stamped account's snapshot.
    let rollback_blob = store::read_slot_blob(harness, None, host).map_err(store_err)?;
    if let Some(outgoing) = outgoing_id
        .as_deref()
        .and_then(|out| records.iter().find(|r| r.id == out))
    {
        if !store::slot_tainted(root, harness) {
            capture_outgoing(outgoing, root, host, outside)?;
        }
    }

    store::write_slot_blob(harness, &target_blob, None, host).map_err(store_err)?;

    if !store::verify_slot(harness, &target_blob, host).map_err(store_err)? {
        let (tail, _) = if harness == "codex" {
            rollback_codex_slot(rollback_blob.as_deref(), root, host)
        } else {
            rollback_materialized_slot(harness, rollback_blob.as_deref(), host)
        };
        return Err(SwitchError::Store(format!(
            "switch to '{id}' failed verification (stored token may be stale/revoked); {tail}"
        )));
    }

    // A codex launched between the pin gate and the write read the outgoing
    // credential at startup; re-scan before the probe widens that window.
    if harness == "codex" {
        let late = store::pinning_sessions_for(harness, host).map_err(store_err)?;
        if !late.is_empty() {
            let names = pid_names(&late);
            let (tail, rolled_back) = rollback_codex_slot(rollback_blob.as_deref(), root, host);
            if !rolled_back {
                if rollback_blob.as_deref().is_some_and(|b| !b.is_empty()) {
                    return Err(SwitchError::Store(format!(
                        "a live {harness} session started mid-switch; {tail}; \
                         slot may hold '{id}' under a live session"
                    )));
                }
                return Err(SwitchError::Deferred(format!(
                    "a live {harness} session ({names}) started during the switch \
                     ({tail}); retry once it exits"
                )));
            }
            return Err(SwitchError::Deferred(format!(
                "a live {harness} session ({names}) started during the switch; \
                 slot rolled back to the previous account, retry once it exits"
            )));
        }
    }

    let mut verification = "structural";
    let mut verification_reason: Option<String> = None;
    if harness == "codex" {
        let (interrupted, probe) = interruptible(|| store::codex_login_ok(host));
        if interrupted {
            let (tail, _) = rollback_after_codex_probe(id, rollback_blob.as_deref(), root, host);
            return Err(SwitchError::Interrupted(format!(
                "codex login verification interrupted; {tail}"
            )));
        }
        let login = match probe {
            Ok(login) => login,
            Err(e) => {
                let (tail, _) =
                    rollback_after_codex_probe(id, rollback_blob.as_deref(), root, host);
                return Err(SwitchError::Store(format!(
                    "codex login verification failed ({}); {tail}",
                    e.message()
                )));
            }
        };
        match login.ok {
            Some(false) => {
                let (tail, _) =
                    rollback_after_codex_probe(id, rollback_blob.as_deref(), root, host);
                return Err(SwitchError::Store(format!(
                    "switch to '{id}' was not recognized by `codex login status`; {tail}"
                )));
            }
            Some(true) => verification = "codex-recognized",
            None => verification_reason = login.reason.map(str::to_string),
        }
    }

    // A kill between the slot write and this stamp leaves the stamp on the
    // previous account; the next successful switch corrects it.
    let io = |e: std::io::Error| SwitchError::Store(e.to_string());
    store::stamp_active_slot(root, harness, id).map_err(io)?;
    let writers: Vec<TaintWriter> = pins
        .iter()
        .map(|p| TaintWriter::Sampled(p.pid, p.started))
        .collect();
    store::set_slot_taint(root, harness, !pinned_by.is_empty(), &writers, host).map_err(io)?;
    store::clear_slot_principal_cache(root, harness);
    // No principal is bound here: a snapshot's provenance is the store, and
    // only `register` asserts who the signed-in account is.
    let mut event = Map::new();
    event.insert("provider".into(), json!(id));
    let account_id = if target.account_id.is_empty() {
        id
    } else {
        target.account_id.as_str()
    };
    event.insert("account_id".into(), json!(account_id));
    event.insert(
        "outgoing".into(),
        json!(outgoing_id.as_deref().unwrap_or("")),
    );
    if harness == "codex" {
        event.insert("slot_changed".into(), json!(true));
        event.insert("verification".into(), json!(verification));
        if let Some(reason) = &verification_reason {
            event.insert("reason".into(), json!(reason));
        }
    }
    outside.emit(event);
    Ok(SwitchResult {
        verification,
        reason: verification_reason,
        pinned_by,
        ..SwitchResult::landed(id)
    })
}

/// The merged records and the one `id` names, refused with the stderr line
/// when it is unknown or lives in its own config dir.
fn lookup(
    id: &str,
    repo_root: Option<&Path>,
    cwd: &Path,
    run: &mut Run,
) -> Result<(Vec<Record>, Record), String> {
    if records::candidates(repo_root, cwd)
        .iter()
        .any(|path| records::reads_legacy_block(&records::read_layer(path)))
    {
        run.err(
            "config.providers is the pre-rename name for config.accounts; the next account write migrates it",
        );
    }
    let accounts = records::load(repo_root, cwd).map_err(|e| format!("error: {e}"))?;
    let Some(record) = accounts.by_id(id).cloned() else {
        return Err(format!("error: account '{id}' not found"));
    };
    // A config-dir account has no shared-slot snapshot to materialize.
    if let Some(dir) = &record.config_dir {
        return Err(format!(
            "error: '{id}' is a config-dir account (its own {}); it is spawn-only. Use it \
             per-worker with `fno agents spawn --account {id}`, not a daemon-wide switch.",
            dir.display()
        ));
    }
    Ok((accounts.records, record))
}

fn print_switched(record: &Record, result: &SwitchResult, run: &mut Run) {
    let active = &result.active;
    if record.harness == "codex" {
        let reason = result.reason.as_deref().unwrap_or("None");
        if !result.slot_changed {
            run.out(&format!(
                "Managed Codex account '{active}' is already materialized (structural; {reason})."
            ));
        } else if result.verification == "codex-recognized" {
            run.out(&format!(
                "Materialized managed Codex account '{active}' into the slot; Codex recognized the local auth."
            ));
        } else {
            run.out(&format!(
                "Materialized managed Codex account '{active}' into the slot (structural fallback: {reason})."
            ));
        }
    } else {
        run.out(&format!(
            "Materialized managed account '{active}' into the slot (verified)."
        ));
    }
    if !result.pinned_by.is_empty() {
        run.err(&format!(
            "warning: swapped under {} live {} session(s); they keep their in-memory auth, and \
             a token refresh from one may overwrite the slot",
            result.pinned_by.len(),
            record.harness
        ));
    }
}

/// The `use` command: the checks, the switch for a managed record, then the
/// pointer write in `scope_word`'s own file. Every check runs before the
/// switch, because a materialized slot cannot be taken back.
pub fn use_account(
    id: &str,
    scope_word: &str,
    repo_root: Option<&Path>,
    cwd: &Path,
    policy: PinPolicy,
    host: &dyn Host,
    outside: &dyn Outside,
) -> Run {
    let mut run = Run::default();
    let (merged, record) = match lookup(id, repo_root, cwd, &mut run) {
        Ok(found) => found,
        Err(line) => return run.fail(&line),
    };
    let scope = if scope_word == "project" {
        Scope::Project
    } else {
        Scope::Global
    };
    let scope_records = match records::load_scope(scope, cwd) {
        Ok(accounts) => accounts.records,
        Err(e) => return run.fail(&format!("error loading existing config: {e}")),
    };
    // A global pointer must name a globally declared record, or every project
    // without that record bricks its loader.
    if scope == Scope::Global && !scope_records.iter().any(|r| r.id == id) {
        return run.fail(&format!(
            "error: account '{id}' is not declared in the global config; declare it there first \
             or set the pointer per project."
        ));
    }

    let managed = record.auth == "managed";
    if managed {
        let root = crate::accounts::slot::store_root(cwd).unwrap_or_default();
        let result = match switch(&record, &merged, &root, policy, host, outside) {
            Ok(result) => result,
            Err(SwitchError::Deferred(msg)) => {
                run.err(&format!("switch deferred: {msg}"));
                run.code = 2;
                return run;
            }
            Err(SwitchError::NeedsLogin(msg)) => {
                if !outside.terminal() {
                    return run.fail(&format!(
                        "{msg} Run in a terminal: fno config accounts use {id}"
                    ));
                }
                if outside.login(&root, id) != 0 {
                    run.code = 1;
                    return run;
                }
                SwitchResult::landed(id)
            }
            Err(SwitchError::Store(msg)) => return run.fail(&format!("switch failed: {msg}")),
            Err(SwitchError::Interrupted(note)) => {
                run.err(&format!("switch interrupted: {note}"));
                run.code = 130;
                return run;
            }
        };
        print_switched(&record, &result, &mut run);
    }

    let Some(target) = records::scope_path(scope, cwd) else {
        return run.fail("error: no HOME to resolve the global config.toml from");
    };
    let written = writer::mutate(&target, |doc| -> Result<(), String> {
        let data: toml::Table = doc
            .to_string()
            .parse()
            .map_err(|e: toml::de::Error| e.to_string())?;
        let config = records::scope_accounts(&data).map_err(|e| e.to_string())?;
        writer::set_records(doc, &config.records, Some(id));
        Ok(())
    });
    match written {
        Ok(Ok(())) => {}
        Ok(Err(msg)) | Err(writer::WriteError::Unreadable(msg)) => {
            return run.fail(&format!("error: {msg}"))
        }
        // The slot and its stamp already switched; only the pointer failed.
        Err(writer::WriteError::Io(e)) if managed => {
            return run.fail(&format!(
                "warning: slot switched to '{id}' but saving the active routing pointer failed \
                 ({e}); re-run `fno config accounts use {id}` to persist it."
            ))
        }
        Err(writer::WriteError::Io(e)) => {
            return run.fail(&format!("error: failed to write settings.yaml: {e}"))
        }
    }
    run.out(&format!(
        "Active account set to '{id}' (scope={scope_word})."
    ));
    run
}

fn parse(args: &[String]) -> Result<Option<(Option<String>, Option<String>)>, String> {
    let mut id = None;
    let mut scope = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = split_flag(arg);
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--scope" => {
                let value = inline.or_else(|| it.next().cloned());
                scope = Some(value.ok_or("error: option '--scope' requires an argument")?);
            }
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("error: no such option: {f}"))
            }
            _ if id.is_none() => id = Some(arg.clone()),
            _ => return Err(format!("error: unexpected extra argument ({arg})")),
        }
    }
    Ok(Some((id, scope)))
}

/// The `use` command over injected seams: the parity test's entry point.
pub fn run_with(args: &[String], cwd: &Path, host: &dyn Host, outside: &dyn Outside) -> Run {
    let mut run = Run::default();
    let (id, scope) = match parse(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            run.out(HELP);
            return run;
        }
        Err(msg) => {
            run.err(&msg);
            run.code = 2;
            return run;
        }
    };
    let Some(id) = id else {
        run.err("error: missing argument 'PROVIDER_ID'");
        run.code = 2;
        return run;
    };
    let scope_word = scope.unwrap_or_else(|| "project".to_string());
    use_account(
        &id,
        &scope_word,
        Some(cwd),
        cwd,
        PinPolicy::Warn,
        host,
        outside,
    )
}

pub fn run(args: &[String]) -> i32 {
    let cwd = pwd();
    let outside = SystemOutside {
        cwd: cwd.clone(),
        interactive: true,
    };
    let out = run_with(args, &cwd, &store::SystemHost, &outside);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    out.code
}

/// The `switch` op over injected seams: `payload` is `{"id", "repo_root"}`.
/// A live pin defers and no pointer is written, so the sweep never moves
/// routing it did not decide.
pub fn run_switch_with(payload: &Value, cwd: &Path, host: &dyn Host, outside: &dyn Outside) -> Run {
    let mut run = Run::default();
    let Some(id) = payload.get("id").and_then(Value::as_str) else {
        run.err("accounts switch: the payload names no id");
        run.code = 2;
        return run;
    };
    let repo_root = payload
        .get("repo_root")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .map(PathBuf::from);
    let (merged, record) = match lookup(id, repo_root.as_deref(), cwd, &mut run) {
        Ok(found) => found,
        Err(line) => return run.fail(&line),
    };
    if record.auth != "managed" {
        return run.fail(&format!("error: account '{id}' is not a managed account"));
    }
    let root = crate::accounts::slot::store_root(cwd).unwrap_or_default();
    match switch(&record, &merged, &root, PinPolicy::Defer, host, outside) {
        Ok(result) => {
            run.out(
                &json!({
                    "active": result.active,
                    "slot_changed": result.slot_changed,
                    "verification": result.verification,
                    "reason": result.reason,
                })
                .to_string(),
            );
            run
        }
        Err(SwitchError::Deferred(msg)) => {
            run.err(&format!("switch deferred: {msg}"));
            run.code = 2;
            run
        }
        Err(
            SwitchError::NeedsLogin(msg) | SwitchError::Store(msg) | SwitchError::Interrupted(msg),
        ) => run.fail(&format!("switch failed: {msg}")),
    }
}

pub fn run_switch(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{SWITCH_USAGE}");
        return 0;
    }
    let mut input = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input) {
        eprintln!("accounts switch: stdin unreadable: {e}");
        return 2;
    }
    let payload: Value = match serde_json::from_str(&input) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            eprintln!("accounts switch: the payload is not a JSON object");
            return 2;
        }
    };
    let cwd = pwd();
    let outside = SystemOutside {
        cwd: cwd.clone(),
        interactive: false,
    };
    let out = run_switch_with(&payload, &cwd, &store::SystemHost, &outside);
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    out.code
}

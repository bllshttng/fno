//! `fno config accounts register <id>`: capture the shared slot into a new
//! managed account and bind the identity of exactly those bytes, or, with
//! `--config-dir`, record a claude account that lives in its own dir.
//!
//! One locked capture serves the proof, the snapshot and the binding: proving
//! one read and storing another is how account A's identity ends up bound to
//! account B's credential. The config save runs inside the lock and before
//! any store write, so a failed save leaves no residue a later registration
//! would read as a duplicate credential.

use crate::accounts::add::{split_flag, upsert_record, UpsertError};
use crate::accounts::binding::{self, SlotProbe};
use crate::accounts::reconcile::{principal_json, slot_switch_remedy};
use crate::accounts::records::{self, Record, Scope};
use crate::accounts::show::pwd;
use crate::accounts::store::{self, Host};
use crate::claude_vault::Principal;
use std::path::{Path, PathBuf};
use std::time::Duration;

const HELP: &str = "Usage: fno config accounts register [OPTIONS] PROVIDER_ID

  Register a managed (shared-slot) or config-dir account.

  Default (no --config-dir): snapshot the CURRENT ~/.claude login into a
  managed account store. Sign into the account first (`claude /login`), then
  run this; it captures the live credential into ~/.fno/providers/<id>/ and
  writes an `auth: managed` record.

  With --config-dir <path>: register an account that lives in its OWN config
  dir (a full second login). No shared-slot snapshot - the dir IS the account.

  -H, --harness TEXT      claude|codex  [default: claude]
  -p, --priority INTEGER  [default: 100]
      --name TEXT
      --config-dir TEXT   Register a per-account CLAUDE_CONFIG_DIR account
                          instead of a shared-slot managed snapshot.
  -s, --scope TEXT        global|project  [default: global]";

#[derive(Default)]
struct Args {
    id: Option<String>,
    harness: Option<String>,
    priority: Option<String>,
    name: Option<String>,
    config_dir: Option<String>,
    scope: Option<String>,
}

/// Click's parse: `--opt value`, `--opt=value`, `-X value`, `-Xvalue`.
fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = split_flag(arg);
        let slot = match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "-H" | "--harness" => &mut out.harness,
            "-p" | "--priority" => &mut out.priority,
            "--name" => &mut out.name,
            "--config-dir" => &mut out.config_dir,
            "-s" | "--scope" => &mut out.scope,
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("error: no such option: {f}"))
            }
            _ if out.id.is_none() => {
                out.id = Some(arg.clone());
                continue;
            }
            _ => return Err(format!("error: unexpected extra argument ({arg})")),
        };
        let value = inline.or_else(|| it.next().cloned());
        *slot = Some(value.ok_or_else(|| format!("error: option '{flag}' requires an argument"))?);
    }
    Ok(Some(out))
}

/// What one run printed and its exit code.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    fn out(&mut self, line: &str) {
        self.stdout.push_str(line);
        self.stdout.push('\n');
    }

    fn err(&mut self, line: &str) {
        self.stderr.push_str(line);
        self.stderr.push('\n');
    }

    fn fail(mut self, line: &str) -> Run {
        self.err(line);
        self.code = 1;
        self
    }
}

/// The capture registered (`moved` when the slot changed during the
/// writes, leaving the stamp tainted), or refused with its stderr text.
enum Capture {
    Registered { adir: PathBuf, moved: bool },
    Refused(String),
}

fn expand_user(raw: &str) -> PathBuf {
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    match raw.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(&rest[1..]),
        _ => PathBuf::from(raw),
    }
}

fn save_error(e: UpsertError) -> String {
    match e {
        UpsertError::Load(msg) | UpsertError::Unreadable(msg) => {
            format!("error loading existing config: {msg}")
        }
        UpsertError::Exists => unreachable!("register replaces its own id"),
        UpsertError::Io(e) => format!("error: failed to write config: {e}"),
    }
}

/// `principal_holder`: another store record already bound to `identity`.
/// Tokens rotate, so the digest check misses a re-registered account;
/// principals do not.
fn principal_holder(identity: &str, exclude_id: &str, root: &Path) -> Option<String> {
    let mut entries: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    entries.sort();
    entries.into_iter().find(|name| {
        name != exclude_id
            && binding::record_principal(name, root).map(|p| binding::identity_key(&p))
                == Some(identity.to_string())
    })
}

fn read_slot(harness: &str, probe: &dyn SlotProbe, host: &dyn Host) -> Result<Vec<String>, String> {
    if harness == "claude" {
        return binding::slot_blobs(None, probe)
            .map_err(|()| "the claude slot credential could not be read".to_string());
    }
    Ok(store::read_slot_blob(harness, None, host)
        .map_err(|e| e.message().to_string())?
        .filter(|b| !b.trim().is_empty())
        .into_iter()
        .collect())
}

/// `managed.register_slot_snapshot`, under the switch flock.
#[allow(clippy::too_many_arguments)]
fn capture(
    record: &Record,
    target: &Path,
    root: &Path,
    probe: &dyn SlotProbe,
    host: &dyn Host,
    lock_timeout: Duration,
    now: f64,
    run: &mut Run,
) -> Result<Capture, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let Some(_lock) = store::lock_switch(root, lock_timeout) else {
        return Err("another slot transition is in progress; try again".to_string());
    };
    let harness = record.harness.as_str();
    let blobs = read_slot(harness, probe, host)?;
    if blobs.is_empty() {
        return Err(format!(
            "no current {harness} login to snapshot for '{}' (sign in first, then register)",
            record.id
        ));
    }
    // Only claude has a principal endpoint; any other proof failure still
    // registers, unbound, so registration works offline.
    let principal: Option<Principal> = if harness == "claude" {
        match binding::principal_of_blobs(&blobs, probe) {
            Ok(p) => Some(p),
            Err("ambiguous-slot") => {
                return Ok(Capture::Refused(format!(
                    "error: the {harness} slot currently holds credentials for two different \
                     accounts, so registering '{}' could bind the wrong one.\n  {}, then re-run \
                     this command",
                    record.id,
                    slot_switch_remedy(&record.id)
                )))
            }
            Err(_) => None,
        }
    } else {
        None
    };
    let blockers = store::taint_writers_still_live(root, harness, host)
        .map_err(|e| e.message().to_string())?;
    if !blockers.is_empty() {
        return Ok(Capture::Refused(format!(
            "error: {} was pinning the {harness} slot when it was tainted and can still \
             overwrite it, so registering '{}' now could stamp a credential that is about to \
             change.\n  stop it and re-run this command",
            blockers.join(", "),
            record.id
        )));
    }
    if let Some(holder) = principal
        .as_ref()
        .and_then(|p| principal_holder(&binding::identity_key(p), &record.id, root))
    {
        return Ok(Capture::Refused(format!(
            "error: that account is already registered as '{holder}'.\n  a rotated token hides \
             this from the credential check, but the proven identity is the same, so two ids \
             would share one quota pool and reconciliation could never tell them apart.\n  sign \
             into the {} account first (no /logout first):  {harness} /login",
            record.id
        )));
    }
    let blob = &blobs[0];
    if let Some(holder) = store::duplicate_credential_holder(root, blob, &record.id) {
        return Ok(Capture::Refused(format!(
            "error: the current {harness} login is already registered as '{holder}'; \
             registering it again as '{id}' would store one credential under two ids.\n  sign \
             into the {id} account first (no /logout first):  {harness} /login\n  or give it its \
             own dir:  fno config accounts register {id} --config-dir ~/.claude-{id}",
            id = record.id
        )));
    }
    // The profile call is a round trip; commit only if the slot still holds
    // the bytes that were proven.
    if read_slot(harness, probe, host)? != blobs {
        return Ok(Capture::Refused(format!(
            "error: the {harness} slot changed while '{}' was being registered (an out-of-band \
             /login), so nothing was written.\n  re-run this command once the slot settles",
            record.id
        )));
    }
    let saved = upsert_record(target, record.clone(), false, &mut |m: &str| run.err(m));
    if let Err(e) = saved {
        return Ok(Capture::Refused(save_error(e)));
    }
    // Taint before the writes: a crash anywhere after this leaves the stamp
    // untrusted, the safe direction to fail.
    let io = |e: std::io::Error| e.to_string();
    store::set_slot_taint(root, harness, true, &[], host).map_err(io)?;
    let adir = store::write_snapshot(
        root,
        &record.id,
        harness,
        &record.account_id,
        blob,
        harness == "claude" && host.keychain_slot(),
        now,
    )
    .map_err(|e| e.message().to_string())?;
    match &principal {
        Some(p) => {
            store::write_record_principal(root, &record.id, &principal_json(p), now).map_err(io)?
        }
        None => store::clear_record_principal(root, &record.id),
    }
    store::stamp_active_slot(root, harness, &record.id).map_err(io)?;
    // A look that could not be taken counts as unsettled.
    if read_slot(harness, probe, host).ok().as_ref() != Some(&blobs) {
        return Ok(Capture::Registered { adir, moved: true });
    }
    store::set_slot_taint(root, harness, false, &[], host).map_err(io)?;
    Ok(Capture::Registered { adir, moved: false })
}

fn register_config_dir(
    fields: toml::Table,
    raw: &str,
    target: &Path,
    scope: &str,
    mut run: Run,
) -> Run {
    if fields.get("harness").and_then(toml::Value::as_str) != Some("claude") {
        return run.fail("error: --config-dir accounts are claude-only");
    }
    let cfg = expand_user(raw);
    if !cfg.is_absolute() {
        return run.fail(&format!(
            "error: --config-dir must be absolute (got '{raw}')"
        ));
    }
    if !cfg.exists() {
        return run.fail(&format!(
            "error: config dir {} does not exist; sign in there first (CLAUDE_CONFIG_DIR={} claude /login)",
            cfg.display(),
            cfg.display()
        ));
    }
    let mut fields = fields;
    fields.insert(
        "config_dir".to_string(),
        toml::Value::String(cfg.display().to_string()),
    );
    let record = match records::build_record(&fields) {
        Ok(record) => record,
        Err(msg) => return run.fail(&format!("error: {msg}")),
    };
    let id = record.id.clone();
    let saved = upsert_record(target, record, false, &mut |m: &str| run.err(m));
    if let Err(e) = saved {
        return run.fail(&save_error(e));
    }
    run.out(&format!(
        "Registered config-dir account '{id}' (CLAUDE_CONFIG_DIR={}, scope={scope}). \
         `fno agents spawn --account {id}` now bills it.",
        cfg.display()
    ));
    run
}

/// The command over injected seams: the parity test's entry point.
pub fn run_with(
    args: &[String],
    cwd: &Path,
    probe: &dyn SlotProbe,
    host: &dyn Host,
    lock_timeout: Duration,
    now: f64,
) -> Run {
    let mut run = Run::default();
    let args = match parse(args) {
        Ok(Some(args)) => args,
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
    let Some(id) = args.id else {
        run.err("error: missing argument 'PROVIDER_ID'");
        run.code = 2;
        return run;
    };
    let priority = match args
        .priority
        .as_deref()
        .map(str::trim)
        .map(str::parse::<i64>)
    {
        None => 100,
        Some(Ok(p)) => p,
        Some(Err(_)) => {
            run.err(&format!(
                "error: Invalid value for '--priority' / '-p': '{}' is not a valid integer.",
                args.priority.unwrap_or_default()
            ));
            run.code = 2;
            return run;
        }
    };
    let scope_word = args.scope.unwrap_or_else(|| "global".to_string());
    let scope = if scope_word == "project" {
        Scope::Project
    } else {
        Scope::Global
    };
    let mut fields = toml::Table::new();
    let name = args
        .name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.clone());
    for (key, value) in [
        ("id", id),
        ("name", name),
        (
            "harness",
            args.harness.unwrap_or_else(|| "claude".to_string()),
        ),
        ("auth", "managed".to_string()),
    ] {
        fields.insert(key.to_string(), toml::Value::String(value));
    }
    fields.insert("priority".to_string(), toml::Value::Integer(priority));
    let Some(target) = records::scope_path(scope, cwd) else {
        return run.fail("error: no HOME to resolve the global config.toml from");
    };
    if let Some(raw) = args.config_dir {
        return register_config_dir(fields, &raw, &target, &scope_word, run);
    }
    let record = match records::build_record(&fields) {
        Ok(record) => record,
        Err(msg) => return run.fail(&format!("error: {msg}")),
    };
    let root = crate::accounts::slot::store_root(cwd).unwrap_or_default();
    match capture(
        &record,
        &target,
        &root,
        probe,
        host,
        lock_timeout,
        now,
        &mut run,
    ) {
        Err(msg) => run.fail(&format!("error: {msg}")),
        Ok(Capture::Refused(msg)) => run.fail(&msg),
        Ok(Capture::Registered { adir, moved }) => {
            run.out(&format!(
                "Registered managed account '{}' (snapshot at {}, scope={scope_word}).",
                record.id,
                adir.display()
            ));
            if moved {
                run.err(&format!(
                    "warning: the {h} slot changed while registering, so its active stamp is \
                     marked tainted rather than trusted; run `fno config accounts reconcile-slot \
                     {h}` once it settles",
                    h = record.harness
                ));
            }
            run
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let out = run_with(
        args,
        &pwd(),
        &binding::SystemProbe,
        &store::SystemHost,
        Duration::from_secs(10),
        now,
    );
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    out.code
}

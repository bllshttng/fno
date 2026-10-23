//! `fno config accounts reconcile-slot [<harness>]`: prove who holds a CLI's
//! shared credential slot through its profile endpoint and repair the managed
//! store to match (`managed.reconcile_slot`). Only a unique match writes; every
//! other outcome leaves taint, stamp and snapshots as they were.

use crate::accounts::binding::{self, SlotProbe};
use crate::accounts::records::{self, Record};
use crate::accounts::show::pwd;
use crate::accounts::store::{self, Host, StoreError};
use crate::claude_vault::Principal;
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `managed.ReconcileResult`: `outcome` is the typed reason; anything other
/// than `matched` wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub outcome: String,
    pub record_id: Option<String>,
    pub detail: String,
}

impl Outcome {
    fn refused(outcome: &str, detail: impl Into<String>) -> Self {
        Outcome {
            outcome: outcome.to_string(),
            record_id: None,
            detail: detail.into(),
        }
    }

    pub fn ok(&self) -> bool {
        self.outcome == "matched"
    }
}

/// `managed.slot_switch_remedy`: the hand path that moves every claude
/// session on the shared slot to `account`.
pub fn slot_switch_remedy(account: &str) -> String {
    let slot = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude");
    let service = binding::keychain_service(&slot).unwrap_or_default();
    format!(
        "with no `/logout` first, sign in as {account} with `claude /login` (interactive \
         sessions) and with `CLAUDE_CONFIG_DIR={} claude /login` (background sessions, \
         which read '{service}'); blocked sessions recover on their next request",
        slot.display()
    )
}

/// The switch mutex `managed.switch` holds: an exclusive flock on
/// `<store>/.switch.lock`, polled until `timeout`.
fn lock_switch(root: &Path, timeout: Duration) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(store::switch_lock_path(root))
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: flock on a descriptor this function owns; dropping the
        // returned file releases it.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `managed.reconcile_slot`. The slot is read inside the switch mutex, so a
/// concurrent switch can never make the observation stale before the write.
pub fn reconcile_slot(
    cli: &str,
    records: &[Record],
    root: &Path,
    probe: &dyn SlotProbe,
    host: &dyn Host,
    lock_timeout: Duration,
    now: f64,
) -> Outcome {
    if cli != "claude" {
        return Outcome::refused(
            "unsupported-harness",
            format!(
                "'{cli}' has no principal endpoint to prove slot identity with; \
                 only claude slots can be reconciled"
            ),
        );
    }
    if !root.is_dir() {
        return Outcome::refused(
            "no-managed-store",
            format!(
                "no managed store at {}; register an account while it is signed in \
                 before there is anything to reconcile against",
                root.display()
            ),
        );
    }
    let Some(_lock) = lock_switch(root, lock_timeout) else {
        return Outcome::refused(
            "lock-timeout",
            "another slot transition is in progress; try again",
        );
    };
    reconcile_locked(cli, records, root, probe, host, now)
        .unwrap_or_else(|why| Outcome::refused("slot-unreadable", why))
}

fn slot_pinned(blockers: &[String]) -> Outcome {
    Outcome::refused(
        "slot-pinned",
        format!(
            "{} was pinning the slot when it was tainted and can still write the \
             previous account's refreshed credential, so proving the identity now \
             would not keep it true; stop it and retry",
            blockers.join(", ")
        ),
    )
}

fn read_slot(cli: &str, probe: &dyn SlotProbe) -> Result<Vec<String>, String> {
    binding::slot_blobs(None, probe)
        .map_err(|()| format!("the {cli} slot credential could not be read"))
}

fn store_err(e: StoreError) -> String {
    e.message().to_string()
}

fn reconcile_locked(
    cli: &str,
    records: &[Record],
    root: &Path,
    probe: &dyn SlotProbe,
    host: &dyn Host,
    now: f64,
) -> Result<Outcome, String> {
    // The pin gate runs before the profile round trip: a writer that rewrites
    // the slot and exits during that call would pass a later liveness check.
    let blockers = store::taint_writers_still_live(root, cli, host).map_err(store_err)?;
    if !blockers.is_empty() {
        return Ok(slot_pinned(&blockers));
    }
    let blobs = read_slot(cli, probe)?;
    if blobs.is_empty() {
        return Ok(Outcome::refused(
            "no-slot-credential",
            format!("no live {cli} login in the shared slot; sign in, then reconcile"),
        ));
    }
    // Prove the capture itself, never a second read of the slot.
    let principal = match binding::principal_of_blobs(&blobs, probe) {
        Ok(principal) => principal,
        Err("ambiguous-slot") => {
            return Ok(Outcome::refused(
                "ambiguous-slot",
                format!(
                    "the {cli} slot presents credentials belonging to different accounts; \
                     whichever was stamped, some reader would get the other - {}",
                    slot_switch_remedy("the account to keep")
                ),
            ))
        }
        Err(failure) => {
            return Ok(Outcome::refused(
                failure,
                "could not prove who the live slot credential belongs to; \
                 taint, stamp and snapshots are unchanged",
            ))
        }
    };
    let blob = &blobs[0];
    let wanted = binding::identity_key(&principal);

    // A record with its own config_dir is attributable without the shared
    // slot, so it cannot claim the slot's identity.
    let mut candidates: Vec<&Record> = records
        .iter()
        .filter(|r| r.harness == cli && r.auth == "managed" && r.config_dir.is_none())
        .collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let matches: Vec<&Record> = candidates
        .into_iter()
        .filter(|r| {
            binding::record_principal(&r.id, root).map(|p| binding::identity_key(&p))
                == Some(wanted.clone())
        })
        .collect();
    let who = who(&principal);
    let matched = match matches.as_slice() {
        [] => {
            return Ok(Outcome::refused(
                "zero-match",
                format!(
                    "the live slot holds {who}, which matches no registered account's \
                     proven identity; sign that account in and run \
                     `fno config accounts register <id>` to bind it"
                ),
            ))
        }
        [one] => *one,
        many => {
            let ids: Vec<&str> = many.iter().map(|r| r.id.as_str()).collect();
            return Ok(Outcome::refused(
                "ambiguous-match",
                format!(
                    "the live slot holds {who}, which matches {} records ({}); one of them \
                     was registered while the other was signed in",
                    ids.len(),
                    ids.join(", ")
                ),
            ));
        }
    };

    // Commit only if the slot still holds the bytes that were proven.
    if read_slot(cli, probe)? != blobs {
        return Ok(Outcome::refused(
            "slot-changed",
            "the slot credential changed while its identity was being proven; \
             nothing was written, retry once it settles",
        ));
    }
    let blockers = store::taint_writers_still_live(root, cli, host).map_err(store_err)?;
    if !blockers.is_empty() {
        return Ok(slot_pinned(&blockers));
    }

    // Taint first, clear last: a crash anywhere between leaves the slot
    // tainted, the safe direction to fail.
    let io = |e: std::io::Error| e.to_string();
    store::set_slot_taint(root, cli, true, &[], host).map_err(io)?;
    let account_id = if matched.account_id.is_empty() {
        matched.id.as_str()
    } else {
        matched.account_id.as_str()
    };
    store::write_snapshot(
        root,
        &matched.id,
        &matched.harness,
        account_id,
        blob,
        matched.harness == "claude" && host.keychain_slot(),
        now,
    )
    .map_err(store_err)?;
    store::write_record_principal(root, &matched.id, &principal_json(&principal), now)
        .map_err(io)?;
    store::stamp_active_slot(root, cli, &matched.id).map_err(io)?;
    // Clearing the taint is what makes the stamp trusted, so it gets the last
    // look, and a look that could not be taken counts as unsettled.
    if read_slot(cli, probe).ok().as_ref() != Some(&blobs) {
        store::clear_slot_principal_cache(root, cli);
        return Ok(Outcome::refused(
            "slot-changed",
            format!(
                "'{}' was proven and its snapshot refreshed, but the slot changed again \
                 before the stamp could be trusted; the slot is marked tainted so nothing \
                 reads it - retry once it settles",
                matched.id
            ),
        ));
    }
    store::set_slot_taint(root, cli, false, &[], host).map_err(io)?;
    let _ = std::fs::remove_file(root.join(format!(".active-{cli}.reconcile-attempt")));
    binding::note_slot_principal(cli, root, &wanted, blob, now);
    Ok(Outcome {
        outcome: "matched".to_string(),
        record_id: Some(matched.id.clone()),
        detail: format!(
            "the live {cli} slot belongs to '{}' (proven by live profile); snapshot \
             refreshed, stamped active, taint cleared",
            matched.id
        ),
    })
}

fn who(p: &Principal) -> String {
    match p.email.as_deref().filter(|e| !e.is_empty()) {
        Some(email) => email.to_string(),
        None if !p.account_uuid.is_empty() => p.account_uuid.clone(),
        None => "an unknown account".to_string(),
    }
}

fn principal_json(p: &Principal) -> serde_json::Value {
    let mut out = json!({
        "account_uuid": p.account_uuid,
        "organization_uuid": p.organization_uuid,
    });
    if let Some(email) = p.email.as_deref().filter(|e| !e.is_empty()) {
        out["email"] = json!(email);
    }
    out
}

const HELP: &str = "Usage: fno config accounts reconcile-slot [HARNESS_NAME]

  Prove who holds a CLI's shared credential slot, and repair the store to match.

  It acts only when the live slot credential's principal binds to exactly one
  registered account: it refreshes that account's snapshot, stamps it active,
  and clears any taint. Anything less than proof writes nothing and exits
  non-zero naming which way it failed. HARNESS_NAME defaults to claude, the only
  CLI with a shared-slot principal endpoint.";

pub fn run(args: &[String]) -> i32 {
    let mut harness = None;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') && flag.len() > 1 => {
                eprintln!("error: no such option: {flag}");
                return 2;
            }
            word if harness.is_none() => harness = Some(word.to_string()),
            word => {
                eprintln!("error: unexpected extra argument ({word})");
                return 2;
            }
        }
    }
    let harness = harness.unwrap_or_else(|| "claude".to_string());
    let cwd = pwd();
    let accounts = match records::load(Some(cwd.as_path()), &cwd) {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let root = crate::accounts::slot::store_root(&cwd).unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let result = reconcile_slot(
        &harness,
        &accounts.records,
        &root,
        &binding::SystemProbe,
        &store::SystemHost,
        Duration::from_secs(10),
        now,
    );
    if result.ok() {
        println!("reconcile-slot: {}", result.detail);
        return 0;
    }
    eprintln!(
        "reconcile-slot: refused ({}): {}",
        result.outcome, result.detail
    );
    1
}

//! `update-journal`: the one Rust door for the `fno doctor update` lifecycle.
//!
//! Four event rows (started, built, installed, failed) append to the global
//! journal through `event_store::append_envelope`, and the installed and
//! failed rows mail the roles through the front binary's own mail command. A
//! failed row whose reason holds Python behind Rust is a skew: it pages the
//! leads once per source rev (urgently unless the hold is planned publish
//! lag), and the row records `paged`. The
//! Python side composes nothing: one argv per step, one exit code back, and
//! the fail trap passes `--rc` instead of a JSON template.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const MAIL_TIMEOUT: Duration = Duration::from_secs(60);

struct Args {
    events: PathBuf,
    type_name: String,
    fields: Map<String, Value>,
    mail_from: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut events = None;
    let mut type_name = None;
    let mut mail_from = None;
    let mut fields = Map::new();
    let known = [
        "new-rev",
        "old-rev",
        "source-path",
        "outcome",
        "rust-rev",
        "rc",
        "reason",
        "stage",
    ];
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let (name, field) = match flag.strip_prefix("--") {
            Some(n) => (n, n.replace('-', "_")),
            None => return Err(format!("expected a --flag, got {flag:?}")),
        };
        let value = args
            .get(i + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?;
        i += 2;
        match name {
            "events" => events = Some(PathBuf::from(value)),
            "type" => type_name = Some(value.clone()),
            "mail-from" => mail_from = Some(value.clone()),
            other if known.contains(&other) => {
                fields.insert(field, json!(value));
            }
            other => return Err(format!("unknown flag --{other}")),
        }
    }
    Ok(Args {
        events: events.ok_or("--events <events.jsonl> is required")?,
        type_name: type_name.ok_or("--type started|built|installed|failed is required")?,
        fields,
        mail_from,
    })
}

/// One bounded mail subprocess: the front binary's mail command is local, but a
/// wedged front door must not hang a failed install's exit past the minute
/// the old Python wrapper allowed. True only when the mail command exited 0.
fn mail_roles(fno_bin: &str, body: &str, urgent: bool) -> bool {
    let mut argv = vec![
        "agents",
        "mail",
        "admin",
        "team",
        "--scope",
        "leads",
        "--subject",
        "fno-update",
    ];
    if urgent {
        argv.push("--urgent");
    }
    argv.push(body);
    let child = Command::new(fno_bin)
        .args(argv)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fno doctor update: WARNING: role mail not sent: {e}");
            return false;
        }
    };
    let deadline = Instant::now() + MAIL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("fno doctor update: WARNING: role mail timed out");
                return false;
            }
        }
    }
}

/// The reason prefix of a hold row: the update left Python behind Rust.
const HOLD_PREFIX: &str = "python held";

/// True when an earlier failed row already paged the skew at `rev`. Every
/// merge re-runs the sync, and one page per source rev is the budget: the
/// user needs the page once, not on each retry.
fn skew_paged(events: &Path, rev: &str) -> bool {
    let q = crate::event_store::EventQuery::of_types(&["fno_update_failed"]);
    let Ok(rows) = crate::event_store::query_events(events, &q) else {
        return false;
    };
    rows.iter().any(|row| {
        let Ok(v) = serde_json::from_str::<Value>(&row.line) else {
            return false;
        };
        let data = &v["data"];
        data["paged"].as_str() == Some("true") && data["new_rev"].as_str() == Some(rev)
    })
}

pub fn run_update_journal(args: &[String]) -> i32 {
    let mut args = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fno-agents update-journal: {e}");
            return 2;
        }
    };
    if !matches!(
        args.type_name.as_str(),
        "started" | "built" | "installed" | "failed"
    ) {
        eprintln!("fno-agents update-journal: --type must be started, built, installed or failed");
        return 2;
    }
    // The failed row's reason is schema-required; the trap passes only --rc,
    // so the command derives the sentence the journal reader expects.
    if args.type_name == "failed" {
        args.fields
            .entry("stage".to_string())
            .or_insert_with(|| json!("install"));
        if !args.fields.contains_key("reason") {
            let rc = args
                .fields
                .get("rc")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            args.fields
                .insert("reason".to_string(), json!(format!("install exited {rc}")));
        }
    }
    let reason = args
        .fields
        .get("reason")
        .and_then(Value::as_str)
        .map(String::from);
    let skew = args.type_name == "failed"
        && reason
            .as_deref()
            .is_some_and(|r| r.starts_with(HOLD_PREFIX));
    let new_rev = args
        .fields
        .get("new_rev")
        .and_then(Value::as_str)
        .map(String::from);
    // A skew row with no source rev cannot dedupe, so it never pages.
    let page_skew = skew
        && args.mail_from.is_some()
        && new_rev
            .as_deref()
            .is_some_and(|rev| !skew_paged(&args.events, rev));
    // The page goes before the row, and the row records `paged` only on a
    // delivered page: a failed send must not suppress the next hold's page.
    if page_skew {
        let bin = args.mail_from.as_deref().unwrap_or_default();
        let rev = new_rev.as_deref().unwrap_or_default();
        let body = format!(
            "fno doctor update SKEW at {}: {}.",
            &rev[..rev.len().min(8)],
            reason.as_deref().unwrap_or_default()
        );
        // A planned hold (rc 0) is publish lag that the next update
        // clears; only an unplanned hold pages urgently.
        let planned = args.fields.get("rc").and_then(Value::as_str) == Some("0");
        if mail_roles(bin, &body, !planned) {
            args.fields.insert("paged".to_string(), json!("true"));
        }
    }
    let data: Value = Value::Object(args.fields.clone());
    let envelope = json!({
        "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "type": format!("fno_update_{}", args.type_name),
        "source": "python",
        "data": data,
    });
    let line = serde_json::to_string(&envelope).unwrap_or_default();
    if let Err(e) = crate::event_store::append_envelope(&args.events, &line, None) {
        let type_name = args.type_name.as_str();
        eprintln!("fno doctor update: WARNING: {type_name} event not journaled: {e}");
        return 1;
    }
    if skew {
        return 0;
    }
    if args.mail_from.is_some() && matches!(args.type_name.as_str(), "installed" | "failed") {
        let bin = args.mail_from.as_deref().unwrap_or_default();
        let body = match args.type_name.as_str() {
            "installed" => {
                let new_rev = args
                    .fields
                    .get("new_rev")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let was = args
                    .fields
                    .get("old_rev")
                    .and_then(Value::as_str)
                    .map(|o| format!(" (was {})", &o[..o.len().min(8)]))
                    .unwrap_or_default();
                format!(
                    "fno doctor update installed {}{was}.",
                    &new_rev[..new_rev.len().min(8)]
                )
            }
            _ => {
                let reason = args
                    .fields
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| {
                        let rc = args
                            .fields
                            .get("rc")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        format!("install exited {rc}")
                    });
                format!("fno doctor update FAILED: {reason}.")
            }
        };
        mail_roles(bin, &body, false);
    }
    0
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn held_row(events: &Path, bin: &Path, rev: &str) -> i32 {
        let args: Vec<String> = [
            "--events",
            events.to_str().unwrap(),
            "--type",
            "failed",
            "--rc",
            "1",
            "--new-rev",
            rev,
            "--reason",
            "python held: the rust bins did not reach the source rev (waiting-ci)",
            "--mail-from",
            bin.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        run_update_journal(&args)
    }

    #[test]
    fn skew_pages_once_per_rev() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("events.jsonl");
        let log = dir.path().join("mail.log");
        let bin = dir.path().join("mail-stub");
        let script = format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display());
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(held_row(&events, &bin, "aaaaaaaa1111"), 0);
        assert_eq!(held_row(&events, &bin, "aaaaaaaa1111"), 0);
        let sent = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            sent.lines().count(),
            1,
            "a second hold at one rev re-paged: {sent}"
        );
        assert!(sent.contains("admin team --scope leads"), "{sent}");
        assert!(sent.contains("--urgent"), "{sent}");
        assert!(sent.contains("SKEW at aaaaaaaa: python held"), "{sent}");
        assert!(sent.contains("waiting-ci"), "{sent}");

        assert_eq!(held_row(&events, &bin, "bbbbbbbb2222"), 0);
        let sent = std::fs::read_to_string(&log).unwrap();
        assert_eq!(sent.lines().count(), 2, "a new rev must page again: {sent}");

        let q = crate::event_store::EventQuery::of_types(&["fno_update_failed"]);
        let rows = crate::event_store::query_events(&events, &q).unwrap();
        let paged: Vec<bool> = rows
            .iter()
            .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
            .map(|v| v["data"]["paged"].as_str() == Some("true"))
            .collect();
        assert_eq!(paged, vec![true, false, true]);

        // A send that exits nonzero records no `paged`, so the next hold at
        // that rev pages again.
        let broken = dir.path().join("fno-broken");
        std::fs::write(&broken, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(held_row(&events, &broken, "cccccccc3333"), 0);
        assert_eq!(held_row(&events, &bin, "cccccccc3333"), 0);
        let sent = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            sent.lines().count(),
            3,
            "a failed send suppressed the page: {sent}"
        );
    }
}

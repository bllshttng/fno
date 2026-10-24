//! `fno config accounts doctor [--json]`: the managed store's real condition,
//! one finding per problem. A duplicate credential, a missing login, an
//! unproven identity and a tainted or drifted slot each surface downstream
//! only as `unknown`; this names them together. Read-only apart from the
//! proof cache the binding refreshes, and it never prints a secret.

use crate::accounts::binding::{self, BindingRequest, SlotProbe, AMBIGUOUS, UNKNOWN_RECEIPT};
use crate::accounts::reconcile::slot_switch_remedy;
use crate::accounts::records::{self, Record};
use crate::accounts::show::pwd;
use crate::accounts::{env, slot, store};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// One problem: `record` is a record id or `slot:<cli>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub record: String,
    pub problem: String,
    pub detail: String,
}

impl Finding {
    fn new(record: impl Into<String>, problem: &str, detail: String) -> Self {
        Finding {
            record: record.into(),
            problem: problem.to_string(),
            detail,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "record": self.record, "problem": self.problem, "detail": self.detail })
    }
}

/// `managed.record_principal` is truthy on `account_uuid` alone, so a half
/// principal still counts as bound here, as it does in Python.
fn principal_bound(root: &Path, record_id: &str) -> bool {
    store::read_meta(root, record_id)
        .and_then(|meta| meta.get("principal").cloned())
        .and_then(|p| p.get("account_uuid").cloned())
        .is_some_and(|v| !store::is_falsy(&v))
}

/// `cli._doctor_findings`: per record, then per CLI slot.
pub fn findings(records: &[Record], root: &Path, probe: &dyn SlotProbe, now: f64) -> Vec<Finding> {
    let mut out = Vec::new();
    for record in records {
        if let Some(blob) = store::read_blob(root, &record.id) {
            if let Some(holder) = store::duplicate_credential_holder(root, &blob, &record.id) {
                out.push(Finding::new(
                    &record.id,
                    "duplicate-credential",
                    format!(
                        "holds the same credential as '{holder}'; one of them was captured \
                         while the other account was signed in"
                    ),
                ));
            }
        }
        if record.auth == "managed"
            && record.harness == "claude"
            && record.config_dir.is_none()
            && !principal_bound(root, &record.id)
        {
            out.push(Finding::new(
                &record.id,
                "unbound-principal",
                format!(
                    "no proven identity is bound, so shared-slot usage cannot be attributed \
                     to it and reads unknown; sign this account in and run \
                     `fno config accounts register {}` to bind it",
                    record.id
                ),
            ));
        }
        if let Some(dir) = &record.config_dir {
            let shown = dir.display();
            if !dir.exists() {
                out.push(Finding::new(
                    &record.id,
                    "missing-config-dir",
                    format!("config_dir {shown} does not exist"),
                ));
            } else if !env::login_present(dir, probe) {
                out.push(Finding::new(
                    &record.id,
                    "no-login-in-config-dir",
                    format!(
                        "config_dir {shown} holds no claude login \
                         (run: CLAUDE_CONFIG_DIR={shown} claude /login)"
                    ),
                ));
            }
        }
    }
    let harnesses: BTreeSet<&str> = records.iter().map(|r| r.harness.as_str()).collect();
    for cli in harnesses {
        if store::slot_tainted(root, cli) {
            out.push(Finding::new(
                format!("slot:{cli}"),
                "tainted-slot",
                format!(
                    "the active stamp was written while sessions were pinned, so the slot \
                     may hold a credential the stamp does not describe; usage stays unknown \
                     until identity is proven - repair with \
                     `fno config accounts reconcile-slot {cli}`"
                ),
            ));
        }
        // Taint watches the door footnote controls; `claude /login` uses the
        // other one and leaves a stamp that is wrong AND untainted, so only
        // asking the slot who it serves turns that into a finding.
        out.extend(slot_identity(cli, records, root, probe, now));
    }
    out
}

/// `cli._slot_identity_findings`: free until it can answer. With no stamp,
/// or no principal bound to the stamped record, there is nothing to compare.
fn slot_identity(
    cli: &str,
    records: &[Record],
    root: &Path,
    probe: &dyn SlotProbe,
    now: f64,
) -> Option<Finding> {
    if cli != "claude" {
        return None;
    }
    let repair = format!("repair with `fno config accounts reconcile-slot {cli}`");
    let stamped = slot::active_slot_id(cli, Some(root), Path::new("")).ok()??;
    let bound = binding::identity_key(&binding::record_principal(&stamped, root)?);
    let got = binding::resolve(
        &BindingRequest {
            record: None,
            harness: cli,
            bearer: None,
            root,
            records,
            now,
            ttl: binding::PRINCIPAL_TTL_S,
        },
        probe,
    );
    let finding =
        |problem: &str, detail: String| Some(Finding::new(format!("slot:{cli}"), problem, detail));
    if got.status == AMBIGUOUS && got.reason.as_deref() == Some("ambiguous-slot") {
        return finding(
            "ambiguous-slot",
            format!(
                "the slot's stored credentials belong to different accounts, so whichever \
                 is stamped, some reader gets the other; {}, then {repair}",
                slot_switch_remedy("the account to keep")
            ),
        );
    }
    let Some(observed) = got.observed_principal.as_deref() else {
        // An unreadable slot cannot demonstrate drift, so this used to be quiet.
        return finding(
            UNKNOWN_RECEIPT,
            format!(
                "who the live slot serves could not be proven ({}), so usage and launch \
                 receipts stay unknown rather than naming '{stamped}'; this is not a \
                 healthy slot and not a successful switch - {repair}",
                got.reason.as_deref().unwrap_or("None")
            ),
        );
    };
    if observed == bound {
        return None;
    }
    let who = got.observed_label.as_deref().unwrap_or(observed);
    finding(
        "slot-identity-drift",
        format!(
            "the stamp names '{stamped}' but the live slot credential belongs to {who} \
             (an out-of-band `{cli} /login`), so usage is being attributed to the wrong \
             account - {repair}"
        ),
    )
}

/// The `-J` array (`json.dumps` byte shape) or the text report.
pub fn render(findings: &[Finding], as_json: bool) -> String {
    if as_json {
        let rows: Vec<Value> = findings.iter().map(Finding::to_json).collect();
        return crate::backlog::render::py_json_compact(&Value::Array(rows));
    }
    if findings.is_empty() {
        return "providers doctor: no problems found.".to_string();
    }
    let mut lines: Vec<String> = findings
        .iter()
        .map(|f| format!("{}: {}: {}", f.record, f.problem, f.detail))
        .collect();
    let n = findings.len();
    lines.push(String::new());
    lines.push(format!(
        "{n} problem{} found.",
        if n == 1 { "" } else { "s" }
    ));
    lines.join("\n")
}

const HELP: &str = "Usage: fno config accounts doctor [OPTIONS]

  Report the managed store's real condition. Exits non-zero on any problem.

  Read-only. Checks each record for a credential another account already holds,
  an unproven identity and a config_dir that holds no login; then checks each
  CLI's slot for taint and, for claude, for a live credential the stamp does not
  name. Usable as a check in a script.

Options:
  -J, --json  Emit a JSON array of findings.";

/// `fno-agents spawn-overlay accounts doctor [--json]`.
pub fn run(args: &[String]) -> i32 {
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "-J" | "--json" => as_json = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: no such option: {flag}");
                return 2;
            }
            word => {
                eprintln!("error: unexpected extra argument ({word})");
                return 2;
            }
        }
    }
    let cwd = pwd();
    let accounts = match records::load(Some(cwd.as_path()), &cwd) {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let root = slot::store_root(&cwd).unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let found = findings(&accounts.records, &root, &binding::SystemProbe, now);
    println!("{}", render(&found, as_json));
    i32::from(!found.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_matches_the_python_report() {
        let one = Finding::new("a", "tainted-slot", "x \"q\" \u{e9}".to_string());
        assert_eq!(render(&[], false), "providers doctor: no problems found.");
        assert_eq!(render(&[], true), "[]");
        assert_eq!(
            render(&[one.clone()], false),
            "a: tainted-slot: x \"q\" \u{e9}\n\n1 problem found."
        );
        assert_eq!(
            render(&[one.clone(), one.clone()], false).lines().last(),
            Some("2 problems found.")
        );
        assert_eq!(
            render(&[one], true),
            r#"[{"record": "a", "problem": "tainted-slot", "detail": "x \"q\" é"}]"#
        );
    }
}

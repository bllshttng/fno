//! The `release_run` signal of the notify_watch arm: the newest finished
//! `release.yml` run, sampled per repo. A failed run pages the operator once
//! and mails the head lead with the run's error line; a passing run clears
//! the signal, so the next failure pages again. The scheduled nightly once
//! failed three days in a row on one `::error::` line and nobody read it.
//!
//! The page cannot come from the workflow itself: the operator's ntfy server
//! sits on a private network a hosted runner cannot reach. This machine can.

use serde_json::Value;
use std::path::Path;
use std::time::Duration;

use crate::operator_notice::{
    forget_at, mark_once, notify_operator_confirmed, notify_signal_via, run_captured,
    slug_from_remote, stored_token, verdict_label,
};

const WORKFLOW: &str = "release.yml";
const TITLE: &str = "release failed";
const SENDER: &str = "fno/release-watch";
const LANE: &str = "release-watch";
const SENDER_LINE: &str = "Automatic notice from the fno daemon release-watch arm, not a person.";
/// GitHub's own trailer on every failed step. It names no cause, so a run
/// with a real `::error::` line reports that line instead.
const GENERIC_ANNOTATION: &str = "Process completed with exit code";

#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Clear,
    Failed { id: u64, url: String, label: String },
}

/// The newest settled run decides. A cancelled run is no verdict: a fresh rc
/// candidate cancels the parked older one by design, and that is not a
/// failure. Row order is never trusted; `created_at` picks the newest.
fn conclusion(run: &Value) -> &str {
    run.get("conclusion").and_then(Value::as_str).unwrap_or("")
}

pub(crate) fn verdict(runs: &Value) -> Option<Verdict> {
    let run = runs
        .get("workflow_runs")?
        .as_array()?
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("completed"))
        .filter(|r| {
            matches!(
                conclusion(r),
                "success" | "failure" | "timed_out" | "startup_failure"
            )
        })
        .max_by_key(|r| r.get("created_at").and_then(Value::as_str).unwrap_or(""))?;
    if conclusion(run) == "success" {
        return Some(Verdict::Clear);
    }
    let id = run.get("id").and_then(Value::as_u64)?;
    let url = run
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let number = run.get("run_number").and_then(Value::as_u64).unwrap_or(0);
    let event = run.get("event").and_then(Value::as_str).unwrap_or("run");
    Some(Verdict::Failed {
        id,
        url,
        label: format!("#{number} ({event}, {})", conclusion(run)),
    })
}

/// The first failure annotation that names a cause, else the generic one.
pub(crate) fn error_line(annotations: &Value) -> Option<String> {
    let messages: Vec<&str> = annotations
        .as_array()?
        .iter()
        .filter(|a| a.get("annotation_level").and_then(Value::as_str) == Some("failure"))
        .filter_map(|a| a.get("message").and_then(Value::as_str))
        .map(|m| m.lines().next().unwrap_or("").trim())
        .filter(|m| !m.is_empty())
        .collect();
    messages
        .iter()
        .find(|m| !m.starts_with(GENERIC_ANNOTATION))
        .or(messages.first())
        .map(|m| m.to_string())
}

fn gh_json(root: &Path, path: &str) -> Option<Value> {
    let out = run_captured("gh", &["api", path], root, Duration::from_secs(30))?;
    serde_json::from_str(out.trim()).ok()
}

/// `(slug, verdict)` for one repo, or None when it cannot be read or holds
/// no settled run yet. A repo with no `release.yml` reads as None too.
fn sample(root: &Path) -> Option<(String, Verdict)> {
    let remote = ["remote", "get-url", "origin"];
    let url = run_captured("git", &remote, root, Duration::from_secs(10))?;
    let slug = slug_from_remote(url.trim());
    if slug.is_empty() {
        return None;
    }
    let runs = gh_json(
        root,
        &format!("repos/{slug}/actions/workflows/{WORKFLOW}/runs?status=completed&per_page=20"),
    )?;
    Some((slug, verdict(&runs)?))
}

/// The run's error line: the first failed job's annotations. Job ids are
/// check-run ids, so one jobs read and one annotations read answer it.
fn fetch_error_line(root: &Path, slug: &str, id: u64) -> Option<String> {
    let jobs = gh_json(root, &format!("repos/{slug}/actions/runs/{id}/jobs"))?;
    let job = jobs
        .get("jobs")?
        .as_array()?
        .iter()
        .find(|j| j.get("conclusion").and_then(Value::as_str) == Some("failure"))?;
    let job_id = job.get("id").and_then(Value::as_u64)?;
    error_line(&gh_json(
        root,
        &format!("repos/{slug}/check-runs/{job_id}/annotations"),
    )?)
}

/// The scope of the one live level-0 lead, or None when none or several hold
/// one. The mail lane resolves the holder from the scope at send time.
fn head_lead_scope() -> Option<String> {
    let registry = crate::paths::AgentsHome::shared_registry_json();
    let rows = crate::client_commands::load_registry_entries(&registry).ok()?;
    let mut scopes: Vec<String> = rows
        .iter()
        .filter(|r| r.get("role_level").and_then(Value::as_i64) == Some(0))
        .filter(|r| !crate::announce::row_terminal(r))
        .filter_map(|r| r.get("role_scope").and_then(Value::as_str))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    scopes.sort();
    scopes.dedup();
    (scopes.len() == 1).then(|| scopes.remove(0))
}

/// Mail the head lead. With no head lead, or a refused send, file one fleet
/// task instead, so the failure still lands on the lead board.
fn mail_head_lead(root: &Path, key: &str, text: &str, url: &str) {
    // Same cargo-test refusal as the operator notice: unpinned means real mail.
    if cfg!(test) && std::env::var_os("FNO_BIN").is_none() {
        return;
    }
    let sent = head_lead_scope().is_some_and(|scope| {
        let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
        cmd.args(["agents", "mail", "send", "--from-name", SENDER])
            .args(["--origin", "scheduler", "--to-lead", &scope, text])
            .stdin(std::process::Stdio::null());
        crate::bounded_cmd::output_with_timeout(cmd, 30).is_some_and(|o| o.status.success())
    });
    if sent {
        return;
    }
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        return;
    };
    if let Err(e) = crate::fleet_task::file_once(
        &crate::provider_cap::questions_path(&home),
        LANE,
        key,
        &root.display().to_string(),
        text,
        Some(url),
        None,
    ) {
        eprintln!("release-watch: task refused: {e}");
    }
}

/// One repo's pass. Returns 1 when a page left the machine.
pub(crate) fn pass(
    root: &Path,
    path: &Path,
    now: u64,
    min_interval_s: u64,
    notes: &mut Vec<String>,
) -> u64 {
    let Some((slug, verdict)) = sample(root) else {
        notes.push(format!("release_run:{}:unreadable", root.display()));
        return 0;
    };
    let key = format!("release_run:{slug}");
    let mail_key = format!("release_run_mail:{slug}");
    let Verdict::Failed { id, url, label } = verdict else {
        forget_at(path, &key);
        forget_at(path, &mail_key);
        if let Some(home) = crate::paths::AgentsHome::from_env_opt() {
            let store = crate::provider_cap::questions_path(&home);
            let cwd = root.display().to_string();
            let _ = crate::fleet_task::close(&store, LANE, &key, &cwd, "release passed", SENDER);
        }
        notes.push(format!("{key}:clear"));
        return 0;
    };
    let token = id.to_string();
    if stored_token(path, &key).as_deref() == Some(token.as_str()) {
        notes.push(format!("{key}:deduped"));
        return 0;
    }
    let line = fetch_error_line(root, &slug, id)
        .unwrap_or_else(|| "no error annotation; open the run".to_string());
    let body = format!("{WORKFLOW} run {label} failed on {slug}: {line}");
    // The mail rides its own mark, written before the page: a page held by
    // the rate floor retries next pass and must not mail the lead twice.
    if mark_once(path, &mail_key, &token) {
        let text =
            format!("{SENDER_LINE} {body} Read it: gh run view {id} --log-failed --repo {slug}");
        mail_head_lead(root, &key, &text, &url);
    }
    let verdict = verdict_label(notify_signal_via(
        path,
        now,
        min_interval_s,
        &key,
        &token,
        TITLE,
        &body,
        Some(&url),
        || notify_operator_confirmed(TITLE, &body, Some(&url)),
    ));
    notes.push(format!("{key}:{verdict}"));
    u64::from(verdict == "sent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: u64, created: &str, conclusion: &str) -> Value {
        json!({"id": id, "status": "completed", "conclusion": conclusion,
               "created_at": created, "run_number": id, "event": "schedule",
               "html_url": format!("https://github.com/o/r/actions/runs/{id}")})
    }

    #[test]
    fn the_newest_settled_run_decides_and_a_cancel_is_no_verdict() {
        // Rows out of order; the newest row is a cancelled rc candidate.
        let runs = json!({"workflow_runs": [
            run(1, "2026-10-14T06:30:00Z", "success"),
            run(3, "2026-10-16T06:30:00Z", "cancelled"),
            run(2, "2026-10-15T06:30:00Z", "failure"),
        ]});
        let Some(Verdict::Failed { id, label, .. }) = verdict(&runs) else {
            panic!("the failure under the cancel must read failed");
        };
        assert_eq!((id, label.as_str()), (2, "#2 (schedule, failure)"));
        let healed = [
            run(2, "2026-10-15", "failure"),
            run(4, "2026-10-17", "success"),
        ];
        assert_eq!(
            verdict(&json!({ "workflow_runs": healed })),
            Some(Verdict::Clear)
        );
        assert_eq!(verdict(&json!({"message": "Not Found"})), None);
    }

    #[test]
    fn the_error_line_skips_the_generic_exit_code_trailer() {
        let annotations = json!([
            {"annotation_level": "failure", "message": "Process completed with exit code 1."},
            {"annotation_level": "warning", "message": "a warning"},
            {"annotation_level": "failure",
             "message": "v0.4.1 is released; bump main with sync-version.sh\nsecond line"},
        ]);
        assert_eq!(
            error_line(&annotations).as_deref(),
            Some("v0.4.1 is released; bump main with sync-version.sh")
        );
        let generic = error_line(&json!([annotations[0].clone()]));
        assert_eq!(generic.unwrap(), "Process completed with exit code 1.");
    }
}

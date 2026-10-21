//! The eval-worker removal fact: an eval attempt the evals history reads
//! `graded` is finished work, so its roster row is the leftover. The
//! retirement sweep reads the same per-machine ledger the runner appends
//! (`history/evals-history.jsonl`, `fno.paths.evals_history`) and joins it
//! on the session id the attempt observed (`observed_session_id`, the
//! field the real history carries). One row per spawn (`--once`), so a
//! graded session id names exactly the row to remove.

use serde_json::Value;
use std::collections::HashSet;
use std::io::BufRead;
use std::path::PathBuf;

/// The registry-name prefix the eval runner mints every worker under
/// (`cli/src/fno/evals/runner.py`, `mint_or_none("ev", "th", "evals", ...)`).
pub(crate) const EVAL_WORKER_PREFIX: &str = "ev-th-evals-";

/// True when the row is an eval worker whose graded attempt the history
/// records, joined on the row's harness session id. `graded` is `None`
/// when the history could not be read - an unreadable ledger never grades
/// a row, and the ordinary gates judge it.
pub(crate) fn is_graded_eval_row(
    name: &str,
    harness_session_id: Option<&str>,
    graded: Option<&HashSet<String>>,
) -> bool {
    if !name.starts_with(EVAL_WORKER_PREFIX) {
        return false;
    }
    let Some(sid) = harness_session_id.filter(|s| !s.is_empty()) else {
        return false;
    };
    graded.is_some_and(|set| set.contains(sid))
}

/// The session ids of every graded attempt in the history, `None` when the
/// ledger is absent or unreadable. One pass reads the file once; the sweep
/// calls this once per pass and passes the set down.
pub(crate) fn graded_eval_workers() -> Option<HashSet<String>> {
    let path = eval_history_path()?;
    let file = std::fs::File::open(path).ok()?;
    let mut graded = HashSet::new();
    for line in std::io::BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { continue };
        let Ok(row) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if row.get("status").and_then(Value::as_str) != Some("graded") {
            continue;
        }
        if let Some(sid) = row.get("observed_session_id").and_then(Value::as_str) {
            if !sid.is_empty() {
                graded.insert(sid.to_string());
            }
        }
    }
    Some(graded)
}

/// The evals history file (`fno.paths.evals_history`): the config override,
/// else the state dir's history folder - the same resolution
/// `backlog::triage_health` reads with.
fn eval_history_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    if let Some(v) = crate::agents_config::config_lookup(&cwd, &["paths", "evals_history"]) {
        let raw = v.as_str().unwrap_or_default();
        if !raw.is_empty() {
            return Some(PathBuf::from(crate::territory::normalize_path(raw)));
        }
    }
    let state = crate::agents_config::state_dir(&cwd)?;
    Some(state.join("history").join("evals-history.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_graded_row_of_the_eval_family_grades() {
        let graded: HashSet<String> = ["sess-1".to_string()].into_iter().collect();
        assert!(is_graded_eval_row(
            "ev-th-evals-1-2",
            Some("sess-1"),
            Some(&graded)
        ));
        assert!(
            !is_graded_eval_row("ev-th-evals-9-9", Some("sess-2"), Some(&graded)),
            "an eval row whose attempt the history never graded keeps under the ordinary gates"
        );
        assert!(
            !is_graded_eval_row("t-worker-1", Some("sess-1"), Some(&graded)),
            "a non-eval row is never graded by the eval ledger"
        );
        assert!(
            !is_graded_eval_row("ev-th-evals-1-2", Some("sess-1"), None),
            "an unreadable ledger never grades a row"
        );
        assert!(
            !is_graded_eval_row("ev-th-evals-1-2", None, Some(&graded)),
            "a row with no observed session id cannot join the ledger"
        );
        assert!(
            !is_graded_eval_row("ev-th-evals-1-2", Some(""), Some(&graded)),
            "an empty session id joins nothing"
        );
    }
}

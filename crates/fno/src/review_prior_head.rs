//! `fno do review prior-head`: the named journal read the review lane's
//! carry-forward block calls. Answers the newest passing `review_attestation`
//! head for a branch other than the current one, so every harness resolves a
//! prior round with one command instead of shipping a jq filter file.

use std::ffi::OsString;
use std::path::PathBuf;

/// The answer for one branch/head question.
pub(crate) struct PriorHead {
    pub ts: String,
    pub sha: String,
    pub journal: String,
}

/// The newest passing `review_attestation` on `branch` whose `head_sha` is
/// present, nonempty, and different from `head`. An unreadable store is a
/// refusal: it can never read as a confident "no prior round", so the
/// refusals come back named instead of swallowed.
pub(crate) fn resolve_prior_head(
    branch: &str,
    head: &str,
    journals: &[PathBuf],
) -> Result<Option<PriorHead>, Vec<String>> {
    let fields = vec![
        ("verdict".to_string(), "pass".to_string()),
        ("branch".to_string(), branch.to_string()),
    ];
    // A missing, empty, or current head_sha never names a prior round, the
    // same three exclusions the lane's former jq filter made.
    let excluded = vec![
        ("head_sha".to_string(), head.to_string()),
        ("head_sha".to_string(), String::new()),
    ];
    let mut refused: Vec<String> = Vec::new();
    let mut newest: Option<PriorHead> = None;
    for journal in journals {
        let entry = crate::event_cli::fold_journal(
            journal,
            Some("review_attestation"),
            &fields,
            &excluded,
            None,
            None,
            1,
            true,
        );
        if entry["status"] == "unreadable" {
            refused.push(format!(
                "{}: {}",
                journal.display(),
                entry["error"].as_str().unwrap_or("unreadable")
            ));
            continue;
        }
        let Some(row) = entry["matching_rows"]
            .as_array()
            .and_then(|rows| rows.last())
            .cloned()
        else {
            continue;
        };
        let ts = row
            .get("ts")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let sha = crate::event_cli::field_lookup(&row, "head_sha")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if sha.is_empty() {
            continue;
        }
        let newer = match &newest {
            None => true,
            Some(best) => ts_ms(&ts) > ts_ms(&best.ts),
        };
        if newer {
            newest = Some(PriorHead {
                ts,
                sha,
                journal: journal.display().to_string(),
            });
        }
    }
    if refused.is_empty() {
        Ok(newest)
    } else {
        Err(refused)
    }
}

/// RFC3339 ts to millis for cross-journal ordering; an unparsable ts sorts
/// before every parsable one (i64::MIN), which keeps a malformed row from
/// winning the pick.
fn ts_ms(raw: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MIN)
}
/// Exit codes: 0 answered (found or none), 2 bad usage, 3 a store refused
/// the read (an unreadable journal never reads as a confident "no prior
/// round").
pub fn run(args: &[OsString]) -> i32 {
    if args.iter().any(|arg| arg == "--help") {
        print!(
            "{}",
            r#"Usage: fno do review prior-head [OPTIONS]

  Print the prior reviewed head for a branch: the newest `review_attestation`
  row with verdict pass on the branch whose head_sha differs from the one
  passed in. Empty output means no prior round (first-round scope).

Options:
  --branch <str>   Branch name to match. Required.
  --head <str>     The current head sha to exclude. Required.
  --events <path>  Read this journal instead of the default set; may repeat.
  -J, --json       Emit one structured result.
  --help           Show this message and exit.
"#
        );
        return 0;
    }
    let mut journals: Vec<PathBuf> = Vec::new();
    let mut branch: Option<String> = None;
    let mut head: Option<String> = None;
    let mut json_mode = false;
    let mut it = args.iter();
    while let Some(tok) = it.next() {
        let Some(tok) = tok.to_str() else { continue };
        match tok {
            "--events" => {
                if let Some(v) = it.next() {
                    journals.push(PathBuf::from(v));
                }
            }
            "--branch" => {
                if let Some(v) = it.next() {
                    branch = Some(v.to_string_lossy().into_owned());
                }
            }
            "--head" => {
                if let Some(v) = it.next() {
                    head = Some(v.to_string_lossy().into_owned());
                }
            }
            "--json" | "-J" => json_mode = true,
            other if other.starts_with('-') => {
                eprintln!("error: unknown flag {other}");
                return 2;
            }
            _other => {
                eprintln!("error: no positional arguments; pass --branch and --head");
                return 2;
            }
        }
    }
    let (Some(branch), Some(head)) = (branch, head) else {
        eprintln!("error: --branch and --head are required");
        return 2;
    };
    if head.is_empty() {
        eprintln!("error: --head must be a nonempty sha");
        return 2;
    }
    if journals.is_empty() {
        journals = crate::config_cli::history::event_journals();
        for journal in &journals {
            if let Err(error) = crate::event_cli::hermetic::check(journal) {
                eprintln!("error: could not resolve event journals: {error}");
                return 3;
            }
        }
    }
    journals.sort();
    journals.dedup();
    match resolve_prior_head(&branch, &head, &journals) {
        Ok(Some(found)) => {
            if json_mode {
                let payload = serde_json::json!({
                    "result": "found",
                    "prior_head": found.sha,
                    "ts": found.ts,
                    "journal": found.journal,
                    "branch": branch,
                    "head": head,
                });
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into())
                );
            } else {
                println!("{}", found.sha);
            }
            0
        }
        Ok(None) => {
            if json_mode {
                let payload = serde_json::json!({
                    "result": "none",
                    "prior_head": null,
                    "ts": null,
                    "journal": null,
                    "branch": branch,
                    "head": head,
                    "coverage": "readable",
                });
                println!(
                    "{}",
                    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into())
                );
            }
            0
        }
        Err(refused) => {
            for line in &refused {
                eprintln!("refused: {line}");
            }
            3
        }
    }
}

#[cfg(all(test, feature = "full"))]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_passing_head_other_than_current() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            att("aaaa", "2026-11-06T10:00:01.000Z"),
            att("bbbb", "2026-11-06T10:00:02.000Z"),
        ];
        let journal = dir.path().join("fno-events.jsonl");
        std::fs::write(&journal, (rows.join("\n") + "\n").into_bytes()).unwrap();
        let answer = resolve_prior_head("feature/t", "dddd", &[journal]).unwrap();
        let got = answer.expect("a prior head exists");
        assert_eq!(got.sha, "bbbb");
        assert_eq!(got.ts, "2026-11-06T10:00:02.000Z");
    }

    /// One valid passing attestation on the branch the test queries.
    fn att(head_sha: &str, ts: &str) -> String {
        format!(
            r#"{{"ts":"{ts}","type":"review_attestation","source":"target","data":{{"reviewer":"code-review","head_sha":"{head_sha}","verdict":"pass","session_id":"s1","branch":"feature/t"}}}}"#
        )
    }
}

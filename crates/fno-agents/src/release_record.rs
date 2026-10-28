//! The release record: one JSON file per release under
//! `<agents home>/releases/<release>.json`, holding every candidate sha the
//! lead pinned, the PRs that sha includes, and the PRs that joined after a
//! pin. Written by the `authorized-merge` op transport
//! (`{"op": "release-pin"|"release-join"|"release-show", ...}`).
//!
//! A pin appends, never overwrites, so a moved pin stays visible. A pin also
//! moves a live merge freeze's sha, so the freeze and the record name one
//! candidate. The notes script reads the record with `--record`.

use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const SCHEMA: u64 = 1;

/// Subjects of `git log --first-parent <base>..<sha>`; injectable for tests.
type GitLog<'a> = &'a dyn Fn(&str, &str) -> Result<Vec<String>, String>;

/// Moves the freeze sha for (sha, repo); `merge_freeze::repin_sha` in
/// production.
type Repin<'a> = &'a dyn Fn(&str, &str) -> Result<Option<String>, String>;

/// Run one release op from an `authorized-merge` payload. `repo_dir` names
/// the checkout the included PRs are read from (default: the cwd).
pub fn run(op: &str, payload: &Value) -> String {
    let dir = AgentsHome::from_env().root().join("releases");
    let repo_dir = PathBuf::from(payload_str(payload, "repo_dir").unwrap_or("."));
    let git = |base: &str, sha: &str| git_subjects(&repo_dir, base, sha);
    run_in(&dir, op, payload, &git, &crate::merge_freeze::repin_sha)
}

fn run_in(dir: &Path, op: &str, payload: &Value, git: GitLog, repin: Repin) -> String {
    let Some(release) = payload_str(payload, "release").filter(|r| valid_release(r)) else {
        return receipt(
            "refused",
            2,
            "a release op needs --release, a name like v0.4.1rc1 (letters, digits, '.', '-')",
        )
        .to_string();
    };
    let path = dir.join(format!("{release}.json"));
    let current = match read(&path) {
        Ok(record) => record,
        Err(e) => return receipt("refused", 5, e).to_string(),
    };
    match op.strip_prefix("release-").unwrap_or(op) {
        "pin" => pin(&path, release, current, payload, git, repin),
        "join" => join(&path, current, payload),
        "show" => match current {
            Some(record) => {
                let mut out = receipt("shown", 0, "");
                out["record"] = record;
                out.to_string()
            }
            None => receipt("refused", 3, format!("no release record for {release}")).to_string(),
        },
        other => receipt("refused", 2, format!("unknown release op: {other}")).to_string(),
    }
}

fn pin(
    path: &Path,
    release: &str,
    current: Option<Value>,
    payload: &Value,
    git: GitLog,
    repin: Repin,
) -> String {
    let sha = payload_str(payload, "sha").unwrap_or("").to_lowercase();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return receipt(
            "refused",
            2,
            format!("--sha must be a 40-hex commit sha, got '{sha}'"),
        )
        .to_string();
    }
    let Some(by) = payload_str(payload, "by").filter(|b| !b.trim().is_empty()) else {
        return receipt("refused", 2, "pin needs a non-blank --by").to_string();
    };
    // The repo scopes the freeze move: the freeze record holds one repo.
    let Some(repo) = payload_str(payload, "repo").filter(|r| r.contains('/')) else {
        return receipt("refused", 2, "pin needs --repo <owner/repo>").to_string();
    };
    let mut record = current.unwrap_or_else(
        || json!({"schema": SCHEMA, "release": release, "pins": [], "included": [], "joiners": []}),
    );
    if let Some(base) = payload_str(payload, "base_tag").filter(|b| !b.trim().is_empty()) {
        // A leading '-' would reach git log as an option, not a revision.
        if base.starts_with('-') {
            return receipt(
                "refused",
                2,
                format!("--base-tag must name a tag, got '{base}'"),
            )
            .to_string();
        }
        record["base_tag"] = json!(base);
    }
    record["repo"] = json!(repo);
    let Some(base) = record
        .get("base_tag")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return receipt("refused", 2, "the first pin needs --base-tag").to_string();
    };
    if current_sha(&record) == Some(sha.as_str()) {
        return receipt(
            "refused",
            3,
            format!("{release} is already pinned at {sha}"),
        )
        .to_string();
    }
    let included = match git(&base, &sha) {
        Ok(subjects) => included_prs(&subjects),
        Err(e) => {
            return receipt("refused", 5, format!("cannot list {base}..{sha}: {e}")).to_string()
        }
    };
    let entry = json!({
        "sha": sha,
        "at": crate::announce::now_iso(),
        "by": by,
        "reason": payload_str(payload, "reason").unwrap_or(""),
    });
    if let Some(pins) = record["pins"].as_array_mut() {
        pins.push(entry);
    }
    record["included"] = json!(included);
    if let Err(e) = write(path, &record) {
        return receipt("error", 2, e).to_string();
    }
    let (code, freeze) = match repin(&sha, repo) {
        Ok(Some(lift)) => (0, format!("moved; lift at {lift}")),
        Ok(None) => (0, "none".to_string()),
        Err(e) => (
            1,
            format!("not moved ({e}); the freeze still names the old sha"),
        ),
    };
    let mut out = receipt("pinned", code, "");
    out["sha"] = json!(sha);
    out["pins"] = json!(record["pins"].as_array().map_or(0, Vec::len));
    out["included"] = json!(included);
    out["freeze"] = json!(freeze);
    out.to_string()
}

fn join(path: &Path, current: Option<Value>, payload: &Value) -> String {
    let Some(mut record) = current else {
        return receipt("refused", 3, "no release record yet; pin a sha first").to_string();
    };
    let Some(pr) = payload.get("pr").and_then(Value::as_u64) else {
        return receipt("refused", 2, "join needs --pr <number>").to_string();
    };
    let Some(by) = payload_str(payload, "by").filter(|b| !b.trim().is_empty()) else {
        return receipt("refused", 2, "join needs a non-blank --by").to_string();
    };
    let has = |key: &str, field: Option<&str>| {
        record[key].as_array().is_some_and(|list| {
            list.iter()
                .any(|v| field.map_or(v, |f| &v[f]).as_u64() == Some(pr))
        })
    };
    if has("included", None) {
        let at = current_sha(&record).unwrap_or("");
        return receipt(
            "refused",
            3,
            format!(
                "PR #{pr} is already included at pin {}",
                &at[..at.len().min(12)]
            ),
        )
        .to_string();
    }
    if has("joiners", Some("pr")) {
        return receipt("refused", 3, format!("PR #{pr} already joined")).to_string();
    }
    let entry = json!({"pr": pr, "at": crate::announce::now_iso(), "by": by});
    if let Some(joiners) = record["joiners"].as_array_mut() {
        joiners.push(entry);
    }
    if let Err(e) = write(path, &record) {
        return receipt("error", 2, e).to_string();
    }
    let mut out = receipt("joined", 0, "");
    out["pr"] = json!(pr);
    out.to_string()
}

fn current_sha(record: &Value) -> Option<&str> {
    record["pins"].as_array()?.last()?.get("sha")?.as_str()
}

/// PR numbers from merge subjects (`Merge pull request #N ...`) and squash
/// subjects (`... (#N)`), ascending.
fn included_prs(subjects: &[String]) -> Vec<u64> {
    let mut prs: Vec<u64> = subjects
        .iter()
        .filter_map(|s| {
            let digits = if let Some(rest) = s.strip_prefix("Merge pull request #") {
                rest.split(|c: char| !c.is_ascii_digit()).next()?
            } else {
                let inner = s.trim_end().strip_suffix(')')?;
                &inner[inner.rfind("(#")? + 2..]
            };
            digits.parse().ok()
        })
        .collect();
    prs.sort_unstable();
    prs.dedup();
    prs
}

fn git_subjects(repo_dir: &Path, base: &str, sha: &str) -> Result<Vec<String>, String> {
    let range = format!("{base}..{sha}");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["log", "--first-parent", "--format=%s", range.as_str()])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err
            .lines()
            .last()
            .unwrap_or("git log failed")
            .trim()
            .to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn valid_release(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// `Ok(None)` is no record; `Err` is an unreadable one.
fn read(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

fn write(path: &Path, record: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("release record dir: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text + "\n").map_err(|e| format!("release record write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("release record rename: {e}"))
}

fn receipt(outcome: &str, code: i32, detail: impl Into<String>) -> Value {
    json!({"outcome": outcome, "exit_code": code, "detail": detail.into()})
}

fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn release_record_pin_join_contract() {
        let dir = tempfile::TempDir::new().unwrap();
        let git = |base: &str, sha: &str| {
            assert_eq!(base, "v0.4.0");
            let mut subjects = vec![
                "Merge pull request #10 from o/feature/x".to_string(),
                "fix(setup): keep the shim (#11)".to_string(),
                "chore: no pr here".to_string(),
            ];
            if sha == B {
                subjects.push("feat: late work (#12)".to_string());
            }
            Ok::<_, String>(subjects)
        };
        let repinned = RefCell::new(Vec::new());
        let repin = |sha: &str, _repo: &str| {
            repinned.borrow_mut().push(sha.to_string());
            Ok::<Option<String>, String>(None)
        };
        let op = |op: &str, payload: Value| -> Value {
            serde_json::from_str(&run_in(dir.path(), op, &payload, &git, &repin)).unwrap()
        };

        assert_eq!(
            op(
                "release-pin",
                json!({"release": "v0.4.1rc1", "sha": A, "by": "lead", "repo": "o/r"})
            )["detail"],
            "the first pin needs --base-tag"
        );
        let dash = json!({"release": "v0.4.1rc1", "base_tag": "--output=x", "sha": A,
                          "by": "lead", "repo": "o/r"});
        assert_eq!(op("release-pin", dash)["exit_code"], 2);
        let first = json!({"release": "v0.4.1rc1", "base_tag": "v0.4.0", "sha": A, "by": "lead", "repo": "o/r"});
        assert_eq!(op("release-pin", first)["included"], json!([10, 11]));

        // AC2: a joiner already in the pin is refused and named.
        let refused = op(
            "release-join",
            json!({"release": "v0.4.1rc1", "pr": 11, "by": "lead"}),
        );
        assert_eq!(refused["exit_code"], 3);
        assert!(
            refused["detail"].as_str().unwrap().contains("PR #11"),
            "{refused}"
        );
        let joined = op(
            "release-join",
            json!({"release": "v0.4.1rc1", "pr": 13, "by": "lead"}),
        );
        assert_eq!(joined["outcome"], "joined");

        // AC1: a moved pin appends; the history reads both pins in order.
        let moved = op(
            "release-pin",
            json!({"release": "v0.4.1rc1", "sha": B, "by": "lead", "repo": "o/r",
                                             "reason": "late fix"}),
        );
        assert_eq!(
            (moved["pins"].clone(), moved["included"].clone()),
            (json!(2), json!([10, 11, 12]))
        );
        let shown = op("release-show", json!({"release": "v0.4.1rc1"}));
        let record = &shown["record"];
        let shas: Vec<&str> = record["pins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["sha"].as_str().unwrap())
            .collect();
        assert_eq!(shas, vec![A, B]);
        assert_eq!(current_sha(record), Some(B));
        assert_eq!(record["joiners"][0]["pr"], 13);
        assert_eq!(*repinned.borrow(), vec![A.to_string(), B.to_string()]);

        let bad = op("release-show", json!({"release": "../x"}));
        assert_eq!(bad["exit_code"], 2);
    }
}

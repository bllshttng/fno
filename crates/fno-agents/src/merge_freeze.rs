//! The merge freeze: one record with a subject, a repo and a pin (a candidate
//! sha, named PRs, or both), written by the team through the `authorized-merge`
//! verb's op transport (`{"op": "freeze-set"|"freeze-clear"|"freeze-check",
//! ...}`).
//!
//! The merge owner reads it as a probe in its decision walk, so every caller
//! of `authorized_merge::run` holds an off-pin PR: `fno do pr merge`, the
//! pr-watch merge arm and the finalize auto-merge arm. The pr-watch pre-check
//! in Python asks the `freeze-check` op for its early skip.
//!
//! Only the user lifts a freeze: a live superuser-lane ruling (operator or
//! chat_attested) at `merge-freeze:<repo>@<pin key>`. A team set with a new pin
//! over a lifted one re-arms the freeze, because the new pin has its own lift
//! subject and no ruling there. A PR on the pin's PR list merges normally: the
//! freeze scopes a release window, it is not a full stop. An unreadable record
//! or decision store fails closed; absence is a real answer.

use crate::authorized_merge::{PrFacts, ProbeOutcome};
use crate::decision_index::{self, Index};
use crate::paths::AgentsHome;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The schema version this module writes. Version 1 (an `allow` list with no
/// repo) still reads, as a PR pin over every repo.
pub const STATE_VERSION: u32 = 2;

/// The record's home, beside `fleet-stop.json`.
pub fn merge_freeze_json() -> PathBuf {
    record_path(&AgentsHome::from_env().root())
}

fn record_path(home: &Path) -> PathBuf {
    home.join("merge-freeze.json")
}

/// The decision-store read the lift fold uses. A parameter so tests seed a
/// store; production passes `decision_index::default_store_live`.
type Laws<'a> = &'a dyn Fn() -> Result<Index, String>;

/// Ok((success, combined output)) for one `gh` call; injectable for tests.
type GhRun<'a> = &'a dyn Fn(&[&str]) -> Result<(bool, String), String>;

/// One freeze verdict for a PR: `Clear` admits; `Frozen` names the freeze and
/// its lift subject; `Unavailable` is an unreadable record or store.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Clear,
    Frozen {
        subject: String,
        set_by: String,
        lift: String,
    },
    Unavailable {
        detail: String,
    },
}

/// A parsed record. `repo` is `None` for a version-1 record, which freezes
/// every repo.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    subject: String,
    set_by: String,
    repo: Option<String>,
    sha: Option<String>,
    prs: Vec<u64>,
}

impl Record {
    fn pin_key(&self) -> String {
        pin_key(self.sha.as_deref(), &self.prs)
    }

    fn lift_subject(&self) -> String {
        lift_subject(self.repo.as_deref().unwrap_or("*"), &self.pin_key())
    }
}

/// `sha:<first 12 hex>` and/or `prs:<n>+<n>` ascending, joined by `,`.
fn pin_key(sha: Option<&str>, prs: &[u64]) -> String {
    let mut parts = Vec::new();
    if let Some(sha) = sha {
        parts.push(format!("sha:{}", sha.chars().take(12).collect::<String>()));
    }
    if !prs.is_empty() {
        let mut sorted = prs.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let list: Vec<String> = sorted.iter().map(u64::to_string).collect();
        parts.push(format!("prs:{}", list.join("+")));
    }
    parts.join(",")
}

fn lift_subject(repo: &str, key: &str) -> String {
    format!("merge-freeze:{repo}@{key}")
}

/// `Ok(None)` is no record; `Err` is an unreadable one.
fn read_record(home: &Path) -> Result<Option<Record>, String> {
    let path = record_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
    let text_of = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    let numbers = |list: Option<&Value>| -> Vec<u64> {
        list.and_then(Value::as_array)
            .map(|l| l.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default()
    };
    let (repo, sha, prs) = match v.get("version").and_then(Value::as_u64) {
        Some(1) => (None, None, numbers(v.get("allow"))),
        Some(2) => (
            text_of("repo").filter(|r| !r.trim().is_empty()),
            v.pointer("/pin/sha")
                .and_then(Value::as_str)
                .map(str::to_string),
            numbers(v.pointer("/pin/prs")),
        ),
        _ => {
            return Err(format!(
                "{} is not a version-1 or version-{STATE_VERSION} freeze record",
                path.display()
            ))
        }
    };
    if sha.is_none() && prs.is_empty() {
        return Err(format!("{} names no pin", path.display()));
    }
    Ok(Some(Record {
        subject: text_of("subject").unwrap_or_else(|| "unnamed freeze".to_string()),
        set_by: text_of("set_by").unwrap_or_default(),
        repo,
        sha,
        prs,
    }))
}

/// A live superuser-lane row at `subject`.
fn lift_row<'a>(index: &'a Index, subject: &str) -> Option<&'a Value> {
    index.rows.iter().find(|row| {
        decision_index::is_law(row)
            && row
                .get("subject")
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case(subject))
    })
}

/// `Ok(true)` when the user lifted this record's pin.
fn is_lifted(record: &Record, laws: Laws) -> Result<bool, String> {
    let index = laws().map_err(|e| format!("the decision store is unreadable ({e})"))?;
    Ok(lift_row(&index, &record.lift_subject()).is_some())
}

/// The verdict for one PR. `repo` is the PR's `owner/repo`; `None` (the
/// pr-watch pre-check sends none) reads as inside the freeze's repo.
fn verdict_for_in(home: &Path, pr: Option<u64>, repo: Option<&str>, laws: Laws) -> Verdict {
    let record = match read_record(home) {
        Ok(None) => return Verdict::Clear,
        Ok(Some(r)) => r,
        Err(detail) => return Verdict::Unavailable { detail },
    };
    if let (Some(frozen), Some(asked)) = (record.repo.as_deref(), repo) {
        if !frozen.eq_ignore_ascii_case(asked) {
            return Verdict::Clear;
        }
    }
    if pr.is_some_and(|n| record.prs.contains(&n)) {
        return Verdict::Clear;
    }
    match is_lifted(&record, laws) {
        Ok(true) => Verdict::Clear,
        Ok(false) => Verdict::Frozen {
            lift: record.lift_subject(),
            subject: record.subject,
            set_by: record.set_by,
        },
        Err(detail) => Verdict::Unavailable { detail },
    }
}

/// The user's two doors, named in every refusal.
fn lift_doors(lift: &str) -> String {
    format!(
        "only the user lifts it: `fno inbox decide '{lift}' 'lift' --authority operator` \
         at a terminal, or `/fno:law` in chat with that subject"
    )
}

/// The merge owner's probe. Frozen refuses (the owner's fail_closed reads it
/// as held, because the user's lift clears it) and disarms an armed queue, which
/// would otherwise merge server side with no fno read. Unavailable is
/// inconclusive, which also fails closed.
pub(crate) fn probe(facts: &PrFacts) -> ProbeOutcome {
    let repo = crate::merge_grant::repo_slug_from_pr_url(&facts.url);
    let verdict = verdict_for_in(
        &AgentsHome::from_env().root(),
        Some(facts.number),
        repo.as_deref(),
        &decision_index::default_store_live,
    );
    probe_outcome(verdict, facts, &|n| {
        crate::merge_hold::disarm_automerge(n);
    })
}

fn probe_outcome(verdict: Verdict, facts: &PrFacts, disarm: &dyn Fn(u64)) -> ProbeOutcome {
    match verdict {
        Verdict::Clear => ProbeOutcome::Clear,
        Verdict::Frozen {
            subject,
            set_by,
            lift,
        } => {
            if facts.armed {
                disarm(facts.number);
            }
            let who = if set_by.is_empty() {
                String::new()
            } else {
                format!(" (set_by {set_by})")
            };
            ProbeOutcome::Refused(format!(
                "merge_freeze: a merge freeze holds ({subject}{who}) and PR #{} is off its \
                 pin {lift}; {}",
                facts.number,
                lift_doors(&lift)
            ))
        }
        Verdict::Unavailable { detail } => ProbeOutcome::Inconclusive(format!(
            "merge_freeze: the freeze is unreadable ({detail}); the merge fails closed"
        )),
    }
}

/// Run one freeze op from an `authorized-merge` payload. Same receipt
/// contract as the hold ops.
pub fn run(op: &str, payload: &Value) -> String {
    run_in(
        &AgentsHome::from_env().root(),
        op,
        payload,
        &decision_index::default_store_live,
        &real_gh,
    )
}

fn real_gh(args: &[&str]) -> Result<(bool, String), String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(args);
    let out = crate::gh_budget::output(cmd).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

fn run_in(home: &Path, op: &str, payload: &Value, laws: Laws, gh: GhRun) -> String {
    match op.strip_prefix("freeze-").unwrap_or(op) {
        "set" => set_in(home, payload, laws, gh),
        "clear" => clear_in(home, payload, laws),
        "check" => check_in(home, payload, laws),
        other => receipt("refused", 2, format!("unknown freeze op: {other}")).to_string(),
    }
}

fn receipt(outcome: &str, code: i32, detail: impl Into<String>) -> Value {
    json!({"outcome": outcome, "exit_code": code, "detail": detail.into()})
}

fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn set_in(home: &Path, payload: &Value, laws: Laws, gh: GhRun) -> String {
    let subject = payload_str(payload, "subject").unwrap_or("");
    let set_by = payload_str(payload, "set_by").unwrap_or("");
    let repo = payload_str(payload, "repo").unwrap_or("");
    for (name, value) in [("subject", subject), ("set-by", set_by), ("repo", repo)] {
        if value.trim().is_empty() {
            return receipt("refused", 2, format!("set needs a non-blank --{name}")).to_string();
        }
    }
    let sha = payload_str(payload, "sha").map(str::to_lowercase);
    if let Some(sha) = &sha {
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return receipt(
                "refused",
                2,
                format!("--sha must be a 40-hex commit sha, got '{sha}'"),
            )
            .to_string();
        }
    }
    let mut prs: Vec<u64> = payload
        .get("prs")
        .or_else(|| payload.get("allow"))
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    prs.sort_unstable();
    prs.dedup();
    // A record with neither pin is a total stop, which the fleet breaker owns.
    if sha.is_none() && prs.is_empty() {
        return receipt(
            "refused",
            2,
            "set needs a pin: a --sha, a --prs list, or both; a freeze with no pin \
             stops every merge, which the fleet breaker owns",
        )
        .to_string();
    }
    let next = Record {
        subject: subject.to_string(),
        set_by: set_by.to_string(),
        repo: Some(repo.to_string()),
        sha,
        prs,
    };
    // Re-arm: an unlifted record refuses; a lifted one yields to a new pin
    // only, because the user lifted that pin and not the window after it.
    match read_record(home) {
        Ok(None) => {}
        Ok(Some(current)) => match is_lifted(&current, laws) {
            Ok(false) => {
                return receipt(
                    "refused",
                    3,
                    format!(
                        "a merge freeze is already active ({}); {}",
                        current.subject,
                        lift_doors(&current.lift_subject())
                    ),
                )
                .to_string()
            }
            Ok(true) if current.lift_subject() == next.lift_subject() => {
                return receipt(
                    "refused",
                    3,
                    format!(
                        "the user lifted this pin ({}); a re-arm needs a new pin",
                        next.lift_subject()
                    ),
                )
                .to_string()
            }
            Ok(true) => {}
            Err(e) => return receipt("refused", 5, e).to_string(),
        },
        Err(e) => {
            return receipt(
                "refused",
                5,
                format!("the freeze record is unreadable ({e})"),
            )
            .to_string()
        }
    }
    let mut pin = serde_json::Map::new();
    if let Some(sha) = &next.sha {
        pin.insert("sha".into(), json!(sha));
    }
    if !next.prs.is_empty() {
        pin.insert("prs".into(), json!(next.prs));
    }
    let record = json!({
        "version": STATE_VERSION,
        "subject": subject,
        "set_by": set_by,
        "reason": payload_str(payload, "reason").unwrap_or(""),
        "repo": repo,
        "pin": pin,
    });
    let path = record_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, format!("{record}\n")) {
        return receipt("error", 2, format!("freeze write failed: {e}")).to_string();
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        return receipt("error", 2, format!("freeze rename failed: {e}")).to_string();
    }
    // Readback: the writer proves the record says what it wrote.
    if read_record(home).ok().flatten().as_ref() != Some(&next) {
        let _ = std::fs::remove_file(&path);
        return receipt(
            "error",
            1,
            "freeze readback did not match the written pin; the record was removed",
        )
        .to_string();
    }
    let (disarmed, failed) = disarm_off_pin(gh, repo, &next.prs);
    let mut out = receipt("frozen", 0, "");
    if let Some(obj) = out.as_object_mut() {
        obj.insert("subject".into(), json!(subject));
        obj.insert("lift".into(), json!(next.lift_subject()));
        obj.insert("disarmed".into(), json!(disarmed));
        obj.insert("disarm_failed".into(), json!(failed));
    }
    out.to_string()
}

/// Disarm every open armed PR of `repo` that is off the pin. Never fails the
/// set: the record is durable, and the owner's probe disarms again at the next
/// gate read. A failure names the by-hand command.
fn disarm_off_pin(gh: GhRun, repo: &str, pin_prs: &[u64]) -> (Vec<u64>, Vec<String>) {
    let list = gh(&[
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        "open",
        "--limit",
        "200",
        "--json",
        "number,autoMergeRequest",
    ]);
    let rows: Vec<Value> = match list {
        Ok((true, text)) => match serde_json::from_str(&text) {
            Ok(Value::Array(rows)) => rows,
            _ => {
                return (
                    vec![],
                    vec![format!("gh pr list --repo {repo}: unparseable output")],
                )
            }
        },
        Ok((false, text)) | Err(text) => {
            let last = text.lines().last().unwrap_or("").trim().to_string();
            return (vec![], vec![format!("gh pr list --repo {repo}: {last}")]);
        }
    };
    let mut disarmed = Vec::new();
    let mut failed = Vec::new();
    for row in rows {
        let Some(n) = row.get("number").and_then(Value::as_u64) else {
            continue;
        };
        let armed = row.get("autoMergeRequest").is_some_and(|a| !a.is_null());
        if !armed || pin_prs.contains(&n) {
            continue;
        }
        let number = n.to_string();
        match gh(&["pr", "merge", &number, "--repo", repo, "--disable-auto"]) {
            Ok((true, _)) => disarmed.push(n),
            _ => failed.push(format!(
                "PR {n}: run gh pr merge {n} --repo {repo} --disable-auto by hand"
            )),
        }
    }
    (disarmed, failed)
}

/// Removes the record only on a live superuser ruling at its lift subject,
/// named by decision id. The lift itself already clears every gate read;
/// this op only tidies the file.
fn clear_in(home: &Path, payload: &Value, laws: Laws) -> String {
    let evidence = payload_str(payload, "evidence").unwrap_or("").trim();
    let record = match read_record(home) {
        Ok(None) => {
            return receipt("refused", 3, "no merge freeze is active; nothing to lift").to_string()
        }
        Ok(Some(r)) => r,
        Err(e) => {
            return receipt(
                "refused",
                5,
                format!("the freeze record is unreadable ({e})"),
            )
            .to_string()
        }
    };
    let lift = record.lift_subject();
    let index = match laws() {
        Ok(i) => i,
        Err(e) => {
            return receipt(
                "refused",
                5,
                format!(
                    "the decision store is unreadable ({e}); {}",
                    lift_doors(&lift)
                ),
            )
            .to_string()
        }
    };
    let row = index.rows.iter().find(|r| {
        !evidence.is_empty()
            && r.get("decision_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.eq_ignore_ascii_case(evidence))
    });
    let lawful = row.is_some_and(|r| {
        decision_index::is_law(r)
            && r.get("subject")
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case(&lift))
    });
    if !lawful {
        return receipt(
            "refused",
            3,
            format!(
                "clear needs --evidence naming a live superuser ruling at {lift}, got \
                 '{evidence}'; {}",
                lift_doors(&lift)
            ),
        )
        .to_string();
    }
    if let Err(e) = std::fs::remove_file(record_path(home)) {
        return receipt("error", 2, format!("freeze clear failed: {e}")).to_string();
    }
    let mut out = receipt("lifted", 0, "");
    if let Some(obj) = out.as_object_mut() {
        obj.insert("evidence".into(), json!(evidence));
    }
    out.to_string()
}

fn check_in(home: &Path, payload: &Value, laws: Laws) -> String {
    let pr = payload.get("pr").and_then(Value::as_u64);
    let repo = payload_str(payload, "repo");
    match verdict_for_in(home, pr, repo, laws) {
        Verdict::Clear => receipt("clear", 0, "").to_string(),
        Verdict::Frozen {
            subject,
            set_by,
            lift,
        } => receipt("frozen", 0, format!("{subject} {set_by}; lift at {lift}")).to_string(),
        Verdict::Unavailable { detail } => receipt("unavailable", 0, detail).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    const SHA: &str = "abc123abc123abc123abc123abc123abc123abc1";

    fn home(test: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let root = dir.path().join(test);
        std::fs::create_dir_all(&root).unwrap();
        let clone = root.clone();
        (dir, clone)
    }

    fn row(id: &str, subject: &str, authority: &str) -> Value {
        json!({"decision_id": id, "subject": subject, "authority_source": authority,
               "ts": "2026-10-20T00:00:00Z"})
    }

    fn store(rows: Vec<Value>) -> impl Fn() -> Result<Index, String> {
        move || {
            Ok(Index {
                rows: rows.clone(),
                damaged: 0,
            })
        }
    }

    fn no_gh(_: &[&str]) -> Result<(bool, String), String> {
        Ok((true, "[]".to_string()))
    }

    fn set(home: &Path, sha: &str, laws: Laws) -> Value {
        let out = run_in(
            home,
            "freeze-set",
            &json!({"subject": "rc", "set_by": "team", "repo": "o/r", "sha": sha, "prs": [7]}),
            laws,
            &no_gh,
        );
        serde_json::from_str(&out).unwrap()
    }

    #[test]
    fn freeze_gate_contract() {
        let (_dir, home) = home("freeze");
        let empty = store(vec![]);
        assert_eq!(
            verdict_for_in(&home, Some(8), Some("o/r"), &empty),
            Verdict::Clear
        );
        assert_eq!(set(&home, SHA, &empty)["outcome"], "frozen");

        // AC1: the pin admits PR 7 and another repo; PR 8 is frozen and named.
        let lift = format!("merge-freeze:o/r@sha:{},prs:7", &SHA[..12]);
        assert_eq!(
            verdict_for_in(&home, Some(7), Some("o/r"), &empty),
            Verdict::Clear
        );
        assert_eq!(
            verdict_for_in(&home, Some(8), Some("p/q"), &empty),
            Verdict::Clear
        );
        let frozen = verdict_for_in(&home, Some(8), Some("o/r"), &empty);
        assert!(
            matches!(&frozen, Verdict::Frozen { lift: l, .. } if *l == lift),
            "{frozen:?}"
        );

        // AC2: only a superuser row at this pin's subject lifts it.
        let team = store(vec![row("d-1", &lift, "team")]);
        assert!(matches!(
            verdict_for_in(&home, Some(8), Some("o/r"), &team),
            Verdict::Frozen { .. }
        ));
        let other = store(vec![row(
            "d-2",
            "merge-freeze:o/r@sha:ffffffffffff",
            "operator",
        )]);
        assert!(matches!(
            verdict_for_in(&home, Some(8), Some("o/r"), &other),
            Verdict::Frozen { .. }
        ));
        let lifted = store(vec![row("d-3", &lift, "operator")]);
        assert_eq!(
            verdict_for_in(&home, Some(8), Some("o/r"), &lifted),
            Verdict::Clear
        );
        let chat = store(vec![row("d-4", &lift, "chat_attested")]);
        assert_eq!(
            verdict_for_in(&home, Some(8), Some("o/r"), &chat),
            Verdict::Clear
        );

        // AC4: an unreadable store fails closed.
        let broken = || -> Result<Index, String> { Err("gone".to_string()) };
        assert!(matches!(
            verdict_for_in(&home, Some(8), Some("o/r"), &broken),
            Verdict::Unavailable { .. }
        ));

        // AC5: free text never clears; an unlifted set refuses.
        let free = run_in(
            &home,
            "freeze-clear",
            &json!({"evidence": "lifted"}),
            &lifted,
            &no_gh,
        );
        assert!(
            free.contains("--authority operator") && free.contains("/fno:law"),
            "{free}"
        );
        assert!(record_path(&home).exists());
        let again = set(&home, SHA, &empty);
        assert!(
            again["detail"].as_str().unwrap().contains("already active"),
            "{again}"
        );

        // AC3: a lifted pin refuses the same pin and re-arms on a new one.
        let same = set(&home, SHA, &lifted);
        assert!(
            same["detail"].as_str().unwrap().contains("lifted this pin"),
            "{same}"
        );
        let next_sha = "def456def456def456def456def456def456def4";
        assert_eq!(set(&home, next_sha, &lifted)["outcome"], "frozen");
        assert!(matches!(
            verdict_for_in(&home, Some(8), Some("o/r"), &lifted),
            Verdict::Frozen { .. }
        ));

        // A ruling at the current pin, named by id, clears the record.
        let new_lift = format!("merge-freeze:o/r@sha:{},prs:7", &next_sha[..12]);
        let ruled = store(vec![row("d-5", &new_lift, "operator")]);
        let cleared = run_in(
            &home,
            "freeze-clear",
            &json!({"evidence": "d-5"}),
            &ruled,
            &no_gh,
        );
        assert!(cleared.contains("\"lifted\""), "{cleared}");
        assert!(!record_path(&home).exists());

        unreadable_and_v1_contract();
        probe_disarm_contract();
    }

    fn unreadable_and_v1_contract() {
        let (_dir, home) = home("freeze-v1");
        let empty = store(vec![]);
        std::fs::write(record_path(&home), "{").unwrap();
        assert!(matches!(
            verdict_for_in(&home, Some(42), None, &empty),
            Verdict::Unavailable { .. }
        ));
        std::fs::write(
            record_path(&home),
            r#"{"version":1,"subject":"old","set_by":"t","allow":[5]}"#,
        )
        .unwrap();
        assert_eq!(
            verdict_for_in(&home, Some(5), Some("x/y"), &empty),
            Verdict::Clear
        );
        assert!(matches!(
            verdict_for_in(&home, Some(6), Some("x/y"), &empty),
            Verdict::Frozen { ref lift, .. } if lift == "merge-freeze:*@prs:5"
        ));
    }

    fn probe_disarm_contract() {
        let facts = PrFacts {
            number: 8,
            armed: true,
            ..PrFacts::default()
        };
        let hit = RefCell::new(vec![]);
        let frozen = Verdict::Frozen {
            subject: "rc".into(),
            set_by: "team".into(),
            lift: "merge-freeze:o/r@prs:7".into(),
        };
        let out = probe_outcome(frozen, &facts, &|n| hit.borrow_mut().push(n));
        assert!(
            matches!(out, ProbeOutcome::Refused(ref r) if r.contains("merge-freeze:o/r@prs:7"))
        );
        assert_eq!(*hit.borrow(), vec![8]);
        let gone = Verdict::Unavailable { detail: "x".into() };
        assert!(matches!(
            probe_outcome(gone, &facts, &|_| {}),
            ProbeOutcome::Inconclusive(_)
        ));
    }

    #[test]
    fn freeze_set_disarms_off_pin_armed_prs() {
        let (_dir, home) = home("freeze-disarm");
        let calls = RefCell::new(Vec::<String>::new());
        let fail_merge = RefCell::new(false);
        let gh = |args: &[&str]| -> Result<(bool, String), String> {
            calls.borrow_mut().push(args.join(" "));
            if args[1] == "list" {
                return Ok((
                    true,
                    r#"[{"number":7,"autoMergeRequest":{"x":1}},{"number":8,"autoMergeRequest":{"x":1}},{"number":9,"autoMergeRequest":null}]"#
                        .to_string(),
                ));
            }
            Ok((!*fail_merge.borrow(), "boom".to_string()))
        };
        let empty = store(vec![]);
        let payload = json!({"subject": "rc", "set_by": "team", "repo": "o/r", "prs": [7]});
        // AC9: only the armed off-pin PR is disarmed.
        let out: Value =
            serde_json::from_str(&run_in(&home, "freeze-set", &payload, &empty, &gh)).unwrap();
        assert_eq!(out["disarmed"], json!([8]), "{out}");
        assert!(calls
            .borrow()
            .iter()
            .any(|c| c == "pr merge 8 --repo o/r --disable-auto"));
        assert!(!calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("pr merge 7") || c.starts_with("pr merge 9")));

        // AC10: a failed disarm still writes the record and names the by-hand command.
        std::fs::remove_file(record_path(&home)).unwrap();
        *fail_merge.borrow_mut() = true;
        let out: Value =
            serde_json::from_str(&run_in(&home, "freeze-set", &payload, &empty, &gh)).unwrap();
        assert_eq!(out["outcome"], "frozen", "{out}");
        assert!(out["disarm_failed"][0]
            .as_str()
            .unwrap()
            .contains("--disable-auto by hand"));
        assert!(record_path(&home).exists());
    }
}

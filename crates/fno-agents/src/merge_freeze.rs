//! The merge freeze: one record with a subject, a repo and a pin (a candidate
//! sha, named PRs, or both), written by the team through the `authorized-merge`
//! command's op transport (`{"op": "freeze-set"|"freeze-clear"|"freeze-check",
//! ...}`).
//!
//! The merge owner reads it as a probe in its decision walk, so every caller
//! of `authorized_merge::run` holds an off-pin PR: `fno do pr merge`, the
//! pr-watch merge arm and the finalize auto-merge arm. The pr-watch pre-check
//! in Python asks the `freeze-check` op for its early skip.
//!
//! Only the user lifts a freeze: a live user ruling at
//! `merge-freeze:<repo>@<pin key>`, an `operator` row or a `chat_attested`
//! row that stores the user's words (`decision_index::is_user_ruling`). A
//! bare chat_attested row any session can mint never lifts. A team set with a new pin
//! over a lifted one re-arms the freeze, because the new pin has its own lift
//! subject and no ruling there. A PR on the pin's PR list merges normally: the
//! freeze scopes a release window, it is not a full stop. An unreadable record
//! or decision store fails closed; absence is a real answer.
//!
//! With `store.remote_url` set, the record lives on the shared primary, so a
//! freeze set on one machine holds a merge made from another. The local file
//! still reads when the primary holds no record, so a freeze an older build
//! wrote keeps holding. An unreachable primary fails closed like an
//! unreadable file.

use crate::authorized_merge::{PrFacts, ProbeOutcome};
use crate::decision_index::{self, Index};
use crate::paths::AgentsHome;
use crate::store_remote::{Remote, SqlValue};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The schema version this module writes. Version 1 (an `allow` list with no
/// repo) still reads, as a PR pin over every repo.
pub const STATE_VERSION: u32 = 2;

/// The primary's table: one row, the same JSON the local file holds.
const TABLE: &str = "merge_freeze";

fn record_path(home: &Path) -> PathBuf {
    home.join("merge-freeze.json")
}

/// The record's homes: the local file beside `fleet-stop.json`, and the
/// shared primary when one is configured. A write goes to the primary when
/// there is one.
struct Store {
    file: PathBuf,
    primary: Option<Remote>,
}

impl Store {
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            file: record_path(&AgentsHome::from_env().root()),
            primary: crate::store_remote::configured()?,
        })
    }

    fn read_text(&self) -> Result<Option<(String, String)>, String> {
        if let Some(remote) = &self.primary {
            let label = format!("the {TABLE} row on {}", remote.url());
            let reply =
                match remote.execute(&format!("SELECT record FROM {TABLE} WHERE id = 1"), &[]) {
                    Ok(reply) => reply,
                    // A primary that never saw a set has no table: no record.
                    Err(e) if e.contains("no such table") => Default::default(),
                    Err(e) => return Err(format!("read {label}: {e}")),
                };
            if let Some(cell) = reply.rows.first().and_then(|row| row.first()) {
                let text = cell.text().ok_or_else(|| format!("{label} is not text"))?;
                return Ok(Some((text.to_string(), label)));
            }
        }
        match std::fs::read_to_string(&self.file) {
            Ok(text) => Ok(Some((text, self.file.display().to_string()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", self.file.display())),
        }
    }

    fn write(&self, text: &str) -> Result<(), String> {
        if let Some(remote) = &self.primary {
            remote.script(&format!(
                "CREATE TABLE IF NOT EXISTS {TABLE} (\
                     id INTEGER PRIMARY KEY CHECK (id = 1), \
                     record TEXT NOT NULL)"
            ))?;
            return remote
                .execute(
                    &format!("INSERT OR REPLACE INTO {TABLE} (id, record) VALUES (1, ?)"),
                    &[SqlValue::Text(text.to_string())],
                )
                .map(drop);
        }
        if let Some(parent) = self.file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("freeze write failed: {e}"))?;
        std::fs::rename(&tmp, &self.file).map_err(|e| format!("freeze rename failed: {e}"))
    }

    /// Removes the record from both homes, so a local copy an older build
    /// wrote cannot re-surface after the primary row goes.
    fn remove(&self) -> Result<(), String> {
        if let Some(remote) = &self.primary {
            match remote.execute(&format!("DELETE FROM {TABLE}"), &[]) {
                Ok(_) => {}
                Err(e) if e.contains("no such table") => {}
                Err(e) => return Err(e),
            }
        }
        match std::fs::remove_file(&self.file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
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
fn read_record(store: &Store) -> Result<Option<Record>, String> {
    let Some((text, path)) = store.read_text()? else {
        return Ok(None);
    };
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
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
                "{path} is not a version-1 or version-{STATE_VERSION} freeze record"
            ))
        }
    };
    if sha.is_none() && prs.is_empty() {
        return Err(format!("{path} names no pin"));
    }
    Ok(Some(Record {
        subject: text_of("subject").unwrap_or_else(|| "unnamed freeze".to_string()),
        set_by: text_of("set_by").unwrap_or_default(),
        repo,
        sha,
        prs,
    }))
}

/// A live user ruling at `subject` (`decision_index::is_user_ruling`): the
/// one row that lifts.
fn is_operator_lift(row: &Value, subject: &str) -> bool {
    decision_index::is_user_ruling(row)
        && row
            .get("subject")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case(subject))
}

fn lift_row<'a>(index: &'a Index, subject: &str) -> Option<&'a Value> {
    index.rows.iter().find(|row| is_operator_lift(row, subject))
}

/// `Ok(true)` when the user lifted this record's pin.
fn is_lifted(record: &Record, laws: Laws) -> Result<bool, String> {
    let index = laws().map_err(|e| format!("the decision store is unreadable ({e})"))?;
    Ok(lift_row(&index, &record.lift_subject()).is_some())
}

/// The verdict for one PR. `repo` is the PR's `owner/repo`; `None` (the
/// pr-watch pre-check sends none) reads as inside the freeze's repo.
fn verdict_for_in(store: &Store, pr: Option<u64>, repo: Option<&str>, laws: Laws) -> Verdict {
    let record = match read_record(store) {
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
        "only the user lifts it. Ask the user in chat; when they say lift, record \
         their exact words: `fno inbox law set '{lift}' 'lift' --rationale 'user in chat: \
         <their words>'`. At a terminal: `fno inbox decide '{lift}' 'lift' --authority operator`"
    )
}

/// True when a live freeze holds this PR. The pr-nudge ladder reads it so
/// a frozen PR gets no "drive it to merge" retry. An unreadable record
/// reads false here: the merge owner still fails closed on its own read.
pub(crate) fn holds_pr(home: &AgentsHome, pr: u64, repo: Option<&str>) -> bool {
    let store = crate::store_remote::configured().map(|primary| Store {
        file: record_path(&home.root()),
        primary,
    });
    store.is_ok_and(|store| {
        matches!(
            verdict_for_in(&store, Some(pr), repo, &decision_index::default_store_live),
            Verdict::Frozen { .. }
        )
    })
}

/// The spawn-time freeze line for `repo`, or None. Same record + lift read
/// as `holds_pr`, asked about the repo (pr None) instead of one PR; an
/// unreadable record or store renders nothing, because this line informs a
/// seed while the merge gate still fails closed on its own read.
pub(crate) fn active_freeze_line(home: &AgentsHome, repo: Option<&str>) -> Option<String> {
    let store = crate::store_remote::configured()
        .ok()
        .map(|primary| Store {
            file: record_path(&home.root()),
            primary,
        })?;
    active_freeze_line_in(&store, repo, &decision_index::default_store_live)
}

/// `active_freeze_line` over injected parts (tests).
fn active_freeze_line_in(store: &Store, repo: Option<&str>, laws: Laws) -> Option<String> {
    // An unknown slug sees only a version-1 record, which freezes every
    // repo; a repo-scoped freeze is not asked about a repo nobody named.
    // (verdict_for_in's repo=None reads as inside, the pr-watch contract -
    // the seed line needs the strict reading, so it gates here.)
    if let (Some(_), None) = (read_record(store).ok().flatten().and_then(|r| r.repo), repo) {
        return None;
    }
    match verdict_for_in(store, None, repo, laws) {
        Verdict::Frozen {
            subject,
            set_by,
            lift,
        } => {
            let who = if set_by.is_empty() {
                String::new()
            } else {
                format!(", set_by {set_by}")
            };
            Some(format!(
                "- {subject} (merge freeze{who}): pinned at {lift}; only the user lifts it"
            ))
        }
        _ => None,
    }
}

/// The merge owner's probe. Frozen refuses (the owner's fail_closed reads it
/// as held, because the user's lift clears it) and disarms an armed queue, which
/// would otherwise merge server side with no fno read. Unavailable is
/// inconclusive, which also fails closed.
pub(crate) fn probe(facts: &PrFacts) -> ProbeOutcome {
    let repo = crate::merge_grant::repo_slug_from_pr_url(&facts.url);
    let verdict = match Store::from_env() {
        Ok(store) => verdict_for_in(
            &store,
            Some(facts.number),
            repo.as_deref(),
            &decision_index::default_store_live,
        ),
        Err(detail) => Verdict::Unavailable { detail },
    };
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
    match Store::from_env() {
        Ok(store) => run_in(
            &store,
            op,
            payload,
            &decision_index::default_store_live,
            &real_gh,
        ),
        Err(e) => receipt(
            "refused",
            5,
            format!("the freeze store is unreadable ({e})"),
        )
        .to_string(),
    }
}

fn real_gh(args: &[&str]) -> Result<(bool, String), String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(args);
    let out = crate::gh_budget::output(cmd).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

fn run_in(store: &Store, op: &str, payload: &Value, laws: Laws, gh: GhRun) -> String {
    match op.strip_prefix("freeze-").unwrap_or(op) {
        "set" => set_in(store, payload, laws, gh),
        "clear" => clear_in(store, payload, laws),
        "check" => check_in(store, payload, laws),
        other => receipt("refused", 2, format!("unknown freeze op: {other}")).to_string(),
    }
}

fn receipt(outcome: &str, code: i32, detail: impl Into<String>) -> Value {
    json!({"outcome": outcome, "exit_code": code, "detail": detail.into()})
}

fn payload_str<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

fn set_in(store: &Store, payload: &Value, laws: Laws, gh: GhRun) -> String {
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
    match read_record(store) {
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
    if let Err(e) = store.write(&format!("{record}\n")) {
        return receipt("error", 2, e).to_string();
    }
    // Readback: the writer proves the record says what it wrote.
    if read_record(store).ok().flatten().as_ref() != Some(&next) {
        let _ = store.remove();
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

/// Removes the record only on a live operator ruling at its lift subject,
/// named by decision id. The lift itself already clears every gate read;
/// this op only tidies the file.
fn clear_in(store: &Store, payload: &Value, laws: Laws) -> String {
    let evidence = payload_str(payload, "evidence").unwrap_or("").trim();
    let record = match read_record(store) {
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
    let lawful = row.is_some_and(|r| is_operator_lift(r, &lift));
    if !lawful {
        return receipt(
            "refused",
            3,
            format!(
                "clear needs --evidence naming a live operator ruling at {lift}, got \
                 '{evidence}'; {}",
                lift_doors(&lift)
            ),
        )
        .to_string();
    }
    if let Err(e) = store.remove() {
        return receipt("error", 2, format!("freeze clear failed: {e}")).to_string();
    }
    let mut out = receipt("lifted", 0, "");
    if let Some(obj) = out.as_object_mut() {
        obj.insert("evidence".into(), json!(evidence));
    }
    out.to_string()
}

fn check_in(store: &Store, payload: &Value, laws: Laws) -> String {
    let pr = payload.get("pr").and_then(Value::as_u64);
    let repo = payload_str(payload, "repo");
    match verdict_for_in(store, pr, repo, laws) {
        Verdict::Clear => receipt("clear", 0, "").to_string(),
        Verdict::Frozen {
            subject,
            set_by,
            lift,
        } => receipt("frozen", 0, format!("{subject} {set_by}; lift at {lift}")).to_string(),
        Verdict::Unavailable { detail } => receipt("unavailable", 0, detail).to_string(),
    }
}

/// Moves a live freeze's sha to `sha` when the release record pins it for
/// `repo`. `Ok(None)` is no freeze, or a freeze over another repo. Only `pin.sha` changes: the sha admits no PR,
/// so the move never widens the freeze, and the new pin key gives a new
/// lift subject, so a lifted freeze re-arms.
pub(crate) fn repin_sha(sha: &str, repo: &str) -> Result<Option<String>, String> {
    repin_sha_in(&Store::from_env()?, sha, repo)
}

fn repin_sha_in(store: &Store, sha: &str, repo: &str) -> Result<Option<String>, String> {
    let Some((text, path)) = store.read_text()? else {
        return Ok(None);
    };
    let mut v: Value = serde_json::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
    if v.get("version").and_then(Value::as_u64) != Some(u64::from(STATE_VERSION)) {
        return Err(format!(
            "{path} is not a version-{STATE_VERSION} freeze; it has no sha pin"
        ));
    }
    let frozen = v.get("repo").and_then(Value::as_str).unwrap_or("");
    if !frozen.eq_ignore_ascii_case(repo) {
        return Ok(None);
    }
    v["pin"]["sha"] = json!(sha);
    store.write(&format!("{v}\n"))?;
    let record = read_record(store)?.ok_or("the freeze vanished after the repin")?;
    if record.sha.as_deref() != Some(sha) {
        return Err("freeze readback did not match the new sha".to_string());
    }
    Ok(Some(record.lift_subject()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    const SHA: &str = "abc123abc123abc123abc123abc123abc123abc1";

    fn home(test: &str) -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let root = dir.path().join(test);
        std::fs::create_dir_all(&root).unwrap();
        let store = Store {
            file: record_path(&root),
            primary: None,
        };
        (dir, store)
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

    fn set(home: &Store, sha: &str, laws: Laws) -> Value {
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

        // AC2: only an operator row at this pin's subject lifts it.
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

        // The spawn-time seed line: a frozen repo renders it with its pin, a
        // lifted one renders none, and another repo sees none either.
        let line = active_freeze_line_in(&home, Some("o/r"), &empty)
            .expect("a frozen repo renders the seed line");
        assert!(line.contains("rc (merge freeze, set_by team)"), "{line}");
        assert!(line.contains(&format!("pinned at {lift}")), "{line}");
        assert!(active_freeze_line_in(&home, Some("o/r"), &lifted).is_none());
        assert!(active_freeze_line_in(&home, Some("p/q"), &empty).is_none());
        // An unknown slug is not inside a repo-scoped freeze: it renders
        // none, and only a version-1 all-repo record could bind it.
        assert!(active_freeze_line_in(&home, None, &empty).is_none());
        // Any harness session can mint a bare chat_attested row, so it holds;
        // the same row carrying the user's chat words lifts.
        let chat = store(vec![row("d-4", &lift, "chat_attested")]);
        assert!(matches!(
            verdict_for_in(&home, Some(8), Some("o/r"), &chat),
            Verdict::Frozen { .. }
        ));
        let mut quoted = row("d-5", &lift, "chat_attested");
        quoted["rationale"] = json!("user in chat: lift it");
        let quoted = store(vec![quoted]);
        assert_eq!(
            verdict_for_in(&home, Some(8), Some("o/r"), &quoted),
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
            free.contains("--authority operator") && !free.contains("/fno:law"),
            "{free}"
        );
        let by_chat = run_in(
            &home,
            "freeze-clear",
            &json!({"evidence": "d-4"}),
            &chat,
            &no_gh,
        );
        assert!(by_chat.contains("\"refused\""), "{by_chat}");
        assert!(home.file.exists());
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
        assert!(!home.file.exists());

        unreadable_and_v1_contract();
        probe_disarm_contract();
    }

    fn unreadable_and_v1_contract() {
        let (_dir, home) = home("freeze-v1");
        let empty = store(vec![]);
        std::fs::write(&home.file, "{").unwrap();
        assert!(matches!(
            verdict_for_in(&home, Some(42), None, &empty),
            Verdict::Unavailable { .. }
        ));
        std::fs::write(
            &home.file,
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
    fn release_record_repin_moves_the_freeze_sha() {
        let (_dir, home) = home("repin");
        let empty = store(vec![]);
        let moved = "def456def456def456def456def456def456def4";
        assert_eq!(repin_sha_in(&home, moved, "o/r"), Ok(None));
        assert_eq!(set(&home, SHA, &empty)["outcome"], "frozen");
        let old_lift = format!("merge-freeze:o/r@sha:{},prs:7", &SHA[..12]);

        // AC3: the freeze reads B, admits the same PRs, and the old lift no
        // longer clears it.
        let new_lift = format!("merge-freeze:o/r@sha:{},prs:7", &moved[..12]);
        assert_eq!(repin_sha_in(&home, moved, "p/q"), Ok(None));
        assert_eq!(repin_sha_in(&home, moved, "o/r"), Ok(Some(new_lift)));
        let record = read_record(&home).unwrap().unwrap();
        assert_eq!(record.sha.as_deref(), Some(moved));
        assert_eq!(record.prs, vec![7]);
        assert_eq!(
            verdict_for_in(&home, Some(7), Some("o/r"), &empty),
            Verdict::Clear
        );
        let old = store(vec![row("d-1", &old_lift, "operator")]);
        assert!(matches!(
            verdict_for_in(&home, Some(8), Some("o/r"), &old),
            Verdict::Frozen { .. }
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
        std::fs::remove_file(&home.file).unwrap();
        *fail_merge.borrow_mut() = true;
        let out: Value =
            serde_json::from_str(&run_in(&home, "freeze-set", &payload, &empty, &gh)).unwrap();
        assert_eq!(out["outcome"], "frozen", "{out}");
        assert!(out["disarm_failed"][0]
            .as_str()
            .unwrap()
            .contains("--disable-auto by hand"));
        assert!(home.file.exists());
    }

    #[test]
    fn freeze_on_the_primary_binds_every_machine() {
        let primary = crate::store_remote::test_primary::start();
        let on = |test: &str| {
            let (dir, local) = home(test);
            let store = Store {
                file: local.file,
                primary: Some(primary.remote.clone()),
            };
            (dir, store)
        };
        let (_a_dir, macbook) = on("macbook");
        let (_b_dir, imac) = on("imac");
        let empty = store(vec![]);

        // A set on one machine holds an off-pin merge on the other, and
        // writes no local file on either.
        assert_eq!(set(&macbook, SHA, &empty)["outcome"], "frozen");
        assert!(!macbook.file.exists() && !imac.file.exists());
        assert!(matches!(
            verdict_for_in(&imac, Some(8), Some("o/r"), &empty),
            Verdict::Frozen { .. }
        ));
        assert!(set(&imac, SHA, &empty)["detail"]
            .as_str()
            .unwrap()
            .contains("already active"));

        // A clear on the other machine lifts it for both.
        let lift = format!("merge-freeze:o/r@sha:{},prs:7", &SHA[..12]);
        let ruled = store(vec![row("d-1", &lift, "operator")]);
        let cleared = run_in(
            &imac,
            "freeze-clear",
            &json!({"evidence": "d-1"}),
            &ruled,
            &no_gh,
        );
        assert!(cleared.contains("\"lifted\""), "{cleared}");
        assert_eq!(
            verdict_for_in(&macbook, Some(8), Some("o/r"), &empty),
            Verdict::Clear
        );

        // A record an older build left in the local file still holds.
        std::fs::write(
            &imac.file,
            r#"{"version":1,"subject":"old","set_by":"t","allow":[5]}"#,
        )
        .unwrap();
        assert!(matches!(
            verdict_for_in(&imac, Some(6), Some("o/r"), &empty),
            Verdict::Frozen { .. }
        ));

        // An unreachable primary fails closed.
        let dead = Store {
            file: macbook.file.clone(),
            primary: Some(crate::store_remote::test_primary::dead()),
        };
        assert!(matches!(
            verdict_for_in(&dead, Some(8), Some("o/r"), &empty),
            Verdict::Unavailable { .. }
        ));
    }
}

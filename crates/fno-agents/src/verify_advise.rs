//! Ship-gate verifier advisory: judge the shipped diff against the plan's
//! acceptance criteria and record a `verifier_verdict` event plus a ledger
//! stamp. The event is canonical; the ledger row is a denormalized
//! convenience. Advisory-only: a spawn failure, timeout, or parse miss
//! records verdict `error` (a doc ship or an AC-free plan records
//! `not_applicable`), and no failure on this path wedges the loop.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::events::now_rfc3339 as now_rfc3339_utc;

const SPAWN_TIMEOUT_S: u64 = 90;

fn ac_heading() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^#{2,4}\s*Acceptance [Cc]riteria").expect("ac heading regex is valid")
    })
}

fn section_end() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^#{1,2}\s").expect("section end regex is valid"))
}

fn ac_label() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\bAC\d+[A-Za-z-]*\b").expect("ac label regex is valid"))
}

fn verdict_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)VERDICT:\s*(pass|concerns|fail)\b").expect("verdict regex is valid")
    })
}

/// Extract the acceptance-criteria text from a plan file. Ok(None) = no plan
/// or no ACs; an exists-but-unreadable doc is Err so the caller records
/// verdict `error`, never a false `not_applicable`.
pub fn read_plan_acs(plan_path: &Path) -> Result<Option<String>, String> {
    if !plan_path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(plan_path).map_err(|e| format!("plan unreadable: {e}"))?;
    // Prefer the dedicated section; only its FIRST heading opens it, and a
    // deeper heading inside is content. Fall back to AC-labelled lines when
    // the section is absent or empty.
    let mut section: Vec<&str> = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        if !in_section {
            if ac_heading().is_match(line) {
                in_section = true;
            }
            continue;
        }
        if section_end().is_match(line) {
            break;
        }
        section.push(line);
    }
    let section = section.join("\n").trim().to_string();
    if !section.is_empty() {
        return Ok(Some(section));
    }
    let labelled: Vec<&str> = text.lines().filter(|l| ac_label().is_match(l)).collect();
    if labelled.is_empty() {
        Ok(None)
    } else {
        Ok(Some(labelled.join("\n")))
    }
}

/// `git <args>` in `cwd`; empty string on any failure (the `_git` shape).
fn git_out(cwd: &Path, args: &[&str]) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args).current_dir(cwd);
    match crate::bounded_cmd::output_with_timeout_result(cmd, 30) {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

pub fn build_prompt(acs: &str, diffstat: &str, commits: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are an independent verification agent. Objectively judge whether ");
    prompt.push_str("the shipped diff plausibly satisfies the plan's acceptance criteria. ");
    prompt.push_str("Be factual; do not trust claims. You may Read files and run read-only ");
    prompt.push_str("commands to check.\n\n");
    prompt.push_str(&format!("## Acceptance criteria\n{acs}\n\n"));
    prompt.push_str(&format!(
        "## Ship diffstat (origin/main...HEAD)\n{diffstat}\n\n"
    ));
    prompt.push_str(&format!("## Commits\n{commits}\n\n"));
    prompt.push_str("Reply with your reasoning, then a final line exactly of the form\n");
    prompt.push_str("VERDICT: pass|concerns|fail\n");
    prompt.push_str("(pass = criteria met; concerns = partially met or unverifiable; ");
    prompt.push_str("fail = a criterion is clearly not met).");
    prompt
}

pub fn parse_verdict(text: &str) -> Option<String> {
    verdict_re()
        .captures(text)
        .map(|c| c[1].to_ascii_lowercase())
}

/// One-shot headless verifier through the sanctioned spawn surface. Ok
/// carries (exit, reply, stderr); Err names the failure with the gate
/// refusal detail.
pub(crate) fn spawn_verifier(
    prompt: &str,
    cwd: &Path,
    session_id: &str,
) -> Result<(i32, String, String), String> {
    let short: String = session_id.chars().take(12).collect();
    let name = if short.is_empty() {
        "verifier-advise-adhoc".to_string()
    } else {
        format!("verifier-advise-{short}")
    };
    let mut argv = vec![
        "agents".to_string(),
        "spawn".to_string(),
        "--name".to_string(),
        name,
        "--harness".to_string(),
        "claude".to_string(),
        "--once".to_string(),
        "--cwd".to_string(),
        cwd.display().to_string(),
        "--timeout".to_string(),
        SPAWN_TIMEOUT_S.to_string(),
    ];
    // argv-fence: exempt - `agents spawn` reserves `--` for its passthrough
    // lane, so no fence exists; the seed is a template-rendered verifier
    // prompt, never user input, and cannot lead with a flag token.
    argv.push(prompt.to_string());
    // The model rides the passthrough lane in claude's own spelling: the
    // spawn's --model axis is retired.
    argv.extend(["--".to_string(), "--model".to_string(), "haiku".to_string()]);
    let (rc, stdout, stderr) = crate::blueprint_judge::headless_spawn(&argv, SPAWN_TIMEOUT_S + 30)?;
    if rc != 0 {
        let detail = crate::backlog::advance::gate_refusal_detail(&stderr);
        return Err(format!("verifier spawn exit {rc}: {detail}"));
    }
    Ok((rc, stdout, stderr))
}

/// The committed `verifier_verdict` lines of one journal: store rows, with
/// raw pre-store bytes as the fallback read.
fn read_event_lines(journal: &Path) -> Result<Vec<String>, String> {
    crate::event_store::import_all(journal)?;
    let q = crate::event_store::EventQuery::of_types(&["verifier_verdict"]);
    let rows = crate::event_store::query_events(journal, &q)?;
    Ok(rows.into_iter().map(|r| r.line).collect())
}

/// The journal's verdict lines: the file's text when it exists (importing it
/// into the store first), else the sibling store's rows, since the backfill
/// commits there without creating the file. Gating on one source existing
/// keeps `import_all`'s create-on-read side effect off a path with neither.
fn verdict_lines(events_path: &Path) -> Option<Vec<String>> {
    if events_path.exists() {
        let lines = match read_event_lines(events_path) {
            Ok(lines) => lines,
            Err(_) => match fs::read_to_string(events_path) {
                Ok(text) => text.lines().map(str::to_string).collect(),
                Err(_) => return None,
            },
        };
        return Some(lines);
    }
    let store = crate::event_store::try_store_path(events_path).ok()?;
    if !store.exists() {
        return None;
    }
    let q = crate::event_store::EventQuery::of_types(&["verifier_verdict"]);
    let rows = crate::event_store::query_events(events_path, &q).ok()?;
    Some(rows.into_iter().map(|r| r.line).collect())
}

/// The verdict already recorded for `session_id`, walking forward so the
/// LAST non-empty verdict wins.
fn recorded_verdict(session_id: &str, events_path: &Path) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let lines = verdict_lines(events_path)?;
    let mut verdict = None;
    for line in &lines {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if row.get("type").and_then(|v| v.as_str()) != Some("verifier_verdict") {
            continue;
        }
        if row.pointer("/data/session_id").and_then(|v| v.as_str()) != Some(session_id) {
            continue;
        }
        if let Some(v) = row.pointer("/data/verdict").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                verdict = Some(v.to_string());
            }
        }
    }
    verdict
}

/// Denormalize the verdict onto this session's ledger rows (the same
/// `/tmp` flock the cost writer holds). Best-effort: every failure prints
/// and returns false; the event is canonical. Dropping the lock file
/// releases the flock on every return path.
fn stamp_ledger(session_id: &str, verdict: &str, ledger_path: &Path) -> bool {
    if session_id.is_empty() || !ledger_path.exists() {
        return false;
    }
    use std::os::fd::AsRawFd;
    let lock_file = match fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(crate::ledger_axes::LOCK_PATH)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("verify_advise: ledger stamp failed: {e}");
            return false;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            break;
        }
        if Instant::now() >= deadline {
            eprintln!("verify_advise: ledger lock busy; skipping stamp (event is canonical)");
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = match fs::read_to_string(ledger_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("verify_advise: ledger stamp failed: {e}");
            return false;
        }
    };
    let mut doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let has_entries = doc.get("entries").is_some_and(|v| v.is_array());
    let entries: &mut Vec<serde_json::Value> = if has_entries {
        doc.get_mut("entries")
            .and_then(|v| v.as_array_mut())
            .unwrap()
    } else {
        match doc.as_array_mut() {
            Some(a) => a,
            None => return false,
        }
    };
    let mut hit = false;
    for entry in entries.iter_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };
        let matches = obj.get("fno_id").and_then(|v| v.as_str()) == Some(session_id)
            || obj.get("session_id").and_then(|v| v.as_str()) == Some(session_id);
        if matches {
            obj.insert(
                "verifier_verdict".to_string(),
                serde_json::Value::String(verdict.to_string()),
            );
            hit = true;
        }
    }
    if !hit {
        return false;
    }
    match crate::ledger_axes::write_ledger(ledger_path, &doc) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("verify_advise: ledger stamp failed: {e}");
            false
        }
    }
}

/// The spawn seam finalize passes its own verifier through.
pub(crate) type Spawn<'a> = &'a dyn Fn(&str, &Path, &str) -> Result<(i32, String, String), String>;

fn decide_verdict(
    reason: &str,
    plan_path: Option<&str>,
    cwd: &Path,
    session_id: &str,
    spawn: Spawn<'_>,
) -> String {
    if reason == "DoneAdvisory" {
        return "not_applicable".to_string();
    }
    let Some(plan_path) = plan_path.filter(|p| !p.is_empty()) else {
        return "not_applicable".to_string();
    };
    let abs = if Path::new(plan_path).is_absolute() {
        PathBuf::from(plan_path)
    } else {
        cwd.join(plan_path)
    };
    let acs = match read_plan_acs(&abs) {
        Ok(Some(acs)) => acs,
        Ok(None) => return "not_applicable".to_string(),
        Err(e) => {
            eprintln!("verify_advise: {e}");
            return "error".to_string();
        }
    };
    let mut diffstat = git_out(cwd, &["diff", "--stat", "origin/main...HEAD"]);
    if diffstat.is_empty() {
        diffstat = "(diff unavailable)".to_string();
    }
    let mut commits = git_out(cwd, &["log", "--oneline", "origin/main..HEAD"]);
    if commits.is_empty() {
        commits = "(log unavailable)".to_string();
    }
    let prompt = build_prompt(&acs, &diffstat, &commits);
    match spawn(&prompt, cwd, session_id) {
        Ok((_, reply, _)) => parse_verdict(&reply).unwrap_or_else(|| "error".to_string()),
        Err(e) => {
            eprintln!("verify_advise: verifier failed: {e}");
            "error".to_string()
        }
    }
}

// Field order is the event's key order: serde serializes struct fields in
// declaration order, and None renders as null.
#[derive(serde::Serialize)]
struct VerdictEvent<'a> {
    ts: String,
    #[serde(rename = "type")]
    event_type: &'static str,
    source: &'static str,
    data: VerdictData<'a>,
}

#[derive(serde::Serialize)]
struct VerdictData<'a> {
    graph_node_id: Option<&'a str>,
    pr_number: Option<u64>,
    verdict: &'a str,
    source: &'a str,
    session_id: &'a str,
}

pub(crate) struct AdviseArgs {
    pub(crate) node_id: Option<String>,
    pub(crate) plan_path: Option<String>,
    pub(crate) session_id: String,
    pub(crate) reason: String,
    pub(crate) cwd: PathBuf,
    pub(crate) project_events: PathBuf,
    pub(crate) global_events: PathBuf,
    pub(crate) ledger: PathBuf,
}

/// Record the advisory verdict: reuse a verdict already in the project log,
/// otherwise spawn the verifier once; mirror the event to every log still
/// missing it; stamp the ledger best-effort. Never fails the caller.
pub(crate) fn run(args: &AdviseArgs, spawn: Spawn<'_>) -> String {
    let prior = recorded_verdict(&args.session_id, &args.project_events);
    let verdict = match prior {
        Some(v) => {
            eprintln!("verify_advise: verdict already recorded ({v}); backfilling missing sinks");
            v
        }
        None => decide_verdict(
            &args.reason,
            args.plan_path.as_deref(),
            &args.cwd,
            &args.session_id,
            spawn,
        ),
    };
    let node_id = args.node_id.as_deref().filter(|s| !s.is_empty());
    let mut paths: Vec<&Path> = vec![args.project_events.as_path()];
    if args.global_events != args.project_events {
        paths.push(args.global_events.as_path());
    }
    let missing: Vec<&Path> = paths
        .into_iter()
        .filter(|p| recorded_verdict(&args.session_id, p).is_none())
        .collect();
    if !missing.is_empty() {
        let pr_number = crate::finalize::gh_pr_ref(&args.cwd).map(|(n, _)| n);
        let event = VerdictEvent {
            ts: now_rfc3339_utc(),
            event_type: "verifier_verdict",
            source: "target",
            data: VerdictData {
                graph_node_id: node_id,
                pr_number,
                verdict: &verdict,
                source: "ship-gate",
                session_id: &args.session_id,
            },
        };
        match serde_json::to_value(&event) {
            Ok(value) => {
                for p in &missing {
                    if let Err(e) =
                        crate::claims::append_event_line(p, &value, Duration::from_secs(2))
                    {
                        eprintln!("verify_advise: event emit to {} failed: {e}", p.display());
                    }
                }
            }
            Err(e) => {
                eprintln!("verify_advise: event emit failed: {e}");
            }
        }
    }
    let _ = stamp_ledger(&args.session_id, &verdict, &args.ledger);
    eprintln!("verify_advise: verdict={verdict}");
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests 1-3 touch the shared /tmp ledger lock the production stamp path
    // holds, and the lib suite runs multithreaded: a sibling stamp would
    // race the busy-lock expectation. Serialize the lock-touching tests.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn run_records_prior_event_and_backfills_global() {
        let _g = lock_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let project = tmp.path().join("project-events.jsonl");
        fs::write(
            &project,
            concat!(
                r#"{"ts":"t","type":"verifier_verdict","source":"target","data":{"graph_node_id":null,"pr_number":null,"verdict":"pass","source":"ship-gate","session_id":"S1"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let global = tmp.path().join("global-events.jsonl");
        let ledger = tmp.path().join("ledger.json");
        let cwd = tmp.path().join("cwd");
        fs::create_dir_all(&cwd).unwrap();
        let never_spawn =
            |_prompt: &str, _cwd: &Path, _sid: &str| -> Result<(i32, String, String), String> {
                panic!("no verifier spawn when a prior verdict is recorded");
            };
        let args = AdviseArgs {
            node_id: Some(String::new()),
            plan_path: None,
            session_id: "S1".to_string(),
            reason: "DonePRGreen".to_string(),
            cwd: cwd.clone(),
            project_events: project.clone(),
            global_events: global.clone(),
            ledger: ledger.clone(),
        };
        let verdict = run(&args, &never_spawn);
        assert_eq!(verdict, "pass");
        let q = crate::event_store::EventQuery::of_types(&["verifier_verdict"]);
        let rows = crate::event_store::query_events(&global, &q).unwrap();
        assert_eq!(rows.len(), 1, "backfill commits exactly one store row");
        let backfilled = rows[0].line.clone();
        assert!(backfilled.contains(r#""type":"verifier_verdict""#));
        assert!(backfilled.contains(r#""session_id":"S1""#));
        assert!(backfilled.contains(r#""verdict":"pass""#));
        assert!(backfilled.contains(r#""source":"ship-gate""#));
        assert_eq!(fs::read_to_string(&project).unwrap().lines().count(), 1);
        assert!(!ledger.exists());
        let _ = run(&args, &never_spawn);
        let after = crate::event_store::query_events(&global, &q).unwrap();
        assert_eq!(after.len(), rows.len(), "second run must not re-backfill");
        assert_eq!(after[0].line, backfilled);
        assert_eq!(fs::read_to_string(&project).unwrap().lines().count(), 1);
    }

    #[test]
    fn stamp_ledger_refuses_when_lock_busy() {
        let _g = lock_guard();
        use std::os::fd::AsRawFd;
        let tmp = tempfile::TempDir::new().unwrap();
        let ledger = tmp.path().join("ledger.json");
        fs::write(&ledger, r#"{"entries":[{"session_id":"S1"}]}"#).unwrap();
        let blocker = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(crate::ledger_axes::LOCK_PATH)
            .unwrap();
        unsafe {
            assert_eq!(
                libc::flock(blocker.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB),
                0
            );
        }
        let started = std::time::Instant::now();
        assert!(!stamp_ledger("S1", "pass", &ledger));
        assert!(started.elapsed() >= std::time::Duration::from_secs(4));
        drop(blocker); // closing the fd releases the flock
        assert!(stamp_ledger("S1", "pass", &ledger));
    }

    #[test]
    fn decide_verdict_unparseable_reply_is_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let d = tmp.path();
        fs::write(
            d.join("plan.md"),
            "## Acceptance Criteria\n\nAC1-HP: it works\n",
        )
        .unwrap();
        let timeout_spawn =
            |_prompt: &str, _cwd: &Path, _sid: &str| -> Result<(i32, String, String), String> {
                Ok((
                    124,
                    String::new(),
                    "reader timed out after 120s: model lane slow or provider wait".to_string(),
                ))
            };
        assert_eq!(
            decide_verdict("DonePRGreen", Some("plan.md"), d, "S9", &timeout_spawn),
            "error"
        );
        let err_spawn =
            |_prompt: &str, _cwd: &Path, _sid: &str| -> Result<(i32, String, String), String> {
                Err("spawn broke".to_string())
            };
        assert_eq!(
            decide_verdict("DonePRGreen", Some("plan.md"), d, "S9", &err_spawn),
            "error"
        );
    }
}

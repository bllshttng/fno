//! The spend journal beside the request budget: who spent which GitHub
//! bucket, on what shape, admitted or refused.
//!
//! The budget ledger (`gh_budget.rs`) stamps `(ms, points)` and prunes at
//! 60 s, so it can never answer the question an hourly outage asks: WHO
//! drained the core bucket. This module appends one JSON line per
//! non-exempt verdict to `machine_locks_dir()/github-request-spend.jsonl`,
//! rotated at 8 MiB to one `.1` generation - about 22 hours of rows at the
//! measured 5,000 lines an hour. Like the ledger, a journal failure never
//! changes a verdict: append errors are ignored on the admission path.
//!
//! The journal is machine-local by construction (it sits beside the
//! ledger) and blind to every caller that never reaches admission: gh
//! spawned by Python `fno-py` straight off PATH, the Python proxy when
//! `FNO_AGENT_SELF` is unset, worker shells, and every other machine. The
//! `spend` op names those blind spots in each report rather than letting a
//! clean-sounding answer stand in for a partial one.
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::OnceLock;

const JOURNAL_NAME: &str = "github-request-spend.jsonl";
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;
const CALLER_HOPS: usize = 6;

/// Which GitHub bucket one argv spends from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bucket {
    Core,
    GraphQL,
    Exempt,
}

impl Bucket {
    fn word(self) -> &'static str {
        match self {
            Bucket::Core => "core",
            Bucket::GraphQL => "graphql",
            Bucket::Exempt => "exempt",
        }
    }
}

/// The bucket plus whether the read rides a conditional `If-None-Match`
/// header (a 304 on one costs no core).
pub(crate) fn bucket_for(argv: &[String]) -> (Bucket, bool) {
    if crate::gh_budget::points_for(argv) == 0 {
        return (Bucket::Exempt, false);
    }
    let conditional = is_conditional(argv);
    let cmd = crate::gh_budget::command_args(argv);
    match cmd.first().map(String::as_str) {
        // gh implements these over GraphQL, so they drain the GraphQL
        // bucket, not core.
        Some("api") if cmd.get(1).map(String::as_str) == Some("graphql") => {
            (Bucket::GraphQL, conditional)
        }
        Some("pr") => match cmd.get(1).map(String::as_str) {
            Some("view" | "list" | "status" | "checks" | "merge" | "ready" | "edit" | "review") => {
                (Bucket::GraphQL, conditional)
            }
            _ => (Bucket::Core, conditional),
        },
        _ => (Bucket::Core, conditional),
    }
}

/// A conditional read carries `-i` and an `If-None-Match:` header - the
/// exact shape `gh_etag::conditional_api_get` sends.
fn is_conditional(argv: &[String]) -> bool {
    let has_include = argv.iter().any(|a| a == "-i" || a == "--include");
    let has_match = argv.iter().enumerate().any(|(i, a)| {
        a.starts_with("If-None-Match:")
            || a.starts_with("--header=If-None-Match:")
            || ((a == "-H" || a == "--header")
                && argv
                    .get(i + 1)
                    .is_some_and(|n| n.starts_with("If-None-Match:")))
    });
    has_include && has_match
}

/// The shape a journal row names: for `api`, the path with the repo and
/// every unstable id folded out; otherwise the first two words. Flags and
/// `--jq` values drop, so one poller's 500 reads share one shape.
pub(crate) fn shape(argv: &[String]) -> String {
    let cmd = crate::gh_budget::command_args(argv);
    let mut body = if cmd.first().map(String::as_str) == Some("api") {
        match first_positional(&cmd[1..]) {
            Some(path) => format!("api {}", fold_path(path)),
            None => "api".to_string(),
        }
    } else {
        cmd.iter()
            .take(2)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    };
    if is_conditional(argv) {
        body.push_str(" [conditional]");
    }
    body
}

/// The first non-flag token, honoring the api flags that consume a value.
fn first_positional(tokens: &[String]) -> Option<&str> {
    let mut skip_next = false;
    for t in tokens {
        if skip_next {
            skip_next = false;
            continue;
        }
        if t.starts_with('-') {
            skip_next = matches!(
                t.as_str(),
                "-X" | "--method"
                    | "-H"
                    | "--header"
                    | "-f"
                    | "-F"
                    | "--field"
                    | "--raw-field"
                    | "--jq"
                    | "--cache"
                    | "--input"
            );
            continue;
        }
        return Some(t);
    }
    None
}

/// `repos/<owner>/<repo>`, `repos/{owner}/{repo}` and `repos/:owner/:repo`
/// fold to `repos/{repo}`; a digit run after a `/` folds to `N`; a 40-hex
/// run folds to `SHA`; `head=<x>` folds to `head=B`. Every other query
/// parameter stays verbatim.
fn fold_path(path: &str) -> String {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (path, None),
    };
    let mut segs: Vec<String> = route.split('/').map(str::to_string).collect();
    if segs.first().map(String::as_str) == Some("repos") && segs.len() >= 3 {
        segs[1] = "{repo}".to_string();
        segs.remove(2);
    }
    for seg in segs.iter_mut().skip(1) {
        if seg.len() == 40 && seg.chars().all(|c| c.is_ascii_hexdigit()) {
            *seg = "SHA".to_string();
        } else if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) {
            *seg = "N".to_string();
        }
    }
    let mut out = segs.join("/");
    if let Some(q) = query {
        out.push('?');
        out.push_str(
            &q.split('&')
                .map(|kv| {
                    if kv.starts_with("head=") {
                        "head=B"
                    } else {
                        kv
                    }
                })
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    out
}

/// The caller label for THIS process, computed once: `FNO_GH_CALLER` wins
/// outright when set (checked per call, so a test can pin it), otherwise
/// the process ancestry names the caller. The walk caches: `journal_verdict`
/// runs it under the ledger flock `admit` holds, and on a non-macOS host
/// each hop is a `ps` probe - twelve subprocess launches per admission is
/// serialization the budget cannot afford.
pub(crate) fn caller_label() -> String {
    if let Ok(v) = std::env::var("FNO_GH_CALLER") {
        if !v.is_empty() {
            return v;
        }
    }
    caller_label_cached().clone()
}

fn caller_label_cached() -> &'static String {
    static LABEL: OnceLock<String> = OnceLock::new();
    LABEL.get_or_init(|| {
        caller_label_from(&|pid| {
            crate::census::process_argv(pid).map(|argv| {
                let parent = crate::census::parent_pid(pid).unwrap_or(0);
                (argv, parent)
            })
        })
    })
}

/// The caller walk over an injected ancestry, so the test fixtures and the
/// live census share one spelling. At most `CALLER_HOPS` hops: the
/// interesting caller is always the near ancestry, and a long chain is a
/// shell anyway. The first ancestor that is not a transport names the
/// caller; a `fno-agents-daemon` anywhere in the window prefixes `daemon `.
pub(crate) fn caller_label_from(lookup: &dyn Fn(u32) -> Option<(Vec<String>, u32)>) -> String {
    caller_label_from_at(std::process::id(), lookup)
}

/// The same walk from an explicit start pid, so a test can walk a
/// synthetic ancestry instead of this process's real one.
pub(crate) fn caller_label_from_at(
    start: u32,
    lookup: &dyn Fn(u32) -> Option<(Vec<String>, u32)>,
) -> String {
    let mut pid = start;
    let mut label: Option<String> = None;
    let mut daemon = false;
    for _ in 0..CALLER_HOPS {
        let Some((argv, parent)) = lookup(pid) else {
            break;
        };
        if argv
            .first()
            .is_some_and(|a| file_basename(a) == "fno-agents-daemon")
        {
            daemon = true;
        }
        if label.is_none() {
            if let Label::Name(word) = label_word(&argv) {
                label = Some(word);
            }
        }
        if parent == 0 || parent == pid || (label.is_some() && daemon) {
            break;
        }
        pid = parent;
    }
    match (label, daemon) {
        (Some(word), true) => format!("daemon {word}"),
        (Some(word), false) => word,
        (None, true) => "daemon unknown".to_string(),
        (None, false) => "unknown".to_string(),
    }
}

enum Label {
    Skip,
    Name(String),
}

fn label_word(argv: &[String]) -> Label {
    // A jobs script reads as its own caller whether it runs via a shebang
    // (argv0 is the script) or through a shell (argv1 is the script). A
    // stop-hook script names the poll-class caller the reserve stands
    // down; the in-process admissions of the stop hook's own fno-agents
    // child label as `loop-check`, so both spellings are poll-class.
    for w in argv.iter().take(2) {
        if w.contains("/.claude/jobs/") && w.ends_with(".sh") {
            let base = file_basename(w);
            let stem = base.trim_end_matches(".sh");
            return Label::Name(format!("worker-script {stem}"));
        }
        if w.contains("/hooks/") && w.ends_with(".sh") && file_basename(w).contains("stop") {
            return Label::Name("hook stop".to_string());
        }
    }
    let Some((idx, prog)) = effective_program(argv) else {
        return Label::Skip;
    };
    let rest = &argv[(idx + 1).min(argv.len())..];
    match file_basename(prog).as_str() {
        // Transports: the door's own process, the adapters, the proxy, gh.
        "gh" | "fno-gh-loopcheck" | "fno-gh-coverage" | "fno-gh-proxy" | "fno-agents-daemon" => {
            Label::Skip
        }
        "fno-agents" => fno_agents_label(rest),
        // The Python twin and the Rust front door name the caller the same
        // way: the `fno` word plus the subcommand chain (do pr merge).
        "fno" | "fno-py" => {
            let words: Vec<&str> = rest
                .iter()
                .map(String::as_str)
                .filter(|w| !w.starts_with('-'))
                .take(3)
                .collect();
            Label::Name(
                format!("fno {}", words.join(" ").trim_end().to_string())
                    .trim_end()
                    .to_string(),
            )
        }
        base => {
            if matches!(base, "zsh" | "bash" | "sh" | "dash" | "ksh") {
                Label::Name("worker-shell".to_string())
            } else {
                let arg = rest
                    .iter()
                    .map(String::as_str)
                    .find(|w| !w.starts_with('-') && !w.starts_with('{'))
                    .unwrap_or("");
                if arg.is_empty() {
                    Label::Name(base.to_string())
                } else {
                    Label::Name(format!("{base} {arg}"))
                }
            }
        }
    }
}

/// The program word: argv0, or argv1 under a script interpreter (`python3
/// fno-py ...` is fno-py traffic).
fn effective_program(argv: &[String]) -> Option<(usize, &str)> {
    let first = argv.first()?;
    if is_interpreter(file_basename(first).as_str()) {
        argv.get(1).map(|p| (1usize, p.as_str()))
    } else {
        Some((0usize, first.as_str()))
    }
}

fn is_interpreter(base: &str) -> bool {
    base == "python"
        || base == "python3"
        || base.starts_with("python3.")
        || base == "perl"
        || base == "ruby"
        || base == "node"
}

fn fno_agents_label(args: &[String]) -> Label {
    let sub = args.first().map(String::as_str).unwrap_or("");
    match sub {
        "" | "fleet-incident" | "pr-worktree" => Label::Skip,
        "authorized-merge" => {
            // The spawner wrapper carries no JSON op and skips; an op names
            // the caller down to the wait flavor.
            let Some(payload) = args
                .iter()
                .skip(1)
                .find(|t| t.trim_start().starts_with('{'))
            else {
                return Label::Skip;
            };
            let Ok(v) = serde_json::from_str::<Value>(payload) else {
                return Label::Skip;
            };
            let Some(op) = v.get("op").and_then(Value::as_str) else {
                return Label::Skip;
            };
            match v.get("until").and_then(Value::as_str) {
                Some(until) => Label::Name(format!("authorized-merge {op} {until}")),
                None => Label::Name(format!("authorized-merge {op}")),
            }
        }
        _ => {
            let arg = args
                .iter()
                .skip(1)
                .map(String::as_str)
                .find(|w| !w.starts_with('-') && !w.starts_with('{') && !w.starts_with('['))
                .unwrap_or("");
            if arg.is_empty() {
                Label::Name(sub.to_string())
            } else {
                Label::Name(format!("{sub} {arg}"))
            }
        }
    }
}

fn file_basename(word: &str) -> String {
    std::path::Path::new(word)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(word)
        .to_string()
}

/// The recurring pollers the core reserve stands down first: everything
/// else (merge, push, create, finalize, operator shells) keeps the last
/// requests of the bucket.
pub(crate) fn is_poll_class(label: &str) -> bool {
    label.starts_with("daemon ")
        || label == "hook stop"
        || label == "loop-check"
        || label == "fno do pr wait"
        || label == "fno do pr watch"
        || label == "lead-rundown"
        || label == "lead-checkin"
        || label == "pr-heal"
        || label == "backlog reconcile"
        || label == "authorized-merge status-read"
        || label.starts_with("authorized-merge status-wait")
}

/// The journal beside the ledger: the same directory contract, so a budget
/// that protects the machine never moves with a project's config.
pub fn journal_path() -> PathBuf {
    crate::agents_config::machine_locks_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("fno-locks"))
        .join(JOURNAL_NAME)
}

/// One verdict's row, appended under the ledger flock `admit` already
/// holds. Exempt shapes never journal (a version probe is not spend), and
/// a temp-ledger caller (every unit test) writes nothing.
pub(crate) fn journal_verdict(
    // events-discipline:allow: the spend ledger, not an event journal
    path: &std::path::Path,
    argv: &[String],
    now: i64,
    verdict: &crate::gh_budget::Verdict,
) {
    if path != crate::gh_budget::ledger_path().as_path() {
        return;
    }
    let (bucket, conditional) = bucket_for(argv);
    if bucket == Bucket::Exempt {
        return;
    }
    let row = json!({
        "ts_ms": now,
        "caller": caller_label(),
        "shape": shape(argv),
        "bucket": bucket.word(),
        "conditional": conditional,
        "points": crate::gh_budget::points_for(argv),
        "verdict": if verdict.admitted { "admitted" } else { "refused" },
        "cause": verdict.cause,
    });
    let _ = append(&journal_path(), &row.to_string());
}

/// One JSON line, O_APPEND in a single `write_all` (a row stays under 512
/// bytes, so lines stay whole without a lock). Past 8 MiB the file renames
/// to `.1` first, replacing any older generation.
pub(crate) fn append(path: &std::path::Path, line: &str) -> Result<(), String> {
    use std::io::Write;
    if std::fs::metadata(path)
        .map(|m| m.len() > ROTATE_BYTES)
        .unwrap_or(false)
    {
        let rotated = PathBuf::from(format!("{}.1", path.display()));
        let _ = std::fs::rename(path, rotated);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    file.write_all(buf.as_bytes())
        .map_err(|e| format!("cannot append {}: {e}", path.display()))
}

/// The fixed blind-spot sentence every spend report carries: a partial
/// journal must never read as a complete answer.
const BLIND_SPOTS: &str = "gh spawned by Python fno-py straight off PATH, the Python proxy when FNO_AGENT_SELF is unset, worker shells, and every other machine never reach this journal";

/// One exempt `gh api rate_limit` read: GitHub prices it at 0, so the
/// probe answers even mid-outage and the report can name the real
/// account-wide state. A failed probe is an error payload, never a guess.
pub(crate) fn rate_limit_probe() -> Result<Value, String> {
    let out = crate::loopcheck::bounded_read(
        std::ffi::OsStr::new("gh"),
        &["api", "rate_limit"],
        std::path::Path::new("."),
        "spend rate_limit",
        std::time::Duration::from_secs(120),
    )
    .map_err(|e| crate::loopcheck::bounded_read_diagnostic("spend rate_limit", &e))?;
    if !out.status.success() {
        let tail: String = String::from_utf8_lossy(&out.stderr_tail)
            .chars()
            .rev()
            .take(200)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(tail);
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("unparseable rate_limit: {e}"))
}

/// The journal rows (the `.1` generation first) newer than `since_ms`.
/// The real journal's read takes the ledger's flock, so a rotation mid-read
/// cannot make one generation's rows vanish from the report.
fn read_rows(journal: &std::path::Path, since_ms: i64) -> Vec<Value> {
    let _lock = if journal == journal_path().as_path() {
        Some(crate::gh_budget::FileLock::try_acquire(
            &crate::gh_budget::lock_path(&crate::gh_budget::ledger_path()),
        ))
    } else {
        None
    };
    let older = PathBuf::from(format!("{}.1", journal.display()));
    let mut rows = Vec::new();
    for path in [older, journal.to_path_buf()] {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v.get("ts_ms")
                .and_then(Value::as_i64)
                .is_some_and(|ts| ts >= since_ms)
            {
                rows.push(v);
            }
        }
    }
    rows
}

/// The `spend` op body, with the clock and the probe outcome injected so
/// tests pass a temp journal: `github` is None when not requested,
/// `Some(Err)` when the exempt read failed, `Some(Ok)` with the
/// `gh api rate_limit` payload when it answered.
pub(crate) fn spend_report_at(
    journal: &std::path::Path,
    now: i64,
    hours: i64,
    github: Option<Result<Value, String>>,
) -> Value {
    let hours = hours.clamp(1, 24);
    let since = now - hours * 3_600_000;
    let rows = read_rows(journal, since);

    let mut admitted_core = 0i64;
    let mut admitted_core_conditional = 0i64;
    let mut admitted_graphql = 0i64;
    let mut refused: std::collections::BTreeMap<String, i64> = Default::default();
    struct CallerAgg {
        core: i64,
        core_conditional: i64,
        graphql: i64,
        refused: i64,
    }
    let mut by_caller: std::collections::BTreeMap<String, CallerAgg> = Default::default();
    let mut by_shape: std::collections::BTreeMap<(String, String, String, bool), i64> =
        Default::default();

    for r in &rows {
        let bucket = r.get("bucket").and_then(Value::as_str).unwrap_or("");
        let conditional = r
            .get("conditional")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let caller = r
            .get("caller")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let shape = r.get("shape").and_then(Value::as_str).unwrap_or("");
        let verdict = r.get("verdict").and_then(Value::as_str).unwrap_or("");
        let agg = by_caller.entry(caller.clone()).or_insert(CallerAgg {
            core: 0,
            core_conditional: 0,
            graphql: 0,
            refused: 0,
        });
        match (verdict, bucket, conditional) {
            ("admitted", "core", true) => {
                admitted_core_conditional += 1;
                agg.core_conditional += 1;
            }
            ("admitted", "core", false) => {
                admitted_core += 1;
                agg.core += 1;
            }
            ("admitted", "graphql", _) => {
                admitted_graphql += 1;
                agg.graphql += 1;
            }
            ("admitted", _, _) => {}
            ("refused", _, _) => {
                let cause = r
                    .get("cause")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                *refused.entry(cause).or_default() += 1;
                agg.refused += 1;
            }
            _ => {}
        }
        if verdict == "admitted" {
            // The shape table names spend: refused rows already sit in the
            // refused-by-cause map, and mixing them here would let a
            // refusal storm name the top shape.
            *by_shape
                .entry((caller, shape.to_string(), bucket.to_string(), conditional))
                .or_default() += 1;
        }
    }

    let mut callers: Vec<(String, CallerAgg)> = by_caller.into_iter().collect();
    callers.sort_by(|a, b| b.1.core.cmp(&a.1.core).then(a.0.cmp(&b.0)));

    let mut shapes: Vec<_> = by_shape.into_iter().collect();
    shapes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    shapes.truncate(25);

    let github_value = match github {
        None => None,
        Some(Err(e)) => Some(json!({"error": e})),
        Some(Ok(v)) => {
            let core = v.pointer("/resources/core");
            let limit = core.and_then(|c| c.get("limit")).and_then(Value::as_i64);
            let used = core.and_then(|c| c.get("used")).and_then(Value::as_i64);
            let remaining = core
                .and_then(|c| c.get("remaining"))
                .and_then(Value::as_i64);
            let reset = core.and_then(|c| c.get("reset")).and_then(Value::as_i64);
            match (limit, used, remaining, reset) {
                (Some(limit), Some(used), Some(remaining), Some(reset)) => {
                    let window_start_ms = (reset - 3_600) * 1000;
                    let mut window_core = 0i64;
                    let mut window_conditional = 0i64;
                    for r in &rows {
                        let in_window = r
                            .get("ts_ms")
                            .and_then(Value::as_i64)
                            .is_some_and(|ts| ts >= window_start_ms);
                        let bucket = r.get("bucket").and_then(Value::as_str) == Some("core");
                        let conditional = r
                            .get("conditional")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let admitted = r.get("verdict").and_then(Value::as_str) == Some("admitted");
                        if in_window && bucket && admitted {
                            if conditional {
                                window_conditional += 1;
                            } else {
                                window_core += 1;
                            }
                        }
                    }
                    Some(json!({
                        "core_limit": limit,
                        "core_used": used,
                        "core_remaining": remaining,
                        "core_reset_in_s": (reset - now / 1000).max(0),
                        "window_core_admitted": window_core,
                        "outside_ledger_core_min": (used - window_core - window_conditional).max(0),
                    }))
                }
                _ => Some(json!({"error": "rate_limit payload carried no resources.core"})),
            }
        }
    };

    let mut out = json!({
        "since_ms": since,
        "machine": crate::claims::hostname(),
        "rows": rows.len(),
        "admitted": {
            "core": admitted_core,
            "core_conditional": admitted_core_conditional,
            "graphql": admitted_graphql,
        },
        "refused": refused,
        "by_caller": callers
            .into_iter()
            .map(|(caller, agg)| json!({
                "caller": caller,
                "core": agg.core,
                "core_conditional": agg.core_conditional,
                "graphql": agg.graphql,
                "refused": agg.refused,
            }))
            .collect::<Vec<_>>(),
        "by_shape": shapes
            .into_iter()
            .map(|((caller, shape, bucket, conditional), count)| json!({
                "caller": caller,
                "shape": shape,
                "bucket": bucket,
                "conditional": conditional,
                "count": count,
            }))
            .collect::<Vec<_>>(),
        "blind_spots": BLIND_SPOTS,
    });
    if let Some(g) = github_value {
        out["github"] = g;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_report_aggregates_a_journal_and_survives_an_empty_one() {
        // AC2-HP: three core rows from one caller, one conditional row from
        // another, one row 2h old - hours 1 drops the old row and ranks
        // by core.
        let tmp = tempfile::tempdir().unwrap();
        let journal = tmp.path().join("github-request-spend.jsonl");
        let now = 1_760_000_000_000i64;
        let row = |ts_ms: i64, caller: &str, bucket: &str, cond: bool, verdict: &str| {
            json!({
                "ts_ms": ts_ms,
                "caller": caller,
                "shape": "api repos/{repo}/pulls/N",
                "bucket": bucket,
                "conditional": cond,
                "points": 1,
                "verdict": verdict,
                "cause": Value::Null,
            })
            .to_string()
        };
        let old_ts = now - 2 * 3_600_000;
        std::fs::write(
            &journal,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                row(now - 1000, "fno do pr info", "core", false, "admitted"),
                row(now - 2000, "fno do pr info", "core", false, "admitted"),
                row(now - 3000, "fno do pr info", "core", false, "admitted"),
                row(
                    now - 4000,
                    "authorized-merge status-wait review",
                    "core",
                    true,
                    "admitted"
                ),
                row(old_ts, "fno do pr info", "core", false, "admitted"),
            ),
        )
        .unwrap();

        let report = spend_report_at(&journal, now, 1, None);
        assert_eq!(report["rows"], 4, "the 2h-old row is excluded");
        assert_eq!(report["admitted"]["core"], 3);
        assert_eq!(report["admitted"]["core_conditional"], 1);
        let callers = report["by_caller"].as_array().unwrap();
        assert_eq!(callers[0]["caller"], "fno do pr info");
        assert_eq!(callers[0]["core"], 3);
        assert_eq!(
            report["by_shape"].as_array().unwrap().len(),
            2,
            "conditional and unconditional shapes rank separately"
        );
        assert!(report["blind_spots"].as_str().is_some());

        // AC2-EDGE: no journal file reads as an empty report, not an error.
        let absent = tmp.path().join("absent.jsonl");
        let report = spend_report_at(&absent, now, 1, None);
        assert_eq!(report["rows"], 0);
        assert_eq!(
            report["by_caller"].as_array().unwrap().len(),
            0,
            "empty lists, exit 0"
        );
        assert!(report["github"].is_null(), "no probe, no github key");

        // The github arm: the exempt probe's account-wide `used` minus this
        // journal's window rows (conditional rows subtract too) names the
        // outside spend as a lower bound.
        let report = spend_report_at(
            &journal,
            now,
            1,
            Some(Ok(json!({
                "resources": {
                    "core": {"limit": 5000, "used": 400, "remaining": 100, "reset": now / 1000 + 600}
                }
            }))),
        );
        assert_eq!(report["github"]["window_core_admitted"], 3);
        assert_eq!(
            report["github"]["outside_ledger_core_min"], 396,
            "400 used - 3 unconditional - 1 conditional"
        );
    }
}

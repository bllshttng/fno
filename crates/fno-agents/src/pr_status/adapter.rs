//! The fixed-purpose gh adapter behind `fno-agents gh-adapter
//! coverage|discretionary` (spawned under the logical names
//! `fno-gh-coverage` and `fno-gh-loopcheck`): metadata, CI, and review
//! coverage are translated from REST helpers, remaining GraphQL shapes run
//! through the serialized quota broker with a purpose fixed by the argv, so
//! an environment variable cannot promote a discretionary read into the
//! coverage reserve. Ported from `cli/src/fno/pr/_internal_gh.py` (the pr
//! port 7 retirement); the interface is "a gh-shaped argv in, gh-shaped
//! streams and an exit code out".

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::pr_status::gh_run::{run, RunResult};
use crate::pr_status::lists;
use crate::pr_status::quota;

/// The metadata field set whose reads translate to the REST pulls fetch.
/// The stop gate's `PR_VIEW_FIELDS` must stay inside it, or the gate's read
/// leaves REST for the broker and a spent budget refuses it.
pub(crate) const METADATA_FIELDS: [&str; 8] = [
    "state",
    "number",
    "headRefName",
    "headRefOid",
    "mergeable",
    "mergeStateStatus",
    "baseRefName",
    "author",
];
/// The coverage reads' field set: reviews + comments pages plus the pulls
/// head/base facts.
const COVERAGE_FIELDS: [&str; 4] = ["reviews", "comments", "headRefOid", "baseRefName"];

/// The argv option reader (`_internal_gh._option`): `--name value` or
/// `--name=value`.
fn option(args: &[String], name: &str) -> Option<String> {
    for (index, arg) in args.iter().enumerate() {
        if arg == name {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}

/// The cwd every adapter leg reads from: the caller's cwd when given, else
/// the process cwd (resolved once per call).
fn cwd_path(cwd: Option<&Path>) -> PathBuf {
    let owned = cwd.map(|p| p.to_string_lossy().into_owned());
    crate::pr_status::gh_run::cwd_or_dot(owned.as_deref())
}

/// The runner's RunError rendered as the legacy probe shape: a missing tool
/// is the 127 the Python `_proc.run` answered, other IO is 126.
fn run_error_result(error: &crate::pr_status::gh_run::RunError) -> RunResult {
    match error {
        crate::pr_status::gh_run::RunError::ToolMissing(name) => RunResult {
            returncode: 127,
            stdout: String::new(),
            stderr: format!("{name} not found on PATH"),
        },
        crate::pr_status::gh_run::RunError::Io(message) => RunResult {
            returncode: 126,
            stdout: String::new(),
            stderr: message.clone(),
        },
    }
}

/// The plain REST GET through the conditional door, judged in process (the
/// `_internal_ronthree` door call now rides the local op). `None` to run
/// the read here.
fn door_get(argv: &[String], cwd: &Path) -> Option<RunResult> {
    if argv.len() != 3 || argv[1] != "api" {
        return None;
    }
    let payload = json!({
        "op": "status-rest-get",
        "cwd": cwd.to_string_lossy(),
        "path": argv[2],
    });
    let answer: Value =
        serde_json::from_str(&crate::pr_status_facts::run_op("status-rest-get", &payload)).ok()?;
    if !answer
        .get("eligible")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    Some(RunResult {
        returncode: if answer.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            0
        } else {
            1
        },
        stdout: answer
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        stderr: answer
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// The runner every adapter read goes through: route `gh`-headed argv onto
/// the real gh with the proxy-scrubbed env, judge the fleet budget once, and
/// try the conditional door first for plain REST GETs. Also a `GhProbe`, so
/// the shared refusal classifier reads it like any other probe.
struct AdapterRunner {
    real_gh: String,
}

impl AdapterRunner {
    fn run(&self, cmd: &[String], cwd: Option<&Path>, timeout: Duration) -> RunResult {
        let argv: Vec<String> = if cmd.first().map(String::as_str) == Some("gh") {
            let mut argv = vec![self.real_gh.clone()];
            argv.extend(cmd[1..].iter().cloned());
            argv
        } else {
            cmd.to_vec()
        };
        if argv.first() == Some(&self.real_gh) {
            if let Some(served) = door_get(&argv, &cwd_path(cwd)) {
                return served;
            }
            let env = match quota::delegate_environment() {
                Ok(env) => env,
                Err(exc) => {
                    return RunResult {
                        returncode: 2,
                        stdout: String::new(),
                        stderr: quota::proxy_identity_refusal(&exc),
                    }
                }
            };
            if let Some(refusal) = crate::gh_budget::admit_spawn_for_origin(&argv[1..]) {
                return RunResult {
                    returncode: quota::REFUSED,
                    stdout: String::new(),
                    stderr: refusal,
                };
            }
            return match run(&argv, cwd, Some(env), timeout) {
                Ok(out) => out,
                Err(error) => run_error_result(&error),
            };
        }
        match run(&argv, cwd, None, timeout) {
            Ok(out) => out,
            Err(error) => run_error_result(&error),
        }
    }
}

impl crate::pr_status_facts::GhProbe for AdapterRunner {
    fn run_gh(&self, cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        let out = self.run(args, Some(cwd), Duration::from_secs(30));
        Ok((out.ok(), out.stdout, out.stderr))
    }
}

/// The numeric selector, else the current branch's PR over the REST
/// pulls-list read. (The `_internal_gh._pr_number` port.)
fn pr_number(args: &[String], cwd: Option<&Path>) -> Result<u64, String> {
    let value_options = ["--repo", "--json", "--jq", "-q", "--template"];
    let mut selector: Option<u64> = None;
    let mut index = 2;
    while index < args.len() {
        let arg = &args[index];
        if value_options.contains(&arg.as_str()) {
            index += 2;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        if arg.is_empty() || !arg.chars().all(|c| c.is_ascii_digit()) || selector.is_some() {
            return Err(format!("unsupported or ambiguous PR selector: {arg}"));
        }
        selector = Some(arg.parse().unwrap_or(0));
        index += 1;
    }
    match selector {
        Some(selector) => Ok(selector),
        None => lists::resolve_current_pr_number(cwd, option(args, "--repo").as_deref()),
    }
}

/// The REST info read behind every metadata leg, via the op the command rides
/// (the same receipt shape, whichever transport answered it).
fn fetch_info(number: u64, cwd: Option<&Path>, repo: Option<&str>) -> Result<Value, String> {
    let mut payload = json!({
        "pr": number,
        "cwd": cwd_path(cwd).to_string_lossy(),
    });
    if let Some(repo) = repo {
        payload["repo"] = json!(repo);
    }
    let out = crate::pr_info::run_op(&payload);
    if let Some(error) = out.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(out)
}

/// `pr view --json <metadata set>` answered from the REST pulls fetch. (The
/// `_internal_gh._metadata` port.)
fn metadata(args: &[String], cwd: Option<&Path>) -> RunResult {
    let number = match pr_number(args, cwd) {
        Ok(number) => number,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let info = match fetch_info(number, cwd, option(args, "--repo").as_deref()) {
        Ok(info) => info,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let payload = json!({
        "state": info.get("state").cloned().unwrap_or(Value::Null),
        "number": info.get("pr").cloned().unwrap_or(Value::Null),
        "headRefName": info.get("head_ref").cloned().unwrap_or(Value::Null),
        "headRefOid": info.get("head_sha").cloned().unwrap_or(Value::Null),
        "mergeable": info.get("mergeable").cloned().unwrap_or(Value::Null),
        "mergeStateStatus": info
            .get("merge_state_status")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "baseRefName": info.get("base_ref").cloned().unwrap_or(Value::Null),
        // The gh CLI shape for `--json author`; an empty login when REST
        // carried none, which the Rust reader treats as unreadable
        // (fail-closed).
        "author": {"login": info.get("author").and_then(Value::as_str).unwrap_or("")},
    });
    RunResult {
        returncode: 0,
        stdout: format!("{payload}\n"),
        stderr: String::new(),
    }
}

/// The bucketed rows are the Rust reader's `status-ci` op; classification
/// lives there. (The `_internal_gh._checks` port.)
fn checks(args: &[String], cwd: Option<&Path>) -> RunResult {
    let number = match pr_number(args, cwd) {
        Ok(number) => number,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let payload = json!({
        "op": "status-ci",
        "cwd": cwd_path(cwd).to_string_lossy(),
        "pr": number,
    });
    let (code, stdout, stderr) = crate::pr_status::status_ci(&payload);
    RunResult {
        returncode: code,
        stdout,
        stderr,
    }
}

/// Paginated REST reads behind the coverage legs (the `_internal_gh.
/// _rest_pages` port). `Err(reason)` names the class so a rate-limit refusal
/// classifies against the LIVE exempt bucket.
fn rest_pages(
    runner: &AdapterRunner,
    endpoint: &str,
    label: &str,
    cwd: Option<&Path>,
) -> Result<Vec<Value>, String> {
    let mut rows: Vec<Value> = Vec::new();
    for page in 1..=100 {
        let result = runner.run(
            &[
                "gh".to_string(),
                "api".to_string(),
                format!("{endpoint}?per_page=100&page={page}"),
            ],
            cwd,
            Duration::from_secs(30),
        );
        if !result.ok() {
            let reason = crate::pr_status::rest_reason(runner, &cwd_path(cwd), &result.stderr);
            return Err(format!("{label} read failed: {}", reason.text));
        }
        let payload: Value = match serde_json::from_str(&result.stdout) {
            Ok(v) => v,
            Err(e) => return Err(format!("{label} returned output that is not JSON: {e}")),
        };
        let Some(rows_page) = payload.as_array() else {
            return Err(format!("{label} returned a value that is not a JSON array"));
        };
        if !rows_page.iter().all(Value::is_object) {
            return Err(format!(
                "{label} returned a value that is not a JSON array of objects"
            ));
        }
        rows.extend(rows_page.iter().cloned());
        if rows_page.len() < 100 {
            return Ok(rows);
        }
    }
    Err(format!("{label} exceeded the 100-page safety limit"))
}

fn login(row: &Value) -> String {
    row.get("user")
        .and_then(|u| u.get("login"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `pr view --json reviews,...` for the coverage purpose, answered from the
/// REST reviews + issue-comments pages. (The `_internal_gh._coverage_reviews`
/// port.)
fn coverage_reviews(args: &[String], cwd: Option<&Path>, real_gh: &str) -> RunResult {
    let number = match pr_number(args, cwd) {
        Ok(number) => number,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let runner = AdapterRunner {
        real_gh: real_gh.to_string(),
    };
    let slug = match lists::slug_or_reason(cwd, option(args, "--repo").as_deref()) {
        Ok(slug) => slug,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let info = match fetch_info(number, cwd, Some(&slug)) {
        Ok(info) => info,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let reviews = match rest_pages(
        &runner,
        &format!("repos/{slug}/pulls/{number}/reviews"),
        "pull reviews",
        cwd,
    ) {
        Ok(reviews) => reviews,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let comments = match rest_pages(
        &runner,
        &format!("repos/{slug}/issues/{number}/comments"),
        "issue comments",
        cwd,
    ) {
        Ok(comments) => comments,
        Err(reason) => {
            return RunResult {
                returncode: 1,
                stdout: String::new(),
                stderr: reason,
            };
        }
    };
    let normalized_reviews: Vec<Value> = reviews
        .iter()
        .map(|row| {
            json!({
                "author": {"login": login(row)},
                "state": row.get("state").and_then(Value::as_str).unwrap_or("").to_string(),
                "submittedAt": row.get("submitted_at").and_then(Value::as_str).unwrap_or("").to_string(),
                "commit": {"oid": row.get("commit_id").and_then(Value::as_str).unwrap_or("").to_string()},
                "body": row.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
            })
        })
        .collect();
    let normalized_comments: Vec<Value> = comments
        .iter()
        .map(|row| {
            json!({
                "author": {"login": login(row)},
                "createdAt": row.get("created_at").and_then(Value::as_str).unwrap_or("").to_string(),
                "body": row.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
            })
        })
        .collect();
    let payload = json!({
        "reviews": normalized_reviews,
        "comments": normalized_comments,
        "headRefOid": info.get("head_sha").cloned().unwrap_or(Value::Null),
        "baseRefName": info.get("base_ref").cloned().unwrap_or(Value::Null),
    });
    RunResult {
        returncode: 0,
        stdout: format!("{payload}\n"),
        stderr: String::new(),
    }
}

/// The adapter's routing policy (`_internal_gh.execute`): metadata fields
/// read REST, the coverage field set reads the REST pages, and everything
/// else routes through the serialized quota broker with the purpose the
/// executable name fixed.
pub(crate) fn execute(
    purpose: &str,
    args: &[String],
    cwd: Option<&Path>,
    real_gh: Option<&str>,
) -> RunResult {
    let resolved = quota::resolve_real_gh().ok().flatten();
    let gh = match real_gh.or(resolved.as_deref()) {
        Some(gh) => gh,
        None => {
            return RunResult {
                returncode: 127,
                stdout: String::new(),
                stderr: "gh not found on PATH".to_string(),
            }
        }
    };
    let cwd_str = cwd.map(|p| p.to_string_lossy().into_owned());
    let broker = |args: &[String]| {
        quota::execute_graphql(
            purpose,
            args,
            Some(&gh),
            None,
            cwd_str.as_deref(),
            Duration::from_secs(120),
        )
    };
    if args.len() >= 2 && args[0] == "pr" && args[1] == "checks" {
        return checks(args, cwd);
    }
    if args.len() >= 2 && args[0] == "pr" && args[1] == "view" {
        let fields: Vec<String> = option(args, "--json")
            .map(|csv| {
                csv.split(',')
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        if !fields.is_empty() && fields.iter().all(|f| METADATA_FIELDS.contains(&f.as_str())) {
            return metadata(args, cwd);
        }
        if purpose == "coverage"
            && fields.iter().any(|f| f == "reviews")
            && fields.iter().any(|f| f == "comments")
            && fields.iter().all(|f| COVERAGE_FIELDS.contains(&f.as_str()))
        {
            return coverage_reviews(args, cwd, &gh);
        }
        return broker(args);
    }
    if args.len() >= 2 && args[0] == "api" && args[1] == "graphql" {
        return broker(args);
    }
    if args.len() >= 2 && args[0] == "pr" && (args[1] == "list" || args[1] == "status") {
        return broker(args);
    }
    // Anything else is a plain gh command: the runner's door + budget +
    // scrubbed-env transport, never the broker.
    let runner = AdapterRunner {
        real_gh: gh.to_string(),
    };
    let mut argv: Vec<String> = vec!["gh".to_string()];
    argv.extend(args.iter().cloned());
    runner.run(&argv, cwd, Duration::from_secs(120))
}

/// The two binaries' shared main: fixed purpose from the executable name,
/// gh-shaped streams out, the adapter's exit code out. (The
/// `_internal_gh._main` port; stderr keeps the trailing-newline guarantee.)
pub fn run_main(purpose: &str, args: Vec<String>) -> i32 {
    let result = execute(purpose, &args, None, None);
    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
        if !result.stderr.ends_with('\n') {
            eprintln!();
        }
    }
    result.returncode
}

/// The client's gh transport arms: `gh <args>` is the proxy the gh shim
/// execs, `gh-adapter <purpose> <args>` the fixed-purpose read. None for
/// any other argv.
pub fn gh_transport_door(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str)? {
        "gh" => Some(crate::pr_status::proxy::run_proxy_main(args[1..].to_vec())),
        "gh-adapter" => Some(match args.get(1).map(String::as_str) {
            Some(purpose @ ("coverage" | "discretionary")) => run_main(purpose, args[2..].to_vec()),
            _ => {
                eprintln!("fno-agents gh-adapter: the purpose must be coverage or discretionary");
                2
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_reads_both_spellings() {
        let args: Vec<String> = ["pr", "view", "7", "--json", "reviews", "--repo=x/y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(option(&args, "--json").as_deref(), Some("reviews"));
        assert_eq!(option(&args, "--repo").as_deref(), Some("x/y"));
        assert_eq!(option(&args, "--jq"), None);
    }

    #[test]
    fn pr_number_rejects_ambiguous_selectors() {
        let args: Vec<String> = ["pr", "view", "7"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pr_number(&args, None).unwrap_or(0), 7);
        let ambiguous: Vec<String> = ["pr", "view", "7", "8"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(pr_number(&ambiguous, None).is_err());
        let flagged: Vec<String> = ["pr", "view", "--json", "reviews", "7"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pr_number(&flagged, None).unwrap_or(0), 7);
    }

    #[test]
    fn execute_refuses_without_a_real_gh() {
        // With FNO_REAL_GH unset and no gh on PATH (the test PATH has none),
        // execute answers the 127 the Python leg answered.
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", "/nonexistent-gh-path");
        std::env::remove_var("FNO_REAL_GH");
        let out = execute(
            "discretionary",
            &["api".to_string(), "graphql".to_string()],
            None,
            None,
        );
        if saved.is_some() {
            std::env::set_var("PATH", saved.unwrap());
        }
        assert_eq!(out.returncode, 127);
        assert!(out.stderr.contains("gh not found"));
    }
}

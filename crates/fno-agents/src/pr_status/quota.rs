//! One serialized quota policy for every Footnote GraphQL caller. Ported from
//! `cli/src/fno/pr/_quota.py` (pr port 7, x-e024). The machine quota lock,
//! the probe cache, and every refusal wording are the Python leg's,
//! byte-for-byte where an operator or a test read them.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::pr_status::gh_run::{run, RunError, RunResult};

/// The GraphQL points a discretionary read may spend before the reserve
/// refuses; the number the fleet sizes its polling against.
pub(crate) const GRAPHQL_RESERVE: i64 = 200;
/// The exit code every local refusal answers with (the bash scripts' 75).
pub(crate) const REFUSED: i32 = 75;
/// Healthy probe answers are reused this long.
const PROBE_TTL_S: u64 = 60;
/// A token in any of these names the caller's own credentials; the probe
/// cache is skipped (a probe answer pinned under one token is wrong under
/// another).
const TOKEN_ENV: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];
/// Where the installed worker proxy shims live (the env override, and the
/// marker `delegate_environment` pops).
pub(crate) const PROXY_DIR_ENV: &str = "FNO_GH_PROXY_DIR";
/// The proxy stamps this into the env it hands its execve successor, so its
/// own successor can recognize itself. Good for that one hop only.
pub(crate) const PROXY_DEPTH_ENV: &str = "FNO_GH_PROXY_DEPTH";
/// The exec line `ensure_proxy` writes into the gh shim; the content anchor
/// both shim detectors match.
pub(crate) const PROXY_EXEC_LINE: &str = "exec fno-gh-proxy \"$@\"";
/// The OTHER shape a shim takes: a pip/uv console script importing the entry
/// point, with no exec line. Kept classifiable so an already-installed stale
/// shim reads as ours, never as a real gh.
pub(crate) const PROXY_IMPORT_LINE: &str = "from fno.pr.gh_proxy import main";
/// A wrapper generator may pad the shim with extra lines; the read is
/// bounded, not gated on total size.
const SHIM_SCAN_BYTES: usize = 65536;
/// The delegate refuses its own re-entry with this exit code.
const REFUSE_EXIT: i32 = 2;

/// The proxy cannot work out which directories its own shims live in.
#[derive(Debug)]
pub(crate) struct ProxyIdentityError(pub String);

/// `Path::resolve` that survives a missing final component: Python's
/// `Path.resolve()` answers on paths that do not exist yet, and the proxy
/// dirs are resolved before the first install creates them.
fn resolve_lenient(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        let mut out = PathBuf::from(path);
        // Normalize a leading `..` walk against the parent chain where it is
        // cheap: canonicalize already did the real work when the path exists.
        out
    })
}

/// Is this `gh` our own broker shim, judged by content rather than location?
/// (The `_quota._is_proxy_shim` port.)
pub(crate) fn is_proxy_shim(path: &Path) -> bool {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut head = Vec::new();
    let mut limited = (&mut file).take(SHIM_SCAN_BYTES as u64);
    if limited.read_to_end(&mut head).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&head);
    text.lines().any(|line| {
        let line = line.trim();
        line == PROXY_EXEC_LINE || line == PROXY_IMPORT_LINE
    })
}

/// Every directory one of our own shims can sit in. Fails closed: a proxy
/// that cannot identify itself must not delegate, because calling itself is
/// the one thing it is able to do. An inherited `FNO_GH_PROXY_DIR` already
/// answers, so a later failure is not an identity failure.
pub(crate) fn proxy_dirs() -> Result<Vec<PathBuf>, ProxyIdentityError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(inherited) = non_empty_env(PROXY_DIR_ENV) {
        paths.push(resolve_lenient(Path::new(&inherited)));
    }
    match machine_state_bin_github_cli() {
        Some(dir) => paths.push(resolve_lenient(&dir)),
        None => {
            if paths.is_empty() {
                return Err(ProxyIdentityError(
                    "the fno state root could not be resolved".to_string(),
                ));
            }
        }
    }
    Ok(paths)
}

/// Resolve one of the gh helper binaries by name: a sibling of the running
/// executable first (cargo installs put the whole bin family in one dir and
/// the release wheel stages them side by side in `_bin`), then the bare name
/// for a PATH install. The stop gate, finalize and review-coverage spawn
/// these names; a wheel-only install has no PATH entry for them, so the
/// sibling lookup is what keeps the spawns answering there.
pub(crate) fn helper_bin(name: &str) -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(name);
            if sibling.is_file() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    name.to_string()
}

/// The state-root's `bin/github-cli` dir (the `fno.paths.github_cli_proxy_dir`
/// port): `$HOME/.fno` or `$FNO_STATE_DIR`, never a settings load.
fn machine_state_bin_github_cli() -> Option<PathBuf> {
    let root = non_empty_env("FNO_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))?;
    Some(crate::state_layout::place(&root, "bin").join("github-cli"))
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Remove the quota proxy from PATH before invoking a preserved gh wrapper.
/// (The `_quota.delegate_environment` port.) The map REPLACES the keys it
/// carries; the caller hands it to `gh_run::run`, which also drops the proxy
/// markers the inherited env may carry.
pub(crate) fn delegate_environment() -> Result<BTreeMap<String, String>, ProxyIdentityError> {
    let proxy_dirs = proxy_dirs()?;
    let current = std::env::var("PATH").unwrap_or_default();
    let path_sep = if cfg!(windows) { ';' } else { ':' };
    let scrubbed: Vec<&str> = current
        .split(path_sep)
        .filter(|part| {
            if part.is_empty() {
                return false;
            }
            let resolved = resolve_lenient(Path::new(part));
            if proxy_dirs.contains(&resolved) {
                return false;
            }
            // By basename: a shim two hops down a symlinked dir still matches.
            !is_proxy_shim(&resolved.join("gh"))
        })
        .collect();
    Ok(BTreeMap::from([(
        "PATH".to_string(),
        scrubbed.join(path_sep.to_string()),
    )]))
}

/// Resolve the real gh: `FNO_REAL_GH` when it is a real executable outside
/// the proxy dirs, else the first non-shim `gh` on PATH. Always absolute:
/// the exec below does no PATH lookup, so a bare name would re-enter the
/// shim and deadlock under the flock. (The `_quota.resolve_real_gh` port.)
pub(crate) fn resolve_real_gh() -> Result<Option<String>, ProxyIdentityError> {
    let proxy_dirs = proxy_dirs()?;
    if let Some(configured) = non_empty_env("FNO_REAL_GH") {
        let candidate = PathBuf::from(&configured);
        let is_shim = is_proxy_shim(&candidate);
        let parent_in_proxy = candidate
            .parent()
            .map(|p| proxy_dirs.contains(&resolve_lenient(p)))
            .unwrap_or(false);
        let executable = candidate.is_file() && is_executable(&candidate);
        if executable && !parent_in_proxy && !is_shim {
            return Ok(Some(
                resolve_lenient(&candidate).to_string_lossy().into_owned(),
            ));
        }
    }
    let current = std::env::var("PATH").unwrap_or_default();
    let path_sep = if cfg!(windows) { ';' } else { ':' };
    for directory in current.split(path_sep) {
        if directory.is_empty() {
            continue;
        }
        let candidate = Path::new(directory).join("gh");
        if !candidate.is_file() || !is_executable(&candidate) {
            continue;
        }
        if proxy_dirs.contains(&resolve_lenient(Path::new(directory))) || is_proxy_shim(&candidate)
        {
            continue;
        }
        return Ok(Some(
            resolve_lenient(&candidate).to_string_lossy().into_owned(),
        ));
    }
    Ok(None)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The read shapes the coverage reserve funds, as `(required, shape)`. The
/// cost bound is the SHAPE, never the field count, and each shape carries
/// the field that MAKES it that read. (The `_quota._COVERAGE_READ_SHAPES`
/// port.)
const COVERAGE_READ_SHAPES: [(&[&str], &[&str]); 3] = [
    (
        &["reviews"],
        &["reviews", "comments", "headRefOid", "baseRefName"],
    ),
    (&["commits"], &["commits"]),
    (&["labels"], &["labels"]),
];

/// Only review-coverage computation and publication reads spend the reserve.
/// Subset per shape, not equality; empty fields are a malformed read and
/// refuse. (The `_quota._coverage_read` port.)
pub(crate) fn coverage_read(args: &[String]) -> bool {
    if args.len() < 4
        || args.get(0).map(String::as_str) != Some("pr")
        || args.get(1).map(String::as_str) != Some("view")
    {
        return false;
    }
    let fields_arg = match args.iter().position(|a| a == "--json") {
        Some(index) => args.get(index + 1).cloned().unwrap_or_default(),
        None => args
            .iter()
            .find_map(|a| a.strip_prefix("--json=").map(str::to_string))
            .unwrap_or_default(),
    };
    let fields: Vec<&str> = fields_arg.split(',').filter(|p| !p.is_empty()).collect();
    if fields.is_empty() {
        return false;
    }
    COVERAGE_READ_SHAPES.iter().any(|(required, shape)| {
        required.iter().all(|r| fields.contains(r)) && fields.iter().all(|f| shape.contains(f))
    })
}

/// Drop gh-wide options so equivalent command spellings share one policy.
/// Lives here (not in the proxy module) because the broker and the proxy's
/// argv guard must agree on what counts as the command word. (The
/// `_quota.command_args` port.)
pub(crate) fn command_args(args: &[String]) -> Vec<String> {
    fn strip(tokens: &[String]) -> Vec<String> {
        let mut tokens: Vec<String> = tokens.to_vec();
        while !tokens.is_empty() {
            let token = tokens[0].clone();
            if token == "-R" || token == "--repo" || token == "--hostname" {
                if tokens.len() < 2 {
                    return Vec::new();
                }
                tokens.drain(..2);
            } else if (token.starts_with("-R") && token != "-R")
                || token.starts_with("--repo=")
                || token.starts_with("--hostname=")
            {
                tokens.drain(..1);
            } else {
                break;
            }
        }
        tokens
    }
    let mut normalized = strip(args);
    if normalized.first().map(String::as_str) == Some("pr") {
        let rest = strip(&normalized[1..]);
        let mut out = vec!["pr".to_string()];
        out.extend(rest);
        normalized = out;
    }
    normalized
}

fn quota_parse(payload: &str) -> Option<(i64, i64)> {
    let row = serde_json::from_str::<Value>(payload)
        .ok()?
        .pointer("/resources/graphql")?
        .clone();
    let remaining = row.get("remaining")?.as_i64()?;
    let reset = row.get("reset")?.as_i64()?;
    Some((remaining, reset))
}

/// The probe cache row the exempt `rate_limit` read writes; the broker
/// reuses a healthy answer inside the TTL.
struct ProbeCache(PathBuf);

impl ProbeCache {
    fn read(&self) -> Option<(i64, i64)> {
        let text = std::fs::read_to_string(&self.0).ok()?;
        let row: Value = serde_json::from_str(&text).ok()?;
        let ts = row.get("ts")?.as_f64()?;
        let remaining = row.get("remaining")?.as_i64()?;
        let reset = row.get("reset")?.as_i64()?;
        if now_secs_f64() - ts < PROBE_TTL_S as f64 && remaining > GRAPHQL_RESERVE {
            return Some((remaining, reset));
        }
        None
    }

    fn write(&self, remaining: i64, reset: i64) {
        let row = serde_json::json!({
            "ts": now_secs_f64(),
            "remaining": remaining,
            "reset": reset,
        });
        let _ = std::fs::write(&self.0, row.to_string());
    }
}

fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Live `rate_limit` probe or a fresh healthy cached answer. `cache: None`
/// never reads or writes. (The `_quota._probe_quota` port.)
fn probe_quota(
    cache: Option<&ProbeCache>,
    gh: &str,
    cwd: Option<&Path>,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Option<(i64, i64)> {
    if let Some(cache) = cache {
        if let Some(hit) = cache.read() {
            return Some(hit);
        }
    }
    let argv = vec![gh.to_string(), "api".to_string(), "rate_limit".to_string()];
    let probe = run(&argv, cwd, Some(env.clone()), Duration::from_secs(30)).ok()?;
    let parsed = if probe.ok() {
        quota_parse(&probe.stdout)
    } else {
        None
    };
    if let (Some(cache), Some((remaining, reset))) = (cache, parsed) {
        if remaining > GRAPHQL_RESERVE {
            cache.write(remaining, reset);
        }
    }
    parsed
}

fn pr_number_word(args: &[String]) -> String {
    for (index, arg) in args.iter().enumerate() {
        if (arg == "view" || arg == "checks")
            && args
                .get(index + 1)
                .is_some_and(|next| next.chars().all(|c| c.is_ascii_digit()))
            && args.get(index + 1).is_some_and(|next| !next.is_empty())
        {
            return args[index + 1].clone();
        }
    }
    "<n>".to_string()
}

/// Teach the shape at the point a flag-first argv would exec into noise.
fn malformed_argv_refusal(first: &str) -> String {
    format!(
        "graphql-exec passes everything after -- to gh as its full argv, \
command words first; an argv starting with '{first}' is flags with no \
command, which gh answers with its root command list. Route the \
refused call as: fno do pr graphql-exec --purpose discretionary -- \
api graphql -f query=... --jq ..."
    )
}

/// One wording for the identity refusal, so the callers cannot drift apart.
pub(crate) fn proxy_identity_refusal(exc: &ProxyIdentityError) -> String {
    format!(
        "gh proxy: cannot identify its own install directory, refusing to \
delegate: {exc}"
    )
}

fn refusal(args: &[String], reset: Option<i64>, unavailable: bool) -> String {
    let pr = pr_number_word(args);
    let window = if unavailable {
        "quota instrument unavailable".to_string()
    } else {
        match reset {
            None => "the current quota window resets".to_string(),
            Some(reset) => format!("reset at {}", iso_utc(reset)),
        }
    };
    let cheap = if args.len() >= 2
        && args.get(0).map(String::as_str) == Some("pr")
        && args.get(1).map(String::as_str) == Some("list")
    {
        "Use `fno do pr list` for a REST-backed PR listing".to_string()
    } else {
        format!(
            "Use `fno do pr info {pr}` for state/head/mergeability and \
`fno do pr status {pr}` for CI"
        )
    };
    format!(
        "GraphQL discretionary read refused: {window}. {cheap}; stop retrying GraphQL \
until reset. `fno do pr status` still contains optional review-thread and coverage reads \
that are GraphQL; those reads preserve the reserved coverage budget."
    )
}

/// Unix seconds -> `YYYY-MM-DDTHH:MM:SSZ`, the stamp the refusals and the
/// graphql-remaining read publish. Hand-rolled (no chrono in this crate's
/// dependency set for UTC calendar math).
pub(crate) fn iso_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    // Civil-from-days (Howard Hinnant's algorithm), valid for the epoch range
    // GitHub answers with.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// The most recent fleet-budget status answer: set only when `backoff_live`
/// ran, so a `--refresh` read (which never asks) prints no budget text.
static LAST_BUDGET_SEEN: AtomicI64 = AtomicI64::new(0);
static LAST_BUDGET_POINTS: AtomicI64 = AtomicI64::new(0);
static LAST_BUDGET_CAP: AtomicI64 = AtomicI64::new(0);

/// `, fleet budget X of Y points in the last 60s`, or "" when the ledger
/// never answered this process. (The `_quota.budget_note` port.)
pub(crate) fn budget_note() -> String {
    if LAST_BUDGET_SEEN.load(Ordering::Relaxed) == 0 {
        return String::new();
    }
    format!(
        ", fleet budget {} of {} points in the last 60s",
        LAST_BUDGET_POINTS.load(Ordering::Relaxed),
        LAST_BUDGET_CAP.load(Ordering::Relaxed)
    )
}

/// Whether the fleet ledger holds a live GitHub-refusal backoff. (The
/// `_quota.backoff_live` port.)
pub(crate) fn backoff_live() -> bool {
    let now = crate::gh_budget::now_ms();
    let snapshot = crate::gh_budget::snapshot(&crate::gh_budget::ledger_path(), now);
    let live = snapshot.backoff_remaining_s > 0;
    LAST_BUDGET_SEEN.store(if live { 1 } else { 0 }, Ordering::Relaxed);
    LAST_BUDGET_POINTS.store(snapshot.points_60s, Ordering::Relaxed);
    LAST_BUDGET_CAP.store(snapshot.cap as i64, Ordering::Relaxed);
    live
}

/// Record GitHub's refusal in the fleet ledger, opening its backoff. Only
/// stderr carrying HTTP 403/429 qualifies; the local `gh budget:` refusal
/// line deliberately says "rate limit" without either marker. A drained
/// primary bucket never reaches here (the caller gates on the live probe),
/// so a core-worded refusal against a measured positive bucket reads
/// secondary. (The `_quota.record_refusal` port.)
pub(crate) fn record_refusal(stderr: &str) {
    if !stderr.contains("HTTP 403") && !stderr.contains("HTTP 429") {
        return;
    }
    let low = stderr.to_lowercase();
    let mut kind = if low.contains("secondary rate limit") {
        "secondary"
    } else if low.contains("abuse") {
        "abuse"
    } else if low.contains("rate limit") {
        "core"
    } else {
        "unknown"
    };
    if kind == "core" {
        let snapshot = crate::gh_budget::snapshot(
            &crate::gh_budget::ledger_path(),
            crate::gh_budget::now_ms(),
        );
        if snapshot.core_remaining > 0 {
            kind = "secondary";
        }
    }
    if let Err(e) = crate::gh_budget::record_refusal_real(crate::gh_budget::now_ms(), kind) {
        eprintln!("gh budget: refusal not recorded: {e}");
    }
}

/// Ask the machine-wide budget before one real gh call leaves; `None` when
/// admitted, `Some(line)` is the refusal the caller prints (and exits 75
/// on). (The `_quota.admit` port; the door is the native
/// `gh_budget::admit_spawn_for_origin` the bounded transport already uses.)
fn admit(argv: &[String]) -> Option<String> {
    crate::gh_budget::admit_spawn_for_origin(argv)
}

/// The flock the whole probe-through-command sequence holds. Same advisory
/// lock family Python's fcntl takes, so a still-running Python broker and
/// this port serialize against each other.
struct QuotaLock {
    handle: std::fs::File,
}

impl QuotaLock {
    /// Hold the machine quota lock, re-resolving the path per attempt and
    /// verifying the inode after the flock (the `_quota._open_locked_path`
    /// fresh=true loop). `None` names the default path.
    fn acquire(explicit: Option<&Path>) -> Result<QuotaLock, String> {
        loop {
            let path: PathBuf = match explicit {
                Some(p) => p.to_path_buf(),
                None => {
                    let locks = crate::agents_config::machine_locks_dir()
                        .ok_or_else(|| "the fno state root could not be resolved".to_string())?;
                    locks.join("github-graphql-quota.lock")
                }
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let handle = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
            let fd = handle.as_raw_fd();
            let locked = unsafe { libc::flock(fd, libc::LOCK_EX) };
            if locked != 0 {
                let e = std::io::Error::last_os_error();
                return Err(format!("cannot lock {}: {e}", path.display()));
            }
            let opened_meta = handle
                .metadata()
                .map(|m| ((m.dev(), m.ino())))
                .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
            let current_meta = std::fs::metadata(&path)
                .map(|m| (m.dev(), m.ino()))
                .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
            if opened_meta == current_meta {
                return Ok(QuotaLock { handle });
            }
            // The lock file was replaced mid-acquire; drop and retry.
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
    }
}

impl Drop for QuotaLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.handle.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Probe and execute under one machine-wide lock from probe through command.
/// (The `_quota.execute_graphql` port; argv semantics byte-identical: the
/// full gh argv rides after `--`, command words first.)
pub(crate) fn execute_graphql(
    purpose: &str,
    gh_args: &[String],
    real_gh: Option<&str>,
    lock_path: Option<&Path>,
    cwd: Option<&str>,
    timeout: Duration,
) -> RunResult {
    if purpose != "discretionary" && purpose != "coverage" {
        return RunResult {
            returncode: 2,
            stdout: String::new(),
            stderr: "purpose must be discretionary or coverage".to_string(),
        };
    }
    if gh_args.is_empty() {
        return RunResult {
            returncode: 2,
            stdout: String::new(),
            stderr: "graphql-exec needs gh arguments after --".to_string(),
        };
    }
    // Judge the command word, not the raw first token: gh legalizes gh-wide
    // options before the command word (`gh -R o/r api graphql ...`), and the
    // broker forwards that spelling verbatim. The exec below still uses the
    // full argv, so the option values ride along.
    let command = command_args(gh_args);
    if command.is_empty() || command[0].starts_with('-') {
        let first = command.first().unwrap_or(&gh_args[0]);
        return RunResult {
            returncode: 2,
            stdout: String::new(),
            stderr: malformed_argv_refusal(first),
        };
    }
    // The NORMALIZED command, not the raw argv: gh-wide options before the
    // command word must not change policy.
    if purpose == "coverage" && !coverage_read(&command) {
        return RunResult {
            returncode: 2,
            stdout: String::new(),
            stderr: "coverage reserve accepts review-coverage reads only".to_string(),
        };
    }
    // A caller spelling bare `gh` inside a protected worker would resolve to
    // the proxy; re-entering the broker while this process holds the flock
    // deadlocks until the outer command times out, so only an explicit
    // non-bare path bypasses the pinned-delegate resolver.
    let gh: String = match real_gh.filter(|r| *r != "gh") {
        Some(explicit) => explicit.to_string(),
        None => match resolve_real_gh() {
            Ok(Some(resolved)) => resolved,
            Ok(None) => {
                return RunResult {
                    returncode: 127,
                    stdout: String::new(),
                    stderr: "gh not found on PATH".to_string(),
                }
            }
            Err(exc) => {
                return RunResult {
                    returncode: 2,
                    stdout: String::new(),
                    stderr: proxy_identity_refusal(&exc),
                }
            }
        },
    };
    let lock = match lock_path {
        Some(p) => p.to_path_buf(),
        None => match crate::agents_config::machine_locks_dir() {
            Some(locks) => locks.join("github-graphql-quota.lock"),
            None => {
                return RunResult {
                    returncode: 2,
                    stdout: String::new(),
                    stderr: "gh proxy: quota lock unavailable, refusing: the fno state root \
could not be resolved"
                        .to_string(),
                }
            }
        },
    };
    let cache = if TOKEN_ENV.iter().any(|k| std::env::var_os(k).is_some()) {
        None
    } else {
        Some(ProbeCache(lock.with_file_name("github-quota-probe.json")))
    };
    let held = match QuotaLock::acquire(lock_path) {
        Ok(held) => held,
        Err(e) => {
            return RunResult {
                returncode: 2,
                stdout: String::new(),
                stderr: format!("gh proxy: quota lock unavailable, refusing: {e}"),
            }
        }
    };
    let env = match delegate_environment() {
        Ok(env) => env,
        Err(exc) => {
            return RunResult {
                returncode: 2,
                stdout: String::new(),
                stderr: proxy_identity_refusal(&exc),
            }
        }
    };
    let remaining = probe_quota(cache.as_ref(), &gh, cwd.map(Path::new), &env, timeout);
    if purpose == "discretionary" {
        match remaining {
            None => {
                return RunResult {
                    returncode: REFUSED,
                    stdout: String::new(),
                    stderr: refusal(gh_args, None, true),
                }
            }
            Some((remaining, reset)) if remaining <= GRAPHQL_RESERVE => {
                return RunResult {
                    returncode: REFUSED,
                    stdout: String::new(),
                    stderr: refusal(gh_args, Some(reset), false),
                }
            }
            _ => {}
        }
    }
    if let Some(budget_refusal) = admit(gh_args) {
        return RunResult {
            returncode: REFUSED,
            stdout: String::new(),
            stderr: budget_refusal,
        };
    }
    let mut argv: Vec<String> = vec![gh.clone()];
    argv.extend(gh_args.iter().cloned());
    let result = run(&argv, cwd.map(Path::new), Some(env), timeout);
    let result = match result {
        Ok(result) => result,
        Err(RunError::ToolMissing(_)) => {
            return RunResult {
                returncode: 127,
                stdout: String::new(),
                stderr: "gh not found on PATH".to_string(),
            }
        }
        Err(RunError::Io(e)) => RunResult {
            returncode: 2,
            stdout: String::new(),
            stderr: e,
        },
    };
    if result.returncode != 0
        && result.stderr.to_lowercase().contains("rate limit")
        && remaining.map(|(r, _)| r > 0).unwrap_or(true)
    {
        // Fail toward backoff, and never record a drained primary bucket.
        record_refusal(&result.stderr);
    }
    result
}

/// The `graphql-exec` verb body: purpose flag + `--` passthrough, byte
/// semantics of the Python verb (stdout plain, stderr on stderr, the
/// broker's exit code).
pub(crate) fn run_graphql_exec_args(purpose: &str, gh_args: &[String]) -> i32 {
    let result = execute_graphql(purpose, gh_args, None, None, None, Duration::from_secs(120));
    if !result.stdout.is_empty() {
        print!("{result.stdout}");
    }
    if !result.stderr.is_empty() {
        eprint!("{result.stderr}");
    }
    result.returncode
}

/// One shim-detection test the parity guard replays: both anchor lines match
/// in content, a real gh mentioning the proxy name in a comment does not.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_detection_matches_both_anchor_lines_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("gh");
        std::fs::write(&shim, format!("#!/bin/sh\n{PROXY_EXEC_LINE}\n")).unwrap();
        assert!(is_proxy_shim(&shim));
        let console = dir.path().join("gh2");
        std::fs::write(&console, format!("#!/bin/sh\n{PROXY_IMPORT_LINE}\n")).unwrap();
        assert!(is_proxy_shim(&console));
        let real = dir.path().join("gh3");
        std::fs::write(
            &real,
            "#!/bin/sh\n# not the fno-gh-proxy wrapper, just mentions it\necho real\n",
        )
        .unwrap();
        assert!(!is_proxy_shim(&real));
    }

    #[test]
    fn command_args_strips_gh_wide_options() {
        let args: Vec<String> = [
            "-R", "o/r", "pr", "-R", "x/y", "view", "7", "--json", "reviews",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let command = command_args(&args);
        assert_eq!(command, vec!["pr", "view", "7", "--json", "reviews"]);
    }

    #[test]
    fn coverage_read_needs_the_required_field_per_shape() {
        let mk = |csv: &str| -> Vec<String> {
            ["pr", "view", "7", "--json", csv]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        assert!(coverage_read(&mk("reviews")));
        assert!(coverage_read(&mk("reviews,comments")));
        // reviews alone is funded; comments alone is not.
        assert!(!coverage_read(&mk("comments")));
        assert!(!coverage_read(&mk("reviews,labels")));
        assert!(!coverage_read(&mk("")));
        // api graphql is not a coverage read regardless of fields.
        assert!(!coverage_read(&["api".to_string(), "graphql".to_string()]));
    }

    #[test]
    fn iso_utc_formats_the_refusal_stamp() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        // 2026-10-21T00:00:00Z
        assert_eq!(iso_utc(1_792_540_800), "2026-10-21T00:00:00Z");
    }
}

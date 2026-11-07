//! What work is stuck: a command process running far past its own deadline, and
//! a single-flight lock whose holder pid is gone. One module answers "is any
//! work stuck on this machine" for the three readers that page or print it
//! (`fno agents status`, arm_watch, the lead check-in), so a hung `backlog
//! advance` can never again run 3h29m with the only signal a banner nobody
//! sees.
//!
//! The read is a snapshot: ps once, the claims dirs once. Findings are
//! self-describing lines so every reader prints the same text without a
//! second renderer.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The age past which a command with no declared timeout reads hung: 3 x the
/// 10-minute-class default, the wall clock the drain's own lease carries.
// ponytail: fixed 1800 s floor and 300 s holder grace, move to [notify] keys if a lane needs a different clock
const DEFAULT_HUNG_FLOOR_S: u64 = 1800;

/// A dead-pid `flight:` hold must sit this long before the read names it: the
/// gate reclaims a dead hold on the next acquire, so a fresh corpse is not
/// news.
// ponytail: fixed 1800 s floor and 300 s holder grace, move to [notify] keys
// if a lane needs a different clock
pub(crate) const DEAD_HOLDER_GRACE_S: i64 = 300;

/// How much of a finding's argv a line carries. The notice names the argv so
/// a missing exclusion shape is fixable from the page alone.
const ARGV_CAP: usize = 120;

/// The argv tokens that mark a long-lived shape: not hung, by design.
const LONG_LIVED_TOKENS: [&str; 3] = ["daemon", "attach", "mux"];

/// One stuck thing, self-describing. `key` is the dedupe token part the
/// arm_watch tick folds it into; `line` is the only text a reader prints.
#[derive(Debug)]
pub struct Finding {
    pub kind: &'static str,
    pub key: String,
    pub line: String,
    /// The claims root the lockfile sits under, so a heal releases the same
    /// file this read scanned. Set only on a dead holder.
    pub root: Option<PathBuf>,
    pub holder: Option<String>,
    pub claim_key: Option<String>,
}

/// A command is hung when its age passes three times its declared `--timeout`
/// (else 1800 s). The command rule and the excluded shapes are the whole
/// classifier; every shape here is pinned by a test.
pub(crate) fn hung_commands(rows: &[(u32, u64, String)], now_unix: u64) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (pid, age_s, args) in rows {
        let Some(finding) = hung_command(*pid, *age_s, args, now_unix) else {
            continue;
        };
        findings.push(finding);
    }
    findings
}

fn hung_command(pid: u32, age_s: u64, args: &str, now_unix: u64) -> Option<Finding> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let argv0 = tokens.first()?;
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    let is_command = matches!(base, "fno" | "fno-agents");
    if !is_command {
        return None;
    }
    // Long-lived shapes: the daemon and the mux clients run for days by
    // design; a bare `fno` is a shell, not a command. The exclusion tokens
    // count only in the subcommand prefix, before the first flag, so an
    // argument like `--project mux` can never swallow a real command.
    let subcommand: Vec<&str> = tokens
        .iter()
        .skip(1)
        .take_while(|a| !a.starts_with('-'))
        .copied()
        .collect();
    if tokens.len() == 1
        || tokens.contains(&"--server")
        || subcommand.iter().any(|a| LONG_LIVED_TOKENS.contains(a))
        || subcommand.windows(2).any(|w| w == ["loop", "run"])
    {
        return None;
    }
    let timeout = declared_timeout(&tokens);
    let floor = 3 * timeout
        .as_ref()
        .map(|(_, s)| *s)
        .unwrap_or(DEFAULT_HUNG_FLOOR_S);
    if age_s <= floor {
        return None;
    }
    let basis = match &timeout {
        Some((raw, _)) => format!("over 3x --timeout {raw}"),
        None => format!("over {DEFAULT_HUNG_FLOOR_S}s"),
    };
    Some(Finding {
        kind: "hung_command",
        key: format!("hung:{pid}@{}", now_unix.saturating_sub(age_s)),
        line: format!(
            "hung command pid {pid} {} {} ({basis})",
            fmt_age(age_s),
            cut(args, ARGV_CAP)
        ),
        root: None,
        holder: None,
        claim_key: None,
    })
}

/// The raw `--timeout <dur>` token, as written (`30`, `30s`, `30m`, `30h`).
fn declared_timeout(tokens: &[&str]) -> Option<(String, u64)> {
    let pos = tokens
        .iter()
        .position(|a| *a == "--timeout" || a.starts_with("--timeout="))?;
    let raw = match tokens[pos].split_once('=') {
        Some((_, value)) => value,
        None => *tokens.get(pos + 1)?,
    };
    let digits = raw.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let secs = match raw.chars().last() {
        Some('s') | Some('0'..='9') => n,
        Some('m') => n * 60,
        Some('h') => n * 3600,
        _ => return None,
    };
    Some((raw.to_string(), secs))
}

/// One `ps -axo pid=,etime=,args=` pass, parsed to (pid, age_s, args). A
/// non-zero exit or unreadable output is an Err, never an empty list: a
/// blind read must not page as "nothing is stuck".
fn ps_rows() -> Result<Vec<(u32, u64, String)>, String> {
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,etime=,args="])
        .output()
        .map_err(|e| format!("ps failed to run: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ps exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(120)
                .collect::<String>()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((pid, rest)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let (etime, args) = match rest.trim_start().split_once(' ') {
            Some((etime, args)) => (etime, args.trim()),
            None => (rest.trim_start(), ""),
        };
        let Some(age) = crate::gc::parse_etime(etime) else {
            continue;
        };
        rows.push((pid, age, args.to_string()));
    }
    Ok(rows)
}

/// Dead holders among the `flight:` claims: rows `long_holds` already aged
/// past the grace whose pid probe read `absent` on this host. Only `flight:`
/// holds count - they are pid-scoped by design; a session claim outlives its
/// ambient pid on purpose. Each line ends with the release command, so the page
/// alone is enough to clear it.
pub fn dead_holders(dirs: &[PathBuf]) -> Result<Vec<Finding>, String> {
    let now_unix = crate::claims::now_ms() / 1000;
    let mut findings = Vec::new();
    // One dir at a time: a row does not say which dir it came from, and the
    // release must name the root that holds the lockfile.
    for dir in dirs {
        let payload = crate::claims::long_holds(std::slice::from_ref(dir), DEAD_HOLDER_GRACE_S)?;
        let root = claims_root_of(dir);
        let rows: Vec<Value> = payload
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in &rows {
            if row.get("pid_observed").and_then(Value::as_str) != Some("absent") {
                continue;
            }
            let Some(key) = row.get("key").and_then(Value::as_str) else {
                continue;
            };
            let holder = row.get("holder").and_then(Value::as_str).unwrap_or("?");
            let pid = row
                .get("pid")
                .map(|p| p.to_string().trim_matches('"').to_string());
            let held_s = row.get("held_s").and_then(Value::as_i64).unwrap_or(0);
            let anchor = (now_unix - held_s.max(0)).max(0) as u64;
            let repair = match &root {
                Some(r) => format!(
                    "; repair: {} heal=auto",
                    crate::arm_repair::release_command(key, holder, Some(r))
                ),
                None => "; heal=operator".to_string(),
            };
            findings.push(Finding {
                kind: "dead_holder",
                key: format!("holder:{key}@{anchor}"),
                line: format!(
                    "dead holder {key} holder {holder} pid {} absent held {}{repair}",
                    pid.as_deref().unwrap_or("None"),
                    fmt_age(held_s.max(0) as u64)
                ),
                root: root.clone(),
                holder: Some(holder.to_string()),
                claim_key: Some(key.to_string()),
            });
        }
    }
    Ok(findings)
}

/// The claims root behind a `<root>/.fno/claims` dir. The space dir's claims
/// has no such root, so no release command can reach it.
fn claims_root_of(dir: &Path) -> Option<PathBuf> {
    if !dir.ends_with(".fno/claims") {
        return None;
    }
    dir.parent().and_then(Path::parent).map(Path::to_path_buf)
}

/// The claims directories the dead-holder read scans: the global dir, then
/// the project dir (`$FNO_CLAIMS_ROOT/.fno/claims` when set, else the space
/// dir's claims), the order the Python claims io resolves.
pub fn claims_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let root = std::env::var_os("FNO_CLAIMS_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    // The claims crate owns the `<root>/.fno/claims` layout; the space
    // dir's claims is the fallback the Python io takes with no root set.
    if let Some(resolved) = crate::claims::claims_dir_for(root.as_deref()) {
        dirs.push(resolved);
    }
    if root.is_none() {
        let space = crate::paths::space_dir(cwd).join("claims");
        if !dirs.contains(&space) {
            dirs.push(space);
        }
    }
    dirs
}

/// The sink-health file the dead-sink read scans: the attention state dir,
/// resolved the way the attention arm resolves it.
fn sink_health_file() -> Option<PathBuf> {
    crate::attention_arm::attention_dir()
        .ok()
        .map(|dir| crate::attention_http::sink_health_path(&dir))
}

/// One finding per dead sink in `sink-health.json`: a sink whose
/// consecutive-failure streak is 3 or more and whose last success is missing
/// or predates the streak's start. A missing or unreadable file adds
/// nothing: an unconfigured sink list is quiet.
fn dead_sinks_from_health(path: &Path) -> Vec<Finding> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(map) = serde_json::from_str::<crate::attention_http::SinkHealth>(&raw) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for (name, row) in map {
        let Some(first_fail) = row.first_fail_at else {
            continue;
        };
        let dead = row.fail_streak >= crate::attention_http::DEAD_SINK_STREAK_MIN
            && row.last_ok_at.is_none_or(|t| first_fail > t);
        if !dead {
            continue;
        }
        let host = row.url.split("://").nth(1).unwrap_or(row.url.as_str());
        let host = host.split('/').next().unwrap_or(host);
        let last = row.last_error.clone().unwrap_or_else(|| {
            row.last_status
                .map(|s| format!("status {s}"))
                .unwrap_or_else(|| "no status".to_string())
        });
        let last = cut(&last, 120);
        findings.push(Finding {
            kind: "dead_sink",
            key: format!("dead_sink:{name}@{first_fail}"),
            line: format!(
                "attention sink {name} dead since {first_fail}: {} posts failed, last {last} ({host})",
                row.fail_streak
            ),
            root: None,
            holder: None,
            claim_key: None,
        });
    }
    findings
}

/// The full read: hung commands, dead holders, then dead attention sinks. An
/// error from either leg fails the whole read - a partially blind answer
/// reads as a failed read, never as a clean empty. The sink-health read is
/// the one exception: a missing or unreadable `sink-health.json` adds
/// nothing and never fails the read, because an unconfigured sink list is
/// quiet.
pub fn collect(cwd: &Path) -> Result<Vec<Finding>, String> {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut findings = hung_commands(&ps_rows()?, now_unix);
    findings.extend(dead_holders(&claims_dirs(cwd))?);
    if let Some(path) = sink_health_file() {
        findings.extend(dead_sinks_from_health(&path));
    }
    Ok(findings)
}

/// The `fno agents status` JSON shape: the two finding lists, or one error.
pub fn status_value(cwd: &Path) -> Value {
    match collect(cwd) {
        Ok(findings) => {
            let hung: Vec<&str> = findings
                .iter()
                .filter(|f| f.kind == "hung_command")
                .map(|f| f.line.as_str())
                .collect();
            let holders: Vec<&str> = findings
                .iter()
                .filter(|f| f.kind == "dead_holder")
                .map(|f| f.line.as_str())
                .collect();
            let sinks: Vec<&str> = findings
                .iter()
                .filter(|f| f.kind == "dead_sink")
                .map(|f| f.line.as_str())
                .collect();
            json!({"hung_commands": hung, "dead_holders": holders, "dead_sinks": sinks})
        }
        Err(e) => json!({"error": e}),
    }
}

/// The `stuck work:` block the human status prints: nothing when the read is
/// clean, one line per finding, the reason when the read failed.
pub fn render_lines(value: &Value) -> Vec<String> {
    if let Some(err) = value.get("error").and_then(Value::as_str) {
        return vec![format!("stuck work: unreadable ({err})")];
    }
    let hung = value
        .get("hung_commands")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let holders = value
        .get("dead_holders")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let sinks = value
        .get("dead_sinks")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    if hung + holders + sinks == 0 {
        return Vec::new();
    }
    let mut lines = vec!["stuck work:".to_string()];
    for item in value
        .get("hung_commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            value
                .get("dead_holders")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .chain(
            value
                .get("dead_sinks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
    {
        if let Some(text) = item.as_str() {
            lines.push(format!("  {text}"));
        }
    }
    lines
}

/// Compact floored age, `45s` / `12m` / `3h29m`.
fn fmt_age(seconds: u64) -> String {
    let h = seconds / 3600;
    let m = (seconds % 3600) / 60;
    if h > 0 {
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{seconds}s")
    }
}

fn cut(text: &str, cap: usize) -> &str {
    match text.char_indices().nth(cap) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::{encode_key, hostname, machine_id, now_ms, SCHEMA_VERSION};
    use tempfile::TempDir;

    fn dead_pid() -> u32 {
        let mut candidate = 999_999u32;
        while unsafe { libc::kill(candidate as i32, 0) } == 0 {
            candidate += 1;
        }
        candidate
    }

    fn flight_rec(
        key: &str,
        holder: &str,
        pid: Option<i32>,
        acquired_s_ago: i64,
    ) -> crate::claims::ClaimRecord {
        crate::claims::ClaimRecord {
            schema_version: SCHEMA_VERSION,
            key: key.into(),
            holder: holder.into(),
            acquired_at: now_ms() - acquired_s_ago * 1000,
            pid,
            host: hostname(),
            pid_unavailable: false,
            expires_at: None,
            reason: None,
            harness: None,
            session_id: None,
            pid_provenance: None,
            machine_id: Some(machine_id()),
            metadata: Default::default(),
        }
    }

    fn write_rec(dir: &Path, rec: &crate::claims::ClaimRecord) {
        crate::claim_store::seed_at_path(&dir.join(format!("{}.lock", encode_key(&rec.key))), rec);
    }

    // AC2-HP: the measured hung advance is a finding.
    #[test]
    fn hung_advance_is_a_finding() {
        let now = 1_800_000_000u64;
        let rows = vec![(
            66_853u32,
            12_540u64,
            "/x/fno backlog advance --loose --project fno --source ab --json".to_string(),
        )];
        let findings = hung_commands(&rows, now);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.kind, "hung_command");
        assert_eq!(f.key, format!("hung:66853@{}", now - 12_540));
        assert!(
            f.line
                .starts_with("hung command pid 66853 3h29m /x/fno backlog advance"),
            "{}",
            f.line
        );
        assert!(f.line.ends_with("(over 1800s)"), "{}", f.line);
    }

    #[test]
    fn bad_ps_is_an_error_not_an_empty_list() {
        // A ps whose exit is non-zero is Err; this stub never runs, so the
        // error kind is the spawn failure itself. The swap to a nonexistent
        // dir rides the shared PATH guard: a concurrent git-spawning test
        // must never inherit it.
        let _path = crate::path_test_guard();
        let prev = std::env::var_os("PATH");
        std::env::set_var("PATH", "/nonexistent-for-stuck-work");
        let result = ps_rows();
        match prev {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        assert!(result.is_err(), "a failed ps is Err");
    }

    // The line carries the release command, and the release it names clears it.
    #[test]
    fn dead_holder_line_carries_a_release_command_that_releases() {
        let td = TempDir::new().unwrap();
        let claims_dir = td.path().join(".fno/claims");
        write_rec(
            &claims_dir,
            &flight_rec(
                "flight:k",
                "single-flight:9:ab",
                Some(dead_pid() as i32),
                600,
            ),
        );
        let findings = dead_holders(std::slice::from_ref(&claims_dir)).unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        let command = format!(
            "; repair: FNO_CLAIMS_ROOT={} fno agents claim release flight:k --holder single-flight:9:ab heal=auto",
            td.path().display()
        );
        assert!(f.line.ends_with(&command), "{}", f.line);
        assert_eq!(f.root.as_deref(), Some(td.path()));
        assert_eq!(f.claim_key.as_deref(), Some("flight:k"));
        assert_eq!(f.holder.as_deref(), Some("single-flight:9:ab"));
        crate::claims::release("flight:k", "single-flight:9:ab", f.root.as_deref(), None).unwrap();
        assert!(dead_holders(&[claims_dir]).unwrap().is_empty());
    }

    #[test]
    fn dead_sink_finding_from_health_file_and_missing_file_is_silent() {
        use crate::attention_http::{SinkHealth, SinkHealthRow};
        use std::collections::BTreeMap;
        let td = TempDir::new().unwrap();
        let path = crate::attention_http::sink_health_path(td.path());
        assert!(dead_sinks_from_health(&path).is_empty());
        let mut map: SinkHealth = BTreeMap::new();
        map.insert(
            "phone".into(),
            SinkHealthRow {
                url: "https://ntfy.example:8443".into(),
                last_ok_at: None,
                first_fail_at: Some(1793985830),
                last_fail_at: Some(1793985831),
                fail_streak: 5,
                last_status: None,
                last_error: Some("curl exited 6: Could not resolve host".into()),
            },
        );
        std::fs::write(&path, serde_json::to_string(&map).unwrap()).unwrap();
        let findings = dead_sinks_from_health(&path);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.kind, "dead_sink");
        assert_eq!(f.key, "dead_sink:phone@1793985830");
        assert!(
            f.line
                .contains("attention sink phone dead since 1793985830: 5 posts failed"),
            "{}",
            f.line
        );
    }
}

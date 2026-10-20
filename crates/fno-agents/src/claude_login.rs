//! Answers one question before a spawn launches: can a claude login on this
//! config dir actually authenticate? `claude auth status` cannot - on a
//! throwaway dir holding an expired, unrefreshable credential it printed
//! `loggedIn: true` and exited 0 (measured 2026-09-21, claude 2.1.278).
//! Only a real one-token headless call answers it: on a dead login the call
//! fails in ~1.3s at zero cost; a live one answers in ~4.6s.

use std::path::Path;
use std::process::Command;

/// The probe's verdict. `LoggedOut` carries claude's own `result` text (the
/// receipt quotes it); `Unknown` carries why the probe could not decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Login {
    LoggedIn,
    LoggedOut(String),
    Unknown(String),
}

/// Result strings that mean the login itself is dead (not an outage, not a
/// quota): captured verbatim from real dead-login runs on 2026-09-21.
const LOGGED_OUT_MARKERS: [&str; 6] = [
    "Please run /login",
    "Not logged in",
    "OAuth session expired",
    "OAuth token revoked",
    "Login expired",
    "Invalid API key",
];

/// A refresh race is transient: another claude process is refreshing the
/// token, so the login may be perfectly healthy. Never refuse on it.
const TRANSIENT_MARKER: &str = "another Claude Code process";

/// Classify one `claude -p --output-format json` run. `success` is the exit
/// status; `stdout` is the JSON envelope claude printed. A success exit with
/// `is_error: false` is logged in; a failure exit with `is_error: true` and a
/// result carrying a dead-login marker is `LoggedOut`. Everything else -
/// unparseable output, an error with no auth marker (overload, rate limit,
/// credit), a success shape that disagrees with the exit status - is
/// `Unknown`, and the gate refuses on `Unknown` never.
pub fn classify(success: bool, stdout: &str) -> Login {
    let envelope = match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(v) if v.is_object() => v,
        _ => {
            return Login::Unknown(format!(
                "output was not one JSON object: {}",
                truncate(stdout)
            ))
        }
    };
    let is_error = envelope
        .get("is_error")
        .and_then(serde_json::Value::as_bool);
    let result = envelope
        .get("result")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    match (success, is_error) {
        (true, Some(false)) => Login::LoggedIn,
        (false, Some(true)) if result.contains(TRANSIENT_MARKER) => {
            Login::Unknown(format!("transient refresh race: {result}"))
        }
        (false, Some(true)) if LOGGED_OUT_MARKERS.iter().any(|m| result.contains(m)) => {
            Login::LoggedOut(result)
        }
        (false, Some(true)) => Login::Unknown(format!(
            "error with no dead-login marker: {}",
            truncate(&result)
        )),
        _ => Login::Unknown(format!(
            "exit status and is_error disagree (success={success}, is_error={is_error:?}): {}",
            truncate(&result)
        )),
    }
}

/// The probe argv, verified on claude 2.1.278: one turn, no tools, no hooks,
/// no session, haiku, prompt `ok`. Runs with `CLAUDE_CONFIG_DIR` pinned to
/// `config_dir`, every key in `held` removed (the probe must authenticate the
/// way a supervisor-born session does: route and credential carriers are
/// poison), and cwd in the temp dir so no project settings or CLAUDE.md load.
/// `get_envs` shows only the set; the removals are not introspectable.
pub(crate) fn probe_command(config_dir: &Path, held: &[String]) -> Command {
    let mut cmd = Command::new("claude");
    cmd.args([
        "-p",
        "--output-format",
        "json",
        "--max-turns",
        "1",
        "--model",
        "haiku",
        "--system-prompt",
        "Reply with one word.",
        "--tools",
        "",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--settings",
        r#"{"disableAllHooks":true}"#,
        "ok",
    ]);
    cmd.env("CLAUDE_CONFIG_DIR", config_dir);
    for key in held {
        cmd.env_remove(key);
    }
    cmd.current_dir(std::env::temp_dir());
    cmd
}

/// Run one probe under a wall-clock bound. A spawn error is `Unknown` (the
/// binary is gone); a status killed by a signal is the timeout verdict the
/// bounded read produced; anything else goes to [`classify`].
pub(crate) fn probe(cmd: Command, secs: u64) -> Login {
    let out = match crate::bounded_cmd::output_with_timeout_result(cmd, secs) {
        Ok(out) => out,
        Err(e) => return Login::Unknown(format!("claude not runnable: {e}")),
    };
    use std::os::unix::process::ExitStatusExt;
    if out.status.signal().is_some() {
        return Login::Unknown(format!("probe timed out after {secs}s"));
    }
    classify(out.status.success(), &String::from_utf8_lossy(&out.stdout))
}

/// Probe the config dir of one account with the ambient poison keys held
/// back, exactly as a supervisor birth would hold them.
pub fn probe_config_dir(config_dir: &Path) -> Login {
    probe(
        probe_command(config_dir, &crate::claude_supervisor::held_poison_keys()),
        20,
    )
}

/// The remedy the refusal receipt carries - the same words
/// `fno/agents/account_env.py` prints for a missing credential.
pub fn login_command(config_dir: &Path) -> String {
    format!("CLAUDE_CONFIG_DIR={} claude /login", config_dir.display())
}

/// The persisted probe verdicts, keyed by account id. Written by the spawn
/// gate, read by launch-time account picking (`fno config accounts pick`)
/// so the rotation skips an account the probe just refused instead of
/// picking it again on every attempt.
pub fn login_probes_path(home: &crate::paths::AgentsHome) -> std::path::PathBuf {
    home.root().join("login-probes.json")
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Record one probe verdict for `account`. `LoggedIn` and `LoggedOut` both
/// overwrite the entry (a fresh live verdict is what lets picking return to
/// an account after `claude /login`); `Unknown` leaves any existing entry
/// untouched - an inconclusive probe must neither refresh a refusal nor
/// clear one. Best-effort: a write failure is silent, the gate's own
/// refusal still stands on the live probe.
pub fn record_verdict(
    home: &crate::paths::AgentsHome,
    account: &str,
    config_dir: &Path,
    verdict: &Login,
) {
    let (name, detail) = match verdict {
        Login::LoggedIn => ("logged_in", None),
        Login::LoggedOut(detail) => ("logged_out", Some(detail.clone())),
        Login::Unknown(_) => return,
    };
    let path = login_probes_path(home);
    let mut doc: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    let entry = match doc.get_mut("probes").and_then(|p| p.as_object_mut()) {
        Some(probes) => probes,
        None => {
            doc["probes"] = serde_json::json!({});
            doc.get_mut("probes").unwrap().as_object_mut().unwrap()
        }
    };
    let mut row = serde_json::json!({
        "verdict": name,
        "config_dir": config_dir.display().to_string(),
        "at_epoch": now_epoch(),
    });
    if let Some(detail) = detail {
        row["detail"] = serde_json::Value::String(detail);
    }
    entry.insert(account.to_string(), row);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, doc.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The spawn gate's recorder: resolve the ambient shared home and persist
/// the verdict. A process with no declared home (tests) records nothing.
pub fn record_verdict_ambient(account: &str, config_dir: &Path, verdict: &Login) {
    if let Some(home) = crate::paths::AgentsHome::from_env_opt() {
        record_verdict(&home, account, config_dir, verdict);
    }
}

fn truncate(s: &str) -> String {
    const CAP: usize = 200;
    if s.chars().count() <= CAP {
        return s.trim().to_string();
    }
    let cut: String = s.chars().take(CAP).collect();
    format!("{cut}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    // The envelope captured on 2026-09-21, verbatim in `result`.
    const EXPIRED: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"Failed to authenticate: OAuth session expired and could not be refreshed","terminal_reason":"api_error"}"#;

    fn fake_claude(body: &str) -> (tempfile::TempDir, Command) {
        let dir = tempfile::tempdir().unwrap();
        let stub = crate::write_exec_stub(dir.path(), "claude", &format!("#!/bin/bash\n{body}\n"));
        (dir, Command::new(&stub))
    }

    #[test]
    fn expired_envelope_classifies_logged_out() {
        let verdict = classify(false, EXPIRED);
        assert_eq!(
            verdict,
            Login::LoggedOut(
                "Failed to authenticate: OAuth session expired and could not be refreshed"
                    .to_string()
            )
        );
    }

    #[test]
    fn refresh_race_reads_unknown_never_logged_out() {
        let verdict = classify(
            false,
            r#"{"is_error":true,"result":"Could not refresh your login because another Claude Code process is refreshing"}"#,
        );
        assert!(matches!(verdict, Login::Unknown(_)));
    }

    #[test]
    fn probe_command_pins_config_dir_and_argv() {
        let held = vec![
            "ANTHROPIC_BASE_URL".to_string(),
            "FNO_ROUTE_PROVIDER".to_string(),
        ];
        let cmd = probe_command(Path::new("/tmp/x"), &held);
        assert_eq!(cmd.get_program(), "claude");
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "CLAUDE_CONFIG_DIR" && v == Some(std::ffi::OsStr::new("/tmp/x"))));
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv.first().map(String::as_str), Some("-p"));
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--output-format" && w[1] == "json"));
        assert!(argv.contains(&"--no-session-persistence".to_string()));
        assert!(argv.contains(&"--strict-mcp-config".to_string()));
        assert_eq!(cmd.get_current_dir(), Some(std::env::temp_dir().as_path()));
    }

    #[test]
    fn a_sleeper_hits_the_bound_and_reads_unknown() {
        // The loop, not a bare sleep: when the runner is fork-starved, a
        // bare `sleep 30` makes bash exit at once with empty output, and
        // that reads as a parse failure instead of the timeout verdict
        // this test pins. The loop has no exit, so the kill at the bound
        // always lands on a live process group.
        let (_dir, cmd) = fake_claude("while :; do sleep 30; done");
        let started = std::time::Instant::now();
        let verdict = probe(cmd, 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert!(
            matches!(&verdict, Login::Unknown(why) if why.contains("timed out after 1s")),
            "{verdict:?}"
        );
    }

    #[test]
    fn login_command_names_the_login_words() {
        assert_eq!(
            login_command(Path::new("/tmp/acct")),
            "CLAUDE_CONFIG_DIR=/tmp/acct claude /login"
        );
    }

    fn read_probe(home: &crate::paths::AgentsHome, account: &str) -> Option<serde_json::Value> {
        let raw = std::fs::read_to_string(login_probes_path(home)).ok()?;
        let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
        doc.get("probes")?.get(account).cloned()
    }

    #[test]
    fn a_logged_out_verdict_is_persisted_with_detail() {
        let root = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(root.path().to_path_buf());
        let dir = Path::new("/tmp/acct");
        record_verdict(
            &home,
            "makers",
            dir,
            &Login::LoggedOut("OAuth session expired".to_string()),
        );
        let row = read_probe(&home, "makers").expect("refusal recorded");
        assert_eq!(row["verdict"], "logged_out");
        assert_eq!(row["detail"], "OAuth session expired");
        assert_eq!(row["config_dir"], "/tmp/acct");
        assert!(row["at_epoch"].is_i64());

        // A live verdict overwrites the refusal; an inconclusive probe
        // leaves the existing entry alone.
        record_verdict(&home, "makers", dir, &Login::LoggedIn);
        let row = read_probe(&home, "makers").expect("verdict recorded");
        assert_eq!(row["verdict"], "logged_in");
        assert!(row.get("detail").is_none());

        record_verdict(
            &home,
            "makers",
            dir,
            &Login::LoggedOut("Login expired".to_string()),
        );
        record_verdict(
            &home,
            "makers",
            dir,
            &Login::Unknown("probe timed out after 20s".to_string()),
        );
        let row = read_probe(&home, "makers").expect("earlier refusal survives");
        assert_eq!(row["verdict"], "logged_out");
    }
}

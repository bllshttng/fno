//! The machine's active GitHub login, read once per machine and cached on
//! disk. The shared `gh-budget:` hold and the poller lease key by it, so
//! two accounts on one primary never share a refusal hold, a lease or a
//! status row (claim_store, gh_poller). The gh shim never pays for it on a
//! stock install: the admit path only asks with `store.remote_url` set,
//! and the bare keys stay machine-local there.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// A good answer lives an hour: logins change rarely, and `gh auth switch`
/// may lag this long before the keyed rows change over.
const LOGIN_TTL_MS: i64 = 3_600_000;
/// A failed read stays cached for 60s, so a broken gh costs one spawn a
/// minute, never one spawn per gh call on the admit path.
const FAIL_TTL_MS: i64 = 60_000;
/// The wall bound for one `gh auth status`: the shared-hold cache exists
/// because a 15s stall there stalled every gh call on the machine
/// (gh_budget::shared_hold), and this read rides that same admit path.
const LOGIN_TIMEOUT_S: u64 = 10;

/// The active login of this machine's gh, or None when this process carries
/// a token env (that token may be some other account entirely, so nothing
/// here may key on it) or gh cannot answer.
pub fn current() -> Option<String> {
    let token_env =
        std::env::var_os("GH_TOKEN").is_some() || std::env::var_os("GITHUB_TOKEN").is_some();
    current_with(&cache_path(), crate::gh_budget::now_ms(), token_env, probe)
}

fn current_with(
    cache: &Path,
    now: i64,
    token_env: bool,
    read: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if token_env {
        return None;
    }
    let cached = std::fs::read_to_string(cache)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    if let Some(row) = cached {
        let read_at = row.get("read_at").and_then(Value::as_i64).unwrap_or(0);
        let ttl = if row.get("login").and_then(Value::as_str).is_some() {
            LOGIN_TTL_MS
        } else {
            FAIL_TTL_MS
        };
        if (0..ttl).contains(&(now - read_at)) {
            return row.get("login").and_then(Value::as_str).and_then(normalize);
        }
    }
    let login = read();
    // tmp-and-rename, the shared_hold_cached pattern: a torn cache file is
    // a miss, never a wrong login.
    let body = serde_json::json!({"read_at": now, "login": login}).to_string();
    let tmp = PathBuf::from(format!("{}.{}", cache.display(), std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, cache);
    }
    login
}

fn probe() -> Option<String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args([
        "auth",
        "status",
        "--active",
        "--hostname",
        "github.com",
        "--json",
        "hosts",
    ]);
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, LOGIN_TIMEOUT_S).ok()?;
    parse_hosts(&String::from_utf8_lossy(&out.stdout))
}

/// The active `github.com` entry of `gh auth status --json hosts`. A
/// keyring token carries its stored login even when the validation call
/// fails (gh 2.102.0 `status.go` sets `entry.Login` from the config before
/// any API call), so a rate-limited host still reads its login; an env
/// token resolves via API and reads empty on failure, which normalizes to
/// None and the caller acts per machine.
fn parse_hosts(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let entries = value.get("hosts")?.get("github.com")?.as_array()?;
    entries
        .iter()
        .find(|e| e.get("active").and_then(Value::as_bool) == Some(true))
        .and_then(|e| e.get("login").and_then(Value::as_str))
        .and_then(normalize)
}

/// GitHub logins ignore case and admit only letters, digits and hyphens;
/// anything else (an empty string from a failed validation, a stray space)
/// reads as no login.
fn normalize(login: &str) -> Option<String> {
    let trimmed = login.trim();
    let ok = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| trimmed.to_ascii_lowercase())
}

/// Beside the budget ledger, the same `state/locks` row as the fleet hold.
fn cache_path() -> PathBuf {
    let fallback = std::env::temp_dir();
    crate::gh_budget::ledger_path()
        .parent()
        .unwrap_or(&fallback)
        .join("github-login.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// The shape measured on this machine, verbatim.
    const ANSWER: &str = r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"bllshttng","tokenSource":"keyring"}]}}"#;
    /// The failing-token shape measured: state error, empty login.
    const FAILED: &str = r#"{"hosts":{"github.com":[{"state":"error","error":"non-200 OK status code: 401 Unauthorized","active":true,"host":"github.com","login":"","tokenSource":"GH_TOKEN"}]}}"#;

    #[test]
    fn two_reads_inside_the_ttl_spawn_gh_once() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("github-login.json");
        let spawns = Rc::new(Cell::new(0));
        let counter = Rc::clone(&spawns);
        let first = current_with(&cache, 1_000, false, move || {
            counter.set(counter.get() + 1);
            parse_hosts(ANSWER)
        });
        assert_eq!(first.as_deref(), Some("bllshttng"), "parsed and lowercased");
        let counter = Rc::clone(&spawns);
        let second = current_with(&cache, 2_000, false, move || {
            counter.set(counter.get() + 1);
            parse_hosts(ANSWER)
        });
        assert_eq!(second.as_deref(), Some("bllshttng"));
        assert_eq!(spawns.get(), 1, "the second read must ride the cache");
        assert!(cache.exists());
    }

    #[test]
    fn a_failed_read_is_cached_and_an_env_token_skips_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("github-login.json");
        let spawns = Rc::new(Cell::new(0));
        let counter = Rc::clone(&spawns);
        let first = current_with(&cache, 1_000, false, move || {
            counter.set(counter.get() + 1);
            parse_hosts(FAILED)
        });
        assert_eq!(first, None, "an empty login reads as no login");
        let counter = Rc::clone(&spawns);
        let second = current_with(&cache, 30_000, false, move || {
            counter.set(counter.get() + 1);
            parse_hosts(FAILED)
        });
        assert_eq!(second, None);
        assert_eq!(spawns.get(), 1, "the failure is cached for 60s");
        let counter = Rc::clone(&spawns);
        let third = current_with(&cache, 70_000, false, move || {
            counter.set(counter.get() + 1);
            parse_hosts(ANSWER)
        });
        assert_eq!(
            third.as_deref(),
            Some("bllshttng"),
            "past the TTL it probes again"
        );
        let counter = Rc::clone(&spawns);
        let with_env = current_with(&cache, 80_000, true, move || {
            counter.set(counter.get() + 1);
            parse_hosts(ANSWER)
        });
        assert_eq!(with_env, None, "a token env reads None");
        assert_eq!(spawns.get(), 2, "the env check skips the probe");
    }
}

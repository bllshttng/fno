use super::{parse_pr_facts, PrFacts};
use crate::pr_status_facts::GhProbe;
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

const READ_BOUND: Duration = Duration::from_secs(15);

pub(super) fn read(cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String> {
    read_with(OsStr::new(&crate::scrape::fno_bin()), cwd, pr, READ_BOUND)
}

fn read_with(bin: &OsStr, cwd: &Path, pr: Option<u64>, bound: Duration) -> Result<PrFacts, String> {
    // A numeric PR with a resolvable slug reads in Rust through the
    // conditional door: one admitted `gh api` read whose 304 costs no core.
    // The `fno do pr info` spawn stays for branch resolution (pr is None)
    // and a missing slug, the two cases where Rust cannot name the
    // resource; behavior is unchanged there.
    if let Some(n) = pr {
        if let Some(slug) = crate::pr_status::cache::git_slug(cwd) {
            return read_pulls(cwd, &slug, n);
        }
    }
    let number = pr.map(|n| n.to_string());
    let mut args = vec!["do", "pr", "info"];
    if let Some(number) = number.as_deref() {
        args.push(number);
    }
    let out = crate::loopcheck::bounded_read(bin, &args, cwd, "PR head", bound)
        .map_err(|e| crate::loopcheck::bounded_read_diagnostic("PR head", &e))?;
    if !out.status.success() {
        return Err(format!(
            "fno do pr info failed: {}",
            String::from_utf8_lossy(&out.stderr_tail).trim()
        ));
    }
    let payload = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("fno do pr info returned unreadable JSON: {e}"))?;
    parse_pr_facts(&payload)
}

/// The PR read in Rust: one conditional `gh api` call through the same
/// probe the status door uses (admitted, 120 s bound inside the probe),
/// mapped by the one state mapping the cache keeps.
fn read_pulls(cwd: &Path, slug: &str, pr: u64) -> Result<PrFacts, String> {
    let args = vec!["api".to_string(), format!("repos/{slug}/pulls/{pr}")];
    let (ok, stdout, stderr) = crate::pr_status_facts::RealGhProbe
        .run_gh(cwd, &args)
        .map_err(|e| format!("PR head read failed: {e}"))?;
    if !ok {
        return Err(format!("PR head read failed: {}", stderr.trim()));
    }
    let payload: Value = serde_json::from_str(&stdout)
        .map_err(|e| format!("PR head read returned unparseable JSON: {e}"))?;
    pulls_facts(&payload, pr)
}

/// The pulls payload to PrFacts: number, head, base, url, body (a JSON
/// null reads as an empty string; an absent key is None, the same
/// out-of-date-deployed-fno reading parse_pr_facts keeps), state via the
/// cache's one mapping, and armed from auto_merge non-null.
fn pulls_facts(payload: &Value, pr: u64) -> Result<PrFacts, String> {
    let head_sha = payload
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if head_sha.is_empty() {
        return Err(format!("the PR read carried no head sha for PR {pr}"));
    }
    Ok(PrFacts {
        number: payload.get("number").and_then(Value::as_u64).unwrap_or(pr),
        head_sha,
        head_ref: payload
            .pointer("/head/ref")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        base_ref: payload
            .pointer("/base/ref")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: payload
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        body: payload
            .get("body")
            .map(|v| v.as_str().unwrap_or("").to_string()),
        state: crate::pr_status::cache::pr_state_word(payload),
        armed: payload
            .get("auto_merge")
            .map(|v| !v.is_null())
            .unwrap_or(false),
    })
}

pub(super) fn receipt(payload: &Value) -> Value {
    let Some(cwd) = payload.get("cwd").and_then(Value::as_str) else {
        return json!({"head": null, "error": "payload needs cwd"});
    };
    let Some(pr) = payload.get("pr").and_then(Value::as_u64) else {
        return json!({"head": null, "error": "payload needs numeric pr"});
    };
    match read(Path::new(cwd), Some(pr)) {
        Ok(facts) => json!({"head": facts.head_sha}),
        Err(error) => json!({"head": null, "error": error}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_payload_maps_to_facts_and_the_stub_fallback_stays_bounded() {
        // AC5-HP: the pulls payload maps field-for-field; a null body reads
        // as Some("") and auto_merge null reads unarmed.
        let payload = json!({
            "number": 7, "head": {"sha": "abc", "ref": "feat"}, "base": {"ref": "main"},
            "html_url": "u", "body": null, "state": "closed",
            "merged_at": "2026-10-16T00:00:00Z", "auto_merge": null
        });
        let facts = pulls_facts(&payload, 7).unwrap();
        assert_eq!(facts.head_sha, "abc");
        assert_eq!(facts.head_ref, "feat");
        assert_eq!(facts.base_ref, "main");
        assert_eq!(facts.body, Some(String::new()), "null body is Some(\"\")");
        assert_eq!(facts.state, "MERGED");
        assert!(!facts.armed);
        assert_eq!(facts.url, "u");
        assert_eq!(facts.number, 7);

        // AC5-EDGE: no head sha is an Err naming it, never a stale value.
        let err = pulls_facts(&json!({"number": 7, "head": {}}), 7).unwrap_err();
        assert!(err.contains("no head sha"), "{err}");

        // The `fno do pr info` spawn survives for the missing-slug case
        // (this tempdir has no git), and the bounded, malformed-refusing
        // contract rides with it.
        let dir = tempfile::tempdir().unwrap();
        let bin = crate::write_exec_stub(dir.path(), "fno", "#!/bin/sh\n[ \"$*\" = 'do pr info 7' ] || exit 1\nprintf '%s' '{\"pr\":7,\"head_sha\":\"old-head\"}'\n");
        assert_eq!(
            read_with(bin.as_os_str(), dir.path(), Some(7), Duration::from_secs(2))
                .unwrap()
                .head_sha,
            "old-head"
        );
        let bin = crate::write_exec_stub(dir.path(), "fno", "#!/bin/sh\n/bin/sleep 30\n");
        let start = std::time::Instant::now();
        let error = read_with(
            bin.as_os_str(),
            dir.path(),
            Some(7),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.contains("outcome=timeout"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}

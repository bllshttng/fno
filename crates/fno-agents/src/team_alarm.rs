//! The empty-team alarm: a teamed scope whose board still holds ready work
//! and which no live session holds is a silent stall. One finding folded into
//! the `arm_watch` tick, so dead arms and an empty team land in ONE page.
//! The alarm notifies; it never gates dispatch and never takes a team.

use crate::stuck_work::Finding;
use serde_json::Value;
use std::path::Path;

/// A team mid-handoff is briefly empty by design, so the alarm waits this
/// long from the first tick that saw the scope empty. The number is not
/// invented: it is the floor `stuck_work` already applies to a hung command
/// (its `DEFAULT_HUNG_FLOOR_S`), the `notify.arm_failing_after_s` default the
/// confirmed send already pages at, two lead_wake beats, and six arm_watch
/// beats. One control plane, one meaning of "long enough".
// ponytail: the first-seen stamp lives in the notify signal store, so a
// restart delays the page by one grace and never suppresses it; a durable
// stamp needs an event the team writes to mark the turn to empty. Build
// that only when a restart-correlated miss is measured.
pub const TEAM_EMPTY_GRACE_S: u64 = 1800;

fn s_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// One read of the org payload: the findings whose grace has run, plus a
/// short note naming the states a reader should still see (a scope inside
/// its grace). An unreadable instrument is `Err`, never an empty list: a
/// blind read must not read as "nothing is wrong".
pub fn evaluate(
    payload: &Value,
    store: &Path,
    now_unix: u64,
) -> Result<(Vec<Finding>, String), String> {
    if payload.get("registry_readable").and_then(Value::as_bool) != Some(true) {
        let reason = payload
            .pointer("/summary/reason")
            .and_then(Value::as_str)
            .unwrap_or("the registry could not be read");
        return Err(format!("registry unreadable: {reason}"));
    }
    if payload
        .pointer("/summary/sweep_ran")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(
            "the manifest-only sweep did not run, so a zero answer is an absence".to_string(),
        );
    }
    let teams = payload
        .get("teams")
        .and_then(Value::as_array)
        .ok_or_else(|| "the org payload carries no teams list".to_string())?;
    let mut findings = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for team in teams {
        let Some(scope) = s_str(team, "scope") else {
            continue;
        };
        // The fold is the instrument for the board half. One unreadable
        // scope fails the whole read, never a clean empty beside it.
        let fold = team
            .get("scope_nodes")
            .ok_or_else(|| format!("scope {scope} carries no fold"))?;
        if s_str(fold, "status") != Some("ok") {
            let reason = s_str(fold, "reason").unwrap_or("the fold did not run");
            return Err(format!("scope {scope} fold unread: {reason}"));
        }
        let unclaimed: Vec<String> = fold
            .pointer("/stuck/unclaimed")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let key = format!("team_empty:{scope}");
        seen.insert(key.clone());
        let empty = s_str(team, "status") == Some("manifest-only");
        if empty && !unclaimed.is_empty() {
            let age_s = match crate::operator_notice::first_seen_age_s(store, &key, now_unix) {
                Some(age_s) => age_s,
                None => {
                    crate::operator_notice::mark_once(store, &key, "empty");
                    0
                }
            };
            if age_s >= TEAM_EMPTY_GRACE_S {
                let threshold_min = fold
                    .pointer("/stuck/threshold_minutes")
                    .and_then(Value::as_u64)
                    .unwrap_or(60);
                let first_seen = now_unix.saturating_sub(age_s);
                findings.push(empty_team_finding(
                    team,
                    scope,
                    age_s,
                    threshold_min,
                    first_seen,
                    &unclaimed,
                ));
            } else {
                notes.push(format!(
                    "team {scope} empty {age_s}s into a {TEAM_EMPTY_GRACE_S}s grace: {}s more",
                    TEAM_EMPTY_GRACE_S - age_s
                ));
            }
        } else {
            // The scope stopped reading empty: recovery is the designed
            // quiet, and the next episode starts a fresh clock.
            crate::operator_notice::forget_at(store, &key);
        }
    }
    // A readable payload is the authority on which scopes exist: a clock
    // whose scope left it is stale and would page a fresh handoff instantly.
    // The Err paths above return before this, so a blind read erases
    // nothing.
    for key in crate::operator_notice::keys_with_prefix(store, "team_empty:") {
        if !seen.contains(&key) {
            crate::operator_notice::forget_at(store, &key);
        }
    }
    Ok((findings, notes.join("; ")))
}

fn empty_team_finding(
    team: &Value,
    scope: &str,
    age_s: u64,
    threshold_min: u64,
    first_seen: u64,
    unclaimed: &[String],
) -> Finding {
    let holder = s_str(team, "holder").unwrap_or("unknown holder");
    let manifest = match s_str(team, "manifest_path") {
        Some(path) => format!("team on manifest {path}"),
        None => "team on the manifest".to_string(),
    };
    Finding {
        kind: "empty_team",
        key: format!("team_empty:{scope}@{first_seen}"),
        line: format!(
            "no lead on scope {scope}: {manifest}, holder {holder}, 0 live rows; \
             empty {}m against a {}m grace; {} ready over {threshold_min}m with no \
             worker ({}); respawn: fno agents spawn --promote {scope} --hand-off; \
             this notifies only, it gates no dispatch and takes no team",
            age_s / 60,
            TEAM_EMPTY_GRACE_S / 60,
            unclaimed.len(),
            crate::org_fold::named_ids(unclaimed),
        ),
        root: None,
        holder: Some(holder.to_string()),
        claim_key: None,
    }
}

/// The daemon-facing read: the team read runs in process
/// (`team_read::team_nodes_payload_at`), anchored at the config root the
/// caller serves, then judges its payload.
pub fn collect(config_cwd: &Path) -> Result<(Vec<Finding>, String), String> {
    let payload = read_org_payload(config_cwd);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    collect_from(
        &Ok(payload),
        &crate::operator_notice::notify_signals_path(),
        now,
    )
}

/// The judge over an already-read payload: the same verdict `collect`
/// answers, with the read handed in, so one org read feeds both the
/// team alarm and the settle pass.
pub(crate) fn collect_from(
    payload: &Result<Value, String>,
    store: &Path,
    now_unix: u64,
) -> Result<(Vec<Finding>, String), String> {
    match payload {
        Err(reason) => Err(reason.clone()),
        Ok(payload) => evaluate(payload, store, now_unix),
    }
}

/// The daemon-facing org read, in process: the team read's `--nodes` payload
/// builder (`team_read::team_nodes_payload_at`) anchored at the config root
/// the caller serves - the anchor the spawned child inherited via its own
/// cwd before this port. The old child's wall budget bounded a hung spawn;
/// an in-process read has no child to kill, so a slow read delays the beat
/// instead of failing it. The answer is always a payload, the way the old
/// child's exit-0 stdout was: a registry-unreadable board rides its own
/// flags (`registry_readable`, `summary.reason`) and the judge refuses it.
pub(crate) fn read_org_payload(config_cwd: &Path) -> Value {
    crate::team_read::team_nodes_payload_at(Some(config_cwd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    const NOW: u64 = 1_788_523_200; // 2026-09-04T12:00 to the second

    fn stuck(unclaimed: &[&str]) -> Value {
        json!({"unclaimed": unclaimed, "blocked": [], "unproven_claim": [],
               "in_review": [], "blind": [], "threshold_minutes": 60})
    }

    fn fold(unclaimed: &[&str]) -> Value {
        json!({"status": "ok", "teams": [], "nodes": [], "stuck": stuck(unclaimed)})
    }

    fn manifest_team(scope: &str, fold: Value) -> Value {
        json!({
            "holder": "sess-uuid-1", "level": 1, "scope": scope,
            "grantor": "human", "status": "manifest-only", "agree": Value::Null,
            "team_source": "manifest", "manifest_path": "/spaces/w/manifest",
            "scope_nodes": fold,
        })
    }

    fn payload(teams: Value) -> Value {
        json!({
            "teams": teams, "conflicts": [], "registry_readable": true,
            "graph_readable": true,
            "summary": {"sweep_ran": true, "total": 1, "manifest_only": 1},
        })
    }

    fn temp_store(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("fno-team-alarm-{name}-{}", std::process::id()))
    }

    fn seed(path: &Path, scope: &str, ts: &str) {
        std::fs::write(
            path,
            format!(r#"{{"team_empty:{scope}": {{"token": "empty", "ts": "{ts}"}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn an_empty_team_over_ready_work_raises_past_the_grace() {
        let store = temp_store("raise");
        seed(&store, "alpha", "2026-09-04T11:30:00Z");
        let p = payload(json!([manifest_team("alpha", fold(&["x-1"]))]));
        let (findings, note) = evaluate(&p, &store, NOW).unwrap();
        assert_eq!(note, "");
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.kind, "empty_team");
        assert!(f.key.starts_with("team_empty:alpha@"));
        let line = &f.line;
        // The four contract clauses, as a conjunction of open markers: no
        // terminator is pinned, so a correctly added clause cannot fail this.
        assert!(line.contains("no lead on scope alpha"), "{line}");
        assert!(
            line.contains("team on manifest /spaces/w/manifest"),
            "{line}"
        );
        assert!(line.contains("holder sess-uuid-1"), "{line}");
        assert!(line.contains("0 live rows"), "{line}");
        assert!(line.contains("empty 30m against a 30m grace"), "{line}");
        assert!(line.contains("ready over 60m with no worker"), "{line}");
        assert!(line.contains("x-1"), "{line}");
        assert!(
            line.contains("respawn: fno agents spawn --promote alpha --hand-off"),
            "{line}"
        );
        assert!(line.contains("gates no dispatch"), "{line}");
        std::fs::remove_file(&store).ok();
    }

    #[test]
    fn a_sweep_that_did_not_run_reads_unreadable_never_clear() {
        let store = temp_store("nosweep");
        let mut p = payload(json!([]));
        p["summary"]["sweep_ran"] = json!(false);
        let err = evaluate(&p, &store, NOW).unwrap_err();
        assert!(err.contains("sweep"), "{err}");
        std::fs::remove_file(&store).ok();
    }
}

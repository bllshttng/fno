//! `fno agents liveness <handle...>` -- the one "is this worker alive" answer
//! (the census rank-2 instrument, once instead of hand-rolled per job):
//! transcript age via the one reader, pid via the door's probe, registry
//! status, and the verdict folded through [`crate::row_verdict`] so no
//! caller grows a second ladder. The caller's own row rides along marked
//! `control: true`, because an ABSENT answer means nothing unless the
//! instrument reads a known-alive row as alive. Daemon-free like `wait`.
//!
//! Exit codes: `0` every requested handle live, `1` any finished, `2` any
//! unknown, `3` no handle resolved.

use crate::agent_ref::{self, Join, Key};
use crate::paths::AgentsHome;
use crate::row_verdict::{self, RowVerdict};
use crate::state::{self, RegistryEntry};
use crate::truth_probe::TruthProbe;
use serde_json::{json, Value};

/// Exit code when no requested handle resolved to a row or a transcript.
pub const ABSENT_EXIT: i32 = 3;

/// The session-id envs the self control falls back through when
/// FNO_AGENT_SELF is unset. Ambient markers a hook stamps.
const SELF_SESSION_ENVS: &[&str] = &[
    "FNO_HARNESS_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "OPENCODE_SESSION_ID",
];

/// `fno agents liveness <handle...> [-J|--json]`
pub fn run(rest: &[String], home: &AgentsHome) -> i32 {
    let (self_name, self_sid) = ambient_self();
    run_measured(
        rest,
        home,
        crate::truth_probe::family1_truth_probe,
        &self_name,
        &self_sid,
    )
}

/// Testable body: the truth reader and the self identity are injected so a
/// test never reads the ambient machine.
pub fn run_measured(
    rest: &[String],
    home: &AgentsHome,
    probe: impl Fn(&str) -> Option<TruthProbe>,
    self_name: &Option<String>,
    self_sid: &Option<String>,
) -> i32 {
    let mut handles: Vec<String> = Vec::new();
    let mut json_out = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" | "-J" => json_out = true,
            other if other.starts_with('-') && other.len() > 1 => {
                eprintln!("fno-agents: liveness: unknown flag: {other}");
                return 2;
            }
            _ => handles.push(a.clone()),
        }
    }
    if handles.is_empty() {
        eprintln!(
            "fno-agents: liveness requires at least one handle (name, short id, or session id)"
        );
        return 2;
    }
    let reg = match state::load_registry(&home.registry_json()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fno-agents: liveness: {}", e);
            return 1;
        }
    };
    let rows = &reg.entries;

    let self_row_ref = self_row(rows, self_name, self_sid);
    let mut out_rows: Vec<Value> = Vec::with_capacity(handles.len());
    let mut resolved_any = false;
    let mut any_finished = false;
    let mut any_unknown = false;
    for handle in &handles {
        let (row, ambiguous) = find_row(rows, handle);
        let truth_handle = row
            .as_deref()
            .and_then(crate::row_truth::row_truth_handle)
            .unwrap_or_else(|| handle.clone());
        let answer = answer_row(
            handle,
            row.as_deref(),
            probe(&truth_handle).as_ref(),
            is_control(&row, &self_row_ref),
            ambiguous,
        );
        match answer.get("verdict").and_then(Value::as_str) {
            Some("live") => resolved_any = true,
            Some("finished") => {
                resolved_any = true;
                any_finished = true;
            }
            Some("absent") => {}
            _ => {
                resolved_any = true;
                any_unknown = true;
            }
        }
        out_rows.push(answer);
    }
    // The documented ladder, not a max(): absent only when NOTHING resolved,
    // unknown over finished otherwise, so a mixed set keeps the doc's word.
    let worst = if !resolved_any {
        ABSENT_EXIT
    } else if any_unknown {
        2
    } else if any_finished {
        1
    } else {
        0
    };

    if json_out {
        // The control row is the self row re-read through the same door, so
        // a caller can verify the instrument on a row known to be alive.
        let control = self_row_ref
            .map(|row| {
                let th =
                    crate::row_truth::row_truth_handle(row).unwrap_or_else(|| row.name.clone());
                answer_row(&row.name, Some(row), probe(&th).as_ref(), true, false)
            })
            .unwrap_or(Value::Null);
        println!(
            "{}",
            json!({
                "rows": out_rows,
                "control": control,
                "control_resolved": self_row_ref.is_some(),
            })
        );
    } else {
        for row in &out_rows {
            println!("{}", human_line(row));
        }
        if self_row_ref.is_none() {
            eprintln!("liveness: no self row resolved; an ABSENT answer proves nothing");
        }
    }
    worst
}

/// The caller's own row: by FNO_AGENT_SELF label first, then by any ambient
/// harness session id. An ambiguous join never answers.
fn self_row<'a>(
    rows: &'a [RegistryEntry],
    self_name: &Option<String>,
    self_sid: &Option<String>,
) -> Option<&'a RegistryEntry> {
    if let Some(name) = self_name.as_deref() {
        if let Join::One(row) = agent_ref::resolve(rows, Key::Name(name), |_| true) {
            return Some(row);
        }
    }
    if let Some(sid) = self_sid.as_deref() {
        if let Join::One(row) = agent_ref::resolve(rows, Key::Id(sid), |_| true) {
            return Some(row);
        }
    }
    None
}

fn find_row<'a>(rows: &'a [RegistryEntry], handle: &str) -> (Option<&'a RegistryEntry>, bool) {
    match agent_ref::resolve(rows, Key::Name(handle), |_| true) {
        Join::One(row) => (Some(row), false),
        Join::Ambiguous => (None, true),
        Join::None => match agent_ref::resolve(rows, Key::Id(handle), |_| true) {
            Join::One(row) => (Some(row), false),
            Join::Ambiguous => (None, true),
            Join::None => (None, false),
        },
    }
}

fn is_control(row: &Option<&RegistryEntry>, self_row: &Option<&RegistryEntry>) -> bool {
    match (row, self_row) {
        (Some(r), Some(s)) => std::ptr::eq(*r, *s),
        _ => false,
    }
}

/// One handle's answer: the census fields plus the probe words the reading
/// rode in on. A rowless handle whose transcript resolved folds its probe
/// state through the door's vendor rung, never a bare guess. An ambiguous
/// join (two rows match the handle) reads unknown naming the ambiguity,
/// never absent, so the basis names the real cause.
fn answer_row(
    handle: &str,
    row: Option<&RegistryEntry>,
    probe: Option<&TruthProbe>,
    control: bool,
    ambiguous: bool,
) -> Value {
    let state = probe.map(|p| p.state.as_str());
    let (verdict, basis) = if ambiguous {
        (
            "unknown".to_string(),
            Some("ambiguous handle: two rows match the join".to_string()),
        )
    } else {
        match (row, probe) {
            (None, None) => (
                "absent".to_string(),
                Some("no row, no transcript".to_string()),
            ),
            _ => fold(row, state),
        }
    };
    let pid = row.and_then(|r| r.pid);
    json!({
        "handle": handle,
        "name": row.map(|r| r.name.clone()),
        "transcript_path": row.and_then(|r| r.transcript_path.clone()),
        "transcript_age_s": probe.and_then(|p| p.last_activity_age_s),
        "transcript_age_basis": probe.and_then(|p| p.last_activity_basis.clone()),
        "pid": pid,
        "pid_alive": pid.map(|p| !crate::daemon::pid_is_gone(p)),
        "registry_status": row.map(crate::agents_top::status_word),
        "verdict": verdict,
        "verdict_basis": basis,
        "state": probe.map(|p| p.state.clone()),
        "reachability": probe.and_then(|p| p.reachability.clone()),
        "control": control,
    })
}

fn word(v: &RowVerdict) -> &'static str {
    match v {
        RowVerdict::Live(_) => "live",
        RowVerdict::Finished(_) => "finished",
        RowVerdict::Unknown(_) => "unknown",
    }
}

/// The fold: a row's door verdict, vendor-reconciled with the probe state. A
/// rowless handle folds an Unknown door through the probe state alone, so a
/// store-resolved transcript can still answer live or finished, never a bare
/// guess.
fn fold(row: Option<&RegistryEntry>, state: Option<&str>) -> (String, Option<String>) {
    let door = match row {
        Some(row) => row_verdict::fno_verdict(row),
        None => RowVerdict::Unknown("no registry row".to_string()),
    };
    let folded = row_verdict::reconcile(&door, state);
    let basis = match &folded {
        RowVerdict::Live(source) => Some((*source).to_string()),
        RowVerdict::Finished(reason) => Some(reason.clone()),
        RowVerdict::Unknown(why) => Some(why.clone()),
    };
    (word(&folded).to_string(), basis)
}

fn human_line(row: &Value) -> String {
    let g = |k: &str| row.get(k).cloned().unwrap_or(Value::Null);
    let name = g("name").as_str().map(str::to_string).unwrap_or_else(|| {
        g("handle")
            .as_str()
            .map(str::to_string)
            .unwrap_or("?".to_string())
    });
    let age = g("transcript_age_s")
        .as_f64()
        .map(|a| format!("{a:.0}s"))
        .unwrap_or_else(|| "unread".to_string());
    let pid = g("pid")
        .as_u64()
        .map(|p| format!("pid={p}"))
        .unwrap_or_else(|| "pid=-".to_string());
    let ctrl = if g("control").as_bool() == Some(true) {
        " CONTROL(self)"
    } else {
        ""
    };
    format!(
        "{} {} age={age} {pid} registry={} basis={}{}",
        g("verdict").as_str().unwrap_or("unknown").to_uppercase(),
        name,
        g("registry_status").as_str().unwrap_or("-"),
        g("verdict_basis").as_str().unwrap_or("-"),
        ctrl,
    )
}

fn ambient_self() -> (Option<String>, Option<String>) {
    let name = std::env::var("FNO_AGENT_SELF")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let sid = SELF_SESSION_ENVS
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.trim().is_empty()));
    (name, sid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as V;

    fn entry(overrides: V) -> RegistryEntry {
        let mut base = json!({
            "name": "a",
            "provider": "claude",
            "cwd": "/tmp",
            "created_at": "2026-01-01T00:00:00Z",
            "status": "live",
        });
        if let (Value::Object(b), Value::Object(o)) = (&mut base, overrides) {
            b.extend(o);
        }
        serde_json::from_value(base).expect("fixture deserializes")
    }

    fn probe(state: &str, age: f64) -> Option<TruthProbe> {
        Some(TruthProbe {
            state: state.into(),
            reachability: Some("reachable".into()),
            basis: Some("transcript".into()),
            last_activity_age_s: Some(age),
            last_activity_basis: Some("mtime".into()),
            last_event_at: None,
            last_message: None,
            observed_model: Value::Null,
            provider_refusal: None,
            harness_title: None,
        })
    }

    fn fixture_home(rows: Vec<RegistryEntry>) -> (tempfile::TempDir, AgentsHome) {
        let dir = tempfile::tempdir().expect("tmpdir");
        let home = AgentsHome::at(dir.path());
        state::update_registry(&home.registry_json(), |registry| {
            registry.entries.extend(rows);
        })
        .expect("registry fixture writes");
        (dir, home)
    }

    const NONE: Option<String> = None;

    #[test]
    fn verdict_ladder_and_absent() {
        // The door owns every verdict; this command only words it. A reaped pid
        // finishes the row even with a fresh transcript; evidence-free reads
        // unknown, never dead; a handle nothing answers for is its own word.
        let gone = crate::row_verdict::reaped_pid();
        let (d1, home) = fixture_home(vec![entry(
            json!({"name": "dead-row", "pid": gone, "pid_start_time": 1}),
        )]);
        let _ = d1;
        assert_eq!(
            run_measured(
                &["dead-row".into()],
                &home,
                |_| probe("working", 5.0),
                &NONE,
                &NONE
            ),
            1,
            "a reaped pid finishes the row even with a fresh transcript"
        );
        let row = entry(json!({
            "name": "dark-row",
            "harness": "claude",
            "harness_session_id": "sess-dark",
        }));
        let (d2, home) = fixture_home(vec![row]);
        let _ = d2;
        assert_eq!(
            run_measured(&["dark-row".into()], &home, |_| None, &NONE, &NONE),
            2,
            "evidence-free reads unknown, never dead"
        );
        assert_eq!(
            run_measured(&["ghost".into()], &home, |_| None, &NONE, &NONE),
            ABSENT_EXIT,
            "absent is its own word, never collapsed into dead"
        );
    }

    #[test]
    fn live_control_and_census_shape() {
        let row = entry(json!({
            "name": "me",
            "pid": std::process::id(),
            "pid_start_time": crate::daemon::process_start_time(std::process::id()).unwrap_or(0),
            "transcript_path": "/tmp/t.jsonl",
        }));
        let (d, home) = fixture_home(vec![row]);
        let _ = d;
        let code = run_measured(
            &["me".into()],
            &home,
            |_| probe("working", 3.0),
            &Some("me".to_string()),
            &NONE,
        );
        assert_eq!(code, 0, "a live pid with a working transcript is live");
        let reg = state::load_registry(&home.registry_json()).expect("registry reads");
        let me = self_row(&reg.entries, &Some("me".to_string()), &NONE);
        assert!(me.is_some_and(|r| r.name == "me"));
        assert!(self_row(&reg.entries, &NONE, &NONE).is_none());
        let answer = answer_row(
            "me",
            me,
            probe("working", 7.0).as_ref(),
            me.is_some(),
            false,
        );
        let fields = [
            "handle",
            "name",
            "transcript_path",
            "transcript_age_s",
            "transcript_age_basis",
            "pid",
            "pid_alive",
            "registry_status",
            "verdict",
            "verdict_basis",
            "state",
            "reachability",
            "control",
        ];
        for field in fields {
            assert!(answer.get(field).is_some(), "missing field {field}");
        }
        assert_eq!(answer.get("transcript_age_s"), Some(&json!(7.0)));
        assert_eq!(answer.get("pid_alive"), Some(&json!(true)));
    }
}

//! The discovered-live-sessions lane: host-local sessions the fno registry does
//! not own, unioned from every harness store and classified by the one truth
//! reader. Robustness is the contract: a malformed, mid-write or vanished file
//! is skipped, and any failure leaves `fno agents list` an empty lane.

pub mod aliases;
pub mod sources;

use crate::paths::AgentsHome;
use crate::session_truth::{self, Stores, TruthSession};
use crate::spawn_context::session_identity_key;
use crate::state::RegistryEntry;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// One session candidate, the row shape every source yields.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub session_id: String,
    pub short_id: String,
    pub pid: i64,
    pub cwd: String,
    pub status: Option<String>,
    pub agent: String,
    pub transcript_path: Option<String>,
    pub name: Option<String>,
    pub project: Option<String>,
    /// The source registry row. Its falsifier probes a pane or a pid, so it is
    /// read only for a row the lane will print.
    pub registry_row: Option<RegistryEntry>,
}

/// The family-1 reading of one session.
#[derive(Debug, Clone, PartialEq)]
pub struct TruthReading {
    pub state: String,
    pub age_s: Option<i64>,
    pub observed_model: Value,
    pub provider_refusal: Option<String>,
}

impl TruthReading {
    pub fn from_payload(payload: &Value) -> Self {
        TruthReading {
            state: payload
                .get("state")
                .and_then(Value::as_str)
                .filter(|state| !state.is_empty())
                .unwrap_or("unknown")
                .to_string(),
            age_s: payload
                .get("last_activity_age_s")
                .and_then(Value::as_f64)
                .map(|age| age as i64),
            observed_model: payload
                .get("observed_model")
                .cloned()
                .unwrap_or(Value::Null),
            provider_refusal: payload
                .get("provider_refusal")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    /// The addressing filter, never the verdict: a stale pid still leaves a
    /// resumable session addressable.
    pub fn is_alive(&self) -> bool {
        matches!(self.state.as_str(), "working" | "watching" | "your-move")
    }
}

/// A session with its alias and truth reading attached.
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    pub candidate: Candidate,
    pub handle: String,
    pub truth: TruthReading,
}

/// Every input the lane reads, so a test can point it at fixture stores.
pub struct Sources {
    pub sessions_dirs: Vec<PathBuf>,
    pub projects_dir: PathBuf,
    pub codex_sessions_dir: PathBuf,
    pub opencode_storage_dir: PathBuf,
    pub name_map_path: PathBuf,
    pub registry_path: PathBuf,
    pub roster_path: PathBuf,
    pub recency_s: f64,
    /// `(pid, cwd)` of each running Claude Code session process.
    pub live_procs: Vec<(u32, String)>,
    /// `(session_id, cwd)` of each thread the Codex app-server has loaded.
    pub loaded_threads: Vec<(String, String)>,
    /// `<8-hex job id> -> pid` of the process that IS that bg session.
    pub bg_socket_pids: Vec<(String, u32)>,
}

impl Sources {
    pub fn ambient() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let sessions_dirs = match non_empty_env("FNO_CLAUDE_SESSIONS_DIR") {
            Some(dir) => vec![dir],
            None => crate::claude_ask::ClaudeHome::from_env().sessions_dirs(),
        };
        let state_root = crate::watchdog::sweep::state_root();
        Sources {
            sessions_dirs,
            projects_dir: non_empty_env("FNO_CLAUDE_PROJECTS_DIR")
                .unwrap_or_else(|| home.join(".claude").join("projects")),
            codex_sessions_dir: crate::store_heal::codex_sessions_dir(),
            opencode_storage_dir: crate::store_heal::opencode_storage_dir(),
            name_map_path: state_root.join("mux").join("session-names.json"),
            registry_path: AgentsHome::shared_registry_json(),
            roster_path: crate::claude_roster::default_roster_path(),
            recency_s: recency_seconds(),
            live_procs: sources::live_claude_procs(),
            loaded_threads: sources::codex_loaded_threads(),
            bg_socket_pids: crate::footprint_cause::bg_socket_pid_map(None)
                .into_iter()
                .collect(),
        }
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The transcript-mtime window: env-overridable, positive only.
fn recency_seconds() -> f64 {
    std::env::var("FNO_CLAUDE_SESSION_RECENCY_SECONDS")
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(600.0)
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// Fold the union to one row per (harness, normalized id). Source order keeps
/// daemon liveness precedence; a later row only fills what the first lacks.
pub fn dedup_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut index: HashMap<(String, String), usize> = HashMap::new();
    let mut live: Vec<Candidate> = Vec::new();
    for row in candidates {
        let key = (row.agent.clone(), session_identity_key(&row.session_id));
        match index.get(&key) {
            None => {
                index.insert(key, live.len());
                live.push(row);
            }
            Some(&at) => {
                let kept = &mut live[at];
                if kept.cwd.is_empty() && !row.cwd.is_empty() {
                    kept.cwd = row.cwd;
                }
                if kept.transcript_path.as_deref().unwrap_or("").is_empty()
                    && row
                        .transcript_path
                        .as_deref()
                        .is_some_and(|p| !p.is_empty())
                {
                    kept.transcript_path = row.transcript_path;
                }
                if kept.name.as_deref().unwrap_or("").is_empty()
                    && row.name.as_deref().is_some_and(|n| !n.is_empty())
                {
                    kept.name = row.name;
                }
            }
        }
    }
    live
}

/// Enumerate, attach project and alias, classify, and sort by handle. `Err`
/// means the alias store's parent cannot exist, which the caller reads as an
/// empty lane.
pub fn discover_live_sessions(
    sources_in: &Sources,
    exclude_short_ids: &HashSet<String>,
    exclude_session_ids: &HashSet<String>,
    project_of: &dyn Fn(&str) -> Option<String>,
    truth_of: &mut dyn FnMut(&Candidate, &str) -> TruthReading,
) -> Result<Vec<DiscoveredSession>, String> {
    let candidates = sources::enumerate(sources_in, exclude_short_ids, exclude_session_ids);
    let mut live = dedup_candidates(candidates);
    let mut projects: HashMap<String, Option<String>> = HashMap::new();
    for row in &mut live {
        if !row.cwd.is_empty() && !projects.contains_key(&row.cwd) {
            projects.insert(row.cwd.clone(), project_of(&row.cwd));
        }
        row.project = projects.get(&row.cwd).cloned().flatten();
    }
    let alias_map =
        aliases::resolve_aliases(&live, &sources_in.name_map_path, &sources_in.registry_path)?;
    let mut sessions: Vec<DiscoveredSession> = live
        .into_iter()
        .map(|candidate| {
            let handle = alias_map
                .get(&candidate.session_id)
                .cloned()
                .unwrap_or_else(|| crate::identity::canonical_handle(&candidate.session_id));
            let truth = truth_of(&candidate, &handle);
            DiscoveredSession {
                candidate,
                handle,
                truth,
            }
        })
        .collect();
    sessions.sort_by(|a, b| a.handle.cmp(&b.handle));
    Ok(sessions)
}

/// The mailbox a row can receive at. A non-claude `short_id` is a daemon worker
/// key, so only claude falls back to it.
pub fn row_address(agent: &str, session_id: &str, short_id: &str) -> Option<String> {
    if !session_id.is_empty() {
        return Some(crate::identity::canonical_handle(session_id));
    }
    (agent == "claude" && !short_id.is_empty()).then(|| short_id.to_string())
}

/// `pid 0` is this lane's "not recorded", which is absence of evidence. Any
/// other unusable value is a process that cannot exist.
fn pid_word(pid: i64) -> Option<&'static str> {
    if pid == 0 {
        return None;
    }
    let probe = u32::try_from(pid).unwrap_or(if pid < 0 { 0 } else { u32::MAX });
    session_truth::pid_falsifier(Some(probe), None)
        .or_else(|| (probe > 1 && crate::census::pid_is_zombie(probe)).then_some("process-gone"))
}

/// The canonical JSON row, with the falsifier handed in so a test needs no
/// live process.
pub fn to_row(session: &DiscoveredSession, falsifier: Option<&'static str>) -> Value {
    let row = &session.candidate;
    let truth = &session.truth;
    let (reachability, basis) = session_truth::classify_reachability_at(
        Some(truth.state.as_str()),
        truth.age_s,
        falsifier,
        session_truth::TRANSCRIPT_EVIDENCE_S as f64,
        None,
        None,
        &truth.observed_model,
    );
    let probe = crate::truth_probe::TruthProbe {
        state: truth.state.clone(),
        reachability: Some(reachability.to_string()),
        basis: Some(basis.to_string()),
        last_activity_age_s: truth.age_s.map(|age| age as f64),
        last_activity_basis: None,
        last_event_at: None,
        last_message: None,
        observed_model: truth.observed_model.clone(),
        provider_refusal: truth.provider_refusal.clone(),
        harness_title: None,
    };
    let status = crate::daemon::list_rows::rendered_status_from_truth(Some(&probe));
    // Never adopted by the registry, so never given a spawn-time route.
    let (progress, progress_basis) = crate::daemon::list_rows::progress_from_truth(
        Some(&probe),
        crate::truth_probe::BatchOutcome::Measured,
        &row.agent,
        None,
    );
    let mut out = Map::new();
    out.insert("handle".into(), json!(session.handle));
    out.insert("short_id".into(), json!(row.short_id));
    out.insert("session_id".into(), json!(row.session_id));
    out.insert(
        "address".into(),
        json!(row_address(&row.agent, &row.session_id, &row.short_id)),
    );
    out.insert("pid".into(), json!(row.pid));
    out.insert("cwd".into(), json!(row.cwd));
    out.insert("project".into(), json!(row.project));
    out.insert("status".into(), json!(status));
    out.insert("reachability".into(), json!(reachability));
    out.insert("basis".into(), json!(basis));
    out.insert("progress".into(), json!(progress));
    out.insert("progress_basis".into(), json!(progress_basis));
    out.insert("last_activity_age_s".into(), json!(truth.age_s));
    out.insert("agent".into(), json!(row.agent));
    Value::Object(out)
}

/// The ambient falsifier: the scanned pid, then the source registry row.
pub fn ambient_falsifier(row: &Candidate) -> Option<&'static str> {
    pid_word(row.pid).or_else(|| {
        row.registry_row
            .as_ref()
            .and_then(session_truth::registry_falsifier)
    })
}

/// `Path.resolve()`: lexical over the tail that does not exist, real over the
/// head that does, so a vanished worktree still compares.
pub fn resolve_lenient(raw: &str) -> String {
    let path = Path::new(raw);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(base) => base.join(path),
            Err(_) => return raw.to_string(),
        }
    };
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => {
                resolved.push(other);
                if let Ok(real) = std::fs::canonicalize(&resolved) {
                    resolved = real;
                }
            }
        }
    }
    resolved.display().to_string()
}

/// `--harness` then `--cwd`. An empty filter is no filter, and an empty row cwd
/// never matches: it must not resolve to the process cwd.
pub fn filter_rows(mut rows: Vec<Value>, cwd: Option<&str>, harness: Option<&str>) -> Vec<Value> {
    if let Some(want) = harness.filter(|value| !value.is_empty()) {
        rows.retain(|row| row.get("agent").and_then(Value::as_str) == Some(want));
    }
    if let Some(want) = cwd.filter(|value| !value.is_empty()) {
        let resolved = resolve_lenient(want);
        rows.retain(|row| {
            row.get("cwd")
                .and_then(Value::as_str)
                .filter(|raw| !raw.is_empty())
                .is_some_and(|raw| resolve_lenient(raw) == resolved)
        });
    }
    rows
}

fn registry_exclusions(path: &Path) -> (HashSet<String>, HashSet<String>) {
    let Ok(registry) = crate::state::load_registry(path) else {
        return (HashSet::new(), HashSet::new());
    };
    let shorts = registry
        .entries
        .iter()
        .filter(|entry| !entry.short_id.is_empty())
        .map(|entry| entry.short_id.clone())
        .collect();
    let session_ids = registry
        .entries
        .iter()
        .filter_map(|entry| entry.cc_session_id.clone())
        .filter(|sid| !sid.is_empty())
        .collect();
    (shorts, session_ids)
}

fn ambient_rows(cwd: Option<&str>, harness: Option<&str>) -> Result<Vec<Value>, String> {
    let sources_in = Sources::ambient();
    let (shorts, session_ids) = registry_exclusions(&sources_in.registry_path);
    let stores = Stores::ambient();
    let now_s = now_epoch();
    let mut truth_of = |row: &Candidate, handle: &str| {
        let session = TruthSession {
            agent: row.agent.clone(),
            session_id: row.session_id.clone(),
            transcript_path: row.transcript_path.as_deref().map(PathBuf::from),
            row: None,
        };
        TruthReading::from_payload(&session_truth::known_session_payload(
            &session, handle, now_s, &stores,
        ))
    };
    let sessions = discover_live_sessions(
        &sources_in,
        &shorts,
        &session_ids,
        &sources::resolve_project_for_cwd,
        &mut truth_of,
    )?;
    let rows: Vec<Value> = sessions
        .iter()
        .filter(|session| session.truth.is_alive())
        .map(|session| to_row(session, ambient_falsifier(&session.candidate)))
        .collect();
    Ok(filter_rows(rows, cwd, harness))
}

/// The filtered lane. Fail-open: an error or a reader panic is an empty lane,
/// so `agents list` never breaks on discovery.
pub fn discovered_rows(cwd: Option<&str>, harness: Option<&str>) -> Vec<Value> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ambient_rows(cwd, harness)))
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

/// `{"discovered_sessions": [...]}` for `--cwd` and `--harness`. Exit 2 on a
/// usage error, else 0 whatever discovery found.
pub fn run_discovered_json(args: &[String]) -> i32 {
    let mut cwd: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let Some(value) = args.get(index + 1) else {
            eprintln!("usage: fno agents discovered-json [--cwd PATH] [--harness NAME]");
            return 2;
        };
        match flag {
            "--cwd" => cwd = Some(value.clone()),
            "--harness" => harness = Some(value.clone()),
            "--provider" => {
                if !value.trim().is_empty() {
                    eprintln!(
                        "--provider was split at the axis rename and is retired on spawn; \
                         the route owns the vendor (docs/HARNESSES.md names each native form)."
                    );
                    return 2;
                }
            }
            _ => {
                eprintln!("usage: fno agents discovered-json [--cwd PATH] [--harness NAME]");
                return 2;
            }
        }
        index += 2;
    }
    let rows = discovered_rows(cwd.as_deref(), harness.as_deref());
    print!(
        "{}",
        crate::roles::cli::ensure_ascii(&crate::research::pyfmt::json_dumps(
            &json!({"discovered_sessions": rows})
        ))
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(agent: &str, sid: &str) -> Candidate {
        Candidate {
            session_id: sid.to_string(),
            short_id: crate::identity::canonical_handle(sid),
            agent: agent.to_string(),
            ..Candidate::default()
        }
    }

    #[test]
    fn identity_is_harness_and_id_never_the_id_alone() {
        let mut claude = candidate("claude", "ABCDEF01-x");
        claude.cwd = String::new();
        let mut upper = candidate("claude", "abcdef01-X");
        upper.cwd = "/repo".to_string();
        upper.name = Some("named".to_string());
        let codex = candidate("codex", "abcdef01-x");
        let live = dedup_candidates(vec![claude, upper, codex]);
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].session_id, "ABCDEF01-x");
        assert_eq!(live[0].cwd, "/repo");
        assert_eq!(live[0].name.as_deref(), Some("named"));
        assert_eq!(live[1].agent, "codex");
    }

    #[test]
    fn an_empty_row_cwd_never_matches_the_cwd_filter() {
        let here = std::env::current_dir().unwrap().display().to_string();
        let rows = vec![
            json!({"cwd": "", "agent": "claude"}),
            json!({"cwd": here, "agent": "codex"}),
        ];
        assert_eq!(filter_rows(rows.clone(), Some(&here), None).len(), 1);
        assert_eq!(filter_rows(rows.clone(), Some(""), Some("")).len(), 2);
        assert_eq!(filter_rows(rows, None, Some("codex")).len(), 1);
    }
}

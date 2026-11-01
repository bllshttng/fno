//! The real seams and the `recovery-sweep` command.

use super::mission::{graph_hex_node, graph_row, mission_complete_with};
use super::redispatch::{RedispatchSeams, RunResult};
use super::{
    run_recovery_sweep, truthy, Candidate, RecoveryCfg, SnapshotView, SweepSeams, CLOSE_MESSAGE,
};
use crate::accounts::error_class::{classify_error, reset_epoch_from, Cooldown};
use crate::accounts::state as account_state;
use crate::claude_ask::{self, ClaudeHome, SessionLocator};
use crate::session_truth::{cursor, resolve_payload, Stores};
use crate::state::RegistryEntry;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const COUNTS_FILE: &str = "recovery-nudges.json";

fn process_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn counts_path(cwd: &Path) -> Option<PathBuf> {
    let root = crate::agents_config::state_dir(cwd)?;
    Some(crate::state_layout::place(&root, COUNTS_FILE))
}

/// Write the cooldown for a refusal already attributed to an account. Returns
/// the account written, or `None`; a missing account and `default` are refused,
/// never resolved to the active one. A lock timeout skips the write, and the
/// event still names the account, as the Python writer does.
pub fn record_quota_lock(
    account: Option<&str>,
    text: &str,
    resets_at: Option<f64>,
    cwd: &Path,
    now: f64,
) -> Option<String> {
    let account = account.filter(|a| !a.is_empty() && *a != "default")?;
    if text.is_empty() {
        return None;
    }
    let rule = classify_error(None, Some(text))?;
    let (backoff, cooldown_ms) = match rule.cooldown {
        Cooldown::Backoff => (true, None),
        Cooldown::FixedMs(ms) => (false, Some(ms as i64)),
    };
    let resets_at = resets_at.or_else(|| {
        let zone = account_state::record_reset_timezone(cwd, account);
        reset_epoch_from(text, zone.as_deref(), now)
    });
    let path = crate::provider_cap::runtime_state_path(cwd);
    account_state::fold_legacy_files(&path, cwd, None);
    let span = resets_at
        .filter(|at| *at > now)
        .and_then(|_| account_state::record_window_seconds(cwd, account));
    let written = account_state::update_health(
        &path,
        account,
        account_state::Rule {
            backoff,
            cooldown_ms,
        },
        None,
        resets_at,
        span,
        now,
        account_state::LOCK_TIMEOUT,
    );
    if let Err(error) = written {
        eprintln!("recovery: health write skipped for {account}: {error}");
    }
    Some(account.to_string())
}

/// The production [`SweepSeams`] and [`RedispatchSeams`].
pub struct LiveSeams {
    cwd: PathBuf,
    rows: Vec<RegistryEntry>,
    home: ClaudeHome,
    stores: Stores,
}

impl LiveSeams {
    pub fn new(cwd: PathBuf) -> Result<Self, String> {
        let path = crate::paths::AgentsHome::from_env().registry_json();
        let registry = crate::state::load_registry(&path)
            .map_err(|error| format!("registry read failed: {error}"))?;
        Ok(Self {
            cwd,
            rows: registry.entries,
            home: ClaudeHome::from_env(),
            stores: Stores::ambient(),
        })
    }

    fn graph(&self) -> PathBuf {
        crate::graph_get::graph_path_for(&self.cwd)
    }
}

impl SweepSeams for LiveSeams {
    fn registry(&self) -> Result<Vec<RegistryEntry>, String> {
        Ok(self.rows.clone())
    }

    fn locate(&self, short_id: &str) -> Option<SessionLocator> {
        claude_ask::locate_session(&self.home, short_id)
    }

    /// A malformed `state.json` degrades to an empty view, so one bad session
    /// never aborts the tick.
    fn read_state(&self, jobs_dir: &Path) -> SnapshotView {
        match claude_ask::read_state_json(jobs_dir) {
            Ok(snap) => SnapshotView {
                state: snap.state,
                updated_at: snap.updated_at,
                output_result: snap.output_result,
            },
            Err(_) => SnapshotView::default(),
        }
    }

    fn truth(&self, candidate: &Candidate) -> Value {
        let handle = candidate.name.as_deref().unwrap_or(&candidate.short_id);
        resolve_payload(
            Some(self.rows.as_slice()),
            handle,
            account_state::wall_now(),
            &self.stores,
            cursor::global(),
        )
    }

    fn liveness(&self, sock_path: &str) -> bool {
        claude_ask::liveness_probe(sock_path)
    }

    fn load_counts(&self) -> Map<String, Value> {
        counts_path(&self.cwd)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| match value {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Best-effort: a failed save makes the cap soft, which beats crashing the
    /// tick on a disk error.
    fn save_counts(&self, counts: &Map<String, Value>) {
        let Some(path) = counts_path(&self.cwd) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(counts) {
            let _ = std::fs::write(path, text);
        }
    }

    fn mission_complete(&self, candidate: &Candidate) -> Option<bool> {
        let graph = self.graph();
        mission_complete_with(
            candidate,
            &|node: &str| graph_row(&graph, node),
            &|hex: &str| graph_hex_node(&graph, hex),
        )
    }

    fn notify_close(&self, candidate: &Candidate) -> bool {
        crate::operator_notice::notify_operator_confirmed(
            &format!("footnote: {} finished but still open", candidate.short_id),
            CLOSE_MESSAGE,
            None,
        )
    }

    /// The zone of the account the candidate runs on, project-rooted: a foreign
    /// candidate's active account lives in its own repository. An unresolved
    /// zone refuses the stamp, not the event.
    fn reset_timezone(&self, cwd: Option<&str>) -> Option<String> {
        let root = cwd.map(Path::new);
        let base = root.unwrap_or(self.cwd.as_path());
        let accounts = crate::accounts::records::load(root, base).ok()?;
        let active = crate::accounts::records::effective_active(&accounts, base)?;
        accounts
            .by_id(&active)?
            .reset_timezone
            .clone()
            .filter(|zone| !zone.is_empty())
    }

    fn record_quota_lock(
        &self,
        account: Option<&str>,
        text: &str,
        resets_at: Option<f64>,
    ) -> Option<String> {
        record_quota_lock(
            account,
            text,
            resets_at,
            &self.cwd,
            account_state::wall_now(),
        )
    }

    fn now(&self) -> f64 {
        account_state::wall_now()
    }
}

impl RedispatchSeams for LiveSeams {
    fn run(
        &self,
        argv: &[String],
        cwd: &str,
        timeout_s: u64,
        env: &[(&str, &str)],
    ) -> Option<RunResult> {
        let (program, rest) = argv.split_first()?;
        let binary: std::ffi::OsString = if program == "fno-agents" {
            crate::mail_cli::send_raw::agents_binary().into()
        } else {
            crate::scrape::fno_bin()
        };
        let mut cmd = std::process::Command::new(binary);
        cmd.args(rest)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let done = crate::bounded_cmd::output_with_timeout(cmd, timeout_s)?;
        Some(RunResult {
            code: done.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
        })
    }

    /// A read miss answers false: the claim and spawn are the real backstop.
    fn node_is_done(&self, node: &str) -> bool {
        graph_row(&self.graph(), node).is_some_and(|row| {
            !row.get("archived_at").is_some_and(truthy)
                && row.get("status").and_then(Value::as_str) == Some("done")
        })
    }

    fn alias_predecessor(&self, new_name: &str, old_name: Option<&str>) {
        let Some(alias) = old_name.map(str::trim).filter(|a| !a.is_empty()) else {
            return;
        };
        let path = crate::paths::AgentsHome::from_env().registry_json();
        let _ = crate::state::update_registry(&path, |registry| {
            let Some(at) = registry.entries.iter().position(|e| e.name == new_name) else {
                return;
            };
            let answers =
                |e: &RegistryEntry| e.name == alias || e.aliases.iter().any(|a| a == alias);
            if answers(&registry.entries[at]) {
                return;
            }
            if registry.entries.iter().any(answers) {
                return;
            }
            registry.entries[at].aliases.push(alias.to_string());
        });
    }
}

/// The `config.recovery` read, with the model's defaults for an absent or
/// invalid value, and whether the sweep is armed at all.
pub fn recovery_settings(cwd: &Path) -> (bool, RecoveryCfg) {
    let settings = crate::backlog::advance_settings::load_merged(Some(cwd));
    let block = settings.get("recovery");
    let int = |key: &str| block.and_then(|b| b.get(key)).and_then(Value::as_i64);
    let enabled = block
        .and_then(|b| b.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
        && crate::backlog::advance_settings::autonomy_master_enabled(Some(cwd))
        && crate::autonomy_policy::restart_allowed(cwd);
    let cfg = RecoveryCfg {
        idle_threshold_seconds: int("idle_threshold_seconds")
            .filter(|v| *v > 0)
            .unwrap_or(900),
        max_nudges: int("max_nudges").filter(|v| *v >= 1).unwrap_or(3),
    };
    (enabled, cfg)
}

/// `recovery-sweep`: one sweep, one JSON answer on stdout. `swept` is false when
/// config disarms the sweep, so a caller never reads a disarmed sweep as a
/// healthy quiet fleet. The caller emits the events. Exit 1 when the sweep could
/// not run, exit 2 on a usage error.
pub fn run_cli(args: &[String]) -> i32 {
    for arg in args {
        match arg.as_str() {
            "--json" | "-J" => {}
            "-h" | "--help" => {
                println!("usage: recovery-sweep [--json]\nRun one recovery sweep. Prints {{candidates, events, swept}}.");
                return 0;
            }
            other => {
                eprintln!("recovery-sweep: unknown argument {other}");
                return 2;
            }
        }
    }
    let cwd = process_cwd();
    let (enabled, cfg) = recovery_settings(&cwd);
    if !enabled {
        println!("{}", json!({"candidates": 0, "events": [], "swept": false}));
        return 0;
    }
    let seams = match LiveSeams::new(cwd) {
        Ok(seams) => seams,
        Err(error) => {
            eprintln!("recovery-sweep: {error}");
            return 1;
        }
    };
    let mut events: Vec<Value> = Vec::new();
    let mut emit = |kind: &str, data: Value| events.push(json!({"type": kind, "data": data}));
    match run_recovery_sweep(&cfg, &seams, &mut emit) {
        Ok(candidates) => {
            println!(
                "{}",
                json!({"candidates": candidates, "events": events, "swept": true})
            );
            0
        }
        Err(error) => {
            eprintln!("recovery-sweep: {error}");
            1
        }
    }
}

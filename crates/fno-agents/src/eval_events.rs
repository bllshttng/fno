//! The eval event arm: each attempt in the eval history ships as one PostHog
//! event, so trend and alert views can read it. PostHog files OTLP records as
//! logs, and its trend insights read events, so this posts to the capture
//! batch route and not through the OTLP arm. Off unless
//! `[telemetry.eval_events] enabled = true` and a token is set, through
//! `token_env` or the `token_file` read. The event carries an allowlist of
//! identifiers and never free text.

use crate::otlp_export::{self, ExportState};
use serde_json::{json, Map, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const EVENT_NAME: &str = "fno_eval_run";
pub const INTERVAL_S: u64 = 300;
const LOOKBACK_S: i64 = 86_400;
const MAX_BATCH: usize = 1000;
const DEFAULT_HOST: &str = "https://us.i.posthog.com";
const DEFAULT_TOKEN_ENV: &str = "FNO_POSTHOG_PROJECT_TOKEN";
const DEFAULT_TOKEN_FILE: &str = "~/.fno/secrets/posthog-project-token";
/// One shared id, because an eval attempt has no person behind it.
const DISTINCT_ID: &str = "fno-evals";
const MAX_VALUE_LEN: usize = 128;

pub enum Resolved {
    Off { forced_off_by: Option<&'static str> },
    Refused(String),
    On(Config),
}

pub struct Config {
    pub url: String,
    pub host: String,
    pub token: String,
}

pub fn enabled(cwd: &Path) -> bool {
    crate::agents_config::config_lookup(cwd, &["telemetry", "eval_events", "enabled"])
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub fn resolve(cwd: &Path) -> Resolved {
    if let Some(var) = otlp_export::forced_off() {
        return Resolved::Off {
            forced_off_by: Some(var),
        };
    }
    if !enabled(cwd) {
        return Resolved::Off {
            forced_off_by: None,
        };
    }
    let table = crate::agents_config::config_lookup(cwd, &["telemetry", "eval_events"]);
    let read = |key: &str, default: &str| {
        table
            .as_ref()
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(default)
            .to_string()
    };
    let base = read("host", DEFAULT_HOST);
    if !base.starts_with("https://") {
        return Resolved::Refused(format!(
            "telemetry.eval_events.host must be https, got: {base}"
        ));
    }
    let token_env = read("token_env", DEFAULT_TOKEN_ENV);
    let token_file = read("token_file", DEFAULT_TOKEN_FILE);
    let token = match token_from_env_or_file(&token_env, &token_file) {
        Ok(token) => token,
        Err(reason) => return Resolved::Refused(reason),
    };
    let base = base.trim_end_matches('/');
    Resolved::On(Config {
        url: format!("{base}/batch/"),
        host: base.trim_start_matches("https://").to_string(),
        token: token.trim().to_string(),
    })
}

/// The env var wins while it holds a token; otherwise the token file is read
/// fresh, so a daemon beat picks up a rotated token without a restart. The
/// file must be mode 0600: group or other bits refuse the read.
fn token_from_env_or_file(token_env: &str, token_file: &str) -> Result<String, String> {
    if let Ok(from_env) = std::env::var(token_env) {
        if !from_env.trim().is_empty() {
            return Ok(from_env);
        }
    }
    let path = crate::github_app::expand_tilde(Path::new(token_file));
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "token env {token_env} is unset or empty and token file {} is missing",
                path.display()
            ));
        }
        Err(e) => {
            return Err(format!(
                "token env {token_env} is unset or empty and token file {} is unreadable: {e}",
                path.display()
            ));
        }
    };
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "token file {} must be mode 0600 (run chmod 600 {})",
            path.display(),
            path.display()
        ));
    }
    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("token file {} is unreadable: {e}", path.display()))?;
    let token = body.trim();
    if token.is_empty() {
        return Err(format!("token file {} is empty", path.display()));
    }
    Ok(token.to_string())
}

/// A row value ships only when it reads as an identifier. A cohort is a typed
/// flag, so words in it stay on this machine.
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_VALUE_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
}

/// `pass` or `fail` for a valid grade, else the attempt status, so a plumbing
/// failure never counts as a failed task.
fn grade(row: &Value) -> &str {
    match row.get("status").and_then(Value::as_str) {
        Some("graded") if row.get("pass") == Some(&Value::Bool(true)) => "pass",
        Some("graded") => "fail",
        Some(other) if identifier(other) => other,
        _ => "legacy",
    }
}

/// The attempt id as a dashed uuid. PostHog drops a duplicate with the same
/// uuid, event, timestamp and distinct id, so a redelivered batch stays one
/// event each.
fn attempt_uuid(row: &Value) -> Option<String> {
    let id = row.get("attempt_id")?.as_str()?;
    if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    ))
}

fn event(row: &Value) -> Option<(String, String, Value)> {
    let uuid = attempt_uuid(row)?;
    let ts = row.get("ts")?.as_str()?.to_string();
    let mut props = Map::new();
    props.insert("grade".into(), json!(grade(row)));
    props.insert("$process_person_profile".into(), json!(false));
    for (name, field) in [
        ("task", "task_id"),
        ("tier", "tier"),
        ("cohort", "experiment_id"),
        ("variant", "variant"),
        ("failure_type", "failure_type"),
        ("seat", "seat"),
        ("lane", "requested_lane"),
        ("sha", "bank_rev"),
    ] {
        if let Some(value) = row
            .get(field)
            .and_then(Value::as_str)
            .filter(|v| identifier(v))
        {
            props.insert(name.into(), json!(value));
        }
    }
    let doc = json!({
        "event": EVENT_NAME,
        "uuid": uuid,
        "timestamp": ts,
        "distinct_id": DISTINCT_ID,
        "properties": props,
    });
    Some((uuid, ts, doc))
}

/// The events a beat owes, oldest first: inside the lookback window, not yet
/// delivered or dropped, capped at one batch.
fn select(history: &str, state: &ExportState, now_ms: i64) -> Vec<(String, String, Value)> {
    let cutoff = now_ms - LOOKBACK_S * 1000;
    let mut pending: Vec<(i64, (String, String, Value))> = history
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|row| event(&row))
        .filter_map(|e| Some((crate::event_store::parse_rfc3339_ms(&e.1)?, e)))
        .filter(|(ms, e)| *ms >= cutoff && !state.knows(&e.0))
        .collect();
    pending.sort_by_key(|(ms, _)| *ms);
    pending.truncate(MAX_BATCH);
    pending.into_iter().map(|(_, e)| e).collect()
}

fn batch_body(token: &str, pending: &[(String, String, Value)]) -> String {
    let events: Vec<&Value> = pending.iter().map(|(_, _, doc)| doc).collect();
    json!({"api_key": token, "batch": events}).to_string()
}

fn state_path(home: &crate::paths::AgentsHome) -> PathBuf {
    home.otlp_export_dir().join("eval_events_state.json")
}

/// One beat. The same delivery rule as the OTLP arm: a 2xx records the batch,
/// a permanent 4xx drops it, and anything else holds it for the next beat.
fn run_beat(
    cfg: &Config,
    home: &crate::paths::AgentsHome,
    history: &str,
    poster: &mut dyn crate::attention_http::HttpPost,
    now_ms: i64,
) -> otlp_export::BeatReport {
    let path = state_path(home);
    let mut state = otlp_export::load_state_at(&path);
    let pending = select(history, &state, now_ms);
    if pending.is_empty() {
        return otlp_export::BeatReport {
            acted: 0,
            skip: Some("caught_up"),
            detail: vec!["eval events: nothing new in the window".to_string()],
        };
    }
    let count = pending.len();
    let headers = [("Content-Type".to_string(), "application/json".to_string())];
    let result = poster.post(&cfg.url, &headers, &batch_body(&cfg.token, &pending));
    let mut report = otlp_export::BeatReport {
        acted: count as u64,
        skip: None,
        detail: Vec::new(),
    };
    if result.delivered() {
        for (id, ts, _) in pending {
            state.delivered.insert(id, ts);
        }
        state.held_error = None;
        report
            .detail
            .push(format!("eval events: delivered {count} to {}", cfg.host));
    } else if result.dropped() {
        for (id, ts, _) in pending {
            state.dropped.insert(id, ts);
        }
        state.held_error = None;
        report.detail.push(format!(
            "eval events: endpoint rejected {count} permanently (status {})",
            result.status.unwrap_or(0)
        ));
    } else {
        state.held_error = Some(match result.status {
            Some(code) => format!("status {code}"),
            None => "no status".to_string(),
        });
        report.acted = 0;
        report.skip = Some("held");
        report
            .detail
            .push(format!("eval events: held {count} for retry"));
    }
    otlp_export::prune(&mut state, now_ms);
    otlp_export::save_state_at(home, &path, &state);
    report
}

fn read_history() -> String {
    std::fs::read_to_string(crate::evals::history_path()).unwrap_or_default()
}

/// `fno-agents evals events`: print the batch the next beat would post, with
/// the token redacted, and the posture on stderr. It never sends.
pub fn preview_command(_args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        eprintln!("fno-agents evals events: no agents home");
        return 1;
    };
    let state = otlp_export::load_state_at(&state_path(&home));
    let pending = select(&read_history(), &state, otlp_export::now_ms());
    println!("{}", batch_body("<redacted>", &pending));
    let posture = match resolve(&cwd) {
        Resolved::Off {
            forced_off_by: Some(var),
        } => format!("off - forced off by {var}"),
        Resolved::Off {
            forced_off_by: None,
        } => "off - nothing leaves this machine".to_string(),
        Resolved::Refused(reason) => format!("refused: {reason}"),
        Resolved::On(cfg) => format!(
            "on -> {}, held: {}",
            cfg.host,
            state.held_error.as_deref().unwrap_or("none")
        ),
    };
    eprintln!("eval events: {posture}; {} pending", pending.len());
    0
}

pub struct Arm {
    config_cwd: PathBuf,
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Self {
            config_cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn emit_tick_row(home: &crate::paths::AgentsHome, acted: u64, skip: Option<&str>, detail: &str) {
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "eval_events",
        crate::tick_ledger::SCHED_DAEMON,
        acted,
        skip,
        Some(detail),
        INTERVAL_S,
    );
}

pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed().as_secs() < INTERVAL_S) {
            return;
        }
        *last = Some(Instant::now());
    }
    if arm.in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    let flag = Arc::clone(&arm.in_flight);
    let cwd = arm.config_cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        match resolve(&cwd) {
            Resolved::Off { forced_off_by } => {
                let why = forced_off_by
                    .map(|v| format!("forced off by {v}"))
                    .unwrap_or_else(|| "telemetry.eval_events.enabled is false".to_string());
                emit_tick_row(&home, 0, Some("disabled"), &why);
            }
            Resolved::Refused(reason) => {
                emit_tick_row(&home, 0, Some("config"), &format!("refused: {reason}"));
            }
            Resolved::On(cfg) => {
                let mut poster = crate::attention_http::CurlPost;
                let report = run_beat(
                    &cfg,
                    &home,
                    &read_history(),
                    &mut poster,
                    otlp_export::now_ms(),
                );
                emit_tick_row(&home, report.acted, report.skip, &report.detail.join("; "));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention_http::{HttpPost, PostResult};

    const NOW_TS: &str = "2026-11-02T12:00:00Z";

    fn now() -> i64 {
        crate::event_store::parse_rfc3339_ms(NOW_TS).unwrap()
    }

    fn row(id: char, extra: Value) -> String {
        let mut base = json!({
            "ts": "2026-11-02T11:00:00.5Z",
            "attempt_id": id.to_string().repeat(32),
            "task_id": "golden-x",
            "tier": "capability",
            "pass": false,
            "status": "graded",
            "reason": "secret reason words",
            "obs": {"worker": "secret-worker", "kept_transcript": "/Users/me/secret"},
            "experiment_id": "gate-main-1",
            "failure_type": "comment-noise",
            "seat": "worker",
            "bank_rev": "abc123",
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        base.to_string()
    }

    struct Scripted {
        status: Option<u16>,
        bodies: Vec<String>,
    }

    impl HttpPost for Scripted {
        fn post(&mut self, _: &str, _: &[(String, String)], body: &str) -> PostResult {
            self.bodies.push(body.to_string());
            PostResult {
                status: self.status,
                body: String::new(),
                error: None,
            }
        }
    }

    /// Only the allowlist leaves. Free text, paths and a worded cohort stay
    /// here, and a plumbing failure never reads as a failed task.
    #[test]
    fn event_carries_only_the_allowlist() {
        let (uuid, _, doc) = event(&serde_json::from_str(&row('a', json!({}))).unwrap()).unwrap();
        assert_eq!(uuid, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        let text = doc.to_string();
        assert!(!text.contains("secret"), "free text leaked: {text}");
        let props = &doc["properties"];
        assert_eq!(props["task"], "golden-x");
        assert_eq!(props["cohort"], "gate-main-1");
        assert_eq!(props["grade"], "fail");
        assert_eq!(props["failure_type"], "comment-noise");
        assert_eq!(props["seat"], "worker");
        assert_eq!(props["sha"], "abc123");

        let worded = row(
            'b',
            json!({"experiment_id": "my private words", "status": "infrastructure"}),
        );
        let (_, _, doc) = event(&serde_json::from_str(&worded).unwrap()).unwrap();
        assert!(doc["properties"].get("cohort").is_none());
        assert_eq!(doc["properties"]["grade"], "infrastructure");
    }

    /// A 2xx ships once, a hold retries the same rows, and a row outside the
    /// window never ships.
    #[test]
    fn beat_delivers_once_and_holds_on_a_transient_failure() {
        let dir = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(dir.path());
        let cfg = Config {
            url: "https://ph.example/batch/".into(),
            host: "ph.example".into(),
            token: "tok".into(),
        };
        let old = row('c', json!({"ts": "2026-10-30T00:00:00Z"}));
        let history = format!("{}\n{old}\nnot json\n", row('a', json!({})));

        let mut held = Scripted {
            status: Some(503),
            bodies: Vec::new(),
        };
        let report = run_beat(&cfg, &home, &history, &mut held, now());
        assert_eq!(report.skip, Some("held"));
        assert_eq!(report.acted, 0);

        let mut ok = Scripted {
            status: Some(200),
            bodies: Vec::new(),
        };
        assert_eq!(run_beat(&cfg, &home, &history, &mut ok, now()).acted, 1);
        let sent: Value = serde_json::from_str(&ok.bodies[0]).unwrap();
        assert_eq!(sent["api_key"], "tok");
        assert_eq!(sent["batch"].as_array().unwrap().len(), 1);

        let report = run_beat(&cfg, &home, &history, &mut ok, now());
        assert_eq!(report.skip, Some("caught_up"));
        assert_eq!(ok.bodies.len(), 1, "a delivered attempt never resends");
    }

    /// The token env is separate from the OTLP headers, so an armed feed
    /// export never carries eval rows. The kill switches are the shared
    /// `forced_off`, proved in otlp_export's `resolve_gates`.
    #[test]
    fn resolve_stays_off_until_each_gate_opens() {
        static ENV_LOCK: std::sync::LazyLock<&'static std::sync::Mutex<()>> =
            std::sync::LazyLock::new(crate::claims::test_env_lock);
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let keys = [
            "FNO_CONFIG",
            "FNO_NO_CANONICAL_CONFIG",
            "FNO_GLOBAL_SETTINGS_PATH",
            "FNO_POSTHOG_PROJECT_TOKEN",
            "OTEL_SDK_DISABLED",
            "DO_NOT_TRACK",
        ];
        let saved: Vec<_> = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::env::set_var("FNO_CONFIG", &cfg);

        std::fs::write(
            &cfg,
            "[telemetry.otlp]\nenabled = true\nendpoint = \"https://c.example\"\n",
        )
        .unwrap();
        assert!(matches!(
            resolve(dir.path()),
            Resolved::Off {
                forced_off_by: None
            }
        ));

        std::fs::write(&cfg, "[telemetry.eval_events]\nenabled = true\n").unwrap();
        // The token file default resolves against the real HOME, so the gate
        // pins token_file at an absent temp path: no run reads a live secret.
        let token_path = dir.path().join("absent-token");
        std::fs::write(
            &cfg,
            format!(
                "[telemetry.eval_events]\nenabled = true\ntoken_file = \"{}\"\n",
                token_path.display()
            ),
        )
        .unwrap();
        assert!(
            matches!(resolve(dir.path()), Resolved::Refused(ref m) if m.contains("FNO_POSTHOG_PROJECT_TOKEN"))
        );

        std::env::set_var("FNO_POSTHOG_PROJECT_TOKEN", "tok");
        match resolve(dir.path()) {
            Resolved::On(c) => assert_eq!(c.url, "https://us.i.posthog.com/batch/"),
            _ => panic!("every gate open resolves on"),
        }

        for (key, value) in saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    /// The token file arm: a 0600 file feeds the token while the env var is
    /// unset, a loose mode refuses with the chmod, a rotation is picked up on
    /// the next read, and the env var still wins when both hold a token.
    #[test]
    fn token_file_feeds_the_token_and_env_still_wins() {
        static ENV_LOCK: std::sync::LazyLock<&'static std::sync::Mutex<()>> =
            std::sync::LazyLock::new(crate::claims::test_env_lock);
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("FNO_POSTHOG_PROJECT_TOKEN");
        std::env::remove_var("FNO_POSTHOG_PROJECT_TOKEN");
        let saved_cfg = std::env::var_os("FNO_CONFIG");
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        let file = dir.path().join("ph-token");
        std::env::set_var("FNO_CONFIG", &cfg);

        std::fs::write(&file, "  ph-tok \n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(
            &cfg,
            format!(
                "[telemetry.eval_events]\nenabled = true\ntoken_file = \"{}\"\n",
                file.display()
            ),
        )
        .unwrap();
        match resolve(dir.path()) {
            Resolved::On(c) => assert_eq!(c.token, "ph-tok", "surrounding space trims"),
            _ => panic!("a 0600 token file resolves on"),
        }

        // A loose mode gates the read and names the fix.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            matches!(resolve(dir.path()), Resolved::Refused(ref m) if m.contains("chmod 600")),
            "a loose token file refuses"
        );

        // The read is fresh, so a rotated token lands on the next beat.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&file, "ph-rotated\n").unwrap();
        match resolve(dir.path()) {
            Resolved::On(c) => assert_eq!(c.token, "ph-rotated"),
            _ => panic!("a rotated token file is read fresh"),
        }

        // The env var wins when both channels hold a token.
        std::env::set_var("FNO_POSTHOG_PROJECT_TOKEN", "tok");
        match resolve(dir.path()) {
            Resolved::On(c) => assert_eq!(c.token, "tok"),
            _ => panic!("the env var outranks the token file"),
        }

        if let Some(v) = saved {
            std::env::set_var("FNO_POSTHOG_PROJECT_TOKEN", v);
        }
        match saved_cfg {
            Some(v) => std::env::set_var("FNO_CONFIG", v),
            None => std::env::remove_var("FNO_CONFIG"),
        }
    }
}

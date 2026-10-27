//! The quota-aware route policy for one autonomous launch: stay, defer, or
//! cut over. Ports `fno/agents/autonomous_route.py` and the
//! `evaluate_quota_signal` / headroom fold from
//! `fno/adapters/providers/runtime_state.py` over the SHARED persisted
//! runtime-state file (`provider_cap::runtime_state_path`, machine-wide by
//! design: quota is a property of an account at a vendor).
//!
//! Fail-open and opt-in, byte for byte with the Python posture: no provider,
//! quota observation off (the default), or a p0 priority short-circuits to
//! UNKNOWN with both verdicts false, so a fresh install never defers and
//! never reroutes. The one deliberate delta: the HTTP probe-on-stale leg is
//! still Python-owned, so a STALE or missing snapshot answers UNKNOWN
//! (proceed) here instead of refreshing - the safe direction, never a false
//! defer and never a reroute off an unread observation.

use serde_json::Value;
use std::path::Path;

/// `config.accounts.quota` (the quota-aware dispatch block). Both flags
/// default off; `defer_dispatch` implies `observe` at the read sites.
#[derive(Debug, Clone)]
pub struct QuotaConfig {
    pub observe: bool,
    pub defer_dispatch: bool,
    pub defer_threshold_pct: f64,
    pub probe_ttl_seconds: f64,
    pub defer_horizon_minutes: f64,
    pub pick_on_launch: bool,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        QuotaConfig {
            observe: false,
            defer_dispatch: false,
            defer_threshold_pct: 90.0,
            probe_ttl_seconds: 300.0,
            defer_horizon_minutes: 60.0,
            pick_on_launch: false,
        }
    }
}

impl QuotaConfig {
    /// One invalid leaf degrades only that leaf; valid leaves survive, so a
    /// bad global key never discards a healthy local setting. Each leaf takes
    /// pydantic's lax coercion (`QuotaConfig.model_validate`).
    pub fn from_block(block: &toml::Table) -> QuotaConfig {
        use crate::accounts::records::{lax_bool, lax_float, lax_int};
        let mut cfg = QuotaConfig::default();
        let leaf = |key: &str| block.get(key);
        if let Some(v) = leaf("observe").and_then(lax_bool) {
            cfg.observe = v;
        }
        if let Some(v) = leaf("defer_dispatch").and_then(lax_bool) {
            cfg.defer_dispatch = v;
        }
        if let Some(v) = leaf("defer_threshold_pct")
            .and_then(lax_float)
            .filter(|v| (0.0..=100.0).contains(v))
        {
            cfg.defer_threshold_pct = v;
        }
        if let Some(v) = leaf("probe_ttl_seconds")
            .and_then(lax_int)
            .filter(|v| *v >= 1)
        {
            cfg.probe_ttl_seconds = v as f64;
        }
        if let Some(v) = leaf("defer_horizon_minutes")
            .and_then(lax_int)
            .filter(|v| *v >= 0)
        {
            cfg.defer_horizon_minutes = v as f64;
        }
        if let Some(v) = leaf("pick_on_launch").and_then(lax_bool) {
            cfg.pick_on_launch = v;
        }
        cfg
    }
}

/// The quota config: `accounts.quota` merged across the account layers
/// (`loader.load_quota_config`).
pub fn load_quota_config(node_cwd: Option<&str>) -> QuotaConfig {
    let cwd = node_cwd
        .map(Path::new)
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    QuotaConfig::from_block(&crate::accounts::records::quota_block(None, &cwd))
}

/// The headroom verdict: how much room the provider's account has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadroomState {
    Ok,
    Low,
    Exhausted,
    Unknown,
}

impl HeadroomState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HeadroomState::Ok => "ok",
            HeadroomState::Low => "low",
            HeadroomState::Exhausted => "exhausted",
            HeadroomState::Unknown => "unknown",
        }
    }
}

/// What one quota read says about a provider for an autonomous launch.
/// `defer` and `cutover` point in OPPOSITE directions on the same LOW window:
/// a near reset is a reason to wait, a far reset is a reason to leave.
#[derive(Debug, Clone)]
pub struct QuotaSignal {
    pub provider_id: String,
    pub state: HeadroomState,
    pub resets_at: Option<f64>,
    pub defer: bool,
    pub cutover: bool,
    pub reason: String,
}

/// The route one autonomous launch attempt resolved to.
#[derive(Debug, Clone)]
pub struct AutonomousRoute {
    /// stay | defer | cutover | unknown-proceed
    pub action: String,
    pub reason: String,
    pub source_record: String,
    pub record_id: Option<String>,
    pub harness: Option<String>,
    pub retry_at: Option<f64>,
    pub window: Option<String>,
    pub defer_fallback: bool,
}

/// The verdict word the route speaks, from the one headroom verdict.
fn state_of(v: crate::fallback_chain::Verdict) -> HeadroomState {
    use crate::fallback_chain::Verdict;
    match v {
        Verdict::Ok => HeadroomState::Ok,
        Verdict::Low => HeadroomState::Low,
        Verdict::Exhausted => HeadroomState::Exhausted,
        Verdict::Unknown => HeadroomState::Unknown,
    }
}

/// The quota read for one provider: config gate, p0 exemption, snapshot
/// verdict, and the two dispatch verdicts read off the SAME probe.
pub fn evaluate_quota_signal(
    provider_id: &str,
    priority: Option<&str>,
    cutover_low_after_minutes: f64,
    node_cwd: Option<&str>,
    now: f64,
) -> QuotaSignal {
    let signal = |state: HeadroomState, resets_at, defer, cutover, reason: &str| QuotaSignal {
        provider_id: provider_id.to_string(),
        state,
        resets_at,
        defer,
        cutover,
        reason: reason.to_string(),
    };
    if provider_id.is_empty() {
        return signal(HeadroomState::Unknown, None, false, false, "no-provider");
    }
    let quota = load_quota_config(node_cwd);
    if !(quota.observe || quota.defer_dispatch) {
        return signal(
            HeadroomState::Unknown,
            None,
            false,
            false,
            "quota-observation-off",
        );
    }
    if priority
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
        == Some("p0")
    {
        return signal(HeadroomState::Unknown, None, false, false, "p0-exempt");
    }
    // Probe-on-stale is the Python leg: a stale snapshot reads as absent
    // here, and absent is UNKNOWN - which proceeds.
    use crate::accounts::state;
    let path = crate::provider_cap::runtime_state_path(node_cwd.map_or(Path::new("."), Path::new));
    let ttl = quota.probe_ttl_seconds;
    let Some(snap) = state::read_usage(&path, provider_id, ttl, now) else {
        return signal(
            HeadroomState::Unknown,
            None,
            false,
            false,
            "probe-unavailable-native",
        );
    };
    let threshold = quota.defer_threshold_pct;
    let mut hv = state::headroom(&path, provider_id, now, ttl, threshold);
    if hv.verdict == crate::fallback_chain::Verdict::Unknown && !snap.windows.is_empty() {
        // The probe SAW the provider but the read-back did not bind: trust
        // the in-hand observation over the disk that failed to keep it.
        hv = crate::fallback_chain::headroom_at(None, Some(&snap), now, ttl, threshold);
    }
    let (h_state, h_resets) = (state_of(hv.verdict), hv.resets_at);
    if !quota.defer_dispatch {
        // Observation-only: neither verdict fires.
        return signal(h_state, h_resets, false, false, "observed");
    }
    let defer;
    let cutover;
    if h_state == HeadroomState::Exhausted {
        defer = true;
        cutover = true;
    } else if h_state == HeadroomState::Low {
        if let Some(resets_at) = h_resets {
            let horizon = quota.defer_horizon_minutes * 60.0;
            defer = horizon > 0.0 && resets_at <= now + horizon;
            let after = cutover_low_after_minutes * 60.0;
            cutover = after > 0.0 && !defer && resets_at > now + after;
        } else {
            defer = false;
            cutover = false;
        }
    } else {
        defer = false;
        cutover = false;
    }
    signal(h_state, h_resets, defer, cutover, "probed")
}

/// Whether any explicit launch intent pins the route: a provider or model
/// named on the invocation or the node, or a configured dispatch harness.
/// A pin forbids automatic replacement, never the existing defer.
pub fn launch_is_pinned(
    node: &Value,
    provider: Option<&str>,
    model: Option<&str>,
    node_cwd: Option<&str>,
) -> bool {
    if ["provider", "model", "harness"].iter().any(|k| {
        node.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    }) {
        return true;
    }
    if provider.map(str::trim).is_some_and(|s| !s.is_empty())
        || model.map(str::trim).is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    // The stage table with the deprecated dispatch.harness folded beneath it.
    let cfg = super::dispatch_resolve::dispatch_cfg_for(node_cwd, "target");
    !cfg.harness.is_empty()
}

/// The cutover destination: the first combo member whose headroom is not
/// EXHAUSTED, in the operator's configured priority order, excluding the
/// exhausted provider. OK / LOW / UNKNOWN all count as healthy targets.
/// Returns None (the defer floor) when not configured for failover, when
/// there is no combo to walk, or when every member is excluded or exhausted.
pub fn select_destination(
    node_cwd: Option<&str>,
    exhausted_provider: &str,
    now: f64,
) -> Option<(String, String)> {
    let cwd = node_cwd.map(Path::new).unwrap_or(Path::new("."));
    let on_exhaustion = crate::agents_config::config_lookup(cwd, &["dispatch", "on_exhaustion"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "defer".to_string());
    if on_exhaustion != "failover" {
        return None;
    }
    // The active combo by name (`accounts.active_combo`, the settings_combo
    // rung); a bare active account is not a combo, and defer is the floor.
    use crate::accounts::records;
    let root = node_cwd.map(Path::new);
    let combo_name = records::load_active_combo(root, cwd)?;
    let combo = records::load_combos(root, cwd)
        .ok()?
        .into_iter()
        .find(|c| c.name == combo_name)?;
    let accounts = records::load(root, cwd).ok()?;
    for pid in &combo.providers {
        if pid == exhausted_provider {
            continue;
        }
        // UNKNOWN never means exhausted: a member with no signal is a valid
        // failover destination.
        let sig = evaluate_quota_signal(pid, None, 0.0, node_cwd, now);
        if sig.state == HeadroomState::Exhausted {
            continue;
        }
        let harness = accounts.by_id(pid)?.harness.trim();
        // A record with no harness cannot pick a --provider.
        if harness.is_empty() {
            return None;
        }
        return Some((pid.clone(), harness.to_string()));
    }
    None
}

/// Whether launch-time account picking is armed AND has a live account.
/// Best-effort and conservative: anything unreadable or disarmed returns
/// False so the defer stands; only a positive answer suppresses it.
fn healthy_alternate_exists(node_cwd: Option<&str>) -> bool {
    if let Some(cwd) = node_cwd {
        let same = std::env::current_dir()
            .map(|d| d == std::path::PathBuf::from(cwd))
            .unwrap_or(false);
        if !same {
            // Scoped to the process cwd: a cross-project node answers False
            // rather than suppress a defer off another repo's registry.
            return false;
        }
    }
    let quota = load_quota_config(node_cwd);
    if !quota.pick_on_launch {
        return false;
    }
    let cwd = node_cwd.map(Path::new).unwrap_or(Path::new("."));
    let Ok(accounts) = crate::accounts::records::load(None, cwd) else {
        return false;
    };
    let now = now_secs();
    accounts.records.iter().any(|rec| {
        let sig = evaluate_quota_signal(&rec.id, None, 0.0, node_cwd, now);
        sig.state != HeadroomState::Exhausted
    })
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Resolve one autonomous launch's route from one quota read.
pub fn select_autonomous_route(
    provider_id: &str,
    priority: Option<&str>,
    pinned: bool,
    node_cwd: Option<&str>,
    _node_id: Option<&str>,
    now: f64,
) -> AutonomousRoute {
    // `dispatch.cutover_low_after_minutes`; a non-integer or negative value
    // degrades to 0 (off).
    let cutover_low = {
        let cwd = node_cwd.map(Path::new).unwrap_or(Path::new("."));
        crate::agents_config::config_lookup(cwd, &["dispatch", "cutover_low_after_minutes"])
            .and_then(|v| v.as_integer())
            .filter(|v| *v >= 0)
            .unwrap_or(0) as f64
    };
    let sig = evaluate_quota_signal(provider_id, priority, cutover_low, node_cwd, now);
    let window = sig.state.as_str().to_string();
    if sig.cutover && !pinned {
        if let Some((record_id, harness)) = select_destination(node_cwd, &sig.provider_id, now) {
            return AutonomousRoute {
                action: "cutover".to_string(),
                reason: format!("{}-cutover", window.to_lowercase()),
                source_record: sig.provider_id.clone(),
                record_id: Some(record_id),
                harness: Some(harness),
                retry_at: sig.resets_at,
                window: Some(window),
                defer_fallback: sig.defer,
            };
        }
    }
    if sig.defer {
        // Deferring is the floor, not the answer: launch-time account picking
        // is the other reroute, and a pin forbids reroutes, not defers.
        if !pinned && healthy_alternate_exists(node_cwd) {
            return AutonomousRoute {
                action: "stay".to_string(),
                reason: "alternate-account-available".to_string(),
                source_record: sig.provider_id.clone(),
                record_id: None,
                harness: None,
                retry_at: None,
                window: Some(window),
                defer_fallback: false,
            };
        }
        return AutonomousRoute {
            action: "defer".to_string(),
            reason: if pinned && sig.cutover {
                "pinned".to_string()
            } else {
                sig.reason.clone()
            },
            source_record: sig.provider_id.clone(),
            record_id: None,
            harness: None,
            retry_at: sig.resets_at,
            window: Some(window),
            defer_fallback: false,
        };
    }
    if sig.state == HeadroomState::Unknown {
        return AutonomousRoute {
            action: "unknown-proceed".to_string(),
            reason: sig.reason.clone(),
            source_record: sig.provider_id.clone(),
            record_id: None,
            harness: None,
            retry_at: None,
            window: Some(window),
            defer_fallback: false,
        };
    }
    AutonomousRoute {
        action: "stay".to_string(),
        reason: sig.reason.clone(),
        source_record: sig.provider_id.clone(),
        record_id: None,
        harness: None,
        retry_at: None,
        window: Some(window),
        defer_fallback: false,
    }
}

#[cfg(test)]
mod quota_signal_tests {
    //! The observe/defer split, pinned where it now lives. The deleted Python
    //! suite held these six shapes; the verdicts are read off ONE probe here
    //! exactly as there: looking is `observe`, acting is `defer_dispatch`,
    //! and neither may fire the other's behavior.

    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// The env pins are process-global, so the config-gated tests serialize.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const NOW: f64 = 1_789_400_000.0;

    struct EnvPin {
        saved_config: Option<OsString>,
        saved_state: Option<OsString>,
    }

    impl EnvPin {
        fn pin(config: &str, state: Option<&Path>) -> (Self, PathBuf) {
            let dir = tempfile::tempdir().expect("tempdir");
            let config_path = dir.path().join("config.toml");
            fs::write(&config_path, config).expect("write config");
            let saved_config = std::env::var_os("FNO_CONFIG");
            let saved_state = std::env::var_os("FNO_RUNTIME_STATE_PATH");
            std::env::set_var("FNO_CONFIG", &config_path);
            if let Some(path) = state {
                std::env::set_var("FNO_RUNTIME_STATE_PATH", path);
            }
            (
                Self {
                    saved_config,
                    saved_state,
                },
                dir.path().to_path_buf(),
            )
        }

        fn pin_state_only(state: Option<&Path>) -> Self {
            let saved_config = std::env::var_os("FNO_CONFIG");
            let saved_state = std::env::var_os("FNO_RUNTIME_STATE_PATH");
            if let Some(path) = state {
                std::env::set_var("FNO_RUNTIME_STATE_PATH", path);
            }
            Self {
                saved_config,
                saved_state,
            }
        }
    }

    impl Drop for EnvPin {
        fn drop(&mut self) {
            match &self.saved_config {
                Some(v) => std::env::set_var("FNO_CONFIG", v),
                None => std::env::remove_var("FNO_CONFIG"),
            }
            match &self.saved_state {
                Some(v) => std::env::set_var("FNO_RUNTIME_STATE_PATH", v),
                None => std::env::remove_var("FNO_RUNTIME_STATE_PATH"),
            }
        }
    }

    fn quota_config(observe: bool, defer_dispatch: bool) -> String {
        format!(
            "[providers.quota]\nobserve = {observe}\ndefer_dispatch = {defer_dispatch}\n\
             defer_threshold_pct = 90.0\nprobe_ttl_seconds = 300.0\n\
             defer_horizon_minutes = 10.0\n"
        )
    }

    /// One fresh usage row for `p1`, as the runtime-state writer leaves it.
    fn state_file(dir: &Path, used_pct: f64, resets_at: f64) -> PathBuf {
        let path = dir.join("runtime-state.json");
        let body = format!(
            r#"{{"schema_version": 2, "usage": {{"p1": {{"probed_at": {}, "source": "t", "windows": [{{"label": "5h", "used_pct": {}, "resets_at": {}}}]}}}}}}"#,
            NOW - 10.0,
            used_pct,
            resets_at
        );
        fs::write(&path, body).expect("write state");
        path
    }

    #[test]
    fn observation_off_short_circuits_before_any_probe() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvPin::pin(&quota_config(false, false), None);
        let sig = evaluate_quota_signal("p1", None, 0.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Unknown);
        assert!(!sig.defer && !sig.cutover);
        assert_eq!(sig.reason, "quota-observation-off");
    }

    #[test]
    fn p0_is_exempt_when_observation_is_armed() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvPin::pin(&quota_config(true, true), None);
        let sig = evaluate_quota_signal("p1", Some("p0"), 0.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Unknown);
        assert!(!sig.defer && !sig.cutover);
        assert_eq!(sig.reason, "p0-exempt");
    }

    #[test]
    fn observe_only_without_a_snapshot_stays_unknown_and_does_not_act() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvPin::pin(&quota_config(true, false), None);
        let sig = evaluate_quota_signal("p1", None, 0.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Unknown);
        assert!(!sig.defer && !sig.cutover);
        assert_eq!(sig.reason, "probe-unavailable-native");
    }

    #[test]
    fn observe_only_reports_the_verdict_a_fresh_snapshot_gives() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_file(&dir.path(), 32.0, NOW + 3600.0);
        let _env = EnvPin::pin(&quota_config(true, false), Some(&state));
        let sig = evaluate_quota_signal("p1", None, 0.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Ok);
        assert!(!sig.defer && !sig.cutover);
        assert_eq!(sig.reason, "observed");
    }

    #[test]
    fn exhaustion_defers_and_cutovers() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_file(&dir.path(), 100.0, NOW + 3600.0);
        let _env = EnvPin::pin(&quota_config(true, true), Some(&state));
        let sig = evaluate_quota_signal("p1", None, 0.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Exhausted);
        assert!(sig.defer && sig.cutover);
        assert_eq!(sig.reason, "probed");
    }

    #[test]
    fn low_near_reset_defers_without_cutover() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_file(&dir.path(), 95.0, NOW + 60.0);
        let _env = EnvPin::pin(&quota_config(true, true), Some(&state));
        let sig = evaluate_quota_signal("p1", None, 30.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Low);
        assert!(sig.defer);
        assert!(!sig.cutover);
    }

    #[test]
    fn low_far_reset_cutovers_when_the_knob_is_on() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_file(&dir.path(), 95.0, NOW + 7200.0);
        let _env = EnvPin::pin(&quota_config(true, true), Some(&state));
        let sig = evaluate_quota_signal("p1", None, 30.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Low);
        assert!(!sig.defer);
        assert!(sig.cutover);
    }

    #[test]
    fn unreadable_state_stays_unknown_and_eligible() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("absent.json");
        let _env = EnvPin::pin_state_only(Some(&missing));
        let sig = evaluate_quota_signal("p1", None, 0.0, None, NOW);
        assert_eq!(sig.state, HeadroomState::Unknown);
        assert!(!sig.defer && !sig.cutover);
        assert_eq!(sig.reason, "probe-unavailable-native");
    }
}

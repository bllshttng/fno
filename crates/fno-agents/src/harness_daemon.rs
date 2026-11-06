use std::fmt;
use std::path::{Path, PathBuf};

/// The state needed to prove that a provider owns the thread outside the
/// calling pane. Provider adapters keep their native state in `raw`; the
/// shared lifecycle only consumes the identity and endpoint fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonState {
    pub raw: serde_json::Value,
    pub pid: Option<u32>,
    pub process_start_time: Option<u64>,
    pub endpoint: String,
    pub incarnation: String,
}

impl DaemonState {
    pub fn new(
        raw: serde_json::Value,
        pid: u32,
        process_start_time: u64,
        endpoint: impl Into<String>,
        incarnation: impl Into<String>,
    ) -> Result<Self, String> {
        Self::with_optional_process_identity(
            raw,
            Some(pid),
            Some(process_start_time),
            endpoint,
            incarnation,
        )
    }

    pub fn with_optional_process_identity(
        raw: serde_json::Value,
        pid: Option<u32>,
        process_start_time: Option<u64>,
        endpoint: impl Into<String>,
        incarnation: impl Into<String>,
    ) -> Result<Self, String> {
        if pid == Some(0) {
            return Err("missing daemon pid".to_string());
        }
        if process_start_time == Some(0) {
            return Err("missing daemon process start time".to_string());
        }
        if pid.is_none() && process_start_time.is_some() {
            return Err("daemon start time has no pid".to_string());
        }
        let endpoint = endpoint.into();
        if endpoint.is_empty() {
            return Err("missing daemon endpoint".to_string());
        }
        let incarnation = incarnation.into();
        if incarnation.is_empty() {
            return Err("missing daemon incarnation".to_string());
        }
        Ok(Self {
            raw,
            pid,
            process_start_time,
            endpoint,
            incarnation,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonReceipt {
    pub harness: String,
    pub durability: String,
    pub incarnation: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureResult {
    pub state: DaemonState,
    pub receipt: DaemonReceipt,
    pub reused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unreadable,
}

impl Liveness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Unreadable => "unreadable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureError(String);

impl EnsureError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for EnsureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EnsureError {}

/// Harness-specific state, health, and replacement hooks. The orchestration
/// owns the lock, the three-valued liveness decision, start-time-guarded reap,
/// readiness validation, state persistence, and receipt shape.
pub trait HarnessDaemonAdapter {
    fn harness(&self) -> &str;
    fn state_path(&self) -> &Path;
    fn lock_path(&self) -> &Path;
    fn parse_state(&self, raw: &str) -> Result<DaemonState, String>;
    fn is_healthy(&self, state: &DaemonState) -> bool;
    fn boot(&self) -> Result<DaemonState, String>;

    fn healthy_state(&self, state: &DaemonState) -> Option<DaemonState> {
        self.is_healthy(state).then(|| state.clone())
    }

    fn may_replace(&self) -> bool {
        true
    }

    fn may_refresh_unhealthy(&self) -> bool {
        false
    }

    fn unreadable_state_may_boot(&self) -> bool {
        false
    }

    fn serialize_state(&self, state: &DaemonState) -> Result<String, String> {
        serde_json::to_string(&state.raw).map_err(|error| error.to_string())
    }
}

/// Ensure one provider daemon is healthy and return a positive durability
/// receipt. Missing state is a boot opportunity. A readable but unhealthy
/// record may be replaced. Torn, malformed, or identity-incomplete state is
/// unreadable and fails closed.
pub fn ensure_harness_daemon<A: HarnessDaemonAdapter>(
    adapter: &A,
) -> Result<EnsureResult, EnsureError> {
    let _lock = acquire_lock(adapter.lock_path()).map_err(|refused| {
        EnsureError::new(lock_refusal_message(
            adapter.harness(),
            adapter.lock_path(),
            &refused,
        ))
    })?;

    match std::fs::read_to_string(adapter.state_path()) {
        Ok(raw) => match adapter.parse_state(&raw) {
            Ok(state) => {
                if let Some(healthy) = adapter.healthy_state(&state) {
                    if healthy != state {
                        persist_state(adapter, &healthy)?;
                    }
                    return Ok(success(adapter.harness(), healthy, true));
                }
                if adapter.may_replace() {
                    reap_owned_process(&state);
                } else if !adapter.may_refresh_unhealthy() {
                    return Err(EnsureError::new(format!(
                        "harness={} observed={} remedy=restart the provider supervisor explicitly",
                        adapter.harness(),
                        Liveness::Dead.as_str()
                    )));
                }
            }
            Err(_reason) if adapter.unreadable_state_may_boot() => {}
            Err(reason) => {
                return Err(EnsureError::new(format!(
                    "harness={} observed={} state={} reason={reason}",
                    adapter.harness(),
                    Liveness::Unreadable.as_str(),
                    adapter.state_path().display()
                )));
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(EnsureError::new(format!(
                "harness={} observed={} state={} reason=read failed: {error}",
                adapter.harness(),
                Liveness::Unreadable.as_str(),
                adapter.state_path().display()
            )));
        }
    }

    let state = adapter.boot().map_err(|reason| {
        EnsureError::new(format!(
            "harness={} observed={} remedy=boot daemon: {reason}",
            adapter.harness(),
            Liveness::Dead.as_str()
        ))
    })?;
    let Some(state) = adapter.healthy_state(&state) else {
        return Err(EnsureError::new(format!(
            "harness={} observed={} remedy=daemon boot did not pass the health probe",
            adapter.harness(),
            Liveness::Dead.as_str()
        )));
    };
    persist_state(adapter, &state)?;
    Ok(success(adapter.harness(), state, false))
}

fn persist_state<A: HarnessDaemonAdapter>(
    adapter: &A,
    state: &DaemonState,
) -> Result<(), EnsureError> {
    let serialized = adapter.serialize_state(&state).map_err(|reason| {
        EnsureError::new(format!(
            "harness={} observed=unreadable reason=state serialization failed: {reason}",
            adapter.harness()
        ))
    })?;
    write_state(adapter.state_path(), &serialized).map_err(|reason| {
        EnsureError::new(format!(
            "harness={} observed=unreadable reason=state persistence failed: {reason}",
            adapter.harness()
        ))
    })?;
    Ok(())
}

fn success(harness: &str, state: DaemonState, reused: bool) -> EnsureResult {
    EnsureResult {
        receipt: DaemonReceipt {
            harness: harness.to_string(),
            durability: "daemon-owned".to_string(),
            incarnation: state.incarnation.clone(),
            endpoint: state.endpoint.clone(),
        },
        state,
        reused,
    }
}

/// The ensure-refusal body for a lock take that failed. A held lock names its
/// stamped holder; a file that cannot even open keeps the permissions remedy.
fn lock_refusal_message(
    harness: &str,
    lock_path: &Path,
    refused: &crate::chokepoint::Refused,
) -> String {
    if refused.io_error.is_some() {
        return format!(
            "harness={harness} observed=unreadable cannot lock daemon state {}; {refused}; remedy=repair permissions",
            lock_path.display()
        );
    }
    format!(
        "harness={harness} observed=lock-held cannot lock daemon state {}; {refused}; remedy=wait for the holder to release the lock or stop that holder process",
        lock_path.display()
    )
}

/// The blocking take the ensure path uses: up to the chokepoint's wait bound,
/// then a refusal that names the stamped holder.
pub(crate) fn acquire_lock(
    path: &Path,
) -> Result<crate::chokepoint::Held, crate::chokepoint::Refused> {
    crate::chokepoint::take(&crate::chokepoint::HARNESS_DAEMON, path)
}

/// Non-blocking variant: `None` = someone else holds it, try again later.
/// The upgrade transaction takes this so a busy lock reads lock-busy instead
/// of parking the restart command behind another transaction. Held through
/// the chokepoint so the hold is stamped and a waiter's refusal can name it.
pub(crate) fn try_acquire_lock(path: &Path) -> std::io::Result<Option<crate::chokepoint::Held>> {
    crate::chokepoint::try_take(
        &crate::chokepoint::HARNESS_DAEMON,
        path,
        "codex-daemon-upgrade",
    )
}

fn write_state(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    std::fs::write(&temp, contents).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, path).map_err(|error| error.to_string())
}

fn reap_owned_process(state: &DaemonState) {
    let (Some(pid), Some(start)) = (state.pid, state.process_start_time) else {
        return;
    };
    if crate::daemon::pid_is_ours(pid, Some(start)) {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_harness_daemon, lock_refusal_message, try_acquire_lock, DaemonState,
        HarnessDaemonAdapter,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct FakeAdapter {
        state_path: PathBuf,
        lock_path: PathBuf,
        boots: Arc<AtomicUsize>,
    }

    impl HarnessDaemonAdapter for FakeAdapter {
        fn harness(&self) -> &str {
            "fake"
        }
        fn state_path(&self) -> &Path {
            &self.state_path
        }
        fn lock_path(&self) -> &Path {
            &self.lock_path
        }
        fn parse_state(&self, raw: &str) -> Result<DaemonState, String> {
            let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            DaemonState::new(value, 42, 9001, "sock://fake", "42:9001")
        }
        fn is_healthy(&self, _state: &DaemonState) -> bool {
            true
        }
        fn boot(&self) -> Result<DaemonState, String> {
            self.boots.fetch_add(1, Ordering::SeqCst);
            DaemonState::new(
                serde_json::json!({"healthy": true}),
                42,
                9001,
                "sock://fake",
                "42:9001",
            )
        }
    }

    #[test]
    fn ac1_hp_healthy_record_is_reused_with_positive_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let lock_path = dir.path().join("state.lock");
        std::fs::write(&state_path, r#"{"healthy":true}"#).unwrap();
        let boots = Arc::new(AtomicUsize::new(0));
        let adapter = FakeAdapter {
            state_path,
            lock_path,
            boots: boots.clone(),
        };

        let result = ensure_harness_daemon(&adapter).unwrap();

        assert!(result.reused);
        assert_eq!(boots.load(Ordering::SeqCst), 0);
        assert_eq!(result.receipt.harness, "fake");
        assert_eq!(result.receipt.durability, "daemon-owned");
        assert_eq!(result.receipt.incarnation, "42:9001");
        assert_eq!(result.receipt.endpoint, "sock://fake");
    }

    #[test]
    fn ac1_err_unreadable_state_refuses_without_boot() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        let lock_path = dir.path().join("state.lock");
        std::fs::write(&state_path, b"not-json").unwrap();
        let boots = Arc::new(AtomicUsize::new(0));
        let adapter = FakeAdapter {
            state_path,
            lock_path,
            boots: boots.clone(),
        };

        let error = ensure_harness_daemon(&adapter).unwrap_err().to_string();

        assert!(error.contains("observed=unreadable"), "{error}");
        assert_eq!(boots.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_lock_refusal_names_the_holder_not_the_removal_remedy() {
        let path = Path::new("/x/fno-harness-daemon.lock");
        let held = crate::chokepoint::Refused {
            row: "harness-daemon",
            path: PathBuf::from(path),
            waited: std::time::Duration::from_secs(10),
            holder_pid: Some(59801),
            holder_alive: true,
            hold_age_secs: Some(372),
            holder_op: Some("harness-daemon".to_string()),
            waiter_pid: 42,
            io_error: None,
        };
        let message = lock_refusal_message("codex", path, &held);
        assert!(message.contains("holder pid 59801"), "{message}");
        assert!(message.contains("held 372s"), "{message}");
        assert!(
            !message.contains("repair permissions"),
            "a held lock is not a permissions problem: {message}"
        );
        let unopenable = crate::chokepoint::Refused {
            io_error: Some("permission denied".to_string()),
            holder_pid: None,
            holder_alive: false,
            hold_age_secs: None,
            holder_op: None,
            ..held
        };
        let message = lock_refusal_message("codex", path, &unopenable);
        assert!(
            message.contains("repair permissions"),
            "only an unopenable file is a permissions problem: {message}"
        );
    }

    #[test]
    fn the_upgrade_try_lock_stamps_its_holder_so_a_refusal_can_name_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fno-harness-daemon.lock");
        let held = try_acquire_lock(&path)
            .expect("lock file opens")
            .expect("an unheld lock is taken");
        let stamp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            stamp.get("pid"),
            Some(&serde_json::json!(std::process::id())),
            "the holder must be stamped for a waiter to name: {stamp}"
        );
        assert_eq!(
            stamp.get("name"),
            Some(&serde_json::json!("codex-daemon-upgrade")),
            "the stamp names the op that held it: {stamp}"
        );
        drop(held);
        let stamp = std::fs::read_to_string(&path).unwrap();
        assert!(
            stamp.trim().is_empty(),
            "releasing clears the stamp: {stamp:?}"
        );
    }
}

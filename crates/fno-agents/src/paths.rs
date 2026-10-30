//! `~/.fno/agents/` filesystem layout (Wave 3).
//!
//! One source of truth for every path the daemon and workers touch, so the
//! layout is changed in one place and tests can redirect the whole tree to a
//! tempdir via `FNO_AGENTS_HOME`.
//!
//! ```text
//! <home>/
//!   registry.json            registry (schema v4)
//!   registry-snapshots/      rolling pre-write copies kept by update_registry
//!   events.jsonl             operator-facing audit log
//!   route-settings/          --settings floors the spawn arms write (0600,
//!                            content-addressed; read by claude, not by us)
//!   supervisor.sock          daemon's client-facing socket (mode 0600)
//!   <short_id>/
//!     state.json             per-agent state (schema v1)
//!     timeline.jsonl         model-facing transcript
//!     worker.sock            daemon<->worker channel (Outcome B)
//!   .orphaned/<short_id>-<ts>/   archived orphan state dirs
//! ```

use std::path::{Path, PathBuf};

/// Environment variable that redirects the entire agents tree (used by tests
/// and by operators who keep state outside `$HOME`).
pub const HOME_ENV: &str = "FNO_AGENTS_HOME";

/// Whether a pin value declares a root. `FNO_TEST_HERMETIC` has the same
/// three states the Python fence reads: `"1"` declares a sandboxed process
/// root, `"0"` is ambient on purpose, and absent under test is an escaped
/// reader. Neither value names a LANE. Nothing in the tree runs on `"0"` as a
/// lane, because `fno doctor test --ambient dirty` ends at `neutralise`, which
/// stamps `1`. `"0"` is set per test, to state an ambient read on purpose.
///
/// A pure function of its input, so the receipt test states the rule without
/// touching the process environment.
fn root_declared(pin: Option<&str>) -> bool {
    matches!(pin, Some("0") | Some("1"))
}

/// Whether this process declared a root, read from the environment.
pub(crate) fn test_root_declared() -> bool {
    root_declared(std::env::var("FNO_TEST_HERMETIC").ok().as_deref())
}

/// Whether this process CLAIMS a sandbox.
///
/// `"1"` is a claim, not a proof. `fno doctor test rust` makes it true by
/// pinning `HOME` inside a `mktemp` sandbox; a bare `FNO_TEST_HERMETIC=1
/// cargo test` does not, and there [`fence_declared_root`] refuses the
/// resolved root instead. That refusal is the point: a process claiming a
/// sandbox it does not have should hear so, loudly.
fn test_sandbox_claimed() -> bool {
    std::env::var("FNO_TEST_HERMETIC").ok().as_deref() == Some("1")
}

/// Refuse an ambient `$HOME` fallback under test, naming the pin to set.
///
/// `cargo test` never sandboxes `HOME`, so a unit test that resolved a state
/// root through the fallback landed manifests in the operator's real
/// `~/.fno/spaces`.
///
/// `declared` is an argument, not an environment read, so the receipt test
/// constructs the case. Reproducing it by removing the pins process-globally
/// raced every lock-free state-resolving test in the binary into this panic.
pub(crate) fn refuse_undeclared_home_fallback(declared: bool, pin: &str) {
    if !cfg!(test) || declared {
        return;
    }
    panic!(
        "fno-agents resolved a state root from $HOME under test with no \
         declared root. Set {pin} to a directory under std::env::temp_dir(), \
         or FNO_TEST_HERMETIC=0 to declare an ambient run on purpose."
    );
}

/// Pin `FNO_CLAIMS_ROOT` to `dir` for a test whose transitive reads resolve
/// the global claims root: the test-side complement of
/// [`refuse_undeclared_home_fallback`]. Set-if-unset, so a test that pins its
/// own root still wins. Callers mutating env should hold `test_env_lock`.
/// Claim writers create the claims dir on write, so none is made here.
#[cfg(test)]
pub(crate) fn pin_test_claims_root(dir: &std::path::Path) {
    let unset = match std::env::var_os("FNO_CLAIMS_ROOT") {
        None => true,
        Some(v) => v.is_empty(),
    };
    if unset {
        std::env::set_var("FNO_CLAIMS_ROOT", dir);
    }
}

/// Refuse a declared root outside the temp dir when the process root is
/// sandboxed, the way the Python accessor fence refuses one.
///
/// Both raw and canonical forms of the temp dir are compared: macOS reports it
/// as `/var/folders/...` while `canonicalize` yields `/private/var/...`.
pub(crate) fn under_temp_dir(path: &Path) -> bool {
    let tmp = std::env::temp_dir();
    let tmp_forms = [
        std::fs::canonicalize(&tmp).unwrap_or_else(|_| tmp.clone()),
        tmp,
    ];
    let root_forms = [
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        path.to_path_buf(),
    ];
    root_forms
        .iter()
        .any(|r| tmp_forms.iter().any(|t| r.starts_with(t)))
}

fn fence_declared_root(claimed: bool, root: &Path) {
    if !cfg!(test) || !claimed {
        return;
    }
    if under_temp_dir(root) {
        return;
    }
    panic!(
        "fno-agents resolved the state root {} outside the test sandbox while \
         FNO_TEST_HERMETIC=1 declares one. A hermetic run must not touch live \
         state; pin the root under std::env::temp_dir().",
        root.display()
    );
}

/// The fence for a state root resolved OUTSIDE this module: the plans chain
/// resolves its own space root through the full config chain, and the same
/// hermetic rule applies there.
pub(crate) fn fence_resolved_root(root: &Path) {
    fence_declared_root(test_sandbox_claimed(), root);
}

/// Resolved `~/.fno/agents/` root and the paths under it.
#[derive(Debug, Clone)]
pub struct AgentsHome {
    root: PathBuf,
}

impl AgentsHome {
    /// Builds the declared-root case from an already-read value. Shared by
    /// `from_env` and `from_env_opt` so the fence runs in one place and
    /// neither caller re-reads `HOME_ENV` to get it.
    fn from_value(v: Option<std::ffi::OsString>) -> Option<Self> {
        let root = PathBuf::from(v?);
        fence_declared_root(test_sandbox_claimed(), &root);
        Some(AgentsHome { root })
    }

    /// Joins the ambient tree onto an already-read `$HOME` value. Split out
    /// of `home_fallback` so the path-joining is testable without an env
    /// read.
    fn home_fallback_from(home: Option<std::ffi::OsString>) -> Self {
        let base = home
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let root = base.join(".fno").join("agents");
        fence_declared_root(test_sandbox_claimed(), &root);
        AgentsHome { root }
    }

    /// The ambient `$HOME/.fno/agents` fallback. Never reads `HOME_ENV` -
    /// callers already know it was absent.
    fn home_fallback() -> Self {
        Self::home_fallback_from(std::env::var_os("HOME"))
    }

    /// Resolve from the environment: `FNO_AGENTS_HOME` if set, else
    /// `$HOME/.fno/agents`. Falls back to `./.fno/agents` if `$HOME`
    /// is somehow unset (CI containers), so the daemon never panics on a missing
    /// home and degrades to a relative tree.
    pub fn from_env() -> Self {
        if let Some(home) = Self::from_value(std::env::var_os(HOME_ENV)) {
            return home;
        }
        refuse_undeclared_home_fallback(test_root_declared(), HOME_ENV);
        Self::home_fallback()
    }

    /// The resolved root, or `None` when a test process declared none.
    ///
    /// For callers that only COMPARE a path against the shared root. Under
    /// test with nothing declared there IS no shared root to compare against,
    /// and inventing one out of ambient `$HOME` is the read this epic bars. A
    /// caller that actually resolves state still refuses through
    /// [`AgentsHome::from_env`].
    ///
    /// `None` covers the UNDECLARED case only. A process that declared a
    /// sandbox it does not have still panics through [`fence_declared_root`];
    /// that refusal is the point, not a hole in this degrade.
    ///
    /// Reads `HOME_ENV` once and reuses that value. Calling `from_env` in
    /// the declared-ambient branch would read it again, and a sibling
    /// setting its own temp home in that gap would leak through silently.
    pub fn from_env_opt() -> Option<Self> {
        let declared = std::env::var_os(HOME_ENV);
        if declared.is_some() {
            return Self::from_value(declared);
        }
        if cfg!(test) && !test_root_declared() {
            return None;
        }
        refuse_undeclared_home_fallback(test_root_declared(), HOME_ENV);
        Some(Self::home_fallback())
    }

    /// Construct rooted at an explicit directory (tests).
    pub fn at(root: impl Into<PathBuf>) -> Self {
        AgentsHome { root: root.into() }
    }

    /// The ambient shared root built from an explicit `$HOME` value: the pure
    /// form of [`Self::ambient_shared_root`], for a caller that already holds
    /// the home and must not re-read the env (the registry write guard takes
    /// the home as an argument so its decision stays testable).
    pub(crate) fn ambient_from(home: &std::ffi::OsStr) -> Self {
        AgentsHome {
            root: Path::new(home).join(".fno").join("agents"),
        }
    }

    /// The shared `registry.json` path, resolved for COMPARISON only.
    ///
    /// [`AgentsHome::from_env_opt`] answers `None` when a test declared no
    /// root, and a caller that only compares a target against the shared file
    /// then has nothing to compare. That degrade reads as "this is not the
    /// shared file", which turns a corruption guard OFF instead of narrowing
    /// it: losing the pin must never widen what a fence permits. This resolves
    /// the same path [`AgentsHome::from_env`] would and skips the fences,
    /// because nothing is written through it.
    ///
    /// The one resolver every surface reads. Three used to disagree here: this
    /// answered the ambient `$HOME/.fno` under a configured `state_dir`, the
    /// claim leaf answered the configured root, and the worktree status read
    /// answered a third ladder - so the mail gate and the claims read saw
    /// different stores on the same machine. The ladder is the claim leaf's
    /// (Python `paths.agents_registry_path`): a declared `FNO_AGENTS_HOME`
    /// wins, else the configured state root (`FNO_STATE_DIR`, the config
    /// `state_dir`, `$HOME/.fno`), else the ambient root when even `HOME` is
    /// gone.
    pub fn shared_registry_json() -> PathBuf {
        if let Some(v) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return PathBuf::from(v).join("registry.json");
        }
        let root = std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::agents_config::state_dir(&cwd));
        match root {
            Some(root) => root.join("agents").join("registry.json"),
            None => Self::ambient_shared_root().join("registry.json"),
        }
    }

    /// The ambient `$HOME/.fno/agents` - the one root every real session
    /// shares. Deliberately NOT [`HOME_ENV`]-aware: `HOME_ENV` names a
    /// declared root, and the question [`Self::is_sandbox`] answers is
    /// whether the declared root IS this ambient one. [`HOME_ENV`]-aware
    /// callers want [`Self::shared_registry_json`].
    fn ambient_shared_root() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".fno")
            .join("agents")
    }

    /// Whether this home is a throwaway one (a test tempdir, a probe scratch
    /// dir) rather than the shared `$HOME/.fno/agents` every real session
    /// uses.
    ///
    /// A throwaway home gets a daemon on purpose: tests and repro scripts ask
    /// for one. What it must not get is fleet work, because every fleet arm
    /// resolves its targets from the real cwd and the real graph, not from
    /// this home. Compared canonicalized, so a symlinked `$HOME` is not a
    /// sandbox.
    pub fn is_sandbox(&self) -> bool {
        !same_path(&self.root, &Self::ambient_shared_root())
    }

    /// The agents root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create the root directory with mode 0700 (owner-only). The daemon
    /// fstat-verifies this separately; this just gets it created.
    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        set_dir_mode_0700(&self.root)
    }

    pub fn registry_json(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    /// The capped spool of session-start reports a harness hook wrote while
    /// the daemon was down; the next successful `session-report` send drains
    /// it. Lives under the agents home so a test-declared root fences it.
    pub fn agent_hooks_spool(&self) -> PathBuf {
        self.root.join("agent-hooks").join("spool.jsonl")
    }

    /// The team name store (`team_names.json`), beside `registry.json`.
    /// The mux reads this file as a contract - see [`crate::team_names`].
    pub fn team_names_json(&self) -> PathBuf {
        self.root.join("team_names.json")
    }

    /// Machine-wide fleet incident breaker (`fleet-stop.json`), next
    /// to `registry.json`. Written by `fleet-incident stop|clear`, read by
    /// every admission gate before its bypass branches; see
    /// [`crate::fleet_incident`].
    pub fn fleet_stop_json(&self) -> PathBuf {
        self.root.join("fleet-stop.json")
    }

    /// Durable incident-fix claims (`fleet-claims.d/`), one file per
    /// incident, next to the breaker. See [`crate::incident_claim`].
    pub fn fleet_claims_dir(&self) -> PathBuf {
        self.root.join("fleet-claims.d")
    }

    /// The running tests a `tests` hold ended, as legacy `(pid, birth)` pairs
    /// an OLDER build paused and its lift still resumes; new holds record
    /// nothing here. See [`crate::test_hold`].
    pub fn test_pause_json(&self) -> PathBuf {
        self.root.join("test-pause.json")
    }

    /// Durable roster-progress sidecar (SECOND HALF): per-row git
    /// evidence (last commit sha/age, branch-ahead, PR number) keyed by row
    /// name, refreshed by the reconcile sweep alongside `registry.json`. A
    /// sidecar, not new `RegistryEntry` fields, deliberately: ~20 existing
    /// literal constructions of that struct across the crate would each need
    /// a matching field, for data no reader needs schema-version protection
    /// on (a stale reader simply sees no progress row, same as today). Keyed
    /// by `name` so a future reader (t-x9de7's `agents_view.rs` render pass)
    /// joins it the same way the registry itself is joined.
    pub fn roster_progress_json(&self) -> PathBuf {
        self.root.join("roster-progress.json")
    }

    /// Per-scope refusal-rate trend baseline (`refusal-trend/`), beside
    /// `registry.json`. One file per scope, so concurrent beats of different
    /// teams share no document. The `lead_checkin` journal is the diff
    /// corpus, not the trend baseline: a beat that prints but does not
    /// journal must still advance the baseline, or a falling series reads
    /// RISING against the same stale pair on every beat. See
    /// [`crate::refusal_trend`].
    pub fn refusal_trend_dir(&self) -> PathBuf {
        self.root.join("refusal-trend")
    }

    pub fn events_jsonl(&self) -> PathBuf {
        self.root.join("events.jsonl")
    }

    /// The transcripts journal: one `transcript_record` envelope per raw
    /// transcript line a session's turn hook pushed over the daemon socket
    /// (the sandboxed-session lane; the file fallback stays for sessions
    /// whose transcript file is readable). High-volume, so it stays out of
    /// `events.jsonl` (the operator audit log).
    pub fn transcripts_journal(&self) -> PathBuf {
        self.root.join("transcripts.jsonl")
    }

    /// The last footprint probe answer (`footprint-cache.json`). Any real
    /// probe run writes it; readers that can serve a recent answer read it
    /// instead of re-spawning the probe child. See [`crate::spawn_gate`].
    pub fn footprint_cache_json(&self) -> PathBuf {
        self.root.join("footprint-cache.json")
    }

    /// Local OTel ingest tree: `port` (the receiver's bound port) and
    /// `otel.db` (one `api_requests` row per API request, harness-neutral).
    /// Owned by the daemon's otel_ingest arm.
    pub fn otel_dir(&self) -> PathBuf {
        self.root.join("otel")
    }

    /// The OTLP exporter's own state: `state.json` (delivery memory) and
    /// `receipts.jsonl` (one line per post attempt).
    pub fn otlp_export_dir(&self) -> PathBuf {
        self.root.join("otlp_export")
    }

    /// Directory of terminal-stop markers. `finalize` drops one file
    /// per fire-and-forget `claude --bg` session whose loop reached a terminal
    /// decision; the daemon sweep consumes it to `claude stop` the parked
    /// worker. Filename = the claude session uuid; content = the reason.
    pub fn terminal_stop_dir(&self) -> PathBuf {
        self.root.join("terminal-stop")
    }

    /// Directory of terminal-run receipts (one file per released session;
    /// the spawn gate's lead_share count skips rows they name).
    /// `terminal_run.rs` owns the format.
    pub fn terminal_runs_dir(&self) -> PathBuf {
        self.root.join("terminal-runs")
    }

    /// Directory of per-session keep marks (one JSON file per harness
    /// session id, written temp-plus-rename). The user sets and clears the
    /// mark with `fno agents reap --keep` / `--unkeep`; every reaper reads
    /// it as a named keep reason. `keep_mark.rs` owns the format.
    pub fn keep_dir(&self) -> PathBuf {
        self.root.join("keep")
    }

    /// Directory of per-session nudge-ladder state (one JSON file per open-PR
    /// session: attempts, last_nudge_at, escalated). Beside the terminal-stop
    /// markers; the daemon's retire arm owns it.
    pub fn pr_nudge_dir(&self) -> PathBuf {
        self.root.join("pr-nudge")
    }

    /// Directory of per-session burn-ladder state (one JSON file per open-do
    /// session: last sample, attempts, escalated). Beside the nudge state;
    /// the daemon's burn arm owns it.
    pub fn burn_watch_dir(&self) -> PathBuf {
        self.root.join("burn-watch")
    }

    /// Operator override dir for detection manifests: a readable
    /// `<provider>.toml` here beats the bundled copy
    /// (`crate::manifest::load_manifest` resolution chain).
    pub fn manifests_dir(&self) -> PathBuf {
        self.root.join("manifests")
    }

    pub fn supervisor_sock(&self) -> PathBuf {
        self.root.join("supervisor.sock")
    }

    /// The directory a devcontainer box mounts to reach the daemon: it holds
    /// a hard link to `supervisor_sock()` that the daemon re-creates at
    /// every bind, and nothing else from the home.
    pub fn sandbox_sock_dir(&self) -> PathBuf {
        self.root.join("sandbox-sock")
    }

    /// Sidecar advisory-lock file for the supervisor-socket singleton guard
    ///. Held exclusively, for the daemon's whole process lifetime, by
    /// whichever process is entitled to bind `supervisor_sock()`. Since
    /// the holder also writes `<pid> <pid_start_time>` into the content at
    /// bind time; see [`supervisor_lock_holder`].
    pub fn supervisor_lock(&self) -> PathBuf {
        self.root.join("supervisor.sock.lock")
    }

    /// Per-agent directory.
    pub fn agent_dir(&self, short_id: &str) -> PathBuf {
        self.root.join(short_id)
    }

    pub fn state_json(&self, short_id: &str) -> PathBuf {
        self.agent_dir(short_id).join("state.json")
    }

    pub fn timeline_jsonl(&self, short_id: &str) -> PathBuf {
        self.agent_dir(short_id).join("timeline.jsonl")
    }

    /// Per-agent worker socket (Outcome B): the daemon reconnects here on
    /// restart, and discovers live workers by scanning for these sockets.
    pub fn worker_sock(&self, short_id: &str) -> PathBuf {
        self.agent_dir(short_id).join("worker.sock")
    }

    /// Archive directory for orphan state dirs (recovery step 2).
    pub fn orphaned_dir(&self) -> PathBuf {
        self.root.join(".orphaned")
    }

    /// Destination for archiving a specific orphan, timestamped to avoid
    /// collisions across repeated recoveries.
    pub fn orphan_archive_dest(&self, short_id: &str, ts: &str) -> PathBuf {
        self.orphaned_dir().join(format!("{short_id}-{ts}"))
    }

    /// Scan the root for per-agent directories that contain a `worker.sock`,
    /// returning their `short_id`s. Used by recovery to discover live workers
    /// independent of the registry (a worker can outlive a registry write).
    pub fn scan_worker_sockets(&self) -> Vec<String> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(_) => return out,
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name();
            let name = match name.to_str() {
                Some(n) if !n.starts_with('.') => n.to_string(),
                _ => continue,
            };
            if self.worker_sock(&name).exists() {
                out.push(name);
            }
        }
        out.sort();
        out
    }
}

/// The pid (and start time, when recorded) written into the supervisor
/// lockfile by the daemon that holds it. `None` for an absent, empty, or
/// malformed file -- and a `None` here is NEVER "no holder": the flock on the
/// lockfile is the authority on whether a holder exists; the content only
/// names one. Callers treat `None` as "no force target", not as a free socket.
pub fn supervisor_lock_holder(home: &AgentsHome) -> Option<(u32, Option<u64>)> {
    let content = std::fs::read_to_string(home.supervisor_lock()).ok()?;
    let fields: Vec<&str> = content.split_whitespace().collect();
    // Strict on purpose: this value becomes a SIGKILL target, so anything the
    // writer would not have produced (extra fields, a non-numeric token) is
    // corruption and answers None rather than a best-effort parse.
    match fields.as_slice() {
        [pid] => Some((pid.parse().ok()?, None)),
        [pid, start] => Some((pid.parse().ok()?, Some(start.parse().ok()?))),
        _ => None,
    }
}

/// Set a directory to mode 0700 (rwx for owner only), regardless of umask. The
/// daemon refuses to serve if `supervisor.sock`'s parent is not 0700 (finding
/// #6 Critical); this is the corrective write.
pub fn set_dir_mode_0700(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms)?;
    }
    let _ = dir;
    Ok(())
}

/// Set a file to mode 0600 (rw for owner only), regardless of umask.
pub fn set_file_mode_0600(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    let _ = path;
    Ok(())
}

/// True if `path` is a directory with exactly mode 0700 (low 9 bits).
#[cfg(unix)]
pub fn is_dir_mode_0700(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => (m.permissions().mode() & 0o777) == 0o700,
        _ => false,
    }
}

/// True if `path` is a file/socket with exactly mode 0600 (low 9 bits).
#[cfg(unix)]
pub fn is_file_mode_0600(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => (m.permissions().mode() & 0o777) == 0o600,
        _ => false,
    }
}

/// Resolve the canonical (main checkout) repo root from `cwd`. A main checkout
/// and a linked worktree answer from the `.git` markers alone, with no fork;
/// every other layout asks `git worktree list --porcelain` and takes the first
/// real working tree (the main checkout; git lists it first). This is robust across the layouts the
/// `--git-common-dir` parent gets wrong: a bare repo (no working tree) and a
/// `--separate-git-dir` mis-report both resolve to None here, so callers fall
/// back to the caller cwd -- the safe side (Failure Modes > Boundaries). From a
/// linked worktree this returns the main checkout; from the main checkout it
/// returns itself (so a `--fresh`-style redirect is a no-op there). The
/// `cli/src/fno/paths.py` leg spells it too, so the two
/// layers cannot drift (; review HIGH). Shared by the client `--fresh`
/// dispatch (`bin/client.rs`).
///
/// A THIRD implementation of this question lives in `fno::digest_overlay`, which
/// crate `fno` needs because it does not depend on `fno-agents`. It parses the
/// linked worktree's `.git` file instead of shelling out, so it costs no
/// subprocess on the mux attach path and, unlike the git fallback here, cannot
/// see a repo whose git dir lives outside the checkout. The marker fast path in
/// this file reads the same `.git` file. Change one and check the other.
pub fn canonical_repo_root(cwd: &Path) -> Option<PathBuf> {
    // Production shape: the memo + the git resolver. The resolver is a fn
    // parameter on the memo body so the fork-once contract is assertable
    // hermetically, without PATH games a parallel test could race.
    canonical_repo_root_cached(cwd, &canonical_repo_root_uncached)
}

/// Memoized per process: one hook fire re-asks the same cwd many times
/// (every guard decision re-resolves the events space), and each miss was
/// a `git worktree list` fork. Measured 2026-10-08: 18 forks per
/// PreToolUse fire, 7 per Stop fire, 4 inside every `state path events`
/// child - the per-user fork burst that hits kern.maxprocperuid when a
/// fleet of sessions fires hooks at once. The answer for a cwd is stable
/// within a fire; the short TTL bounds how long a long-lived daemon can
/// serve a stale root after a repo move. Negative answers cache too: a
/// non-repo cwd re-forked git on every ask before. Concurrent misses for
/// one cwd single-flight on an in-flight marker (the daemon resolves
/// spawns for the same repo on parallel `spawn_blocking` tasks), while
/// different cwds never block each other.
fn canonical_repo_root_cached(
    cwd: &Path,
    resolve: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Option<PathBuf> {
    let (state, cv) = canonical_root_cache();
    let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let now = std::time::Instant::now();
        if let Some((hit, at)) = cache.entries.get(cwd) {
            if now.duration_since(*at) < CANONICAL_ROOT_TTL {
                return hit.clone();
            }
        }
        if cache.inflight.contains(cwd) {
            let (woken, _) = cv
                .wait_timeout(cache, std::time::Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner());
            cache = woken;
            continue;
        }
        break;
    }
    cache.inflight.insert(cwd.to_path_buf());
    let marker = InflightMarker(cwd.to_path_buf());
    drop(cache);
    let resolved = resolve(cwd);
    let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if cache.entries.len() >= CANONICAL_ROOT_CAP {
        cache
            .entries
            .retain(|_, (_, at)| now.duration_since(*at) < CANONICAL_ROOT_TTL);
    }
    if cache.entries.len() >= CANONICAL_ROOT_CAP {
        cache.entries.clear();
    }
    cache
        .entries
        .insert(cwd.to_path_buf(), (resolved.clone(), now));
    // The marker's drop re-locks the cache; the publish guard must be gone
    // first or the same thread blocks on a non-reentrant Mutex forever.
    drop(cache);
    drop(marker);
    resolved
}

const CANONICAL_ROOT_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const CANONICAL_ROOT_CAP: usize = 256;

struct RootCache {
    entries: std::collections::HashMap<PathBuf, (Option<PathBuf>, std::time::Instant)>,
    /// Cwds a thread is resolving right now. A same-key miss waits on the
    /// condvar instead of forking its own git; a panic in the resolver still
    /// clears the marker through `InflightMarker`'s drop.
    inflight: std::collections::HashSet<PathBuf>,
}

fn canonical_root_cache() -> &'static (std::sync::Mutex<RootCache>, std::sync::Condvar) {
    use std::sync::OnceLock;
    static CACHE: OnceLock<(std::sync::Mutex<RootCache>, std::sync::Condvar)> = OnceLock::new();
    CACHE.get_or_init(|| {
        (
            std::sync::Mutex::new(RootCache {
                entries: std::collections::HashMap::new(),
                inflight: std::collections::HashSet::new(),
            }),
            std::sync::Condvar::new(),
        )
    })
}

/// Clears this thread's in-flight marker on every exit path, including a
/// resolver panic, so waiters can take over the resolution.
struct InflightMarker(PathBuf);

impl Drop for InflightMarker {
    fn drop(&mut self) {
        let (state, cv) = canonical_root_cache();
        let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
        cache.inflight.remove(&self.0);
        cv.notify_all();
    }
}

#[cfg(test)]
pub(crate) fn canonical_root_cache_reset() {
    let (state, _) = canonical_root_cache();
    let mut cache = state.lock().unwrap_or_else(|e| e.into_inner());
    cache.entries.clear();
    cache.inflight.clear();
}

/// The cache is process-global and cargo runs tests in parallel threads.
/// Two tests that reset it must not overlap: one reset can erase the
/// other's in-flight marker and a waiter then re-resolves.
#[cfg(test)]
static CANONICAL_ROOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the filesystem alone says about the repo around a cwd.
enum RepoProbe {
    /// Something git may read differently (env redirect, malformed marker,
    /// relative or missing cwd): ask git.
    Unsure,
    /// No ancestor carries a `.git`: git would find no work tree either.
    NoRepo,
    /// The nearest ancestor with a valid `.git`, and that marker's shape.
    Found(PathBuf, DotGit),
}

enum DotGit {
    /// A real `.git` directory: a main checkout.
    Dir,
    /// A `.git` file: the git dir it points at (linked worktree or submodule).
    File(PathBuf),
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> Option<u64> {
    None
}

/// True when the environment moves git's own discovery, so the markers alone
/// would answer a different question than `git` does.
fn git_discovery_redirected() -> bool {
    [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some())
}

/// Walk up from `cwd` the way git's discovery does, reading only markers.
/// This is the hook allow path: every guard row resolves the events space, and
/// a fork per resolve stalled each Bash call for seconds once the fleet
/// saturated the per-user process table. Git stops at a filesystem boundary by
/// default; so does this walk, handing the question back to git there.
fn probe_repo(cwd: &Path) -> RepoProbe {
    if !cwd.is_absolute() || !cwd.is_dir() {
        return RepoProbe::Unsure;
    }
    // Real path first: a symlinked cwd must climb its target's ancestors, not
    // the link's, or a repo above the target reads as no repo at all.
    let Ok(cwd) = std::fs::canonicalize(cwd) else {
        return RepoProbe::Unsure;
    };
    let start_dev = device_of(&cwd);
    for dir in cwd.ancestors() {
        if device_of(dir) != start_dev {
            return RepoProbe::Unsure;
        }
        let dot = dir.join(".git");
        let Ok(meta) = std::fs::metadata(&dot) else {
            continue;
        };
        if meta.is_dir() {
            if !dot.join("HEAD").is_file() {
                return RepoProbe::Unsure;
            }
            return RepoProbe::Found(dir.to_path_buf(), DotGit::Dir);
        }
        let target = std::fs::read_to_string(&dot).ok().and_then(|text| {
            let line = text.lines().next()?;
            let target = line.strip_prefix("gitdir:")?.trim();
            (!target.is_empty()).then(|| dir.join(target))
        });
        return match target {
            Some(gitdir) => RepoProbe::Found(dir.to_path_buf(), DotGit::File(gitdir)),
            None => RepoProbe::Unsure,
        };
    }
    RepoProbe::NoRepo
}

/// `Some(answer)` when the markers settle the question without git. Only the
/// unambiguous layouts answer: a main checkout, and a linked worktree whose
/// common dir is `<root>/.git`. A bare repo, a `--separate-git-dir` layout and
/// a submodule return `None` so git's rules stay the authority there.
fn canonical_root_from_markers(cwd: &Path) -> Option<Option<PathBuf>> {
    if git_discovery_redirected() {
        return None;
    }
    canonical_root_of(probe_repo(cwd))
}

fn canonical_root_of(probe: RepoProbe) -> Option<Option<PathBuf>> {
    match probe {
        RepoProbe::Unsure => None,
        RepoProbe::NoRepo => Some(None),
        RepoProbe::Found(top, DotGit::Dir) => {
            Some(Some(std::fs::canonicalize(&top).unwrap_or(top)))
        }
        RepoProbe::Found(_, DotGit::File(gitdir)) => {
            let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
            let common = std::fs::canonicalize(gitdir.join(common.trim())).ok()?;
            if common.file_name()? != ".git" {
                return None;
            }
            let root = common.parent()?;
            root.join(".git").is_dir().then(|| Some(root.to_path_buf()))
        }
    }
}

/// The work tree root containing `cwd`, `None` outside any work tree. Markers
/// first; git only when they cannot settle it.
pub fn worktree_toplevel(cwd: &Path) -> Option<PathBuf> {
    if !git_discovery_redirected() {
        match probe_repo(cwd) {
            RepoProbe::Found(top, _) => return Some(std::fs::canonicalize(&top).unwrap_or(top)),
            RepoProbe::NoRepo => return None,
            RepoProbe::Unsure => {}
        }
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .map(|root| std::fs::canonicalize(&root).unwrap_or(root))
}

fn canonical_repo_root_uncached(cwd: &Path) -> Option<PathBuf> {
    if let Some(answer) = canonical_root_from_markers(cwd) {
        return answer;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(["worktree", "list", "--porcelain"]);
    let out = crate::bounded_cmd::output_with_timeout(cmd, 30)?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8(out.stdout).ok()?;
    // Porcelain records are blank-line separated; each starts with
    // `worktree <path>` followed by attribute lines (`bare`, `HEAD`, ...). The
    // FIRST non-bare record is the main worktree. A real working tree has a `.git`
    // child; a `--separate-git-dir` mis-report (the external git dir listed as the
    // main worktree) does not, so it returns None rather than rooting under an
    // arbitrary git dir -- the same first-non-bare-decides rule as the Python side.
    for record in stdout.split("\n\n") {
        let mut lines = record.lines();
        let first = match lines.next() {
            Some(l) => l,
            None => continue,
        };
        let path_str = match first.strip_prefix("worktree ") {
            Some(p) => p.trim(),
            None => continue,
        };
        // `bare` appears as its own attribute line under the worktree path.
        if lines.any(|l| l.trim() == "bare") {
            continue;
        }
        if path_str.is_empty() {
            return None;
        }
        let candidate = Path::new(path_str);
        if candidate.join(".git").exists() {
            return Some(
                std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf()),
            );
        }
        return None;
    }
    None
}

/// Resolve the current worktree root from any path inside it.
///
/// Unlike [`canonical_repo_root`], this preserves linked-worktree isolation.
/// A non-git path falls back to the caller path so journal construction remains
/// available in tests and degraded environments.
pub fn worktree_repo_root(cwd: &Path) -> PathBuf {
    if cwd.join(".git").exists() {
        return std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    }
    worktree_toplevel(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// `Path.resolve()` for a path that may not exist: canonicalize the deepest
/// existing ancestor, re-append the missing tail.
pub(crate) fn resolve_loose(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        match std::fs::canonicalize(&cur) {
            Ok(existing) => {
                let mut out = existing;
                for comp in missing.iter().rev() {
                    out.push(comp);
                }
                return out;
            }
            Err(_) => match (cur.file_name().map(|f| f.to_os_string()), cur.parent()) {
                (Some(name), Some(parent)) => {
                    missing.push(name);
                    cur = parent.to_path_buf();
                }
                _ => return path.to_path_buf(),
            },
        }
    }
}

/// `$HOME`, or `/` when the environment lost it. Shared by the install and
/// reclaim commands, which both place state under the user's home.
pub(crate) fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

// ---------------------------------------------------------------------------
// Project spaces: the per-repository state root OUTSIDE any checkout
// ---------------------------------------------------------------------------

/// The spaces ROOT (the directory holding `<slug>` space dirs): `FNO_SPACES_DIR`
/// verbatim -- it names the root itself, like Python `spaces_root` -- else the
/// state chain's `spaces` dir: `FNO_STATE_DIR`, the configured `state_dir`,
/// `$HOME/.fno`. Spaces are operator state, so `FNO_AGENTS_HOME` plays no
/// part (the daemon's runtime home is state-dir independent).
fn spaces_root_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_SPACES_DIR").filter(|v| !v.is_empty()) {
        let root = PathBuf::from(v);
        fence_declared_root(test_sandbox_claimed(), &root);
        return root;
    }
    durable_spaces_root()
}

/// [`spaces_root_dir`] with the per-process `FNO_SPACES_DIR` pin ignored: the
/// env var rides one process (a test sandbox, an isolation run), the default
/// state root outlives it. A one-time migration moves the ONLY copy, so its
/// destination must be a root the repo's own processes still resolve.
fn durable_spaces_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // Python spaces_root(): the `config.paths.spaces_dir` override, resolved
    // like every paths override and anchored at the cwd when relative. It is
    // DURABLE: it outlives the per-process pin, so the migration guard
    // validates destinations built under it.
    if let Some(raw) = crate::paths_cli::cfg_str(&cwd, &["paths", "spaces_dir"]) {
        if let Ok(p) = crate::paths_cli::resolve_override_path(&raw) {
            let root = crate::paths_cli::anchor_at(&cwd, p);
            fence_declared_root(test_sandbox_claimed(), &root);
            return root;
        }
    }
    let state_root = crate::paths_cli::declared_state_dir(&cwd).unwrap_or_else(|| {
        refuse_undeclared_home_fallback(test_root_declared(), "FNO_SPACES_DIR");
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".fno"))
            .unwrap_or_else(|| PathBuf::from(".fno"))
    });
    let root = state_root.join("spaces");
    fence_declared_root(test_sandbox_claimed(), &root);
    root
}

/// The spaces ROOT (the directory holding one space dir per repository):
/// the same resolution `space_dir`'s parent logic uses. Public for the
/// `fno-agents` bin client, whose dead-team sweep walks it.
pub fn spaces_root() -> PathBuf {
    spaces_root_dir()
}

/// The nearest ancestor of `path` that is a checkout root, or None. A pure
/// walk: no env, no cwd, no subprocess, so the answer about `old` cannot be
/// bent by whoever is resolving state in this call.
fn repo_root_of(path: &Path) -> Option<PathBuf> {
    let mut candidate = path.parent()?;
    loop {
        if candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

/// Canonical form with a fallback for paths that do not fully exist yet:
/// canonicalize the deepest existing ancestor and re-append the missing tail,
/// so a symlinked root (/var vs /private/var) compares equal even when the
/// destination's own directory is created only by the move itself.
pub(crate) fn canonical_best_effort(path: &Path) -> PathBuf {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&probe) {
            let mut out = canonical;
            for component in missing.iter().rev() {
                out.push(component);
            }
            return out;
        }
        match (probe.file_name(), probe.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                probe = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The per-repo space key: the full canonical path with `/` swapped for `-`
/// (Claude's project-dir shape, operator ruling 2026-09-04: self-describing in
/// both directions -- read the dir, see the path; no hash to reverse). The
/// input is the CANONICAL root, so every worktree of a repo mints one slug.
/// Byte-parity with Python `paths.space_slug` -- the two languages must mint
/// the SAME directory, so change one and check the other.
pub fn space_slug(canonical_root: &Path) -> String {
    canonical_root.to_string_lossy().replace('/', "-")
}

/// The space for the repository containing `cwd`, keyed on its CANONICAL root
/// so every worktree of one repo answers the same path. Cross-worktree state
/// (claims, events, leads) resolves here; mirrors Python `paths.space_dir`.
pub fn space_dir(cwd: &Path) -> PathBuf {
    let root = canonical_repo_root(cwd).unwrap_or_else(|| worktree_repo_root(cwd));
    spaces_root_dir().join(space_slug(&root))
}

/// [`space_dir`] for best-effort callers, resolved from a ONE-READ snapshot
/// of the pins: `None` when no state root is declared. A hermetic test
/// process must not resolve an ambient `$HOME` (the guard refuses exactly
/// that), and the snapshot keeps a parallel test's pin cleanup from flipping
/// the resolution mid-read. The stop/rm claims release skips on `None`
/// instead of failing the stop.
pub fn space_dir_opt(cwd: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FNO_SPACES_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        return space_dir_from_root(dir, cwd);
    }
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // The configured spaces override answers before the state chain, so the
    // best-effort callers read the same space the strict resolver answers.
    if let Some(raw) = crate::paths_cli::cfg_str(&base, &["paths", "spaces_dir"]) {
        if let Ok(p) = crate::paths_cli::resolve_override_path(&raw) {
            return space_dir_from_root(crate::paths_cli::anchor_at(&base, p), cwd);
        }
    }
    let agents_home = std::env::var_os(HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    if let Some(home) = agents_home {
        let root = home
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| home.clone())
            .join("spaces");
        return space_dir_from_root(root, cwd);
    }
    // The configured state chain (carrier, then config state_dir), then the
    // guarded default: the strict resolver's answer, one-read.
    if let Some(root) = crate::paths_cli::declared_state_dir(&base) {
        return space_dir_from_root(root.join("spaces"), cwd);
    }
    // No pin at all: under test that is an undeclared process (skip);
    // in production the durable default is the real answer.
    if cfg!(test) && !test_root_declared() {
        return None;
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".fno").join("spaces"))?;
    space_dir_from_root(home, cwd)
}

/// The slug join every [`space_dir_opt`] arm shares: the space for `cwd`'s
/// repo under one resolved root.
fn space_dir_from_root(root: PathBuf, cwd: &Path) -> Option<PathBuf> {
    let repo_root = canonical_repo_root(cwd).unwrap_or_else(|| worktree_repo_root(cwd));
    Some(root.join(space_slug(&repo_root)))
}

/// The per-worktree slice: `<space>/worktrees/<name>/` from a linked
/// worktree, the space root itself from canonical. Session-keyed state
/// (target-state.md, run-log.jsonl) resolves here; mirrors Python
/// `paths.worktree_space_dir`.
pub fn worktree_space_dir(cwd: &Path) -> PathBuf {
    let root = worktree_repo_root(cwd);
    match canonical_repo_root(cwd) {
        Some(canonical) if canonical != root => space_dir(cwd).join("worktrees").join(
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
        _ => space_dir(cwd),
    }
}

/// [`worktree_space_dir`] for best-effort readers, over [`space_dir_opt`]:
/// `None` when no state root is declared (a hermetic test), the durable
/// answer everywhere else. A read-only gate skips on `None` instead of
/// tripping the undeclared-root guard.
pub fn worktree_space_dir_opt(cwd: &Path) -> Option<PathBuf> {
    let root = worktree_repo_root(cwd);
    match canonical_repo_root(cwd) {
        Some(canonical) if canonical != root => space_dir_opt(cwd).map(|dir| {
            dir.join("worktrees").join(
                root.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        }),
        _ => space_dir_opt(cwd),
    }
}

/// The project event journal on the space. Migrates the legacy
/// `<checkout>/.fno/events.jsonl` once on first resolve so appenders never
/// split the file across the two locations; one contract for every reader
/// and `fno-agents state path events`.
/// The project journal as Python `paths.project_events_json` reads it: the
/// `FNO_EVENTS_PATH` pin a harness sets, else the space journal of
/// `$FNO_REPO_ROOT`, else of the cwd's repository. A caller that already
/// holds its own root uses [`events_path`] instead.
pub fn project_events_path() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_EVENTS_PATH").filter(|v| !v.is_empty()) {
        return PathBuf::from(pin);
    }
    let anchor = std::env::var_os("FNO_REPO_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    events_path(&anchor)
}

pub fn events_path(cwd: &Path) -> PathBuf {
    let path = space_dir(cwd).join("events.jsonl");
    migrate_from_checkout(
        &worktree_repo_root(cwd).join(".fno").join("events.jsonl"),
        &path,
    );
    path
}

/// One-shot lazy migration of a legacy `<repo>/.fno/<rel>` file onto the
/// space, mirroring Python `paths.migrate_from_checkout`. Moves `old` to
/// `new` when `old` exists, is not a symlink, and `new` does not; leaves a
/// `MOVED-TO` pointer naming the space so a stale reader fails loud. Best
/// effort: any failure leaves the old file in place.
///
/// The destination must be the source repository's own DURABLE space (the
/// default state root - never a per-process `FNO_SPACES_DIR` pin): a move
/// into a sandbox root strands the only copy where the repo's own readers
/// never look again, behind a MOVED-TO pointer they do not follow.
/// A source outside any checkout has no durable space to check against and
/// keeps the old behavior.
pub fn migrate_from_checkout(old: &Path, new: &Path) -> bool {
    if old == new || new.exists() || !old.exists() || old.is_symlink() {
        return false;
    }
    // The state root is not a checkout journal (law d-8ddaba56): a session
    // whose cwd was $HOME outside git resolved <repo>/.fno to the state root,
    // and moving its global journal stranded it in a fake space behind a
    // MOVED-TO pointer at the top level of the state root. Cheap compares
    // only: this runs on every journal resolve, and a config parse here
    // would tax every call for a corner a config override owns.
    if let Some(parent) = old.parent() {
        if parent
            .file_name()
            .map(|n| n == std::ffi::OsStr::new(".fno"))
            .unwrap_or(false)
        {
            let is_state_root = match std::env::var_os("FNO_STATE_DIR") {
                Some(pinned) => same_path(parent, Path::new(&pinned)),
                None => std::env::var_os("HOME")
                    .map(|h| same_path(parent, &PathBuf::from(h).join(".fno")))
                    .unwrap_or(false),
            };
            if is_state_root {
                return false;
            }
        }
    }
    if let Some(repo) = repo_root_of(old) {
        let canonical = canonical_repo_root(&repo).unwrap_or(repo);
        let durable_space = durable_spaces_root().join(space_slug(&canonical));
        // Compare canonical forms: macOS aliases /var to /private/var, so raw
        // spellings of the same space would false-refuse and strand the
        // journal behind a migration that never fires. best-effort because
        // new's own directory is created by the move, not before it.
        let new_space = new
            .parent()
            .map(canonical_best_effort)
            .unwrap_or_else(|| canonical_best_effort(new));
        if !new_space.starts_with(canonical_best_effort(&durable_space)) {
            return false;
        }
    }
    if let Some(parent) = new.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    if std::fs::rename(old, new).is_err() {
        if std::fs::copy(old, new).is_err() {
            return false;
        }
        let _ = std::fs::remove_file(old);
    }
    let marker = old.parent().map(|p| p.join("MOVED-TO"));
    if let Some(marker) = marker {
        if !marker.exists() {
            let _ = std::fs::write(
                &marker,
                format!(
                    "{}\n",
                    new.parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_default()
                        .display()
                ),
            );
        }
    }
    true
}

/// Whether two paths name the same directory, compared canonicalized on
/// both sides so a symlinked parent never breaks the comparison. A path
/// that cannot be canonicalized compares as written.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// One test's declared state root: `FNO_SPACES_DIR` and `FNO_AGENTS_HOME`
/// pinned under `std::env::temp_dir()`, restored on drop.
///
/// This is the Rust half of the three declarations. `cargo test` sandboxes no
/// `HOME`, so a test that resolves a state root without one of these pins
/// reaches the operator's live `~/.fno` - which is how loop_lead unit tests
/// landed lead manifests in it.
///
/// The directory name carries the pid AND a sequence: two tests sharing a
/// pid-only name raced each other's cleanup. The process-wide env lock is held
/// for the guard's lifetime, so a test holding one must not take that lock
/// itself.
#[cfg(test)]
pub(crate) struct DeclaredRoot {
    root: PathBuf,
    _env: Option<std::sync::MutexGuard<'static, ()>>,
    spaces: Option<std::ffi::OsString>,
    agents: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl DeclaredRoot {
    pub(crate) fn declare(tag: &str) -> Self {
        let env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Self::pin(tag, Some(env))
    }

    /// For a test that already holds the env lock. Taking it twice deadlocks.
    pub(crate) fn declare_held(tag: &str) -> Self {
        Self::pin(tag, None)
    }

    fn pin(tag: &str, env: Option<std::sync::MutexGuard<'static, ()>>) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);

        let root = std::env::temp_dir().join(format!(
            "{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).ok();
        let held = Self {
            spaces: std::env::var_os("FNO_SPACES_DIR"),
            agents: std::env::var_os(HOME_ENV),
            root,
            _env: env,
        };
        std::env::set_var("FNO_SPACES_DIR", &held.root);
        std::env::set_var(HOME_ENV, held.root.join("agents"));
        held
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
impl Drop for DeclaredRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
        match &self.spaces {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
        match &self.agents {
            Some(v) => std::env::set_var(HOME_ENV, v),
            None => std::env::remove_var(HOME_ENV),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-paths-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    /// Run git in `dir`, panicking with git's own stderr when it fails.
    ///
    /// Three copies of this lived inside the tests below, each asserting on a
    /// bare bool. A CI failure then read `assertion failed: git(...)` and cost a
    /// whole pass to diagnose, so the stderr is the point of the helper.
    ///
    /// The ambient config files are pinned out because a git child inherits the
    /// process environment at the moment it starts, and sibling tests in this
    /// binary mutate process-global `$HOME`. The fixture repo carries its own
    /// `user.name` / `user.email` locally, so it needs nothing from either file.
    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} in {dir:?} did not run: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} in {dir:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    /// Mirrors the Python `skipif(no git)`: these fixtures need a real git.
    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn layout_paths_are_under_root() {
        let root = tmp("layout");
        let home = AgentsHome::at(&root);
        assert_eq!(home.registry_json(), root.join("registry.json"));
        assert_eq!(home.supervisor_sock(), root.join("supervisor.sock"));
        assert_eq!(home.state_json("wkA"), root.join("wkA/state.json"));
        assert_eq!(home.worker_sock("wkA"), root.join("wkA/worker.sock"));
        assert!(home
            .orphan_archive_dest("wkA", "20260524T000000Z")
            .starts_with(root.join(".orphaned")));
    }

    #[test]
    fn from_env_honors_override() {
        // FNO_AGENTS_HOME is process-global and the unit tests share one
        // process. Unlocked, this set/remove pair landed inside another test's
        // window and sent its resolution at the real ~/.fno/agents.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tmp("env");
        std::env::set_var(HOME_ENV, &root);
        let home = AgentsHome::from_env();
        assert_eq!(home.root(), root.as_path());
        std::env::remove_var(HOME_ENV);
    }

    #[test]
    fn shared_registry_json_runs_one_ladder_for_every_reader() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Declared root wins, verbatim: FNO_AGENTS_HOME names the agents dir.
        let home = tmp("shared-reg-home");
        std::env::set_var(HOME_ENV, &home);
        assert_eq!(
            AgentsHome::shared_registry_json(),
            home.join("registry.json")
        );
        std::env::remove_var(HOME_ENV);
        // FNO_STATE_DIR names a state root; the registry lives in agents/.
        let state = tmp("shared-reg-state");
        std::env::set_var("FNO_STATE_DIR", &state);
        // Empty spelling counts as unset: the empty value falls through the
        // ladder instead of joining onto the empty path (which would have
        // read `registry.json` out of the cwd).
        std::env::set_var(HOME_ENV, "");
        assert_eq!(
            AgentsHome::shared_registry_json(),
            state.join("agents").join("registry.json")
        );
        // And the variable gone entirely reads the same arm: one ladder,
        // not two spellings of unset.
        std::env::remove_var(HOME_ENV);
        assert_eq!(
            AgentsHome::shared_registry_json(),
            state.join("agents").join("registry.json")
        );
        std::env::remove_var("FNO_STATE_DIR");
        // A configured state_dir routes the same way, read through a pinned
        // FNO_CONFIG so the developer's own config cannot leak in.
        let cfg_dir = tmp("shared-reg-cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let state2 = tmp("shared-reg-state2");
        std::fs::write(
            cfg_dir.join("config.toml"),
            format!("state_dir = \"{}\"\n", state2.display()),
        )
        .unwrap();
        std::env::set_var("FNO_CONFIG", cfg_dir.join("config.toml"));
        assert_eq!(
            AgentsHome::shared_registry_json(),
            state2.join("agents").join("registry.json")
        );
        std::env::remove_var("FNO_CONFIG");
        // Nothing set: the ambient $HOME/.fno arm, with HOME pinned so the
        // developer's machine cannot leak in either. The caller's HOME comes
        // back afterwards: other tests read it unlocked.
        let cfg_dir2 = tmp("shared-reg-empty-cfg");
        std::fs::create_dir_all(&cfg_dir2).unwrap();
        std::fs::write(cfg_dir2.join("config.toml"), "").unwrap();
        std::env::set_var("FNO_CONFIG", cfg_dir2.join("config.toml"));
        let prev_home = std::env::var_os("HOME");
        let fake_home = tmp("shared-reg-ambient-home");
        std::env::set_var("HOME", &fake_home);
        assert_eq!(
            AgentsHome::shared_registry_json(),
            fake_home.join(".fno").join("agents").join("registry.json")
        );
        std::env::remove_var("FNO_CONFIG");
        match prev_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    // ── is_sandbox ─────────────────────────────────────────────────

    #[test]
    fn is_sandbox_false_for_the_ambient_shared_home() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tmp("sandbox-shared");
        let agents = root.join(".fno").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let held_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &root);
        assert!(!AgentsHome::at(&agents).is_sandbox());
        match held_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    // ── supervisor_lock_holder ─────────────────────────────────────

    #[test]
    fn supervisor_lock_holder_garbage_is_none_never_a_guess() {
        let root = tmp("holder-garbage");
        let home = AgentsHome::at(&root);
        home.ensure_root().unwrap();
        for garbage in [
            "",
            "garbage\n",
            "-1 5\n",
            "99999999999 5\n",
            "12 13 14 extra\n",
        ] {
            std::fs::write(home.supervisor_lock(), garbage).unwrap();
            assert_eq!(
                supervisor_lock_holder(&home),
                None,
                "content {garbage:?} must answer None (no target), never a parsed guess"
            );
        }
        // Absent file, same answer.
        std::fs::remove_file(home.supervisor_lock()).unwrap();
        assert_eq!(supervisor_lock_holder(&home), None);
        std::fs::remove_dir_all(&root).ok();
    }

    // ── canonical_repo_root ────────────────────────────────────

    #[test]
    fn canonical_repo_root_resolves_main_from_linked_worktree() {
        // The core failure mode: resolution must give the MAIN checkout, not the
        // linked worktree, via git-common-dir (athens/milan-style fixture).
        if !git_available() {
            return;
        }
        let base = tmp("wt");
        let main = base.join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.email", "t@t"]);
        git(&main, &["config", "user.name", "t"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = base.join("wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "feat",
            ],
        );

        let want = std::fs::canonicalize(&main).unwrap();
        // From the linked worktree -> canonical main.
        assert_eq!(canonical_repo_root(&linked).unwrap(), want);
        // From the main checkout itself -> the same root (no-op redirect).
        assert_eq!(canonical_repo_root(&main).unwrap(), want);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn canonical_repo_root_none_for_bare_repo() {
        // A bare repo reports a git dir whose basename is not `.git`; resolution
        // must return None (caller-cwd fallback), never a wrong parent. Same
        // guard protects the --separate-git-dir layout (review HIGH 1).
        if !git_available() {
            return;
        }
        let base = tmp("bare");
        std::fs::create_dir_all(&base).unwrap();
        let bare = base.join("repo.git");
        git(&base, &["init", "--bare", "-q", bare.to_str().unwrap()]);
        assert!(
            canonical_repo_root(&bare).is_none(),
            "a bare repo must resolve to None (safe-side fallback), not a wrong parent"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    fn canonical_root_of_probe(cwd: &Path) -> Option<Option<PathBuf>> {
        canonical_root_of(probe_repo(cwd))
    }

    #[test]
    fn markers_resolve_main_and_linked_worktree_without_spawning_git() {
        // Hand-built layout, so a pass proves the answer came from the
        // markers: no git binary is involved in building or reading it.
        let base = tmp("markers");
        let main = base.join("main");
        let gitdir = main.join(".git/worktrees/wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        let linked = base.join("wt");
        std::fs::create_dir_all(linked.join("sub")).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        std::fs::create_dir_all(main.join("deep")).unwrap();

        let want = std::fs::canonicalize(&main).unwrap();
        assert_eq!(
            canonical_root_of_probe(&linked.join("sub")),
            Some(Some(want.clone()))
        );
        assert_eq!(
            canonical_root_of_probe(&main.join("deep")),
            Some(Some(want.clone()))
        );
        // A symlinked cwd climbs its target's ancestors, not the link's.
        let link = base.join("link");
        std::os::unix::fs::symlink(main.join("deep"), &link).unwrap();
        assert_eq!(canonical_root_of_probe(&link), Some(Some(want)));
        let linked_real = std::fs::canonicalize(&linked).unwrap();
        assert!(
            matches!(probe_repo(&linked.join("sub")), RepoProbe::Found(top, _) if top == linked_real),
            "the walk stops at the linked worktree, not the main checkout"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn canonical_repo_root_memoizes_one_resolve_per_cwd_per_ttl_window() {
        // The fork-storm contract: repeated asks for one cwd resolve once.
        // Negative answers cache the same way, so a non-repo cwd stops
        // re-forking git too. Hermetic by injection; no PATH mutation that a
        // parallel test's git spawn could race.
        let _serial = CANONICAL_ROOT_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        canonical_root_cache_reset();
        let repo = tmp("memo");
        let empty = tmp("memo-empty");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        use std::cell::RefCell;
        let count = RefCell::new(0u32);
        let resolver = |p: &Path| {
            *count.borrow_mut() += 1;
            if p == empty.as_path() {
                None
            } else {
                Some(p.to_path_buf())
            }
        };
        assert_eq!(
            canonical_repo_root_cached(&repo, &resolver),
            Some(repo.clone())
        );
        assert_eq!(*count.borrow(), 1);
        assert_eq!(
            canonical_repo_root_cached(&repo, &resolver),
            Some(repo.clone()),
            "same cwd inside the TTL window must hit the memo"
        );
        assert_eq!(*count.borrow(), 1);
        assert_eq!(canonical_repo_root_cached(&empty, &resolver), None);
        assert_eq!(*count.borrow(), 2);
        assert_eq!(
            canonical_repo_root_cached(&empty, &resolver),
            None,
            "a miss must cache too, or a non-repo cwd re-forks git per ask"
        );
        assert_eq!(*count.borrow(), 2);
        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn space_slug_is_the_path_with_slashes_swapped() {
        let p = Path::new("/tmp/whatever");
        let slug = space_slug(p);
        assert_eq!(slug, "-tmp-whatever");
        assert_eq!(slug, space_slug(p), "same root mints the same slug");
        let other = Path::new("/tmp/other/whatever");
        assert_ne!(slug, space_slug(other), "a different root must not collide");
        // Golden value pinning the cross-language wire format: Python
        // `space_slug` swaps the same separators, and the two languages must
        // mint the SAME directory. Change one and check the other.
        assert_eq!(space_slug(Path::new("/repos/web")), "-repos-web");
    }

    #[test]
    fn migrate_from_checkout_refuses_the_state_root() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = tmp("migrate-root");
        let state_root = base.join(".fno");
        std::fs::create_dir_all(&state_root).unwrap();
        let old = state_root.join("events.jsonl");
        std::fs::write(&old, "global rows").unwrap();
        let new = base.join("spaces").join("x").join("events.jsonl");
        let prior = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", &state_root);
        let refused = migrate_from_checkout(&old, &new);
        match prior {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
        assert!(!refused, "the state-root journal never moves into a space");
        assert!(old.exists(), "legacy file untouched");
        assert!(
            !state_root.join("MOVED-TO").exists(),
            "no MOVED-TO pointer at the top level of the state root"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// Saved-env guard: the suite may run under a neutralised environment
    /// whose pins other tests read, so every mutation is restored, not just
    /// removed.
    struct EnvGuard {
        spaces: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        hermetic: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn capture() -> Self {
            Self {
                spaces: std::env::var_os("FNO_SPACES_DIR"),
                home: std::env::var_os(HOME_ENV),
                hermetic: std::env::var_os("FNO_TEST_HERMETIC"),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.spaces {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match &self.home {
                Some(v) => std::env::set_var(HOME_ENV, v),
                None => std::env::remove_var(HOME_ENV),
            }
            match &self.hermetic {
                Some(v) => std::env::set_var("FNO_TEST_HERMETIC", v),
                None => std::env::remove_var("FNO_TEST_HERMETIC"),
            }
        }
    }

    /// The refusal itself: with nothing declared, resolving a state root must
    /// name the pin to set rather than reach the operator's real `~/.fno`.
    /// A bare `cargo test` sandboxes no HOME, so this is the only fence there.
    ///
    /// The fence is handed its declaration rather than reached through the
    /// process environment. Removing the three pins process-globally to
    /// reproduce the case raced every lock-free state-resolving test in the
    /// binary into this same panic: `finalize.rs` through `run_log_path` and
    /// `events_path`, `claude_adopt.rs`, `codex_inject.rs`. None of them take
    /// `test_env_lock`, and a mutex only guards the tests that hold it.
    #[test]
    #[should_panic(expected = "FNO_SPACES_DIR")]
    fn an_undeclared_root_refuses_and_names_the_pin() {
        refuse_undeclared_home_fallback(false, "FNO_SPACES_DIR");
    }

    /// A claimed sandbox that is not one is refused, not trusted. Nothing in
    /// tree reaches this today, so without the test the guard's green would be
    /// an absence rather than a receipt.
    #[test]
    #[should_panic(expected = "outside the test sandbox")]
    fn a_claimed_sandbox_that_is_not_one_is_refused() {
        fence_declared_root(
            true,
            Path::new("/nonexistent-outside-any-tmpdir/.fno/agents"),
        );
    }

    /// Mirror of the Python row "durable-config-root-migrates": with no env
    /// pin, the default state root IS the durable home and a genuine legacy
    /// checkout journal still moves once, behind a MOVED-TO marker.
    #[test]
    fn migrate_durable_root_still_moves_checkout_journal() {
        if !git_available() {
            return;
        }
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::capture();
        let agents_home = tmp("durable-home");
        std::env::set_var(HOME_ENV, &agents_home);
        std::env::remove_var("FNO_SPACES_DIR");
        let base = tmp("durable");
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(".fno")).unwrap();
        let old = repo.join(".fno").join("events.jsonl");
        std::fs::write(&old, "rows").unwrap();
        git(&repo, &["init", "-q"]);
        let slug = space_slug(&std::fs::canonicalize(&repo).unwrap());
        let expected = agents_home
            .parent()
            .unwrap()
            .join("spaces")
            .join(slug)
            .join("events.jsonl");

        let resolved = events_path(&repo);

        assert_eq!(resolved, expected);
        assert_eq!(std::fs::read_to_string(&expected).unwrap(), "rows");
        assert!(!old.exists(), "the legacy file must move exactly once");
        assert!(repo.join(".fno").join("MOVED-TO").exists());
        std::fs::remove_dir_all(&base).ok();
        std::fs::remove_dir_all(&agents_home).ok();
    }
}

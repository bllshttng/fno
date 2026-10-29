//! The pane spine: the harness-neutral half of the Python pane back half
//! (`dispatch_spawn_bounded_pane` + `dispatch_spawn_pane`,
//! `cli/src/fno/agents/mux_spawn.py`), ported so every pane spawn is answered
//! here - one arm per harness, no client bounce, no Python round trip.
//!
//! Split shape: this module is PR 1 of the pane-port split - the spine plus
//! its goldens behind a no-op arm table. PR 2 seated the codex arm in a
//! `codex` submodule; the following row seated the claude arm (`claude`):
//! argv, session pin, billing guard, monitor/route/happy legs. The client
//! bounce is deleted with the claude arm, and the routing seam no longer
//! carves any explicit-pane shape to Python; a bare spawn's default
//! substrate still resolves in the Python door (thread, with the seatable
//! refusal and the keeper lanes).
//!
//! Ordering is the Python leg's ordering, because the contract it enforces is
//! the contract reviewers will hold this module to: refusals before the
//! placement lease, the lease before the per-agent lock, the lock before
//! `pane run`, and a registry row only after the pane exists - no
//! half-created row on any failure path (AC1-ERR).
//!
//! Known divergence, recorded: the Python leg derives
//! `FNO_MUX_SHELL_INTEGRATION`, `FNO_PROCESS_ADMISSION_MAX` and
//! `FNO_MUX_PANE_GROUP_MAX` from `settings.yaml`, whose loader is
//! Python-only (`_shell_integration`'s ponytail note). This spine passes the
//! ambient values through and synthesizes the Python defaults when absent;
//! a custom config value rides only if the caller exported it. That bridge
//! lands with the Rust settings reader, not here.
pub mod claude;
pub mod codex;
pub(crate) mod env_wrap;
pub mod placement;
pub mod readiness;
pub mod simple;

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use crate::agent_lock::AgentLock;
use crate::claims::{self, now_ms, AcquireOpts, AcquireOutcome};
use crate::claude_ask::py_repr;
use crate::paths::AgentsHome;
use crate::process_probe::process_start_time;
use crate::scrape::fno_bin;
use crate::state::{self, Lineage, MuxRef, RegistryEntry};
use crate::AgentStatus;

/// `DispatchAskError(..., exit_code=2)`: named refusals - a held placement
/// lease, an existing agent name, an arm's own pre-spawn refusal.
pub const EXIT_REFUSAL: i32 = 2;
/// `DispatchAskError(..., exit_code=1)`: the spawn itself failed after the
/// attempt - a failed `pane run`, an unanswered control read with no
/// adoptable pane, a required bind that found nothing.
pub const EXIT_SPAWN_FAILED: i32 = 1;
/// The per-agent lock timeout (`hold_agent_lock`'s default 30s) surfacing
/// through `dispatch.py`'s handler: `DispatchAskError(..., exit_code=11)`.
pub const EXIT_LOCK_TIMEOUT: i32 = 11;
/// `_run_mux`'s FileNotFoundError/OSError exits: the front door could not be
/// The shell's command-not-found code, shared with heal's missing-`gh` exit: one allocation name for the one convention (`spawn_gate.rs:45`).
pub const EXIT_CMD_NOT_FOUND: i32 = 127;
/// `fno mux pane run`'s exit when the command reached the server but the control
/// read was never answered (crates/fno `EXIT_CONTROL_UNANSWERED`). Unlike any
/// other non-zero code this does NOT prove the pane is absent, so the spine
/// reconciles instead of refusing.
const MUX_CONTROL_UNANSWERED: i32 = 20;
/// The bounded lane's placement lease: one pane placement at a time per mux
/// session (`placement:<session>:global`, 60 s TTL), the same key, holder
/// shape and TTL Python's wrapper writes (`dispatch_spawn_bounded_pane`).
const PLACEMENT_LEASE_TTL_MS: i64 = 60_000;
/// Python `hold_agent_lock`'s default timeout (`fno/agents/lock.py`).
const AGENT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
/// Python `_pane_group_max`'s fail-safe default.
const DEFAULT_PANE_GROUP_MAX: &str = "4";

/// The group cap as a number for the tab-fit arithmetic (the transport arg
/// carries the same value as a string).
fn pane_group_max() -> u64 {
    DEFAULT_PANE_GROUP_MAX.parse().unwrap_or(4)
}
/// Python `_process_admission_max`'s fail-safe default.
const DEFAULT_PROCESS_ADMISSION_MAX: &str = "400";

/// Everything the spine knows about one pane spawn, parsed once from the
/// client door's request params. An existing-pane attach never reaches here:
/// the Python seam routes it to Python by its own predicate before exec.
pub struct PaneContext {
    pub provider: String,
    /// The seed text (positional or `--message`). May be empty; the arm
    /// decides whether it rides the argv or a post-bind turn/start.
    pub message: String,
    pub cwd: PathBuf,
    /// Explicit `--server`; absent resolves through `FNO_SERVER` /
    /// `FNO_SESSION` / "main" like `resolve_mux_session` in mux_server.py.
    pub session: Option<String>,
    /// `--bounded-placement`: hold the placement lease and pass `--fit`.
    pub bounded_placement: bool,
    /// Geometry flags. The arms answer explicit geometry themselves (the
    /// goldens pin the split, group-move and attach shapes); they parse here
    /// so the spine builder stays byte-faithful to the Python leg.
    pub squad: Option<String>,
    pub split: Option<String>,
    pub tab: Option<String>,
    pub at: Option<String>,
    /// The bounded lane's stable `id:<n>` tab token.
    pub tab_id: Option<String>,
    /// A `--pane <id>` attach: start the worker inside this already-placed,
    /// confirmed-pristine pane instead of running a new one.
    pub pane: Option<u64>,
    /// The worker's registry name: the codex identity leaf renders from it
    /// (`mux_spawn.py:3506`).
    pub worker_name: String,
    /// A `--node` dispatch target: makes the seed a code payload for the
    /// git-grant gate however the message is spelled (`rust_runtime.py:632`).
    pub node: Option<String>,
    /// Provider passthrough: the fenced spawn tail and `--harness-arg`, which
    /// the client door folds into one list. The arms append it where the
    /// Python leg appends `passthrough` (`mux_spawn.py:970`).
    pub passthrough: Vec<String>,
    /// The `--monitor` value (PR 3661): the one supported monitor, resolved
    /// by the owning arm. Absent means "no monitor requested".
    pub monitor: Option<String>,
    /// The `--route` target (`provider,model` or a declared lane name): the
    /// owning arm resolves it to the routed env the wrapper carries.
    pub route: Option<String>,
    /// The `--role`: with no `--route`, the claude arm resolves its route.
    pub role: Option<String>,
    /// The spawn carries a promoted role (a `--promote <scope>` plan, or a
    /// complete `--role-level`/`--role-scope` pair): the codex pane launches
    /// at the role-aware pane default instead of the bounded one.
    pub promoted: bool,
    /// The resolved account overlay (`accounts::launch::pane_overlay`),
    /// sealed. The wrapper carries it under any route.
    pub account_env: Map<String, Value>,
}

impl PaneContext {
    /// The minimal context: default session resolution, no geometry, no
    /// bounded lease, empty message. Tests mutate the two or three fields a
    /// case needs instead of restating ten.
    pub fn simple(provider: &str, cwd: &str) -> Self {
        PaneContext {
            provider: provider.to_string(),
            message: String::new(),
            cwd: PathBuf::from(cwd),
            session: None,
            bounded_placement: false,
            squad: None,
            split: None,
            tab: None,
            at: None,
            tab_id: None,
            pane: None,
            worker_name: String::new(),
            node: None,
            passthrough: Vec::new(),
            monitor: None,
            route: None,
            role: None,
            promoted: false,
            account_env: Map::new(),
        }
    }

    /// None when the params are not a pane spawn; the door pre-gates the
    /// substrate, so this is an internal-invariant miss, not a lane.
    pub fn from_params(params: &Value) -> Option<Self> {
        let substrate = params
            .get("substrate")
            .and_then(Value::as_str)
            .unwrap_or("pane");
        if substrate != "pane" {
            return None;
        }
        let opt_str = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
        };
        Some(PaneContext {
            provider: params
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            message: params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            cwd: params
                .get("cwd")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            session: opt_str("session"),
            bounded_placement: params
                .get("bounded_placement")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            squad: opt_str("squad"),
            split: opt_str("split"),
            tab: opt_str("tab"),
            at: opt_str("at"),
            tab_id: opt_str("tab_id"),
            pane: params.get("pane").and_then(Value::as_u64),
            worker_name: params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            node: opt_str("node"),
            passthrough: params
                .get("harness_args")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|token| token.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            monitor: params
                .get("monitor")
                .and_then(Value::as_str)
                .map(str::to_string),
            route: params
                .get("route")
                .and_then(Value::as_str)
                .map(str::to_string),
            role: opt_str("role"),
            // A promote plan, or a COMPLETE role pair, names a promoted
            // spawn. A lone half widens nothing: the thread lane's role
            // reader refuses an incomplete role, so the pane's widening
            // reads the same permission boundary.
            promoted: params
                .get("promote")
                .and_then(Value::as_array)
                .is_some_and(|scopes| !scopes.is_empty())
                || (params.get("role_level").is_some() && params.get("role_scope").is_some()),
            account_env: params
                .get("account_env")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        })
    }
}

/// What an arm adds to the spine: the wrapped provider argv and the
/// post-run bind. One impl per harness; the codex arm lands in PR 2 beside
/// this module, the claude arm in row 8.
pub struct ArmPlan {
    /// The provider argv, already fenced-ready: the spine appends it after
    /// the `--` it puts on every `pane run`.
    pub wrapped: Vec<String>,
    /// The seed held OUT of the argv for a post-bind turn/start (codex).
    /// `None` delivers the message inside the argv like every thread spawn.
    pub held_seed: Option<String>,
    /// A `preassigned` binding harness starts with the id it minted, so the
    /// pane is addressable before its row exists: the id rides `--fno-id`.
    pub preassigned_id: Option<String>,
}

/// The post-run binding outcome the arm reports; the spine turns it into the
/// row status and the receipt's `bound`/`unbound_reason` keys.
pub enum ArmBind {
    Bound {
        session_id: String,
    },
    Unbound {
        reason: String,
    },
    /// The pane was confirmed dead before any id bound (the arm's death
    /// oracle saw it absent from a non-empty listing): the row records the
    /// failure the Python leg records, never a still-spawning pane.
    Died {
        reason: String,
    },
    /// The harness binds no session at all (gemini, agy): a receipt with
    /// neither `bound: false` nor an `unbound_reason`.
    NotApplicable,
}

/// A named refusal or failure. `after_pane` marks the failures that happen
/// once the pane exists: the spine reaps first, then prints, so a failed
/// spawn never leaves an unowned pane behind (the Python leg's reap wrappers
/// at mux_spawn.py:3874/:3980).
#[derive(Debug)]
pub struct PaneRefusal {
    pub line: String,
    pub exit_code: i32,
    pub after_pane: Option<PaneFacts>,
}

/// The pane facts every post-run step reads.
#[derive(Debug)]
pub struct PaneFacts {
    pub session: String,
    pub pane_id: u64,
    pub child_pid: Option<u32>,
    /// The spawn clock, stamped immediately before `pane run` (Python stamps
    /// it there for the same reason: a sibling pane starting during the lock
    /// wait must not widen the bind oracle's candidate window).
    pub spawn_started_ms: i64,
}

/// One harness's pane arm.
pub trait PaneArm {
    fn name(&self) -> &'static str;
    /// Pre-spawn: refusals, the wrapped argv, daemon ensure, the loaded-ids
    /// baseline. Runs under the placement lease (when bounded) and before the
    /// per-agent lock's pane run, mirroring the Python ordering.
    fn prepare(&self, ctx: &PaneContext) -> Result<ArmPlan, PaneRefusal>;
    /// Post-run: the bind wait and seed delivery. The spine owns the row
    /// write, the late-bind pass and the receipt this feeds.
    fn bind_and_seed(
        &self,
        ctx: &PaneContext,
        pane: &PaneFacts,
        plan: &ArmPlan,
    ) -> Result<ArmBind, PaneRefusal>;
    /// The spawn-time late-bind pass for a row that outlived its bind window.
    /// Default: no pass (the harness has none); codex implements the bounded
    /// re-probe + compare-and-set stamp.
    fn late_bind(&self, _ctx: &PaneContext, _pane: &PaneFacts) -> Option<String> {
        None
    }
}

/// The arm table: one arm per harness, the codex arm checked first (its
/// daemon bind wait and late bind set it apart), every other provider seated
/// through the simple table. A provider with no arm is the caller's refusal,
/// not a fall-through.
pub fn pane_arm_for(ctx: &PaneContext) -> Option<&'static dyn PaneArm> {
    if codex::CodexArm::owns(ctx) {
        return Some(&codex::CODEX_ARM);
    }
    let arm: &dyn PaneArm = match ctx.provider.as_str() {
        "claude" => &claude::CLAUDE_ARM,
        "opencode" => &simple::OPENCODE_ARM,
        "pi" => &simple::PI_ARM,
        "cursor-agent" => &simple::CURSOR_ARM,
        "grok" => &simple::GROK_ARM,
        "agy" => &simple::AGY_ARM,
        "footnote" => &simple::FOOTNOTE_ARM,
        _ => return None,
    };
    Some(arm)
}

/// The retired pane harnesses refuse in the spine with a named line
/// (AC4-ERR): the Gemini CLI was deprecated 2026-07-27 and no arm seats it.
pub fn pane_refusal_for(provider: &str) -> Option<String> {
    (provider == "gemini").then(|| {
        "gemini has no pane arm: the Gemini CLI was deprecated 2026-07-27 (Google's successor is Antigravity); spawn agy instead"
            .to_string()
    })
}

/// The binary door's pane entry: the spine answers every explicit-pane shape
/// (receipt or refusal printed). The provider default (dispatch_flags
/// `resolve_dispatch_harness`: explicit flag > invoking-harness inference >
/// builtin claude) resolves here, where the deleted client bounce used to
/// re-exec Python to get it. Narrowing, recorded: the front door scrubs the
/// ambient identity markers before the exec, so the inference half can only
/// fire on a DIRECT binary call; a routed explicit-pane spawn without -H
/// resolves to claude. Pass -H when the invoking seat is not claude.
pub fn run_pane_spawn(home: &AgentsHome, params: &Value, name: &str, _args: &[String]) -> i32 {
    let mut params = params.clone();
    let provider = params
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if provider.trim().is_empty() {
        let ident = crate::spawn_context::resolve_self_identity(
            &|key| std::env::var(key).ok(),
            None,
            None,
            home,
        );
        let inferred = ident
            .harness
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "claude".to_string());
        params["provider"] = Value::String(inferred);
    } else if !crate::provider::KNOWN_PROVIDERS.contains(&provider) {
        eprintln!(
            "unknown provider {}; supported: {}",
            crate::claude_ask::py_repr(provider),
            crate::provider::known_providers_csv()
        );
        return EXIT_REFUSAL;
    }
    // The account step, once the harness is known: the overlay rides the
    // wrapper, because the pane shell is the mux server's child.
    let harness = params["provider"].as_str().unwrap_or_default().to_string();
    match crate::accounts::launch::pane_overlay(&params, &harness) {
        Err(code) => return code,
        Ok(None) => {}
        Ok(Some(applied)) => params["account_env"] = Value::Object(applied.env),
    }
    let ctx = match PaneContext::from_params(&params) {
        Some(ctx) => ctx,
        None => {
            eprintln!("run_pane_spawn reached with a non-pane substrate");
            return EXIT_REFUSAL;
        }
    };
    if let Some(line) = pane_refusal_for(&ctx.provider) {
        eprintln!("{line}");
        return EXIT_REFUSAL;
    }
    let arm = match pane_arm_for(&ctx) {
        Some(arm) => arm,
        None => {
            eprintln!(
                "{} has no pane arm",
                crate::claude_ask::py_repr(&ctx.provider)
            );
            return EXIT_REFUSAL;
        }
    };
    spawn_pane(home, &ctx, name, arm)
}

/// The spine's whole lane: lease, prepare, lock, collision, pane run, parse
/// (or reconcile), child pid, bind, row, late-bind, receipt. Every failure
/// path prints its named line and returns the Python leg's exit code.
fn spawn_pane(home: &AgentsHome, ctx: &PaneContext, name: &str, arm: &dyn PaneArm) -> i32 {
    // The group-tab + split/at combination refuses before the lease, byte
    // equal to the Python leg (AC3-ERR).
    if let Some(tab) = ctx.tab.as_deref() {
        let (_selector, group) = placement::resolve_group_tab(tab);
        if group.is_some() && (ctx.split.is_some() || ctx.at.is_some()) {
            return refuse(&PaneRefusal {
                line: placement::group_tab_conflict_line(tab),
                exit_code: EXIT_REFUSAL,
                after_pane: None,
            });
        }
    }
    if let Some(line) = placement::pane_placement_conflict(
        ctx.pane,
        ctx.squad.as_deref(),
        ctx.split.as_deref(),
        ctx.at.as_deref(),
        ctx.tab.as_deref(),
        ctx.tab_id.as_deref(),
        ctx.bounded_placement,
    ) {
        return refuse(&PaneRefusal {
            line,
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    }
    let session = resolve_mux_session(ctx.session.as_deref());
    // The bounded wrapper holds the lease around everything below and drops
    // it after; a held lease refuses before any pane or row exists.
    let _lease = if ctx.bounded_placement {
        match PlacementLease::acquire(&session, None) {
            Ok(lease) => Some(lease),
            Err(holder) => {
                eprintln!("placement lease held by {holder}; no pane spawned");
                return EXIT_REFUSAL;
            }
        }
    } else {
        None
    };

    let plan = match arm.prepare(ctx) {
        Ok(plan) => plan,
        Err(refusal) => return refuse(&refusal),
    };

    // Per-agent lock -> collision check, the Python leg's steps 3-4. The
    // client's own pre-check ran outside any lock, so the authoritative read
    // is this one, under the lock the registry write shares.
    let registry_path = home.registry_json();
    let _agent_lock = match AgentLock::acquire(home, name, AGENT_LOCK_TIMEOUT) {
        Ok(lock) => lock,
        Err(()) => {
            // The Python pane leg lets `AgentLockTimeout` escape to the CLI
            // unhandled; the ask path's handler is the only named shape
            // (`lock timeout for agent {name!r} after {timeout}s`, exit 11),
            // so the spine answers with that instead of a traceback.
            return refuse(&PaneRefusal {
                line: format!("lock timeout for agent {} after 30.0s", py_repr(name)),
                exit_code: EXIT_LOCK_TIMEOUT,
                after_pane: None,
            });
        }
    };
    let registry = match state::load_registry(&registry_path) {
        Ok(registry) => registry,
        Err(exc) => {
            return refuse(&PaneRefusal {
                line: format!("registry read failed: {exc}"),
                exit_code: 12,
                after_pane: None,
            });
        }
    };
    // Exact-name clash, Python's `next((e for e in entries if e.name == name))`
    // - not the alias/session-resolving `find`. A PROVED-DEAD row does not own
    // the name (`_RECLAIMABLE_STATUSES`): the write transaction drops the
    // corpse and appends the replacement, so a dispatcher's retry under the
    // same deterministic worker name recovers instead of deadlocking on `rm`.
    let replacing = match registry.entries.iter().find(|e| e.name == name) {
        Some(clash) if is_reclaimable(clash.status) => true,
        Some(_) => {
            return refuse(&PaneRefusal {
                line: format!(
                    "agent {} already exists; use 'fno agents rm {name}' first or pick another name",
                    py_repr(name)
                ),
                exit_code: EXIT_REFUSAL,
                after_pane: None,
            });
        }
        None => false,
    };

    let spawn_started_ms = now_ms();
    // `_capture_spawn_trigger` POPS the var before the pane-env snapshot
    // (dispatch.py's ordering fix): the trigger stamps THIS row without
    // leaking into the pane shell, where the child would inherit the "I am a
    // machine dispatch" mark.
    let spawn_trigger = capture_spawn_trigger();
    // The attach lane: an existing-pane spawn never runs `pane run`. The
    // target is checked (found, unoccupied, a confirmed pristine idle
    // shell), the worker start text is sent with `--guarded`, and the pane
    // id is the caller's. Checks run on the fresh listing, never a stale
    // context field.
    let attach = match ctx.pane {
        Some(target) => {
            let (rows, detail) = pane_ls_detailed(&session, &FsMux);
            let rows = rows.unwrap_or_default();
            if rows.is_empty() && !detail.is_empty() {
                return refuse(&PaneRefusal {
                    line: format!("pane listing failed: {detail}"),
                    exit_code: EXIT_SPAWN_FAILED,
                    after_pane: None,
                });
            }
            if let Err(refusal) = placement::resolve_existing_pane(&session, target, &rows) {
                return refuse(&refusal);
            }
            Some(target)
        }
        None => None,
    };
    let pane_id = if let Some(target) = attach {
        if let Err(line) =
            placement::start_existing_pane(&session, target, &ctx.cwd, &plan.wrapped, &FsMux)
        {
            return refuse(&PaneRefusal {
                line,
                exit_code: EXIT_SPAWN_FAILED,
                after_pane: None,
            });
        }
        target
    } else {
        let run_args = pane_run_args(ctx, &session, name, &plan);
        let run_env = pane_run_env();
        let proc = match FsMux.run(&run_args, &run_env) {
            Ok(proc) => proc,
            Err(exc) => {
                // `_run_mux`'s FileNotFoundError/OSError arms: the front door
                // never ran, so no pane exists and the line names the binary.
                let (line, code) = spawn_exec_failure_line(&exc);
                return refuse(&PaneRefusal {
                    line,
                    exit_code: code,
                    after_pane: None,
                });
            }
        };
        match parse_pane_run(&proc, json_receipt(ctx), &session) {
            Ok(PaneRunOutcome::Created { pane_id }) => pane_id,
            Ok(PaneRunOutcome::ControlUnanswered) => {
                match reconcile_unanswered_run(
                    &session,
                    &ctx.cwd,
                    spawn_started_ms,
                    &registry,
                    &FsMux,
                ) {
                    Ok(pane_id) => pane_id,
                    Err(refusal) => return refuse(&refusal),
                }
            }
            Err(detail) => {
                return refuse(&PaneRefusal {
                    line: pane_run_failure_line(&session, &detail),
                    exit_code: EXIT_SPAWN_FAILED,
                    after_pane: None,
                });
            }
        }
    };
    // The group-tab move lands between the parse and the bind: cosmetic by
    // the Python leg's own account (the seed already rode in and the worker
    // may be live), so a failure prints its named line and keeps the pane.
    let group = ctx
        .tab
        .as_deref()
        .map(placement::resolve_group_tab)
        .and_then(|t| t.1);
    if let Some(group) = group {
        if let Err(detail) =
            placement::place_pane_in_group_tab(&session, pane_id, &group, pane_group_max(), &FsMux)
        {
            eprintln!(
                "fno agents spawn: pane {pane_id} left in its own tab; grouping into {group:?} failed: {detail}"
            );
        }
    }
    let child_pid = lookup_child_pid(&session, pane_id, &FsMux);
    let pane = PaneFacts {
        session: session.clone(),
        pane_id,
        child_pid,
        spawn_started_ms,
    };

    let bind = match arm.bind_and_seed(ctx, &pane, &plan) {
        Ok(bind) => bind,
        Err(refusal) => return refuse(&refusal),
    };

    if let Err(exc) = write_pane_row(home, name, ctx, &pane, &bind, replacing, &spawn_trigger) {
        return refuse(&PaneRefusal {
            line: format!("registry row write failed: {exc}"),
            exit_code: 12,
            after_pane: Some(pane),
        });
    }

    // The spawn-time late-bind pass: one more chance for a bind that landed
    // just past the window, before the receipt returns (Python scopes it to
    // `binding-window-expired` exactly this way, mux_spawn.py:4636).
    if let ArmBind::Unbound { .. } = bind {
        if let Some(session_id) = arm.late_bind(ctx, &pane) {
            let pid_start_time = pane.child_pid.and_then(process_start_time);
            let _ = stamp_late_bind(
                home,
                name,
                &session_id,
                pane.child_pid,
                pid_start_time,
                &pane.session,
                pane.pane_id,
            );
        }
    }

    let receipt = PaneReceipt::new(name, ctx, &pane, &bind);
    println!("{}", receipt.to_json());
    0
}

fn refuse(refusal: &PaneRefusal) -> i32 {
    if let Some(pane) = &refusal.after_pane {
        let (reaped, detail) = reap_spawned_pane(&pane.session, pane.pane_id, &FsMux);
        if reaped {
            eprintln!(
                "{}; pane {} reaped, no registry row written",
                refusal.line, pane.pane_id
            );
        } else {
            eprintln!(
                "{}; pane {} may still exist in session {} because exact cleanup failed: {detail}",
                refusal.line,
                pane.pane_id,
                py_repr(&pane.session)
            );
        }
    } else {
        eprintln!("{}", refusal.line);
    }
    refusal.exit_code
}

/// `fno agents spawn` resolution order (`resolve_mux_session`,
/// mux_server.py): flag > `FNO_SERVER` > `FNO_SESSION` (with the deprecation
/// line) > "main".
pub fn resolve_mux_session(explicit: Option<&str>) -> String {
    resolve_mux_session_from(
        explicit,
        &std::env::var("FNO_SERVER").unwrap_or_default(),
        &std::env::var("FNO_SESSION").unwrap_or_default(),
    )
}

pub fn resolve_mux_session_from(
    explicit: Option<&str>,
    fno_server: &str,
    fno_session: &str,
) -> String {
    // Python truthiness on every leg: a non-empty value wins as-is (the flag
    // verbatim, the env values untrimmed), decided by `strip()` only.
    if let Some(session) = explicit.filter(|s| !s.is_empty()) {
        return session.to_string();
    }
    if !fno_server.trim().is_empty() {
        return fno_server.to_string();
    }
    if !fno_session.trim().is_empty() {
        eprintln!(
            "warning: FNO_SESSION is deprecated; use FNO_SERVER instead. \
             The alias will be removed in a future release."
        );
        return fno_session.to_string();
    }
    "main".to_string()
}

/// The bounded lane's placement lease. The holder names the process and a
/// random half like Python's `mux-placement:<pid>:<uuid4>`; the guard's Drop
/// releases strictly and loudly (a swallow here is a custody loss the next
/// holder pays for).
#[derive(Debug)]
pub struct PlacementLease {
    key: String,
    holder: String,
    root: Option<PathBuf>,
}

impl PlacementLease {
    /// Err carries the live holder's name for the golden refusal line
    /// (`placement lease held by <holder>; no pane spawned`); a store error
    /// reports "unknown" the way Python's `getattr(exc, "holder", "unknown")`
    /// does - fail closed either way.
    ///
    /// The root is the GLOBAL claims root (`global_claims_root()`), Python's
    /// `claims_root or global_claims_root()`: `placement:` is not a global-id
    /// prefix in the Rust resolver, so a root-less acquire would land in a
    /// cwd-local store and two spawns in different directories would never
    /// contend the same lease - the lock would guard nothing it exists to.
    pub fn acquire(session: &str, root: Option<&Path>) -> Result<Self, String> {
        let resolved = root
            .map(Path::to_path_buf)
            .or_else(crate::claims_root::global_claims_root);
        let holder = format!("mux-placement:{}:{}", std::process::id(), random_hex16());
        let key = format!("placement:{session}:global");
        let outcome = claims::acquire(
            &key,
            &holder,
            AcquireOpts {
                pid: Some(std::process::id()),
                ttl_ms: Some(PLACEMENT_LEASE_TTL_MS),
                reason: Some("bounded mux pane placement".to_string()),
                root: resolved.clone(),
                ..Default::default()
            },
        );
        match outcome {
            AcquireOutcome::Acquired(_) => Ok(PlacementLease {
                key,
                holder,
                root: resolved,
            }),
            AcquireOutcome::HeldByOther { holder, .. } => Err(holder),
            AcquireOutcome::Error(_) => Err("unknown".to_string()),
        }
    }
}

impl Drop for PlacementLease {
    fn drop(&mut self) {
        // `release_claim(strict=True)`'s loud finally: a swallow here is a
        // custody loss the next holder pays for. The store's non-strict
        // release reports the miss; this prints it.
        if let Err(exc) = claims::release(&self.key, &self.holder, self.root.as_deref(), None) {
            eprintln!(
                "mux placement lease {} release failed after spawn: {exc}",
                self.key
            );
        }
    }
}

/// 16 random hex chars for the lease holder's unique half, from the OS
/// CSPRNG through the crate's declared `getrandom`.
fn random_hex16() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `mux pane run` transport, injected so tests drive fake answers - the
/// same seam Python's `runner=subprocess.run` parameter occupies.
pub trait MuxCli {
    fn run(&self, args: &[String], env: &[(String, String)]) -> std::io::Result<Output>;
}

/// The real runner: shells the installed `fno` front door, the same spell
/// `resume_wake.rs` uses for `mux pane read`.
pub struct FsMux;

impl MuxCli for FsMux {
    fn run(&self, args: &[String], env: &[(String, String)]) -> std::io::Result<Output> {
        Command::new(fno_bin())
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .output()
    }
}

/// `true` when the pane-run transport carries `--json` (the explicit-geometry
/// lanes); the plain lane prints the bare pane id.
fn json_receipt(ctx: &PaneContext) -> bool {
    ctx.at.is_some() || ctx.tab_id.is_some()
}

/// The outer `pane run` transport, placement directives before the `--` that
/// fences the provider argv (`build_pane_argv` stays placement-blind).
pub fn pane_run_args(ctx: &PaneContext, session: &str, name: &str, plan: &ArmPlan) -> Vec<String> {
    let mut args: Vec<String> = [
        "mux".into(),
        "pane".into(),
        "run".into(),
        "--claim".into(),
        "--server".into(),
        session.into(),
        "--cwd".into(),
        ctx.cwd.to_string_lossy().into_owned(),
        "--worker".into(),
        name.into(),
        // The argv carries the group cap explicitly; the env copy (below)
        // latches the same value at server birth.
        "--max-panes".into(),
        DEFAULT_PANE_GROUP_MAX.into(),
    ]
    .to_vec();
    if let Some(squad) = &ctx.squad {
        args.extend(["squad".to_string(), squad.clone()]);
    }
    if let Some(split) = &ctx.split {
        args.extend(["split".to_string(), split.clone()]);
    }
    if let Some(tab) = &ctx.tab {
        // A bare name is a pane GROUP: no tab rides the transport; the pane
        // lands in its own tab and joins the group after the run (the
        // act-then-place contract). A selector rides verbatim.
        let (selector, _group) = placement::resolve_group_tab(tab);
        if let Some(selector) = selector {
            args.extend(["--tab".to_string(), selector]);
        }
    }
    if let Some(at) = &ctx.at {
        // Forwarded verbatim; the mux CLI resolves `current` from FNO_PANE so
        // one Rust parser owns env identity (the Python arm's note).
        args.extend(["at".to_string(), at.clone()]);
    }
    if let Some(tab_id) = &ctx.tab_id {
        args.extend(["--tab".to_string(), tab_id.trim().to_string()]);
    }
    if ctx.bounded_placement
        && ctx.split.is_none()
        && ctx.at.is_none()
        && ctx.tab.is_none()
        && ctx.tab_id.is_none()
    {
        // The bounded lane with no geometry asks the SERVER to pick the tab:
        // first below max-panes, else a new one. Python's gate reads
        // `not (split or at or tab or tab_id)` - a squad is NOT geometry: a
        // bounded spawn into a squad still asks the server to fit it.
        args.push("--fit".to_string());
    }
    if json_receipt(ctx) {
        args.push("--json".to_string());
    }
    if let Some(id) = &plan.preassigned_id {
        args.extend(["--fno-id".to_string(), id.clone()]);
    }
    args.push("--".to_string());
    args.extend(plan.wrapped.iter().cloned());
    args
}

/// The pane-run subprocess env: ambient minus the writable-dir grant (the
/// argv already carries it; leaving it would pin THIS spawn's grant onto the
/// server and every later pane shell), plus the three config->env bridges.
pub fn pane_run_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    for (k, v) in std::env::vars_os() {
        let Some(k) = k.to_str() else { continue };
        if k == "FNO_WORKER_ADD_DIRS" {
            continue;
        }
        if let Some(v) = v.to_str() {
            env.push((k.to_string(), v.to_string()));
        }
    }
    for (key, default) in [
        ("FNO_MUX_SHELL_INTEGRATION", "on"),
        ("FNO_PROCESS_ADMISSION_MAX", DEFAULT_PROCESS_ADMISSION_MAX),
        ("FNO_MUX_PANE_GROUP_MAX", DEFAULT_PANE_GROUP_MAX),
    ] {
        if !env.iter().any(|(k, _)| k == key) {
            env.push((key.to_string(), default.to_string()));
        }
    }
    env
}

/// The failed-`pane run` line: non-zero exit means no pane was created, so
/// the refusal leaves no half-created state (the G1 contract).
pub fn pane_run_failure_line(session: &str, detail: &str) -> String {
    let detail = if detail.trim().is_empty() {
        "no output"
    } else {
        detail.trim()
    };
    format!(
        "mux pane spawn failed in session {}: {detail} (no registry row written; there is \
         no daemon-PTY fallback)",
        py_repr(session)
    )
}

#[derive(Debug)]
pub enum PaneRunOutcome {
    Created { pane_id: u64 },
    ControlUnanswered,
}

pub fn parse_pane_run(
    proc: &Output,
    json_receipt: bool,
    session: &str,
) -> Result<PaneRunOutcome, String> {
    if proc.status.code() == Some(MUX_CONTROL_UNANSWERED) {
        return Ok(PaneRunOutcome::ControlUnanswered);
    }
    if !proc.status.success() {
        let raw = if proc.stderr.is_empty() {
            &proc.stdout
        } else {
            &proc.stderr
        };
        return Err(String::from_utf8_lossy(raw).trim().to_string());
    }
    let stdout = String::from_utf8_lossy(&proc.stdout);
    let last = stdout.trim().split('\n').next_back().unwrap_or_default();
    let unparseable = || {
        format!(
            "mux pane run returned unparseable {} {} for session {}; a pane may exist without \
             a registry row - inspect with 'fno mux pane ls --session {session}'",
            if json_receipt { "--json" } else { "output" },
            py_repr(&stdout),
            py_repr(session)
        )
    };
    if json_receipt {
        let payload: Value = serde_json::from_str(last).map_err(|_| unparseable())?;
        payload
            .get("pane_id")
            .and_then(Value::as_u64)
            .map(|pane_id| PaneRunOutcome::Created { pane_id })
            .ok_or_else(unparseable)
    } else {
        last.trim()
            .parse::<u64>()
            .map(|pane_id| PaneRunOutcome::Created { pane_id })
            .map_err(|_| unparseable())
    }
}

/// After `MUX_CONTROL_UNANSWERED`, decide whether a pane exists instead of
/// asserting it does not (the Python leg's LD2/LD3): enumerate the session's
/// panes and adopt exactly one candidate matching this spawn - same cwd, a
/// live child pid started at/after the spawn clock, and not already claimed
/// by a registry row. Zero, one, many -> three different, honest answers.
/// Never retries the run: the command reached the server, and re-sending risks
/// a second pane.
pub fn reconcile_unanswered_run(
    session: &str,
    cwd: &Path,
    spawn_started_ms: i64,
    registry: &state::Registry,
    mux: &dyn MuxCli,
) -> Result<u64, PaneRefusal> {
    let (rows, detail) = pane_ls_detailed(session, mux);
    let rows = match rows {
        Some(rows) if !rows.is_empty() => rows,
        // An empty or unanswerable listing is UNKNOWN, never proof of
        // absence: `pane ls` prints [] and exits 0 for a refused socket too.
        // The read's failure detail rides the line the way Python appends it:
        // ` ({detail})`, present only when there is something to say.
        _ => {
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!(" ({detail})")
            };
            return Err(PaneRefusal {
                line: format!(
                    "the mux never answered 'pane run' for session {}, and 'pane ls' could \
                     not confirm whether a pane exists{suffix}; a pane may be live - inspect \
                     with 'fno mux pane ls --session {session}' before retrying.",
                    py_repr(session)
                ),
                exit_code: EXIT_SPAWN_FAILED,
                after_pane: None,
            });
        }
    };
    // The claimed-id set is SCOPED TO THIS SESSION (Python: pane ids are
    // allocated independently per server, and registry identity is the
    // (session, pane_id) pair).
    let claimed: Vec<u64> = registry
        .entries
        .iter()
        .filter_map(|e| {
            e.mux
                .as_ref()
                .filter(|m| m.session == session)
                .map(|m| m.pane_id)
        })
        .collect();
    let mut candidates: Vec<u64> = Vec::new();
    for row in &rows {
        let row_cwd = row.get("cwd").and_then(Value::as_str).unwrap_or_default();
        if row_cwd != cwd.to_string_lossy() {
            continue;
        }
        let Some(pid) = row.get("child_pid").and_then(Value::as_u64) else {
            continue;
        };
        let Some(started) = crate::claims::process_create_time_ms(pid as i32) else {
            continue;
        };
        if started < spawn_started_ms {
            continue;
        }
        let Some(pane_id) = row.get("pane_id").and_then(Value::as_u64) else {
            continue;
        };
        if claimed.contains(&pane_id) {
            continue;
        }
        candidates.push(pane_id);
    }
    match candidates.len() {
        0 => Err(PaneRefusal {
            line: format!(
                "no pane was created; the mux never answered and no pane in {} matches this \
                 spawn. Retry, or use --substrate thread.",
                py_repr(session)
            ),
            exit_code: EXIT_SPAWN_FAILED,
            after_pane: None,
        }),
        1 => Ok(candidates[0]),
        n => {
            let ids = candidates
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(PaneRefusal {
                line: format!(
                    "the mux never answered and {n} panes in {} match this spawn ({ids}); \
                     cannot tell them apart - inspect with 'fno mux pane ls --session \
                     {session}' before retrying.",
                    py_repr(session)
                ),
                exit_code: EXIT_SPAWN_FAILED,
                after_pane: None,
            })
        }
    }
}

/// `mux pane ls --server <session> --json`: the listing rows plus the failure
/// detail the unknown-answer line carries (` ({detail})`), Python's split of
/// "could not ask" (None) from "answered empty" (Some, empty). The detail is
/// the run error, the non-zero stderr, or the named unparseable-output line.
pub fn pane_ls_detailed(session: &str, mux: &dyn MuxCli) -> (Option<Vec<Value>>, String) {
    let args: Vec<String> = [
        "mux".into(),
        "pane".into(),
        "ls".into(),
        "--server".into(),
        session.into(),
        "--json".into(),
    ]
    .to_vec();
    let proc = match mux.run(&args, &[]) {
        Ok(proc) => proc,
        Err(exc) => return (None, exc.to_string()),
    };
    if !proc.status.success() {
        return (
            None,
            String::from_utf8_lossy(&proc.stderr).trim().to_string(),
        );
    }
    let stdout = String::from_utf8_lossy(&proc.stdout);
    match serde_json::from_str::<Vec<Value>>(stdout.trim()) {
        Ok(rows) => (Some(rows), String::new()),
        Err(_) => (
            None,
            format!("unparseable pane ls output: {}", py_repr(stdout.trim())),
        ),
    }
}

/// `mux pane ls --server <session> --json` rows, or None when the mux could
/// not be asked (exit non-zero or unparseable output).
pub fn pane_ls(session: &str, mux: &dyn MuxCli) -> Option<Vec<Value>> {
    pane_ls_detailed(session, mux).0
}

/// Best-effort child-pid fetch (`pane ls --json`); feeds the row's `pid` so
/// reconcile/GC can probe liveness. None on any miss - the pane is live
/// regardless.
pub fn lookup_child_pid(session: &str, pane_id: u64, mux: &dyn MuxCli) -> Option<u32> {
    for row in pane_ls(session, mux)? {
        if row.get("pane_id").and_then(Value::as_u64) == Some(pane_id) {
            return row
                .get("child_pid")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
        }
    }
    None
}

/// Kill the pane and return whether the CHILD is confirmed gone. The pid is
/// read before the kill (the mux cannot report it afterwards) and its
/// incarnation token guards against a recycled pid - the same
/// start-time-equality check Python's reap wrapper runs.
pub fn reap_spawned_pane(session: &str, pane_id: u64, mux: &dyn MuxCli) -> (bool, String) {
    let child_pid = lookup_child_pid(session, pane_id, mux);
    // The incarnation token, read before the kill: platform-native units
    // (the registry's `pid_start_time` units), equality-checked only.
    let start_before = child_pid.and_then(process_start_time);
    let args: Vec<String> = [
        "mux".into(),
        "pane".into(),
        "kill".into(),
        "--server".into(),
        session.into(),
        pane_id.to_string(),
    ]
    .to_vec();
    let proc = match mux.run(&args, &[]) {
        Ok(proc) => proc,
        Err(exc) => return (false, exc.to_string()),
    };
    if !proc.status.success() {
        let detail = if !proc.stderr.is_empty() {
            String::from_utf8_lossy(&proc.stderr).trim().to_string()
        } else if !proc.stdout.is_empty() {
            String::from_utf8_lossy(&proc.stdout).trim().to_string()
        } else {
            "no output".to_string()
        };
        return (false, detail);
    }
    let Some(child_pid) = child_pid else {
        return (
            false,
            format!(
                "pane {pane_id} kill returned 0 but its child pid was never resolved, so the \
                 process is unconfirmed"
            ),
        );
    };
    if start_before.is_some() && process_start_time(child_pid) == start_before {
        return (
            false,
            format!(
                "pane {pane_id} kill returned 0 but child pid {child_pid} is still running; the \
                 worker survived teardown"
            ),
        );
    }
    (true, String::new())
}

/// The registry row the Python leg mints (mux_spawn.py:4400): written in the
/// SAME transaction shape - one append inside the per-agent lock, status
/// derived from the bind, the mux ref the one live transport ref. The corpse
/// of a reclaimed name drops in the same transaction (`replacing`), a bind id
/// another row already holds is dropped to live-only (the claim check), and a
/// row with no resolvable handle gets a touched log file (the write guard's
/// three-leg invariant needs one).
pub fn write_pane_row(
    home: &AgentsHome,
    name: &str,
    ctx: &PaneContext,
    pane: &PaneFacts,
    bind: &ArmBind,
    replacing: bool,
    spawn_trigger: &Option<String>,
) -> Result<(), String> {
    let (status, session_id) = match bind {
        ArmBind::Bound { session_id } => (AgentStatus::Live, Some(session_id.clone())),
        ArmBind::Unbound { .. } => (AgentStatus::Spawning, None),
        ArmBind::Died { .. } => (AgentStatus::Failed, None),
        ArmBind::NotApplicable => (AgentStatus::Live, None),
    };
    let pid_start_time = pane.child_pid.and_then(process_start_time);
    let entry_log_path = resolvable_handle_log_fallback(home, name, pane, &session_id);
    let stamp: std::collections::BTreeMap<String, String> =
        crate::accounts::launch::pane_stamp().into_iter().collect();
    let launch_account = stamp.get(crate::state::LAUNCH_ACCOUNT_ENV_KEY).cloned();
    let entry = RegistryEntry {
        name: name.to_string(),
        harness: Some(ctx.provider.clone()),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        requested_model: None,
        requested_provider: None,
        requested_effort: None,
        spawn_payload: (!ctx.message.is_empty()).then(|| ctx.message.clone()),
        cwd: ctx.cwd.to_string_lossy().into_owned(),
        log_path: entry_log_path,
        status,
        harness_session_id: session_id,
        pid: pane.child_pid,
        pid_start_time,
        mux: Some(MuxRef {
            session: pane.session.clone(),
            pane_id: pane.pane_id,
        }),
        origin: Some("spawn".to_string()),
        substrate: Some("pane".to_string()),
        spawn_trigger: spawn_trigger.clone(),
        launch_account_source: stamp
            .get(crate::state::LAUNCH_ACCOUNT_SOURCE_ENV_KEY)
            .cloned(),
        account_record_id: launch_account.clone(),
        launch_account,
        ..RegistryEntry::new(None, Lineage::captured((None, None, None)))
    };
    write_pane_row_txn(home, name, entry, replacing)
}

/// Python's pid-start units note and the three-leg fallback, split out of
/// [`write_pane_row`]: the minted row's `log_path` is set only when the touch
/// produced a real file.
fn resolvable_handle_log_fallback(
    home: &AgentsHome,
    name: &str,
    pane: &PaneFacts,
    session_id: &Option<String>,
) -> Option<String> {
    let leg1 = pane.child_pid.is_some() && pane.child_pid.and_then(process_start_time).is_some();
    let leg3 = session_id.as_deref().is_some_and(|s| !s.is_empty());
    if leg1 || leg3 {
        return None;
    }
    let log_path = home.root().join("logs").join(format!("{name}.log"));
    let mut open = std::fs::OpenOptions::new();
    std::fs::create_dir_all(log_path.parent()?).ok()?;
    open.write(true).create(true).open(&log_path).ok()?;
    Some(log_path.display().to_string())
}

/// The registry transaction Python's `_append` runs: pre-checked race
/// refusal, corpse drop, claim-dup id nulling, then the append.
fn write_pane_row_txn(
    home: &AgentsHome,
    name: &str,
    entry: RegistryEntry,
    replacing: bool,
) -> Result<(), String> {
    write_pane_row_txn_inner(home, name, entry, replacing).map_err(|e| match e {
        TxnFailure::State(exc) => exc.to_string(),
        TxnFailure::Race(line) => line,
    })
}

enum TxnFailure {
    State(state::StateError),
    Race(String),
}

fn write_pane_row_txn_inner(
    home: &AgentsHome,
    name: &str,
    entry: RegistryEntry,
    replacing: bool,
) -> Result<(), TxnFailure> {
    let outcome = state::update_registry(&home.registry_json(), |registry| {
        race_checked_append(name, entry, replacing, registry)
    });
    outcome
        .map_err(TxnFailure::State)
        .and_then(|inner| inner.map_err(TxnFailure::Race))
        .map(|_| ())
}

fn race_checked_append(
    name: &str,
    entry: RegistryEntry,
    replacing: bool,
    registry: &mut state::Registry,
) -> Result<bool, String> {
    if replacing {
        let revived = registry
            .entries
            .iter()
            .any(|e| e.name == name && !is_reclaimable(e.status));
        if revived {
            return Err(race_refusal_line(name));
        }
        registry
            .entries
            .retain(|e| !(e.name == name && is_reclaimable(e.status)));
    }
    let entry = drop_claimed_bind_id(entry, registry);
    registry.entries.push(entry);
    Ok(true)
}

fn race_refusal_line(name: &str) -> String {
    format!(
        "agent {} was reclaimable when checked but is not now (a concurrent writer changed its \
         status); nothing was written - re-run, or 'fno agents rm {name}' first",
        py_repr(name)
    )
}

fn drop_claimed_bind_id(mut entry: RegistryEntry, registry: &state::Registry) -> RegistryEntry {
    let Some(id) = entry.harness_session_id.clone() else {
        return entry;
    };
    let claimed = registry
        .entries
        .iter()
        .any(|e| e.harness_session_id.as_deref() == Some(id.as_str()));
    if claimed {
        entry.harness_session_id = None;
    }
    entry
}

/// The compare-and-set late-bind stamp (Python's `stamp_late_bind`, through
/// `codex_backfill_stamp_applies`' guards): only this pane's id-less
/// non-terminal row, and never an id another row holds. A raced miss leaves
/// the row `spawning` for the next reconcile to heal; the bool says whether
/// the stamp landed.
pub fn stamp_late_bind(
    home: &AgentsHome,
    name: &str,
    session_id: &str,
    child_pid: Option<u32>,
    pid_start_time: Option<u64>,
    session: &str,
    pane_id: u64,
) -> bool {
    let path = home.registry_json();
    let applies = |row: &RegistryEntry| {
        row.name == name
            && row.harness_session_id.is_none()
            && row.pid == child_pid
            && row.pid_start_time == pid_start_time
            && row.mux.as_ref()
                == Some(&MuxRef {
                    session: session.to_string(),
                    pane_id,
                })
            && !matches!(row.status, AgentStatus::Exited | AgentStatus::PermanentDead)
    };
    let stamped = state::update_registry(&path, |registry| {
        let dup = registry
            .entries
            .iter()
            .any(|r| r.name != name && r.harness_session_id.as_deref() == Some(session_id));
        let mut landed = false;
        if !dup {
            for row in registry.entries.iter_mut() {
                if applies(row) {
                    row.harness_session_id = Some(session_id.to_string());
                    row.status = AgentStatus::Live;
                    landed = true;
                }
            }
        }
        landed
    });
    // Python's return contract is "the row NOW carries the id": the write
    // always happens (an Err leaves the bool false), then the re-read is the
    // truth - a concurrent writer may have clobbered or landed the same
    // stamp, and only the re-read sees it.
    let _ = stamped;
    state::load_registry(&path)
        .ok()
        .map(|registry| {
            registry
                .find(name)
                .filter(|row| row.harness_session_id.as_deref() == Some(session_id))
                .is_some()
        })
        .unwrap_or(false)
}

/// A row in one of these statuses does not own
/// its name; the spawn's write transaction drops the corpse and appends the
/// replacement.
fn is_reclaimable(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Exited | AgentStatus::Failed | AgentStatus::PermanentDead
    )
}

/// `dispatch._capture_spawn_trigger`: pop `FNO_SPAWN_TRIGGER`, trimmed, None
/// when empty. The pop runs BEFORE the pane-env snapshot so the trigger
/// stamps this row without leaking into the pane shell, where the child
/// would inherit the machine-dispatch mark.
fn capture_spawn_trigger() -> Option<String> {
    let value = std::env::var("FNO_SPAWN_TRIGGER").ok()?;
    std::env::remove_var("FNO_SPAWN_TRIGGER");
    let trimmed = value.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// `_run_mux`'s FileNotFoundError/OSError arms: the front door never ran, so
/// no pane exists and the line names the binary. Both exit 127.
fn spawn_exec_failure_line(exc: &std::io::Error) -> (String, i32) {
    let bin = fno_bin().to_string_lossy().into_owned();
    if exc.kind() == std::io::ErrorKind::NotFound {
        (
            format!(
                "the '{bin}' binary was not found on PATH; the pane substrate is hosted by the \
                 fno mux (set FNO_BIN or install fno)"
            ),
            EXIT_CMD_NOT_FOUND,
        )
    } else {
        (format!("could not run '{bin}': {exc}"), EXIT_CMD_NOT_FOUND)
    }
}

/// The stdout receipt, key-for-key the object `cmd_spawn` builds
/// (cli.py:2230): `bound` and `status` stay separate facts, conditionals are
/// present only when they have something to say, and serde_json's
/// preserve_order keeps the key order stable for the golden.
pub struct PaneReceipt {
    obj: Map<String, Value>,
}

impl PaneReceipt {
    pub fn new(name: &str, ctx: &PaneContext, pane: &PaneFacts, bind: &ArmBind) -> Self {
        let mut obj = Map::new();
        obj.insert("name".into(), Value::String(name.to_string()));
        obj.insert("short_id".into(), Value::String(String::new()));
        obj.insert("harness".into(), Value::String(ctx.provider.clone()));
        obj.insert(
            "status".into(),
            Value::String(
                match bind {
                    ArmBind::Bound { .. } | ArmBind::NotApplicable => "live",
                    ArmBind::Unbound { .. } => "spawning",
                    ArmBind::Died { .. } => "failed",
                }
                .to_string(),
            ),
        );
        obj.insert("mux_session".into(), Value::String(pane.session.clone()));
        obj.insert("pane_id".into(), Value::from(pane.pane_id));
        // `bound` precedes `seed`, and both precede the conditionals - the
        // CLI's insertion order. `seed` is null when no seed was requested
        // (Python's Optional[str] default), "submitted" once one rode the
        // argv or a delivered turn; "unattempted" is a probe answer this
        // spine does not have yet (the arm supplies it in PR 2).
        obj.insert(
            "bound".into(),
            match bind {
                ArmBind::Bound { .. } => Value::Bool(true),
                ArmBind::Unbound { .. } => Value::Bool(false),
                ArmBind::Died { .. } => Value::Bool(false),
                ArmBind::NotApplicable => Value::Null,
            },
        );
        obj.insert(
            "seed".into(),
            if ctx.message.is_empty() {
                Value::Null
            } else {
                Value::String("submitted".to_string())
            },
        );
        match bind {
            ArmBind::Bound { session_id } => {
                obj.insert("session_id".into(), Value::String(session_id.clone()));
            }
            ArmBind::Unbound { reason } => {
                obj.insert("pane_alive".into(), Value::Bool(true));
                obj.insert("unbound_reason".into(), Value::String(reason.clone()));
            }
            ArmBind::Died { reason } => {
                obj.insert("pane_alive".into(), Value::Bool(false));
                obj.insert("unbound_reason".into(), Value::String(reason.clone()));
            }
            ArmBind::NotApplicable => {}
        }
        PaneReceipt { obj }
    }

    /// Spacing (", " between items, ": " after keys) so
    /// the printed receipt is byte-equal to the CLI's stdout, not
    /// serde_json-compact.
    pub fn to_json(&self) -> String {
        let body: Vec<String> = self
            .obj
            .iter()
            .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), v))
            .collect();
        format!("{{{}}}", body.join(", "))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "row8_tests.rs"]
mod row8_tests;

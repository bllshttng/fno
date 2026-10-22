//! The claude pane arm (row 8, PR B): the `build_pane_argv` claude branch
//! (`cli/src/fno/agents/mux_spawn.py:933-948`) plus the legs that branch
//! composes around it - the session pin (`:3472`), the billing guard
//! (`claude_argv_is_interactive` :490, checked at :3534), the monitor
//! resolution (`resolve_monitor` :265), the route-model pin
//! (`inject_route_model_argv` :466) and the happy wrap (`happy_pane_argv`
//! :353). The bind is preassigned: the pinned uuid names the transcript, and
//! a happy monitor deliberately binds nothing (happy owns the id).
//!
//! Recorded narrowings, all bounded by what the client door seam-carves to
//! Python before an arm runs:
//! - `--account` never reaches a pane arm, so `resolve_monitor`'s
//!   conflicting-credentials gate (mux_spawn.py:314-327) is unreachable here.
//! - The provider-cap route gates (mux_spawn.py:3330-3341) are unreachable:
//!   the arm resolves `ctx.route` itself and stamps `ROUTE_PROVIDER_ENV`, so
//!   a route it carries always has a bound provider identity.
//! - `resolve_monitor`'s expected-route equality check (mux_spawn.py:332-346)
//!   is a tautology here: the arm resolves the route fresh in the same call.
//! - `config.agents.happy_routed_panes` defaults off through the arm; the
//!   explicit `--monitor happy` lane is the supported path (the Rust settings
//!   reader does not exist yet).
//! - Python's unrouted model-env clear (`_mesh_env_pairs`' harness==claude
//!   leg) has no arm half: the scrub rides only when a route resolved.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::env_wrap::{mesh_env_wrapper, qos_wrap_for, seed_provenance_env};
use super::readiness::{await_interactive_readiness, ReadinessState};
use super::simple::{grant_tokens, seed_text, spawn_failed_refusal};
use super::{
    ArmBind, ArmPlan, PaneArm, PaneContext, PaneFacts, PaneRefusal, EXIT_CMD_NOT_FOUND,
    EXIT_REFUSAL,
};
use crate::claude_ask::py_repr;
use crate::provider_route::{claude_route, ClaudeRoute, RouteCtx};

/// The one claude pane arm. Stateless: every per-spawn fact rides the
/// [`PaneContext`] and the plan it produces.
pub struct ClaudeArm;

/// The arm instance `pane_arm_for` returns; a static so the
/// `&'static dyn PaneArm` table stays allocation-free.
pub static CLAUDE_ARM: ClaudeArm = ClaudeArm;

impl ClaudeArm {
    pub fn owns(ctx: &PaneContext) -> bool {
        ctx.provider == "claude"
    }
}

/// The billing-guard refusal, byte-equal to the Python leg
/// (mux_spawn.py:3536-3539; the predicate is `claude_argv_is_interactive`
/// :490): any `-p`/`--print` token on the composed argv is the
/// Agent-SDK-credit lane, and the mux hosts only interactive
/// subscription-billed claude.
const BILLING_REFUSAL: &str = "refusing to pane-host claude with -p/--print (that bills the Agent SDK pool); the mux spawns interactive subscription-billed claude";

/// The happy wrap's `--settings` refusal, byte-equal to the Python leg
/// (mux_spawn.py:422-431).
const HAPPY_SETTINGS_REFUSAL: &str = "refusing to launch a routed claude pane through happy with --settings: happy consumes that flag for its own hook server and discards the caller's file, so the route would be silently ignored and the worker would launch on the default account. Carry the route in the env(1) wrapper (happy merges its own environment into the claude child), with the non-secret remainder as --claude-env KEY=VALUE. Never put a credential on --claude-env: happy is a long-lived parent, so its argv is a world-readable `ps` token.";

/// The happy wrap's `--session-id` refusal, byte-equal to the Python leg
/// (mux_spawn.py:435-440).
const HAPPY_SESSION_ID_REFUSAL: &str = "refusing to launch a claude pane through happy with --session-id: happy extracts that flag out of the argv and never re-adds it under its hook server, so claude mints a different id and the spawn receipt would name a session that never exists (unpeekable worker, id-less registry row). Let happy own the id; fno discovers it afterwards.";

/// The explicit-monitor PATH refusal, byte-equal to the Python leg
/// (mux_spawn.py:445-447). The config-lane variant is unreachable through the
/// arm: auto-happy is seam-carved off (the recorded narrowing above).
const HAPPY_NOT_ON_PATH: &str = "--monitor happy was requested, but 'happy' is not on PATH; install it with npm install -g happy";

/// The route keys `--monitor happy` requires, in the Python leg's order
/// (`resolve_monitor`, mux_spawn.py:292-296). The pane grammar has no
/// `--model` to refuse (the axis is retired), so a route whose model leg is
/// empty dies on the missing-key half of this gate instead.
const MONITOR_ROUTE_KEYS: [&str; 3] = [
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
];

/// `resolve_monitor`'s completeness refusal (mux_spawn.py:344), pinned
/// locally: spawn_axes' MONITOR_ZAI_GOLDEN is the headless door's different
/// line, and this gate is pane-only.
const MONITOR_COMPLETE_GOLDEN: &str =
    "--monitor happy currently requires a complete resolved zai route";

/// A resolved route for one pane spawn: the parsed target's provider and the
/// resolved env with the provider stamp - the env the monitor gates read,
/// the wrapper carries, and the happy wrap forwards.
struct ResolvedRoute {
    provider: String,
    env: BTreeMap<String, String>,
}

/// `spawn_route.rs`'s refusal spelling, reused so the pane arm and the thread
/// seam cannot disagree about what a bad `--route` says.
fn route_refused(route: &str, reason: &str) -> PaneRefusal {
    PaneRefusal {
        line: crate::spawn_route::refused(route, reason),
        exit_code: EXIT_REFUSAL,
        after_pane: None,
    }
}

/// Resolve `ctx.route` the way `route_seam` does for a thread spawn
/// (spawn_route.rs:100-146), minus the settings file: a pane carries its
/// route as env, and `route_seam` itself refuses the pane substrate to
/// Python - the one leg the pane arm must not call.
fn resolve_route(ctx: &PaneContext) -> Result<Option<ResolvedRoute>, PaneRefusal> {
    let Some(route) = ctx.route.as_deref() else {
        return Ok(None);
    };
    let (provider, model) = match crate::spawn_route::parse_target(route) {
        Some((p, m)) => (p, m),
        None => {
            let row = crate::route_gather::run_mode(
                &json!({"mode": "lane", "match": "name", "name": route}),
                &ctx.cwd,
            );
            if row.get("found").and_then(Value::as_bool) != Some(true) {
                return Err(PaneRefusal {
                    line: format!(
                        "--route takes 'provider,model' or a declared lane name; got {}",
                        py_repr(route)
                    ),
                    exit_code: EXIT_REFUSAL,
                    after_pane: None,
                });
            }
            let field = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            if field("vendor").is_empty() {
                return Err(route_refused(
                    route,
                    &format!(
                        "--route {} names a lane that declares no route; no worker launched, \
                         node stays dispatchable.",
                        py_repr(route)
                    ),
                ));
            }
            (field("vendor"), field("model"))
        }
    };
    // resolve_explicit_route: a blank provider or model resolves nothing.
    let pname = provider.trim().to_lowercase();
    if pname.is_empty() || model.trim().is_empty() {
        return Err(route_refused(route, crate::spawn_route::ROUTE_KEYLESS));
    }
    let mut env = match claude_route(
        &ctx.cwd,
        &pname,
        model.trim(),
        &RouteCtx::Peer(pname.clone()),
        &|k| std::env::var(k).ok(),
    ) {
        ClaudeRoute::Routed { env, .. } => env,
        ClaudeRoute::Skipped { notices } => {
            let reason = if notices.is_empty() {
                crate::spawn_route::ROUTE_KEYLESS.to_string()
            } else {
                notices.join("; ")
            };
            return Err(route_refused(route, &reason));
        }
    };
    // bind_route_provider: the stamp rides the route env into the wrapper.
    env.insert(
        crate::spawn_route::ROUTE_PROVIDER_ENV.to_string(),
        provider.clone(),
    );
    Ok(Some(ResolvedRoute { provider, env }))
}

/// The claude argv before the monitor, route and happy legs: the declared
/// `interactive_create` head (`claude --session-id <uuid>`), `--name`, the
/// `--add-dir` grant, the passthrough tail, then the seed fence. Token order
/// is the Python leg's (`mux_spawn.py:933-948`); the pin rides BEFORE `--name`
/// (the golden's `pinned_session` case).
pub(crate) fn claude_core_argv(
    ctx: &PaneContext,
    seed: &str,
    uuid: Option<&str>,
) -> Result<Vec<String>, PaneRefusal> {
    let contract =
        crate::harness_capabilities::HarnessContract::packaged().expect("embedded contract");
    let mut argv = contract
        .render_session_argv_raw("claude", "interactive_create", uuid)
        .expect("embedded claude interactive_create");
    if !ctx.worker_name.is_empty() {
        argv.push("--name".to_string());
        argv.push(ctx.worker_name.clone());
    }
    argv.extend(grant_tokens("claude"));
    argv.extend(ctx.passthrough.iter().cloned());
    if !seed.is_empty() {
        // The seed rides behind `--` so a leading-flag seed reaches claude as
        // the prompt positional instead of dying in its flag parser
        // (mux_spawn.py:944-947).
        argv.push("--".to_string());
        argv.push(seed.to_string());
    }
    Ok(argv)
}

/// The monitor gates (`resolve_monitor` port): the value gate on any explicit
/// value, then the completeness gate a happy monitor pays. The
/// harness-is-claude gate is unreachable - this arm only owns claude.
fn monitor_gates(ctx: &PaneContext, route: &Option<ResolvedRoute>) -> Result<(), PaneRefusal> {
    let Some(monitor) = ctx.monitor.as_deref() else {
        // The auto-happy tail (mux_spawn.py:348-349) reads
        // config.agents.happy_routed_panes, which defaults off through the
        // arm; the explicit `--monitor happy` lane is the supported path.
        return Ok(());
    };
    if monitor != "happy" {
        return Err(PaneRefusal {
            line: format!(
                "{} (got {})",
                crate::spawn_axes::MONITOR_VALUE_GOLDEN,
                py_repr(monitor)
            ),
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    }
    // The completeness gate: a resolved zai route carrying all three keys.
    // Python's suffix names the missing keys; a provider mismatch with every
    // key present prints the bare line, like the Python leg.
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let env = route.as_ref().map(|r| &r.env).unwrap_or(&empty);
    let missing: Vec<String> = MONITOR_ROUTE_KEYS
        .iter()
        .filter(|k| env.get(**k).map(|v| v.trim().is_empty()).unwrap_or(true))
        .map(|k| k.to_string())
        .collect();
    let provider_ok = route.as_ref().map(|r| r.provider == "zai").unwrap_or(false);
    if !provider_ok || !missing.is_empty() {
        let detail = if missing.is_empty() {
            String::new()
        } else {
            format!("; missing {}", missing.join(", "))
        };
        return Err(PaneRefusal {
            line: format!("{}{}", MONITOR_COMPLETE_GOLDEN, detail),
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    }
    Ok(())
}

/// `shutil.which("happy")`'s PATH walk: an executable-looking `happy` file in
/// any PATH dir.
fn happy_on_path() -> bool {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("happy").is_file() {
                return true;
            }
        }
    }
    false
}

/// `inject_route_model_argv` (mux_spawn.py:466-488): pin the route's model on
/// argv ahead of every settings rung. An argv that already names a model is
/// caller intent and wins; the token lands flags-first, where it can never sit
/// inside a `--` seed fence.
fn inject_route_model_argv(argv: Vec<String>, route_env: &BTreeMap<String, String>) -> Vec<String> {
    if argv.first().map(String::as_str) != Some("claude") {
        return argv;
    }
    let model = route_env
        .get("ANTHROPIC_MODEL")
        .map(|s| s.trim())
        .unwrap_or_default();
    if model.is_empty()
        || argv
            .iter()
            .any(|tok| tok == "--model" || tok == "-m" || tok.starts_with("--model="))
    {
        return argv;
    }
    let mut out = vec![argv[0].clone(), "--model".to_string(), model.to_string()];
    out.extend_from_slice(&argv[1..]);
    out
}

/// The happy wrap (`happy_pane_argv` port): refuse the two flags happy eats,
/// demand the binary, then carry the argv on `happy` with the route's
/// non-secret keys as `--claude-env` pairs. The secrets (ANTHROPIC_API_KEY,
/// ANTHROPIC_AUTH_TOKEN) ride the env(1) wrapper, never the long-lived happy
/// argv - `SECRET_ROUTE_VARS` (account_env.py:97).
fn happy_wrap(
    argv: Vec<String>,
    route_env: &BTreeMap<String, String>,
) -> Result<Vec<String>, PaneRefusal> {
    if argv
        .iter()
        .any(|tok| tok == "--settings" || tok.starts_with("--settings="))
    {
        return Err(PaneRefusal {
            line: HAPPY_SETTINGS_REFUSAL.to_string(),
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    }
    if argv
        .iter()
        .any(|tok| tok == "--session-id" || tok.starts_with("--session-id="))
    {
        return Err(PaneRefusal {
            line: HAPPY_SESSION_ID_REFUSAL.to_string(),
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    }
    if !happy_on_path() {
        return Err(PaneRefusal {
            line: HAPPY_NOT_ON_PATH.to_string(),
            exit_code: EXIT_CMD_NOT_FOUND,
            after_pane: None,
        });
    }
    let mut out = vec!["happy".to_string()];
    for (key, value) in route_env {
        if key == "ANTHROPIC_API_KEY" || key == "ANTHROPIC_AUTH_TOKEN" {
            continue;
        }
        out.push("--claude-env".to_string());
        out.push(format!("{key}={value}"));
    }
    out.extend_from_slice(&argv[1..]);
    Ok(out)
}

/// The plan for one spawn: argv build, the billing guard, the monitor gates,
/// the route-model pin, the happy wrap, then the wrapper. `pub(crate)` for
/// the row-8 golden-parity tests.
pub(crate) fn make_claude_plan(ctx: &PaneContext) -> Result<ArmPlan, PaneRefusal> {
    let seed = seed_text(ctx);
    // The session pin (mux_spawn.py:3472): claude pins, except under happy,
    // where the pin would be discarded and the receipt would name a session
    // that never exists - so a happy spawn mints nothing and binds nothing.
    let happy = ctx.monitor.as_deref() == Some("happy");
    let uuid = if happy {
        None
    } else {
        Some(
            crate::identity::mint_fno_id()
                .expect("OS CSPRNG unavailable for the pane session uuid"),
        )
    };
    let route = resolve_route(ctx)?;
    let mut argv = claude_core_argv(ctx, &seed, uuid.as_deref())?;
    // The billing guard reads the COMPOSED argv, so a passthrough token faces
    // the identical check an fno-emitted one would (mux_spawn.py:3541-3544).
    if argv.iter().any(|tok| tok == "-p" || tok == "--print") {
        return Err(PaneRefusal {
            line: BILLING_REFUSAL.to_string(),
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    }
    monitor_gates(ctx, &route)?;
    // The model pin runs before the happy wrap so happy forwards it like any
    // other claude option (mux_spawn.py:3545-3550).
    if let Some(route) = &route {
        argv = inject_route_model_argv(argv, &route.env);
    }
    if happy {
        let env = &route
            .as_ref()
            .expect("the monitor gates resolved the happy route")
            .env;
        argv = happy_wrap(argv, env)?;
    }
    // The route env rides the wrapper (happy_pane_argv's docstring: env(1)
    // carries it into happy's own environment, which happy merges into the
    // claude child); the scrub unsets ride with it, so the route wins over
    // anything inherited.
    let overlay: Vec<(String, String)> = route
        .map(|r| r.env.into_iter().collect())
        .unwrap_or_default();
    let seed_prov = seed_provenance_env(&seed);
    let qos_argv = qos_wrap_for(ctx, argv);
    let wrapped = mesh_env_wrapper(
        &ctx.worker_name,
        "claude",
        uuid.as_deref(),
        &seed_prov,
        &overlay,
        qos_argv,
    );
    // The seed rode the argv fence; nothing is held for a post-bind submit.
    Ok(ArmPlan {
        wrapped,
        held_seed: None,
        preassigned_id: uuid,
    })
}

/// The post-run bind: the readiness gate, then the preassigned answer. No
/// seed submit - the seed is argv-committed.
fn bind_claude(
    ctx: &PaneContext,
    pane: &PaneFacts,
    plan: &ArmPlan,
) -> Result<ArmBind, PaneRefusal> {
    let readiness =
        await_interactive_readiness("claude", &pane.session, pane.pane_id, &super::FsMux);
    if readiness.state == ReadinessState::Failed {
        return Err(spawn_failed_refusal(
            ctx,
            pane,
            format!(
                "agent {name} {detail} in session {sess}",
                name = py_repr(&ctx.worker_name),
                detail = readiness.detail,
                sess = py_repr(&pane.session),
            ),
        ));
    }
    match &plan.preassigned_id {
        Some(session_id) => Ok(ArmBind::Bound {
            session_id: session_id.clone(),
        }),
        // happy: claudeLocal extracts a pinned `--session-id` and never
        // re-adds it on the normal launch branch (mux_spawn.py:3464-3471), so
        // fno mints nothing and discovers the id after the spawn - the row is
        // id-less and the pane unpeekable by design, whether healthy or a
        // corpse.
        None => Ok(ArmBind::Unbound {
            reason: "happy owns the session id; discovered after the spawn".to_string(),
        }),
    }
}

impl PaneArm for ClaudeArm {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn prepare(&self, ctx: &PaneContext) -> Result<ArmPlan, PaneRefusal> {
        make_claude_plan(ctx)
    }

    fn bind_and_seed(
        &self,
        ctx: &PaneContext,
        pane: &PaneFacts,
        plan: &ArmPlan,
    ) -> Result<ArmBind, PaneRefusal> {
        bind_claude(ctx, pane, plan)
    }
}

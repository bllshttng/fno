//! The simple-argv pane arms (row 8, PR A): opencode, pi, cursor-agent,
//! grok, agy and footnote. One arm struct, six argv builders - the harnesses
//! differ in how `build_pane_argv`'s branches compose tokens, not in the
//! spine lane they run, so one trait impl with per-harness `prepare`
//! halves the boilerplate a file-per-arm layout would have paid. The claude
//! arm is row 8 PR B: its account/route/monitor legs need seams this module
//! does not touch.

use super::env_wrap::{
    is_command_seed, mesh_env_wrapper, normalize_command, qos_wrap_for, seed_provenance_env,
};
use super::readiness::{
    await_interactive_readiness, submit_spawn_seed, ReadinessState, SeedOutcome,
};
use super::{
    ArmBind, ArmPlan, PaneArm, PaneContext, PaneFacts, PaneRefusal, EXIT_REFUSAL, EXIT_SPAWN_FAILED,
};
use crate::claude_ask::py_repr;

/// The harnesses whose argv composes from the declared roster's
/// interactive_create lane plus a grant and passthrough.
pub struct SimpleArm {
    pub harness: &'static str,
}

pub static OPENCODE_ARM: SimpleArm = SimpleArm {
    harness: "opencode",
};
pub static PI_ARM: SimpleArm = SimpleArm { harness: "pi" };
pub static CURSOR_ARM: SimpleArm = SimpleArm {
    harness: "cursor-agent",
};
pub static GROK_ARM: SimpleArm = SimpleArm { harness: "grok" };
pub static AGY_ARM: SimpleArm = SimpleArm { harness: "agy" };
pub static FOOTNOTE_ARM: SimpleArm = SimpleArm {
    harness: "footnote",
};

impl SimpleArm {
    pub fn owns(&self, ctx: &PaneContext) -> bool {
        ctx.provider == self.harness
    }
}

/// The cursor-agent refusal: a native worktree token on a pane passthrough
/// is a named refusal (the Python leg's `refuse_cursor_native_worktree_tokens`,
/// mux_spawn.py:787), before any mint or argv.
fn cursor_agent_refusal(ctx: &PaneContext) -> Result<(), PaneRefusal> {
    for token in &ctx.passthrough {
        let flag = token.split('=').next().unwrap_or_default();
        if matches!(flag, "-w" | "--worktree" | "--worktree-base") {
            return Err(PaneRefusal {
                line: format!(
                    "cursor-agent passthrough refuses native worktree flag {flag:?}; \
                     fno owns the worker worktree"
                ),
                exit_code: EXIT_REFUSAL,
                after_pane: None,
            });
        }
    }
    Ok(())
}

/// `ADD_DIR_PROVIDERS` (writable_dirs.py:92): the harnesses whose argv cell
/// carries `--add-dir`. The grant's VALUE set is the seam-published
/// `FNO_WORKER_ADD_DIRS` (`state_dirs_from_env`), never recomputed here.
const ADD_DIR_PROVIDERS: [&str; 4] = ["claude", "codex", "agy", "cursor-agent"];

/// One repeated `--add-dir` pair per state dir, when the harness maps the
/// cell. Non-member harnesses carry no grant and print no note: the seam
/// already publishes the set, and the note is Python's per-process stderr
/// once-per-provider line, which a one-shot binary arm does not re-print.
/// `pub(crate)` for the claude arm (row 8 PR B), which grants identically.
pub(crate) fn grant_tokens(harness: &str) -> Vec<String> {
    if !ADD_DIR_PROVIDERS.contains(&harness) || harness == "codex" {
        return Vec::new();
    }
    crate::claude_ask::state_dirs_from_env()
        .iter()
        .flat_map(|d| ["--add-dir".to_string(), d.clone()])
        .collect()
}

/// The per-spawn seed text: normalize a command seed, then the one payload
/// entry (brevity block once, fleet-law block last) - the Python leg's
/// render_seed/prepare ordering.
/// `pub(crate)` for the claude arm, which composes the same seed.
pub(crate) fn seed_text(ctx: &PaneContext) -> String {
    let message = ctx.message.as_str();
    let base = if is_command_seed(message) {
        normalize_command(message, ctx.provider.as_str())
    } else {
        message.to_owned()
    };
    crate::spawn_payload::finalize_spawn_payload(&base).0
}

/// The pinned session id for the preassigned-binding harnesses
/// (`pin_session`, mux_spawn.py:3472-3480): pi and footnote pin; the claude
/// pin lives with the claude arm (PR B); cursor-agent mints via create-chat.
fn pinned_uuid(harness: &str) -> Option<String> {
    match harness {
        "pi" | "footnote" => Some(
            crate::identity::mint_fno_id()
                .expect("OS CSPRNG unavailable for the pane session uuid"),
        ),
        _ => None,
    }
}

/// opencode (mux_spawn.py:1013): bare `opencode` is the TUI; the seed rides
/// `--prompt=<seed>` equal-form (yargs misparses the split form), the
/// per-harness default model pair, then passthrough. No grant: opencode's
/// `--add-dir` cell is not additive.
fn build_opencode(ctx: &PaneContext, seed: &str) -> Vec<String> {
    let contract =
        crate::harness_capabilities::HarnessContract::packaged().expect("embedded contract");
    let mut argv = contract
        .render_session_argv_raw("opencode", "interactive_create", None)
        .expect("embedded opencode interactive_create");
    if !seed.is_empty() {
        argv.push(format!("--prompt={seed}"));
    }
    argv.push("--model".to_string());
    argv.push(crate::opencode_ask::OPENCODE_DEFAULT_MODEL.to_string());
    argv.extend(grant_tokens("opencode"));
    argv.extend(ctx.passthrough.iter().cloned());
    argv
}

/// pi (mux_spawn.py:1037): the WATCHING lane's plain TUI, never `--mode rpc`.
/// The identity render carries the pinned `--session-id`; the route tokens
/// from the one pi route owner; a bare positional seed (pi offers no
/// equal-form and no `--` fence: probed on pi 0.84.2, `--` is not an
/// end-of-options marker there).
fn build_pi(ctx: &PaneContext, seed: &str, uuid: Option<&str>) -> Vec<String> {
    let contract =
        crate::harness_capabilities::HarnessContract::packaged().expect("embedded contract");
    let mut argv = contract
        .render_session_argv_raw("pi", "interactive_create", uuid)
        .expect("embedded pi interactive_create");
    let route = crate::pi::pi_route("", "", "", "");
    argv.extend(route.tokens);
    argv.extend(grant_tokens("pi"));
    if !seed.is_empty() {
        // argv-fence: exempt - pi takes the seed as a trailing positional
        // after its flag block; the python leg pushes it bare and the golden
        // pins the unfenced form (mux_spawn.py's pi branch).
        argv.push(seed.to_string());
    }
    argv.extend(ctx.passthrough.iter().cloned());
    argv
}

/// agy (mux_spawn.py:1000): the Rust owner's pane posture (agy_launch's
/// keeper_posture, lane pane), the state-root grant, then passthrough. No
/// seed in argv; the readiness gate types it.
fn build_agy(ctx: &PaneContext) -> Vec<String> {
    let posture = crate::agy_launch::keeper_posture("agy", "pane")
        .expect("the agy pane posture is in the vocabulary");
    let mut argv = vec!["agy".to_string()];
    argv.extend(posture.tokens);
    argv.extend(grant_tokens("agy"));
    argv.extend(ctx.passthrough.iter().cloned());
    argv
}

/// `_ensure_agy_folder_trust` (mux_spawn.py:2300): best-effort upsert of
/// agy's exact cwd key in its trust file. A parent already marked
/// TRUST_PARENT answers true without a write.
fn ensure_agy_folder_trust(cwd: &std::path::Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let trust_file = std::path::Path::new(&home)
        .join(".gemini")
        .join("trustedFolders.json");
    let key = cwd.to_string_lossy().to_string();
    let data: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&trust_file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if data.contains_key(&key) || data.keys().any(|k| is_trust_parent(k, cwd)) {
        return true;
    }
    let mut data = data;
    data.insert(key, serde_json::Value::String("TRUST_FOLDER".to_string()));
    crate::state::write_json_atomic(&trust_file, &data).is_ok()
}

/// A parent directory of cwd already carries TRUST_PARENT.
fn is_trust_parent(name: &str, cwd: &std::path::Path) -> bool {
    let path = std::path::Path::new(name);
    cwd.ancestors().skip(1).any(|a| a == path)
}

/// grok (mux_spawn.py:1094): the plain TUI; identity + grant? grok maps no
/// `--add-dir` cell, so no grant; the seed is typed after readiness.
fn build_grok(ctx: &PaneContext) -> Vec<String> {
    let contract =
        crate::harness_capabilities::HarnessContract::packaged().expect("embedded contract");
    let mut argv = contract
        .render_session_argv_raw("grok", "interactive_create", None)
        .expect("embedded grok interactive_create");
    argv.extend(grant_tokens("grok"));
    argv.extend(ctx.passthrough.iter().cloned());
    argv
}

/// footnote (mux_spawn.py:1077): the TUI takes one launch spec, not a
/// session flag. The model rides the passthrough fence and goes into the
/// spec; the spec file the Rust owner writes 0600 answers the argv tail.
fn build_footnote(ctx: &PaneContext, uuid: Option<&str>) -> Result<Vec<String>, PaneRefusal> {
    let mut model: Option<String> = None;
    let pass: Vec<String> = ctx.passthrough.to_vec();
    for (i, tok) in pass.iter().enumerate() {
        let next = pass.get(i + 1);
        if tok == "-m" || tok == "--model" {
            model = next.cloned();
            break;
        }
        if let Some(v) = tok.strip_prefix("--model=") {
            model = Some(v.to_string());
            break;
        }
    }
    let Some(model) = model else {
        return Err(PaneRefusal {
            line: FOOTNOTE_NO_MODEL.to_string(),
            exit_code: EXIT_REFUSAL,
            after_pane: None,
        });
    };
    let spec = crate::footnote_harness::write_pane_spec(&ctx.cwd, &model, None, None, uuid);
    match spec {
        Ok((path, _sid)) => {
            let contract = crate::harness_capabilities::HarnessContract::packaged()
                .expect("embedded contract");
            let mut argv = contract
                .render_session_argv_raw("footnote", "interactive_create", uuid)
                .expect("embedded footnote interactive_create");
            argv.push(path.display().to_string());
            Ok(argv)
        }
        Err(e) => Err(PaneRefusal {
            line: format!("footnote pane spec write failed: {e}"),
            exit_code: EXIT_SPAWN_FAILED,
            after_pane: None,
        }),
    }
}

/// The footnote no-model refusal, byte-equal to the golden
/// (footnote_python_leg.json cases.argv.no_model.line).
const FOOTNOTE_NO_MODEL: &str =
    "-H footnote needs a model: pass it after the fence, `-- -m <provider>/<model>`";

/// The plan for one spawn: the wrapped argv, the pinned session id and the
/// post-readiness seed policy. `pub(crate)` for the row-8 golden-parity
/// tests, which replay the committed captures through the same entry the
/// arm trait uses.
pub(crate) fn make_plan(arm: &SimpleArm, ctx: &PaneContext) -> Result<ArmPlan, PaneRefusal> {
    let seed = seed_text(ctx);
    let uuid = match arm.harness {
        "cursor-agent" => {
            Some(
                crate::cursor_agent::create_chat(&ctx.cwd).map_err(|e| PaneRefusal {
                    line: format!("cursor-agent create-chat could not mint a chat id: {e}"),
                    exit_code: EXIT_SPAWN_FAILED,
                    after_pane: None,
                })?,
            )
        }
        "pi" | "footnote" => pinned_uuid(arm.harness),
        _ => None,
    };
    let argv = match arm.harness {
        "opencode" => build_opencode(ctx, &seed),
        "pi" => build_pi(ctx, &seed, uuid.as_deref()),
        "cursor-agent" => {
            cursor_agent_refusal(ctx)?;
            build_cursor(ctx, uuid.as_deref())
        }
        "grok" => build_grok(ctx),
        "agy" => {
            ensure_agy_folder_trust(&ctx.cwd);
            build_agy(ctx)
        }
        "footnote" => build_footnote(ctx, uuid.as_deref())?,
        _ => unreachable!("no pane arm for {h}", h = arm.harness),
    };
    let typed = types_seed(arm.harness);
    let wrapped = {
        let seed_prov = seed_provenance_env(&seed);
        let qos_argv = qos_wrap_for(ctx, argv);
        // A --dispatch-account overlay rides the wrapper like a route does.
        let overlay: Vec<(String, String)> = ctx
            .account_env
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .chain(crate::accounts::launch::pane_stamp())
            .collect();
        mesh_env_wrapper(
            &ctx.worker_name,
            arm.harness,
            uuid.as_deref(),
            &seed_prov,
            &overlay,
            qos_argv,
        )
    };
    Ok(ArmPlan {
        wrapped,
        held_seed: (typed && !seed.is_empty()).then(|| seed.clone()),
        preassigned_id: uuid,
    })
}

impl PaneArm for SimpleArm {
    fn name(&self) -> &'static str {
        self.harness
    }

    fn prepare(&self, ctx: &PaneContext) -> Result<ArmPlan, PaneRefusal> {
        make_plan(self, ctx)
    }

    fn bind_and_seed(
        &self,
        ctx: &PaneContext,
        pane: &PaneFacts,
        plan: &ArmPlan,
    ) -> Result<ArmBind, PaneRefusal> {
        bind_simple(self, ctx, pane, plan)
    }
}

/// cursor-agent (mux_spawn.py:1063): the declared form joins the chat the
/// create-chat mint ran in make_plan; the worktree is fno's.
fn build_cursor(ctx: &PaneContext, uuid: Option<&str>) -> Vec<String> {
    let contract =
        crate::harness_capabilities::HarnessContract::packaged().expect("embedded contract");
    let mut argv = contract
        .render_session_argv_raw("cursor-agent", "interactive_create", uuid)
        .expect("embedded cursor-agent interactive_create");
    argv.extend(grant_tokens("cursor-agent"));
    argv.extend(ctx.passthrough.iter().cloned());
    argv
}

/// The post-pane failure carrier: the spine's refuse() reaps this pane and
/// appends the may-still-exist wording when its own reap misses. `pub(crate)`
/// for the claude arm, which fails the spawn on the same Python line.
pub(crate) fn spawn_failed_refusal(
    _ctx: &PaneContext,
    pane: &PaneFacts,
    head: String,
) -> PaneRefusal {
    PaneRefusal {
        line: format!(
            "{head}; pane {id} reaped, no registry row written",
            id = pane.pane_id
        ),
        exit_code: EXIT_SPAWN_FAILED,
        after_pane: Some(PaneFacts {
            session: pane.session.clone(),
            pane_id: pane.pane_id,
            child_pid: pane.child_pid,
            spawn_started_ms: pane.spawn_started_ms,
        }),
    }
}

/// The post-run bind for the simple arms: the readiness gate, the seed, then
/// the harness's bind answer.
/// The harnesses whose seed is typed into the pane after the readiness gate
/// (the rest commit it in argv). `make_plan` gates the held seed on it and
/// `seed_phase` gates the typed submit on it - one predicate, two readers.
fn types_seed(harness: &str) -> bool {
    matches!(harness, "agy" | "cursor-agent" | "grok" | "footnote")
}

fn bind_simple(
    arm: &SimpleArm,
    ctx: &PaneContext,
    pane: &PaneFacts,
    plan: &ArmPlan,
) -> Result<ArmBind, PaneRefusal> {
    let readiness =
        await_interactive_readiness(arm.harness, &pane.session, pane.pane_id, &super::FsMux);
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
    seed_phase(arm, ctx, pane, plan)
}

/// The seed phase: type the held seed when the arm types, retry once from
/// `unattempted`, and fail the spawn on `unconfirmed`.
fn seed_phase(
    arm: &SimpleArm,
    ctx: &PaneContext,
    pane: &PaneFacts,
    plan: &ArmPlan,
) -> Result<ArmBind, PaneRefusal> {
    if ctx.message.is_empty() || !types_seed(arm.harness) {
        // Argv seeds: committed at exec time; the frame read is skipped
        // (recorded narrowing: the Python argv-seed pass can send a bare
        // submit; the arm never types after an argv commit).
        return bind_answer(arm, ctx, pane, plan);
    }
    let seed = plan.held_seed.clone().unwrap_or_default();
    let first = submit_spawn_seed(
        arm.harness,
        &pane.session,
        pane.pane_id,
        &seed,
        false,
        &super::FsMux,
    );
    let mut outcome = first.outcome;
    if matches!(outcome, SeedOutcome::Unattempted { .. }) {
        std::thread::sleep(super::readiness::SEED_RETRY_DELAY);
        let second = submit_spawn_seed(
            arm.harness,
            &pane.session,
            pane.pane_id,
            &seed,
            false,
            &super::FsMux,
        );
        outcome = second.outcome;
    }
    match outcome {
        SeedOutcome::Submitted | SeedOutcome::Unknown { .. } => bind_answer(arm, ctx, pane, plan),
        SeedOutcome::Unconfirmed { detail } => Err(spawn_failed_refusal(
            ctx,
            pane,
            format!(
                "agent {name} {detail}",
                name = py_repr(&ctx.worker_name),
                detail = detail,
            ),
        )),
        SeedOutcome::Unattempted { .. } => bind_answer(arm, ctx, pane, plan),
    }
}

/// The bind answer per harness: preassigned ids bind immediately; opencode
/// backfills from its store; grok and agy bind no session at all.
fn bind_answer(
    arm: &SimpleArm,
    ctx: &PaneContext,
    pane: &PaneFacts,
    plan: &ArmPlan,
) -> Result<ArmBind, PaneRefusal> {
    match arm.harness {
        "pi" | "footnote" | "cursor-agent" => Ok(ArmBind::Bound {
            session_id: plan.preassigned_id.clone().unwrap_or_default(),
        }),
        "opencode" => Ok(opencode_backfill(&ctx.cwd, pane.spawn_started_ms)),
        "grok" | "agy" => Ok(ArmBind::NotApplicable),
        _ => Ok(ArmBind::Unbound {
            reason: "no spawn-time identity for this harness".to_string(),
        }),
    }
}

/// The opencode store backfill (port of `_backfill_opencode_session_id`,
/// mux_spawn.py:664): two attempts, 0.4s apart, one 5s-bounded `opencode db`
/// read per attempt. Exactly one `ses_` id for this cwd since the spawn
/// clock binds; two are ambiguous (a same-cwd race) and retrying cannot
/// narrow them; a miss leaves the row live-only.
fn opencode_backfill(cwd: &std::path::Path, since: i64) -> ArmBind {
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        if let Some(id) = query_opencode_sessions(cwd, since) {
            return ArmBind::Bound { session_id: id };
        }
    }
    ArmBind::Unbound {
        reason: "no unique opencode session for this cwd after spawn".to_string(),
    }
}

/// One 5s-bounded read-only `opencode db` query; stdout on exit 0, None
/// otherwise (`_run_opencode_db`, mux_spawn.py:604).
fn query_opencode_sessions(cwd: &std::path::Path, since_ms: i64) -> Option<String> {
    let escaped = cwd.to_string_lossy().replace('\'', "''");
    let sql = format!(
        "select id from session where directory='{escaped}' and time_created >= {since_ms}"
    );
    let mut child = std::process::Command::new("opencode")
        .arg("db")
        .arg(&sql)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    let mut stdout = String::new();
    use std::io::Read as _;
    child.stdout.take()?.read_to_string(&mut stdout).ok()?;
    let ids: Vec<String> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with("ses_")
                && l.len() > 4
                && l[4..].chars().all(|c| c.is_ascii_alphanumeric())
        })
        .map(str::to_string)
        .collect();
    match ids.len() {
        1 => Some(ids.into_iter().next().unwrap_or_default()),
        _ => None,
    }
}

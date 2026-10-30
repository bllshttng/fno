//! The agent.stop verb: the response the switchboard answers with, the
//! already-terminal no-op branch, and the stop claims release. Moved out
//! of daemon.rs under its file budget: the module owns one question -
//! what a stop writes and releases.

use super::*;

/// Daemon diagnostics in the locked `status-v1.json` shape (US6.10, LD35):
///
/// ```json
/// {
///   "schema_version": 1,
///   "daemon":   {"state", "pid", "uptime_secs", "version",
///                "exe_path", "exe_mtime", "exe_size", "pid_start_time"},
///   "agents":   {"total", "by_status": {"<status>": <count>, ...}},
///   "drives":   {"active": <controlling-driver count>},
///   "restarts": {"queue_depth", "consecutive_failures_max_seen"},
///   "channels": {"registered": <entries with an mcp_channel_id>}
/// }
/// ```
///
/// The shape is the contract Wave 7's `status-v1.json` schema + CI parity check
/// codify; keep additions backward-compatible. `daemon.state` is always
/// `serving` here because a served RPC implies the daemon got past recovery.
///
/// `agents.by_status` is a histogram of the STORED lifecycle enum -- what was
/// last WRITTEN to each registry row -- and NOT a reachability census. It will
/// not match `fno agents list`, and that is correct rather than a bug: they
/// answer different questions. Because the daemon reconciles once at startup,
/// these values can be stale for its entire uptime, so an `exited` here means
/// "we recorded exited at some point", not "unreachable now". For reachability,
/// read `fno agents list` (its `reachability` + `basis` fields), which derives
/// from `cli/src/fno/agents/reachability.py`.
///
/// Deliberately NOT renamed to say so: the field name is pinned by the schema
/// and its CI parity check, and a breaking rename would buy wording alone.

pub(super) async fn handle_stop(ctx: &Ctx, req: &Request) -> Response {
    let mut response = stop_body(ctx, req).await;
    attach_stopped_claims_release(ctx, req, "stop", &mut response).await;
    response
}

async fn stop_body(ctx: &Ctx, req: &Request) -> Response {
    let requested_name = match req.params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return Response::err(req.id, ErrorCode::InvalidParams, "missing `name`"),
    };
    let registry = match load_registry_offloaded(ctx.home.registry_json()).await {
        Ok(r) => r,
        Err(e) => return registry_read_failed(req.id, e),
    };
    let entry = match entry_for_lifecycle(
        &registry,
        &requested_name,
        &ctx.home.registry_json(),
        true,
    )
    .await
    {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            return Response::err(
                req.id,
                ErrorCode::AgentNotFound,
                format!("agent {requested_name} not found"),
            )
        }
        Err(message) => return Response::err(req.id, ErrorCode::InvalidParams, message),
    };
    let name = entry.name.clone();
    if entry.status == AgentStatus::Exited {
        // An exited agent needs no stop work. (Pre-G4 this also force-cleared a
        // lingering WebSocket driver; the drive surface was retired at G4.)
        // The stop still journals: the claims release treats this response as
        // a confirmed stop, and quiet recovery must agree, or a lead's stop
        // of an already-terminal row leaves no evidence and the resume
        // ladder revives the worker it stood down on every pass.
        let _ = ctx.emitter.emit(
            "agent_stopped",
            &json!({"name": name, "reason": "already_exited"}),
        );
        return Response::ok(
            req.id,
            json!({"already_exited": true, "short_id": entry.short_id}),
        );
    }
    // A pane-hosted row's ONE live ref is the mux pane (refusal text in
    // stop_refusal_detail). Above the claude branch on purpose: stop_claude's
    // fallback signals the recorded pid and leaves the pane.
    if let Some(mux) = entry.mux.as_ref() {
        let (probe, precheck) = stop_refusal_detail::pane_verdict(&entry);
        return Response::err(
            req.id,
            ErrorCode::InvalidParams,
            stop_refusal_detail::pane_row_refusal(
                &name,
                &mux.session,
                mux.pane_id,
                probe,
                precheck.as_ref(),
            ),
        );
    }
    // Claude agents are not PTY-managed (LD8): there is no worker to shut down.
    // Shell out to the claude supervisor and propagate its outcome.
    if entry.harness_name() == "claude" {
        return stop_claude(ctx, req, &name, &entry).await;
    }
    if is_codex_thread_entry(&entry) {
        // Stop means INTERRUPT the in-flight turn, then DROP the actor
        // (closing its connection to the shared daemon), and only then stamp
        // Exited.
        //
        // The interrupt IS the stop now. There is no child to kill: the
        // shared daemon owns the thread, so a turn that survives the bounded
        // settle keeps running there, and the report below says exactly that.
        let interrupt_report = match rm_teardown::end_codex_thread_confirmed(ctx, &entry).await {
            Ok(report) => report,
            // Keep the handle and leave the row non-terminal. The actor still
            // holds the interrupt handle for the live turn, so a retry can
            // reach it, and a terminal row would also make the thread
            // invisible to `codex_thread_recovery_candidate`.
            Err(interrupt_report) => {
                let _ = ctx.emitter.emit(
                    "agent_stop_refused",
                    &json!({"name": name, "backend": "codex-thread", "interrupt": interrupt_report}),
                );
                return Response::ok(
                    req.id,
                    json!({
                        "stopped": false,
                        "backend": "codex-thread",
                        "interrupt": interrupt_report,
                    }),
                );
            }
        };
        let stop_name = name.clone();
        let wake_home = ctx.home.clone();
        if let Err(error) = update_registry_offloaded(ctx.home.registry_json(), move |registry| {
            if let Some(entry) = registry.find_mut(&stop_name) {
                entry.status = AgentStatus::Exited;
                entry.exited_at = Some(now_rfc3339_like());
                crate::state::record_stop(entry, "stop-command", Some("codex-thread".into()));
                // best-effort: a failed stamp costs a later wake its name,
                // never the stop.
                crate::wake_name::stamp_from_row(&wake_home, entry);
            }
        })
        .await
        {
            return Response::err(
                req.id,
                state_error_code(&error),
                format!("codex thread {name} stopped but registry write failed: {error}"),
            );
        }
        let _ = ctx.emitter.emit(
            "agent_stopped",
            &json!({"name": name, "backend": "codex-thread", "interrupt": interrupt_report}),
        );
        return Response::ok(
            req.id,
            json!({
                "stopped": true,
                "backend": "codex-thread",
                "interrupt": interrupt_report,
            }),
        );
    }
    // A lane-B keeper thread: its short_id is empty, so the no-op arm below
    // would report a stop that stopped nothing. Kill goes over the row's own
    // socket and is CONFIRMED before the row goes terminal.
    if let Some(sock) = keeper_thread_sock(&entry) {
        return stop_keeper_row(ctx, req, &name, &entry, &sock).await;
    }
    // An empty short_id (Python-authored; the daemon's create path always
    // derives one) is a codex/gemini row with no daemon worker. Mirror Python
    // `stop_agent`: emit `agent_stopped` and leave the registry UNCHANGED. The
    // PTY path below would write `status = Exited`, which Python's loader
    // rejects (Codex P1, PR #364).
    if entry.short_id.is_empty() {
        let _ = ctx.emitter.emit(
            "agent_stopped",
            &json!({"name": name, "provider": entry.harness_name(), "claude_exit": Value::Null}),
        );
        return Response::ok(
            req.id,
            json!({"stopped": true, "provider": entry.harness_name(), "no_op": true}),
        );
    }
    // Ask the worker to shut down its PTY child, then CONFIRM it went away: a
    // swallowed failure would mark the agent exited while the PTY keeps running.
    if !stop_worker_confirmed(ctx, &entry).await {
        return Response::err(
            req.id,
            ErrorCode::Internal,
            format!("agent {name}: worker did not confirm shutdown; it may still be running"),
        );
    }
    // Surface a registry-write failure rather than reporting a clean stop
    // while the on-disk status still reads live.
    let stop_name = name.clone();
    let wake_home = ctx.home.clone();
    if let Err(e) = update_registry_offloaded(ctx.home.registry_json(), move |r| {
        if let Some(e) = r.find_mut(&stop_name) {
            e.status = AgentStatus::Exited;
            crate::state::record_stop(e, "stop-command", None);
            // best-effort: a failed stamp costs a later wake its name,
            // never the stop.
            crate::wake_name::stamp_from_row(&wake_home, e);
        }
    })
    .await
    {
        let _ = ctx.emitter.emit(
            "agent_stop_error",
            &json!({"name": name, "error": e.to_string()}),
        );
        return Response::err(
            req.id,
            state_error_code(&e),
            format!("agent {name}: worker stopped but registry write failed: {e}"),
        );
    }
    let _ = ctx.emitter.emit("agent_stopped", &json!({"name": name}));
    Response::ok(req.id, json!({"stopped": true, "short_id": entry.short_id}))
}

/// One release per confirmed stop/rm command (change 5). Resolves the
/// stopped row's identity, scans the global claims dir and the row's own
/// space claims dir, releases the provably-dead claims the stopped holder
/// keeps, emits ONE audit event, and rides the receipt on the response under
/// `claims`. A `stopped: false` response (a codex interrupt that did not
/// settle) releases nothing and carries no `claims` key.
pub(super) async fn attach_stopped_claims_release(
    ctx: &Ctx,
    req: &Request,
    command: &str,
    response: &mut Response,
) {
    let confirmed = match response.result() {
        Some(result) => {
            // A no-op stop (a synchronous provider with nothing to stop)
            // reports stopped:true but stopped nothing, so it releases
            // nothing - the same contract the Python no-op arm holds.
            !matches!(result.get("no_op"), Some(Value::Bool(true)))
                && (matches!(result.get("stopped"), Some(Value::Bool(true)))
                    || matches!(result.get("already_exited"), Some(Value::Bool(true)))
                    || matches!(result.get("removed"), Some(Value::Bool(true))))
        }
        None => false,
    };
    if !confirmed {
        return;
    }
    let Some(name) = req.params.get("name").and_then(Value::as_str) else {
        return;
    };
    // A stop leaves the row (terminal); an rm removes it, so the caller
    // passes the resolved identity instead of re-reading the registry. The
    // requested token may be a session id rather than the row name (a
    // stop-by-id, or a cross-project heal that just minted the row), so
    // resolve through the same finder the lifecycle commands resolve with; an
    // exact-name miss here dropped the healed row's claims on the floor.
    let identity = load_registry_offloaded(ctx.home.registry_json())
        .await
        .ok()
        .and_then(|registry| {
            registry.find_name_or_full_session_id(name).map(|e| {
                (
                    Some(e.name.clone()),
                    e.harness_session_id.clone(),
                    Some(e.cwd.clone()),
                )
            })
        });
    let (resolved_name, session_id, cwd) = identity.unwrap_or((None, None, None));
    let name: &str = resolved_name.as_deref().unwrap_or(name);
    release_stopped_claims_into(&ctx.emitter, name, session_id, cwd, command, response);
}

/// The synchronous core of the release: resolve dirs, run
/// [`crate::claims::release_for_stopped_session`], emit the event, insert the
/// receipt under `claims`. Shared by the stop wrapper and handle_rm_with's
/// tail (where the row is already gone from the registry).
pub(super) fn release_stopped_claims_into(
    emitter: &EventEmitter,
    name: &str,
    session_id: Option<String>,
    cwd: Option<String>,
    command: &str,
    response: &mut Response,
) {
    let Some(result) = response.result_mut().and_then(|r| r.as_object_mut()) else {
        return;
    };
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Some(global) = crate::claims::global_claims_dir() {
        dirs.push(global);
    }
    if let Some(space_claims) = cwd
        .as_deref()
        .filter(|c| !c.is_empty())
        .and_then(|cwd| crate::paths::space_dir_opt(std::path::Path::new(cwd)))
        .map(|dir| dir.join("claims"))
        .filter(|dir| !dirs.contains(dir))
    {
        dirs.push(space_claims);
    }
    if dirs.is_empty() {
        return;
    }
    let target = crate::claims::StoppedHolder {
        name: name.to_string(),
        harness_session_id: session_id.clone(),
    };
    let receipt = match crate::claims::release_for_stopped_session(&target, &dirs, None) {
        Ok(receipt) => receipt,
        Err(error) => {
            let _ = emitter.emit(
                "agent_stop_claims_released",
                &json!({"name": name, "verb": command, "error": error}),
            );
            return;
        }
    };
    let _ = emitter.emit(
        "agent_stop_claims_released",
        &json!({
            "name": name,
            "session_id": session_id.clone(),
            "verb": command,
            "released": receipt.released,
            "kept": receipt.kept,
        }),
    );
    result.insert(
        "claims".into(),
        serde_json::to_value(&receipt).unwrap_or(Value::Null),
    );
}

//! The `agent.list` entry point and its attention ordering, extracted from
//! the parent under the file-budget gate: the list projection's change
//! (the `substrate` key) moved with the code it touched.

use super::*;

/// The attention window this surface orders by. Session-truth's stall window
/// is 7200s and correct FOR REAPING; for display it is exactly the gap a
/// dead-under-two-hours worker hides in, so the ordering window is ten
/// minutes. Mirrors `session_truth.STALE_ATTENTION_S` and the mux client's
/// constant - the three cannot share code (the crates do not link), so the
/// shared fixture in schemas/ is what pins them together.
pub(crate) const STALE_ATTENTION_S: f64 = 600.0;

const LIST_PROJECTION_OMISSIONS: [&str; 0] = [];

/// `agent.list`, with the truth probe injected as a BATCH seam: one call for
/// the whole filtered page, keyed by handle. The per-row seam it replaced spent
/// one Python interpreter cold start per row per list.
pub(super) fn handle_list_with_truth<F>(ctx: &Ctx, req: &Request, truth_fn: F) -> Response
where
    F: Fn(
        &[String],
    ) -> (
        std::collections::HashMap<String, crate::truth_probe::TruthProbe>,
        crate::truth_probe::BatchOutcome,
    ),
{
    let all = req
        .params
        .get("all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Task 3.1: accept cwd/provider/status/progress filters matching Python list_agents.
    // Legacy project_root filter still accepted for backward compat.
    let filter_cwd = req
        .params
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(String::from);
    let filter_provider = req
        .params
        .get("provider")
        .and_then(|v| v.as_str())
        .map(String::from);
    let filter_status = req
        .params
        .get("status")
        .and_then(|v| v.as_str())
        .map(String::from);
    let filter_progress = req
        .params
        .get("progress")
        .and_then(|v| v.as_str())
        .map(String::from);
    let cwd_project = req
        .params
        .get("project_root")
        .and_then(|v| v.as_str())
        .map(String::from);

    // Reject an invalid --status up front so a typo fails fast with exit 13
    // instead of silently returning zero rows + exit 0 (Codex P2 on PR #361).
    // Mirrors Python's AgentStatusFilter enum, which Typer
    // rejects at parse time.
    if let Some(ref st) = filter_status {
        if !matches!(
            st.as_str(),
            "writing" | "quiet" | "parked" | "orphaned" | "refused" | "unknown"
        ) {
            return Response::err(
                req.id,
                ErrorCode::InvalidStatus,
                format!(
                    "invalid --status '{st}' (status is served activity: writing | quiet | parked | orphaned | refused | unknown; process liveness is the liveness field on fno agents list --json)"
                ),
            );
        }
    }
    if let Some(ref progress) = filter_progress {
        if !matches!(
            progress.as_str(),
            "advancing" | "awaiting-operator" | "parked" | "refused" | "unknown"
        ) {
            return Response::err(
                req.id,
                ErrorCode::InvalidStatus,
                format!(
                    "invalid --progress '{progress}' (expected: advancing | awaiting-operator | parked | refused | unknown)"
                ),
            );
        }
    }
    // Normalize the cwd filter so equivalent paths (`.` vs absolute, symlinks)
    // match, mirroring Python's `Path(cwd).resolve()` before filtering (Codex P2
    // on PR #361; this is the cwd half of cv-eeaad75d). canonicalize requires the
    // path to exist; fall back to the raw string when it can't resolve so a
    // non-existent filter still does an exact-string match rather than erroring.
    let norm_path = |p: &str| -> String {
        std::fs::canonicalize(p)
            .ok()
            .and_then(|pb| pb.to_str().map(String::from))
            .unwrap_or_else(|| p.to_string())
    };
    let filter_cwd_norm = filter_cwd.as_deref().map(&norm_path);

    // an unreadable registry is an RPC error, never a valid empty
    // roster with discovered-only rows beside it. `unwrap_or_default()` here is
    // what let a broken registered lane publish `count: 0` next to a healthy
    // `discovered_count` and read as "no agents". This handler is sync, so it
    // takes the asserted blocking read inline.
    let registry = match load_registry_asserted(&ctx.home.registry_json()) {
        Ok(reg) => reg,
        Err(e) => return registry_read_failed(req.id, e),
    };
    let filtered: Vec<_> = registry
        .entries
        .iter()
        .filter(|e| {
            if !all {
                if let Some(ref p) = cwd_project {
                    if &e.project_root != p {
                        return false;
                    }
                }
            }
            if let Some(ref cwd) = filter_cwd_norm {
                if &norm_path(&e.cwd) != cwd {
                    return false;
                }
            }
            if let Some(ref prov) = filter_provider {
                // The provider filter reads the v15+ vendor axis, never the
                // harness: comparing harness_name() here dropped a worker
                // hosted on one CLI and routed to another vendor.
                if e.provider.as_deref() != Some(prov.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    // ONE probe call for the whole page. This handler renders `state`,
    // `observed_model` and the reachability triple into every row, so it needs
    // a real reading per row and no stat can stand in for one: a grown
    // transcript means the tail CHANGED, which makes the last reading stale
    // rather than confirmed. Batching is the whole win here, and it is enough -
    // 24 rows cost 18.7 s as per-row subprocesses and 0.8 s as one.
    //
    // Duplicate handles across rows collapse in the request and fan back out on
    // read: free deduplication the per-row path never had.
    let handles: Vec<String> = {
        let mut seen = std::collections::BTreeSet::new();
        filtered
            .iter()
            .map(|e| registry_truth_handle(e))
            .filter(|h| seen.insert(h.clone()))
            .collect()
    };
    let (truths, batch_outcome) = truth_fn(&handles);
    // The instrument's own receipt: a page where the probe answered
    // nothing must be readable AS that, not as 43 rows confidently `unknown`.
    // The rendered status word cannot carry the distinction (the vocabulary is
    // frozen by the --status filter), so the envelope does.
    let truth_probe_asked = handles.len();
    let truth_probe_answered = handles.iter().filter(|h| truths.contains_key(*h)).count();
    let node_prs = crate::list_row::node_primary_prs(
        &crate::feed::graph_path(&ctx.home),
        filtered.iter().filter_map(|e| e.node.as_deref()),
    );
    let classified: Vec<_> = filtered
        .into_iter()
        .map(|e| {
            // A handle absent from the batch behaves exactly as a `None` probe
            // did on the per-row path: every reader below already treats an
            // unanswered row that way.
            let truth = truths.get(&registry_truth_handle(e)).cloned();
            let rendered_status = rendered_status_from_truth(truth.as_ref());
            // The whole reachability triple, not just the verdict that
            // `rendered_status` above was picked from. That rendered word says
            // WHAT the row is; the triple says which question was answered and
            // off what evidence, and only the triple separates a positive
            // transcript reading from a fired falsifier. Null on a `fno` too old
            // to emit them: a stale probe that did not answer must read as
            // absent, never as no-evidence. Both basis legs are worded, never
            // blank-on-a-guess: `basis_word_from_truth` decides between an
            // absent reading and a page the batch never measured, and
            // `activity_basis_from_truth` names the age's instrument beside
            // it.
            let basis_word = basis_word_from_truth(truth.as_ref(), batch_outcome);
            let activity_basis = activity_basis_from_truth(truth.as_ref(), batch_outcome);
            let evidence = (
                json!(truth.as_ref().and_then(|t| t.reachability.as_deref())),
                basis_word,
                json!(truth.as_ref().and_then(|t| t.last_activity_age_s)),
                json!(truth.as_ref().and_then(|t| t.last_event_at.as_deref())),
                json!(truth.as_ref().and_then(|t| t.last_message.as_deref())),
                activity_basis,
            );
            // The orthogonal axis: reachability answers "can I reach this
            // process"; progress answers "is it advancing, awaiting the
            // operator, parked, or refused" -- read off the SAME probe, so a
            // refused-but-reachable row is never rendered as a fourth
            // reachability value.
            let (progress, progress_basis) = progress_from_truth(
                truth.as_ref(),
                batch_outcome,
                e.harness_name(),
                e.route_settings_path.as_deref(),
            );
            // A probe that did not answer is the same situation Python's
            // resolver reports as `no-transcript` (its dominant cause here is
            // the routine exit-13 miss), so both emitters say the same thing
            // about the same row instead of one of them inventing a null.
            let observed_model = truth
                .map(|t| t.observed_model)
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| json!({"kind": "no-transcript"}));
            (
                e,
                rendered_status,
                observed_model,
                evidence,
                progress,
                progress_basis,
            )
        })
        .collect();
    let mut entries: Vec<Value> = classified
        .into_iter()
        .filter(
            |(_e, rendered_status, _observed, _evidence, progress, _progress_basis)| {
                if let Some(ref st) = filter_status {
                    if rendered_status != &st.as_str() {
                        return false;
                    }
                }
                if let Some(ref want) = filter_progress {
                    if progress != want {
                        return false;
                    }
                }
                true
            },
        )
        .map(
            |(e, rendered_status, observed_model, evidence, progress, progress_basis)| {
                let (
                    reachability,
                    basis,
                    last_activity_age_s,
                    last_event_at,
                    last_message,
                    last_activity_basis,
                ) = evidence;
                // Return the full row shape matching Python's serialize_entry. The
                // key set is pinned by schemas/agents-list-row.json, asserted here
                // and by the Python test; edit that file before adding a key.
                //
                // live_status remains null because the daemon does not duplicate the
                // harness supervisor view. `status`, however, is the family-1
                // transcript verdict attached above; stored registry status is only
                // lifecycle metadata and cannot prove read-side liveness or death.
                //
                // session_id is the provider-specific resume id (short_id for
                // claude, codex_session_id for codex, gemini_session_id for
                // gemini), falling back to the generic `session_id` so a row with
                // only the generic id set still reports a resume key.
                let resume_id: Option<String> = match e.harness_name() {
                    "claude" => e
                        .transport_short()
                        .map(str::to_string)
                        .or_else(|| e.session_id.clone()),
                    "codex" => e.codex_session_id.clone().or_else(|| e.session_id.clone()),
                    "gemini" => e.gemini_session_id.clone().or_else(|| e.session_id.clone()),
                    // Python writes opencode ids to the canonical harness_session_id
                    // and drops `session_id` on write (it is Rust-set only), so
                    // falling through would report null for every opencode row. Same
                    // resolution as `to_agent_entry` and `client_commands::resume_session_id`.
                    "opencode" => e
                        .harness_session_id
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| e.session_id.clone()),
                    _ => e.session_id.clone(),
                };
                let session_id: Value = resume_id.map(Value::String).unwrap_or(Value::Null);
                let short_id: Value = e
                    .transport_short()
                    .map(|s| Value::String(s.to_string()))
                    .unwrap_or(Value::Null);
                let (pr, pr_basis) = crate::list_row::pr_pair(e.node.as_deref(), node_prs.as_ref());
                let log_path: Value = e
                    .log_path
                    .as_deref()
                    .map(|s| Value::String(s.to_string()))
                    .unwrap_or(Value::Null);
                // ADDRESS is the row's fno handle (codex rows; others keep
                // the harness head), and Claude's transport key is the
                // last-resort fallback.
                let address: Value = row_address(e)
                    .map(Value::String)
                    .or_else(|| {
                        if e.harness_name() == "claude" {
                            e.transport_short().map(|s| Value::String(s.to_string()))
                        } else {
                            None
                        }
                    })
                    .unwrap_or(Value::Null);
                // Same formatter as Python's `AgentEntry.team_label`, so the two
                // surfaces render an identical descriptor for the same row. Python
                // tests the scope for falsiness (`self.role_scope or '?'`), so the
                // empty string has to fall back here too, not just None.
                let team: Value = match e.role_level {
                    Some(level) => Value::String(format!(
                        "L{level} {}",
                        e.role_scope
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("?")
                    )),
                    None => Value::Null,
                };
                let mut row = json!({
                    "name": e.name,
                    // `harness` is the sole identity axis, and it names the CLI,
                    // never the model vendor. `provider` beside it is the v15+
                    // model-vendor axis stamped at spawn; the pre-split alias that
                    // carried the harness value under this name stayed omitted
                    // until, which hid the real vendor axis from every
                    // RPC consumer. `observed_model` below remains the honest
                    // answer to what actually answered.
                    "harness": e.harness_name(),
                    "provider": e.provider,
                    // Stored effort is a separate spawn axis. It is passed
                    // through unchanged; observed_model remains transcript truth.
                    "effort": e.effort,
                    "harness_session_id": e.harness_session_id,
                    // The two identity axes plus classified lineage,
                    // mirroring Python's serialize_entry: `thread_id` is the
                    // stable fno identity, `current_session_id` the address
                    // delivery follows now, and the predecessor chain / fork
                    // edge are the retained history. Emitted as separate keys
                    // so a renderer cannot present a retired id as current.
                    "thread_id": e.fno_id,
                    "current_session_id": e.harness_session_id,
                    // `node_reason` beside `node` is the sibling receipt:
                    // why a mint could not bind the node the seed named.
                    // `node_basis` beside it names the non-mint writer that
                    // did bind one (the first-turn hook); absence reads as
                    // spawn/target work.
                    "node": e.node,
                    "node_reason": e.node_reason,
                    "node_basis": e.node_basis,
                    "pr": pr,
                    "pr_basis": pr_basis,
                    "predecessor_session_ids": e.predecessor_session_ids,
                    "forked_from_session_id": e.forked_from_session_id,
                    "short_id": short_id,
                    "session_id": session_id,
                    "address": address,
                    "cwd": e.cwd,
                    "created_at": e.created_at,
                    "last_message_at": e.last_message_at,
                    "last_message_at_basis": null,
                    "last_reconciled_at": e.last_reconciled_at,
                    // The SERVED liveness triple: word, stamp, basis; the freshness
                    // rule is served_liveness.rs, mirrored per crate, pinned by contract test.
                    "liveness": served_fresh_liveness(
                        e.liveness.as_deref(),
                        e.liveness_measured_at.as_deref(),
                    ),
                    "liveness_basis": served_liveness_basis(
                        e.liveness.as_deref(),
                        e.liveness_measured_at.as_deref(),
                    ),
                    "liveness_measured_at": e.liveness_measured_at,
                    "mail_unread": e.mail_unread,
                    // The harness's own title for the session, served
                    // from the probe's fresh reading; a probe that ANSWERED
                    // None is trusted (the harness carries no title now, e.g.
                    // a rotated transcript), and the sweep's stored last-seen
                    // value stands only for a row the batch never measured.
                    // Beside `name`, never in it: the label is fno's, the
                    // title is the harness's.
                    "harness_title": truths
                        .get(&registry_truth_handle(e))
                        .map(|t| t.harness_title.clone())
                        .unwrap_or_else(|| e.harness_title.clone()),
                    "status": rendered_status,
                    // The reachability triple, from the same probe the rendered
                    // word above came from. `fno agents list` is where `peek` and
                    // the census helpers below send a reader for this evidence, and
                    // the default `list` is THIS projection whenever an installed
                    // binary is present -- so emitting it Python-side only left the
                    // documented field missing on the path readers actually take.
                    "reachability": reachability,
                    "basis": basis,
                    // The orthogonal progress axis, from the same probe as the
                    // reachability triple above (fno.agents.reachability.classify_progress).
                    "progress": progress,
                    "progress_basis": progress_basis,
                    "last_activity_age_s": last_activity_age_s,
                    // The instrument the age came from (`last-entry` |
                    // `mtime` | `opencode-db`), the resolver's reason word
                    // (`not-found` | `no-records` | `resolver-error`) when it
                    // could not resolve the handle, or `unmeasured` when the
                    // batch never ran for this page - never a bare null: the
                    // three unknown-reason words are the difference between
                    // "no transcript" and "the resolver crashed".
                    "last_activity_basis": last_activity_basis,
                    // The absolute stamp of the newest transcript activity and the
                    // flattened LAST-turn text, from the same probe as the age -
                    // the pair that makes a wedged-but-`working` row visible. Null
                    // when the probe never answered, which an absent reading must
                    // render as, never a fresh one.
                    "last_event_at": last_event_at,
                    "last_message": last_message,
                    "live_status": null,
                    // The model this worker is ACTUALLY answering as, from the same
                    // family-1 probe that produced `status` above -- so the daemon
                    // never grows a second transcript reader that could disagree
                    // with the truth command about the same session.
                    "observed_model": observed_model,
                    // The stored request with its basis, beside the observation.
                    // A null substitution marker is match-or-unknown, never clean.
                    "model": e.model,
                    "model_basis": e.model_basis,
                    "requested_model": e.requested_model,
                    "model_substituted": model_substitution_marker(
                        e.requested_model.as_deref(),
                        &observed_model,
                    ),
                    "pid": e.pid,
                    "pid_start_time": e.pid_start_time,
                    "log_path": log_path,
                    // The mux hosting ref ({session, pane_id}) for a pane-hosted row,
                    // else null. A pane row's short_id is empty, so this is the only
                    // key that says where such a worker actually lives; without it a
                    // caller reads a bound pane worker as unhosted.
                    "mux": e.mux,
                    // The lane the row was spawned on, read from the
                    // registry record. Never inferred from `mux` or
                    // `thread_id`: a paneless pane row and a thread row would
                    // then read identically, which is the confusion a reader
                    // cannot recover from.
                    "substrate": e.substrate,
                    // Team (US9): the compact descriptor plus the raw fields, so a
                    // minion can resolve who to escalate to.
                    "role": team,
                    "role_level": e.role_level,
                    "role_scope": e.role_scope,
                    "role_grantor": e.role_grantor,
                    // The parent edge the orphan check keys on; null is a real answer.
                    "spawned_by_session": e.spawned_by_session,
                    // The served CHILD/PEER word; null before the first stamp.
                    "lineage_kind": e.lineage_kind,
                    // How this session came to exist: "operator" for one a human
                    // started by hand, "spawn" for a footnote-created worker, null
                    // for a row nothing stamped. Emitted on BOTH serializers because
                    // `fno agents list` auto-routes to this projection whenever an
                    // installed binary is present, so a Python-only key would be
                    // missing from the path nearly every reader takes.
                    "origin": e.origin,
                    // the row's mail delivery policy ("bus-only" holds
                    // mail on the durable bus, null is the injectable default).
                    // Stored since v14, read by every injector gate, and until
                    // now never rendered anywhere a human or a lead could see
                    // it. The remaining time on a timed hold is Python-only
                    // (`dnd` in schemas/agents-list-row.json): its clock lives
                    // under fno's config-resolved state dir, which the daemon
                    // does not load.
                    "delivery_policy": e.delivery_policy,
                    "project_root": e.project_root,
                });
                if let Some(object) = row.as_object_mut() {
                    for (key, value) in crate::model_price::served_session_metrics_keys(e) {
                        object.insert(key, value);
                    }
                    // No `pid_alive` injection here: this row's `status` is
                    // `rendered_status`, which `rendered_status_from_truth`
                    // draws from a closed set of live/orphaned/unknown, so the
                    // stale-spawning rule cannot match it. Injecting the input
                    // would be a guard that never fires, reading as coverage.
                    // The rule's one live carrier is Python's
                    // `spawn_gate.census` (`fno agents top`), which measures
                    // liveness itself and renders the stored token.
                    apply_row_contradiction(object, e.exited_at.as_deref(), chrono::Utc::now());
                    object.remove("pid_start_time");
                }
                row
            },
        )
        .collect();
    // Attention order: evidence of neglect first, the same order the mux
    // table and the Python list lane apply (all three assert against one
    // shared fixture). Registry insertion order said nothing about who needs
    // the operator; the row's own `status` word is a low-pass filter that
    // reads `live` for a worker dead under two hours, so it is barred from
    // the key. The needs-me fold rank does not ride this surface (it is a
    // mux-client concept); rows sort on their evidence alone.
    entries.sort_by(|a, b| attention_sort_key(a).cmp(&attention_sort_key(b)));
    // Echo the filters the daemon applied so `list --json` self-describes its
    // query, matching Python `read.list_agents`'s `filters_applied` (sigma-review:
    // the client previously always fell back to an all-null block because the
    // daemon omitted this field). `cwd` is the value the client sent; absolute
    // resolution to match Python's `Path(cwd).resolve()` is deferred (cv-eeaad75d).
    let filters_applied = json!({
        "cwd": filter_cwd_norm,
        "provider": filter_provider,
        "status": filter_status,
        "progress": filter_progress,
    });
    list_response(
        req,
        entries,
        filters_applied,
        truth_probe_asked,
        truth_probe_answered,
        all,
        &ctx.home,
    )
}

pub(super) fn handle_list(ctx: &Ctx, req: &Request) -> Response {
    handle_list_with_truth(
        ctx,
        req,
        crate::truth_probe::family1_truth_probe_many_measured,
    )
}

/// The `agent.list` response tail: the sorted entries, the echoed filters,
/// the truth-probe counters, and the `--all` provenance lane. The payload
/// key is always present so a consumer can tell "no retired rows" from an
/// older shape, the way the discovered lane is.
pub(super) fn list_response(
    req: &Request,
    entries: Vec<Value>,
    filters_applied: Value,
    truth_probe_asked: usize,
    truth_probe_answered: usize,
    all: bool,
    home: &AgentsHome,
) -> Response {
    let retired_sessions = if all { retired_rows(home) } else { Vec::new() };
    Response::ok(
        req.id,
        json!({
            "agents": entries,
            "filters_applied": filters_applied,
            "fields_omitted": LIST_PROJECTION_OMISSIONS,
            "truth_probe_asked": truth_probe_asked,
            "truth_probe_answered": truth_probe_answered,
            "retired_sessions": retired_sessions,
            "retired_count": retired_sessions.len(),
        }),
    )
}

/// The reaped and REMOVED sessions `--all` shows: every removal the shared
/// fold recovers, newest first. A reaped row leaves the registry, so a
/// wrongful reap is invisible without this lane; the receipt is the record
/// that the row existed, and the event names WHY it left. A receipt-less
/// removal (a never-bound row the receipt builder refused) shows from its
/// `registry_row_removed` event, with `state: "removed"` and honestly null
/// session/resume.
pub(super) fn retired_rows(home: &AgentsHome) -> Vec<Value> {
    let (removals, _notes) = crate::removals::read(home, None);
    let mut out: Vec<Value> = removals
        .into_iter()
        .map(|r| {
            json!({
                "name": r.name,
                "harness": r.harness,
                "session_id": r.session_id,
                "short_id": r.short_id,
                "cwd": r.cwd,
                "state": if r.receipt.is_some() { "reaped" } else { "removed" },
                "reaped_at": r.ts,
                "cause": r.cause.unwrap_or_else(|| "not recorded".into()),
                "cause_at": r.cause_at.unwrap_or_else(|| "not recorded".into()),
                "basis": r.reason.unwrap_or_else(|| "not recorded".into()),
                "node": r.node,
                "resume": r.resume,
                "receipt": r.receipt.map(|p| p.display().to_string()),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let key = |v: &Value| {
            v.get("reaped_at")
                .and_then(Value::as_str)
                .map(crate::feed::ts_key)
                .unwrap_or((0, 0))
        };
        key(b).cmp(&key(a))
    });
    out
}

/// One row's list-lane attention key: evidence tier, then longest-silent
/// first, then name so consecutive lists never shuffle equal rows. Only
/// fields that carry their evidence with them (`basis`,
/// `last_activity_age_s`) - never `status`, never a bare verdict. A row with
/// no probe answer (all three null) lands in the neutral tier with age 0:
/// absence of a reading is not urgency.
/// `to_bits` is order-preserving for non-negative f64 (and an age is a
/// duration, always non-negative), which is what lets a float age ride an
/// `Ord` tuple key.
pub(super) fn attention_sort_key(row: &Value) -> (u8, std::cmp::Reverse<u64>, String) {
    let basis = row.get("basis").and_then(|v| v.as_str());
    let age = row
        .get("last_activity_age_s")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let tier = if matches!(basis, Some("process-gone") | Some("pane-gone"))
        || row.get("reachability").and_then(|v| v.as_str()) == Some("unreachable")
    {
        5
    } else if basis == Some("transcript") && age >= STALE_ATTENTION_S {
        0
    } else if basis == Some("silent") {
        1
    } else if basis == Some("no-evidence") {
        2
    } else {
        4
    };
    (
        tier,
        std::cmp::Reverse(age.to_bits()),
        row.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    )
}

/// The STATUS word `list` renders: SERVED ACTIVITY, never a `live` token
/// (x-aaaa, AC7). Nothing decides on this word anymore - retirement reads the
/// reverse join, the lanes read their own probes - so the column answers the
/// operator's actual question, what is this session doing: `writing` (the
/// transcript moved inside `STALE_ATTENTION_S`), `quiet` (older), `parked`
/// (the tail closed a promise). A row whose last assistant turn is a provider
/// refusal reads `refused`: that error record is the newest transcript entry,
/// so without this arm a corpse reads `writing`. A positively falsified row
/// reads `orphaned`, and a probe that did not answer reads `unknown`. A confirmed-live pid does
/// NOT lift an unanswered age to `quiet`: the word is activity, and a process
/// being up says nothing about when it last wrote - the same row must render
/// the same word through the Python list lane, which has no pid census.
pub(crate) fn rendered_status_from_truth(
    probe: Option<&crate::truth_probe::TruthProbe>,
) -> &'static str {
    if probe.and_then(|p| p.reachability.as_deref()) == Some("unreachable") {
        return "orphaned";
    }
    if probe.is_some_and(|p| p.provider_refusal.is_some()) {
        return "refused";
    }
    match probe.map(|p| p.state.as_str()) {
        Some("done") => "parked",
        Some(_) => match probe.and_then(|p| p.last_activity_age_s) {
            Some(age) if age < STALE_ATTENTION_S => "writing",
            Some(_) => "quiet",
            None => "unknown",
        },
        None => "unknown",
    }
}

/// True for a Claude model id or tier alias. Mirrors
/// `fno.agents.model_routing.is_anthropic_model` (cli/src/fno/agents/model_routing.py) --
/// duplicated rather than shelled out to because the daemon already pays one
/// probe per row and a second process spawn per row would multiply that cost
/// for a four-branch string check.
fn is_anthropic_model(model: &str) -> bool {
    let name = model.trim().to_ascii_lowercase();
    name.starts_with("claude-") || matches!(name.as_str(), "opus" | "sonnet" | "haiku" | "fable")
}

/// Structural refusal predicate (Locked Decision 3). Mirrors
/// `fno.agents.reachability._is_refused` -- never reads the transcript's
/// prose, so a reworded refusal message cannot break it. Fails OPEN: a
/// recorded `route_settings_path` records the INTENDED route, so a
/// foreign-routed worker answering as a foreign model is healthy, not refused.
fn is_refused(observed_model: &Value, harness: &str, route_settings_path: Option<&str>) -> bool {
    if harness != "claude" {
        return false;
    }
    if route_settings_path.is_some() {
        return false;
    }
    if observed_model.get("kind").and_then(Value::as_str) != Some("observed") {
        return false;
    }
    match observed_model.get("model").and_then(Value::as_str) {
        Some(model) => !is_anthropic_model(model),
        None => false,
    }
}

/// Map a truth probe onto the progress axis `list` renders, mirroring Python's
/// `classify_progress` (`fno/agents/reachability.py`). Reads the SAME probe
/// `rendered_status_from_truth` reads, plus `harness` and
/// `route_settings_path` off the registry entry -- no second probe is paid.
///
/// Precedence matches the Python classifier exactly: a falsified/unresolved
/// row first (`reachability` absent or `unreachable` -- AC12-FR, the
/// compatibility-fallback case included, since an unmeasured row has no
/// progress state to report either), then the refusal predicate, then the
/// truth-state arms plus the measured transcript age. A written `working`
/// state is not progress evidence when its transcript stopped advancing.
pub(crate) fn progress_from_truth(
    probe: Option<&crate::truth_probe::TruthProbe>,
    batch: crate::truth_probe::BatchOutcome,
    harness: &str,
    route_settings_path: Option<&str>,
) -> (&'static str, &'static str) {
    // The probe's own absence splits by the batch outcome: a handle
    // missing from a batch that ran clean is the measured `no-evidence`
    // verdict; a handle missing because the batch itself timed out is
    // `unmeasured`, a different fact the row may not publish as a verdict. A
    // probe that ANSWERED renders exactly as before whatever the batch did -
    // this handle was measured.
    match probe {
        None => {
            return match batch {
                crate::truth_probe::BatchOutcome::Measured => ("unknown", "no-evidence"),
                crate::truth_probe::BatchOutcome::NotMeasured => ("unknown", "unmeasured"),
            }
        }
        Some(p) => match p.reachability.as_deref() {
            Some("unreachable") | None => return ("unknown", "no-evidence"),
            _ => {}
        },
    }
    let observed_model = probe.map(|p| &p.observed_model);
    if observed_model.is_some_and(|om| is_refused(om, harness, route_settings_path)) {
        return ("refused", "model-refused");
    }
    // Below the structural arm, mirroring Python: `model-refused` is read off
    // observed-model evidence, this one off the transcript's own refusal text.
    if probe.is_some_and(|p| p.provider_refusal.is_some()) {
        return ("refused", "provider-refused");
    }
    match probe.map(|p| p.state.as_str()) {
        Some("working" | "watching") => match probe.and_then(|p| p.last_activity_age_s) {
            None => ("unknown", "no-evidence"),
            Some(age) if age >= STALE_ATTENTION_S => ("unknown", "silent"),
            Some(_) => ("advancing", "transcript-turn"),
        },
        Some("your-move") => ("awaiting-operator", "operator-turn"),
        Some("done") => ("parked", "promise"),
        Some("stalled") => ("unknown", "silent"),
        _ => ("unknown", "no-evidence"),
    }
}

pub(crate) fn registry_truth_handle(entry: &RegistryEntry) -> String {
    if let Some(session_id) = entry
        .harness_session_id
        .as_deref()
        .filter(|id| !id.is_empty())
    {
        return session_id.to_string();
    }
    if !entry.short_id.is_empty() {
        entry.short_id.clone()
    } else {
        entry.name.clone()
    }
}

/// The printed ADDRESS for a list row: the head of the row's fno-minted
/// `fno_id`, but only for codex rows - their head collides across one
/// UUIDv7 clock window and Python mail never resolved a codex head anyway.
/// Every other harness keeps the harness head until Python mail send and
/// peek resolve the fno handle, so a claude worker's listed mailbox stays
/// one its sender can reach. A legacy row (fno_id a harness copy, short id
/// or name) keeps the harness head too; the claude `transport_short`
/// fallback stays at the caller.
pub(super) fn row_address(e: &RegistryEntry) -> Option<String> {
    let own: Vec<&str> = [
        e.harness_session_id.as_deref(),
        e.related_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let minted = (e.harness_name() == "codex")
        .then(|| crate::identity::fno_handle(e.fno_id.as_deref(), &own))
        .flatten();
    minted.or_else(|| {
        e.harness_session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(crate::identity::canonical_handle)
    })
}

/// The `basis` leg beside `reachability`, worded by the batch
/// outcome when the probe is absent: a handle the batch never measured reads
/// `unmeasured`, a handle a clean batch resolved nothing for keeps the null
/// an absent reading has always rendered (the `no-evidence` verdict lives on
/// `progress_basis`, where [`progress_from_truth`] words it the same way). A
/// probe that answered carries its own basis.
pub(super) fn basis_word_from_truth(
    probe: Option<&crate::truth_probe::TruthProbe>,
    batch: crate::truth_probe::BatchOutcome,
) -> Value {
    match probe.and_then(|t| t.basis.clone()) {
        Some(basis) => json!(basis),
        None if probe.is_none() && batch == crate::truth_probe::BatchOutcome::NotMeasured => {
            json!("unmeasured")
        }
        None => Value::Null,
    }
}

/// The instrument the activity age came from, lifted off the same probe:
/// `last-entry` | `mtime` | `opencode-db` when the probe answered, the
/// resolver's reason word (`not-found` | `no-records` | `resolver-error`)
/// when it could not resolve the handle, and `unmeasured` when the batch
/// never ran for this page. The three unknown-reason words are the
/// difference between "this worker has no transcript" and "the resolver
/// crashed"; both rendered as the same blank before, and a null
/// never stands alone.
pub(super) fn activity_basis_from_truth(
    probe: Option<&crate::truth_probe::TruthProbe>,
    batch: crate::truth_probe::BatchOutcome,
) -> Value {
    match probe.and_then(|t| t.last_activity_basis.clone()) {
        Some(basis) => json!(basis),
        None if probe.is_none() && batch == crate::truth_probe::BatchOutcome::NotMeasured => {
            json!("unmeasured")
        }
        None => Value::Null,
    }
}

/// Parse one row timestamp into `(value, basis)`-compatible `Option`s for the
/// contradiction rules: absent and unreadable both read `None`.
fn row_timestamp(value: Option<&Value>) -> Option<chrono::DateTime<chrono::Utc>> {
    let value = value?;
    if let Some(raw) = value.as_str() {
        return chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|parsed| parsed.with_timezone(&chrono::Utc));
    }
    let micros = value.as_u64()?;
    if micros <= 1_000_000_000_000 {
        return None;
    }
    chrono::DateTime::from_timestamp_micros(micros as i64)
}

/// Refuse a row-level verdict when the same emitted row carries fresher
/// evidence against it. This is deliberately pure and shared by the fixture
/// test with Python; the caller supplies all fields before the row is written.
/// `now` is injected so the fixture's fixed clock and production's wall clock
/// assert the same rules.
pub(crate) fn apply_row_contradiction(
    row: &mut Map<String, Value>,
    exited_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) {
    // The falsifier as it ARRIVED, snapshotted before any rule below rewrites
    // `basis`. Python's `_supervisor_contradicted` reads the input mapping, so
    // reading the mutated map here would name a different falsifier than the
    // twin for the same row, and the shared fixture has no case where two
    // rules fire together to catch it.
    let incoming_basis = row
        .get("basis")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // `status` for the same reason, and the reason generalises: Python reads
    // the input `row` and writes a SEPARATE `projected` dict, so every rule
    // there sees the original. This twin mutates `row` in place, so any rule
    // reading `status` after an earlier one rewrote it diverges from Python
    // for that row. Today the two rules are mutually exclusive - `terminal`
    // is `orphaned`/`exited` and this one needs `spawning` - so nothing
    // changes; snapshot anyway, because relying on that exclusion is a rule
    // no test states and the next rule added here will not know it.
    let incoming_status = row
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let event_at = row_timestamp(row.get("last_event_at"));
    let reconciled_at = row_timestamp(row.get("last_reconciled_at"));
    let terminal = matches!(
        row.get("status").and_then(Value::as_str),
        Some("orphaned" | "exited")
    );
    // An exit-recorded verdict dates from the exit stamp. The reconcile stamp
    // is rewritten every sweep tick, so a later event could never beat it.
    let verdict_at = if incoming_basis == "exit-recorded" {
        exited_at
            .and_then(|s| row_timestamp(Some(&json!(s))))
            .or(reconciled_at)
    } else {
        reconciled_at
    };
    if terminal && event_at.is_some() && verdict_at.is_some() && event_at > verdict_at {
        row.insert("status".into(), json!("unknown"));
        row.insert("basis".into(), json!("stale-verdict-fresher-event"));
    }

    let message_at = row_timestamp(row.get("last_message_at"));
    let message_is_too_new = match (message_at, event_at) {
        (Some(message), Some(event)) => message - event > chrono::Duration::seconds(2),
        _ => false,
    };
    if message_is_too_new {
        row.insert("last_message_at".into(), Value::Null);
        row.insert(
            "last_message_at_basis".into(),
            json!("refused-newer-than-transcript"),
        );
    }

    // A stored `spawning` token a live pid has outlived: the token
    // stopped being a measurement. Fires only on POSITIVE liveness (the
    // caller measured a live pid and injected `pid_alive: true`); unknown
    // keeps the token, and a missing `created_at` is absent age evidence,
    // not staleness. Mirrors `_spawning_outlived_by_a_live_pid` in Python;
    // rows read `spawning` for 3-16 hours while alive.
    if incoming_status == "spawning"
        && row.get("pid_alive") == Some(&Value::Bool(true))
        // `> Duration::seconds(600)`, not `num_seconds() > 600`: num_seconds
        // truncates, so a 600.5s-old row read `spawning` here and `live` in
        // Python, whose timedelta compare keeps the fraction.
        && row_timestamp(row.get("created_at"))
            .is_some_and(|created_at| now - created_at > chrono::Duration::seconds(600))
    {
        row.insert("status".into(), json!("quiet"));
        row.insert("basis".into(), json!("stale-spawning-live-pid"));
    }
    row.remove("pid_alive");

    // Both keys ALWAYS ride the row, as `reachability`/`basis` and
    // `progress`/`progress_basis` already do on this same row. A conditional
    // key cannot be told apart from a producer that forgot to set one, and the
    // list-row contract in schemas/agents-list-row.json is an exact key set.
    let (origin, origin_basis) = liveness_origin(row);
    row.insert("liveness_origin".into(), origin);
    row.insert(
        "liveness_origin_basis".into(),
        origin_basis.map_or(Value::Null, |basis| json!(basis)),
    );
    // A superseded supervisor claim beside the falsifier
    // that beat it: `superseded_live_status` is a caller-injected input (like
    // `pid`), popped here; only the basis key survives. PRESENCE is the
    // caller's assertion that a supersession happened; which words claim
    // nothing lives in the gate that stamps this input (read.py's {idle,
    // done} admission set) - a second word list here once denied a
    // supersession the gate had stamped. Mirrors `_supervisor_contradicted`
    // in Python.
    let superseded = row
        .get("superseded_live_status")
        .and_then(Value::as_str)
        .is_some_and(|word| !word.is_empty());
    let contradicted = superseded
        && row.get("reachability").and_then(Value::as_str) == Some("unreachable")
        && !incoming_basis.is_empty();
    row.insert(
        "live_status_basis".into(),
        if contradicted {
            json!(format!("contradicted-by-{incoming_basis}"))
        } else {
            // This projection never runs the claude live-status
            // probe, so null here would read as "measured, nothing found".
            // The lane that did not ask says so: a blank is the
            // one thing this pair may not be); the Python list surface, which
            // does run it, keeps its own words.
            json!("not-probed")
        },
    );
    row.remove("superseded_live_status");
}

/// Parse one row timestamp into `(value, basis)`, separating absent from
/// unreadable. Mirrors `_read_field` in cli/src/fno/agents/row_contradiction.py.
///
/// Folding the two together is what let `liveness_origin: null` mean five
/// different things at once, so a reader holding one null could not tell
/// "nothing was recorded" from "something this parser cannot read".
fn row_field_with_basis(
    row: &Map<String, Value>,
    key: &str,
    label: &str,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<String>) {
    match row.get(key) {
        None | Some(Value::Null) => (None, Some(format!("{label}-absent"))),
        raw => match row_timestamp(raw) {
            Some(parsed) => (Some(parsed), None),
            None => (None, Some(format!("{label}-unreadable"))),
        },
    }
}

/// Return `(liveness_origin, basis)` for one row. Mirrors `_liveness_origin`
/// in cli/src/fno/agents/row_contradiction.py, and the shared fixture at
/// schemas/agents-row-contradiction.json drives both.
///
/// THE PID GATE COMES FIRST. This producer already checked it and the Python
/// one did not, so a pidless row read `survivor` there and null here: one
/// field, two reachable implementations, one guard. A non-null origin carries
/// no basis, because the value is its own evidence.
fn liveness_origin(row: &Map<String, Value>) -> (Value, Option<String>) {
    if !row.get("pid").is_some_and(|value| !value.is_null()) {
        return (Value::Null, Some("pid-absent".to_string()));
    }
    let (created_at, basis) = row_field_with_basis(row, "created_at", "created-at");
    let Some(created_at) = created_at else {
        return (Value::Null, basis);
    };
    let (pid_started_at, basis) = row_field_with_basis(row, "pid_start_time", "pid-start");
    let Some(pid_started_at) = pid_started_at else {
        return (Value::Null, basis);
    };
    if (pid_started_at - created_at).num_seconds() > 600 {
        (json!("resumed"), None)
    } else {
        (json!("survivor"), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The SAME probe fixtures the parent's tests read: these assertions moved
    // here to sit beside the functions they test, and a second copy of the
    // fixture is how two case tables start disagreeing.
    use crate::daemon::tests::{probe, probe_with_verdict};
    use crate::truth_probe::BatchOutcome::{self, Measured};
    use serde_json::json;

    /// The verdict OUTRANKS the transcript state, which is the whole point of
    /// putting it on the wire: a session whose process died forty minutes ago
    /// still reads `working` from its transcript, and mapping that state is how
    /// `list` reported a dead worker live. Same input state, opposite render.
    #[test]
    fn the_reachability_verdict_outranks_the_transcript_state() {
        assert_eq!(
            rendered_status_from_truth(probe_with_verdict("working", "unreachable").as_ref()),
            "orphaned",
            "a falsified row must not render writing merely because its transcript is recent"
        );
        // A verdict that did not resolve falls through to the ACTIVITY read:
        // a 12s-old transcript is writing whatever the state word says.
        assert_eq!(
            rendered_status_from_truth(probe_with_verdict("stalled", "unknown").as_ref()),
            "writing"
        );
        assert_eq!(
            rendered_status_from_truth(probe("stalled").as_ref()),
            "unknown",
            "a probe that answered nothing reads unknown, never orphaned "
        );
    }

    /// A verdict-carrying probe with an explicit `observed_model`, for the
    /// progress-axis tests below (`probe_with_verdict` above always carries
    /// `Value::Null`, which is `no-transcript` and can never refuse).
    fn probe_observed(
        state: &str,
        reachability: &str,
        observed_model: Value,
    ) -> Option<crate::truth_probe::TruthProbe> {
        Some(crate::truth_probe::TruthProbe {
            state: state.into(),
            reachability: Some(reachability.into()),
            basis: Some("transcript".into()),
            last_activity_age_s: Some(12.0),
            last_activity_basis: None,
            last_event_at: None,
            last_message: None,
            observed_model,
            provider_refusal: None,
            harness_title: None,
        })
    }

    #[test]
    fn progress_ac1_ac2_done_is_parked_working_is_advancing() {
        assert_eq!(
            progress_from_truth(
                probe_with_verdict("done", "reachable").as_ref(),
                Measured,
                "claude",
                None
            ),
            ("parked", "promise")
        );
        assert_eq!(
            progress_from_truth(
                probe_with_verdict("working", "reachable").as_ref(),
                Measured,
                "claude",
                None
            ),
            ("advancing", "transcript-turn")
        );
        assert_eq!(
            progress_from_truth(
                probe_with_verdict("your-move", "reachable").as_ref(),
                Measured,
                "claude",
                None
            ),
            ("awaiting-operator", "operator-turn")
        );
    }

    #[test]
    fn progress_ac3_ac4_refusal_outranks_working_but_never_a_routed_worker() {
        let refused_model = json!({"kind": "observed", "model": "glm-5.2[1m]"});
        assert_eq!(
            progress_from_truth(
                probe_observed("working", "reachable", refused_model.clone()).as_ref(),
                Measured,
                "claude",
                None
            ),
            ("refused", "model-refused"),
            "the refusal must outrank the active working truth state"
        );
        assert_eq!(
            progress_from_truth(
                probe_observed("working", "reachable", refused_model).as_ref(),
                Measured,
                "claude",
                Some("/x/route-settings/ab12.json")
            ),
            ("advancing", "transcript-turn"),
            "a deliberately routed worker must never be condemned"
        );
    }

    #[test]
    fn progress_ac7_unreachable_is_unknown_no_evidence_regardless_of_state() {
        for state in ["working", "done", "your-move", "stalled"] {
            assert_eq!(
                progress_from_truth(
                    probe_with_verdict(state, "unreachable").as_ref(),
                    Measured,
                    "claude",
                    None
                ),
                ("unknown", "no-evidence"),
                "state={state}"
            );
        }
    }

    /// The positive control, since the live fleet-wide timeout does not
    /// reproduce on demand: the SAME missing-handle input, worded by the batch
    /// outcome. `unmeasured` says the instrument did not run; `no-evidence`
    /// stays the verdict a clean batch that resolved nothing earns. Assert the
    /// word, never the absence of the other one.
    #[test]
    fn a_missing_handle_splits_by_the_batch_outcome() {
        assert_eq!(
            progress_from_truth(None, BatchOutcome::NotMeasured, "claude", None),
            ("unknown", "unmeasured"),
            "the instrument did not run: never publish that as the no-evidence verdict"
        );
        assert_eq!(
            progress_from_truth(None, BatchOutcome::Measured, "claude", None),
            ("unknown", "no-evidence"),
            "a clean batch that resolved nothing keeps its verdict"
        );
        // A probe that ANSWERED is measured whatever the page-level outcome:
        // the batch timing out must not re-word the handles it did reach.
        assert_eq!(
            progress_from_truth(
                probe_with_verdict("working", "reachable").as_ref(),
                BatchOutcome::NotMeasured,
                "claude",
                None
            ),
            ("advancing", "transcript-turn")
        );
    }

    // ------------------------------------------------------------------
    // The provider-refusal arm. The Python lane's `test_reachability.py`
    // asserts one row per distinct branch of this table, because two lanes
    // rendering the same row differently is the defect this field exists to
    // close.
    // ------------------------------------------------------------------

    fn refused_probe(provider_refusal: Option<&str>) -> Option<crate::truth_probe::TruthProbe> {
        let mut probe = probe_with_verdict("working", "reachable").unwrap();
        probe.last_activity_age_s = Some(469.0);
        probe.provider_refusal = provider_refusal.map(str::to_owned);
        Some(probe)
    }

    /// AC3-HP. The measured specimen: t-1666-guard-retry, dead on a usage-limit
    /// 429 for 469 s, rendered `writing` on both lanes because the error record
    /// is the newest transcript entry.
    #[test]
    fn ac3_a_provider_refused_row_renders_refused_on_both_axes() {
        let probe = refused_probe(Some("provider_4xx_quota"));
        assert_eq!(rendered_status_from_truth(probe.as_ref()), "refused");
        assert_eq!(
            progress_from_truth(probe.as_ref(), Measured, "claude", None),
            ("refused", "provider-refused")
        );
    }

    /// The shared row-contradiction truth table (schemas/agents-row-
    /// contradiction.json), asserted against the Rust projection here; the
    /// Python twin asserts the same cases against `project_row`. Moved beside
    /// `apply_row_contradiction` under the file-budget gate.
    #[test]
    fn row_contradiction_fixture_matches_python_projection() {
        const FIXTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/agents-row-contradiction.json"
        ));
        let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture is valid JSON");
        let now = chrono::DateTime::parse_from_rfc3339(
            fixture["now"].as_str().expect("fixture now is a string"),
        )
        .expect("fixture now is a timestamp")
        .with_timezone(&chrono::Utc);
        for case in fixture["cases"].as_array().expect("cases is an array") {
            let mut row = case["row"].as_object().expect("row is an object").clone();
            let exited_at = row.remove("exited_at");
            apply_row_contradiction(&mut row, exited_at.as_ref().and_then(Value::as_str), now);
            assert!(!row.contains_key("exited_at"), "case={}", case["name"]);
            // Where the two lanes legitimately differ - the Rust
            // projection never runs the claude live-status probe, so its
            // no-contradiction basis reads `not-probed` where Python renders
            // null - the case carries an `expected_rust` override. Absent,
            // `expected` binds both lanes.
            let expected = case
                .get("expected_rust")
                .unwrap_or(&case["expected"])
                .as_object()
                .expect("expected is an object");
            for (key, expected) in expected {
                assert_eq!(row.get(key), Some(expected), "case={}", case["name"]);
            }
        }
    }

    /// The `--all` provenance lane: a reaped row the registry dropped still
    /// shows, newest first, carrying the resume command and the newest
    /// recorded cause from the event journal. The event store is absent in
    /// this staged world, so the journal text is the source and the join
    /// runs on the same code path.
    #[test]
    fn the_all_lane_joins_the_newest_recorded_cause() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let receipts = home.root().join("reap-receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join("claude-11111111-2222-3333-4444-555555555555.json"),
            json!({
                "row_name": "warden",
                "short_id": "11111111",
                "harness": "claude",
                "harness_session_id": "11111111-2222-3333-4444-555555555555",
                "cwd": "/tmp/wt",
                "log_path": null,
                "created_at": "2026-09-24T10:00:00Z",
                "reaped_at": "2026-09-25T16:19:49Z",
                "resume": "claude --bg --resume 11111111-2222-3333-4444-555555555555 --model opus --effort high",
                "ledger": {"graph_node_id": "x-node", "pr_number": 1943}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            home.events_jsonl(),
            concat!(
                r#"{"ts":"2026-09-25T16:19:48Z","type":"registry_row_removed","source":"daemon","data":{"session_id":"11111111-2222-3333-4444-555555555555"}}"#,
                "\n",
                r#"{"ts":"2026-09-25T16:19:49Z","type":"agent_row_reaped","source":"daemon","data":{"harness_session_id":"11111111-2222-3333-4444-555555555555","basis":"every named node done: x-node"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let rows = retired_rows(&home);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row["name"], "warden");
        assert_eq!(row["state"], "reaped");
        assert_eq!(row["node"], "x-node");
        assert_eq!(row["cause"], "agent_row_reaped", "the newest event wins");
        assert_eq!(row["basis"], "every named node done: x-node");
        assert!(
            row["resume"]
                .as_str()
                .unwrap()
                .contains("--model opus --effort high"),
            "the resume command rides the row verbatim"
        );
        // A receipt whose cause never journal'd still shows, honestly absent.
        std::fs::write(
            receipts.join("claude-99999999-2222-3333-4444-555555555555.json"),
            json!({
                "row_name": "ghost",
                "short_id": "99999999",
                "harness": "claude",
                "harness_session_id": "99999999-2222-3333-4444-555555555555",
                "cwd": "/tmp/other",
                "log_path": null,
                "created_at": "2026-09-24T10:00:00Z",
                "reaped_at": "2026-09-25T15:00:00Z",
                "resume": "claude --bg --resume 99999999-2222-3333-4444-555555555555"
            })
            .to_string(),
        )
        .unwrap();
        let rows = retired_rows(&home);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "warden", "newest reaped_at first");
        assert_eq!(rows[1]["cause"], "not recorded");
        std::fs::remove_dir_all(home.root()).ok();
    }
}

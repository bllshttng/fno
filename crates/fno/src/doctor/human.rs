//! The human output renderer (doctor.py:2138-2883), section order pinned by
//! the frozen text golden.

use std::path::Path;

use serde_json::{json, Value};

use super::report;

fn emit(err: bool, line: &str) {
    if err {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

pub(crate) use super::report::emit_blockers;

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn opt_s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

const REVIEW_WINDOW_S: i64 = 900;

fn review_invocation_lines() -> Vec<String> {
    let path = report::project_events_path();
    if !path.exists() {
        return vec![
            "fno doctor: review invocations: no event journal; no sent attempt to lose".to_string(),
        ];
    }
    scan_review_journal(&path)
}

fn scan_review_journal(path: &Path) -> Vec<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            return vec![format!(
                "fno doctor: review invocations: unmeasurable ({e})"
            )];
        }
    };
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::seconds(REVIEW_WINDOW_S);
    let window_start = now - chrono::Duration::seconds(2 * REVIEW_WINDOW_S);
    let mut sent: Vec<(String, chrono::DateTime<chrono::Utc>, Value)> = Vec::new();
    let mut attested: Vec<String> = Vec::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(data) = event.get("data").and_then(Value::as_object) else {
            continue;
        };
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_type == "review_invocation" {
            let Some(inv) = data.get("invocation_id").and_then(Value::as_str) else {
                continue;
            };
            let ts = event.get("ts").and_then(Value::as_str).unwrap_or_default();
            let Ok(t) = chrono::DateTime::parse_from_rfc3339(&ts.replace('Z', "+00:00")) else {
                continue;
            };
            sent.retain(|(id, _, _)| id != inv);
            sent.push((
                inv.to_string(),
                t.with_timezone(&chrono::Utc),
                event.clone(),
            ));
        } else if event_type == "review_attestation" {
            if let Some(inv) = data.get("invocation_id").and_then(Value::as_str) {
                attested.push(inv.to_string());
            }
        }
    }
    finish_review_scan(sent, attested, cutoff, window_start)
}

fn finish_review_scan(
    sent: Vec<(String, chrono::DateTime<chrono::Utc>, Value)>,
    attested: Vec<String>,
    cutoff: chrono::DateTime<chrono::Utc>,
    window_start: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let lost: Vec<_> = sent
        .iter()
        .filter(|(id, t, _)| *t >= window_start && *t <= cutoff && !attested.contains(id))
        .collect();
    if lost.is_empty() {
        return vec![format!(
            "fno doctor: review invocations: scan complete; none lost in the last {}m",
            REVIEW_WINDOW_S / 60
        )];
    }
    vec![format!(
        "fno doctor: review invocations: {} sent attempt(s) have no matching attestation after {}m",
        lost.len(),
        REVIEW_WINDOW_S / 60
    )]
}

pub(crate) fn emit_human(rep: &Value, src: Option<&Path>, err: bool, cargo_present: bool) {
    for line in review_invocation_lines() {
        emit(err, &line);
    }
    emit_source_sync(rep, err);
    emit_status(rep, src, err);
    emit_rust_binary(rep, err, cargo_present);
    emit_binary_rev(rep, err);
    emit_components(rep, err);
    emit_daemon_drift(rep, err);
    emit_mux_front_door(rep, err);
    emit_stale_servers(rep, err);
    emit_running_components(rep, err);
    emit_orphans(rep, err);
    emit_pr_watch(rep, err);
    emit_control_plane_arms(rep, err);
    emit_observer_unavailable(rep, err);
    emit_fd_limit(rep, err);
    emit_dead_letter(rep, err);
    emit_managed_block(rep, err);
    emit_surface_lines(rep, err);
    emit_groom(rep, err);
    emit_post_merge_sync(rep, err);
    emit_evals(rep, err);
    emit_launch_agents(rep, err);
    emit_silent_switches(rep, err);
    emit_review_gap(rep, err);
    emit_optout_residue(rep, err);
    emit_plugin_cache(rep, err);
    emit_codex_dual(rep, err);
    emit_pre_push(rep, err);
    emit_plugin_hooks(rep, err);
    emit_codex_foreign(rep, err);
    emit_codex_app_server(rep, err);
}

fn emit_source_sync(rep: &Value, err: bool) {
    let ss = rep
        .get("source_checkout_sync")
        .cloned()
        .unwrap_or(Value::Null);
    let status = ss.get("status").and_then(Value::as_str).unwrap_or_default();
    let behind = ss.get("behind").and_then(Value::as_i64);
    if status == "behind" {
        let unit = if behind == Some(1) {
            "commit"
        } else {
            "commits"
        };
        emit(err, &format!(
            "fno doctor: resolved source checkout is {} {} behind origin/main; freshness is relative to stale local source. Sync the checkout before trusting verification.",
            behind.unwrap_or(0), unit
        ));
    } else if status == "unknown" && ss.get("source_head").is_some() {
        emit(err, &format!(
            "fno doctor: source checkout sync is unknown ({}); freshness against origin/main is unmeasured.",
            ss.get("detail").and_then(Value::as_str).filter(|d| !d.is_empty()).unwrap_or("no detail")
        ));
    }
}

fn emit_status(rep: &Value, src: Option<&Path>, err: bool) {
    let status = rep
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status == "fresh" {
        emit(err, &format!(
            "fno doctor: installed fno is up to date with source at {} (rev {}). Unmerged work in another branch or worktree is not included, and `fno` forwards to this installed build from anywhere. To exercise another checkout run: cd <checkout>/cli && uv run fno-py <command>",
            src.map(|p| p.to_string_lossy()).unwrap_or("the resolved source checkout").into_owned(),
            rep.get("source_rev").and_then(Value::as_str).unwrap_or("unknown")
        ));
        return;
    }
    if status == "stale" {
        emit_status_stale(rep, src, err);
        return;
    }
    emit_status_unknown(rep, src, err);
}

fn emit_status_stale(rep: &Value, src: Option<&Path>, err: bool) {
    let src_label = src
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "source".into());
    let missing: Vec<String> = rep
        .get("missing_commands")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let content_stale = rep.get("content_stale") == Some(&Value::Bool(true));
    let missing_keys: Vec<String> = rep
        .get("missing_config_keys")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let python_stale = rep.get("python_stale") == Some(&Value::Bool(true));
    if !missing.is_empty() {
        emit(err, &format!(
            "fno doctor: installed fno is behind {src_label} (missing: {}). Run `fno doctor update` (or `fno doctor --fix`).",
            missing.join(", ")
        ));
        return;
    }
    if content_stale {
        emit(err, &format!(
            "fno doctor: installed fno is STALE - {} .py file(s) on disk differ from {src_label} (a cache-hit reinstall can leave old bytes while the installed-rev marker still reads HEAD). Run `fno doctor update` (or `fno doctor --fix`).",
            rep.get("content_drift_count").and_then(Value::as_i64).unwrap_or(0)
        ));
        return;
    }
    if !missing_keys.is_empty() {
        let first = missing_keys[0].clone();
        let inst = rep.get("installed_rev").and_then(Value::as_str);
        let srcrev = rep.get("source_rev").and_then(Value::as_str);
        let mut msg = format!(
            "fno doctor: Python config schema is STALE (deployed is missing {} config key(s), e.g. {first}).",
            missing_keys.len()
        );
        if let (Some(i), Some(s)) = (inst, srcrev) {
            if i != s {
                msg.push_str(&format!(" Installed rev {i} != source {s}."));
            }
        }
        emit(
            err,
            &format!("{msg} Run `fno doctor update` (or `fno doctor --fix`)."),
        );
        return;
    }
    if python_stale {
        emit(err, &format!(
            "fno doctor: installed fno is behind {src_label} (installed rev {} != source {}). Run `fno doctor update` (or `fno doctor --fix`).",
            rep.get("installed_rev").and_then(Value::as_str).unwrap_or("None"),
            rep.get("source_rev").and_then(Value::as_str).unwrap_or("None")
        ));
        return;
    }
    let ri = rep.get("rust_installed_rev").and_then(Value::as_str);
    let rs = rep.get("rust_source_rev").and_then(Value::as_str);
    if let (Some(ri), Some(rs)) = (ri, rs) {
        emit(err, &format!(
            "fno doctor: rust bins STALE (installed {} != source {}). Run fno doctor update (or fno doctor --fix).",
            short12(ri), short12(rs)
        ));
        return;
    }
    emit(err, "fno doctor: rust bins STALE (a deployed component does not match the source build). Run fno doctor update (or fno doctor --fix); the component lines below name which one.");
}

fn emit_status_unknown(rep: &Value, src: Option<&Path>, err: bool) {
    let indeterminate = rep.get("content_indeterminate") == Some(&Value::Bool(true));
    let installed_rev = rep.get("installed_rev").and_then(Value::as_str);
    let source_rev = rep.get("source_rev").unwrap_or(&Value::Null);
    if indeterminate
        && installed_rev.is_some()
        && Some(installed_rev.unwrap_or_default()) == source_rev.as_str()
    {
        emit(err, "fno doctor: status unknown - the installed-rev marker matches HEAD, but the content check could not read the installed/source .py bytes to confirm it (the marker alone can lie about a cache-hit reinstall). Check file permissions, or run `fno doctor update` to be safe.");
        return;
    }
    if src.is_none() {
        emit(
            err,
            "fno doctor: status unknown (no source checkout to compare against).",
        );
        return;
    }
    emit(err, "fno doctor: status unknown (installed rev undeterminable; capability probe found no missing commands).");
}

fn short12(v: &str) -> &str {
    let n = v.char_indices().nth(12).map(|(i, _)| i).unwrap_or(v.len());
    &v[..n]
}

fn emit_rust_binary(rep: &Value, err: bool, cargo_present: bool) {
    let bin = opt_s(rep, "rust_binary");
    let rust_stale = rep.get("rust_stale") == Some(&Value::Bool(true));
    if bin.is_none() && !cargo_present {
        emit(
            err,
            "fno doctor: rust fno-agents binary: not found (cargo leg not applicable).",
        );
        return;
    }
    if rust_stale {
        let ri = rep
            .get("rust_installed_rev")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let rs = rep
            .get("rust_source_rev")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let label = bin.unwrap_or("(cargo-installed)");
        emit(err, &format!(
            "fno doctor: rust fno-agents binary: {label} rust bins STALE (installed {} != source {}). Run fno doctor update (or fno doctor --fix).",
            short12(ri), short12(rs)
        ));
        return;
    }
    if !cargo_present {
        emit(err, &format!(
            "fno doctor: rust fno-agents binary: {} revision not tracked (no cargo-installed binary; the rust staleness check applies to the cargo-installed binary only).",
            bin.unwrap_or("(cargo-installed)")
        ));
        return;
    }
    let ri = rep.get("rust_installed_rev").and_then(Value::as_str);
    let rs = rep.get("rust_source_rev").and_then(Value::as_str);
    let label = bin.unwrap_or("(cargo-installed)");
    if ri.is_some() && rs.is_some() && ri == rs {
        emit(
            err,
            &format!(
                "fno doctor: rust fno-agents binary: {label} rust bins fresh (rev {}).",
                short12(ri.unwrap_or_default())
            ),
        );
        return;
    }
    if ri.is_none() {
        emit(err, &format!(
            "fno doctor: rust fno-agents binary: {label} rust revision unknown (binary does not self-report a crates/ rev; rebuild via fno doctor update)."
        ));
        return;
    }
    emit(
        err,
        &format!("fno doctor: rust fno-agents binary: {label} rust revision unknown."),
    );
}

fn emit_binary_rev(rep: &Value, err: bool) {
    let Some(rev) = opt_s(rep, "binary_rev") else {
        return;
    };
    emit(
        err,
        &format!(
            "fno doctor: rust fno-agents binary built at HEAD {} (build provenance).",
            short12(rev)
        ),
    );
}

fn emit_components(rep: &Value, err: bool) {
    let Some(components) = rep.get("components").and_then(Value::as_array) else {
        return;
    };
    let non_fresh: Vec<&Value> = components
        .iter()
        .filter(|c| {
            let st = c.get("status").and_then(Value::as_str).unwrap_or_default();
            st != "fresh" && st != "updated"
        })
        .collect();
    if components.is_empty() {
        return;
    }
    if !non_fresh.is_empty() {
        for row in non_fresh {
            if let Some(line) = row.get("line").and_then(Value::as_str) {
                emit(err, &format!("fno doctor: {line}"));
            }
        }
        return;
    }
    let names: Vec<String> = components
        .iter()
        .filter_map(|c| c.get("component").and_then(Value::as_str).map(String::from))
        .collect();
    let drifted = rep
        .get("daemon_drift")
        .is_some_and(|d| !d.is_null() && *d != Value::Bool(false));
    if drifted {
        emit(
            err,
            &format!(
                "fno doctor: components: {}/{} fresh on disk; a daemon drift note follows.",
                components.len(),
                components.len()
            ),
        );
        return;
    }
    emit(
        err,
        &format!(
            "fno doctor: components: {}/{} fresh ({}).",
            components.len(),
            components.len(),
            names.join(", ")
        ),
    );
}

fn emit_daemon_drift(rep: &Value, err: bool) {
    let drifted = rep
        .get("daemon_drift")
        .is_some_and(|d| !d.is_null() && *d != Value::Bool(false));
    if drifted {
        emit(err, &format!("fno doctor: note: {}", rep["daemon_drift"]));
    }
}

fn emit_mux_front_door(rep: &Value, err: bool) {
    match rep.get("mux_front_door").and_then(Value::as_str) {
        Some("active") => emit(
            err,
            &format!(
                "fno doctor: mux front door: `fno` -> {} (active).",
                rep.get("mux_binary").and_then(Value::as_str).unwrap_or("?")
            ),
        ),
        Some("not-installed") => emit(
            err,
            "fno doctor: mux front door: crates/fno not cargo-installed; bare `fno` will not launch the mux. Run `fno doctor update` (or cargo install --path crates/fno).",
        ),
        Some("shadowed") => emit(
            err,
            &format!(
                "fno doctor: mux front door: installed at {} but `fno` on PATH resolves to {}; the mux is shadowed. Ensure the cargo bin dir precedes any Python `fno` on PATH.",
                rep.get("mux_binary").and_then(Value::as_str).unwrap_or("?"),
                rep.get("path_fno").and_then(Value::as_str).unwrap_or("nothing on PATH")
            ),
        ),
        _ => {}
    }
}

fn emit_stale_servers(rep: &Value, err: bool) {
    for sess in rep
        .get("mux_server_stale")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        emit(
            err,
            "fno doctor: mux server below the wire compatibility floor; run `fno agents restart` to cut it over (auto-heals pane-less servers; add `--mux` to also end servers with live panes).",
        );
    }
}

fn emit_running_components(rep: &Value, err: bool) {
    for row in rep
        .get("running_components")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        let verdict = row
            .get("verdict")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if verdict != "stale" {
            continue;
        }
        let pid = row
            .get("pid")
            .map(|p| format!(" pid {p}"))
            .unwrap_or_default();
        emit(
            err,
            &format!(
                "fno doctor: {} '{}'{} is an older build ({}); on restart: {}; keeps {}.",
                row.get("component").and_then(Value::as_str).unwrap_or("?"),
                row.get("name").and_then(Value::as_str).unwrap_or("unnamed"),
                pid,
                row.get("evidence").and_then(Value::as_str).unwrap_or("?"),
                row.get("on_restart").and_then(Value::as_str).unwrap_or("?"),
                row.get("survives").and_then(Value::as_str).unwrap_or("?")
            ),
        );
    }
}

fn emit_orphans(rep: &Value, err: bool) {
    let orphans: Vec<String> = rep
        .get("orphan_files")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if orphans.is_empty() {
        return;
    }
    emit(
        err,
        &format!(
            "fno doctor: found {} orphaned file(s) from removed capture paths (safe to delete): {}.",
            orphans.len(),
            orphans.join(", ")
        ),
    );
}

fn emit_pr_watch(rep: &Value, err: bool) {
    let pw = rep.get("pr_watch").cloned().unwrap_or(json!({}));
    let verdict = pw
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let detail = pw.get("detail").and_then(Value::as_str).unwrap_or_default();
    if verdict == "dead" || verdict == "wedged" {
        let fix = pw.get("fix").and_then(Value::as_str).unwrap_or_default();
        let fallback = if verdict == "dead" {
            "fno do pr watch install"
        } else {
            "fno do pr watch refresh"
        };
        let fix = if fix.is_empty() { fallback } else { fix };
        emit(err, &format!(
            "fno doctor: pr-watch {verdict} ({}); run `{fix}`, then verify with `fno do pr watch status`.",
            detail
        ));
    } else if verdict == "healthy-pending" {
        emit(
            err,
            &format!("fno doctor: pr-watch installed, awaiting first tick ({detail})."),
        );
    }
}

fn emit_control_plane_arms(rep: &Value, err: bool) {
    let cpa = rep.get("control_plane_arms").cloned().unwrap_or(json!({}));
    if let Some(reason) = cpa.get("unknown_reason").and_then(Value::as_str) {
        emit(
            err,
            &format!(
                "fno doctor: control-plane arms readout unknown ({reason}); staleness is unmeasured."
            ),
        );
    }
    for arm in cpa
        .get("red")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        if let Some(line) = arm.get("line").and_then(Value::as_str) {
            emit(err, &format!("fno doctor: control-plane arm {line}"));
        } else if arm.get("producer_evidence").and_then(Value::as_str) == Some("unobserved") {
            emit(
                err,
                &format!(
                    "fno doctor: control-plane arm {} is UNOBSERVED (no producer receipt in the journals)",
                    arm.get("arm").and_then(Value::as_str).unwrap_or("?")
                ),
            );
        } else {
            emit(
                err,
                &format!(
                    "fno doctor: control-plane arm {} is STALE (last tick {}s ago, interval {}s, skip: {})",
                    arm.get("arm").and_then(Value::as_str).unwrap_or("?"),
                    arm.get("age_s").and_then(Value::as_i64).unwrap_or(0),
                    arm.get("interval_s").and_then(Value::as_i64).unwrap_or(0),
                    arm.get("skip_reason").and_then(Value::as_str).unwrap_or("none")
                ),
            );
        }
    }
}

fn emit_observer_unavailable(rep: &Value, err: bool) {
    let pw = rep.get("pr_watch").cloned().unwrap_or(json!({}));
    let verdict = pw
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if verdict != "disabled" && verdict != "dead" {
        return;
    }
    let standing = report::standing_dispatch_grant();
    if !standing {
        return;
    }
    let fix = pw
        .get("fix")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .unwrap_or("fno config set pr_watch.enabled true");
    emit(
        err,
        &format!(
            "fno doctor: observer_unavailable - auto_merge.grant=dispatch is standing, but the PR watcher is {verdict} ({}); recorded grants will not execute. Run `{fix}`, then verify with `fno do pr watch status`.",
            pw.get("detail").and_then(Value::as_str).unwrap_or_default()
        ),
    );
}

fn emit_fd_limit(rep: &Value, err: bool) {
    let fd = rep.get("fd_limit").cloned().unwrap_or(json!({}));
    if fd.get("verdict").and_then(Value::as_str) != Some("low") {
        return;
    }
    let soft = fd.get("soft");
    let floor = fd.get("threshold").and_then(Value::as_i64).unwrap_or(1024);
    let launchd = fd.get("launchd_soft");
    if launchd.is_none() {
        emit(
            err,
            &format!("fno doctor: open-file soft limit is {soft} in THIS process (floor {floor})."),
        );
        emit(
            err,
            "Raise it in the launch context that starts this process (shell profile, service unit, or container limits)."
        );
        return;
    }
    let soft = fd.get("soft");
    let floor2 = fd.get("threshold").and_then(Value::as_i64).unwrap_or(1024);
    let soft_num = soft.and_then(Value::as_i64);
    let below = soft_num.is_some() && soft_num.unwrap_or(0) <= floor2;
    if below {
        emit(
            err,
            &format!(
                "fno doctor: open-file soft limit is {} in THIS process (floor {}).",
                soft_num.unwrap_or(0),
                floor2
            ),
        );
    } else {
        emit(err, &format!(
            "fno doctor: open-file soft limit is {} in THIS process, but launchd's session default is {} (floor {}).",
            soft_num.unwrap_or(0),
            launchd.and_then(Value::as_i64).unwrap_or(0),
            floor2
        ));
    }
    let kern = fd.get("kern_maxfiles").and_then(Value::as_i64);
    let kern_note = match kern {
        Some(k) => format!("kern.maxfiles is {k}, so the kernel is not the constraint"),
        None => "the kernel's own ceiling is far higher than either number".to_string(),
    };
    emit(err, &format!(
        "The limit is inherited from the launch context, not the machine: {kern_note}, and a login shell can read a healthy number while every spawned worker starves."
    ));
    emit(
        err,
        "Raise it for launchd children: sudo launchctl limit maxfiles 65536 unlimited",
    );
    emit(err, "The raise only reaches NEWLY launched processes, so restart the affected sessions afterwards. Persistence across reboot needs a LaunchDaemon plist.");
}

fn emit_dead_letter(rep: &Value, err: bool) {
    let dl = rep.get("dead_letter").cloned().unwrap_or(json!({}));
    if dl.get("drain_hook_wired") == Some(&Value::Bool(false)) {
        emit(
            err,
            "fno doctor: a2a drain-self SessionStart hook is NOT wired; durable mail to this claude env will strand (senders see 'queued' forever). Wire inject-mail-drain-session-start.sh into hooks.json SessionStart.",
        );
    }
    let stale: Vec<String> = dl
        .get("stale_unread")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|row| row.get("handle").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if !stale.is_empty() {
        let mut uniq: Vec<String> = stale.clone();
        uniq.sort();
        uniq.dedup();
        emit(
            err,
            &format!(
                "fno doctor: {} unread a2a message(s) stranded on the bus for a dead handle ({}); no live session will ever drain them.",
                stale.len(),
                uniq.join(", ")
            ),
        );
    }
}

fn emit_managed_block(rep: &Value, err: bool) {
    let mb = rep.get("managed_block").cloned().unwrap_or(json!({}));
    if mb.get("stale") == Some(&Value::Bool(true)) {
        emit(
            err,
            &format!(
                "fno doctor: {} footnote block is v{} (current v{}); re-run `fno config setup` to refresh it.",
                mb.get("file").and_then(Value::as_str).unwrap_or("?"),
                mb.get("stamped").and_then(Value::as_i64).unwrap_or(0),
                mb.get("current").and_then(Value::as_i64).unwrap_or(1)
            ),
        );
    }
}

fn emit_surface_lines(rep: &Value, err: bool) {
    let surf = rep.get("harness_surface").cloned().unwrap_or(json!({}));
    for line in surf
        .get("opencode_doctor_lines")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        if let Some(text) = line.get("text").and_then(Value::as_str) {
            emit(err, &format!("fno doctor: {text}"));
        }
    }
    emit_codex_context_window(rep, err);
    let plugin = surf.get("codex_plugin").cloned().unwrap_or(json!({}));
    let status = plugin
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status == "fresh" {
        emit(
            err,
            &format!(
                "fno doctor: codex plugin: fresh (channel={} version={} digest={}).",
                plugin.get("channel").and_then(Value::as_str).unwrap_or("?"),
                plugin
                    .get("cache_version")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                short12(
                    plugin
                        .get("cache_digest")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                )
            ),
        );
    } else if !status.is_empty() {
        let detail = plugin
            .get("issue")
            .and_then(Value::as_str)
            .filter(|i| !i.is_empty())
            .unwrap_or(status);
        let enabled: Vec<String> = plugin
            .get("enabled_plugin_ids")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let remedy = plugin
            .get("remedy")
            .and_then(Value::as_str)
            .unwrap_or("None");
        emit(
            err,
            &format!(
                "fno doctor: codex plugin: {} ({}; enabled={}); run `{}`.",
                status.to_uppercase(),
                detail,
                if enabled.is_empty() {
                    "none".to_string()
                } else {
                    enabled.join(", ")
                },
                remedy
            ),
        );
    }
}

fn emit_codex_context_window(rep: &Value, err: bool) {
    let report = rep
        .get("harness_surface")
        .and_then(|h| h.get("codex_context_window"))
        .cloned()
        .unwrap_or(json!({}));
    let overstated = report.get("overstated") == Some(&Value::Bool(true));
    let leans = report.get("leans_on_cached_cap") == Some(&Value::Bool(true));
    if !overstated && !leans {
        return;
    }
    let kept = report
        .get("percent")
        .and_then(Value::as_f64)
        .map(|p| format!(" (codex keeps {p})"))
        .unwrap_or_default();
    let mut line = format!(
        "fno doctor: codex {}={} with {}={} runs at an effective {}{kept}.",
        s(&report, "model_source"),
        s(&report, "model"),
        s(&report, "window_source"),
        report
            .get("configured")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        report
            .get("effective")
            .map(|v| v.to_string())
            .unwrap_or_default()
    );
    if overstated {
        line.push_str(" The TUI footer shows the configured value, not this one.");
    }
    if leans {
        let cap = report
            .get("max_context_window")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let base = report
            .get("context_window")
            .and_then(Value::as_i64)
            .unwrap_or(cap);
        let stamp = opt_s(&report, "cache_fetched_at").unwrap_or_default();
        let client = opt_s(&report, "cache_client_version").unwrap_or_default();
        let fetched = match (stamp.is_empty(), client.is_empty()) {
            (false, false) => format!("(fetched {stamp} by codex {client})"),
            (false, true) => format!("(fetched {stamp})"),
            (true, false) => format!("(written by codex {client})"),
            (true, true) => "(the cache records no fetch time)".to_string(),
        };
        let model = s(&report, "model");
        let pct = report.get("percent").and_then(Value::as_f64);
        let base_tier = pct
            .map(|p| (base as f64 * p / 100.0).floor() as i64)
            .unwrap_or(base);
        let provenance = format!(
            "{fetched}. That cap is served per fetching client, surface and originator both, and the cache records neither"
        );
        let synthesized = report.get("base_synthesized") == Some(&Value::Bool(true));
        if cap > base {
            line.push_str(&format!(
                " models_cache.json holds {model} at {cap}, above its {base} base {provenance}. A base-tier fetch drops this to {base_tier}."
            ));
        } else if cap == base && !synthesized {
            line.push_str(&format!(
                " models_cache.json caps {model} at {cap}, its base {provenance}, so whichever launcher fetched last set it for every thread started since."
            ));
        } else {
            line.push_str(&format!(
                " models_cache.json puts {model} at {cap} {provenance}, so whichever launcher fetched last set it for every thread started since."
            ));
        }
        let daemon_live = report.get("app_server_running") == Some(&Value::Bool(true));
        if daemon_live {
            line.push_str(" A codex app-server daemon is live here and every app-server fetch lands the base cap, so it will keep pulling this back down.");
        }
    }
    emit(err, &line);
}

fn emit_groom(rep: &Value, err: bool) {
    let gr = rep.get("groom").cloned().unwrap_or(json!({}));
    let state = gr.get("state").and_then(Value::as_str).unwrap_or_default();
    let agent_installed = gr.get("agent_installed") == Some(&Value::Bool(true));
    if state == "never" {
        let remedy = if agent_installed {
            " despite an installed agent; check ~/.fno/logs/groom.err.log."
        } else {
            "; run `fno backlog groom --install-agent` to schedule it daily."
        };
        emit(
            err,
            &format!("fno doctor: backlog grooming has NEVER run{remedy}"),
        );
    } else if gr.get("stale") == Some(&Value::Bool(true)) {
        emit(err, &format!(
            "fno doctor: backlog grooming last ran {}h ago (the daily pass is not running); check `launchctl list | grep sh.fno.groom` and ~/.fno/logs/groom.err.log.",
            gr.get("hours").and_then(Value::as_f64).unwrap_or(0.0) as i64
        ));
    }
}

fn emit_post_merge_sync(rep: &Value, err: bool) {
    let pms = rep.get("post_merge_sync").cloned().unwrap_or(json!({}));
    if pms.get("stale") == Some(&Value::Bool(true)) {
        emit(err, &format!(
            "fno doctor: post-merge sync STALE - the canonical checkout is not synced with recent merges ({}); run `fno do pr sync-canonical --pr-number <n>` and check ~/.fno/logs/pr-watcher.err.log.",
            pms.get("detail").and_then(Value::as_str).unwrap_or_default()
        ));
    } else if pms.get("state").and_then(Value::as_str) == Some("unknown") {
        emit(err, &format!(
            "fno doctor: post-merge sync UNKNOWN - could not read merge state ({}); run `gh auth status`.",
            pms.get("detail").and_then(Value::as_str).filter(|d| !d.is_empty()).unwrap_or("gh unavailable or unauthenticated")
        ));
    }
}

fn emit_evals(rep: &Value, err: bool) {
    let ev = rep.get("evals").cloned().unwrap_or(Value::Null);
    if ev.is_null() || ev.get("never_ran") == Some(&Value::Bool(true)) {
        let detail = if ev.is_null() {
            "no eval history"
        } else {
            "no regression-tier run on record"
        };
        emit(
            err,
            &format!(
            "fno doctor: evals UNKNOWN ({detail}); run `fno doctor evals run --tier regression -y`."
        ),
        );
    } else if ev.get("stale") == Some(&Value::Bool(true)) {
        emit(err, &format!(
            "fno doctor: evals STALE - the newest regression-tier run is {}d old; run `fno doctor evals run --tier regression -y`.",
            ev.get("age_days").and_then(Value::as_i64).unwrap_or(0)
        ));
    } else if ev
        .get("regressed")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        let names: Vec<String> = ev
            .get("regressed")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let window = ev.get("window_days").and_then(Value::as_i64).unwrap_or(7);
        emit(err, &format!(
            "fno doctor: evals REGRESSING - {} dropped against the prior {window}d window; run `fno doctor evals trend`.",
            names.join(", ")
        ));
    }
}

fn emit_launch_agents(rep: &Value, err: bool) {
    let agents = rep.get("launch_agents").cloned().unwrap_or(json!({}));
    if agents.get("applicable") == Some(&Value::Bool(false)) {
        emit(
            err,
            "fno doctor: LaunchAgent health: not applicable (no launchctl on this host).",
        );
    }
    for entry in agents
        .get("dead")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        let label = entry.get("label").and_then(Value::as_str).unwrap_or("?");
        let remedy = if label == "sh.fno.pr-watcher" {
            "run `fno do pr watch refresh`"
        } else {
            "re-run `fno doctor update` if the entry point moved"
        };
        let exit_code = entry.get("exit");
        let exit_text = exit_code
            .map(|e| e.to_string())
            .unwrap_or_else(|| "exit unknown".to_string());
        emit(err, &format!(
            "fno doctor: LaunchAgent {label} last exited {exit_text} (it is installed but failing); check its log under ~/.fno/ and {remedy}."
        ));
    }
}

fn emit_silent_switches(rep: &Value, err: bool) {
    let ss = rep.get("silent_switches").cloned().unwrap_or(json!({}));
    let stamp = ss.get("posture").cloned().unwrap_or(json!({}));
    if let Some(p) = stamp.get("posture").and_then(Value::as_str) {
        emit(
            err,
            &format!(
                "fno doctor: applied posture: {p} (applied {}, scope {}).",
                stamp
                    .get("applied_at")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into()),
                stamp
                    .get("scope")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".into())
            ),
        );
    }
    for f in ss
        .get("findings")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        emit_silent_finding(&f, err);
    }
}

fn emit_silent_finding(f: &Value, err: bool) {
    let sw = s(f, "switch");
    let cmd = s(f, "command");
    let count = f.get("count").map(|c| c.to_string()).unwrap_or_default();
    let label = s(f, "count_label");
    if s(f, "direction") == "inaction" {
        let clause = if count.is_empty() {
            " (idle)".to_string()
        } else {
            format!(" but {count} {label} waiting")
        };
        emit(
            err,
            &format!(
                "fno doctor: {sw} is OFF{clause}; nothing is happening. Run `{cmd}` to enable."
            ),
        );
    } else {
        let clause = if count.is_empty() {
            String::new()
        } else {
            format!(" ({count} {label})")
        };
        emit(err, &format!(
            "fno doctor: {sw} is ARMED{clause}; a green PR can merge unattended. Run `{cmd}` to disarm."
        ));
    }
    if let Some(cause) = opt_s(f, "cause") {
        emit(err, &format!("  cause: {cause}"));
    }
}

fn emit_review_gap(rep: &Value, err: bool) {
    let gap = rep
        .get("auto_merge_review_gap")
        .cloned()
        .unwrap_or(Value::Null);
    if gap.is_null() {
        return;
    }
    let manifests = gap.get("manifests").and_then(Value::as_i64).unwrap_or(0);
    let breakdown = s(&gap, "breakdown");
    let label = if breakdown.is_empty() {
        format!("{manifests} armed manifest(s)")
    } else {
        format!("{manifests} armed manifest(s): {breakdown}")
    };
    let keys: Vec<String> = gap
        .get("keys")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    emit(
        err,
        &format!(
            "fno doctor: auto_merge is ARMED with zero review lanes - the merge gate requires no review for a code payload ({}; {label}). Remedy: {}",
            keys.join("; "),
            s(&gap, "remedy")
        ),
    );
}

fn emit_optout_residue(rep: &Value, err: bool) {
    let findings = rep
        .get("merge_gating_optouts")
        .and_then(|m| m.get("residue"))
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default();
    for f in findings {
        emit(
            err,
            &format!(
                "fno doctor: {} remains set to its opt-out value in {} without a live claim ({}); run `{}` or let the reaper restore it.",
                s(&f, "key"), s(&f, "path"), s(&f, "claim_state"), s(&f, "command")
            ),
        );
    }
}

fn emit_plugin_cache(rep: &Value, err: bool) {
    let pc = rep.get("plugin_cache").cloned().unwrap_or(json!({}));
    for root in pc
        .get("roots")
        .and_then(Value::as_array)
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        let role = if root.get("live") == Some(&Value::Bool(true)) {
            "live"
        } else {
            "second copy"
        };
        emit(
            err,
            &format!(
                "fno doctor: plugin root ({}, {role}): {}: {}{}",
                s(&root, "origin"),
                s(&root, "path"),
                s(&root, "status"),
                s(&root, "note")
            ),
        );
    }
    if !pc.is_null()
        && pc
            .get("roots")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty())
    {
        emit(
            err,
            &format!(
                "fno doctor: plugin stage freshness unknown ({}).",
                s(&pc, "detail")
            ),
        );
    }
}

fn emit_codex_dual(rep: &Value, err: bool) {
    let dual = rep
        .get("harness_surface")
        .and_then(|h| h.get("codex_hooks_dual"))
        .cloned()
        .unwrap_or(Value::Bool(false));
    if dual == Value::Bool(true) {
        emit(
            err,
            "fno doctor: codex hooks load from both config.toml and hooks.json; run `fno config setup cli-hooks-codex --migrate-legacy-hooks-json` to converge (or `fno doctor --codex-hooks` for detail)."
        );
    }
}

fn emit_pre_push(rep: &Value, err: bool) {
    let pp = rep.get("pre_push_hook").cloned().unwrap_or(json!({}));
    if s(&pp, "status") == "legacy" {
        emit(
            err,
            "fno doctor: the installed pre-push hook gates on the pushing checkout's branch, so it refuses every push from a canonical checkout on main. Run `bash scripts/install-pre-push-hook.sh` to replace it (the old file is backed up)."
        );
    }
}

fn emit_plugin_hooks(rep: &Value, err: bool) {
    let ph = rep.get("plugin_hooks").cloned().unwrap_or(json!({}));
    if ph.get("applicable") == Some(&Value::Bool(true)) {
        for merr in ph
            .get("manifest_errors")
            .and_then(Value::as_array)
            .map(|a| a.to_vec())
            .unwrap_or_default()
        {
            emit(
                err,
                &format!(
                "fno doctor: hook manifest {}: {} [{}] - that harness loads no hooks (fail-open).",
                s(&merr, "issue"), s(&merr, "path"), s(&merr, "manifest")
            ),
            );
        }
        for miss in ph
            .get("missing_scripts")
            .and_then(Value::as_array)
            .map(|a| a.to_vec())
            .unwrap_or_default()
        {
            emit(
                err,
                &format!(
                    "fno doctor: hook references MISSING script {} [{}/{}] - the guard fails open with no signal.",
                    s(&miss, "script"), s(&miss, "manifest"), s(&miss, "event")
                ),
            );
        }
        for failure in ph
            .get("failures")
            .and_then(Value::as_array)
            .map(|a| a.to_vec())
            .unwrap_or_default()
        {
            let stderr = s(&failure, "stderr");
            let tail = if stderr.is_empty() {
                String::new()
            } else {
                let cut: String = stderr.chars().take(160).collect();
                format!(" - {cut}")
            };
            emit(
                err,
                &format!(
                    "fno doctor: hook LAUNCH FAILED (rc={}): {} [{}/{}]{}",
                    failure
                        .get("rc")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "None".into()),
                    s(&failure, "resolved"),
                    s(&failure, "manifest"),
                    s(&failure, "event"),
                    tail
                ),
            );
        }
    } else if !s(&ph, "reason").is_empty() {
        emit(
            err,
            &format!(
                "fno doctor: plugin hook probe skipped ({}).",
                s(&ph, "reason")
            ),
        );
    }
}

fn emit_codex_foreign(rep: &Value, err: bool) {
    let foreign = rep
        .get("harness_surface")
        .and_then(|h| h.get("codex_hooks_foreign_json"))
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    if foreign > 0 {
        emit(err, &format!(
            "fno doctor: codex hooks: {foreign} foreign hook(s) in ~/.codex/hooks.json (informational; run `fno doctor --codex-hooks` for the per-command config.toml/hooks.json breakdown)."
        ));
    }
}

fn emit_codex_app_server(rep: &Value, err: bool) {
    let cas = rep.get("codex_app_server").cloned().unwrap_or(json!({}));
    if cas.get("present") != Some(&Value::Bool(true)) {
        emit(
            err,
            "fno doctor: codex app-server daemon not running (no daemon listening on its control socket); live mail to codex sessions demotes to durable. Start it BEFORE the codex TUI: `codex app-server daemon start` (or `codex app-server daemon bootstrap` for durable SSH-driven use). A session launched before the daemon cannot receive live mail without a restart."
        );
    }
}

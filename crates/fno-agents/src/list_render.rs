//! The `fno agents list` table path: fetch the host-local discovered lane,
//! filter it, and draw the registry and discovered tables. Verbatim move out
//! of bin/client.rs for the file budget (the same shape as client_render.rs,
//! which banked the first slice of that shrink). No behavior change.

use crate::client_render::{render_checked, truncate_cell, LAST_MESSAGE_WIDTH};
use serde_json::Value;

/// Per-column widths in chars, not bytes: the `{:<width$}` pad counts chars,
/// so a byte width on non-ASCII text (a CJK cwd, an emoji message) pads past
/// the intended column and shoves the rest of the row wide.
fn display_widths<const N: usize>(headers: [&str; N], display: &[[String; N]]) -> [usize; N] {
    let mut widths = headers.map(str::len);
    for row in display {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    widths
}

/// Shell out to the Python `fno agents discovered-json` helper for the P1
/// discovered-live-sessions lane and return the rows.
///
/// The Rust client owns the `list` rendered surface, but discovery lives in
/// Python (it needs psutil's cross-platform process create-time for the
/// reuse-safe liveness the design requires; the Rust-native liveness degrades
/// to existence-only on macOS). Fail-open by contract: a missing `fno`, a
/// non-zero exit, or unparseable output yields an empty lane so `list` is
/// never broken by discovery (US5). `FNO_AGENTS_RUNTIME=python` pins the child
/// to the Python dispatch so it cannot recurse back into this binary.
pub fn fetch_discovered_sessions(
    cwd_filter: Option<&str>,
    provider_filter: Option<&str>,
    status_filter: Option<&str>,
    progress_filter: Option<&str>,
) -> Vec<Value> {
    use std::process::Command;

    // No live-only early return. It encoded "a discovered session is live by
    // definition", which the shared reachability verdict retired: a discovered
    // row whose process is provably gone now comes back `orphaned`, so the
    // early return made `--status orphaned` drop through Rust a row that Python
    // prints -- one runtime-dependent answer to one question. The row-level
    // filter at the bottom is the single place status is applied.
    let mut argv = vec!["agents".to_string(), "discovered-json".to_string()];
    if let Some(c) = cwd_filter {
        argv.push("--cwd".into());
        argv.push(c.into());
    }
    // Without this the rendered surface disagrees with the Python one:
    // `--harness claude` would list every discovered codex/opencode session.
    // An empty value is "no filter" on the Python side, so forwarding it would
    // make the two runtimes disagree again in the other direction.
    if let Some(p) = provider_filter.filter(|p| !p.is_empty()) {
        argv.push("--harness".into());
        argv.push(p.into());
    }

    // Every `fno agents list` on the box runs this, so several terminals asking
    // at once each paid their own Python cold start for one answer. The latch
    // is keyed on the argv above, so a run with a different `--cwd` or
    // `--harness` is a different flight and keeps its own answer.
    let key = crate::single_flight::flight_key(&argv);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let flight = crate::single_flight::run_or_join(
        &key,
        crate::agents_config::single_flight_ttl(&cwd),
        crate::agents_config::single_flight_join_budget(&cwd),
        // No outer deadline to subtract from: this path has no caller-supplied
        // budget, so the wait it may have spent changes nothing about the run.
        |_spent| {
            let mut cmd = Command::new(crate::scrape::fno_bin());
            cmd.args(&argv);
            cmd.env("FNO_AGENTS_RUNTIME", "python");
            // Fail-open by contract, and the same rule the latch needs: only a
            // clean run is worth sharing, so a failure spends no cache entry
            // and the next caller retries for real.
            match cmd.output() {
                Ok(o) if o.status.success() => Some(o.stdout),
                _ => None,
            }
        },
    );
    let output = match flight.stdout {
        Some(bytes) => bytes,
        None => return Vec::new(),
    };
    let parsed: Value = match serde_json::from_slice(&output) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut rows: Vec<Value> = parsed
        .get("discovered_sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    retain_discovered_by_status(&mut rows, status_filter);
    retain_discovered_by_progress(&mut rows, progress_filter);
    rows
}

/// Keep only the discovered rows whose OWN verdict matches the requested one.
///
/// Split out so it is assertable without the shellout above. The test that
/// used to cover this pinned the early return (`a non-live filter yields
/// nothing`), and once that return was gone the same assertion still passed --
/// vacuously, because the subprocess answers with nothing under test. An
/// assertion that survives the behavior it describes is not covering it.
fn retain_discovered_by_status(rows: &mut Vec<Value>, status_filter: Option<&str>) {
    // A discovered session whose process is provably gone now comes back
    // `orphaned` (the shared reachability verdict), so a live-only run that
    // trusted the CALLER's filter alone would print an orphaned row under a
    // banner that says LIVE.
    if let Some(want) = status_filter {
        rows.retain(|r| r.get("status").and_then(Value::as_str) == Some(want));
    }
}

fn retain_discovered_by_progress(rows: &mut Vec<Value>, progress_filter: Option<&str>) {
    if let Some(want) = progress_filter {
        rows.retain(|r| r.get("progress").and_then(Value::as_str) == Some(want));
    }
}

/// Compact single-unit age for the CHECKED column: the largest whole unit of
/// the elapsed seconds -- `3s`, `4m`, `18h`, `2d` (plan, AC2-EDGE).
/// Negative input (a row reconciled in the "future" via clock skew) clamps to
/// `0s` rather than rendering a misleading negative age.
/// Render agents list as a human-readable table: ROW NAME SESSION HARNESS
/// MODEL EFFORT PR AGE LAST MESSAGE STATUS. MODEL and PR name the basis they
/// were read from, so an absent value never reads as unset. AGE is the age of
/// the transcript's newest activity, beside LAST MESSAGE and STATUS on
/// purpose: a row claiming to be busy while its transcript is hours old shows
/// the disagreement. JSON keeps every column this table drops.
pub fn render_list_table(
    agents: &Value,
    discovered: &[Value],
    truth_probe_asked: Option<u64>,
    truth_probe_answered: Option<u64>,
    retired: &Value,
) -> String {
    let headers = [
        "ROW",
        "NAME",
        "SESSION",
        "HARNESS",
        "MODEL",
        "EFFORT",
        "PR",
        "AGE",
        "LAST MESSAGE",
        "STATUS",
    ];
    let empty_arr = vec![];
    let rows = agents.as_array().unwrap_or(&empty_arr);
    let now = chrono::Utc::now();
    let text = |r: &Value, key: &str| {
        r[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
            .to_string()
    };

    let display: Vec<[String; 10]> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            // The full id, never truncated: the first eight are the address.
            let session = r["harness_session_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    // Claude's legacy `session_id` is its short transport id,
                    // not the full session id promised by this column.
                    (r["harness"].as_str() != Some("claude")).then(|| text(r, "session_id"))
                })
                .unwrap_or_else(|| "unknown".to_string());
            // `unknown`, not `never`: no transcript was READ, which is an
            // absent reading, not a claim that no event ever happened.
            let age = match r["last_event_at"].as_str() {
                Some(ts) => render_checked(Some(ts), now),
                None => "unknown".to_string(),
            };
            let last_msg = r["last_message"]
                .as_str()
                .map(|s| truncate_cell(s, LAST_MESSAGE_WIDTH))
                .unwrap_or_else(|| "-".to_string());
            [
                (i + 1).to_string(),
                text(r, "name"),
                session,
                text(r, "harness"),
                crate::list_row::model_cell(r),
                text(r, "effort"),
                crate::list_row::pr_cell(r),
                age,
                last_msg,
                text(r, "status"),
            ]
        })
        .collect();

    let widths = display_widths(headers, &display);

    let mut lines = Vec::new();
    // The instrument's receipt, in the artifact itself: a total
    // outage must not read as a wall of `unknown` statuses the reader was
    // meant to trust. The daemon's stderr WARN is write-only; this line is
    // the one the operator actually sees.
    if truth_probe_asked.unwrap_or(0) > 0 && truth_probe_answered == Some(0) {
        lines.push(format!(
            "truth probe failed: 0 of {} rows answered; every STATUS below is unmeasured, not healthy",
            truth_probe_asked.unwrap_or(0)
        ));
    }
    // Header row
    let header_line = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(header_line.trim_end().to_string());
    // Data rows
    for row in &display {
        let data_line = row
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{:width$}", cell, width = widths[i]))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(data_line.trim_end().to_string());
    }
    let mut out = lines.join("\n") + "\n";
    if !discovered.is_empty() {
        out.push_str(&render_discovered_section(discovered));
    }
    out.push_str(&crate::reap_render::retired_section(retired));
    out
}

/// Render the host-local discovered-live-sessions lane below the registry
/// table (AC1-UI). Columns: ADDRESS (the mailbox) LABEL (friendly alias)
/// STATUS PROJECT CWD. ADDRESS leads, matching the Python renderer:
/// `<project>-<short8>` is not an address. The value is read off the row
/// (`to_row` resolves it from the session's own harness), so this renderer
/// and the Python one cannot answer differently about the same session.
fn render_discovered_section(discovered: &[Value]) -> String {
    let headers = ["ADDRESS", "LABEL", "STATUS", "PROJECT", "CWD"];
    let display: Vec<[String; 5]> = discovered
        .iter()
        .map(|r| {
            let address = r["address"].as_str().unwrap_or("-").to_string();
            let label = r["handle"].as_str().unwrap_or("-").to_string();
            let status = r["status"].as_str().unwrap_or("-").to_string();
            let project = r["project"].as_str().unwrap_or("-").to_string();
            let cwd = r["cwd"].as_str().unwrap_or("-").to_string();
            [address, label, status, project, cwd]
        })
        .collect();

    let widths = display_widths(headers, &display);

    let mut lines = Vec::new();
    lines.push(String::new()); // blank separator line
    lines.push(format!(
        "DISCOVERED LIVE SESSIONS ({}, host-local)",
        display.len()
    ));
    lines.push(
        headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:width$}", h, width = widths[i]))
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end()
            .to_string(),
    );
    for row in &display {
        lines.push(
            row.iter()
                .enumerate()
                .map(|(i, cell)| format!("{:width$}", cell, width = widths[i]))
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end()
                .to_string(),
        );
    }
    lines.join("\n") + "\n"
}

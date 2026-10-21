//! `fno backlog calibration` - the planned-vs-actual read over shipped
//! nodes: `difficulty_history`/`cost_sessions` joined against the
//! node-tagged `friction` event stream (crate::backlog::calibration owns the
//! computation; this module owns argv, I/O and render).

use super::calibration::{
    aggregate, compute, friction_counts_by_node, friction_counts_by_node_checked,
};
use super::scorecard;
use serde_json::{json, Value};

struct CalibrationArgs {
    domain: Option<String>,
    json_out: bool,
    by_lane: bool,
    since: Option<i64>,
}

impl CalibrationArgs {
    fn parse(tail: &[String]) -> Option<CalibrationArgs> {
        let mut domain = None;
        let mut json_out = false;
        let mut by_lane = false;
        let mut since = None;
        let mut i = 0;
        while i < tail.len() {
            match tail[i].as_str() {
                "--domain" | "-d" => {
                    i += 1;
                    domain = Some(tail.get(i)?.clone());
                }
                "--json" | "-J" => json_out = true,
                "--by-lane" => by_lane = true,
                "--since" => {
                    i += 1;
                    since = Some(tail.get(i)?.parse().ok()?);
                }
                // No positional arguments: any other token (flag-shaped or
                // not) is a usage error.
                _ => return None,
            }
            i += 1;
        }
        // --since alone names no view the plain read takes, so it is a usage
        // error rather than a silently ignored flag.
        if since.is_some() && !by_lane {
            return None;
        }
        Some(CalibrationArgs {
            domain,
            json_out,
            by_lane,
            since,
        })
    }
}

const CALIBRATION_HELP: &str = "Join a shipped node's filed difficulty/size against its actual cost and friction\n\nUsage: fno backlog calibration [OPTIONS]\n\nOptions:\n  -d, --domain <DOMAIN>  Filter to one domain\n  -J, --json             Emit {\"nodes\": [...], \"aggregate\": [...]}\n  --by-lane              Fold work into (kind, harness, provider, model) lane cells over the window\n  --since <DAYS>         Scorecard window in days, default 28 (requires --by-lane)\n  -h, --help             Print help";

pub fn run(tail: &[String]) -> i32 {
    if tail.iter().any(|t| t == "--help" || t == "-h") {
        println!("{CALIBRATION_HELP}");
        return 0;
    }
    let Some(args) = CalibrationArgs::parse(tail) else {
        eprintln!(
            "fno backlog calibration: usage: fno backlog calibration [-d DOMAIN] [-J] [--by-lane [--since DAYS]] (--help for detail)"
        );
        return 2;
    };
    // Mirrors session_cli.rs/create_cli.rs: calibration reads the local
    // graph directly and has no Python/external-tracker leg to forward to
    // (unlike get/find), so an external backend must refuse rather than
    // silently compute over a store the tracker no longer treats as
    // authoritative. The by-lane scorecard shares the refusal: it reads the
    // same store.
    if crate::graph_get::external_backend_selected() {
        eprintln!(
            "fno backlog calibration: this verb reads graph state directly; under an \
external tracker backend it is refused rather than computed over a non-authoritative store."
        );
        return 1;
    }
    if args.by_lane {
        let since_days = args.since.unwrap_or(28);
        if since_days < 1 || since_days > 36_500 {
            eprintln!("fno backlog calibration: --since takes 1 to 36500 days");
            return 2;
        }
        return run_by_lane(since_days, args.json_out, args.domain.as_deref());
    }
    let graph_path = super::settings::graph_path();
    let query = super::RowQuery {
        filter: super::api::NodeFilter {
            status_in: Some(vec!["done".to_string()]),
            ..Default::default()
        },
        ..Default::default()
    };
    let nodes = match super::read_entries_where(&graph_path, &query) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("fno backlog calibration: could not read the graph cleanly: {err}");
            return super::get_cli::GRAPH_UNREADABLE_EXIT;
        }
    };
    let nodes: Vec<Value> = match &args.domain {
        Some(d) => nodes
            .into_iter()
            .filter(|n| n.get("domain").and_then(Value::as_str) == Some(d.as_str()))
            .collect(),
        None => nodes,
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let journal = crate::paths::events_path(&cwd);
    let friction_by_node = friction_counts_by_node(&journal);
    let rows = compute(&nodes, &friction_by_node);
    let agg = aggregate(&rows);
    if args.json_out {
        println!(
            "{}",
            super::render::py_json_pretty(&json!({"nodes": rows, "aggregate": agg}))
        );
        return 0;
    }
    if rows.is_empty() {
        eprintln!("fno backlog calibration: no done node carries a cost_sessions row yet");
        return 0;
    }
    for r in &rows {
        println!(
            "{}\t{}\t{}\t${:.2}\t{}session(s)\t{}friction\t{}",
            r.id,
            r.planned_difficulty.as_deref().unwrap_or("?"),
            r.domain.as_deref().unwrap_or("-"),
            r.actual_cost_usd,
            r.session_count,
            r.friction.values().sum::<u32>(),
            if r.smooth { "smooth" } else { "rough" },
        );
    }
    println!("\n-- by domain/difficulty --");
    for a in &agg {
        println!(
            "{}/{}\t{} node(s)\t{} smooth\tavg ${:.2}",
            a.domain.as_deref().unwrap_or("-"),
            a.planned_difficulty.as_deref().unwrap_or("?"),
            a.node_count,
            a.smooth_count,
            a.avg_cost_usd,
        );
    }
    0
}

/// The by-lane scorecard path: every live node, the native ledger read and
/// the friction journal, folded per lane cell. The lane axes and the cell
/// shape live in `scorecard`; this stays argv, reads and render.
fn run_by_lane(since_days: i64, json_out: bool, domain: Option<&str>) -> i32 {
    let graph_path = super::settings::graph_path();
    let query = super::RowQuery {
        filter: super::api::NodeFilter::default(),
        ..Default::default()
    };
    let nodes = match super::read_entries_where(&graph_path, &query) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("fno backlog calibration: could not read the graph cleanly: {err}");
            return super::get_cli::GRAPH_UNREADABLE_EXIT;
        }
    };
    let ledger = scorecard::LedgerIndex::load();
    let cwd = std::env::current_dir().unwrap_or_default();
    let journal = crate::paths::events_path(&cwd);
    // An unreadable friction store stays None down the fold, so the
    // friction columns read null instead of a measured zero.
    let (friction_by_node, friction_readable) = friction_counts_by_node_checked(&journal);
    // The review feed: one batched read over the same journal, keyed by PR.
    let (review_fails_by_pr, review_readable) = scorecard::review_fails_by_pr(&journal);
    let feeds = scorecard::OutcomeFeeds {
        review_fails_by_pr: review_readable.then_some(&review_fails_by_pr),
    };
    // The fold joins PR numbers only within the cwd's own repo: the origin
    // remote's host/owner/repo, or None when no remote reads (every node
    // then keeps the slice-1 in-scope read).
    let repo_scope = crate::finalize::repo_identity_from_git_remote(&cwd);
    let mut built = scorecard::build_records(
        &nodes,
        &ledger,
        friction_readable.then_some(&friction_by_node),
        &feeds,
        repo_scope.as_deref(),
        since_days,
        chrono::Local::now().naive_local(),
        domain,
    );
    // The fix-of-fix feed walks the merged line once, narrowed to the PRs
    // this fold carries; a read failure or a non-repo cwd leaves the column
    // null with its coverage saying so.
    let fix_by_pr = scorecard::fix_by_pr(&built.records, &cwd, since_days);
    if let Some(fix_by_pr) = &fix_by_pr {
        scorecard::stamp_fix_of_fix(&mut built.records, fix_by_pr);
    }
    let cells = scorecard::fold_cells(&built.records);
    let cov = scorecard::coverage(&built.records, built.dropped_unparsed);
    if json_out {
        println!(
            "{}",
            super::render::py_json_pretty(&json!({
                "since_days": since_days,
                "coverage": cov,
                "cells": cells,
            }))
        );
        return 0;
    }
    if cells.is_empty() {
        println!(
            "fno backlog calibration --by-lane: no node has an execute session ending in the last {since_days}d"
        );
        return 0;
    }
    println!("fno backlog calibration --by-lane (last {since_days}d)\n");
    println!("Coverage");
    println!("  nodes in window:     {}", cov["nodes"]);
    for (label, key) in [
        ("harness", "harness_pct"),
        ("provider", "provider_pct"),
        ("model", "model_pct"),
        ("cost", "cost_pct"),
    ] {
        println!("  {label:<19}{}%", cov[key]);
    }
    let dropped = cov["dropped_unparsed"].as_i64().unwrap_or(0);
    if dropped > 0 {
        println!("  dropped, session time unparseable: {dropped}");
    }
    println!(
        "\n  {:<16}{:<10}{:<14}{:<22}{:>6}{:>7}{:>13}{:>10}{:>10}{:>9}{:>9}{:>7}{:>7}{:>7}{:>6}",
        "kind",
        "harness",
        "provider",
        "model",
        "nodes",
        "merged",
        "ci95",
        "spend$",
        "$/merged",
        "med hrs",
        "bounce%",
        "ci/n",
        "red%",
        "find/n",
        "fof%"
    );
    for c in &cells {
        let fmt = |v: &Value| -> String {
            v.as_f64()
                .map(|f| {
                    if f.fract() == 0.0 {
                        format!("{}", f as i64)
                    } else {
                        format!("{f}")
                    }
                })
                .unwrap_or_else(|| "n/a".to_string())
        };
        println!(
            "  {:<16}{:<10}{:<14}{:<22}{:>6}{:>7}{:>13}{:>10}{:>10}{:>9}{:>9}{:>7}{:>7}{:>7}{:>6}",
            c["kind"].as_str().unwrap_or_default(),
            c["harness"].as_str().unwrap_or_default(),
            c["provider"].as_str().unwrap_or_default(),
            c["model"].as_str().unwrap_or_default(),
            c["nodes"].as_i64().unwrap_or(0),
            c["merged"].as_i64().unwrap_or(0),
            c["merged_ci95"]
                .as_array()
                .map(|a| format!(
                    "[{:.2} {:.2}]",
                    a.first().and_then(Value::as_f64).unwrap_or(0.0),
                    a.last().and_then(Value::as_f64).unwrap_or(0.0)
                ))
                .unwrap_or_else(|| "n/a".to_string()),
            // An unmeasured spend prints n/a, never a fake 0.00.
            c["spend_usd"]
                .as_f64()
                .map(|f| format!("{f:.2}"))
                .unwrap_or_else(|| "n/a".to_string()),
            fmt(&c["cost_per_merged_usd"]),
            fmt(&c["median_work_hours"]),
            fmt(&c["bounce_pct"]),
            c["ci_failure_per_node"]
                .as_f64()
                .map(|f| format!("{f:.2}"))
                .unwrap_or_else(|| "n/a".to_string()),
            c["ci_red_pct"]
                .as_f64()
                .map(|f| format!("{f}"))
                .unwrap_or_else(|| "n/a".to_string()),
            c["review_findings_per_node"]
                .as_f64()
                .map(|f| format!("{f:.2}"))
                .unwrap_or_else(|| "n/a".to_string()),
            c["fix_of_fix_pct"]
                .as_f64()
                .map(|f| format!("{f}"))
                .unwrap_or_else(|| "n/a".to_string()),
        );
    }
    0
}

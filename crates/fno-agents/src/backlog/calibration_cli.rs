//! `fno backlog calibration` - the planned-vs-actual read over shipped
//! nodes: `difficulty_history`/`cost_sessions` joined against the
//! node-tagged `friction` event stream (crate::backlog::calibration owns the
//! computation; this module owns argv, I/O and render).

use super::calibration::{aggregate, compute, friction_counts_by_node, NodeCalibration};
use serde_json::{json, Value};

struct CalibrationArgs {
    domain: Option<String>,
    json_out: bool,
}

impl CalibrationArgs {
    fn parse(tail: &[String]) -> Option<CalibrationArgs> {
        let mut domain = None;
        let mut json_out = false;
        let mut i = 0;
        while i < tail.len() {
            match tail[i].as_str() {
                "--domain" | "-d" => {
                    i += 1;
                    domain = Some(tail.get(i)?.clone());
                }
                "--json" | "-J" => json_out = true,
                // No positional arguments: any other token (flag-shaped or
                // not) is a usage error.
                _ => return None,
            }
            i += 1;
        }
        Some(CalibrationArgs { domain, json_out })
    }
}

const CALIBRATION_HELP: &str = "Join a shipped node's filed difficulty/size against its actual cost and friction\n\nUsage: fno backlog calibration [OPTIONS]\n\nOptions:\n  -d, --domain <DOMAIN>  Filter to one domain\n  -J, --json             Emit {\"nodes\": [...], \"aggregate\": [...]}\n  -h, --help             Print help";

fn row_to_json(row: &NodeCalibration) -> Value {
    json!({
        "id": row.id,
        "title": row.title,
        "domain": row.domain,
        "planned_difficulty": row.planned_difficulty,
        "re_estimated": row.re_estimated,
        "planned_size": row.planned_size,
        "actual_cost_usd": row.actual_cost_usd,
        "session_count": row.session_count,
        "duration_hours": row.duration_hours,
        "friction": row.friction,
        "smooth": row.smooth,
    })
}

pub fn run(tail: &[String]) -> i32 {
    if tail.iter().any(|t| t == "--help" || t == "-h") {
        println!("{CALIBRATION_HELP}");
        return 0;
    }
    let Some(args) = CalibrationArgs::parse(tail) else {
        eprintln!(
            "fno backlog calibration: usage: fno backlog calibration [-d DOMAIN] [-J] (--help for detail)"
        );
        return 2;
    };
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
        let nodes_json: Vec<Value> = rows.iter().map(row_to_json).collect();
        let agg_json: Vec<Value> = agg
            .iter()
            .map(|a| {
                json!({
                    "domain": a.domain,
                    "planned_difficulty": a.planned_difficulty,
                    "node_count": a.node_count,
                    "smooth_count": a.smooth_count,
                    "avg_cost_usd": a.avg_cost_usd,
                    "avg_duration_hours": a.avg_duration_hours,
                })
            })
            .collect();
        println!(
            "{}",
            super::render::py_json_pretty(&json!({"nodes": nodes_json, "aggregate": agg_json}))
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

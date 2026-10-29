//! The scoreboard text renderers, ported byte-exactly from the Python
//! `_render*` functions in `cli/src/fno/scoreboard/cli.py`. The by-provider
//! view keeps its own renderer beside its fold (`scoreboard_provider::render`)
//! and the door prints that text verbatim. Python wrote through
//! `sys.stdout.write` with explicit newlines; each function here emits the
//! same bytes with `print!`.

use serde_json::{Map, Value};

/// Python `_fmt`: `n/a` when unmeasurable (never a fake 0), a plain integer
/// when whole (no sci-notation for million-scale token counts), else one
/// decimal.
fn fmt(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return "n/a".to_string();
    };
    match v {
        Value::Null => "n/a".to_string(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => match n.as_f64() {
                Some(f) if f.is_finite() && f.fract() == 0.0 => (f as i64).to_string(),
                Some(f) => format!("{f:.1}"),
                None => "n/a".to_string(),
            },
        },
        _ => "n/a".to_string(),
    }
}

/// Python `str()` on the numbers an f-string prints bare: serde's compact
/// form matches it for both shapes the folds emit (`42` and `42.0`).
fn num_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// `x or '?'` on the row's session id: an empty or missing string is `'?'`.
fn sid_or_q(row: &Value) -> String {
    row.get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("?")
        .to_string()
}

/// `x or '?'` on the row's pr_number: 0, empty, or absent is `'?'`.
fn pr_number_or_q(row: &Value) -> String {
    match row.get("pr_number") {
        Some(Value::Number(n)) if n.as_i64() == Some(0) => "?".to_string(),
        Some(Value::String(s)) if s.is_empty() => "?".to_string(),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    }
}

/// `_render_lanes`.
pub(crate) fn render_lanes(view: &Value) {
    let win = view.get("since_days").and_then(Value::as_i64).unwrap_or(28);
    print!("fno whoami scoreboard --lanes (last {win}d)\n\n");
    let cov = &view["coverage"];
    print!("Coverage\n");
    print!("  rows in window:  {}\n", num_str(cov.get("rows")));
    print!(
        "  provider:        {}/{}\n",
        num_str(cov.get("provider")),
        num_str(cov.get("rows"))
    );
    print!(
        "  model:           {}/{}\n",
        num_str(cov.get("model")),
        num_str(cov.get("rows"))
    );
    print!(
        "  effort:          {}/{}\n",
        num_str(cov.get("effort")),
        num_str(cov.get("rows"))
    );
    let rows_n = cov.get("rows").and_then(Value::as_i64).unwrap_or(0);
    let partial = ["provider", "model", "effort"]
        .iter()
        .any(|axis| cov.get(axis).and_then(Value::as_i64).unwrap_or(0) < rows_n);
    if partial {
        print!("  ! axis coverage is partial; missing values are not a model verdict.\n");
    }

    print!("\nRetrospective\n");
    let empty = Vec::new();
    let retro = view
        .get("retrospective")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if retro.is_empty() {
        print!("  no execution rows in window.\n");
    } else {
        print!("  provider          model                    effort size runs ok% wall-min carveouts sample\n");
        for row in retro {
            print!(
                "  {:<16} {:<24} {:<6} {:<4} {:>4} {:>3}% {:>8.1} {:>9} {}\n",
                row.get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                row.get("model").and_then(Value::as_str).unwrap_or_default(),
                row.get("effort")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                row.get("size").and_then(Value::as_str).unwrap_or_default(),
                num_str(row.get("runs")),
                num_str(row.get("ok_pct")),
                row.get("wall_minutes")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                num_str(row.get("carveouts_filed")),
                row.get("sample_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        }
    }

    print!("\nLive\n");
    let live = view.get("live").and_then(Value::as_array).unwrap_or(&empty);
    if live.is_empty() {
        print!("  no active lanes.\n");
    } else {
        print!("  provider          model                    effort occupancy cap headroom\n");
        for row in live {
            let cap = match row.get("cap") {
                Some(Value::Null) | None => "n/a".to_string(),
                Some(v) => num_str(Some(v)),
            };
            let headroom = match row.get("headroom") {
                Some(Value::Null) | None => "n/a".to_string(),
                Some(v) => num_str(Some(v)),
            };
            print!(
                "  {:<16} {:<24} {:<6} {:>9} {:>3} {:>8}\n",
                row.get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                row.get("model").and_then(Value::as_str).unwrap_or_default(),
                row.get("effort")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                num_str(row.get("occupancy")),
                cap,
                headroom,
            );
        }
    }
    let rate_limited = view.get("rate_limited").cloned().unwrap_or(Value::Null);
    print!(
        "\n  provider_rate_limited events: {}\n",
        num_str(Some(&rate_limited))
    );
}

/// `_render`, the main scoreboard.
pub(crate) fn render(sb: &Value) {
    let win = sb.get("since_days").and_then(Value::as_i64).unwrap_or(28);
    if sb["state"] == "no_data" {
        print!("fno whoami scoreboard (last {win}d)\n\n  no terminal sessions in window.\n");
        return;
    }
    let cov = &sb["coverage"];
    print!("fno whoami scoreboard (last {win}d)\n\n");
    print!("Coverage\n");
    print!("  rows in window:      {}\n", num_str(cov.get("rows")));
    print!(
        "  termination_reason:  {}%",
        num_str(cov.get("termination_reason_pct"))
    );
    print!(
        "    node linkage:  {}%\n",
        num_str(cov.get("node_linkage_pct"))
    );
    // Silent-failure guard: whenever coverage is partial on EITHER axis, the
    // caveat rides on the same screen as any rate below. Never a bare rate.
    let term_pct = cov
        .get("termination_reason_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let node_pct = cov
        .get("node_linkage_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if term_pct < 100 || node_pct < 100 {
        print!(
            "  ! rates below reflect {term_pct}% termination / {node_pct}% node-linkage: a partial window is not a trend.\n"
        );
    }
    // Journal integrity rides the same screen as any rate it could bias.
    let incomplete = |ec: &Value| !ec.get("complete").and_then(Value::as_bool).unwrap_or(true);
    if let Some(ec) = sb.get("event_coverage").filter(|ec| incomplete(ec)) {
        print!(
            "  ! event journals incomplete: {} malformed line(s), {} unreadable file(s); omissions, not zeros.\n",
            ec.get("malformed_lines").and_then(Value::as_i64).unwrap_or(0),
            ec.get("unreadable_paths").and_then(Value::as_i64).unwrap_or(0),
        );
    }
    let emit = sb.get("emission_failures");
    if let Some(emit) = emit.filter(|e| e.get("available").and_then(Value::as_bool) == Some(true)) {
        print!(
            "  ! touch emission failures: {} since {} (measured {}; server instance lifetime).\n",
            num_str(emit.get("count")),
            num_str(emit.get("measured_since")),
            num_str(emit.get("measured_at")),
        );
    } else if let Some(emit) = emit {
        print!(
            "  ! touch emission failures: unknown ({}).\n",
            emit.get("reason")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("server unreachable"),
        );
    }

    // shipped is the merge; the terminal count rides beside it for one release.
    if let Some(shipped) = sb.get("shipped_nodes").filter(|v| !v.is_null()) {
        let by_term = num_str(sb.get("shipped_by_terminal"));
        let classes = sb.get("delivery_classes").cloned().unwrap_or_default();
        let mut bits: Vec<String> = Vec::new();
        if let Some(obj) = classes.as_object() {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for name in keys {
                bits.push(format!("{} {name}", num_str(obj.get(name))));
            }
        }
        let no_row = sb
            .get("merged_nodes_without_ledger_row")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let shipped_s = num_str(Some(shipped));
        print!(
            "\nShipped       {shipped_s} nodes (confirmed merge, doc or delivery evidence); by session terminal alone: {by_term}\n"
        );
        if !bits.is_empty() {
            print!("              by evidence: {}\n", bits.join(" "));
        }
        if no_row != 0 {
            print!("              merged nodes with no ledger row: {no_row}\n");
        }
        let shipped_n = shipped.as_f64().unwrap_or(0.0);
        let by_term_n = by_term.parse::<f64>().unwrap_or(0.0);
        if shipped_n > 0.0 && by_term_n < 0.9 * shipped_n {
            print!(
                "  ! terminal-only undercounts nodes whose PR merged after the session stopped; the merge is the count.\n"
            );
        }
    }
    if let Some(scope) = sb.get("project_scope").filter(|s| !s.is_null()) {
        print!(
            "\nProject scope {}: {} nodes; {} unattributed, {} other-project row(s) kept out.\n",
            scope
                .get("project")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            num_str(scope.get("nodes")),
            num_str(scope.get("unattributed_rows")),
            num_str(scope.get("other_project_rows")),
        );
    }

    print!("\nStop-cause distribution\n");
    let stop_cause = sb.get("stop_cause").cloned().unwrap_or_default();
    let mut sc_items: Vec<(String, i64)> = stop_cause
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    if sc_items.is_empty() {
        print!("  (no termination_reason on any row in window)\n");
    } else {
        sc_items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (reason, n) in &sc_items {
            print!("  {reason:<14} {n}\n");
        }
    }

    let sp = sb.get("spend").cloned().unwrap_or_default();
    print!("\nSpend split\n");
    print!(
        "  ship-terminal:   ${:.2}\n",
        sp.get("ship_terminal_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    );
    print!(
        "  wedge-terminal:  ${:.2}\n",
        sp.get("wedge_terminal_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    );
    print!(
        "  other:           ${:.2}\n",
        sp.get("other_usd").and_then(Value::as_f64).unwrap_or(0.0)
    );

    print!("\nAutonomy      ");
    let au = sb.get("autonomy").cloned().unwrap_or_default();
    if au
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        print!(
            "{} human touches / shipped node ({} touches, {} nodes)\n",
            num_str(au.get("touches_per_shipped_node")),
            num_str(au.get("touches")),
            num_str(au.get("shipped_nodes")),
        );
    } else {
        print!(
            "n/a - {}\n",
            au.get("reason").and_then(Value::as_str).unwrap_or("")
        );
    }

    print!("Survival      ");
    let su = sb.get("survival").cloned().unwrap_or_default();
    if su
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let pending = su
            .get("pending")
            .and_then(Value::as_i64)
            .filter(|p| *p != 0);
        let suffix = match pending {
            Some(p) => format!(", {p} pending observation"),
            None => String::new(),
        };
        print!(
            "{}% ({}/{} shipped nodes{suffix})\n",
            num_str(su.get("rate_pct")),
            num_str(su.get("survived")),
            num_str(su.get("shipped_nodes")),
        );
    } else {
        print!(
            "n/a - {}\n",
            su.get("reason").and_then(Value::as_str).unwrap_or("")
        );
    }
}

/// `_render_calibration`.
pub(crate) fn render_calibration(cal: &Value) {
    print!("fno whoami scoreboard --calibration\n\n");
    let mut excl_bits: Vec<String> = Vec::new();
    if let Some(obj) = cal.get("excluded").and_then(Value::as_object) {
        if !obj.is_empty() {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                let n = obj[k].as_i64().unwrap_or(0);
                excl_bits.push(format!("{n} {k}"));
            }
        }
    }
    if let Some(u) = cal.get("unattributed").and_then(Value::as_i64) {
        if u != 0 {
            excl_bits.push(format!("{u} unattributed"));
        }
    }
    let excl_line = if excl_bits.is_empty() {
        String::new()
    } else {
        format!(" (excluded: {})", excl_bits.join(", "))
    };

    if cal["state"] == "insufficient" {
        let n = num_str(cal.get("n"));
        let need = num_str(cal.get("need"));
        print!("  {n} verdicts so far, need >={need} for calibration.{excl_line}\n");
        return;
    }

    let n = num_str(cal.get("n"));
    print!("  N={n} verdicts{excl_line}\n");
    if let Some(u) = cal.get("untimed_outcomes").and_then(Value::as_i64) {
        if u != 0 {
            print!("  ! {u} node(s) lack a timestamped ship row; their outcomes are conservative (any caused_by fix counts as bounced).\n");
        }
    }
    print!("\n");
    let outcomes = ["merged_clean", "bounced", "reverted"];
    print!("  {:<10}", "");
    for o in outcomes {
        print!("{o:>14}");
    }
    print!("\n");
    for verdict in ["pass", "concerns", "fail"] {
        print!("  {verdict:<10}");
        let row = cal
            .get("table")
            .and_then(|t| t.get(verdict))
            .cloned()
            .unwrap_or_default();
        for o in outcomes {
            let n = row.get(o).and_then(Value::as_i64).unwrap_or(0);
            print!("{n:>14}");
        }
        print!("\n");
    }
    let fp = cal.get("false_positive").cloned().unwrap_or_default();
    let count = num_str(fp.get("count"));
    let of_pass = num_str(fp.get("of_pass"));
    let rate = num_str(fp.get("rate_pct"));
    print!("\n  false-positive (pass -> bounced/reverted): {count}/{of_pass} ({rate}%)\n");
}

/// `_render_by_skill`.
pub(crate) fn render_by_skill(sb: &Value) {
    let win = sb.get("since_days").and_then(Value::as_i64).unwrap_or(28);
    if sb["state"] == "no_data" {
        print!(
            "fno whoami scoreboard --by-skill (last {win}d)\n\n  no terminal sessions in window.\n"
        );
        return;
    }
    let cov = &sb["coverage"];
    print!("fno whoami scoreboard --by-skill (last {win}d)\n\n");
    print!("Coverage\n");
    print!("  rows in window:      {}\n", num_str(cov.get("rows")));
    print!(
        "  attributed:          {}%\n",
        num_str(cov.get("attributed_pct"))
    );
    if cov
        .get("attributed_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        < 100
    {
        print!(
            "  ! rows below reflect {}% attribution coverage - unattributed rows are listed, never dropped.\n",
            num_str(cov.get("attributed_pct"))
        );
    }
    print!("\n");
    print!(
        "  {:<32}{:<10}{:>6}{:>7}{:>9}{:>11}{:>10}  method\n",
        "skill", "version", "runs", "ship%", "revert%", "touch/run", "cost/run"
    );
    let empty = Vec::new();
    for row in sb.get("rows").and_then(Value::as_array).unwrap_or(&empty) {
        let revert = match row.get("revert_rate_pct") {
            Some(Value::Null) | None => "n/a".to_string(),
            Some(v) => format!("{}%", num_str(Some(v))),
        };
        print!(
            "  {:<32}{:<10}{:>6}{:>6}%{revert:>9}{:>11}{:>10.2}  {}\n",
            row.get("skill").and_then(Value::as_str).unwrap_or_default(),
            row.get("version")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            row.get("runs").and_then(Value::as_i64).unwrap_or(0),
            row.get("ship_rate_pct")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            num_str(row.get("touches_per_run")),
            row.get("cost_per_run")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            row.get("method")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
    }
}

/// `_render_efficiency`.
pub(crate) fn render_efficiency(eff: &Value) {
    let win = eff.get("since_days").and_then(Value::as_i64).unwrap_or(28);
    if eff["state"] == "no_data" {
        print!("fno whoami scoreboard --efficiency (last {win}d)\n\n  no terminal sessions in window.\n");
        return;
    }
    let cov = &eff["coverage"];
    print!("fno whoami scoreboard --efficiency (last {win}d)\n\n");
    print!("Coverage\n");
    print!("  rows in window:      {}\n", num_str(cov.get("rows")));
    print!(
        "  loop-check join:     {}%",
        num_str(cov.get("loop_join_pct"))
    );
    print!("    transcript:  {}%", num_str(cov.get("transcript_pct")));
    print!(
        "    node linkage:  {}%\n",
        num_str(cov.get("node_linkage_pct"))
    );
    print!(
        "  outcome tracked:     {}% of shipped rows\n",
        num_str(cov.get("outcome_tracked_pct"))
    );
    let loop_join = cov
        .get("loop_join_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let node_link = cov
        .get("node_linkage_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if loop_join < 100 || node_link < 100 {
        print!(
            "  ! metrics below reflect {loop_join}% loop-join / {node_link}% node-linkage: a partial window is not a trend.\n"
        );
    }
    if let Some(u) = cov.get("ci_unparsed").and_then(Value::as_i64) {
        if u != 0 {
            print!("  ! {u} loop_check fire(s) carried an unrecognized ci shape (emitter drift); their sessions' ci_reds are n/a, not counted as green.\n");
        }
    }
    print!("\nPer-outcome-class cost\n");
    print!(
        "  {:<20}{:>4}{:>10}{:>10}{:>11}{:>9}\n",
        "class", "n", "spend$", "med tok", "med fires", "med min"
    );
    if let Some(obj) = eff.get("per_outcome_class").and_then(Value::as_object) {
        let mut items: Vec<(&String, &Value)> = obj.iter().collect();
        items.sort_by(|a, b| {
            let na = a.1.get("n").and_then(Value::as_i64).unwrap_or(0);
            let nb = b.1.get("n").and_then(Value::as_i64).unwrap_or(0);
            nb.cmp(&na).then_with(|| a.0.cmp(b.0))
        });
        for (cls, b) in items {
            print!(
                "  {cls:<20}{:>4}{:>10.2}{:>10}{:>11}{:>9}\n",
                b.get("n").and_then(Value::as_i64).unwrap_or(0),
                b.get("spend_usd").and_then(Value::as_f64).unwrap_or(0.0),
                fmt(b.get("median_tokens")),
                fmt(b.get("median_fires")),
                fmt(b.get("median_duration_min")),
            );
        }
    }

    if let Some(obj) = eff.get("plan_vs_build_cost").and_then(Value::as_object) {
        if !obj.is_empty() {
            print!("\nPlan vs build cost per node\n");
            print!("  {:<14}{:>10}{:>10}\n", "node", "plan$", "build$");
            let mut items: Vec<(&String, &Value)> = obj.iter().collect();
            items.sort_by(|a, b| {
                let pa = a.1.get("plan_usd").and_then(Value::as_f64).unwrap_or(0.0);
                let pb = b.1.get("plan_usd").and_then(Value::as_f64).unwrap_or(0.0);
                pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
            });
            for (nid, c) in items {
                print!(
                    "  {nid:<14}{:>10.2}{:>10.2}\n",
                    c.get("plan_usd").and_then(Value::as_f64).unwrap_or(0.0),
                    c.get("build_usd").and_then(Value::as_f64).unwrap_or(0.0),
                );
            }
        }
    }

    print!("\nDistribution (rows with >=1 loop_check fire)\n");
    print!(
        "  {:<18}{:>10}{:>10}{:>6}\n",
        "metric", "median", "p90", "n"
    );
    let empty = Map::new();
    for metric in ["loop_fires", "ci_reds", "tokens_total", "duration_minutes"] {
        let d = eff
            .get("distribution")
            .and_then(|v| v.get(metric))
            .cloned()
            .unwrap_or(Value::Object(empty.clone()));
        print!(
            "  {metric:<18}{:>10}{:>10}{:>6}\n",
            fmt(d.get("median")),
            fmt(d.get("p90")),
            d.get("n").and_then(Value::as_i64).unwrap_or(0),
        );
    }
}

/// `_render_plan_fidelity`.
pub(crate) fn render_plan_fidelity(pf: &Value) {
    let win = pf.get("since_days").and_then(Value::as_i64).unwrap_or(28);
    if pf["state"] == "no_data" {
        print!("fno whoami scoreboard --plan-fidelity (last {win}d)\n\n  no terminal sessions in window.\n");
        return;
    }
    let cov = &pf["coverage"];
    print!("fno whoami scoreboard --plan-fidelity (last {win}d)\n\n");
    print!(
        "  {} planned row(s), {}% joined to a delivery.\n\n",
        num_str(cov.get("planned_rows")),
        num_str(cov.get("joined_pct")),
    );
    let empty = Vec::new();
    for r in pf
        .get("results")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        if r.get("status").and_then(Value::as_str) == Some("unjoined") {
            print!("  {:<24} unjoined (plan not yet delivered)\n", sid_or_q(r));
            continue;
        }
        let ac_s = match r.get("ac_coverage").and_then(Value::as_object) {
            Some(ac) if !ac.is_empty() => format!(
                "{}/{} ({}%)",
                num_str(ac.get("verified")),
                num_str(ac.get("total")),
                num_str(ac.get("pct")),
            ),
            _ => "n/a".to_string(),
        };
        let dm = fmt(r.get("data_model_surprise"));
        let drift = match r.get("scope_drift").and_then(Value::as_object) {
            Some(sd) if !sd.is_empty() => sd
                .get("unplanned")
                .and_then(Value::as_array)
                .map(|a| a.len().to_string())
                .unwrap_or_else(|| "0".to_string()),
            _ => "n/a".to_string(),
        };
        let probes_s = match r.get("probes") {
            Some(pr) if !pr.is_null() => {
                let o = pr.as_object().map(|o| !o.is_empty()).unwrap_or(false);
                if o {
                    format!(
                        "{}/{}",
                        num_str(pr.get("passed")),
                        num_str(pr.get("declared")),
                    )
                } else {
                    "n/a".to_string()
                }
            }
            _ => "n/a".to_string(),
        };
        let ctx_s = match r
            .get("context_outcome_trace")
            .and_then(|t| t.get("context"))
        {
            Some(ctx) if !ctx.is_null() => match ctx.get("bytes") {
                Some(b) if !b.is_null() => format!("{}B", num_str(Some(b))),
                _ => "n/a".to_string(),
            },
            _ => "n/a".to_string(),
        };
        let outcome = match r.get("outcome").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => "n/a".to_string(),
        };
        print!(
            "  {sid:<24} PR#{pr} AC {ac_s} | drift {drift} | data-model-surprise {dm} | deviations {dev} | probes {probes_s} | context {ctx_s} | outcome {outcome}\n",
            sid = sid_or_q(r),
            pr = pr_number_or_q(r),
            dev = fmt(r.get("deviation_load")),
        );
    }
    let comparison = pf.get("context_comparison").cloned().unwrap_or_default();
    let label = comparison
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("rejected");
    let reason = comparison
        .get("reason")
        .cloned()
        .or_else(|| comparison.get("claim").cloned())
        .unwrap_or_else(|| Value::String("no contract".to_string()));
    print!(
        "\n  context comparison: {label} ({})\n",
        reason.as_str().unwrap_or("no contract"),
    );
}

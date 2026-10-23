//! The scorecard's router: one verdict per candidate inventory row per kind
//! under a stage command, folded from the by-lane cells. It proposes and never
//! applies: the accept line prints and `fno config set` stays the user's
//! hand, and every proposal names the live seat laws it would contradict.
//! The decision constants and the accept line are the climb fold's, so one
//! number and one config shape decide both folds.

use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A declared row needs this many nodes on a kind before its cell can read
/// a verdict; below it the router answers explore, never propose.
const EXPLORE_FLOOR: u64 = 5;

/// The seat law governing blueprint seating: blueprinting runs only on the
/// families below, never glm.
const BLUEPRINT_LAW: &str = "d-7b9e5ce4";

/// The d-7b9e5ce4 model families allowed to blueprint, matched by
/// normalized-model prefix. glm never matches: the law names it barred.
const BLUEPRINT_MODEL_PREFIXES: &[&str] = &["opus", "fable", "gpt-5.6-sol", "gpt-6-astra"];

/// The stage whose kinds route through the blueprint seat law.
const BLUEPRINT_STAGE: &str = "blueprint";

/// One declared inventory row, reduced to the facts a cell-to-lane match
/// and a proposal line need.
pub(crate) struct RowKey {
    pub name: String,
    pub harness: String,
    pub model_norm: String,
    /// The row's model id as declared, so a spawn or accept line names the
    /// model the lane really drives (brackets and all).
    pub model_raw: String,
    pub vendor: String,
    pub route: String,
}

impl RowKey {
    fn from_raw(raw: &Value) -> RowKey {
        let get = |k: &str| {
            raw.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let model = get("model");
        let model_id = get("model_id");
        let norm = if model_norm_is_empty(&model) {
            crate::context_window::normalize_model(&model_id)
        } else {
            crate::context_window::normalize_model(&model)
        };
        let route = get("route");
        let vendor = route.split('/').next().unwrap_or("").trim().to_string();
        let model_raw = if model.is_empty() { model_id } else { model };
        RowKey {
            name: get("name"),
            harness: get("harness"),
            model_norm: norm,
            model_raw,
            vendor,
            route,
        }
    }
}

fn model_norm_is_empty(model: &str) -> bool {
    crate::context_window::normalize_model(model).is_empty()
}

/// The declared inventory from the config at the current directory: the
/// routing rows plus the objective the grid orders them by. `None` reads
/// "nothing declared" - the router then proposes nothing and says so.
pub(crate) fn declared_inventory() -> Option<(Vec<RowKey>, Value)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let routing = crate::agents_config::config_lookup(&cwd, &["routing"])?;
    let models = routing.get("models").and_then(|v| v.as_array())?;
    let mut rows = Vec::new();
    for raw in models {
        let Ok(j) = serde_json::to_value(raw) else {
            continue;
        };
        let key = RowKey::from_raw(&j);
        if key.name.is_empty() || key.harness.is_empty() || key.model_norm.is_empty() {
            continue;
        }
        rows.push(key);
    }
    if rows.is_empty() {
        return None;
    }
    let objective = routing
        .get("objective")
        .and_then(|v| v.as_str())
        .unwrap_or("cheapest-that-clears")
        .to_string();
    let prefer = routing
        .get("prefer_harness")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let inventory = json!({
        "declared": true,
        "objective": objective,
        "prefer_harness": prefer,
        "rows": models
            .iter()
            .filter_map(|t| serde_json::to_value(t).ok())
            .collect::<Vec<Value>>(),
    });
    Some((rows, inventory))
}

/// The lane the command actually seats on one band: a declared lanes list
/// routes the command off the grid (the slot walk reads one lanes list for
/// every band), so the seated list decides first, and the grid names the
/// incumbent only for a command that declares none. Either way the pick is
/// declared-only: an empty capacity map reads every row
/// unknown-permitted, so no live capacity read shapes the incumbent.
fn incumbent_lane(stage: &str, difficulty: &str, inventory: &Value) -> Option<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Some(profile) = crate::agents_config::config_lookup(&cwd, &["agents", "profiles", stage])
    {
        let lanes_of = |t: &toml::Value| -> Option<String> {
            let arr = t.as_array()?;
            let first = arr.first()?;
            first.as_str().map(str::to_string).or_else(|| {
                first
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            })
        };
        let overlay = profile
            .get("by_difficulty")
            .and_then(|b| b.get(difficulty))
            .and_then(|b| b.get("lanes"))
            .and_then(lanes_of)
            .filter(|n| !n.is_empty());
        if overlay.is_some() {
            return overlay;
        }
        let top = profile
            .get("lanes")
            .and_then(lanes_of)
            .filter(|n| !n.is_empty());
        if top.is_some() {
            return top;
        }
    }
    let payload = json!({
        "inventory": inventory,
        "capacity": {},
        "harness_installed": {},
        "node": {"difficulty": difficulty, "priority": "p2"},
    });
    let mut chain = Vec::new();
    let pick =
        crate::route_slot::grid_leg(&payload, &format!("agents.profiles.{stage}"), &mut chain);
    pick.get("candidate")
        .and_then(|c| c.get("lane"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The declared rows one cell could have ridden: harness equal, normalized
/// model equal, and a route vendor (when the row names one) matching the
/// cell's provider. A row with no route is provider-agnostic, so a cell can
/// legitimately match several rows - the caller refuses that ambiguity with
/// a note instead of silently reading the first match.
/// The kind's cells that rode one row, by name. A provider-agnostic row
/// can match several cells; the caller refuses that ambiguity with a note
/// instead of silently reading the first match.
fn row_cells<'a>(kind_cells: &[&'a Value], rows: &[RowKey], row: &RowKey) -> Vec<&'a Value> {
    kind_cells
        .iter()
        .filter(|c| cell_rows(rows, c).iter().any(|x| x.name == row.name))
        .copied()
        .collect()
}

fn cell_rows<'a>(rows: &'a [RowKey], cell: &Value) -> Vec<&'a RowKey> {
    let harness = cell.get("harness").and_then(Value::as_str).unwrap_or("");
    let provider = cell.get("provider").and_then(Value::as_str).unwrap_or("");
    let model = cell
        .get("model")
        .and_then(Value::as_str)
        .map(crate::context_window::normalize_model)
        .unwrap_or_default();
    rows.iter()
        .filter(|r| {
            r.harness == harness
                && r.model_norm == model
                && (r.vendor.is_empty() || r.vendor == provider)
        })
        .collect()
}

fn in_blueprint_law(row: &RowKey) -> bool {
    BLUEPRINT_MODEL_PREFIXES
        .iter()
        .any(|p| row.model_norm.starts_with(p))
}

/// The seat laws a proposal seating `row` would contradict, with their
/// one-line gist: the router names them, the user decides. A glm-flash
/// target lane runs against the temporary opus/sonnet seating until it
/// lapses; any other target lane runs against the standing glm preference.
fn contradicted_laws(stage: &str, band: &str, row: &RowKey, today: NaiveDate) -> Vec<Value> {
    let mut out = Vec::new();
    if stage == BLUEPRINT_STAGE {
        out.push(json!({
            "id": BLUEPRINT_LAW,
            "gist": "blueprints run only on opus, fable, gpt-5.6-sol or gpt-6-astra, never glm",
            "contradicted": false,
        }));
        if !row.model_norm.starts_with("opus") {
            out.push(json!({
                "id": "d-e38d92cc",
                "gist": "blueprints stay on opus; a non-opus blueprint proposal would move them",
                "contradicted": true,
            }));
        }
        return out;
    }
    let flash = row.model_norm == "glm-5.3-flash";
    if today < NaiveDate::from_ymd_opt(2026, 10, 23).unwrap() {
        // The temporary law seats by band: sonnet for low and medium, opus
        // for high.
        let low_band = matches!(band, "low" | "medium");
        let aligned = (row.model_norm.starts_with("sonnet") && low_band)
            || (row.model_norm.starts_with("opus") && !low_band);
        if !aligned {
            out.push(json!({
                "id": "d-e38d92cc",
                "gist": "until 2026-10-23 the law seats sonnet on low and medium bands and opus on high",
                "contradicted": true,
            }));
        }
    } else if !flash {
        out.push(json!({
            "id": "d-e38d92cc",
            "gist": "from 2026-10-23 target work returns to glm-5.3-flash[1m]",
            "contradicted": true,
        }));
    }
    if !flash {
        out.push(json!({
            "id": "d-ecaf06b8",
            "gist": "prefer glm-5.3-flash for all dispatch lanes until the operator says otherwise",
            "contradicted": true,
        }));
        out.push(json!({
            "id": "d-058e4e17",
            "gist": "execution and target spawns ride glm-5.3-flash[1m]",
            "contradicted": true,
        }));
    }
    out
}

fn spawn_line(row: &RowKey) -> String {
    let route_flag = if row.route.is_empty() {
        format!("-m {}", row.model_raw)
    } else {
        format!("--route {}", row.route)
    };
    format!(
        "fno agents spawn --as thread -H {} {route_flag} -c <repo>",
        row.harness
    )
}

fn climb_cmd(stage: &str, incumbent: &str, row: &RowKey) -> String {
    format!(
        "fno doctor evals report --climb --baseline {incumbent} --trial {} \
--stage {stage} --trial-lane {}",
        row.name, row.name
    )
}

/// One verdict from the measured pair, the climb fold's constants deciding:
/// the interval arm reads closeness, the cost arm reads the tie rule. Both
/// costs and both intervals must be measured - an unread value proposes
/// nothing.
fn verdict_pair(cand: Option<(&Value, u64)>, inc: Option<(&Value, u64)>) -> (&'static str, Value) {
    let cell_facts = |c: &Value| -> Option<(f64, f64, f64)> {
        let lo = c.get("merged_ci95")?.as_array()?.first()?.as_f64()?;
        let hi = c.get("merged_ci95")?.as_array()?.get(1)?.as_f64()?;
        let cost = c.get("cost_per_merged_usd")?.as_f64()?;
        Some((lo, hi, cost))
    };
    // Explore reads the node count alone: a declared row with no cell (zero
    // nodes) or a thin one gets the spawn line before any incumbent facts
    // are asked for, and a thin candidate explores even against no
    // measured incumbent.
    let c_nodes = cand.as_ref().map(|(_, n)| *n).unwrap_or(0);
    if c_nodes < EXPLORE_FLOOR {
        return ("explore", json!("fewer than 5 nodes on this kind"));
    }
    let (Some((c, _)), Some((i, i_nodes))) = (cand, inc) else {
        return ("hold", json!("unmeasured pair"));
    };
    if i_nodes < EXPLORE_FLOOR {
        return ("hold", json!("incumbent underpowered"));
    }
    let (Some((clo, _chi, ccost)), Some((ilo, ihi, icost))) = (cell_facts(c), cell_facts(i)) else {
        return ("hold", json!("cost or interval unmeasured"));
    };
    if (clo >= ilo - crate::evals_climb::NOISE
        && ccost <= icost * (1.0 - crate::evals_climb::COST_CUT))
        || (clo > ihi && ccost <= icost)
    {
        ("propose", json!(null))
    } else {
        ("hold", json!("no arm fired"))
    }
}

/// The production entry: read the declared inventory, name each band's
/// incumbent from the grid, then fold the verdicts.
pub(crate) fn propose(cells: &[Value], stage: &str) -> Value {
    let Some((rows, inventory)) = declared_inventory() else {
        return json!({
            "stage": stage,
            "proposals": [],
            "note": "no declared routing.models; nothing to propose against",
        });
    };
    let mut bands: Vec<String> = Vec::new();
    for c in cells {
        let kind = c.get("kind").and_then(Value::as_str).unwrap_or("");
        let segs: Vec<&str> = kind.split('/').collect();
        if segs.first() == Some(&stage) {
            if let Some(b) = segs.get(1) {
                if !bands.iter().any(|x| x == b) {
                    bands.push((*b).to_string());
                }
            }
        }
    }
    let mut incumbents: BTreeMap<String, Option<String>> = BTreeMap::new();
    for b in &bands {
        incumbents.insert(b.clone(), incumbent_lane(stage, b, &inventory));
    }
    let today = chrono::Utc::now().date_naive();
    propose_with(cells, stage, &rows, &incumbents, today)
}

/// The hermetic core: the verdicts over handed-in rows, incumbents and
/// date. The seat laws change on a fixed day, so the clock is read by
/// the caller, never here.
pub(crate) fn propose_with(
    cells: &[Value],
    stage: &str,
    rows: &[RowKey],
    incumbents: &BTreeMap<String, Option<String>>,
    today: NaiveDate,
) -> Value {
    let mut kinds: Vec<String> = Vec::new();
    for c in cells {
        let kind = c.get("kind").and_then(Value::as_str).unwrap_or("");
        if kind.split('/').next() == Some(stage) && !kinds.iter().any(|k| k == kind) {
            kinds.push(kind.to_string());
        }
    }
    let mut proposals = Vec::new();
    for kind in &kinds {
        let segs: Vec<&str> = kind.split('/').collect();
        let difficulty = segs.get(1).copied().unwrap_or("unknown");
        let kind_cells: Vec<&Value> = cells
            .iter()
            .filter(|c| c.get("kind").and_then(Value::as_str) == Some(kind.as_str()))
            .collect();
        let Some(incumbent) = incumbents.get(difficulty).cloned().flatten() else {
            proposals.push(json!({
                "kind": kind, "verdict": "hold",
                "note": "the grid names no incumbent lane for this band",
            }));
            continue;
        };
        // The row's cells: every kind cell that could have ridden the row,
        // by name - a provider-agnostic row can match more than one cell,
        // and the fold refuses that ambiguity with a note instead of
        // silently reading the first match.
        let inc_cells = rows
            .iter()
            .find(|r| r.name == incumbent)
            .map(|r| row_cells(&kind_cells, rows, r))
            .unwrap_or_default();
        let inc_facts = match inc_cells.len() {
            1 => Some((
                inc_cells[0],
                inc_cells[0]
                    .get("nodes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )),
            _ => None,
        };
        for row in rows {
            if stage == BLUEPRINT_STAGE && !in_blueprint_law(row) {
                continue;
            }
            if row.name == incumbent {
                continue;
            }
            let matches = row_cells(&kind_cells, rows, row);
            let (cell, ambiguity) = match matches.len() {
                0 => (None, false),
                1 => (Some(matches[0]), false),
                _ => (None, true),
            };
            let nodes = cell
                .and_then(|c| c.get("nodes"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let (verdict, note) = if ambiguity {
                ("hold", json!("several cells map to this lane"))
            } else {
                verdict_pair(cell.map(|c| (c, nodes)), inc_facts)
            };
            let mut p = json!({
                "kind": kind,
                "lane": row.name,
                "harness": row.harness,
                "verdict": verdict,
                "nodes": nodes,
                "laws": contradicted_laws(stage, difficulty, row, today),
            });
            if let Some(c) = cell {
                for k in ["merged", "merged_ci95", "cost_per_merged_usd"] {
                    p[k] = c.get(k).cloned().unwrap_or(Value::Null);
                }
            }
            if verdict == "propose" {
                p["accept_line"] = match crate::evals_climb::accept_line(stage, &row.name) {
                    Some(line) => json!(line),
                    None => json!(null),
                };
                p["climb_cmd"] = json!(climb_cmd(stage, &incumbent, row));
            }
            if verdict == "explore" {
                p["spawn_line"] = json!(spawn_line(row));
            }
            if !note.is_null() {
                p["note"] = note;
            }
            proposals.push(p);
        }
    }
    json!({"stage": stage, "proposals": proposals})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(
        kind: &str,
        harness: &str,
        provider: &str,
        model: &str,
        nodes: u64,
        merged: u64,
        cost: Option<f64>,
    ) -> Value {
        // The same closed form slice 2 folds with, so the fixture bounds
        // read like production's.
        let (_rate, lo, hi) = crate::evals_trend::wilson_rate(nodes as usize, merged as usize);
        json!({
            "kind": kind,
            "harness": harness,
            "provider": provider,
            "model": model,
            "nodes": nodes,
            "merged": merged,
            "merged_ci95": [lo, hi],
            "cost_per_merged_usd": cost,
        })
    }

    fn row(name: &str, harness: &str, model: &str, vendor: &str) -> RowKey {
        RowKey {
            name: name.to_string(),
            harness: harness.to_string(),
            model_norm: crate::context_window::normalize_model(model),
            model_raw: model.to_string(),
            vendor: vendor.to_string(),
            route: if vendor.is_empty() {
                String::new()
            } else {
                format!("{vendor}/{model}")
            },
        }
    }

    /// The eve of the temporary seat law's lapse: the tests pin the day
    /// so their law assertions never depend on when they run.
    fn before_lapse() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
    }

    fn incumbents(band: &str, lane: &str) -> BTreeMap<String, Option<String>> {
        BTreeMap::from([(band.to_string(), Some(lane.to_string()))])
    }

    #[test]
    fn a_cheaper_close_candidate_proposes_with_the_accept_line() {
        // Incumbent sonnet: 40% merged, $10 per merged. Candidate glm flash:
        // 45% merged, $6 per merged - 40% cheaper, close enough to propose.
        let cells = vec![
            cell(
                "target/medium",
                "claude",
                "anthropic",
                "sonnet",
                20,
                8,
                Some(10.0),
            ),
            cell(
                "target/medium",
                "claude",
                "zai",
                "glm-5.3-flash",
                20,
                9,
                Some(6.0),
            ),
        ];
        let rows = vec![
            row("claude-sonnet", "claude", "sonnet", ""),
            row("zai-flash", "claude", "glm-5.3-flash[1m]", "zai"),
        ];
        let out = propose_with(
            &cells,
            "target",
            &rows,
            &incumbents("medium", "claude-sonnet"),
            before_lapse(),
        );
        let props = out["proposals"].as_array().unwrap();
        let p = props
            .iter()
            .find(|p| p["lane"] == json!("zai-flash"))
            .unwrap();
        assert_eq!(p["verdict"], json!("propose"), "{p}");
        // The accept line names the candidate row first: the lanes array
        // opens with the trial lane, single-quoted shell-side.
        let line = p["accept_line"].as_str().unwrap();
        assert!(line.contains("'[\"zai-flash"), "{line}");
        assert!(line.contains("on_exhausted degrade"), "{line}");
        assert!(p["climb_cmd"].as_str().unwrap().contains("--climb"));
        // Until the temporary seat law lapses, a glm proposal names it. After
        // that date glm is the standing seat and contradicts nothing.
        let laws: Vec<&str> = p["laws"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["contradicted"] == json!(true))
            .filter_map(|l| l["id"].as_str())
            .collect();
        let lapse = chrono::NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        let seat_law_live = chrono::Utc::now().date_naive() < lapse;
        assert_eq!(laws.contains(&"d-e38d92cc"), seat_law_live, "{laws:?}");
    }

    #[test]
    fn a_thin_declared_row_explores_with_a_spawn_line() {
        let cells = vec![cell(
            "target/medium",
            "claude",
            "anthropic",
            "sonnet",
            20,
            8,
            Some(10.0),
        )];
        let rows = vec![
            row("claude-sonnet", "claude", "sonnet", ""),
            row("codex-luna", "codex", "gpt-5.6-luna", ""),
        ];
        let out = propose_with(
            &cells,
            "target",
            &rows,
            &incumbents("medium", "claude-sonnet"),
            before_lapse(),
        );
        let props = out["proposals"].as_array().unwrap();
        let p = props
            .iter()
            .find(|p| p["lane"] == json!("codex-luna"))
            .unwrap();
        assert_eq!(p["verdict"], json!("explore"), "{p}");
        assert!(p["nodes"] == json!(0), "{p}");
        let line = p["spawn_line"].as_str().unwrap();
        assert!(
            line.starts_with("fno agents spawn --as thread -H codex"),
            "{line}"
        );
        // The incumbent never proposes against itself.
        assert!(props.iter().all(|p| p["lane"] != json!("claude-sonnet")));
    }

    #[test]
    fn a_pricier_candidate_holds_and_the_blueprint_stage_never_sees_glm() {
        // Incumbent sonnet $10; the glm candidate is only 10% cheaper: hold.
        let target_cells = vec![
            cell(
                "target/medium",
                "claude",
                "anthropic",
                "sonnet",
                20,
                8,
                Some(10.0),
            ),
            cell(
                "target/medium",
                "claude",
                "zai",
                "glm-5.3-flash",
                20,
                8,
                Some(9.0),
            ),
        ];
        let rows = vec![
            row("claude-sonnet", "claude", "sonnet", ""),
            row("zai-flash", "claude", "glm-5.3-flash[1m]", "zai"),
        ];
        let out = propose_with(
            &target_cells,
            "target",
            &rows,
            &incumbents("medium", "claude-sonnet"),
            before_lapse(),
        );
        let p = out["proposals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["lane"] == json!("zai-flash"))
            .unwrap();
        assert_eq!(p["verdict"], json!("hold"), "{p}");

        // The same cheaper glm cell under the blueprint stage never becomes
        // a candidate: the d-7b9e5ce4 list bars glm there.
        let bp_cells = vec![
            cell(
                "blueprint/medium",
                "claude",
                "anthropic",
                "claude-opus-5-5",
                20,
                8,
                Some(10.0),
            ),
            cell(
                "blueprint/medium",
                "claude",
                "zai",
                "glm-5.3-flash",
                20,
                9,
                Some(1.0),
            ),
        ];
        let bp_rows = vec![
            row("claude-opus-5-5", "claude", "claude-opus-5-5", ""),
            row("zai-flash", "claude", "glm-5.3-flash[1m]", "zai"),
        ];
        let out = propose_with(
            &bp_cells,
            "blueprint",
            &bp_rows,
            &incumbents("medium", "claude-opus-5-5"),
            before_lapse(),
        );
        let props = out["proposals"].as_array().unwrap();
        assert!(
            props.iter().all(|p| p["lane"] != json!("zai-flash")),
            "{props:?}"
        );
        assert_eq!(props.len(), 0, "{props:?}");
    }
}

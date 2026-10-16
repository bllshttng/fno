//! Read-only explanations over the same selection and capacity owners as advance.
use super::advance;
use super::advance_cli::{s, Args};
use serde_json::{json, Value};
use std::collections::BTreeMap;
const FILTERS:&[(&str,&str)]=&[
("roadmap","not on the requested roadmap"),("mission","not in the requested mission"),("parent-scope","not a descendant of the requested epic"),("project","belongs to another project (pass --all to widen, or --project)"),("live-claim","a live session already holds node:<id>; check `fno agents claim status`"),("unmerged-open-pr","already carries a PR that has not merged; the work is in review, not waiting"),("container","an epic is never built directly; its work lives in its children"),("batched","committed to an open batch; it ships via the batch PR"),("selection-guard","under a dead ancestor, contained elsewhere, undesigned, or stale past the quarantine window")];
const NOTE:&str="advance is DISARMED, so nothing above would run automatically. This report is a dry run of the pipeline, not a record of a decision advance made.";

pub(super) fn build(o: &Args) -> Result<Value, String> {
    let entries = super::advance_cli::rows()?;
    let occupied = advance::live_worked_overlay()?.into_keys().collect();
    let opts = crate::backlog_ready::ReadyOpts {
        project: o.project.clone(),
        all: o.project.is_none(),
        parent: o.epic.clone(),
        repo_root: Some(
            super::collision::find_repo_root()
                .to_string_lossy()
                .into_owned(),
        ),
        claimed: occupied,
        held: advance::held_questions(),
        now_ms: crate::claims::now_ms(),
        staleness_days: Some(advance::guard_staleness_days(None)),
        ..Default::default()
    };
    let reply = crate::backlog_ready::select(&entries, &opts).map_err(|e| e.0)?;
    let mut survivors = reply.rows;
    let mut excluded = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    for drop in &reply.drops {
        *counts.entry(drop.filter.clone()).or_default() += 1;
        reasons.insert(drop.id.clone(), drop.filter.clone());
    }
    let mut budget = super::advance_cli::budget();
    if o.epic.is_some() {
        let mut selected = Vec::new();
        for child in &survivors {
            let project = s(child, "project");
            let root = super::triage::project_root_from_settings(Some(project));
            let total = selected.len() as i64;
            let (capped, lane, _) = super::advance_cli::cap_verdict(
                child,
                &budget,
                total,
                o.model.as_deref(),
                o.provider.as_deref(),
            );
            let reason = if project.is_empty() {
                Some("no-project".into())
            } else if root.is_none() {
                Some("unmapped-project".into())
            } else if capped {
                Some("lane-cap".into())
            } else {
                read_gate(child, root.as_deref().unwrap())
            };
            if let Some(reason) = reason {
                *counts.entry(reason.clone()).or_default() += 1;
                reasons.insert(s(child, "id").into(), reason.clone());
                excluded.push(json!({"id":child["id"],"reason":reason,"lane":lane}));
            } else {
                selected.push(child.clone());
                if let Some(lane) = lane {
                    *budget.used.entry(lane).or_default() += 1;
                }
            }
        }
        if let Some(max) = o.max {
            let max = max.max(0) as usize;
            for child in selected.iter().skip(max) {
                *counts.entry("max-dispatch".into()).or_default() += 1;
                reasons.insert(s(child, "id").into(), "max-dispatch".into());
                excluded.push(json!({"id":child["id"],"reason":"max-dispatch"}));
            }
            selected.truncate(max);
        }
        survivors = selected;
    }
    let winner = survivors.first();
    let asked_id = o.explain_node.as_deref();
    let subject = asked_id
        .and_then(|id| entries.iter().find(|r| s(r, "id") == id))
        .or(winner);
    let asked = if let Some(id) = asked_id {
        let rank = survivors.iter().position(|r| s(r, "id") == id);
        let known = entries.iter().find(|r| s(r, "id") == id);
        let drop = reply.drops.iter().find(|d| d.id == id);
        json!({"id":id,"known":known.is_some(),"dropped_by":reasons.get(id),"drop_reason":drop.map(|d|&d.reason),"rank":rank,"never_a_candidate":known.is_some()&&rank.is_none()&&!reasons.contains_key(id),"status":known.map(|r|&r["persisted_status"])})
    } else {
        json!({})
    };
    let probe = crate::spawn_gate_verb::probe_answer(
        &json!({"mode":"probe","caller_session":crate::claims::resolve_identity().1}),
    );
    let mut gates = Vec::new();
    let walker = advance::walker_key().is_some_and(|key| advance::claim_is_live(&key));
    gates.push(json!({"name":"walker-claim","measured":if walker{"live"}else{"free"},"threshold":"free","verdict":if walker{"refuse"}else{"pass"},"key":null,"note":if walker{Some("a live walk owns this repo; it will pick the node up itself")}else{None}}));
    if let Some(node) = subject {
        let reason = claim_gate(node);
        gates.push(json!({"name":"node-claim","measured":reason.as_deref().unwrap_or("free"),"threshold":"free","verdict":if reason.is_some(){"refuse"}else{"pass"},"key":null,"note":null}));
    }
    gates.extend(probe["rows"].as_array().cloned().unwrap_or_default());
    let routing = routing(subject, o.model.as_deref());
    let (armed, rank) = advance::auto_continue_resolve(None);
    let decision = json!({"would_dispatch":if o.epic.is_some(){json!(survivors.iter().map(|r|&r["id"]).collect::<Vec<_>>())}else{json!(subject.map(|r|&r["id"]))},"max_dispatch":o.max,"armed":armed,"armed_rank":rank,"note":if armed{None}else{Some(NOTE)}});
    let selection = if o.epic.is_some() {
        let ordered = [
            "no-project",
            "unmapped-project",
            "walker-live",
            "lane-cap",
            "max-dispatch",
        ];
        let drops = ordered
            .iter()
            .map(|name| json!({"filter":name,"dropped":counts.get(*name).copied().unwrap_or(0)}))
            .chain(
                counts
                    .iter()
                    .filter(|(name, _)| !ordered.contains(&name.as_str()))
                    .map(|(name, n)| json!({"filter":name,"dropped":n})),
            )
            .collect::<Vec<_>>();
        json!({"width":budget.fleet,"pool":super::advance_cli::ready(&entries,None,o.epic.as_deref())?.len(),"drops":drops,"would_fill":survivors.iter().take(o.top).collect::<Vec<_>>(),"stop":if counts.contains_key("max-dispatch"){Some("max-dispatch")}else if counts.contains_key("lane-cap"){Some("cap-full")}else{None},"excluded":excluded})
    } else {
        json!({"pool":survivors.len()+reply.drops.len(),"drops":FILTERS.iter().map(|(name,_)|json!({"filter":name,"dropped":counts.get(*name).copied().unwrap_or(0)})).collect::<Vec<_>>(),"survivors":survivors.len(),"head":survivors.iter().take(o.top).collect::<Vec<_>>(),"why":FILTERS.iter().map(|(k,v)|(k.to_string(),json!(v))).collect::<serde_json::Map<_,_>>()})
    };
    let mut report = json!({"selection":selection,"asked":asked,"gates":gates,"routing":routing,"decision":decision});
    if let Some(epic) = &o.epic {
        report["mode"] = json!("lane-fill");
        report["epic"] = json!(epic);
    }
    Ok(report)
}
fn claim_gate(node: &Value) -> Option<String> {
    let observation = advance::observe_node_claim(
        s(node, "id"),
        node["cwd"].as_str(),
        false,
        false,
        None,
        None,
        None,
        None,
    );
    if observation.blocks_dispatch() {
        observation.refusal_reason()
    } else if advance::claim_is_live(&format!("dispatch:{}", s(node, "id"))) {
        Some("already-claimed".into())
    } else {
        None
    }
}
fn read_gate(node: &Value, root: &str) -> Option<String> {
    if advance::walker_live_at(root) {
        Some("walker-live".into())
    } else {
        claim_gate(node)
    }
}
fn routing(node: Option<&Value>, model: Option<&str>) -> Value {
    let Some(node) = node else {
        return json!({"chain":[],"candidate":null,"inputs":{},"routing":"unarmed","skipped":[]});
    };
    let verb = crate::backlog_ready::effective_verb(node)
        .ok()
        .and_then(|v| v.0)
        .unwrap_or_else(|| "target".into());
    let verb = super::fields::canonical_verb_key(&verb);
    let payload = json!({"rung_base":format!("agents.profiles.{verb}"),"node":node,"substrate":null,"permission_mode":null,"constrain_harness":null,"explicit_lane":false,"gate_bypassed":false,"role":if s(node,"plan_path").is_empty(){Some("planning")}else{None},"protected_role":null,"model_occupied":false,"work_verb":verb,"explicit_model_value":model,"explicit_route_value":null,"explicit_vendor_value":null,"capacity_refresh":true});
    let answer = crate::route_slot::resolve_slot_payload(&payload);
    let chain = answer["chain"].as_array().cloned().unwrap_or_default();
    json!({"chain":chain,"candidate":answer["candidate"],"inputs":{"difficulty":node["difficulty"],"priority":node["priority"],"role":payload["role"],"plan_path":node["plan_path"],"capacity":answer["meta"]["capacity"]},"routing":answer["verdict"],"skipped":chain.iter().filter(|s|s.as_str().is_some_and(|s|s.starts_with("slot skip "))).collect::<Vec<_>>()})
}
pub(super) fn render(report: &Value) -> String {
    let sel = &report["selection"];
    let fill = report["mode"] == "lane-fill";
    let mut out = Vec::new();
    if fill {
        out.push(format!(
            "SELECTION  lane fill (epic {})  width {}  {} ready -> {} would fill",
            s(report, "epic"),
            sel["width"],
            sel["pool"],
            sel["would_fill"].as_array().map(Vec::len).unwrap_or(0)
        ))
    } else {
        out.push(format!(
            "SELECTION  {} candidates -> {} eligible",
            sel["pool"], sel["survivors"]
        ));
    }
    for row in sel["drops"].as_array().into_iter().flatten() {
        let n = row["dropped"].as_u64().unwrap_or(0);
        let filter = s(row, "filter");
        out.push(if fill {
            format!("  -{n:<5} {filter}")
        } else if n > 0 {
            format!("  -{n:<5} {filter:<18} {}", s(&sel["why"], filter))
        } else {
            format!("  -{n:<5} {filter:<18}")
        });
    }
    if fill {
        out.push(format!(
            "  stop: {}",
            if sel["stop"].is_null() {
                "None"
            } else {
                s(sel, "stop")
            }
        ));
        for r in sel["excluded"].as_array().into_iter().flatten() {
            out.push(format!("    excluded {}: {}", s(r, "id"), s(r, "reason")));
        }
    }
    let head = &sel[if fill { "would_fill" } else { "head" }];
    if head.as_array().is_some_and(|a| !a.is_empty()) {
        out.push(
            if fill {
                "  would fill:"
            } else {
                "  ranked head:"
            }
            .into(),
        );
        for (i, row) in head.as_array().into_iter().flatten().enumerate() {
            let priority = if s(row, "priority").is_empty() {
                "-"
            } else {
                s(row, "priority")
            };
            let band = if s(row, "difficulty").is_empty() {
                "-"
            } else {
                s(row, "difficulty")
            };
            out.push(format!(
                "   {} {}. {}  {priority:<3} {band:<7} {}",
                if i == 0 { "->" } else { "  " },
                i + 1,
                s(row, "id"),
                s(row, "title").chars().take(60).collect::<String>()
            ));
        }
    }
    let asked = &report["asked"];
    if asked.as_object().is_some_and(|a| !a.is_empty()) {
        out.push(String::new());
        let id = s(asked, "id");
        out.push(if asked["known"] == false {
            format!("ASKED  {id}: no such node")
        } else if !asked["dropped_by"].is_null() {
            let reason = if s(asked, "dropped_by") == "selection-guard" {
                s(asked, "drop_reason")
            } else {
                s(asked, "dropped_by")
            };
            format!("ASKED  {id}: dropped by {reason}")
        } else if asked["never_a_candidate"] == true {
            format!(
                "ASKED  {id}: never a candidate (status {}, not ready and not cold-dispatchable)",
                s(asked, "status")
            )
        } else {
            format!(
                "ASKED  {id}: eligible, ranked {}",
                asked["rank"].as_u64().unwrap_or(0) + 1
            )
        });
    }
    out.push("\nGATES".into());
    for gate in report["gates"].as_array().into_iter().flatten() {
        let display = |k: &str, default: &str| {
            if gate[k].is_null() {
                default.to_string()
            } else {
                gate[k]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| gate[k].to_string())
            }
        };
        out.push(format!(
            "  {:<22} {:>12} / {:<12} {}",
            s(gate, "name"),
            display("measured", "?"),
            display("threshold", "-"),
            s(gate, "verdict")
        ));
        if !gate["key"].is_null() {
            let last = out.last_mut().unwrap();
            last.push_str(&format!("  [{}]", s(gate, "key")));
        }
        if !gate["note"].is_null() {
            out.push(format!("                         {}", s(gate, "note")));
        }
    }
    out.push("\nROUTING".into());
    let routing = &report["routing"];
    let chain = routing["chain"].as_array();
    if chain.is_none_or(|c| c.is_empty()) {
        out.push("  (no chain: nothing to route)".into())
    } else {
        for step in chain.into_iter().flatten() {
            out.push(format!("  {}", step.as_str().unwrap_or("")));
        }
    }
    let candidate = &routing["candidate"];
    out.push(if candidate.is_null() {
        "  -> grid declined; the spawn falls back to caller defaults".into()
    } else {
        format!("  -> {} {}", s(candidate, "harness"), s(candidate, "model"))
    });
    out.push(format!("  routing={}", s(routing, "routing")));
    out.push("\nDECISION".into());
    let d = &report["decision"];
    if fill {
        let ids = d["would_dispatch"].as_array().cloned().unwrap_or_default();
        out.push(if ids.is_empty() {
            "  would dispatch: nothing (fill selected no node)".into()
        } else {
            format!(
                "  would dispatch {} lane(s): {}",
                ids.len(),
                ids.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        if !d["max_dispatch"].is_null() {
            out.push(format!("  overall --max {} honored", d["max_dispatch"]));
        }
    } else {
        out.push(format!(
            "  would dispatch: {}",
            d["would_dispatch"]
                .as_str()
                .unwrap_or("nothing (no eligible node)")
        ));
    }
    out.push(format!(
        "  armed: {} (rank={})",
        if d["armed"] == true { "True" } else { "False" },
        s(d, "armed_rank")
    ));
    if !d["note"].is_null() {
        out.push(format!("  {}", s(d, "note")));
    }
    out.join("\n")
}

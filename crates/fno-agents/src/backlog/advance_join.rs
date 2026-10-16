//! Join a held plan through its task frontier and per-band write policies.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const INFRA: &[&str] = &[
    ".git/",
    ".fno/",
    "target/",
    "node_modules/",
    ".venv/",
    "__pycache__/",
];
const BANDS: &[&str] = &["high", "medium", "low"];

#[derive(Debug)]
pub struct PlanGraph {
    waves: Vec<Vec<String>>,
    surfaces: BTreeMap<String, BTreeSet<String>>,
    bands: Vec<String>,
    blockers: BTreeMap<String, BTreeSet<String>>,
}

pub fn plan_graph(path: &Path) -> Result<PlanGraph, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let front: Value = text
        .strip_prefix("---")
        .and_then(|s| s.split_once("\n---"))
        .map(|(s, _)| serde_yaml_ng::from_str(s))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or(json!({}));
    let fallback = front["difficulty"].as_str().unwrap_or("").to_lowercase();
    let body = text
        .lines()
        .skip_while(|l| l.trim() != "## Execution Strategy")
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    let fence = regex::Regex::new(r"(?is)```ya?ml\s*\n(.*?)```").unwrap();
    let yaml = fence
        .captures(&body)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str())
        .unwrap_or(&body);
    let raw_yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(yaml).map_err(|e| format!("Execution Strategy YAML: {e}"))?;
    let raw: Value = serde_json::to_value(raw_yaml).map_err(|e| e.to_string())?;
    let mut graph = PlanGraph {
        waves: Vec::new(),
        surfaces: BTreeMap::new(),
        bands: Vec::new(),
        blockers: BTreeMap::new(),
    };
    for task in raw["tasks"].as_array().into_iter().flatten() {
        let id = scalar(&task["id"]);
        if id.is_empty() {
            return Err("task has no id".into());
        }
        let surface = strings(&task["surface"])?;
        if graph.surfaces.insert(id.clone(), surface).is_some() {
            return Err(format!("duplicate task {id}"));
        }
        if task.get("blocked_by").is_some() {
            graph.blockers.insert(id, strings(&task["blocked_by"])?);
        }
    }
    for wave in raw["waves"].as_array().into_iter().flatten() {
        let tasks = wave["tasks"]
            .as_array()
            .ok_or("wave tasks must be a list")?
            .iter()
            .map(scalar)
            .collect::<Vec<_>>();
        for id in &tasks {
            if !graph.surfaces.contains_key(id) {
                return Err(format!("wave references unknown task {id}"));
            }
        }
        let band = wave["difficulty"].as_str().unwrap_or("").to_lowercase();
        graph.bands.push(if BANDS.contains(&band.as_str()) {
            band
        } else if BANDS.contains(&fallback.as_str()) {
            fallback.clone()
        } else {
            String::new()
        });
        graph.waves.push(tasks);
    }
    Ok(graph)
}
fn scalar(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| {
        if v.is_null() {
            String::new()
        } else {
            v.to_string()
        }
    })
}
fn strings(v: &Value) -> Result<BTreeSet<String>, String> {
    if v.is_null() {
        return Ok(BTreeSet::new());
    }
    v.as_array()
        .ok_or_else(|| "task surface/blockers must be a list".to_string())?
        .iter()
        .map(|s| {
            s.as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| "task surface/blockers must contain nonempty strings".to_string())
        })
        .collect()
}
pub fn width(graph: &PlanGraph) -> Result<usize, String> {
    let mut blockers = graph.blockers.clone();
    let mut all = BTreeSet::new();
    for (pos, tasks) in graph.waves.iter().enumerate() {
        for id in tasks {
            if !all.insert(id.clone()) {
                return Err(format!("task {id} occurs in several waves"));
            }
            blockers.entry(id.clone()).or_insert_with(|| {
                if pos == 0 {
                    BTreeSet::new()
                } else {
                    graph.waves[pos - 1].iter().cloned().collect()
                }
            });
        }
        let items = tasks
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    graph.surfaces.get(id).cloned().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        let (groups, unevaluated) =
            super::collision::partition(&items, super::collision::HIDDEN_SHARED_OUTPUT_ROOTS);
        for group in groups {
            let ordered = tasks
                .iter()
                .filter(|t| group.contains(*t))
                .collect::<Vec<_>>();
            for pair in ordered.windows(2) {
                blockers
                    .entry(pair[1].clone())
                    .or_default()
                    .insert(pair[0].clone());
            }
        }
        for id in &unevaluated {
            blockers
                .entry(id.clone())
                .or_default()
                .extend(tasks.iter().filter(|t| !unevaluated.contains(*t)).cloned());
        }
    }
    let mut done = BTreeSet::new();
    let mut width = 0;
    loop {
        let ready = all
            .iter()
            .filter(|id| {
                !done.contains(*id) && blockers.get(*id).is_none_or(|b| b.is_subset(&done))
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        width = width.max(ready.len());
        done.extend(ready);
    }
    if done != all {
        return Err(format!(
            "task graph is unsolvable (cycle or unknown blocker): {}",
            all.difference(&done)
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(width)
}

pub fn write_policies(graph: &PlanGraph, bands: &[String]) -> BTreeMap<String, Value> {
    let mut surfaces: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (tasks, band) in graph.waves.iter().zip(&graph.bands) {
        if band.is_empty() {
            continue;
        }
        for id in tasks {
            surfaces
                .entry(band.clone())
                .or_default()
                .extend(graph.surfaces.get(id).cloned().unwrap_or_default());
        }
    }
    let items = surfaces
        .iter()
        .map(|(b, p)| (b.clone(), p.clone()))
        .collect::<Vec<_>>();
    let (groups, unevaluated) = super::collision::partition(&items, &[]);
    bands
        .iter()
        .map(|band| {
            let own = surfaces.get(band);
            let verdict = if own.is_none_or(BTreeSet::is_empty) || unevaluated.contains(band) {
                "unevaluated"
            } else if groups.iter().any(|g| g.contains(band) && g.len() > 1) {
                "overlapping"
            } else {
                "enforced"
            };
            let policy = if verdict == "enforced" {
                let own = own.unwrap();
                let allow = own
                    .iter()
                    .cloned()
                    .chain(INFRA.iter().map(|s| s.to_string()))
                    .collect::<Vec<_>>();
                let deny = surfaces
                    .iter()
                    .filter(|(b, _)| *b != band)
                    .flat_map(|(_, p)| p.iter())
                    .filter(|p| !own.contains(*p))
                    .cloned()
                    .collect::<BTreeSet<_>>();
                json!({"band":band,"verdict":verdict,"allow_write":allow,"deny_edit":deny})
            } else {
                json!({"band":band,"verdict":verdict,"allow_write":null,"deny_edit":null})
            };
            (band.clone(), policy)
        })
        .collect()
}

fn live_joiners(node: &str) -> Vec<String> {
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        return Vec::new();
    };
    let Ok(reg) = crate::state::load_registry(&home.registry_json()) else {
        return Vec::new();
    };
    let hex = node.rsplit('-').next().unwrap_or(node);
    let prefixes = [
        format!("j-{node}-"),
        format!("jn-t-{node}-"),
        format!("j-{hex}-"),
        format!("jn-t-{hex}-"),
    ];
    let candidates = reg
        .entries
        .iter()
        .filter(|r| prefixes.iter().any(|p| r.name.starts_with(p)))
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect::<Vec<_>>();
    let handles = candidates
        .iter()
        .filter_map(|r| r["harness_session_id"].as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    if handles.is_empty() {
        return Vec::new();
    }
    let argv = ["agents", "truth", "--handles", &handles, "--json"].map(str::to_string);
    let answer =
        super::advance::bounded_command_env(Path::new(&crate::scrape::fno_bin()), &argv, 30, &[])
            .ok()
            .filter(|o| o.code == 0)
            .and_then(|o| serde_json::from_str::<Value>(&o.stdout).ok())
            .unwrap_or(json!({}));
    candidates
        .iter()
        .filter(|r| {
            let sid = r["harness_session_id"].as_str().unwrap_or("");
            let row = &answer[sid];
            let state = row["state"].as_str().unwrap_or("unknown");
            if !matches!(state, "unknown" | "") {
                return state != "done" && row["reachability"].as_str() != Some("unreachable");
            }
            let Some(path) = crate::claude_drive::find_transcript(sid) else {
                return false;
            };
            crate::loopcheck::intent::read_tail_lines(&path, 128 * 1024)
                .iter()
                .rev()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find_map(|row| {
                    row["timestamp"]
                        .as_str()
                        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                })
                .is_some_and(|stamp| {
                    crate::claims::now_ms() - stamp.timestamp_millis() <= 20 * 60 * 1000
                })
        })
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect()
}

fn join(node: &str, workers: Option<i64>, model: Option<&str>) -> Result<Value, (i32, String)> {
    let fail = |code, text: String| (code, text);
    let rows = crate::graph_store::read_rows_strict(&super::settings::graph_path())
        .map_err(|e| fail(2, e.to_string()))?;
    let (index, _) = crate::backlog_ready::find_node_index(&rows, node);
    let entry = index
        .map(|i| &rows[i])
        .ok_or_else(|| fail(2, format!("no graph node {node}")))?;
    let node = entry["id"].as_str().unwrap_or(node);
    let plan_raw = entry["plan_path"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| fail(4, format!("{node} has no bound plan")))?;
    let (state, record) = crate::claims::status(&format!("node:{node}"), None);
    if state != crate::claims::ClaimState::Live {
        return Err(fail(
            2,
            format!(
                "{node} has no live node claim (state: {}); nothing to join",
                state.as_str()
            ),
        ));
    }
    let worktree = record
        .as_ref()
        .and_then(|r| r.metadata.get("worktree").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            fail(
                2,
                format!("live claim for {node} names no holder worktree; nothing to join"),
            )
        })?;
    let root = Path::new(worktree);
    if !root.is_dir() {
        return Err(fail(
            2,
            format!("holder worktree for {node} is gone ({worktree}); nothing to join"),
        ));
    }
    let live = live_joiners(node);
    if !live.is_empty() {
        return Err(fail(
            5,
            format!(
                "{node} is already joined by {}; join is not idempotent",
                live.join(", ")
            ),
        ));
    }
    let plan = super::collision::resolve_plan_path(plan_raw, root);
    let graph =
        plan_graph(&plan).map_err(|e| fail(4, format!("{node} bound plan unreadable: {e}")))?;
    let width =
        width(&graph).map_err(|e| fail(4, format!("{node} plan task graph unsolvable: {e}")))?;
    if width < 2 {
        return Err(fail(
            3,
            format!("width {width}: a second worker has nothing to pull"),
        ));
    }
    let bands = BANDS
        .iter()
        .filter(|b| graph.bands.iter().any(|s| s == **b))
        .map(|b| b.to_string())
        .collect::<Vec<_>>();
    let band = bands.first().map(String::as_str).unwrap_or("medium");
    let priority = match entry["priority"].as_str().unwrap_or("p2") {
        p @ ("p0" | "p1" | "p2" | "p3") => p,
        _ => "p2",
    };
    let table = match priority {
        "p0" => [3, 4, 4],
        "p1" => [2, 3, 4],
        "p3" => [1, 1, 2],
        _ => [1, 2, 3],
    };
    let workers_source = if workers.is_some() {
        "explicit"
    } else {
        "derived"
    };
    let workers = workers.unwrap_or(
        table[match band {
            "low" => 0,
            "high" => 2,
            _ => 1,
        }],
    );
    let sandbox = super::advance_settings::load_merged(None)
        .pointer("/join/sandbox")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut count = workers.max(1).min((width - 1) as i64) as usize;
    if sandbox && !bands.is_empty() {
        count = count.min(bands.len());
    }
    let worker_bands = (0..count)
        .map(|k| {
            if bands.is_empty() {
                String::new()
            } else {
                bands[k % bands.len()].clone()
            }
        })
        .collect::<Vec<_>>();
    let policies = if sandbox {
        write_policies(&graph, &worker_bands)
    } else {
        BTreeMap::new()
    };
    let names = (1..=count)
        .map(|k| {
            crate::naming::dispatch_agent_name(
                Some("jn"),
                "t",
                node,
                Some(&k.to_string()),
                None,
                None,
                None,
            )
            .map_err(|e| fail(6, format!("joiner name unmintable: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let lead = &names[0];
    let brief_dir = root.join(".fno/join-briefs");
    std::fs::create_dir_all(&brief_dir)
        .map_err(|e| fail(1, format!("join cannot write the joiner brief: {e}")))?;
    let mut brief = format!("# Joiner brief: {node}\n\nlead and mail hub: {lead}\n\nRoute findings and questions through {lead}. The lead collects, deduplicates and sends one message to the node holder.\n");
    if !bands.is_empty() {
        brief.push_str("\nband table:\n\n| worker | band |\n|--------|------|\n");
        for (name, band) in names.iter().zip(&worker_bands) {
            brief.push_str(&format!("| {name} | {band} |\n"));
        }
        brief.push_str("\nFor each dispatch round, query --ready --band <your band>. Claim only the FIRST ready entry. Above-band tasks belong to peers. Re-query after a lost claim. Never retry a lost claim or dispatch the whole frontier.\n");
    }
    brief.push_str(&format!("\nBefore dispatching any worker, claim ONE ready task via `fno backlog task update {node} <task> --status in_progress`.\n"));
    let policy_dir = root.join(".fno/join-partition");
    let mut policy_paths: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (name, band) in names.iter().zip(&worker_bands) {
        if let Some(policy) = policies.get(band).filter(|p| p["verdict"] == "enforced") {
            std::fs::create_dir_all(&policy_dir).map_err(|e| fail(1, e.to_string()))?;
            let mut policy = policy.clone();
            policy["worker"] = json!(name);
            policy["sandbox"] = json!({"enabled":true,"filesystem":{
                "allowWrite":std::iter::once(super::settings::graph_path().parent().unwrap_or(Path::new(".")).to_string_lossy().into_owned()).chain(INFRA.iter().map(|s|root.join(s).to_string_lossy().into_owned())).collect::<Vec<_>>(),
                "denyWrite":policy["deny_edit"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|s|root.join(s).to_string_lossy().into_owned()).collect::<Vec<_>>()}});
            let path = policy_dir.join(format!("{name}.json"));
            std::fs::write(&path, policy.to_string()).map_err(|e| fail(1, e.to_string()))?;
            brief.push_str(&format!(
                "\n{name}: write policy enforced for band {band}. Read {} before edits.\n",
                path.display()
            ));
            policy_paths.insert(name.clone(), path);
        }
    }
    std::fs::write(brief_dir.join(format!("{node}.md")), brief)
        .map_err(|e| fail(1, e.to_string()))?;
    let mut spawned = Vec::new();
    let mut lanes = serde_json::Map::new();
    for (name, band) in names.iter().zip(&worker_bands) {
        let lane = json!({"difficulty":band,"plan_path":plan,"priority":priority});
        let mut pick = if band.is_empty() {
            super::dispatch_resolve::LanePick {
                harness: None,
                model: None,
                route: None,
                account: None,
                lane: None,
                decline: None,
            }
        } else {
            super::dispatch_resolve::grid_lane_for(Some(&lane), model, None, None)
        };
        if pick.harness.as_deref().is_some_and(|h| h != "claude") {
            pick.decline = Some(format!(
                "grid=harness-not-claude ({})",
                pick.harness.as_deref().unwrap_or("")
            ));
            pick.harness = None;
            pick.model = None;
            pick.route = None;
            pick.account = None;
            pick.lane = None;
        }
        let h = pick.harness.take();
        let m = pick.model.take();
        let route = pick.route.take();
        let account = pick.account.take();
        let why = pick.decline.take();
        let lane_name = pick.lane.take();
        let mut argv = [
            "agents",
            "spawn",
            "--substrate",
            "thread",
            "--harness",
            h.as_deref().unwrap_or("claude"),
            "--cwd",
            worktree,
            "--name",
            name,
        ]
        .map(str::to_string)
        .to_vec();
        // The lane pin spelling: a named lane resolves its own axes at the
        // spawn door, so the model and route pair collapse into the name.
        let lane_route = lane_name.as_deref().or(route.as_deref());
        for (flag, value) in [
            (
                "--model",
                m.as_deref().or(model).filter(|_| lane_name.is_none()),
            ),
            ("--route", lane_route),
            ("--account", account.as_deref()),
        ] {
            if let Some(v) = value {
                argv.extend([flag.to_string(), v.to_string()]);
            }
        }
        if let Some(path) = policy_paths.get(name) {
            argv.extend([
                "--sandbox-write-policy".into(),
                path.to_string_lossy().into_owned(),
            ]);
        }
        argv.push(format!("/fno:execute waves {}", plan.display()));
        let brief = if name == lead {
            format!("lead joiner of {node}: you are the mail hub {lead}")
        } else {
            format!("joiner of {node}: mail hub is {lead}")
        };
        let env = [
            ("TARGET_BRIEF".to_string(), brief),
            ("FNO_WORKER_BAND".to_string(), band.clone()),
        ];
        let out = super::advance::bounded_command_env(
            Path::new(&crate::scrape::fno_bin()),
            &argv,
            600,
            &env,
        )
        .map_err(|e| fail(1, e.to_string()))?;
        if out.code != 0 || super::advance_dispatch::spawn_receipt_identity(&out.stdout).is_empty()
        {
            let detail = if out.stderr.trim().is_empty() {
                out.stdout.trim()
            } else {
                out.stderr.trim()
            };
            if spawned.is_empty() {
                return Err(fail(1, format!("join spawn {name} failed: {detail}")));
            }
            eprintln!("join: spawn {name} failed: {detail}");
            continue;
        }
        spawned.push(name.clone());
        let mut receipt = json!({"band":band,"harness":h,"model":m,"sandbox":if sandbox {policies.get(band).and_then(|p|p["verdict"].as_str()).unwrap_or("unevaluated")} else {"off"}});
        if !band.is_empty() && receipt["harness"].is_null() {
            receipt["grid"] = json!("declined");
            if let Some(why) = why {
                receipt["grid_reason"] = json!(why);
            }
        }
        lanes.insert(name.clone(), receipt);
    }
    Ok(
        json!({"node":node,"worktree":worktree,"width":width,"priority":priority,"band":band,"workers":workers,"workers_source":workers_source,"spawned":spawned,"lead":lead,"lanes":lanes}),
    )
}

pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: fno backlog join NODE [--workers N] [--model MODEL|-m MODEL]\nJoin a held wide plan. Refusals: 2 no live claim, 3 width one, 4 unreadable plan, 5 already joined, 6 invalid name.");
        return 0;
    }
    if args.first().is_some_and(|a| a == "width" || a == "armed") && args.len() == 2 {
        if args[0] == "armed" {
            let (armed, rank) = super::advance::auto_continue_resolve(Some(Path::new(&args[1])));
            println!("armed={armed} rank={rank}");
            return 0;
        }
        return match plan_graph(Path::new(&args[1])).and_then(|g| width(&g)) {
            Ok(n) if n > 0 => {
                println!("{n}");
                0
            }
            _ => 1,
        };
    }
    let mut node = None;
    let mut workers = None;
    let mut model = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--workers" => {
                i += 1;
                workers = match args.get(i).and_then(|s| s.parse::<i64>().ok()) {
                    Some(n) => Some(n),
                    None => {
                        eprintln!("join: --workers requires an integer");
                        return 2;
                    }
                };
            }
            "--model" | "-m" => {
                i += 1;
                model = args.get(i).cloned();
                if model.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    eprintln!("join: --model must not be empty");
                    return 2;
                }
            }
            s if !s.starts_with('-') && node.is_none() => node = Some(s.to_string()),
            s => {
                eprintln!("join: unexpected argument {s}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(node) = node else {
        eprintln!("join: NODE is required");
        return 2;
    };
    match join(&node, workers, model.as_deref()) {
        Ok(receipt) => {
            join_emit(
                "join_dispatched",
                json!({"node":receipt["node"],"width":receipt["width"],"requested":workers.unwrap_or(receipt["spawned"].as_array().map(Vec::len).unwrap_or(0) as i64),"spawned":receipt["spawned"],"lead":receipt["lead"],"bands":receipt["lanes"].as_object().map(|m|m.iter().map(|(k,v)|(k.clone(),v["band"].clone())).collect::<serde_json::Map<_,_>>())}),
                None,
            );
            println!("{}", serde_json::to_string_pretty(&receipt).unwrap());
            0
        }
        Err((code, message)) => {
            join_emit(
                "join_refused",
                json!({"node":node,"code":code,"reason":message}),
                None,
            );
            if let Some(home) = crate::paths::AgentsHome::from_env_opt() {
                crate::friction::record_gate_refusal(
                    &home.events_jsonl(),
                    "join",
                    Some(node.as_str()),
                    &message,
                    &message,
                );
            }
            eprintln!("backlog join: {message}");
            code
        }
    }
}

fn join_emit(kind: &str, data: Value, _path: Option<&Path>) {
    if let Some(home) = crate::paths::AgentsHome::from_env_opt() {
        let _ = crate::events::EventEmitter::new(home.events_jsonl(), "agents").emit(kind, &data);
    }
}

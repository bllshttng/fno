//! The command center fold: the fleet map, the backlog rows and this mux's
//! agent rows become machines, pods (one project on one machine) and seats.

use crate::backlog_model::Inputs;
use crate::proto::AgentRow;
use serde_json::Value;

/// The status words on the wire, in the order a pod lists its seats.
pub(crate) const STATUS_ORDER: [&str; 4] = ["writing", "parked", "quiet", "unknown"];

pub(crate) struct FleetInputs {
    pub(crate) fleet: Result<Value, String>,
    pub(crate) backlog: Inputs,
    pub(crate) measured_at: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Work {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) priority: Option<String>,
    pub(crate) difficulty: Option<String>,
    pub(crate) plan: Option<String>,
    pub(crate) pr: Option<u64>,
    pub(crate) epic: Option<(String, Option<String>)>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Seat {
    pub(crate) name: String,
    /// The short handle `fno agents session attach` takes.
    pub(crate) short: String,
    pub(crate) session_id: Option<String>,
    pub(crate) harness: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) model_basis: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) last_message_at: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) context_pct: Option<i64>,
    pub(crate) status: &'static str,
    pub(crate) status_age_s: Option<i64>,
    pub(crate) work: Option<Work>,
    /// This mux's registry row, joined by full session id only. Only a
    /// joined seat takes action keys.
    pub(crate) local: Option<AgentRow>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Pod {
    pub(crate) project: String,
    pub(crate) seats: Vec<Seat>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Machine {
    pub(crate) id: String,
    pub(crate) host: String,
    pub(crate) local: bool,
    pub(crate) fresh: bool,
    pub(crate) age_s: i64,
    pub(crate) verdict: Option<String>,
    pub(crate) cpu: Option<f64>,
    pub(crate) mem_used: Option<f64>,
    pub(crate) cap: Option<i64>,
    pub(crate) takes_work: bool,
    pub(crate) pods: Vec<Pod>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Fleet {
    pub(crate) machines: Vec<Machine>,
    /// fleet top's own notice (no `store.remote_url`) or its primary error.
    pub(crate) notice: Option<String>,
    pub(crate) seat_error: Option<String>,
    pub(crate) measured_at: u64,
}

impl Fleet {
    pub(crate) fn seats(&self) -> impl Iterator<Item = &Seat> {
        self.machines
            .iter()
            .flat_map(|m| &m.pods)
            .flat_map(|p| &p.seats)
    }
}

/// `fno-agents top --machines --json` beside the backlog read, each with
/// its own failure, on the same 30 s bound the org fold had.
pub(crate) async fn gather(graph: &std::path::Path, agents: Vec<AgentRow>) -> FleetInputs {
    let mut command = tokio::process::Command::new("fno-agents");
    command
        .args(["top", "--machines", "--json"])
        .kill_on_drop(true);
    let (backlog, output) = tokio::join!(
        crate::backlog_model::gather(graph, agents),
        tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
    );
    let fleet = match output {
        Err(_) => Err("fleet top timed out after 30s".into()),
        Ok(Err(error)) => Err(format!("fleet top could not run: {error}")),
        Ok(Ok(output)) if !output.status.success() => Err(format!(
            "fleet top exited {}: {}",
            output
                .status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "by signal".into()),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(Ok(output)) => serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("fleet top invalid JSON: {e}")),
    };
    FleetInputs {
        fleet,
        backlog,
        measured_at: chrono::Utc::now().timestamp().max(0) as u64,
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A pane with no harness, or a shell, is not a seat.
fn is_shell(harness: Option<&str>) -> bool {
    harness.is_none_or(|h| matches!(h, "zsh" | "bash" | "sh" | "fish" | "shell"))
}

fn work_of(backlog: &Inputs, id: &str) -> Work {
    let Some(node) = crate::backlog_model::node(backlog, id) else {
        return Work {
            id: id.into(),
            ..Work::default()
        };
    };
    Work {
        id: id.into(),
        title: Some(node.card.title.clone()),
        status: node.card.status.clone(),
        priority: node.card.priority.clone(),
        difficulty: node.difficulty.clone(),
        plan: node.plan_path.clone(),
        pr: node.prs.first().map(|p| p.number),
        epic: node.parent.first().map(|l| (l.id.clone(), l.title.clone())),
    }
}

/// The pure fold. A seat's pod is its published project, else its node's
/// graph project, else `unassigned`.
pub(crate) fn fold(map: &Value, backlog: &Inputs, agents: &[AgentRow], measured_at: u64) -> Fleet {
    let mut machines: Vec<Machine> = map["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let host = text(m, "host").unwrap_or_else(|| "unknown host".into());
            let id = text(m, "machine_id").unwrap_or_else(|| host.clone());
            let mut pods: Vec<Pod> = Vec::new();
            for s in m["sessions"].as_array().into_iter().flatten() {
                let harness = text(s, "harness");
                if is_shell(harness.as_deref()) {
                    continue;
                }
                let Some(name) = text(s, "name") else {
                    continue;
                };
                let node = text(s, "node");
                let work = node.as_deref().map(|n| work_of(backlog, n));
                let project = text(s, "project")
                    .or_else(|| {
                        node.as_deref()
                            .and_then(|n| crate::backlog_model::resolve_row(backlog, n))
                            .and_then(|row| text(row, "project"))
                    })
                    .unwrap_or_else(|| "unassigned".into());
                let session_id = text(s, "session_id");
                let short = text(s, "session")
                    .or_else(|| session_id.as_ref().map(|id| id.chars().take(8).collect()))
                    .unwrap_or_else(|| name.clone());
                let status = text(s, "status")
                    .and_then(|w| STATUS_ORDER.iter().copied().find(|o| *o == w))
                    .unwrap_or("unknown");
                let local = session_id.as_deref().and_then(|sid| {
                    agents
                        .iter()
                        .find(|a| !a.exited && a.harness_session_id.as_deref() == Some(sid))
                        .cloned()
                });
                let seat = Seat {
                    name,
                    short,
                    session_id,
                    harness,
                    model: text(s, "model"),
                    model_basis: text(s, "model_basis"),
                    started_at: text(s, "started_at"),
                    last_message_at: text(s, "last_message_at"),
                    cwd: text(s, "cwd"),
                    role: text(s, "role"),
                    context_pct: s.get("context_pct").and_then(Value::as_i64),
                    status,
                    status_age_s: s.get("age_s").and_then(Value::as_i64),
                    work,
                    local,
                };
                match pods.iter_mut().find(|p| p.project == project) {
                    Some(pod) => pod.seats.push(seat),
                    None => pods.push(Pod {
                        project,
                        seats: vec![seat],
                    }),
                }
            }
            pods.sort_by(|a, b| {
                (a.project == "unassigned", &a.project)
                    .cmp(&(b.project == "unassigned", &b.project))
            });
            for pod in &mut pods {
                pod.seats.sort_by_key(|s| {
                    (
                        s.role.is_none(),
                        STATUS_ORDER.iter().position(|o| *o == s.status),
                        s.name.clone(),
                    )
                });
            }
            Machine {
                id,
                host,
                local: m["local"].as_bool().unwrap_or(false),
                fresh: m["fresh"].as_bool().unwrap_or(false),
                age_s: m["age_s"].as_i64().unwrap_or(0),
                verdict: text(m, "verdict"),
                cpu: m["cpu"].as_f64(),
                mem_used: m["mem_used"].as_f64(),
                cap: m["cap"].as_i64(),
                takes_work: m["takes_work"].as_bool().unwrap_or(false),
                pods,
            }
        })
        .collect();
    machines.sort_by(|a, b| (!a.local, &a.host).cmp(&(!b.local, &b.host)));
    Fleet {
        machines,
        notice: text(map, "notice")
            .or_else(|| text(map, "error").map(|e| format!("primary unreadable: {e}"))),
        seat_error: text(map, "seat_error"),
        measured_at,
    }
}

#[cfg(test)]
#[path = "tests/org_seats_tests.rs"]
pub(crate) mod tests;

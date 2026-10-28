//! The command center fold: the fleet map, the backlog rows and this mux's
//! agent rows become machines, the leads on each machine, and seats.

use crate::backlog_model::Inputs;
use crate::proto::AgentRow;
use serde::Serialize;
use serde_json::Value;

/// The status words on the wire, in the order a lead lists its seats.
pub(crate) const STATUS_ORDER: [&str; 4] = ["writing", "parked", "quiet", "unknown"];

pub(crate) struct FleetInputs {
    pub(crate) fleet: Result<Value, String>,
    pub(crate) backlog: Inputs,
    pub(crate) measured_at: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
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

#[derive(Debug, Clone, Default, Serialize)]
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
    /// The published project, else the node's graph project.
    pub(crate) project: Option<String>,
    pub(crate) context_pct: Option<i64>,
    pub(crate) status: &'static str,
    pub(crate) status_age_s: Option<i64>,
    pub(crate) work: Option<Work>,
    /// This mux's registry row, joined by full session id only. Only a
    /// joined seat takes action keys.
    #[serde(skip)]
    pub(crate) local: Option<AgentRow>,
}

#[derive(Debug, Clone, Default, Serialize)]
/// The seats on one machine that report to one lead. `name` is the lead's
/// seat name, or [`NO_LEAD`].
pub(crate) struct Lead {
    pub(crate) name: String,
    pub(crate) seats: Vec<Seat>,
}

/// The group of seats whose spawner chain reaches no seat with a role.
pub(crate) const NO_LEAD: &str = "no lead";

/// One fleet session, as the lead walk reads it.
struct Link {
    name: String,
    session_id: Option<String>,
    parent: Option<String>,
    role: Option<String>,
}

/// The lead a session reports to: itself when it holds a role, else the
/// first seat with a role up its spawner chain. `parent` is the spawner's
/// session id prefix. The walk stops after 8 steps, so a loop reads as no
/// lead.
fn lead_of(s: &Value, links: &[Link]) -> Option<String> {
    if text(s, "role").is_some() {
        return text(s, "name");
    }
    let mut parent = text(s, "parent");
    for _ in 0..8 {
        let p = parent?;
        let up = links
            .iter()
            .find(|l| l.session_id.as_deref().is_some_and(|id| id.starts_with(&p)))?;
        if up.role.is_some() {
            return Some(up.name.clone());
        }
        parent = up.parent.clone();
    }
    None
}

#[derive(Debug, Clone, Default, Serialize)]
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
    pub(crate) leads: Vec<Lead>,
}

#[derive(Debug, Clone, Default, Serialize)]
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
            .flat_map(|m| &m.leads)
            .flat_map(|p| &p.seats)
    }
}

/// `fno agents fleet top --machines --json` beside the backlog read, each
/// with its own failure, on the same 30 s bound the org fold had.
pub(crate) async fn gather(graph: &std::path::Path, agents: Vec<AgentRow>) -> FleetInputs {
    let (backlog, fleet) = tokio::join!(crate::backlog_model::gather(graph, agents), read_fleet());
    FleetInputs {
        fleet,
        backlog,
        measured_at: chrono::Utc::now().timestamp().max(0) as u64,
    }
}

/// The fleet top read, with each failure named.
pub(crate) async fn read_fleet() -> Result<Value, String> {
    match crate::org_model::tab_read(crate::org_model::FLEET_TOP_ARGV, Vec::new()).await {
        None => Err("fleet top timed out after 30s".into()),
        Some(Err(error)) => Err(format!("fleet top could not run: {error}")),
        Some(Ok(output)) if !output.status.success() => Err(format!(
            "fleet top exited {}: {}",
            output
                .status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "by signal".into()),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Some(Ok(output)) => serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("fleet top invalid JSON: {e}")),
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

/// The pure fold. A seat sits under the lead it reports to, on its own
/// machine; a lead on another machine still names the group.
pub(crate) fn fold(map: &Value, backlog: &Inputs, agents: &[AgentRow], measured_at: u64) -> Fleet {
    let links: Vec<Link> = map["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["sessions"].as_array().into_iter().flatten())
        .filter_map(|s| {
            Some(Link {
                name: text(s, "name")?,
                session_id: text(s, "session_id"),
                parent: text(s, "parent"),
                role: text(s, "role"),
            })
        })
        .collect();
    let mut machines: Vec<Machine> = map["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let host = text(m, "host").unwrap_or_else(|| "unknown host".into());
            let id = text(m, "machine_id").unwrap_or_else(|| host.clone());
            let mut leads: Vec<Lead> = Vec::new();
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
                let project = text(s, "project").or_else(|| {
                    node.as_deref()
                        .and_then(|n| crate::backlog_model::resolve_row(backlog, n))
                        .and_then(|row| text(row, "project"))
                });
                let lead = lead_of(s, &links).unwrap_or_else(|| NO_LEAD.into());
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
                    project,
                    context_pct: s.get("context_pct").and_then(Value::as_i64),
                    status,
                    status_age_s: s.get("age_s").and_then(Value::as_i64),
                    work,
                    local,
                };
                match leads.iter_mut().find(|l| l.name == lead) {
                    Some(group) => group.seats.push(seat),
                    None => leads.push(Lead {
                        name: lead,
                        seats: vec![seat],
                    }),
                }
            }
            leads.sort_by(|a, b| (a.name == NO_LEAD, &a.name).cmp(&(b.name == NO_LEAD, &b.name)));
            for lead in &mut leads {
                lead.seats.sort_by_key(|s| {
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
                leads,
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

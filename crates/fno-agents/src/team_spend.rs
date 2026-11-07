use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SessionMeter {
    Exact(f64),
    Estimated(f64),
    Unmetered,
}

impl SessionMeter {
    pub(crate) fn usd(self) -> Option<f64> {
        match self {
            Self::Exact(usd) | Self::Estimated(usd) => Some(usd),
            Self::Unmetered => None,
        }
    }
}

pub(crate) fn session_meter(
    otel_db: &Path,
    ledger: &Path,
    fno_id: Option<&str>,
    harness_session_id: Option<&str>,
) -> SessionMeter {
    let harness_session_id = harness_session_id.filter(|id| !id.is_empty());
    if let Some(session_id) = harness_session_id {
        if let Some(usd) = crate::otel_ingest::session_cost_usd(otel_db, session_id) {
            return SessionMeter::Exact(usd);
        }
        if let Some(cents) = crate::model_price::served_session_cost(Some(session_id)).0 {
            return SessionMeter::Estimated(cents as f64 / 100.0);
        }
    }

    if let Some(fno_id) = fno_id.filter(|id| !id.is_empty()) {
        if let Some(usd) = crate::loopcheck::session_cost_from_ledger(ledger, fno_id) {
            return SessionMeter::Estimated(usd);
        }
    }
    if let Some(session_id) = harness_session_id.filter(|id| Some(*id) != fno_id) {
        if let Some(usd) = crate::loopcheck::session_cost_from_ledger(ledger, session_id) {
            return SessionMeter::Estimated(usd);
        }
    }

    SessionMeter::Unmetered
}

#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct TeamSpend {
    pub metered_usd: f64,
    pub unmetered: Vec<String>,
    pub members: Vec<String>,
}

#[derive(Clone, Default)]
struct Member {
    session_id: String,
    name: String,
    parent_session_id: Option<String>,
    role_level: Option<u32>,
}

fn session_key(session_id: &str) -> String {
    session_id.trim().to_ascii_lowercase()
}

fn event_member(data: &serde_json::Value) -> Option<Member> {
    let session_id = data
        .get("harness_session_id")
        .or_else(|| data.get("session_uuid"))
        .or_else(|| data.get("session_id"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    if session_id.is_empty() {
        return None;
    }
    Some(Member {
        session_id: session_id.to_string(),
        name: data
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        parent_session_id: data
            .get("spawned_by_session")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        role_level: data
            .get("role_level")
            .and_then(serde_json::Value::as_u64)
            .and_then(|level| u32::try_from(level).ok()),
    })
}

pub fn team_spend(root_harness_session: &str) -> Result<TeamSpend, String> {
    let home = crate::paths::AgentsHome::from_env();
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let ledger = crate::paths_cli::ledger_json(&cwd);
    let otel_db = home.otel_dir().join("otel.db");
    team_spend_at(&home, root_harness_session, &ledger, &otel_db)
}

fn team_spend_at(
    home: &crate::paths::AgentsHome,
    root_harness_session: &str,
    ledger: &Path,
    otel_db: &Path,
) -> Result<TeamSpend, String> {
    let registry = crate::state::load_registry(&home.registry_json())
        .map_err(|error| format!("worker registry unreadable: {error}"))?;
    let event_query =
        crate::event_store::EventQuery::of_types(&["agent_spawned", "registry_row_removed"]);
    let events = crate::event_store::journal_text_checked(&home.events_jsonl(), &event_query)
        .map_err(|error| format!("worker birth journal unreadable: {error}"))?;

    let mut members = BTreeMap::<String, Member>::new();
    for line in events.lines() {
        let row: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("worker birth journal invalid: {error}"))?;
        let Some(data) = row.get("data") else {
            continue;
        };
        match row.get("type").and_then(serde_json::Value::as_str) {
            Some("agent_spawned") => {
                let Some(member) = event_member(data) else {
                    continue;
                };
                members.insert(session_key(&member.session_id), member);
            }
            Some("registry_row_removed") => {
                let Some(session_id) = data
                    .get("harness_session_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                let key = session_key(session_id);
                if data
                    .get("receipt_staged")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    let harness = data
                        .get("harness")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let path = crate::receipt::reap_receipt_path_for(home, harness, session_id);
                    let receipt = crate::receipt::read_reap_receipt(&path).map_err(|error| {
                        format!(
                            "worker removal receipt unreadable at {}: {error}",
                            path.display()
                        )
                    })?;
                    let mut member = members.remove(&key).unwrap_or_default();
                    member.session_id = receipt.harness_session_id.clone();
                    member.name = receipt.row_name.clone();
                    if let Some(identity) = receipt.identity.as_ref() {
                        if let Some(role_level) = identity.get("role_level") {
                            member.role_level = role_level
                                .as_u64()
                                .and_then(|level| u32::try_from(level).ok());
                        }
                        if let Some(parent_session_id) = identity.get("spawned_by_session") {
                            member.parent_session_id = parent_session_id
                                .as_str()
                                .map(str::trim)
                                .filter(|id| !id.is_empty())
                                .map(str::to_string);
                        }
                    }
                    members.insert(key, member);
                } else if let Some(member) = event_member(data) {
                    members.entry(key).or_insert(member);
                }
            }
            _ => {}
        }
    }
    for row in registry.entries {
        let Some(session_id) = row
            .harness_session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        members.insert(
            session_key(session_id),
            Member {
                session_id: session_id.to_string(),
                name: row.name,
                parent_session_id: row
                    .spawned_by_session
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty()),
                role_level: row.role_level,
            },
        );
    }

    let mut root_key = session_key(root_harness_session);
    if root_key.is_empty() {
        return Err("team root harness session is empty".into());
    }
    members.entry(root_key.clone()).or_insert_with(|| Member {
        session_id: root_harness_session.trim().to_string(),
        ..Default::default()
    });

    // A CHILD member resolves upward to its lead; a PEER handoff stays in its
    // own session, matching the descendant fold below.
    let mut ancestor_seen = HashSet::new();
    while ancestor_seen.insert(root_key.clone()) {
        let Some(member) = members.get(&root_key) else {
            break;
        };
        if member.role_level == Some(0) {
            break;
        }
        let Some(parent_id) = member.parent_session_id.as_deref() else {
            break;
        };
        let parent_key = session_key(parent_id);
        let Some(parent) = members.get(&parent_key) else {
            break;
        };
        if crate::spawn_edge::lineage_kind(&member.name, parent.role_level.is_some())
            != crate::spawn_edge::LineageKind::Child
        {
            break;
        }
        root_key = parent_key;
    }

    let mut included = HashSet::from([root_key.clone()]);
    let mut pending = VecDeque::from([root_key]);
    while let Some(parent_key) = pending.pop_front() {
        let Some(parent) = members.get(&parent_key) else {
            continue;
        };
        let children: Vec<(String, String)> = members
            .iter()
            .filter_map(|(key, child)| {
                let parent_id = child.parent_session_id.as_deref()?;
                (session_key(parent_id) == parent_key).then(|| (key.clone(), child.name.clone()))
            })
            .collect();
        for (child_key, child_name) in children {
            if included.contains(&child_key)
                || crate::spawn_edge::lineage_kind(&child_name, parent.role_level.is_some())
                    != crate::spawn_edge::LineageKind::Child
            {
                continue;
            }
            included.insert(child_key.clone());
            pending.push_back(child_key);
        }
    }

    let mut member_ids: Vec<String> = included
        .iter()
        .filter_map(|key| members.get(key).map(|member| member.session_id.clone()))
        .collect();
    member_ids.sort();

    let mut spend = TeamSpend::default();
    for session_id in member_ids {
        match session_meter(otel_db, ledger, None, Some(&session_id)) {
            SessionMeter::Exact(usd) | SessionMeter::Estimated(usd) => {
                spend.metered_usd += usd;
            }
            SessionMeter::Unmetered => spend.unmetered.push(session_id.clone()),
        }
        spend.members.push(session_id);
    }
    Ok(spend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{self, Lineage, RegistryEntry};
    use serde_json::json;

    fn row(
        name: &str,
        session: &str,
        parent: Option<&str>,
        role_level: Option<u32>,
        cwd: &str,
    ) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            harness: Some("codex".into()),
            harness_session_id: Some(session.into()),
            spawned_by_session: parent.map(str::to_string),
            role_level,
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    fn birth(name: &str, session: &str, parent: &str, cwd: &str) -> serde_json::Value {
        let lineage =
            Lineage::captured((Some(parent.into()), Some("codex".into()), Some(cwd.into())));
        crate::spawn_edge::birth_event(
            name,
            &lineage,
            json!({
                "harness_session_id": session,
                "cwd": cwd,
                "substrate": "thread",
            }),
        )
    }

    #[test]
    fn team_spend_counts_retired_children_once_and_excludes_peer_handoffs() {
        let temp = tempfile::tempdir().unwrap();
        let home = crate::paths::AgentsHome::at(temp.path().join("agents"));
        home.ensure_root().unwrap();
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.to_string_lossy().into_owned();
        let ledger = temp.path().join("ledger.json");
        let otel_db = temp.path().join("absent-otel.db");

        state::update_registry(&home.registry_json(), |registry| {
            registry.entries.extend([
                row("lead", "h-root", None, Some(0), &cwd),
                row("t-child-b", "h-b", Some("h-root"), None, &cwd),
            ]);
        })
        .unwrap();

        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        for (name, session, parent) in [
            ("t-child-a", "h-a", "h-root"),
            ("t-child-b", "h-b", "h-root"),
            ("grandchild-c", "h-c", "h-a"),
            ("handoff-peer", "h-peer", "h-b"),
            ("t-legacy", "h-legacy", "h-root"),
            ("jn-t-legacy-child", "h-legacy-child", "h-legacy"),
        ] {
            emitter
                .emit("agent_spawned", &birth(name, session, parent, &cwd))
                .unwrap();
        }
        let retired_lead = row("t-child-a", "h-a", Some("h-root"), Some(1), &cwd);
        let receipt = crate::receipt::build_reap_receipt(
            &retired_lead,
            None,
            crate::receipt::Writer::RegistryWrite,
            &|_| None,
        )
        .unwrap();
        crate::receipt::write_reap_receipt(&home, &receipt).unwrap();
        emitter
            .emit(
                "registry_row_removed",
                &json!({
                    "name": "t-child-a",
                    "short_id": "",
                    "harness": "codex",
                    "harness_session_id": "h-a",
                    "remover": "test-remover",
                    "reason": "retired",
                    "receipt_staged": true,
                }),
            )
            .unwrap();
        let legacy_parent = row("t-legacy", "h-legacy", Some("h-root"), None, &cwd);
        let mut legacy_receipt = crate::receipt::build_reap_receipt(
            &legacy_parent,
            None,
            crate::receipt::Writer::RegistryWrite,
            &|_| None,
        )
        .unwrap();
        let legacy_identity = legacy_receipt
            .identity
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        legacy_identity.remove("role_level");
        legacy_identity.remove("spawned_by_session");
        crate::receipt::write_reap_receipt(&home, &legacy_receipt).unwrap();
        emitter
            .emit(
                "registry_row_removed",
                &json!({
                    "name": "t-legacy",
                    "short_id": "",
                    "harness": "codex",
                    "harness_session_id": "h-legacy",
                    "remover": "test-remover",
                    "reason": "legacy receipt",
                    "receipt_staged": true,
                }),
            )
            .unwrap();
        std::fs::write(
            &ledger,
            r#"{"entries":[
                {"fno_id":"root-run","session_id":"root-run","sessions":["h-root"],"cost_usd":0.5},
                {"fno_id":"child-a-run","session_id":"child-a-run","sessions":["h-a"],"cost_usd":2.0},
                {"fno_id":"child-b-run","session_id":"child-b-run","sessions":["h-b"],"cost_usd":null},
                {"fno_id":"child-c-run","session_id":"child-c-run","sessions":["h-c"],"cost_usd":1.5},
                {"fno_id":"legacy-run","session_id":"legacy-run","sessions":["h-legacy","h-legacy-child"],"cost_usd":0.0}
            ]}"#,
        )
        .unwrap();

        let spend = team_spend_at(&home, "h-root", &ledger, &otel_db).unwrap();
        assert_eq!(
            spend.members,
            vec![
                "h-a".to_string(),
                "h-b".to_string(),
                "h-c".to_string(),
                "h-legacy".to_string(),
                "h-legacy-child".to_string(),
                "h-root".to_string(),
            ]
        );
        assert_eq!(spend.unmetered, vec!["h-b".to_string()]);
        assert!((spend.metered_usd - 4.0).abs() < 0.001);

        let peer_spend = team_spend_at(&home, "h-peer", &ledger, &otel_db).unwrap();
        assert_eq!(peer_spend.members, vec!["h-peer".to_string()]);
    }
}

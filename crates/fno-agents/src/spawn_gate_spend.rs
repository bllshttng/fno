use std::path::Path;

use crate::spawn_gate::{GateInput, Refusal, EXIT_TEAM_COST_CAP};

fn team_cost_cap(cwd: &Path) -> Option<Result<f64, String>> {
    for path in crate::agents_config::config_candidates(cwd) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let root = match content.parse::<toml::Value>() {
            Ok(root) => root,
            Err(error) => return Some(Err(error.to_string())),
        };
        let Some(team) = root.get("budget").and_then(|v| v.get("team")) else {
            continue;
        };
        let Some(team) = team.as_table() else {
            return Some(Err("budget.team must be a table".into()));
        };
        let Some(value) = team.get("cost_cap_usd") else {
            continue;
        };
        return Some(
            value
                .as_float()
                .or_else(|| value.as_integer().map(|n| n as f64))
                .or_else(|| {
                    value
                        .as_str()
                        .and_then(|raw| raw.trim().parse::<f64>().ok())
                })
                .ok_or_else(|| format!("invalid budget.team.cost_cap_usd: {value}")),
        );
    }
    None
}

pub(crate) fn check_team_cost_cap(cwd: &Path, input: &GateInput) -> Result<(), Refusal> {
    let Some(cap) = team_cost_cap(cwd) else {
        return Ok(());
    };
    let cap = cap.map_err(|raw| {
        Refusal::with_receipt(
            EXIT_TEAM_COST_CAP,
            serde_json::json!({
                "status":"refused", "reason":"team_cost_cap_config_invalid", "axis":"team_cost",
                "detail":raw, "remedy":"fix budget.team.cost_cap_usd in project config"
            }),
        )
    })?;
    let Some(session) = input.caller_session.as_deref() else {
        return Ok(());
    };
    let Ok(registry) =
        crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
    else {
        return Ok(());
    };
    let parent_teamed = registry
        .entries
        .iter()
        .any(|row| row.harness_session_id.as_deref() == Some(session) && row.role_level.is_some());
    if crate::spawn_edge::lineage_kind(&input.name, parent_teamed)
        != crate::spawn_edge::LineageKind::Child
    {
        return Ok(());
    }
    let Ok(spend) = crate::team_spend::team_spend(session) else {
        return Ok(());
    };
    if spend.metered_usd < cap {
        return Ok(());
    }
    let receipt = serde_json::json!({
        "status":"refused", "reason":"team_cost_cap", "axis":"team_cost",
        "summary":format!("${:.2} metered of ${:.2}, {} unmetered", spend.metered_usd, cap, spend.unmetered.len()),
        "remedy":"ask the project owner or operator to raise budget.team.cost_cap_usd in project config"
    });
    Err(Refusal::with_receipt(EXIT_TEAM_COST_CAP, receipt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_team_cost_cap_from_project_config() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join(".fno/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[budget.team]\ncost_cap_usd = 10.0\n").unwrap();
        let cap = team_cost_cap(temp.path());
        assert!(matches!(cap, Some(Ok(value)) if value == 10.0));
    }
}

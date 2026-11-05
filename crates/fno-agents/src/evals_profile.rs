//! Local, evidence-based user profiles. User-set fields survive every refresh.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

const FIELDS: [&str; 6] = [
    "role",
    "domain",
    "experience_band",
    "build_mode",
    "verification_capacity",
    "question_tolerance",
];
fn missing(reason: &str) -> Value {
    crate::correction_readings::Reading::unmeasured(reason).to_json()
}
fn derived(value: &str, basis: &str) -> Value {
    json!({"value":value,"basis":basis})
}

pub(crate) fn id(state: &Path) -> Result<String, String> {
    let dir = state.join("evals/profiles");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(".seed");
    if !path.exists() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
        crate::evals_traces::create_once(&path, &seed)?;
    }
    let seed = std::fs::read(path).map_err(|e| e.to_string())?;
    let key: [u8; 32] = seed
        .try_into()
        .map_err(|_| "profile seed must contain 32 bytes")?;
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "no local user identity")?;
    Ok(blake3::keyed_hash(&key, user.as_bytes()).to_hex()[..24].to_string())
}

fn dominant(counts: BTreeMap<String, u64>, basis: &str) -> Value {
    counts
        .into_iter()
        .filter(|(name, count)| !name.is_empty() && name != "unmeasured" && *count > 0)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(name, _)| derived(&name, basis))
        .unwrap_or_else(|| missing("no usable usage evidence"))
}

fn refresh(
    state: &Path,
    usage: &Value,
    domains: &[String],
    decisions: &[Value],
    update: Option<(&str, &str)>,
) -> Result<Value, String> {
    if let Some((key, value)) = update {
        if !FIELDS.contains(&key) || value.trim().is_empty() || value.len() > 256 {
            return Err("profile-set needs one of role, domain, experience_band, build_mode, verification_capacity, question_tolerance and a nonempty value of at most 256 bytes".into());
        }
    }
    let id = id(state)?;
    let dir = state.join("evals/profiles");
    let path = dir.join(format!("{id}.json"));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{id}.lock")))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let mut profile: Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("profile {}: {e}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"user_set":{}}),
        Err(e) => return Err(e.to_string()),
    };
    if !profile["user_set"].is_object() {
        return Err("profile user_set must be an object".into());
    }
    if let Some((key, value)) = update {
        profile["user_set"][key] = json!(value);
    }
    let mut roles = BTreeMap::new();
    for (key, count) in usage["seat_turns"].as_object().into_iter().flatten() {
        if let (Some(seat), Some(count)) = (key.split('|').nth(1), count.as_u64()) {
            *roles.entry(seat.to_string()).or_default() += count;
        }
    }
    let mut domain_counts = BTreeMap::new();
    for domain in domains {
        *domain_counts.entry(domain.clone()).or_default() += 1;
    }
    let harnesses: BTreeSet<_> = usage["typed_turns"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, n)| n.as_u64().is_some_and(|n| n > 0))
        .filter_map(|(k, _)| k.split('|').nth(1))
        .collect();
    let mut fields = json!({
        "role":dominant(roles,"correction_fold_seat_turns"),
        "domain":dominant(domain_counts,"graph_project_domains"),
        "experience_band":missing("harness usage does not measure experience"),
        "build_mode":if harnesses.is_empty() { missing("no typed harness usage") } else if harnesses.len()>1 { derived("multi_harness","correction_fold_typed_turns") } else { derived("single_harness","correction_fold_typed_turns") },
        "verification_capacity":if decisions.iter().any(|d|d["subject"].as_str().is_some_and(|s|s.to_ascii_lowercase().starts_with("visual approval")) && d["decision"].as_str().and_then(crate::correction_detect::reaction_rule)==Some("approve")) { derived("observed_visual_approval","decision_record") } else { missing("no explicit visual approval observed") },
        "question_tolerance":if decisions.iter().any(|d|d["question"].is_string() && d["decision"].as_str().is_some_and(|v|!v.trim().is_empty())) { derived("observed_question_answers","decision_record") } else { missing("no explicit question and answer pair observed") },
    });
    for key in FIELDS {
        if let Some(value) = profile["user_set"].get(key) {
            if !value.is_string() {
                return Err(format!("user_set.{key} must be a string"));
            }
            fields[key] = json!({"value":value,"basis":"user_set"});
        }
    }
    profile["id"] = json!(id);
    profile["split"] = json!(if u8::from_str_radix(&id[..2], 16).unwrap_or(255) < 51 {
        "held_out"
    } else {
        "train"
    });
    profile["fields"] = fields;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
    tmp.write_all(&serde_json::to_vec_pretty(&profile).map_err(|e| e.to_string())?)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    tmp.persist(path).map_err(|e| e.to_string())?;
    Ok(profile)
}

pub(crate) fn local(state: &Path, update: Option<(&str, &str)>) -> Result<Value, String> {
    let usage_path = crate::correction_fold::cache_path(state);
    let usage: Value = match std::fs::read(usage_path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("correction usage: {e}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.to_string()),
    };
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let graph = crate::backlog::settings::graph_path_at(Some(&cwd));
    let nodes = crate::graph_store::read_rows_where(
        &graph,
        &crate::backlog::RowQuery {
            fields: Some(vec!["domain".into()]),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let domains: Vec<_> = nodes
        .iter()
        .filter_map(|n| n["domain"].as_str().map(str::to_string))
        .collect();
    let journal = crate::decision_index::default_state_path("decisions.jsonl");
    let decisions = crate::decision_index::read_store_live(&graph, &journal)?;
    if decisions.damaged > 0 {
        eprintln!(
            "eval profile: {} damaged decision rows were excluded",
            decisions.damaged
        );
    }
    refresh(state, &usage, &domains, &decisions.rows, update)
}

pub fn run(args: &[String]) -> i32 {
    let update = match args {
        [flag] if flag == "--profile" => None,
        [flag, assignment] if flag == "--profile-set" => match assignment.split_once('=') {
            Some(pair) => Some(pair),
            None => {
                eprintln!("profile-set needs FIELD=VALUE");
                return 2;
            }
        },
        _ => {
            eprintln!("usage: fno-agents evals export --profile | --profile-set FIELD=VALUE");
            return 2;
        }
    };
    match local(&crate::evals_candidates::state_root(), update) {
        Ok(profile) => {
            println!("{profile}");
            0
        }
        Err(e) => {
            eprintln!("eval profile: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evals_profile_uses_local_evidence_preserves_user_values_and_keeps_identity_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let usage = json!({"typed_turns":{"2026-11-01|claude|m":20,"2026-11-01|codex|m":10},"seat_turns":{"2026-11-01|worker":30}});
        let domains = ["code".to_string()];
        let decisions = [
            json!({"question":"Approve the visual changes?","decision":"approved","subject":"visual approval PR 7 at abc"}),
        ];
        let first = refresh(
            tmp.path(),
            &usage,
            &domains,
            &decisions,
            Some(("experience_band", "expert")),
        )
        .unwrap();
        let id = first["id"].as_str().unwrap();
        let second = refresh(tmp.path(), &usage, &domains, &decisions, None).unwrap();
        assert_eq!(second["id"], id);
        assert_eq!(
            second["fields"]["experience_band"],
            json!({"value":"expert","basis":"user_set"})
        );
        assert_eq!(second["fields"]["role"]["value"], "worker");
        assert_eq!(second["fields"]["domain"]["value"], "code");
        assert_eq!(second["fields"]["build_mode"]["value"], "multi_harness");
        assert!(second["fields"]["verification_capacity"]["value"].is_string());
        assert!(second["fields"]["question_tolerance"]["value"].is_string());
        let other = tempfile::tempdir().unwrap();
        let other_profile = refresh(other.path(), &json!({}), &[], &[], None).unwrap();
        assert_ne!(other_profile["id"], id);
        assert_eq!(
            other_profile["fields"]["experience_band"]["basis"],
            "unmeasured"
        );
        assert!(refresh(
            tmp.path(),
            &usage,
            &domains,
            &decisions,
            Some(("unknown_field", "x"))
        )
        .is_err());
    }
}

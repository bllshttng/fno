//! parity-stage: characterization
//! parity-oracle: fno.agents.keeper_lane

use fno_agents::watchdog::keeper::{keeper_verdict_with, KeeperLaneResult, KeeperObs};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BOUND_KB: u64 = 2 * 1024 * 1024;

fn golden() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/watchdog/keeper.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(item: &Value, key: &str) -> Option<String> {
    item[key].as_str().map(str::to_string)
}

fn observation(item: &Value) -> KeeperObs {
    KeeperObs {
        pid: item["pid"].as_u64().unwrap() as u32,
        lane: text(item, "lane").unwrap(),
        sock: text(item, "sock").map(PathBuf::from),
        session: text(item, "session"),
        cwd: text(item, "cwd"),
        age_s: item["age_s"].as_f64().unwrap(),
        child_pids: item["child_pids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pid| pid.as_u64().unwrap() as u32)
            .collect(),
        sock_state: text(item, "sock_state").unwrap(),
        claimed_by: text(item, "claimed_by"),
        registry_ok: item["registry_ok"].as_bool().unwrap(),
        graph: text(item, "graph").map(PathBuf::from),
        rss_kb: item["rss_kb"].as_u64(),
    }
}

#[test]
fn verdicts_json_and_render_answer_the_frozen_lane() {
    let frozen = golden();
    let observations: Vec<KeeperObs> = frozen["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(observation)
        .collect();
    let verdicts: BTreeMap<u32, (String, String)> = observations
        .iter()
        .map(|obs| {
            let verdict = keeper_verdict_with(obs, None, BOUND_KB, &|_: &Path| true, &|| true);
            (obs.pid, verdict)
        })
        .collect();
    let result = KeeperLaneResult {
        observations: observations.clone(),
        verdicts: verdicts.clone(),
        broken_reason: None,
    };
    assert_eq!(result.to_json().to_string(), frozen["to_json"].to_string());
    assert_eq!(result.render(), frozen["render"].as_str().unwrap());

    let broken = KeeperLaneResult {
        observations: observations[..1].to_vec(),
        verdicts,
        broken_reason: Some("registry unreadable: bad".to_string()),
    };
    assert_eq!(
        broken.to_json().to_string(),
        frozen["broken_to_json"].to_string()
    );
    assert_eq!(broken.render(), frozen["broken_render"].as_str().unwrap());
    assert_eq!(
        KeeperLaneResult::default().render(),
        frozen["empty_render"].as_str().unwrap()
    );
}

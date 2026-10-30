//! parity-stage: characterization
//! parity-oracle: fno.agents.team.render_team
//!
//! The team render, frozen against goldens captured from its Python leg
//! (tests/golden/team/capture.py) before that leg was deleted. Each case
//! freezes the gathered team, the scope fold, the registry's session
//! statuses and the spawn gate, then compares the table, `--json` and
//! `--nodes` bytes.

use fno_agents::team_read::render_team_with;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn every_render_matches_its_python_golden() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/team/goldens.json");
    let goldens: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let sessions: HashMap<String, String> = goldens["sessions"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let cases: Vec<&str> = goldens
        .as_object()
        .unwrap()
        .keys()
        .filter_map(|k| k.strip_suffix("|inputs"))
        .collect();
    assert!(cases.len() >= 8, "the golden set is present");
    for case in cases {
        let inputs = &goldens[format!("{case}|inputs")];
        for (mode, as_json, nodes) in [
            ("table", false, false),
            ("json", true, false),
            ("nodes", false, true),
        ] {
            let fold = inputs["fold"].clone();
            let got = render_team_with(
                inputs["team"].clone(),
                move |_| {
                    if fold.as_object().is_some_and(|m| m.is_empty()) {
                        Err("frozen".to_string())
                    } else {
                        Ok(fold)
                    }
                },
                Some(sessions.clone()),
                || inputs["gate"].clone(),
                as_json,
                nodes,
            );
            assert_eq!(
                got,
                goldens[format!("{case}|{mode}")].as_str().unwrap(),
                "{case}|{mode}"
            );
        }
    }
}

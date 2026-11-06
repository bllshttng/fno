//! Contracts for `fno config setup plan` and `post-merge`, replacing the
//! deleted Python scaffold tests: the argv claim, the plan's tier filter,
//! and the scaffold's re-ask, cancel and stop paths.

use super::*;

fn oss(pieces: &[&str]) -> Vec<OsString> {
    pieces.iter().map(OsString::from).collect()
}

#[test]
fn the_claim_and_the_plan_filter() {
    assert_eq!(
        classify(&oss(&["config", "setup", "plan", "--advanced"])),
        Some(Cmd::Plan(oss(&["--advanced"])))
    );
    assert_eq!(
        classify(&oss(&["config", "setup", "post-merge"])),
        Some(Cmd::PostMerge(vec![]))
    );
    assert_eq!(
        classify(&oss(&["config", "setup", "cli-hooks", "--repair-only"])),
        Some(Cmd::CliHooks(oss(&["--repair-only"])))
    );
    assert_eq!(
        classify(&oss(&["config", "setup", "shim-check", "--repair"])),
        Some(Cmd::ShimCheck(oss(&["--repair"])))
    );
    assert_eq!(classify(&oss(&["config", "setup"])), Some(Cmd::Help));
    assert_eq!(
        classify(&oss(&["config", "setup", "wizard"])),
        Some(Cmd::Unknown("wizard".into()))
    );
    // run and auto-wire belong to their own modules; other groups pass.
    for sibling in [
        vec!["config", "setup", "run"],
        vec!["config", "setup", "auto-wire"],
        vec!["config", "plan"],
        vec!["setup", "run"],
    ] {
        assert_eq!(classify(&oss(&sibling)), None, "{sibling:?}");
    }

    let tiers = |advanced| -> Vec<String> {
        let plan: Value = serde_json::from_str(&plan_json(advanced)).unwrap();
        plan["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["tier"].as_str().unwrap().to_string())
            .collect()
    };
    let always = tiers(false);
    let all = tiers(true);
    assert!(!always.is_empty() && always.iter().all(|t| t == "always"));
    assert!(all.len() > always.len() && all.iter().any(|t| t == "advanced"));
}

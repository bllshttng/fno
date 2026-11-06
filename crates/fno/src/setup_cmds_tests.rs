//! Contracts for `fno config setup plan` and `post-merge`, replacing the
//! deleted Python scaffold tests: the argv claim, the plan's tier filter,
//! and the scaffold's re-ask, cancel and stop paths.

use super::*;
use serde_json::json;

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

/// Queued answers and set results; records every write that landed.
struct Fake {
    verdict: Value,
    answers: Vec<Option<&'static str>>,
    set_results: Vec<Result<(), (i32, String)>>,
    writes: Vec<(String, String)>,
    lines: Vec<String>,
}

impl Fake {
    fn new(verdict: Value, answers: Vec<Option<&'static str>>) -> Fake {
        Fake {
            verdict,
            answers,
            set_results: vec![],
            writes: vec![],
            lines: vec![],
        }
    }
}

impl Io for Fake {
    fn readiness(&mut self) -> Result<Value, String> {
        Ok(self.verdict.clone())
    }
    fn set(&mut self, key: &str, value: &str) -> Result<(), (i32, String)> {
        let result = if self.set_results.is_empty() {
            Ok(())
        } else {
            self.set_results.remove(0)
        };
        if result.is_ok() {
            self.writes.push((key.to_string(), value.to_string()));
        }
        result
    }
    fn prompt(&mut self, _message: &str, default: &str) -> Option<String> {
        if self.answers.is_empty() {
            return None;
        }
        self.answers.remove(0).map(|a| {
            if a.is_empty() {
                default.to_string()
            } else {
                a.to_string()
            }
        })
    }
    fn confirm(&mut self, _message: &str) -> bool {
        false
    }
    fn echo(&mut self, line: &str) {
        self.lines.push(line.to_string());
    }
}

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
        classify(&oss(&["setup", "post-merge"])),
        Some(Cmd::PostMerge(vec![]))
    );
    assert_eq!(
        classify(&oss(&["config", "setup", "cli-hooks", "--repair-only"])),
        Some(Cmd::CliHooks(oss(&["--repair-only"])))
    );
    assert_eq!(classify(&oss(&["config", "setup"])), Some(Cmd::Help));
    assert_eq!(
        classify(&oss(&["setup", "run"])),
        Some(Cmd::Moved("run".into()))
    );
    assert_eq!(
        classify(&oss(&["config", "setup", "wizard"])),
        Some(Cmd::Unknown("wizard".into()))
    );
    // run and auto-wire belong to their own modules; other groups pass.
    for sibling in [
        vec!["config", "setup", "run"],
        vec!["config", "setup", "auto-wire"],
        vec!["config", "plan"],
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

#[test]
fn the_scaffold_reasks_cancels_and_stops() {
    let unconfigured = json!({"status": "unconfigured", "project_id": null});

    // A rejected value asks again; the accepted one and the default id land.
    let mut io = Fake::new(
        unconfigured.clone(),
        vec![Some("/abs/lot.md"), Some(""), Some("")],
    );
    io.set_results = vec![Err((2, "must be repo-relative".into())), Ok(()), Ok(())];
    assert_eq!(
        scaffold(&mut io, "demo"),
        Outcome::Written("internal/demo/backlog/parking-lot.md".into())
    );
    assert!(io
        .lines
        .iter()
        .any(|l| l.starts_with("rejected: must be repo-relative")));
    assert_eq!(
        io.writes,
        vec![
            (
                PARKING_KEY.into(),
                "internal/demo/backlog/parking-lot.md".into()
            ),
            ("project.id".into(), "demo".into()),
        ]
    );

    // EOF at the first prompt writes nothing.
    let mut io = Fake::new(unconfigured.clone(), vec![None]);
    assert_eq!(scaffold(&mut io, "demo"), Outcome::Cancelled);
    assert!(io.writes.is_empty());

    // A structural refusal stops instead of asking again.
    let mut io = Fake::new(unconfigured, vec![Some("lot.md"), Some("lot.md")]);
    io.set_results = vec![Err((1, "malformed config.toml".into()))];
    assert_eq!(scaffold(&mut io, "demo"), Outcome::Failed);
    assert_eq!(io.answers.len(), 1, "asked once, then stopped");

    // An unreadable config never reaches a prompt.
    let mut io = Fake::new(
        json!({"status": "error", "cause": "bad toml"}),
        vec![Some("x")],
    );
    assert_eq!(scaffold(&mut io, "demo"), Outcome::Failed);
    assert_eq!(io.answers.len(), 1);
    assert!(io.lines[0].contains("bad toml"));

    // A ready repo declined at the confirm is left alone.
    let ready = json!({"status": "ready", "parking_lot_path": "a.md", "project_id": "p"});
    let mut io = Fake::new(ready, vec![]);
    assert_eq!(scaffold(&mut io, "demo"), Outcome::AlreadyReady);
    assert!(io.writes.is_empty());
}

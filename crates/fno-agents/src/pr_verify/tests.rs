//! The golden replay over `tests/fixtures/pr_verify/cases.json`, captured
//! from the Python leg this port replaces. Each row pins the exit code,
//! stdout, stderr and the resulting state file; the fake probe answers the
//! case's canned gh world at the same seam the real probes run.

use super::*;
use crate::gate_probes::Run;
use serde_json::Value;
use tempfile::TempDir;

fn cases() -> Vec<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pr_verify/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("pr_verify cases fixture"))
        .expect("pr_verify cases JSON")
}

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn ok(stdout: String) -> Option<Run> {
    Some(Run {
        ok: true,
        code: Some(0),
        stdout,
        stderr: String::new(),
    })
}

fn failed(stderr: &str) -> Option<Run> {
    Some(Run {
        ok: false,
        code: Some(1),
        stdout: String::new(),
        stderr: format!("{stderr}\n"),
    })
}

/// The canned gh world one case answers with, mirroring the Python
/// capture's FakeGH dispatch on the argv shapes the command emits.
struct World {
    toplevel: PathBuf,
    repo: String,
    pr_state: Option<Value>,
    reviews: Value,
    author: String,
    issue_comments: Value,
    review_comments: Value,
    gh_missing: bool,
    repo_view_fail: bool,
    reviews_api_fail: bool,
}

fn world_of(case: &Value, toplevel: &Path) -> World {
    let fake = &case["fake"];
    World {
        toplevel: toplevel.to_path_buf(),
        repo: if fake["repo"].is_string() {
            s(fake, "repo")
        } else {
            "o/r".into()
        },
        pr_state: match &fake["pr_states"] {
            Value::Array(rows) => rows.first().cloned().filter(|r| !r.is_null()),
            _ => None,
        },
        reviews: fake["reviews"].clone(),
        author: if fake["author"].is_null() {
            String::new()
        } else {
            s(fake, "author")
        },
        issue_comments: fake["issue_comments"].clone(),
        review_comments: fake["review_comments"].clone(),
        gh_missing: fake["gh_missing"].as_bool().unwrap_or(false),
        repo_view_fail: fake["repo_view_fail"].as_bool().unwrap_or(false),
        reviews_api_fail: fake["reviews_api_fail"].as_bool().unwrap_or(false),
    }
}

fn probe(w: &World) -> impl Fn(&[&str], &Path) -> Option<Run> + '_ {
    move |args: &[&str], _dir: &Path| {
        if args.first() == Some(&"git") && args.get(1) == Some(&"rev-parse") {
            return ok(format!("{}\n", w.toplevel.display()));
        }
        if w.gh_missing {
            return None;
        }
        if args.first() != Some(&"gh") {
            return None;
        }
        let joined = args.join(" ");
        if args.get(1) == Some(&"pr") && args.get(2) == Some(&"view") {
            if joined.contains("state,mergedAt") {
                return match &w.pr_state {
                    Some(row) => ok(serde_json::to_string(row).unwrap_or_default()),
                    None => failed("gh: could not read the PR"),
                };
            }
            if joined.contains("author") {
                return ok(format!("{}\n", w.author));
            }
            return Some(Run {
                ok: true,
                code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            });
        }
        if args.get(1) == Some(&"repo") && args.get(2) == Some(&"view") {
            if w.repo_view_fail {
                return failed("gh: repo view failed");
            }
            return ok(format!("{}\n", w.repo));
        }
        if args.get(1) == Some(&"api") {
            if joined.contains("/reviews") {
                if w.reviews_api_fail {
                    return failed("gh: api failed");
                }
                return ok(serde_json::to_string(&w.reviews).unwrap_or_default());
            }
            if joined.contains("/issues/") && joined.contains("/comments") {
                return ok(serde_json::to_string(&w.issue_comments).unwrap_or_default());
            }
            if joined.contains("/comments") {
                return ok(serde_json::to_string(&w.review_comments).unwrap_or_default());
            }
        }
        Some(Run {
            ok: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[test]
fn the_golden_replay_pins_every_captured_case() {
    let rows = cases();
    assert!(rows.len() >= 18, "the fixture shipped with its rows");
    for case in &rows {
        let tmp = TempDir::new().expect("case temp dir");
        let state_file = tmp.path().join("state.md");
        std::fs::write(&state_file, s(case, "state_file")).expect("state file");
        let world = world_of(case, tmp.path());
        let probe = probe(&world);
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let pr = s(case, "pr");
        let exit = match s(case, "kind").as_str() {
            "merged" => verify_merged(
                &pr,
                state_file.to_string_lossy().as_ref(),
                tmp.path(),
                &probe,
                !world.gh_missing,
                &|root: &Path| root.to_path_buf(),
                &mut out,
                &mut err,
            ),
            _ => verify_reviews(
                &pr,
                state_file.to_string_lossy().as_ref(),
                tmp.path(),
                &probe,
                !world.gh_missing,
                &|root: &Path| root.to_path_buf(),
                &mut out,
                &mut err,
            ),
        };
        let want = &case["want"];
        assert_eq!(
            exit,
            want["exit"].as_i64().unwrap_or(-1) as i32,
            "{} exit",
            case["name"]
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            s(want, "out"),
            "{} stdout",
            case["name"]
        );
        assert_eq!(
            String::from_utf8_lossy(&err),
            s(want, "err"),
            "{} stderr",
            case["name"]
        );
        assert_eq!(
            std::fs::read_to_string(&state_file).unwrap_or_default(),
            s(want, "state_after"),
            "{} state file",
            case["name"]
        );
    }
}

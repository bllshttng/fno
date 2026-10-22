//! Which PR the stop gate reads when the session's PR closed and the node
//! shipped through a replacement.

use fno_agents::loopcheck::run_loop_check_capture;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

#[derive(Debug, serde::Deserialize)]
struct Decision {
    decision: String,
    termination_reason: Option<String>,
    message: String,
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// PR 900 is the closed one the branch resolves to; PR 940 merged with a
/// `Fixes x-aaaa` line. `$SEARCH_HITS` is the REST search answer.
const GH: &str = r#"
if echo "$*" | grep -q -- "--version"; then
  echo 'gh version 2.x'; exit 0
fi
if echo "$*" | grep -q "search/issues"; then
  echo "$SEARCH_HITS"; exit 0
fi
if echo "$*" | grep -q "headRefName"; then
  if echo "$*" | grep -q "940"; then
    echo '{"state":"MERGED","number":940,"headRefName":"feat-b","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000042"}'
  else
    echo '{"state":"CLOSED","number":900,"headRefName":"feat-a","headRefOid":"deadbeefdeadbeefdeadbeefdeadbeef00000042"}'
  fi
  exit 0
fi
if echo "$*" | grep -q "checks"; then
  echo '[{"name":"ci","state":"SUCCESS","bucket":"pass"}]'; exit 0
fi
if echo "$*" | grep -q "pulls/"; then
  echo '[]'; exit 0
fi
if echo "$*" | grep -q "reviews"; then
  echo '{"reviews":[],"comments":[]}'; exit 0
fi
exit 1
"#;

fn fire(search_hits: &str) -> Decision {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    fs::create_dir_all(cwd.join(".fno")).unwrap();
    fs::write(
        cwd.join(".fno/config.toml"),
        "[review]\nrequired_bots = []\n",
    )
    .unwrap();
    std::env::set_var("FNO_NUDGE_DISABLED", "1");
    std::env::set_var("FNO_LOOPCHECK_MIN_FIRE_GAP_SECS", "0");
    for args in [
        &["init", "-q"][..],
        &[
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .unwrap()
            .success());
    }
    let state = cwd.join("target-state.md");
    fs::write(
        &state,
        "---\nsession_id: sess-replaced\ncreated_at: 2026-06-05T00:00:00Z\nattended: true\n---\ngraph_node_id: x-aaaa\n",
    )
    .unwrap();
    let transcript = cwd.join("transcript.jsonl");
    let promise = serde_json::json!({"message": {"role": "assistant",
        "content": "Done! <promise>MISSION COMPLETE</promise>"}});
    fs::write(&transcript, format!("{promise}\n")).unwrap();

    let bins = TempDir::new().unwrap();
    let gh = script(
        bins.path(),
        "gh",
        &format!("SEARCH_HITS='{search_hits}'\n{GH}"),
    );
    let git = script(
        bins.path(),
        "git",
        r#"echo "deadbeefdeadbeefdeadbeefdeadbeef00000042""#,
    );
    let owned: Vec<String> = [
        "loop-check",
        "--state",
        state.to_str().unwrap(),
        "--transcript",
        transcript.to_str().unwrap(),
        "--cwd",
        cwd.to_str().unwrap(),
        "--now",
        "2026-06-05T00:30:00Z",
        &format!("--gh-bin={}", gh.display()),
        &format!("--git-bin={}", git.display()),
        "--gh-budget-ledger",
        &format!("{}/.fno/gh-budget.json", cwd.display()),
        "--global-settings",
        "/nonexistent/global-settings.yaml",
        "--global-events",
        "/nonexistent/global-events.jsonl",
        "--author-harness",
        "none",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    let (_, output) = run_loop_check_capture(&owned);
    serde_json::from_str(&output).unwrap_or_else(|error| panic!("{error}: {output}"))
}

#[test]
fn closed_pr_replaced_by_a_merged_fixes_pr_is_done() {
    let replaced = fire(
        r#"{"items":[{"number":900,"state":"closed","pull_request":{"merged_at":null},"body":"Fixes x-aaaa"},{"number":940,"state":"closed","pull_request":{"merged_at":"2026-06-05T00:20:00Z"},"body":"Fixes x-aaaa"}]}"#,
    );
    assert_eq!(replaced.decision, "allow", "{}", replaced.message);
    assert_eq!(
        replaced.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "the merged replacement ends the loop: {}",
        replaced.message
    );

    let alone = fire(r#"{"items":[]}"#);
    assert_ne!(
        alone.termination_reason.as_deref(),
        Some("DonePRGreen"),
        "with no replacement the closed PR still holds the gate"
    );
}

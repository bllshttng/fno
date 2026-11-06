//! parity-stage: characterization
//! parity-oracle: cli/src/fno/pr_watch/cli.py:tick
//!
//! Characterization for the pr-watch tick port: `fno-agents pr-watch tick` is
//! pinned by the frozen goldens under tests/golden/pr-watch-tick/. While the
//! Python leg lived, this file ran as a LIVE differential under
//! `FNO_CAPTURE_GOLDEN=1`: both legs ran the same fixture home against one
//! stubbed `gh`, and the run asserted identity of the masked event journal,
//! stdout and exit before the goldens froze. The Python package was deleted in
//! the same change, so the goldens stand as the contract and capture refuses.
//!
//! The fleet-tail phases run on a time slot, so their rows leave the journal
//! view; run ids, pids and durations are masked.

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{assert_golden, slug, Golden};
use serde_json::{json, Value};

mod common;

const GH_STUB: &str = r#"#!/bin/sh
# Answers the tick's REST reads for one repo: o/r, PRs 7 (open) and 8 (merged).
args="$*"
if [ "${PR_WATCH_TEST_GH_DELAY:-0}" -gt 0 ]; then sleep "$PR_WATCH_TEST_GH_DELAY"; fi
case " $args " in *" -i "*) printf 'HTTP/2.0 200 OK\r\n\r\n' ;; esac
case "$args" in
  *rate_limit*) echo '{"resources":{"graphql":{"remaining":4000,"reset":1893456000},"core":{"remaining":4000,"reset":1893456000}}}' ;;
  *repos/o/r/pulls/7/reviews*) echo '[{"user":{"login":"bot[bot]"},"submitted_at":"2026-10-30T00:00:00Z","state":"COMMENTED"}]' ;;
  *repos/o/r/issues/7/comments*) echo '[]' ;;
  *repos/o/r/pulls/8/reviews*|*repos/o/r/issues/8/comments*) echo '[]' ;;
  *repos/o/r/pulls/7*) echo '{"number":7,"state":"open","merged_at":null,"merged":false,"draft":false,"created_at":"2026-10-29T00:00:00Z","head":{"sha":"aaa","ref":"f"},"base":{"ref":"main"},"html_url":"https://github.com/o/r/pull/7","merge_commit_sha":null}' ;;
  *repos/o/r/pulls/8*) echo '{"number":8,"state":"closed","merged_at":"2026-10-30T00:00:00Z","merged":true,"draft":false,"created_at":"2026-10-29T00:00:00Z","head":{"sha":"aaa","ref":"f"},"base":{"ref":"main"},"html_url":"https://github.com/o/r/pull/8","merge_commit_sha":"abc"}' ;;
  *repos/o/r/pulls*) echo '[{"number":7,"state":"open","draft":false,"created_at":"2026-10-29T00:00:00Z"}]' ;;
  *) echo "{}" ;;
esac
"#;

struct Case {
    label: &'static str,
    enabled: bool,
    prs: &'static [i64],
    /// Seeded watermark rows, by key.
    watermark: Value,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "disabled",
            enabled: false,
            prs: &[],
            watermark: json!({}),
        },
        Case {
            label: "empty graph",
            enabled: true,
            prs: &[],
            watermark: json!({}),
        },
        Case {
            label: "first seen baselines",
            enabled: true,
            prs: &[7],
            watermark: json!({}),
        },
        Case {
            label: "new review fires",
            enabled: true,
            prs: &[7],
            watermark: json!({"o/r#7": {"last_review_ts": "2026-10-01T00:00:00Z",
                                         "last_seen_state": "OPEN", "retries": 0, "parked": null,
                                         "merge_dispatched": false}}),
        },
        Case {
            label: "orphan row drops",
            enabled: true,
            prs: &[7],
            watermark: json!({"o/r#99": {"last_seen_state": "OPEN", "parked": "max-age"}}),
        },
    ]
}

struct Home {
    dir: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    bin: PathBuf,
}

fn write_case(root: &Path, case: &Case, leg: &str) -> Home {
    let dir = root.join(slug(case.label)).join(leg);
    let (repo, state, bin) = (dir.join("repo"), dir.join(".fno"), dir.join("bin"));
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(state.join("state")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("gh");
    std::fs::write(&stub, GH_STUB).unwrap();
    let fire = bin.join("fire-ok");
    std::fs::write(&fire, "#!/bin/sh\necho '{\"is_error\": false}'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    for path in [&stub, &fire] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let config = format!(
        "state_dir = \"{}\"\n\n[pr_watch]\nenabled = {}\ninterval_seconds = 600\n\n[autonomy]\nenabled = true\n\n[recovery]\nenabled = false\n\n[evals]\nschedule_days = 0\n\n[review]\ngithub_apps = [\"bot\"]\n",
        state.display(), case.enabled
    );
    std::fs::write(state.join("config.toml"), &config).unwrap();
    std::fs::create_dir_all(repo.join(".fno")).unwrap();
    std::fs::write(repo.join(".fno/config.toml"), &config).unwrap();
    std::fs::write(
        state.join("state/pr-watcher-state.json"),
        serde_json::to_string_pretty(&case.watermark).unwrap(),
    )
    .unwrap();
    let rows: Vec<Value> = case
        .prs
        .iter()
        .map(|pr| {
            json!({"id": format!("x-{pr}"), "title": "fixture", "status": "in_review",
                   "cwd": repo, "pr_number": pr, "pr_url": format!("https://github.com/o/r/pull/{pr}")})
        })
        .collect();
    fno_agents::graph_store::seed_rows_with_slugs(&state.join("graph.json"), &rows)
        .expect("seed rows");
    Home {
        dir,
        repo,
        state,
        bin,
    }
}

fn command(home: &Home, program: &Path) -> Command {
    let mut cmd = Command::new(program);
    let path = format!("{}:/usr/bin:/bin", home.bin.display());
    cmd.env_clear()
        .envs(fno_agents::test_run::self_owner_env())
        .env("HOME", &home.dir)
        .env("PATH", path)
        .env("FNO_STATE_DIR", &home.state)
        .env("FNO_CLAIMS_ROOT", &home.state)
        .env("FNO_AGENTS_BIN", env!("CARGO_BIN_EXE_fno-agents"))
        .env("FNO_BIN", home.bin.join("no-fno"))
        .env("PR_WATCH_FIRE_CMD", home.bin.join("fire-ok"))
        .env("FNO_GH_FACTS_DIR", home.dir.join("gh-facts"))
        .current_dir(&home.repo);
    cmd
}

fn masked_journal(home: &Home) -> String {
    let out = home.dir.join("export.jsonl");
    fno_agents::event_store::export_jsonl(&home.state.join("db/events.jsonl"), &out)
        .expect("export");
    let mut rows = Vec::new();
    for line in std::fs::read_to_string(&out).unwrap().lines() {
        let mut row: Value = serde_json::from_str(line).unwrap();
        let data = &mut row["data"];
        if data["arm"]
            .as_str()
            .is_some_and(|a| matches!(a, "stranded" | "recovery" | "watchdog"))
        {
            continue;
        }
        for key in ["pid", "duration_s", "phase_s", "receipt_id"] {
            if data.get(key).is_some() {
                data[key] = json!("<MASK>");
            }
        }
        // With no cut this names the last phase that ran, which the time slot picks.
        if row["type"] == "pr_watch_tick_end" {
            row["data"]["phase"] = json!("<MASK>");
        }
        rows.push(
            json!({"type": row["type"], "source": row["source"], "data": row["data"]}).to_string(),
        );
    }
    let text = rows.join("\n") + "\n";
    let re = regex::Regex::new(r"read_ms=-?\d+").unwrap();
    re.replace_all(&text, "read_ms=<MS>")
        .replace(&home.dir.display().to_string(), "<CASE>")
}

fn golden_of(home: &Home, output: std::process::Output) -> Golden {
    let stdout =
        String::from_utf8_lossy(&output.stdout).replace(&home.dir.display().to_string(), "<CASE>");
    let keep: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.starts_with("stranded sweep"))
        .collect();
    Golden {
        exit: output.status.code(),
        streams: vec![keep.join("\n") + "\n", masked_journal(home)],
    }
}

fn rust_leg(home: &Home) -> Golden {
    let mut cmd = command(home, Path::new(env!("CARGO_BIN_EXE_fno-agents")));
    cmd.args(["pr-watch", "tick"]);
    golden_of(home, cmd.output().expect("rust leg"))
}

#[test]
fn the_tick_matches_its_frozen_goldens() {
    let root = tempfile::tempdir().unwrap();
    for case in cases() {
        let rust = rust_leg(&write_case(root.path(), &case, "rust"));
        assert_golden("pr-watch-tick", case.label, &rust, None);
    }
}

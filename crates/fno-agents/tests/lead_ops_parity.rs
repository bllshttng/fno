//! parity-stage: characterization
//! parity-oracle: fno.lead.cli (done, escalate, drain, board --last-run)
//!
//! Characterization for the last lead commands on their native argvs:
//! `lead-state --drain`, `--done` and `--escalate`, and `board --last-run`.
//! The goldens under tests/golden/lead_ops/ were captured on 2026-10-26 from
//! the Python commands over the lead_lifecycle fixtures, before the Python
//! package was deleted. Capture mode refuses: a golden can only be captured
//! while the old leg runs.
//!
//! The temp root folds to `<ROOT>` and its slug to `<ROOTSLUG>`. A fleet task
//! id is random, so the escalation receipts fold it to `<ID>`.

use common::{assert_golden, Golden};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const SUBJECT: &str = "lead_ops";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lead_lifecycle")
        .join(name)
}

fn slug(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// Fold the root and the random fleet task id.
fn normalize(text: &str, root: &Path) -> String {
    let canonical = fs::canonicalize(root).unwrap();
    let text = text
        .replace(&slug(&canonical), "<ROOTSLUG>")
        .replace(&slug(root), "<ROOTSLUG>")
        .replace(&canonical.to_string_lossy().into_owned(), "<ROOT>")
        .replace(&root.to_string_lossy().into_owned(), "<ROOT>");
    text.split('\n')
        .map(|line| {
            if let Some(rest) = line.strip_prefix("operator:") {
                if rest.starts_with("ft-") {
                    return "operator:<ID>".to_string();
                }
            }
            if line.starts_with("lead: ") {
                if let Some((head, last)) = line.rsplit_once(' ') {
                    if last.starts_with("ft-") {
                        return format!("{head} <ID>");
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One fresh world: an agents home holding the fixture registry, a state dir
/// holding the seeded graph, a project journal and a git repo to stand in.
fn world() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path();
    for dir in ["home", "state", "project", "spaces", "bin", "repo", ".fno"] {
        fs::create_dir_all(p.join(dir)).unwrap();
    }
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(p.join("repo"))
        .status()
        .unwrap();
    assert!(init.success());
    fs::copy(fixture("registry.json"), p.join("home/registry.json")).unwrap();
    fs::copy(
        fixture("events-first.jsonl"),
        p.join("project/events.jsonl"),
    )
    .unwrap();
    fs::write(p.join("state/.path-migration-done"), "").unwrap();
    fs::write(
        p.join(".fno/config.toml"),
        "[work.workspaces.t]\nprojects = [{ name = \"fno\", path = \"/nonexistent\" }]\n",
    )
    .unwrap();
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(fixture("graph-rows.json")).unwrap()).unwrap();
    let graph = fno_agents::state_layout::graph_anchor(&p.join("state"));
    fs::create_dir_all(graph.parent().unwrap()).unwrap();
    fno_agents::graph_store::seed_rows_with_slugs(&graph, &rows).unwrap();
    let stub = p.join("bin/fno-stub");
    fs::write(&stub, "#!/bin/bash\nexit 3\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    tmp
}

/// The space this world's repo keys.
fn space(root: &Path) -> PathBuf {
    root.join("spaces")
        .join(slug(&fs::canonicalize(root.join("repo")).unwrap()))
}

/// Write one scope's manifest, holding `session`.
fn manifest(root: &Path, scope: &str, session: &str) -> PathBuf {
    let leads = space(root).join("leads");
    fs::create_dir_all(&leads).unwrap();
    let text = fs::read_to_string(fixture("manifest-epic-y.md"))
        .unwrap()
        .replace("epic-y", scope)
        .replace("sess-lead", session);
    let path = leads.join(format!("{scope}.md"));
    fs::write(&path, text).unwrap();
    path
}

/// Move every registry row's cwd into this world's repo.
fn rows_live_in_the_repo(root: &Path) {
    let path = root.join("home/registry.json");
    let repo = serde_json::to_string(&root.join("repo")).unwrap();
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("\"cwd\":\"/tmp\"", &format!("\"cwd\":{repo}"));
    fs::write(path, text).unwrap();
}

/// Run one native argv from the world's repo, every other variable cleared,
/// and assert its golden.
fn case(label: &str, root: &Path, extra: &[(&str, &str)], argv: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(argv)
        .current_dir(root.join("repo"))
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("HOME", root)
        .env("FNO_HOME", root.join("state"))
        .env("FNO_AGENTS_BIN", env!("CARGO_BIN_EXE_fno-agents"))
        .env("FNO_BIN", root.join("bin/fno-stub"))
        .env("FNO_AGENTS_HOME", root.join("home"))
        .env("FNO_STATE_DIR", root.join("state"))
        .env("FNO_SPACES_DIR", root.join("spaces"))
        .env("FNO_EVENTS_PATH", root.join("project/events.jsonl"))
        .envs(fno_agents::test_run::self_owner_env())
        .envs(extra.iter().copied())
        .output()
        .unwrap();
    let streams = vec![
        normalize(&String::from_utf8_lossy(&out.stdout), root),
        normalize(&String::from_utf8_lossy(&out.stderr), root),
    ];
    assert_golden(
        SUBJECT,
        label,
        &Golden {
            exit: out.status.code(),
            streams,
        },
        None,
    );
}

#[test]
fn drain_matches_the_python_goldens() {
    let tmp = world();
    let p = tmp.path();
    let argv = ["lead-state", "--drain", "epic-y"];
    case("drain counts the scope", p, &[], &argv);
    case("drain answers from the cache", p, &[], &argv);
}

#[test]
fn done_matches_the_python_goldens() {
    let tmp = world();
    let p = tmp.path();
    case(
        "done from an attended shell",
        p,
        &[],
        &["lead-state", "--done"],
    );
    let orphan = manifest(p, "epic-q", "sess-gone");
    case(
        "done clears an orphan manifest",
        p,
        &[],
        &["lead-state", "--done", "--scope", "epic-q"],
    );
    assert!(!orphan.exists(), "the orphan manifest stayed");
    let held = manifest(p, "epic-y", "sess-lead");
    case(
        "done vacates the holder from an attended shell",
        p,
        &[],
        &["lead-state", "--done", "--scope", "epic-y"],
    );
    assert!(!held.exists(), "the vacated manifest stayed");
    // A registry write imports the rows into graph.db and fences the file.
    let registry = fno_agents::registry_read::registry_text(&p.join("home/registry.json")).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&registry).unwrap();
    let live = rows["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "lead-live")
        .unwrap();
    assert!(live.get("role_scope").is_none_or(|s| s.is_null()), "{live}");
}

#[test]
fn done_as_an_agent_matches_the_python_goldens() {
    let tmp = world();
    let p = tmp.path();
    rows_live_in_the_repo(p);
    let held = manifest(p, "epic-y", "sess-lead");
    let lead = [("CLAUDE_CODE_SESSION_ID", "sess-lead")];
    case(
        "done refuses an unregistered agent",
        p,
        &[("CLAUDE_CODE_SESSION_ID", "sess-unknown")],
        &["lead-state", "--done"],
    );
    case(
        "done refuses a session with no role",
        p,
        &[("CLAUDE_CODE_SESSION_ID", "sess-plain")],
        &["lead-state", "--done"],
    );
    case(
        "done refuses another role's territory",
        p,
        &lead,
        &["lead-state", "--done", "--scope", "epic-z"],
    );
    case(
        "done steps the holder down",
        p,
        &lead,
        &["lead-state", "--done"],
    );
    assert!(!held.exists(), "the stepped-down manifest stayed");
}

#[test]
fn escalate_matches_the_python_goldens() {
    let tmp = world();
    let p = tmp.path();
    let argv = ["lead-state", "--escalate", "epic-y", "--stalled", "x-a001"];
    case("escalate records an operator question", p, &[], &argv);
    case("escalate keeps one question per stalled set", p, &[], &argv);
}

#[test]
fn last_run_matches_the_python_goldens() {
    let tmp = world();
    let p = tmp.path();
    case(
        "last run finds no lead walk",
        p,
        &[],
        &["board", "--user-command", "--last-run"],
    );
    case(
        "last run refuses a bad window",
        p,
        &[],
        &["board", "--user-command", "--last-run", "--since", "4x"],
    );
}

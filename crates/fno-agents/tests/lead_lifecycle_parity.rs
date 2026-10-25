//! parity-stage: characterization
//! parity-oracle: fno.lead.cli (checkin)
//!
//! Characterization for the lead lifecycle commands on their native argvs.
//! The goldens under tests/golden/lead_lifecycle/ were captured on 2026-10-25
//! from the Python commands over the fixtures in
//! tests/fixtures/lead_lifecycle/, before the Python legs were deleted.
//! Capture mode refuses: a golden can only be captured while the old leg runs.
//!
//! The temp root folds to `<ROOT>` (raw and canonical spellings) and its slug
//! to `<ROOTSLUG>`. The check-in's control-plane rows name the host's service
//! manager, so that block and the attention line that repeats it fold to
//! `<FOLDED>`.

use common::{assert_golden, Golden};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const SUBJECT: &str = "lead_lifecycle";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lead_lifecycle")
        .join(name)
}

fn slug(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

fn normalize(text: &str, root: &Path) -> String {
    let canonical = fs::canonicalize(root).unwrap();
    let text = text
        .replace(&slug(&canonical), "<ROOTSLUG>")
        .replace(&slug(root), "<ROOTSLUG>")
        .replace(&canonical.to_string_lossy().into_owned(), "<ROOT>")
        .replace(&root.to_string_lossy().into_owned(), "<ROOT>");
    let mut out: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in text.split('\n') {
        if line == "control plane:" {
            out.push("control plane: <FOLDED>".to_string());
            in_block = true;
            continue;
        }
        if in_block && line.starts_with("  ") {
            continue;
        }
        in_block = false;
        if line.starts_with("change: attention: ") {
            out.push("change: attention: <FOLDED>".to_string());
            continue;
        }
        out.push(line.to_string());
    }
    out.join("\n")
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// One fresh world: an agents home holding the fixture registry, a state dir
/// holding the seeded graph, a project journal, a git repo to stand in, and
/// stubs for the `fno` and `gh` calls the beat makes.
fn world(events: &str) -> tempfile::TempDir {
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
    fs::copy(fixture(events), p.join("project/events.jsonl")).unwrap();
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
    write_script(
        &p.join("bin/fno-stub"),
        "#!/bin/bash\ncase \"$*\" in\n  \
         \"doctor footprint --json\") echo '{\"cpu_pct\":12.5,\"processes\":40,\"lanes\":3}' ;;\n  \
         \"agents org --json\") echo '{\"summary\":{\"workers\":2},\"teams\":[]}' ;;\n  \
         *) exit 3 ;;\nesac\n",
    );
    write_script(&p.join("bin/gh"), "#!/bin/bash\necho \"[]\"\n");
    tmp
}

/// Run one native argv from the world's repo under its pins, every other
/// variable cleared, and assert its golden.
fn case(label: &str, root: &Path, argv: &[&str]) {
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
fn checkin_matches_the_python_goldens() {
    let argv = [
        "lead-checkin",
        "--user-command",
        "--scope",
        "epic-y",
        "--name",
        "gavin-fixture",
        "--theme",
        "fixture lead",
        "--no-emit",
    ];
    // No prior beat, then a measured prior loop beat the diff reads.
    for (label, events) in [
        ("checkin first", "events-first.jsonl"),
        ("checkin diff", "events-diff.jsonl"),
    ] {
        let tmp = world(events);
        case(label, tmp.path(), &argv);
    }
}

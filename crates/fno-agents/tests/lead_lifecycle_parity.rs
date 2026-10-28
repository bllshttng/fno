//! parity-stage: characterization
//! parity-oracle: fno.lead.cli (checkin, init, cancel, term)
//!
//! Characterization for the lead lifecycle commands on their native argvs.
//! The goldens under tests/golden/lead_lifecycle/ were captured on 2026-10-25
//! from the Python commands over the fixtures in
//! tests/fixtures/lead_lifecycle/, before the Python legs were deleted.
//! Capture mode refuses: a golden can only be captured while the old leg runs.
//! The check-in's board lines are the exception: the capture host refused the
//! board's GraphQL read, so those lines follow this world's `gh` stub instead.
//! A later hand edit added the check-in's `boot_revival` line and its coverage
//! bump when that reading joined the render, after the old leg was gone.
//!
//! The temp root folds to `<ROOT>` (raw and canonical spellings) and its slug
//! to `<ROOTSLUG>`. The check-in's control-plane rows name the host's service
//! manager, so that block and the attention line that repeats it fold to
//! `<FOLDED>`. A freshly minted manifest's `fno_id` folds to `<FNO_ID>`.

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
        if line.starts_with("fno_id: ") {
            out.push("fno_id: <FNO_ID>".to_string());
            continue;
        }
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

/// The space this world's repo keys.
fn space(root: &Path) -> PathBuf {
    root.join("spaces")
        .join(slug(&fs::canonicalize(root.join("repo")).unwrap()))
}

/// Arm the scope manifest a promoted lead over epic-y holds.
fn arm_manifest(root: &Path) {
    let leads = space(root).join("leads");
    fs::create_dir_all(&leads).unwrap();
    fs::copy(fixture("manifest-epic-y.md"), leads.join("epic-y.md")).unwrap();
    // The fixture's static created_at ages out of the 96h default term, and
    // an expired reading makes the declare case's own gate refuse it. Arm
    // with a fresh stamp (the runtime-rewrite idiom rows_live_in_the_repo
    // uses for cwd), so the gate's window never lapses on the wall clock.
    let path = leads.join("epic-y.md");
    let text = fs::read_to_string(&path).unwrap();
    let fresh = (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let stamped = format!("created_at: {fresh}");
    let text = text.replacen("created_at: 2026-10-24T08:55:00Z", &stamped, 1);
    assert!(
        text.contains(&stamped),
        "the fixture's created_at stamp drifted"
    );
    fs::write(path, text).unwrap();
}

/// Move every registry row's cwd into this world's repo, so the role's
/// space is the repo's space on every host.
fn rows_live_in_the_repo(root: &Path) {
    let path = root.join("home/registry.json");
    let repo = serde_json::to_string(&root.join("repo")).unwrap();
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("\"cwd\":\"/tmp\"", &format!("\"cwd\":{repo}"));
    fs::write(path, text).unwrap();
}

/// Run one native argv from the world's repo under its pins, every other
/// variable cleared, and assert its golden.
fn case(label: &str, root: &Path, argv: &[&str]) {
    case_in(label, root, "repo", &[], argv);
}

/// [`case`] from `cwd` under the root, with extra variables set.
fn case_in(label: &str, root: &Path, cwd: &str, extra: &[(&str, &str)], argv: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(argv)
        .current_dir(root.join(cwd))
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

#[test]
fn cancel_matches_the_python_goldens() {
    let tmp = world("events-first.jsonl");
    let p = tmp.path();
    let argv = ["lead-state", "--cancel", "--scope", "epic-y"];
    case("cancel without a manifest", p, &argv);
    arm_manifest(p);
    case("cancel sets the signal", p, &argv);
    let sentinel = space(p).join("leads/epic-y.cancelled");
    assert!(sentinel.is_file(), "the set left no sentinel");
    // The journal is a store behind events.jsonl; read it through the store.
    let query = fno_agents::event_store::EventQuery::of_types(&["cancel_signal_set"]);
    let journal =
        fno_agents::event_store::journal_text_checked(&space(p).join("events.jsonl"), &query)
            .unwrap();
    let set: serde_json::Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    assert_eq!(set["type"], "cancel_signal_set");
    assert_eq!(set["data"]["path"], sentinel.display().to_string());
    let clear = ["lead-state", "--cancel", "--scope", "epic-y", "--clear"];
    case("cancel clears the signal", p, &clear);
    assert!(!sentinel.exists(), "the clear left the sentinel");
}

#[test]
fn term_matches_the_python_goldens() {
    let tmp = world("events-first.jsonl");
    let p = tmp.path();
    case("term without a spec", p, &["lead-state", "--term"]);
    case(
        "term with a bad spec",
        p,
        &["lead-state", "--term", "span:4x"],
    );
    case(
        "term from an attended shell",
        p,
        &["lead-state", "--term", "span:4h"],
    );
    // The holder runs from outside its repo: the write keys on the row's
    // cwd, never the process cwd.
    rows_live_in_the_repo(p);
    arm_manifest(p);
    case_in(
        "term declares the holder's own term",
        p,
        "home",
        &[("CLAUDE_CODE_SESSION_ID", "sess-lead")],
        &["lead-state", "--term", "span:48h"],
    );
    let manifest = fs::read_to_string(space(p).join("leads/epic-y.md")).unwrap();
    assert!(manifest.contains("term: span:48h"), "{manifest}");
}

#[test]
fn init_matches_the_python_goldens() {
    let tmp = world("events-first.jsonl");
    let p = tmp.path();
    let init = |session: &str, scope: &str, force: bool| {
        let mut argv = vec![
            "lead-state",
            "--init",
            "--scope",
            scope,
            "--harness-session-id",
        ];
        argv.push(session);
        if force {
            argv.push("--force");
        }
        argv.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let run = |label: &str, extra: &[(&str, &str)], argv: Vec<String>| {
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        case_in(label, p, "repo", extra, &argv);
    };
    run(
        "init with the lead loop off",
        &[],
        init("sess-lead", "epic-y", false),
    );
    let config = p.join(".fno/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str("\n[lead]\nenabled = true\n");
    fs::write(&config, text).unwrap();
    case(
        "init without a session id",
        p,
        &["lead-state", "--init", "--scope", "epic-y"],
    );
    run(
        "init with a blank scope",
        &[],
        init("sess-lead", " ", false),
    );
    run(
        "init refused at admission",
        &[],
        init("sess-new", "epic-y", false),
    );
    // Admission reads the space journal for each session's measured
    // snapshot and the Stop fire after it.
    fs::create_dir_all(space(p)).unwrap();
    fs::copy(
        fixture("readiness-events.jsonl"),
        space(p).join("events.jsonl"),
    )
    .unwrap();
    // A predecessor's cancel signal never outlives a fresh init.
    let stale = space(p).join("leads/epic-y.cancelled");
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, "").unwrap();
    run(
        "init writes the manifest",
        &[],
        init("sess-lead", "epic-y", false),
    );
    let manifest = fs::read_to_string(space(p).join("leads/epic-y.md")).unwrap();
    assert!(
        manifest.contains("harness_session_id: sess-lead"),
        "{manifest}"
    );
    assert!(!stale.exists(), "init left the stale cancel signal");
    run(
        "init refuses an existing manifest",
        &[],
        init("sess-lead", "epic-y", false),
    );
    let lead = [("CLAUDE_CODE_SESSION_ID", "sess-lead")];
    run(
        "init over the holder's own role",
        &lead,
        init("sess-lead", "epic-y", true),
    );
    run(
        "init over other territory",
        &lead,
        init("sess-lead", "epic-z", true),
    );
    let plain = [("CLAUDE_CODE_SESSION_ID", "sess-plain")];
    run(
        "init from a row with no role",
        &plain,
        init("sess-plain", "epic-y", true),
    );
    // Every spelling of one epic set arms the same sorted, deduped file.
    case(
        "init over an epic set",
        p,
        &[
            "lead-state",
            "--init",
            "--scope",
            "epic-z",
            "--scope",
            "epic-y",
            "--scope",
            "epic-z",
            "--harness-session-id",
            "sess-lead",
            "--force",
        ],
    );
    assert!(space(p).join("leads/epic-y,epic-z.md").is_file());
    run(
        "init refuses a path-unsafe scope",
        &[],
        init("sess-lead", "a/b", false),
    );
}

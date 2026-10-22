//! parity-stage: characterization
//! parity-oracle: fno.roles.cli.context_catalog
//!
//! Characterization for `fno agents roles context`, on the Rust door as
//! `fno-agents spawn-overlay roles context`. The goldens under
//! tests/golden/roles_context/ were captured from the Python verb and its
//! `fno.roles.context` catalog on this same fixture before both were
//! deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). Capture mode now
//! refuses: a golden can only be captured while the old leg runs.
//!
//! The fixture pins what is easiest to lose: the config file order of the
//! artifacts, pathlib's collapse of `./` and `//`, the relative path
//! anchored to the repo root, and the Python error class named for a
//! missing file, a directory and a path through a file. The fixture path
//! folds to `<FIXTURE>` in both its spelled and its resolved form.

use common::{assert_golden, Golden};
use std::path::Path;
use std::process::Command;

mod common;

const CONFIG: &str = r#"[context.artifacts.zeta]
path = "docs/brief.md"
sensitivity = "public"

[context.artifacts.absent]
path = "./docs/missing.md"

[context.artifacts.folder]
path = "docs"
sensitivity = "restricted"
"#;

fn normalize(text: &str, fixture: &Path) -> String {
    let spelled = fixture.to_string_lossy().into_owned();
    let resolved = std::fs::canonicalize(fixture)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| spelled.clone());
    let mut spellings = vec![spelled, resolved];
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
    spellings.iter().fold(text.to_string(), |text, s| {
        text.replace(s.as_str(), "<FIXTURE>")
    })
}

#[test]
fn roles_context_matches_the_python_goldens() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = tmp.path();
    let repo = fixture.join("repo");
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/brief.md"), "# Brief\n\ncaf\u{e9} facts.\n").unwrap();
    std::fs::write(fixture.join("config.toml"), CONFIG).unwrap();
    let snapshot = format!(
        "[context.artifacts.brief]\npath = \"{}/docs//brief.md\"\nsensitivity = \"sensitive\"\n\n\
         [context.artifacts.through-a-file]\npath = \"docs/brief.md/inner\"\n",
        repo.display()
    );
    std::fs::write(fixture.join("snapshot.toml"), snapshot).unwrap();
    std::fs::write(fixture.join("empty.toml"), "").unwrap();

    let cases: [(&str, &str, &[&str]); 6] = [
        (
            "context text lists each artifact",
            "config.toml",
            &["context"],
        ),
        (
            "context json dumps every reference",
            "config.toml",
            &["context", "--json"],
        ),
        (
            "context stamps the snapshot",
            "snapshot.toml",
            &["-J", "context", "--snapshot", "snap-9"],
        ),
        (
            "context with no artifacts says so",
            "empty.toml",
            &["context"],
        ),
        (
            "context refuses an extra argument",
            "config.toml",
            &["context", "extra"],
        ),
        ("context help", "config.toml", &["context", "--help"]),
    ];
    for (label, config, argv) in cases {
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["spawn-overlay", "roles"])
            .args(argv)
            .current_dir(fixture)
            .env("FNO_CONFIG", fixture.join(config))
            .env("FNO_REPO_ROOT", &repo)
            .envs(fno_agents::test_run::self_owner_env())
            .output()
            .unwrap();
        let code = out.status.code();
        let mut streams = vec![normalize(&String::from_utf8_lossy(&out.stdout), fixture)];
        if code != Some(0) {
            streams.push(normalize(&String::from_utf8_lossy(&out.stderr), fixture));
        }
        assert_golden(
            "roles_context",
            label,
            &Golden {
                exit: code,
                streams,
            },
            None,
        );
    }
}

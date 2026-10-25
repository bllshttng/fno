//! parity-stage: characterization
//! parity-oracle: cli/src/fno/plugins/cli.py
//!
//! Characterization for `fno config plugins`, on the Rust door as
//! `fno-agents spawn-overlay plugins`. The goldens under
//! tests/golden/plugins/ were captured from the Python commands before they
//! were deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). The cases in
//! tests/fixtures/plugins-cases.json run in order against one role root, so
//! activation, upgrade, deactivation and a corrupt registry are each read
//! from the state the case before left.
//!
//! Four things normalize on both sides. The role root folds to `<ROOT>` and
//! the fixture packs to `<PACKS>`. The running footnote version folds to
//! `<VERSION>`, so a release does not move a golden. The text after a YAML,
//! JSON or schema error marker folds to `<ERROR>`: that wording was the
//! parser's own, never a contract. Every digest is compared as captured, so
//! the pack and role-manifest digests stay byte-equal with the Python leg.

use common::{assert_golden, Golden};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const MARKERS: [&str; 3] = [
    "failed schema validation: ",
    "is not valid JSON: ",
    "/malformed/plugin.yaml: ",
];

fn packs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins")
}

fn normalize(text: &str, root: &Path) -> String {
    let mut text = text.to_string();
    let mut roots = vec![root.to_string_lossy().into_owned()];
    if let Ok(real) = std::fs::canonicalize(root) {
        roots.push(real.to_string_lossy().into_owned());
    }
    roots.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for raw in roots {
        text = text.replace(&raw, "<ROOT>");
    }
    let text = text
        .replace(&packs().to_string_lossy().into_owned(), "<PACKS>")
        .replace(env!("CARGO_PKG_VERSION"), "<VERSION>");
    text.split('\n')
        .map(|line| {
            for marker in MARKERS {
                if let Some(at) = line.find(marker) {
                    let tail = if line.ends_with("\",") {
                        "\","
                    } else if line.ends_with('"') {
                        "\""
                    } else {
                        ""
                    };
                    return format!("{}<ERROR>{tail}", &line[..at + marker.len()]);
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn plugins_matches_the_python_goldens() {
    let cases: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins-cases.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("roles");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let packs = packs().to_string_lossy().into_owned();
    let root_arg = root.to_string_lossy().into_owned();
    for case in &cases {
        let label = case["label"].as_str().unwrap();
        if case["setup"] == "corrupt" {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(".pack-registry.json"), "{").unwrap();
        }
        if let Some(file) = case["file"].as_str() {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            assert_golden(
                "plugins",
                label,
                &Golden {
                    exit: None,
                    streams: vec![normalize(&text, &root)],
                },
                None,
            );
            continue;
        }
        let argv: Vec<String> = case["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                a.as_str()
                    .unwrap()
                    .replace("{ROOT}", &root_arg)
                    .replace("{PACKS}", &packs)
            })
            .collect();
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["spawn-overlay", "plugins"])
            .args(&argv)
            .current_dir(tmp.path())
            .env("HOME", &home)
            .env_remove("FNO_ROLES_ROOT")
            .envs(fno_agents::test_run::self_owner_env())
            .output()
            .unwrap();
        let code = out.status.code();
        let mut streams = vec![normalize(&String::from_utf8_lossy(&out.stdout), &root)];
        if code != Some(0) {
            streams.push(normalize(&String::from_utf8_lossy(&out.stderr), &root));
        }
        assert_golden(
            "plugins",
            label,
            &Golden {
                exit: code,
                streams,
            },
            None,
        );
    }
    // The packs the repo ships verify clean: every condition checked and
    // passed, which is what bundling and the growth-launch skill rely on.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fresh = tmp.path().join("fresh").to_string_lossy().into_owned();
    for pack in ["plugins/growth-studio", "plugins/company-conformance"] {
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["spawn-overlay", "plugins", "--root", &fresh, "verify"])
            .arg(repo.join(pack))
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{pack} must verify clean:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

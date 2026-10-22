//! parity-stage: characterization
//! parity-oracle: fno.inbox.settings.read_peer_surfaces (with
//! read_surface_patterns and read_peer_projects, the deleted Python readers;
//! goldens captured from them over the fixtures below before deletion)
//!
//! Characterization tests for `compaction inbox-peers --json`, the native
//! answer behind `fno inbox peers`. The contract is the Python readers' maps
//! in config order, their `str()` coercion of list items, and their stderr
//! drop warnings: a peer that is not a mapping, a `surfaces` value that is
//! not a list, and a surface whose globs are not a list. A peer with no
//! `surfaces` or no `project` drops silently.

use common::{assert_golden, Golden};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

mod common;

const FIXTURES: &[(&str, &str, &str)] = &[
    (
        "well_formed",
        ".fno/config.toml",
        "[inbox.peers.web-lead]\nsurfaces = [\"web\", \"api\"]\nproject = \"storefront\"\n\n[inbox.peers.cli-lead]\nsurfaces = [\"cli\"]\n\n[inbox.peers.empty-peer]\n\n[inbox.surface_patterns]\nweb = [\"web/**\", \"*.css\"]\napi = [\"crates/api/**\"]\ncli = [\"cli/**\"]\n",
    ),
    (
        "not_a_mapping",
        ".fno/config.toml",
        "[inbox.peers]\nzeta = \"loose-string\"\n\n[inbox.peers.alpha]\nsurfaces = [\"api\"]\nproject = \"backend\"\n",
    ),
    (
        "not_a_list",
        ".fno/config.toml",
        "[inbox.peers.alpha]\nsurfaces = \"api-server\"\nproject = \"backend\"\n\n[inbox.peers.beta]\nsurfaces = [\"cli\", 7, true]\n\n[inbox.surface_patterns]\napi = \"crates/**\"\ncli = [\"cli/**\"]\n",
    ),
    (
        "legacy_yaml",
        ".fno/settings.yaml",
        "schema_version: 1\nconfig:\n  inbox:\n    peers:\n      docs-lead:\n        surfaces: [docs]\n        project: handbook\n      ops: [not, a, mapping]\n    surface_patterns:\n      docs: [\"docs/**\", \"*.md\"]\n",
    ),
    ("no_config", "", ""),
];

fn tmp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-inbox-peers-parity-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn inbox_peers_matches_the_python_readers() {
    for (label, rel, body) in FIXTURES {
        let dir = tmp_dir(label);
        if !rel.is_empty() {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &dir)
            .env("FNO_AGENTS_HOME", dir.join("agents-home"))
            .env("FNO_TEST_HERMETIC", "1")
            .envs(fno_agents::test_run::self_owner_env())
            .current_dir(&dir)
            .args(["compaction", "inbox-peers", "--json"])
            .output()
            .unwrap();
        let rust = Golden {
            exit: out.status.code(),
            streams: vec![
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ],
        };
        assert_golden("inbox_peers", label, &rust, None);
        let _ = fs::remove_dir_all(&dir);
    }
}

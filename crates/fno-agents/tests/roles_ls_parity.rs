//! parity-stage: characterization
//! parity-oracle: fno.roles.cli.list_roles
//!
//! Characterization for `fno agents roles ls`, the first roles command on the
//! Rust door (`fno-agents spawn-overlay roles ls`). The goldens under
//! tests/golden/roles_ls/ were captured from the Python command on this same
//! fixture before it was deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). Capture mode now
//! refuses: a golden can only be captured while the old leg runs.
//!
//! Two fields normalize on both sides. The fixture path folds to
//! `<FIXTURE>`. The `error` of an `invalid` row folds to `<ERROR>`: that
//! text was pydantic's or the JSON decoder's own wording, never a
//! contract. An `unreadable` row keeps its OS error text byte for byte.

use common::{assert_golden, Golden};
use serde_json::json;
use std::path::Path;
use std::process::Command;

mod common;

fn manifest(role_id: &str) -> serde_json::Value {
    json!({
        "role": {"id": role_id, "function_id": "marketing"},
        "function": {"id": "marketing"},
        "mission": "Grow the caf\u{e9}",
        "deliverable_kinds": ["brief"],
        "authority_ceiling": "internal",
        "review_policy": {"required": "yes", "minimum_reviewers": "1"},
        "delivery_policy": {},
        "default_topology": "solo",
        "routing_hint": {"provider": "anthropic"},
    })
}

/// The layered root: two valid sources (one nested, one with a padded
/// role id and lax-typed review fields), a schema-invalid source that
/// still names its role, a file that is not JSON, a stray non-JSON file,
/// and a `plan` layer that is a file.
fn build_fixture(roles: &Path) {
    let files = [
        (
            "built-in/\u{e9}quipe.json",
            json!({
                "layer": "built-in",
                "source_id": "built-in/\u{e9}quipe.json",
                "snapshot_revision": "snapshot-1",
                "role": {"id": "owner", "function_id": "marketing"},
                "manifest": manifest("owner"),
            })
            .to_string(),
        ),
        ("built-in/readme.txt", "not a definition".to_string()),
        (
            "company/sub/nested.json",
            json!({
                "layer": "company",
                "source_id": "company/sub/nested.json",
                "snapshot_revision": "snapshot-1",
                "role": {"id": " analyst ", "function_id": "marketing"},
                "manifest": manifest("analyst"),
            })
            .to_string(),
        ),
        (
            "project/broken.json",
            json!({
                "layer": "project",
                "source_id": "project/broken.json",
                "snapshot_revision": "snapshot-1",
                "role": {"id": "broken", "function_id": "support"},
                "manifest": {"mission": "missing required manifest fields"},
                "status": "valid",
            })
            .to_string(),
        ),
        ("project/bad.json", "{\"a\":".to_string()),
        ("plan", "a layer that is a file".to_string()),
    ];
    for (rel, text) in files {
        let path = roles.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// Fold the fixture path, and the error text of every `invalid` row in the
/// pretty JSON listing (one row per `  {` .. `  }` block).
fn normalize(text: &str, fixture: &Path) -> String {
    let text = text.replace(&fixture.to_string_lossy().into_owned(), "<FIXTURE>");
    let mut out: Vec<String> = Vec::new();
    let mut row: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if line == "  {" {
            row = vec![line];
        } else if !row.is_empty() {
            row.push(line);
            if line.starts_with("  }") {
                let invalid = row.contains(&"    \"status\": \"invalid\"");
                for r in row.drain(..) {
                    if invalid && r.starts_with("    \"error\": \"") {
                        out.push("    \"error\": \"<ERROR>\",".into());
                    } else {
                        out.push(r.into());
                    }
                }
            }
        } else {
            out.push(line.into());
        }
    }
    out.join("\n")
}

#[test]
fn roles_ls_matches_the_python_goldens() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = tmp.path();
    let roles = fixture.join("roles");
    build_fixture(&roles);
    let roles_arg = roles.to_string_lossy().into_owned();
    let missing = format!("plan={}", fixture.join("extra/missing.json").display());
    let absent = fixture.join("absent").to_string_lossy().into_owned();
    let group = ["--root", roles_arg.as_str(), "--source", missing.as_str()];
    let cases: [(&str, Vec<&str>); 4] = [
        (
            "ls lists every source with its status",
            [&group[..], &["ls"][..]].concat(),
        ),
        (
            "ls json keeps invalid and unreadable sources",
            [&group[..], &["ls", "--json"][..]].concat(),
        ),
        (
            "ls refuses a source without a layer",
            vec!["--root", roles_arg.as_str(), "--source", "bogus", "ls"],
        ),
        (
            "ls on a missing root lists nothing",
            vec!["--root", absent.as_str(), "ls"],
        ),
    ];
    for (label, argv) in cases {
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["spawn-overlay", "roles"])
            .args(&argv)
            .env_remove("FNO_ROLES_ROOT")
            .envs(fno_agents::test_run::self_owner_env())
            .output()
            .unwrap();
        let code = out.status.code();
        let mut streams = vec![normalize(&String::from_utf8_lossy(&out.stdout), fixture)];
        if code != Some(0) {
            streams.push(normalize(&String::from_utf8_lossy(&out.stderr), fixture));
        }
        assert_golden(
            "roles_ls",
            label,
            &Golden {
                exit: code,
                streams,
            },
            None,
        );
    }
}

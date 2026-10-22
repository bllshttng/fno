//! parity-stage: characterization
//! parity-oracle: fno.roles.cli.show_role
//!
//! Characterization for `fno agents roles show`, on the Rust door as
//! `fno-agents spawn-overlay roles show`. The goldens under
//! tests/golden/roles_show/ were captured from the Python verb on this same
//! fixture before it was deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). Capture mode now
//! refuses: a golden can only be captured while the old leg runs.
//!
//! Two fields normalize on both sides, as in roles_ls_parity.rs. The
//! fixture path folds to `<FIXTURE>`. The `error` of an `invalid` row folds
//! to `<ERROR>`, in the JSON rows and in the text block from `  error:` to
//! its `  raw:` line: that text was pydantic's or the JSON decoder's own
//! wording, never a contract. An `unreadable` row keeps its OS error text.

use common::{assert_golden, Golden};
use serde_json::json;
use std::path::Path;
use std::process::Command;

mod common;

fn source(layer: &str, source_id: &str, role_id: &str, function_id: &str) -> String {
    json!({
        "layer": layer,
        "source_id": source_id,
        "snapshot_revision": "snapshot-1",
        "role": {"id": role_id, "function_id": function_id},
        "manifest": {
            "role": {"id": role_id, "function_id": function_id},
            "function": {"id": function_id},
            "mission": "Grow the caf\u{e9}",
            "deliverable_kinds": ["brief"],
            "authority_ceiling": "internal",
            "review_policy": {"required": "yes", "minimum_reviewers": "1"},
            "delivery_policy": {},
            "default_topology": "solo",
            "routing_hint": {"provider": "anthropic"},
        },
    })
    .to_string()
}

/// The layered root: `marketing/owner` in two layers (the lower one
/// shadowed), `support/owner` in a nested company source, a schema-invalid
/// source that still names its role, a file that is not JSON, a stray
/// non-JSON file, and a `plan` layer that is a file.
fn build_fixture(roles: &Path) {
    let files = [
        (
            "built-in/\u{e9}quipe.json",
            source(
                "built-in",
                "built-in/\u{e9}quipe.json",
                "owner",
                "marketing",
            ),
        ),
        ("built-in/readme.txt", "not a definition".to_string()),
        (
            "company/owner.json",
            source("company", "company/owner.json", "owner", "marketing"),
        ),
        (
            "company/sub/support.json",
            source("company", "company/sub/support.json", "owner", "support"),
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

/// Fold the fixture path, and the error text of every `invalid` row: one
/// row per `  {` .. `  }` block in the JSON listing, and in the text
/// listing every line from `  error:` up to the row's `  raw:` line.
fn normalize(text: &str, fixture: &Path) -> String {
    let text = text.replace(&fixture.to_string_lossy().into_owned(), "<FIXTURE>");
    let mut out: Vec<String> = Vec::new();
    let mut row: Vec<&str> = Vec::new();
    let mut invalid_header = false;
    let mut in_error = false;
    for line in text.split('\n') {
        if in_error {
            if !line.starts_with("  raw: ") {
                continue;
            }
            in_error = false;
        }
        if line == "  {" {
            row = vec![line];
            continue;
        }
        if !row.is_empty() {
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
            continue;
        }
        if invalid_header && line.starts_with("  error: ") {
            out.push("  error: <ERROR>".into());
            in_error = true;
        } else {
            out.push(line.into());
        }
        if !line.starts_with("  ") {
            invalid_header = line.contains(" status=invalid ");
        }
    }
    out.join("\n")
}

#[test]
fn roles_show_matches_the_python_goldens() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = tmp.path();
    let roles = fixture.join("roles");
    build_fixture(&roles);
    let roles_arg = roles.to_string_lossy().into_owned();
    let missing = format!("plan={}", fixture.join("extra/missing.json").display());
    let group = ["--root", roles_arg.as_str(), "--source", missing.as_str()];
    let cases: [(&str, Vec<&str>); 8] = [
        (
            "show text marks the contributing and shadowed sources",
            [&group[..], &["show", "owner"][..]].concat(),
        ),
        (
            "show json keeps role-less invalid and unreadable sources",
            [&group[..], &["show", "owner", "--json"][..]].concat(),
        ),
        (
            "show function slash role filters by function",
            [&["-J"][..], &group[..], &["show", "marketing/owner"][..]].concat(),
        ),
        (
            "show keeps a schema-invalid source that names its role",
            [&group[..], &["show", "broken"][..]].concat(),
        ),
        (
            "show refuses a malformed role token",
            [&group[..], &["show", "a/b/c"][..]].concat(),
        ),
        ("show without a role refuses", vec!["show"]),
        ("show help", vec!["show", "--help"]),
        (
            "show reads the role after the end of options",
            [
                &["-J"][..],
                &group[..],
                &["show", "--", "marketing/owner"][..],
            ]
            .concat(),
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
            "roles_show",
            label,
            &Golden {
                exit: code,
                streams,
            },
            None,
        );
    }
}

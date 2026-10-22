//! parity-stage: characterization
//! parity-oracle: fno.roles.cli.resolve_role_command
//!
//! Characterization for `fno agents roles resolve`, on the Rust door as
//! `fno-agents spawn-overlay roles resolve`, and so for the resolver behind
//! the `roles-resolve` kind. The goldens under tests/golden/roles_resolve/
//! were captured from the Python verb and its resolver on this same fixture
//! before both were deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). Capture mode now
//! refuses: a golden can only be captured while the old leg runs.
//!
//! The resolved case pins byte parity where it is easiest to lose: the
//! manifest and bundle digests over a lax bool, a lax int, a non-ASCII
//! mission and an offset datetime with a fraction. The fixture path folds
//! to `<FIXTURE>`, and the text after `cannot validate <path>: ` folds to
//! `<ERROR>`: that text was pydantic's own wording, never a contract.

use common::{assert_golden, Golden};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

mod common;

fn manifest(role_id: &str, function_id: &str, overrides: &Value) -> Value {
    let mut manifest = json!({
        "role": {"id": role_id, "function_id": function_id},
        "function": {"id": function_id},
        "mission": "Grow the caf\u{e9}",
        "deliverable_kinds": ["brief"],
        "required_capabilities": ["growth.draft"],
        "authority_ceiling": "internal",
        "context_selectors": [
            {"kind": "brief", "identifier": "company-brief", "max_sensitivity": "sensitive"}
        ],
        "review_policy": {"required": "yes", "minimum_reviewers": "1"},
        "delivery_policy": {"required_evidence": ["artifact-exists"]},
        "default_topology": "solo",
        "routing_hint": {"provider": "anthropic", "model": "opus"},
    });
    for (key, value) in overrides.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    manifest
}

fn source(
    dir: &Path,
    layer: &str,
    name: &str,
    role: (&str, &str),
    revision: &str,
    overrides: Value,
) {
    let id = format!("{layer}/{name}.json");
    let path = dir.join(&id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let document = json!({
        "layer": layer,
        "source_id": id,
        "snapshot_revision": revision,
        "role": {"id": role.0, "function_id": role.1},
        "manifest": manifest(role.0, role.1, &overrides),
    });
    std::fs::write(path, document.to_string()).unwrap();
}

/// `owner` overlays a stricter company layer on its built-in base. `wide`
/// widens its authority, `drift` mixes revisions, `twin` is ambiguous in
/// one layer, and `dup` lives in two functions.
fn build_fixture(roles: &Path) {
    let none = || json!({});
    let s1 = "snapshot-1";
    source(
        roles,
        "built-in",
        "owner",
        ("owner", "marketing"),
        s1,
        none(),
    );
    source(
        roles,
        "company",
        "owner",
        ("owner", "marketing"),
        s1,
        json!({
            "review_policy": {
                "required": true,
                "minimum_reviewers": 2,
                "required_review_kinds": ["brand"],
            },
            "context_selectors": [{
                "kind": "brief",
                "identifier": "company-brief",
                "max_sensitivity": "internal",
                "requires_freshness": true,
            }],
        }),
    );
    source(roles, "built-in", "wide", ("wide", "marketing"), s1, none());
    source(
        roles,
        "project",
        "wide",
        ("wide", "marketing"),
        s1,
        json!({"authority_ceiling": "external"}),
    );
    source(
        roles,
        "built-in",
        "drift",
        ("drift", "marketing"),
        s1,
        none(),
    );
    source(
        roles,
        "company",
        "drift",
        ("drift", "marketing"),
        "snapshot-2",
        none(),
    );
    source(
        roles,
        "built-in",
        "twin-a",
        ("twin", "marketing"),
        s1,
        none(),
    );
    source(
        roles,
        "built-in",
        "twin-b",
        ("twin", "marketing"),
        s1,
        none(),
    );
    source(roles, "built-in", "dup", ("dup", "marketing"), s1, none());
    source(roles, "company", "dup", ("dup", "support"), s1, none());
}

fn write(path: &Path, value: Value) -> String {
    std::fs::write(path, value.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

fn normalize(text: &str, fixture: &Path) -> String {
    let text = text.replace(&fixture.to_string_lossy().into_owned(), "<FIXTURE>");
    let Some(start) = text.find("cannot validate ") else {
        return text;
    };
    match text[start..].find(": ") {
        Some(colon) => format!("{}<ERROR>\n", &text[..start + colon + 2]),
        None => text,
    }
}

#[test]
fn roles_resolve_matches_the_python_goldens() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = tmp.path();
    let roles = fixture.join("roles");
    build_fixture(&roles);
    let broken = fixture.join("broken");
    source(
        &broken,
        "built-in",
        "owner",
        ("owner", "marketing"),
        "snapshot-1",
        json!({}),
    );
    std::fs::write(broken.join("plan"), "a layer that is a file").unwrap();

    let scope = json!({"node_id": "x-a8c0", "attempt_id": "attempt-1", "role_id": "owner"});
    let brief = json!({
        "kind": "brief",
        "identifier": "company-brief",
        "provenance": "briefs/company-brief.md",
        "work_order_scope": scope,
        "content_digest": "a".repeat(64),
        "content_revision": "v1",
        "snapshot_revision": "snapshot-1",
        "fresh_until": "2099-01-01T00:00:00.5+05:30",
        "sensitivity": "internal",
        "byte_size": 120,
        "readable": true,
    });
    let mut bigger = brief.clone();
    bigger["provenance"] = json!("briefs/company-brief-long.md");
    bigger["content_digest"] = json!("b".repeat(64));
    bigger["byte_size"] = json!("300");
    bigger["fresh_until"] = json!("2099-01-01T00:00:00Z");
    let mut stale_brief = brief.clone();
    stale_brief["fresh_until"] = json!("2000-01-01T00:00:00Z");
    let caps = write(
        &fixture.join("caps.json"),
        json!([{
            "capability": "growth.draft",
            "available": "yes",
            "source_id": "founder-launch",
            "snapshot_revision": "snapshot-1",
            "work_order_scope": scope,
        }]),
    );
    let ctx = write(&fixture.join("ctx.json"), json!([bigger, brief]));
    let stale = write(&fixture.join("stale.json"), json!([stale_brief]));
    let bad = write(
        &fixture.join("bad-caps.json"),
        json!([{"capability": "growth.draft"}]),
    );

    let roles_arg = roles.to_string_lossy().into_owned();
    let broken_arg = broken.to_string_lossy().into_owned();
    let group = vec!["--root", roles_arg.as_str()];
    let order = vec!["--work-order", "x-a8c0", "--attempt", "attempt-1"];
    let full = [
        &order[..],
        &["--capabilities", caps.as_str(), "--context", ctx.as_str()][..],
    ]
    .concat();
    let cat = |parts: &[Vec<&str>]| -> Vec<String> {
        parts.concat().into_iter().map(String::from).collect()
    };
    let resolve =
        |args: &[&'static str]| -> Vec<&'static str> { [&["resolve"][..], args].concat() };
    let caps_only = vec!["--capabilities", caps.as_str()];
    let stale_ctx = vec!["--capabilities", caps.as_str(), "--context", stale.as_str()];
    let bad_caps = vec!["--capabilities", bad.as_str()];
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "resolve text names the contributing source",
            cat(&[group.clone(), resolve(&["owner"]), full.clone()]),
        ),
        (
            "resolve json pins the manifest and bundle digests",
            cat(&[group.clone(), resolve(&["owner", "--json"]), full.clone()]),
        ),
        (
            "resolve without the capability blocks",
            cat(&[group.clone(), resolve(&["owner"]), order.clone()]),
        ),
        (
            "resolve without context blocks",
            cat(&[
                group.clone(),
                resolve(&["owner", "--json"]),
                order.clone(),
                caps_only,
            ]),
        ),
        (
            "resolve with stale context blocks",
            cat(&[
                group.clone(),
                resolve(&["owner", "--json"]),
                order.clone(),
                stale_ctx,
            ]),
        ),
        (
            "resolve on another snapshot blocks",
            cat(&[
                vec!["-J"],
                group.clone(),
                resolve(&["marketing/owner", "--snapshot", "snapshot-2"]),
                full.clone(),
            ]),
        ),
        (
            "resolve refuses an authority expansion",
            cat(&[group.clone(), resolve(&["wide"]), order.clone()]),
        ),
        (
            "resolve refuses a mixed revision",
            cat(&[group.clone(), resolve(&["drift", "--json"]), order.clone()]),
        ),
        (
            "resolve refuses an ambiguous layer",
            cat(&[group.clone(), resolve(&["twin", "--json"]), order.clone()]),
        ),
        (
            "resolve of an unknown role is not found",
            cat(&[group.clone(), resolve(&["ghost", "--json"]), order.clone()]),
        ),
        (
            "resolve refuses a role in two functions",
            cat(&[group.clone(), resolve(&["dup"]), order.clone()]),
        ),
        (
            "resolve blocks on an unchecked source",
            cat(&[
                vec!["--root", broken_arg.as_str()],
                resolve(&["owner", "--json"]),
                full.clone(),
            ]),
        ),
        (
            "resolve refuses an invalid capabilities file",
            cat(&[group.clone(), resolve(&["owner"]), order.clone(), bad_caps]),
        ),
        (
            "resolve without a work order refuses",
            cat(&[group.clone(), resolve(&["owner", "--attempt", "attempt-1"])]),
        ),
        ("resolve help", cat(&[resolve(&["--help"])])),
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
            "roles_resolve",
            label,
            &Golden {
                exit: code,
                streams,
            },
            None,
        );
    }
}

//! parity-stage: characterization
//! parity-oracle: fno.agents.model_routing._default_business_lookup
//!
//! Characterization for the business-role lookup model routing reads, on
//! the Rust door as `fno-agents spawn-overlay` kind `business-role`. The
//! goldens under tests/golden/roles_routing/ were captured from the Python
//! `_default_business_lookup` and `resolve_manifest_routing` on this same
//! fixture before both were deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). Capture mode now
//! refuses: a golden can only be captured while the old leg runs.
//!
//! The cases pin the root tri-state (a missing default root is not_found,
//! a configured root that is missing or a file blocks), the overlay chain
//! refusals, and the routing projection with its source digest. The answer
//! renders as Python's `json.dumps(indent=2, sort_keys=True)`. The fixture
//! path folds to `<FIXTURE>`, and an exception's own wording in `detail`
//! folds to `<ERROR>`: it was Python's text, never a contract.

use common::{assert_golden, Golden};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;

fn source(root: &Path, layer: &str, name: &str, function_id: &str, revision: &str, over: Value) {
    let mut manifest = json!({
        "role": {"id": "publisher", "function_id": function_id},
        "function": {"id": function_id},
        "mission": "Publish one bounded artifact.",
        "deliverable_kinds": ["brief"],
        "authority_ceiling": "internal",
        "review_policy": {"required": true, "minimum_reviewers": 1},
        "delivery_policy": {"required_evidence": ["artifact"]},
        "default_topology": "direct",
        "routing_hint": {"provider": "zai", "model": "base-model"},
    });
    for (key, value) in over.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    let id = format!("{layer}/{name}.json");
    let document = json!({
        "layer": layer,
        "source_id": id,
        "snapshot_revision": revision,
        "role": {"id": "publisher", "function_id": function_id},
        "manifest": manifest,
    });
    let path = root.join(&id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, document.to_string()).unwrap();
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), sorted(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

fn normalize(mut answer: Value, fixture: &Path) -> String {
    let detail = answer.get("detail").and_then(Value::as_str).unwrap_or("");
    let python_error = detail.split_once(": ").is_some_and(|(head, _)| {
        head.ends_with("Error") && head.chars().all(char::is_alphanumeric)
    });
    if python_error {
        answer["detail"] = json!("<ERROR>");
    }
    let mut text = serde_json::to_string_pretty(&sorted(&answer)).unwrap() + "\n";
    let spelled = fixture.to_string_lossy().into_owned();
    let resolved = std::fs::canonicalize(fixture)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| spelled.clone());
    let mut spellings = vec![spelled, resolved];
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for spelling in spellings {
        text = text.replace(&spelling, "<FIXTURE>");
    }
    text
}

#[test]
fn business_role_lookup_matches_the_python_goldens() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = tmp.path();
    std::fs::create_dir_all(fixture.join("repo")).unwrap();
    std::fs::write(fixture.join("file"), "not a directory").unwrap();
    let none = || json!({});
    let s1 = "snapshot-1";
    let comms = "communications";

    let tight = fixture.join("tight");
    source(
        &tight,
        "built-in",
        "publisher",
        comms,
        s1,
        json!({"authority_ceiling": "external"}),
    );
    source(
        &tight,
        "project",
        "publisher",
        comms,
        s1,
        json!({"routing_hint": {"provider": "zai", "model": "tightened-model"}}),
    );
    let hintless = fixture.join("hintless");
    source(
        &hintless,
        "company",
        "publisher",
        comms,
        s1,
        json!({"routing_hint": null}),
    );
    let mixed = fixture.join("mixed");
    source(&mixed, "built-in", "publisher", comms, s1, none());
    source(&mixed, "project", "publisher", comms, "snapshot-2", none());
    let wide = fixture.join("wide");
    source(&wide, "built-in", "publisher", comms, s1, none());
    source(
        &wide,
        "project",
        "publisher",
        comms,
        s1,
        json!({"authority_ceiling": "external"}),
    );
    let split = fixture.join("split");
    source(&split, "built-in", "publisher", comms, s1, none());
    source(&split, "company", "publisher", "sales", s1, none());
    let corrupt = fixture.join("corrupt");
    source(&corrupt, "built-in", "publisher", comms, s1, none());
    std::fs::create_dir_all(corrupt.join("project")).unwrap();
    std::fs::write(corrupt.join("project/broken.json"), "{not-json").unwrap();
    let twin = fixture.join("twin");
    source(&twin, "built-in", "publisher-a", comms, s1, none());
    source(&twin, "built-in", "publisher-b", comms, s1, none());

    let missing = fixture.join("missing");
    let file = fixture.join("file");
    let cases: [(&str, Option<&Path>, &str); 11] = [
        (
            "an unconfigured missing root is not found",
            None,
            "publisher",
        ),
        (
            "a configured missing root blocks",
            Some(missing.as_path()),
            "publisher",
        ),
        (
            "a root that is a file blocks",
            Some(file.as_path()),
            "publisher",
        ),
        (
            "a tightening overlay projects the highest layer",
            Some(tight.as_path()),
            "publisher",
        ),
        (
            "a manifest without a routing hint projects nothing",
            Some(hintless.as_path()),
            "publisher",
        ),
        (
            "a mixed revision blocks",
            Some(mixed.as_path()),
            "publisher",
        ),
        (
            "an authority expansion blocks",
            Some(wide.as_path()),
            "publisher",
        ),
        (
            "a role in two functions blocks",
            Some(split.as_path()),
            "publisher",
        ),
        (
            "a corrupt source blocks",
            Some(corrupt.as_path()),
            "publisher",
        ),
        (
            "an ambiguous layer blocks",
            Some(twin.as_path()),
            "publisher",
        ),
        (
            "an unknown role is not found",
            Some(tight.as_path()),
            "ghost",
        ),
    ];
    for (label, root, role) in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        command
            .arg("spawn-overlay")
            .env("FNO_REPO_ROOT", fixture.join("repo"))
            .envs(fno_agents::test_run::self_owner_env())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match root {
            Some(root) => command.env("FNO_ROLES_ROOT", root),
            None => command.env_remove("FNO_ROLES_ROOT"),
        };
        let mut child = command.spawn().unwrap();
        let payload = json!({"kind": "business-role", "role": role});
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let answer: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "{label}: no JSON answer ({e}): {stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert_golden(
            "roles_routing",
            label,
            &Golden {
                exit: out.status.code(),
                streams: vec![normalize(answer, fixture)],
            },
            None,
        );
    }
}

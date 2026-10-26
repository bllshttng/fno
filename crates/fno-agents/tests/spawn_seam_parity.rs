//! parity-stage: characterization
//! parity-oracle: fno.agents.spawn_defaults
//!
//! Characterization parity for `spawn_admission`: the scan projection the
//! compose transport sent, the argv normalization it ran first, and the three
//! pre-route seed refusals. Each `tests/golden/spawn_seam/*.json` case holds
//! what the Python leg answered, captured by `spawn_seam_golden_driver.py`.
//! The inputs below mirror that driver's case table.
//!
//! A minted name is a wall-clock draw, not parity: a mint case asserts the
//! name sits in the `--name` slot and reads `adjective-noun`, then compares
//! the rest of the argv. `cannot_fire` replays `plugin_status: None`, the
//! verdict the capture machine's codex read produced.

use fno_agents::spawn_admission::{
    cannot_fire_refusal, lost_command_refusal, normalize, scan_projection, seedless_thread_refusal,
};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/spawn_seam")
        .join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn argv(toks: &[&str]) -> Vec<String> {
    toks.iter().map(|t| t.to_string()).collect()
}

fn text(case: &Value) -> Option<String> {
    case.get("text").and_then(Value::as_str).map(str::to_string)
}

const SCAN_CASES: &[&str] = &[
    "scan_bare_message",
    "scan_named_with_flags",
    "scan_model_shorthand",
    "scan_model_behind_fence",
    "scan_pane_explicit",
    "scan_headless_once",
    "scan_cloud",
    "scan_role_lead",
    "scan_route_value",
    "scan_account_flag",
    "scan_dispatch_account_pane",
    "scan_resume",
    "scan_retired_provider",
    "scan_permission_yolo",
    "scan_node_flag",
    "scan_node_seed_id",
    "scan_argv_payload",
    "scan_explicit_thread",
];

#[test]
fn scan_projection_matches_the_python_transport() {
    for name in SCAN_CASES {
        let case = golden(name);
        let toks: Vec<String> = case["argv"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: argv"))
            .iter()
            .map(|t| t.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(scan_projection(&toks[1..]), case["scan"], "{name}");
    }
}

#[test]
fn normalize_matches_the_python_leg() {
    let existing: HashSet<String> = ["w1", "taken-name"].iter().map(|s| s.to_string()).collect();
    let cwd = std::env::temp_dir();
    let cases: &[(&str, &[&str])] = &[
        ("normalize_mints_name", &["spawn", "check the failing test"]),
        ("normalize_keeps_name", &["spawn", "--name", "w1", "hi"]),
        (
            "normalize_fence_shorthand",
            &["spawn", "-m", "glm-5.2", "hi"],
        ),
        ("normalize_second_positional", &["spawn", "one", "two"]),
        (
            "normalize_payload_untouched",
            &["spawn", "hi", "--", "codex", "-m", "x"],
        ),
    ];
    for (name, input) in cases {
        let case = golden(name);
        let got = normalize(&argv(input), Some(&existing), &cwd);
        // print() writes its newline as a second write; the notes carry none.
        let want_stderr: Vec<String> = case["stderr"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .filter(|l| *l != "\n")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(got.stderr, want_stderr, "{name}");
        if let Some(code) = case.get("exit").and_then(Value::as_i64) {
            assert_eq!(got.exit, Some(code as i32), "{name}");
            continue;
        }
        assert_eq!(got.exit, None, "{name}");
        let want: Vec<String> = case["argv"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: argv"))
            .iter()
            .map(|t| t.as_str().unwrap_or_default().to_string())
            .collect();
        let named = input.iter().any(|t| *t == "--name");
        if named {
            assert_eq!(got.argv, want, "{name}");
            continue;
        }
        assert_eq!(got.argv.len(), want.len(), "{name}: {:?}", got.argv);
        assert_eq!(got.argv[1], "--name", "{name}");
        let minted = &got.argv[2];
        let words: Vec<&str> = minted.split('-').collect();
        assert_eq!(words.len(), 2, "{name}: {minted}");
        assert!(!existing.contains(minted), "{name}: {minted}");
        let mut rest = got.argv.clone();
        rest.remove(2);
        let mut want_rest = want.clone();
        want_rest.remove(2);
        assert_eq!(rest, want_rest, "{name}");
    }
}

#[test]
fn seed_refusals_match_the_python_texts() {
    type Seedless<'a> = (
        &'a str,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
    );
    let seedless: &[Seedless] = &[
        ("refusal_seedless_thread", "thread", None, None, None),
        ("refusal_seedless_pane_no_seed", "pane", None, None, None),
        (
            "refusal_seedless_resume",
            "thread",
            Some("abcd1234"),
            None,
            None,
        ),
        (
            "refusal_seedless_named",
            "bg",
            None,
            Some("w1"),
            Some("x-9a6b"),
        ),
    ];
    for (name, substrate, resume, worker, node) in seedless {
        let got = seedless_thread_refusal(
            Some("claude"),
            Some(*substrate),
            None,
            *resume,
            false,
            *worker,
            *node,
        );
        assert_eq!(got, text(&golden(name)), "{name}");
    }

    let lost: &[(&str, &str)] = &[
        ("refusal_lost_command", "$fno:target scratch"),
        ("refusal_lost_plain_seed", "plain text seed"),
        ("refusal_lost_bash_eaten", ":target x-9a6b"),
        ("refusal_lost_zsh_eaten", "arget x-9a6b"),
    ];
    for (name, seed) in lost {
        assert_eq!(lost_command_refusal(seed), text(&golden(name)), "{name}");
    }

    for (name, harness) in [
        ("refusal_cannot_fire_codex", "codex"),
        ("refusal_cannot_fire_claude", "claude"),
    ] {
        assert_eq!(
            cannot_fire_refusal("/fno:review medium", harness, None),
            text(&golden(name)),
            "{name}"
        );
    }
    // The measured-absent verdicts refuse; the text names the seed's command.
    let refused =
        cannot_fire_refusal("/fno:review medium", "codex", Some("missing")).unwrap_or_default();
    assert!(
        refused.starts_with("the seed invokes '/fno:review' but"),
        "{refused}"
    );
}

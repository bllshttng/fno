//! parity-stage: characterization
//! parity-oracle: fno.config.schema_gen.render_markdown
//!
//! Characterization for `fno config schema` and `fno config setup plan`, now
//! rendered natively from crates/fno/config_schema.json. The goldens under
//! tests/golden/config_schema/ were captured from the Python commands before
//! their renderers were deleted (step 2 of the port protocol,
//! docs/architecture/dual-implementation-inventory.md). The `setup plan`
//! goldens are the captured wizard plan filtered by tier, exactly as the
//! Python leaf printed `json.dumps(json.loads(wizard_plan()), indent=2)`.
//!
//! The repo root in a `--check` receipt folds to `<ROOT>`. Every other byte
//! compares as captured.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn golden(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/config_schema")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_default()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let root = repo();
    let out = Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(args)
        .current_dir(&root)
        .output()
        .expect("fno runs");
    let fold = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes).replace(&root.display().to_string(), "<ROOT>")
    };
    (
        out.status.code().unwrap_or(-1),
        fold(&out.stdout),
        fold(&out.stderr),
    )
}

#[test]
fn config_schema_matches_the_python_goldens() {
    let cases: [(&str, &[&str], i32); 8] = [
        ("json_schema", &["config", "schema", "--json-schema"], 0),
        ("wizard_plan", &["config", "schema", "--wizard-plan"], 0),
        ("markdown", &["config", "schema", "--markdown"], 0),
        ("toml", &["config", "schema", "--toml"], 0),
        (
            "markdown_check",
            &["config", "schema", "--markdown", "--check"],
            0,
        ),
        ("toml_check", &["config", "schema", "--toml", "--check"], 0),
        (
            "two_modes",
            &["config", "schema", "--markdown", "--toml"],
            2,
        ),
        ("setup_plan", &["config", "setup", "plan"], 0),
    ];
    for (name, args, exit) in cases {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, exit, "[{name}] exit; stderr: {stderr}");
        assert!(
            stdout == golden(&format!("{name}.stdout")),
            "[{name}] stdout differs from its golden"
        );
        let want_err = golden(&format!("{name}.stderr"));
        if !want_err.is_empty() {
            assert_eq!(stderr, want_err, "[{name}] stderr");
        }
    }

    // `--advanced` and the moved `fno setup plan` spelling.
    let (code, stdout, _) = run(&["config", "setup", "plan", "--advanced"]);
    assert_eq!(code, 0);
    assert!(
        stdout == golden("setup_plan_advanced.stdout"),
        "--advanced stdout differs from its golden"
    );
    let (code, stdout, stderr) = run(&["setup", "plan"]);
    assert_eq!(code, 0);
    assert!(
        stdout == golden("setup_plan.stdout"),
        "moved spelling stdout differs from its golden"
    );
    assert!(
        stderr.contains("fno setup is now fno config setup"),
        "moved spelling names its home: {stderr}"
    );
}

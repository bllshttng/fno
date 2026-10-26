//! parity-stage: characterization
//! parity-oracle: fno.executor._surface
//!
//! Characterization harness for the frontend-surface matcher door. The
//! goldens (tests/golden/executor_surface/<case>.{exit,out,err}) were
//! captured from the Python leg (`fno.executor._surface`) before its family
//! deleted; each case pipes the same fixture the capture driver built and
//! drives the real `fno-agents executor-route surface` door, so the frozen
//! Python verdict is the contract the native matcher answers.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// One frozen case: the golden slug, the mode flag, the stdin fixture the
/// capture driver piped. `input` bytes must match the capture exactly.
struct Case {
    label: &'static str,
    flag: &'static str,
    input: &'static str,
}

#[test]
fn the_surface_door_matches_the_captured_leg_on_every_golden() {
    let cases = [
        Case { label: "tsx-path", flag: "", input: "src/components/Foo.tsx\n" },
        Case { label: "jsx-path", flag: "", input: "app/panel.jsx\n" },
        Case { label: "components-prefix", flag: "", input: "components/Sidebar.tsx\n" },
        Case { label: "components-mid", flag: "", input: "src/features/dashboard/components/Chart.tsx\n" },
        Case { label: "routes-mid", flag: "", input: "web/routes/api.ts\n" },
        Case { label: "src-styles-mid", flag: "", input: "ui/src/styles/theme.css\n" },
        Case { label: "plain-py", flag: "", input: "cli/src/fno/loop.py\n" },
        Case { label: "empty-stdin", flag: "", input: "" },
        Case { label: "blank-lines", flag: "", input: "\n\n" },
        Case { label: "multi-file-mix", flag: "", input: "cli/src/fno/loop.py\nsrc/components/Foo.tsx\n" },
        Case { label: "multi-file-backend-only", flag: "", input: "cli/src/fno/loop.py\ncrates/fno-agents/src/lib.rs\n" },
        Case { label: "has-ui-true", flag: "--has-ui", input: "src/routes/settings.tsx\n" },
        Case { label: "has-ui-false", flag: "--has-ui", input: "README.md\n" },
        Case { label: "has-ui-empty", flag: "--has-ui", input: "" },
        Case { label: "trailing-no-newline", flag: "", input: "src/components/Foo.tsx" },
        Case { label: "bare-tsx-suffix", flag: "", input: "panel.tsx\n" },
    ];
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/executor_surface");
    for case in &cases {
        let expected_exit = std::fs::read_to_string(dir.join(format!("{}.exit", case.label)))
            .unwrap_or_else(|e| panic!("executor_surface/{}: golden exit missing: {e}", case.label))
            .trim()
            .parse::<i32>()
            .expect("golden exit parses");
        let expected_out = std::fs::read_to_string(dir.join(format!("{}.out", case.label)))
            .unwrap_or_else(|e| panic!("executor_surface/{}: golden out missing: {e}", case.label));
        let expected_err = std::fs::read_to_string(dir.join(format!("{}.err", case.label)))
            .unwrap_or_else(|e| panic!("executor_surface/{}: golden err missing: {e}", case.label));
        let (code, stdout, stderr) = run_door(case.flag, case.input);
        assert_eq!(code, expected_exit, "executor_surface/{}: exit", case.label);
        assert_eq!(stdout, expected_out, "executor_surface/{}: stdout", case.label);
        assert_eq!(stderr, expected_err, "executor_surface/{}: stderr", case.label);
    }
}

fn run_door(flag: &str, input: &str) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["executor-route", "surface"])
        .args(flag.split_whitespace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(fno_agents::test_run::self_owner_env())
        .spawn()
        .expect("run fno-agents executor-route surface");
    use std::io::Write as _;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("pipe stdin");
    let out = child.wait_with_output().expect("collect door output");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

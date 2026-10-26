//! parity-stage: characterization
//! parity-oracle: fno.doctor
//!
//! The goldens under tests/golden/doctor/ were frozen from the Python legs
//! (cli/src/fno/doctor.py + doctor_cli.py + bundle/, all deleted in the same
//! change) under a pinned empty machine: HOME and cwd pointed at throwaway
//! dirs, PATH stripped to /usr/bin:/usr/sbin, FNO_REPO_ROOT set only where a
//! case names it, CODEX/SHELL/plugin-root env unset, and the binary run from
//! a directory with no co-installed siblings. A golden can only be re-frozen
//! by reverting the deletion, never from this binary.
//!
//! The runner copies the freshly built front door into an isolated dir per
//! case, so the sibling-of-binary lookup finds no fno-agents (the capture
//! machine had none) and the empty-machine shape stays byte-stable.

use std::process::Command;

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/doctor");

struct Case {
    slug: &'static str,
    args: &'static [&'static str],
    /// The relative stderr stream is compared only when the golden froze one.
    err: bool,
    /// The bundle + plugin-file cases resolve their repo root from
    /// FNO_REPO_ROOT (an empty pinned dir); the empty-machine cases unset it.
    repo_root: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            slug: "empty_machine_text",
            args: &["doctor"],
            err: false,
            repo_root: false,
        },
        Case {
            slug: "empty_machine_json",
            args: &["doctor", "--json"],
            err: true,
            repo_root: false,
        },
        Case {
            slug: "empty_machine_blockers",
            args: &["doctor", "--blockers"],
            err: false,
            repo_root: false,
        },
        Case {
            slug: "plugin_file_unknown_no_source",
            args: &["doctor", "plugin-file", "/nonexistent/skills/foo/SKILL.md"],
            err: false,
            repo_root: true,
        },
        Case {
            slug: "bundle_build_refusal",
            args: &["doctor", "bundle", "build"],
            err: true,
            repo_root: true,
        },
        Case {
            slug: "bundle_check_refusal",
            args: &["doctor", "bundle", "check"],
            err: true,
            repo_root: true,
        },
        Case {
            slug: "bundle_lint_refusal",
            args: &["doctor", "bundle", "lint"],
            err: true,
            repo_root: true,
        },
    ]
}

/// The masks the freeze applied; the test applies the same set to its own
/// output so machine-varying numbers fold identically on both sides. The
/// token pairs are substituted in order: canonicalized paths first (a
/// canonical macOS temp path contains the raw form as a substring), then
/// the raw forms.
fn mask(text: &str, paths: &[(String, &str)]) -> String {
    let patterns: [(&str, &str); 6] = [
        (r#""free_gb": [0-9.]+"#, "\"free_gb\": <FREE>"),
        (r#""soft": [0-9]+"#, "\"soft\": <FD>"),
        (
            r#""hard": (?:\"?unlimited\"?|[0-9]+)"#,
            "\"hard\": <FDHARD>",
        ),
        (
            r#""launchd_soft": (?:null|[0-9]+)"#,
            "\"launchd_soft\": <LAUNCHD>",
        ),
        (
            r#""kern_maxfiles": (?:null|[0-9]+)"#,
            "\"kern_maxfiles\": <KERN>",
        ),
        (r#"("git_version": \{"version": ")[^"]*"#, "$1<GITVER>"),
    ];
    let mut out = text.to_string();
    for (pattern, replacement) in patterns {
        let re = regex::Regex::new(pattern).expect("valid pattern");
        out = re.replace_all(&out, replacement).into_owned();
    }
    for (needle, token) in paths {
        out = out.replace(needle.as_str(), token);
    }
    out
}

fn run_case(case: &Case) -> (Option<i32>, String, String) {
    let tmp = tempfile::TempDir::new().expect("tmp dir");
    let case_dir = tmp.path().join("case");
    let home = tmp.path().join("home");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    let exe = env!("CARGO_BIN_EXE_fno");
    let copied = bin_dir.join("fno");
    std::fs::copy(exe, &copied).unwrap();

    let mut cmd = Command::new(&copied);
    cmd.args(case.args)
        .current_dir(&case_dir)
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/usr/sbin")
        .env("CARGO_HOME", tmp.path().join("cargo-home"));
    for var in [
        "CODEX_HOME",
        "OPENCODE_CONFIG_DIR",
        "FNO_CONFIG",
        "FNO_STATE_DIR",
        "FNO_REPO_ROOT",
        "FNO_AGENTS_BIN",
        "FNO_AGENTS_FRONT",
        "CLAUDE_PLUGIN_ROOT",
        "PLUGIN_ROOT",
        "SHELL",
        "FNO_EVENTS_PATH",
    ] {
        cmd.env_remove(var);
    }
    // The repo-root cases pin FNO_REPO_ROOT after the strip, matching the
    // capture environment; the empty-machine cases run with it unset.
    if case.repo_root {
        cmd.env("FNO_REPO_ROOT", &case_dir);
    }
    let out = cmd.output().expect("fno binary runs");
    let code = out.status.code();
    let canon = |p: &std::path::Path| -> String {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned()
    };
    let paths = [
        (canon(&case_dir), "<CASE>"),
        (canon(&home), "<PINHOME>"),
        (case_dir.to_string_lossy().into_owned(), "<CASE>"),
        (home.to_string_lossy().into_owned(), "<PINHOME>"),
    ];
    (
        code,
        mask(&String::from_utf8_lossy(&out.stdout), &paths),
        mask(&String::from_utf8_lossy(&out.stderr), &paths),
    )
}

fn read_golden(slug: &str, suffix: &str) -> Option<String> {
    std::fs::read_to_string(format!("{GOLDEN_DIR}/{slug}.{suffix}")).ok()
}

#[test]
fn doctor_matches_the_python_goldens() {
    for case in cases() {
        let (code, out, err) = run_case(&case);
        let expected_exit: i32 = read_golden(case.slug, "exit")
            .unwrap_or_else(|| panic!("missing golden {}.exit", case.slug))
            .trim()
            .parse()
            .unwrap();
        assert_eq!(code, Some(expected_exit), "case {}", case.slug);
        assert_eq!(
            out,
            read_golden(case.slug, "out")
                .unwrap_or_else(|| panic!("missing golden {}.out", case.slug)),
            "case {} stdout",
            case.slug
        );
        let golden_err = read_golden(case.slug, "err");
        if case.err {
            assert_eq!(
                err,
                golden_err.unwrap_or_else(|| panic!("missing golden {}.err", case.slug)),
                "case {} stderr",
                case.slug
            );
        } else {
            assert!(
                golden_err.is_none(),
                "case {} has an unfrozen .err",
                case.slug
            );
        }
    }
}

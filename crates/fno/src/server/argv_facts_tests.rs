#![cfg(test)]

//! `argv_facts` tests: the wrapper-token provenance reads.

use super::*;

#[test]
fn wrapper_anchored_scans_stop_at_the_command() {
    // env(1) wrapper prefix: `env FNO_AGENT_SELF=... FNO_NODE=x-66e8 ... claude`.
    let argv: Vec<String> = [
        "env",
        "FNO_AGENT_SELF=peer",
        "FNO_NODE=x-66e8",
        "FNO_SLUG=some-slug",
        "claude",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(node_from_argv(&argv), Some("x-66e8".to_string()));
    let ad_hoc = |a: &[&str]| node_from_argv(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // A plain `pane run htop` (no wrapper) has no provenance.
    assert_eq!(ad_hoc(&["htop"]), None);
    // An empty-valued token is treated as absent (no empty-string exports).
    assert_eq!(ad_hoc(&["env", "FNO_NODE=", "sh"]), None);
    // A command that merely MENTIONS FNO_NODE= in its own args is not
    // provenance: scanning stops at the command (first non-`NAME=` token).
    assert_eq!(
        ad_hoc(&["env", "FOO=1", "grep", "FNO_NODE=x", "file"]),
        None
    );
    // No `env` wrapper at all -> never scanned, even with a bare token.
    assert_eq!(ad_hoc(&["grep", "FNO_NODE=x", "file"]), None);
    // The same anchoring governs argv_runs_claude: bare and env-wrapped
    // claude match; a QoS wrapper re-anchors the command after its `--`.
    let runs = |a: &[&str]| argv_runs_claude(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert!(runs(&["claude"]));
    assert!(runs(&["env", "FNO_AGENT_SELF=w", "claude"]));
    assert!(runs(&[
        "env",
        "FNO_AGENT_SELF=w",
        "/usr/sbin/taskpolicy",
        "-c",
        "utility",
        "--",
        "claude"
    ]));
    // An argument naming claude is never the command.
    assert!(!runs(&["man", "claude"]));
    assert!(!runs(&["bash", "-c", "claude --print"]));
    assert!(!runs(&["env", "A=b", "taskpolicy", "--", "codex"]));
    // No command at all.
    assert!(!runs(&["env", "A=b"]));
}
#[test]
fn claude_theme_rides_right_after_the_command_and_skips_attach() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let themed = |a: &[&str]| {
        let mut argv = v(a);
        with_claude_theme(&mut argv, "custom:fno:footnote-paper");
        argv
    };
    let flag = ["--settings", "{\"theme\":\"custom:fno:footnote-paper\"}"];
    // After the command token, so a `--` seed stays the prompt positional.
    assert_eq!(
        themed(&["env", "A=b", "claude", "--", "seed"]),
        v(&["env", "A=b", "claude", flag[0], flag[1], "--", "seed"])
    );
    assert_eq!(
        themed(&[
            "taskpolicy",
            "-c",
            "utility",
            "--",
            "claude",
            "--resume",
            "x"
        ]),
        v(&[
            "taskpolicy",
            "-c",
            "utility",
            "--",
            "claude",
            flag[0],
            flag[1],
            "--resume",
            "x"
        ])
    );
    // attach ignores the flag; a named settings file and non-claude stay untouched.
    for same in [
        &["claude", "attach", "deadbee1"][..],
        &["claude", "--settings", "/s.json"][..],
        &["claude", "--settings=/s.json"][..],
        &["codex", "resume"][..],
    ] {
        assert_eq!(themed(same), v(same));
    }
}

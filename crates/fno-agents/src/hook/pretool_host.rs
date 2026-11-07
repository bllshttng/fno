//! `fno-agents hook pretool` - the PreToolUse guard host for the agy and pi
//! wires.
//!
//! One entry runs every claude PreToolUse BLOCK guard for one tool call: it
//! translates the wire payload to the claude shape
//! ([`super::adapter::agy::to_claude_pretool`] /
//! [`super::adapter::pi::to_claude_pretool`]), walks
//! `<plugin-root>/hooks/hooks.json` PreToolUse groups in file order, runs
//! each command whose script carries a `claude PreToolUse <script> block`
//! row in `hooks/hook-classes.tsv` (the wait-class carriers and the
//! claude-only report scripts never run here), and relays the FIRST deny in
//! the wire's decision shape. hooks.json stays the only guard list: a guard
//! added there reaches agy and pi with no second list in code.
//!
//! Each command runs through `bash -c` with `CLAUDE_PLUGIN_ROOT=<root>`,
//! `FNO_HOOK_CONTEXT=1`, `FNO_HOOK_HARNESS=<wire>`, the payload's cwd when it
//! names a directory, and the claude payload on stdin, under the group's
//! `timeout` (seconds, default 10). A deny is stdout JSON with
//! `hookSpecificOutput.permissionDecision == "deny"` or `decision ==
//! "block"`, or exit 2 with stderr as the reason. The first deny wins. A
//! command that cannot answer denies when its tsv fail column is `closed`
//! (write-gate's contract) and is skipped with a stderr line when `open`.
//! The host itself always exits 0 after printing: the wire shims keep their
//! own fail-open tails, so a stale binary that lacks this entry fails the
//! shim's binary loop and the tool proceeds.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitStatus;

/// Entry: parse the flags, run the host, print the decision, always exit 0
/// (2 only on a usage fault, so a stale binary falls out of the shim's
/// binary loop to the fail-open tail).
pub fn run(args: &[String]) -> i32 {
    let mut wire = String::new();
    let mut root = PathBuf::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wire" => wire = it.next().cloned().unwrap_or_default(),
            "--plugin-root" => root = PathBuf::from(it.next().cloned().unwrap_or_default()),
            other => {
                eprintln!(
                    "fno-agents hook pretool: unknown flag {other:?}; usage: hook pretool --wire agy|pi --plugin-root <dir>"
                );
                return 2;
            }
        }
    }
    if wire != "agy" && wire != "pi" {
        eprintln!("fno-agents hook pretool: --wire must be agy or pi");
        return 2;
    }
    let stdin = super::read_stdin();
    let (code, out) = host_main(&wire, &root, &stdin);
    println!("{out}");
    code
}

/// The host body, split from `run` so tests read the decision text without
/// capturing stdout. Any stderr line is printed as it happens.
fn host_main(wire: &str, root: &PathBuf, stdin: &str) -> (i32, String) {
    if !root.join("hooks/hooks.json").is_file() {
        // No hooks.json to host: nothing can run, the tool proceeds. The
        // same shape the pi extension returns (nothing) on a missing root.
        eprintln!(
            "pretool host: no hooks.json under {}; allowing",
            root.display()
        );
        return (0, allow_shape(wire));
    }
    let raw: Value = serde_json::from_str(stdin.trim()).unwrap_or(Value::Null);
    let payload = match wire {
        "agy" => super::adapter::agy::to_claude_pretool(raw),
        _ => super::adapter::pi::to_claude_pretool(raw),
    };
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if payload.is_null() || tool.is_empty() {
        // An unreadable payload is not a refusal (the hooks' allow-on-empty
        // contract, mod.rs read_stdin).
        return (0, allow_shape(wire));
    }
    let classes = BlockClasses::load(&root.join("hooks/hook-classes.tsv"));
    let groups = serde_json::from_str::<Value>(
        &std::fs::read_to_string(root.join("hooks/hooks.json")).unwrap_or_default(),
    )
    .ok()
    .and_then(|doc| doc.pointer("/hooks/PreToolUse").cloned())
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    let claude_json = payload.to_string();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    for group in &groups {
        if !matcher_matches(group.get("matcher"), tool) {
            continue;
        }
        for hook in group
            .get("hooks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let command = hook.get("command").and_then(Value::as_str).unwrap_or("");
            if command.is_empty() {
                continue;
            }
            let Some(script) = script_name(command) else {
                continue;
            };
            // The class authority gates here, not inside the guard: a
            // wait-class carrier or a claude-only report script never runs
            // on another harness.
            let Some(fail_closed) = classes.block_fail(script) else {
                continue;
            };
            let secs = hook
                .get("timeout")
                .and_then(Value::as_u64)
                .filter(|s| *s > 0)
                .unwrap_or(10);
            let mut cmd = std::process::Command::new("bash");
            cmd.arg("-c").arg(command);
            cmd.env("CLAUDE_PLUGIN_ROOT", &root.as_os_str());
            cmd.env("FNO_HOOK_CONTEXT", "1");
            cmd.env("PYTHONDONTWRITEBYTECODE", "1");
            cmd.env("FNO_HOOK_HARNESS", wire);
            if cwd.as_ref().is_some_and(|c| c.is_dir()) {
                cmd.current_dir(cwd.as_ref().unwrap());
            }
            match crate::bounded_cmd::output_with_timeout_stdin(cmd, secs, &claude_json) {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if let Some(reason) = denied_reason(&stdout, &out.status, &stderr) {
                        return (0, deny_shape(wire, &reason));
                    }
                    if !out.status.success() {
                        // Errored or killed past its bound. A `closed` guard
                        // that cannot answer denies (its claude shim fails
                        // closed the same way); an `open` one is skipped,
                        // named on stderr.
                        if fail_closed {
                            return (
                                0,
                                deny_shape(
                                    wire,
                                    &format!("{script} could not answer; refusing (fail closed)"),
                                ),
                            );
                        }
                        eprintln!("pretool host: block guard {script} failed; skipped (fail open)");
                    }
                }
                Err(e) => {
                    if fail_closed {
                        return (
                            0,
                            deny_shape(
                                wire,
                                &format!("{script} could not run: {e}; refusing (fail closed)"),
                            ),
                        );
                    }
                    eprintln!(
                        "pretool host: block guard {script} failed ({e}); skipped (fail open)"
                    );
                }
            }
        }
    }
    (0, allow_shape(wire))
}

/// The wire's allow shape: agy reads `{"decision":"allow"}`, pi `{}`.
fn allow_shape(wire: &str) -> String {
    if wire == "agy" {
        "{\"decision\":\"allow\"}".to_string()
    } else {
        "{}".to_string()
    }
}

/// The wire's deny shape with the guard's own reason.
fn deny_shape(wire: &str, reason: &str) -> String {
    if wire == "agy" {
        json!({"decision": "deny", "reason": reason}).to_string()
    } else {
        json!({"block": true, "reason": reason}).to_string()
    }
}

/// One claude PreToolUse block row: script -> fail-closed?
struct BlockClasses {
    block: BTreeMap<String, bool>,
}

impl BlockClasses {
    fn load(path: &std::path::Path) -> Self {
        let mut block = BTreeMap::new();
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() >= 5
                && cols[0] == "claude"
                && cols[1] == "PreToolUse"
                && cols[3] == "block"
            {
                block.insert(cols[2].to_string(), cols[4] == "closed");
            }
        }
        Self { block }
    }

    /// The script's fail-closed flag, `None` when it has no claude
    /// PreToolUse `block` row (not a guard this host runs).
    fn block_fail(&self, script: &str) -> Option<bool> {
        self.block.get(script).copied()
    }
}

/// Claude matcher semantics for this list: an anchored regex on the claude
/// tool name; a missing or empty matcher matches everything. An invalid
/// pattern selects nothing (opencode's host skips it the same way).
fn matcher_matches(matcher: Option<&Value>, tool: &str) -> bool {
    let Some(pat) = matcher.and_then(Value::as_str) else {
        return true;
    };
    if pat.is_empty() {
        return true;
    }
    regex::Regex::new(&format!("^(?:{pat})$"))
        .map(|re| re.is_match(tool))
        .unwrap_or(false)
}

/// The `.sh` basename a hooks.json command runs: the span after `/hooks/` up
/// to the first quote or whitespace. The shipped command shape is
/// `ENV=1 bash "${CLAUDE_PLUGIN_ROOT}/hooks/<script>.sh" [args]`.
fn script_name(command: &str) -> Option<&str> {
    let idx = command.find("/hooks/")?;
    let rest = &command[idx + "/hooks/".len()..];
    let end = rest
        .find(|c: char| c == '"' || c.is_whitespace())
        .unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty()).then_some(name)
}

/// The deny a guard's answer carries: stdout JSON with
/// `hookSpecificOutput.permissionDecision == "deny"` (reason from
/// `permissionDecisionReason` or `reason`), or `decision == "block"`, or
/// exit 2 with stderr as the reason. Non-JSON stdout at exit 0 allows, the
/// same read claude and the opencode host make.
fn denied_reason(stdout: &str, status: &ExitStatus, stderr: &str) -> Option<String> {
    if let Some(reason) = stdout_deny(stdout) {
        return Some(reason);
    }
    if status.code() == Some(2) {
        let reason = stderr.trim();
        return Some(if reason.is_empty() {
            "denied by fno hook".to_string()
        } else {
            reason.to_string()
        });
    }
    None
}

fn stdout_deny(stdout: &str) -> Option<String> {
    let t = stdout.trim();
    if t.is_empty() || t == "{}" {
        return None;
    }
    let v: Value = serde_json::from_str(t).ok()?;
    let permission = v
        .get("hookSpecificOutput")
        .and_then(|h| h.get("permissionDecision"))
        .and_then(Value::as_str);
    if permission == Some("deny") {
        return Some(
            v.get("hookSpecificOutput")
                .and_then(|h| h.get("permissionDecisionReason"))
                .and_then(Value::as_str)
                .or_else(|| v.get("reason").and_then(Value::as_str))
                .unwrap_or("denied by fno hook")
                .unwrap_or("denied by fno hook")
                .to_string(),
        );
    }
    if v.get("decision").and_then(Value::as_str) == Some("block") {
        return Some(
            v.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("denied by fno hook")
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    /// One fixture plugin root: hooks.json with the six guard groups under
    /// the claude matchers verbatim, one wait-class probe that must never
    /// run, stub guard scripts whose deny reason names the script and the
    /// harness env, and the tsv block rows. Returns (root, tsv path).
    fn fixture_root(tag: &str, fail_for_write_gate: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("fx-pretool-{tag}-{}", std::process::id()));
        let hooks = root.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        let script = |name: &str, body: &str| {
            let path = hooks.join(name);
            fs::write(&path, body).unwrap();
        };
        let deny_stub = |name: &str| {
            script(
                name,
                &format!(
                    "#!/usr/bin/env bash\necho \"{{\\\"decision\\\":\\\"block\\\",\\\"reason\\\":\\\"stub-guard:{name}:harness=$FNO_HOOK_HARNESS:cwd=$PWD\\\"}}\"\n"
                ),
            );
        };
        deny_stub("write-gate.sh");
        deny_stub("worktree-write-protect.sh");
        deny_stub("join-partition-write-guard.sh");
        deny_stub("plan-location-guard.sh");
        deny_stub("lead-delegation-guard.sh");
        deny_stub("rules-dispatch.sh");
        script(
            "session-state.sh",
            "#!/usr/bin/env bash\necho '{\"decision\":\"block\",\"reason\":\"stub-guard:session-state.sh:RAN\"}'\n",
        );
        let doc = json!({
            "hooks": {"PreToolUse": [
                {"matcher": "Edit|Write|Bash", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/write-gate.sh\""}]},
                {"matcher": "Edit|Write", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/worktree-write-protect.sh\""}]},
                {"matcher": "Edit|Write", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/join-partition-write-guard.sh\""}]},
                {"matcher": "Write", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/plan-location-guard.sh\""}]},
                {"matcher": "Edit|Write|NotebookEdit|Bash", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/lead-delegation-guard.sh\""}]},
                {"matcher": "Edit|Write|Bash|Skill", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/rules-dispatch.sh\" pre-tool", "timeout": 4}]},
                {"matcher": "", "hooks": [{"type": "command",
                    "command": "PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/session-state.sh\" claude PreToolUse"}]},
            ]}
        });
        fs::write(
            hooks.join("hooks.json"),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        let mut tsv = String::from(
            "# harness\tevent\tscript\tclass\tfail\nclaude\tPreToolUse\twrite-gate.sh\tblock\t",
        );
        tsv.push_str(fail_for_write_gate);
        tsv.push_str("\n");
        for name in [
            "worktree-write-protect.sh",
            "join-partition-write-guard.sh",
            "plan-location-guard.sh",
            "lead-delegation-guard.sh",
            "rules-dispatch.sh",
        ] {
            tsv.push_str(&format!("claude\tPreToolUse\t{name}\tblock\topen\n"));
        }
        // The wait-class probe: same matcher-empty group the real list has,
        // never a guard this host runs.
        tsv.push_str("claude\tPreToolUse\tsession-state.sh\twait\t-\n");
        fs::write(hooks.join("hook-classes.tsv"), tsv).unwrap();
        root
    }

    /// Drive the host body directly: no stdout capture, the decision comes
    /// back as the string.
    fn drive(wire: &str, root: &PathBuf, payload: Value) -> String {
        let (_code, out) = host_main(wire, root, &payload.to_string());
        out
    }

    /// The 12 (wire, guard) pairs the done-when names, one deny fixture row
    /// each: the wire payload selects the guard through its claude matcher
    /// and the host relays the deny in the wire's shape. The write-gate row
    /// also proves first-deny ordering: a Bash payload matches the
    /// write-gate, lead-delegation and rules-dispatch groups, all stub
    /// deny, and the answer must be the FIRST group's reason.
    #[test]
    fn every_guard_pair_gates_on_both_wires() {
        // (wire, script, wire tool name, wire args JSON) - the agy rows carry
        // the measured agy tool names, the pi rows pi's lowercase names; both
        // exercise the translators. One row per line: the done probe counts
        // these pair rows textually, and rustfmt keeps a short array
        // literal's rows intact.
        let rows: [(&str, &str, &str, &str); 12] = [
            (
                "agy",
                "write-gate.sh",
                "run_command",
                r#"{"CommandLine":"ls"}"#,
            ),
            (
                "agy",
                "worktree-write-protect.sh",
                "file_change",
                r#"{"TargetFile":"a.rs"}"#,
            ),
            (
                "agy",
                "join-partition-write-guard.sh",
                "file_change",
                r#"{"AbsPath":"a.rs"}"#,
            ),
            (
                "agy",
                "plan-location-guard.sh",
                "write_blob",
                r#"{"Path":"a.md"}"#,
            ),
            (
                "agy",
                "lead-delegation-guard.sh",
                "edit_notebook",
                r#"{"TargetFile":"a.ipynb"}"#,
            ),
            ("agy", "rules-dispatch.sh", "Skill", r#"{}"#),
            ("pi", "write-gate.sh", "bash", r#"{"command":"ls"}"#),
            (
                "pi",
                "worktree-write-protect.sh",
                "edit",
                r#"{"path":"a.rs"}"#,
            ),
            (
                "pi",
                "join-partition-write-guard.sh",
                "edit",
                r#"{"path":"a.rs"}"#,
            ),
            (
                "pi",
                "plan-location-guard.sh",
                "write",
                r#"{"path":"a.md"}"#,
            ),
            (
                "pi",
                "lead-delegation-guard.sh",
                "NotebookEdit",
                r#"{"path":"a.ipynb"}"#,
            ),
            ("pi", "rules-dispatch.sh", "Skill", r#"{}"#),
        ];
        let root = fixture_root("pairs", "closed");
        for (wire, script, tool, args) in &rows {
            let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
            let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
            let payload = if *wire == "agy" {
                json!({"toolCall": {"name": tool, "args": args}, "workspacePaths": ["/w"], "conversationId": "s1"})
            } else {
                json!({"toolName": tool, "input": args, "cwd": "/w", "session_id": "s1"})
            };
            let out = drive(wire, &root, payload);
            let expect = format!("stub-guard:{script}:harness={wire}");
            assert!(
                out.contains(&expect),
                "{wire} {script}: expected {expect:?} in {out}"
            );
            if *wire == "agy" {
                assert!(out.contains("\"decision\":\"deny\""), "{wire}: {out}");
            } else {
                assert!(out.contains("\"block\":true"), "{wire}: {out}");
            }
        }
        // The wait-class probe never ran: its group matches every payload.
        let bash = drive(
            "agy",
            &root,
            json!({"toolCall": {"name": "run_command", "args": {"CommandLine": "ls"}}, "workspacePaths": ["/w"]}),
        );
        assert!(
            !bash.contains("session-state.sh:RAN"),
            "wait-class carrier ran on the agy wire: {bash}"
        );
        // The cwd the host hands the guard is the payload's cwd: the stub
        // reasons carry $PWD.
        let edit = drive(
            "pi",
            &root,
            json!({"toolName": "edit", "input": {"path": "a.rs"}, "cwd": root.display().to_string(), "session_id": "s1"}),
        );
        assert!(
            edit.contains(&format!("cwd={}", root.display())),
            "guard cwd is not the payload cwd: {edit}"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// AC2-EDGE: a `closed` guard that cannot answer denies; an `open` one
    /// is skipped and named on stderr. The stubs sleep past their one
    /// second group timeout, so the host kills them (nonzero, no exit 2).
    #[test]
    fn a_timed_out_closed_guard_denies_and_an_open_one_is_skipped() {
        let build = |tag: &str, script: &str, fail: &str| {
            let root =
                std::env::temp_dir().join(format!("fx-pretool-{tag}-{}", std::process::id()));
            let hooks = root.join("hooks");
            fs::create_dir_all(&hooks).unwrap();
            fs::write(hooks.join("guard.sh"), "#!/usr/bin/env bash\nsleep 5\n").unwrap();
            let doc = json!({
                "hooks": {"PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command",
                        "command": format!("PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"{}\"", hooks.join(script).display()), "timeout": 1}]},
                ]}
            });
            fs::write(
                hooks.join("hooks.json"),
                serde_json::to_string_pretty(&doc).unwrap(),
            )
            .unwrap();
            fs::write(
                hooks.join("hook-classes.tsv"),
                format!("claude\tPreToolUse\t{script}\tblock\t{fail}\n"),
            )
            .unwrap();
            root
        };
        // Fail closed: the killed guard's chain refuses.
        let closed = build("closed", "guard.sh", "closed");
        let out = drive(
            "pi",
            &closed,
            json!({"toolName": "bash", "input": {"command": "ls"}, "cwd": "/w"}),
        );
        assert!(
            out.contains("\"block\":true"),
            "closed timeout must deny: {out}"
        );
        assert!(out.contains("could not answer"), "{out}");
        fs::remove_dir_all(&closed).ok();
        // Fail open: the killed guard is skipped and named on stderr; the
        // chain allows.
        let open = build("open", "guard.sh", "open");
        let out = drive(
            "pi",
            &open,
            json!({"toolName": "bash", "input": {"command": "ls"}, "cwd": "/w"}),
        );
        assert_eq!(out, "{}", "open timeout must allow: {out}");
        fs::remove_dir_all(&open).ok();
    }

    /// A degenerate read allows in the wire's own allow shape: an
    /// unparseable payload (an unreadable payload is not a refusal) and a
    /// root with no hooks.json (AC6 shape at the host layer; the shim tail
    /// and the pi extension degrade around it).
    #[test]
    fn a_degenerate_root_or_payload_allows() {
        let root = fixture_root("unparseable", "closed");
        let (_code, out) = host_main("agy", &root, "not json at all");
        assert_eq!(out, "{\"decision\":\"allow\"}");
        let (_code, out) = host_main("pi", &root, "");
        assert_eq!(out, "{}");
        fs::remove_dir_all(&root).ok();
        let dir = std::env::temp_dir().join(format!("fx-pretool-empty-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (_code, out) = host_main("agy", &dir, "{}");
        assert_eq!(out, "{\"decision\":\"allow\"}");
        fs::remove_dir_all(&dir).ok();
    }

    /// The matcher reads as claude reads it: anchored on the tool name,
    /// empty or absent matches everything, an invalid pattern selects
    /// nothing.
    #[test]
    fn matchers_read_claude_semantics() {
        let m = |pat: Value| matcher_matches(Some(&pat), "mcp__github__x");
        assert!(m(json!("mcp__.*")));
        assert!(m(json!("mcp__github__x|mcp__other")));
        assert!(!m(json!("Write")));
        assert!(!m(json!("mcp__other")));
        assert!(matcher_matches(Some(&json!("")), "Anything"));
        assert!(matcher_matches(None, "Anything"));
        assert!(!matcher_matches(
            Some(&json!("(unclosed")),
            "mcp__github__x"
        ));
    }

    /// The script basename extraction reads the shipped command shapes:
    /// quoted claude-root path, with and without trailing arguments.
    #[test]
    fn script_names_read_the_shipped_command_shapes() {
        assert_eq!(
            script_name("PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/write-gate.sh\""),
            Some("write-gate.sh")
        );
        assert_eq!(
            script_name("PYTHONDONTWRITEBYTECODE=1 FNO_HOOK_CONTEXT=1 bash \"${CLAUDE_PLUGIN_ROOT}/hooks/rules-dispatch.sh\" pre-tool"),
            Some("rules-dispatch.sh")
        );
        assert_eq!(
            script_name("bash \"${CLAUDE_PLUGIN_ROOT}/hooks/session-state.sh\" claude PreToolUse"),
            Some("session-state.sh")
        );
        assert_eq!(script_name("echo no script here"), None);
    }
}

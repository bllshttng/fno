//! The plugin-hook launch probe (doctor.py:3850-4200).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::super::resolve_source;

fn manifest_specs() -> Vec<(String, String, String)> {
    let mut specs: Vec<(String, String, String)> = Vec::new();
    specs.push((
        "hooks.json".to_string(),
        "hooks/hooks.json".to_string(),
        "CLAUDE_PLUGIN_ROOT".to_string(),
    ));
    specs.push((
        "codex-hooks.json".to_string(),
        "hooks/codex-hooks.json".to_string(),
        "PLUGIN_ROOT".to_string(),
    ));
    specs
}

fn find_plugin_root_with_hooks() -> Option<PathBuf> {
    for var in ["CLAUDE_PLUGIN_ROOT", "PLUGIN_ROOT"] {
        if let Some(val) = std::env::var_os(var) {
            let root = PathBuf::from(val);
            if root.join("hooks/hooks.json").is_file() {
                return Some(root);
            }
        }
    }
    let src = resolve_source(None)?;
    for parent in std::iter::successors(Some(src), |p| p.parent().map(|x| x.to_path_buf())) {
        if parent.join("hooks/hooks.json").is_file() {
            return Some(parent);
        }
    }
    None
}

fn probe_root_value(root_var: &str, manifest_root: &PathBuf) -> String {
    if let Some(val) = std::env::var_os(root_var) {
        return val.to_string_lossy().into_owned();
    }
    manifest_root.to_string_lossy().into_owned()
}

pub(crate) fn plugin_hooks_launch_report() -> Value {
    let shell = std::env::var_os("SHELL");
    let shell = match shell {
        Some(s) => s.to_string_lossy().into_owned(),
        None => return not_applicable("$SHELL unset"),
    };
    let Some(root) = find_plugin_root_with_hooks() else {
        return not_applicable("no plugin root with hooks/ resolved");
    };
    let root_display = root.to_string_lossy().into_owned();
    let mut launches: Vec<Value> = Vec::new();
    let mut missing: Vec<Value> = Vec::new();
    let mut manifest_errors: Vec<Value> = Vec::new();
    for (name, rel, root_var) in manifest_specs() {
        let path = root.join(&rel);
        let root_value = probe_root_value(&root_var, &root);
        if !path.is_file() {
            manifest_errors.push(json!({
                "manifest": name, "issue": "missing",
                "path": path.to_string_lossy(),
            }));
            continue;
        }
        let data = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => v,
                Err(e) => {
                    manifest_errors.push(json!({
                        "manifest": name,
                        "issue": format!("unparseable: {e}").chars().take(200).collect::<String>(),
                        "path": path.to_string_lossy(),
                    }));
                    continue;
                }
            },
            Err(e) => {
                manifest_errors.push(json!({
                    "manifest": name,
                    "issue": format!("unparseable: {e}").chars().take(200).collect::<String>(),
                    "path": path.to_string_lossy(),
                }));
                continue;
            }
        };
        let hooks_ok = data.get("hooks").map(|h| h.is_object()).unwrap_or(false);
        if !hooks_ok {
            manifest_errors.push(json!({
                "manifest": name,
                "issue": "malformed: top-level 'hooks' object missing or wrong type",
                "path": path.to_string_lossy(),
            }));
            continue;
        }
        let bad = bad_event_groups(&data);
        if !bad.is_empty() {
            manifest_errors.push(json!({
                "manifest": name,
                "issue": format!("malformed: event group(s) not arrays: {}", bad.join(", ")),
                "path": path.to_string_lossy(),
            }));
            continue;
        }
        for (event, command) in manifest_event_commands(&data) {
            for script in referenced_hook_scripts(&command, &root) {
                if !script.is_file() {
                    missing.push(json!({
                        "manifest": name, "event": event,
                        "script": script.to_string_lossy(),
                    }));
                }
            }
            if event == "PreToolUse" {
                let mut result = launch_one_hook(&command, &root_value, &shell, &root_var);
                result["manifest"] = json!(name);
                result["event"] = json!(event);
                launches.push(result);
            } else {
                for problem in command_launch_problems(&command, &root) {
                    missing.push(json!({
                        "manifest": name, "event": event, "script": problem,
                    }));
                }
            }
        }
    }
    let failures: Vec<Value> = launches
        .iter()
        .filter(|r| r.get("rc").map(|rc| rc != &json!(0)).unwrap_or(true))
        .cloned()
        .collect();
    json!({
        "applicable": true,
        "shell": shell,
        "manifest_root": root_display,
        "launched": launches.len(),
        "failed": failures.len(),
        "failures": failures,
        "missing_scripts": missing,
        "manifest_errors": manifest_errors,
    })
}

fn not_applicable(reason: &str) -> Value {
    json!({"applicable": false, "reason": reason})
}

fn bad_event_groups(data: &Value) -> Vec<String> {
    let mut bad: Vec<String> = Vec::new();
    if let Some(hooks) = data.get("hooks").and_then(Value::as_object) {
        for (ev, grp) in hooks {
            if ev != "state" && !grp.is_array() {
                bad.push(ev.clone());
            }
        }
    }
    bad.truncate(3);
    bad
}

fn manifest_event_commands(data: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let Some(hooks) = data.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for (event, groups) in hooks {
        if event == "state" {
            continue;
        }
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let Some(entries) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for hook in entries {
                if let Some(cmd) = hook.get("command").and_then(Value::as_str) {
                    if !cmd.is_empty() {
                        out.push((event.clone(), cmd.to_string()));
                    }
                }
            }
        }
    }
    out
}

fn resolve_for_display(command: &str, root_value: &str) -> String {
    command
        .replace("${PLUGIN_ROOT}", root_value)
        .replace("${CLAUDE_PLUGIN_ROOT}", root_value)
}

fn referenced_hook_scripts(command: &str, root: &std::path::Path) -> Vec<PathBuf> {
    let root_str = root.to_string_lossy().into_owned();
    let expanded = resolve_for_display(command, &root_str);
    let re =
        regex::Regex::new(&format!(r"{}\S+", regex::escape(&root_str))).expect("valid pattern");
    re.find_iter(&expanded)
        .map(|m| {
            let tok = m.as_str().trim_end_matches(|c| c == ';' || c == '"');
            PathBuf::from(tok)
        })
        .collect()
}

fn command_launch_problems(command: &str, root: &std::path::Path) -> Vec<String> {
    let root_str = root.to_string_lossy().into_owned();
    let expanded = resolve_for_display(command, &root_str);
    let tokens = match shlex::split(&expanded) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut i = 0;
    if tokens.first().map(String::as_str) == Some("env") {
        i = 1;
        while i < tokens.len() && tokens[i].contains('=') && !tokens[i].starts_with('-') {
            i += 1;
        }
    }
    let Some(head) = tokens.get(i) else {
        return Vec::new();
    };
    if !head.contains('/') && HOOK_INTERPRETERS.contains(&head.as_str()) {
        if super::which(head).is_none() {
            return vec![format!("interpreter '{head}' not on PATH")];
        }
        return Vec::new();
    }
    let script = std::path::Path::new(head);
    if script.is_file() {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(script)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o755);
        if mode & 0o111 == 0 {
            return vec![format!("bare-invoked script not executable: {head}")];
        }
    }
    Vec::new()
}

const HOOK_INTERPRETERS: [&str; 5] = ["bash", "sh", "python3", "python", "node"];

const HOOK_PROBE_TIMEOUT_SECS: u64 = 8;

fn launch_one_hook(command: &str, root_value: &str, shell: &str, root_var: &str) -> Value {
    use std::io::Read as _;
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let resolved = resolve_for_display(command, root_value);
    let workdir = match tempfile::Builder::new().prefix("fno-hook-probe.").tempdir() {
        Ok(d) => d,
        Err(e) => return launch_error(command, &resolved, &format!("temp creation failed: {e}")),
    };
    let payload = json!({
        "session_id": "fno-doctor",
        "cwd": workdir.path().to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": {"command": "git status --short"},
    })
    .to_string();
    let mut cmd = Command::new(shell);
    cmd.arg("-lc").arg(command).current_dir(workdir.path());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only THIS manifest's root var (matching the real harness); drop the rest
    // so a wrong-placeholder command is not masked. fno + claude-project state
    // sandbox to the temp cwd; HOME stays real so interpreters resolve.
    cmd.env_remove("PLUGIN_ROOT")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("PLUGIN_DATA")
        .env_remove("CLAUDE_PLUGIN_DATA")
        .env("FNO_HOME", workdir.path())
        .env("CLAUDE_PROJECT_DIR", workdir.path())
        .env(root_var, root_value);
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return launch_error(command, &resolved, &format!("launch failed: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(HOOK_PROBE_TIMEOUT_SECS);
    let rc;
    let mut stderr_text = String::new();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                rc = Some(status.code().unwrap_or(-1));
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr_text);
                }
                break;
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                rc = None;
                stderr_text = "timed out".to_string();
                break;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => {
                rc = None;
                break;
            }
        }
    }
    let trimmed = stderr_text.trim();
    let start = trimmed
        .char_indices()
        .rev()
        .nth(299)
        .map(|(i, _)| i)
        .unwrap_or(0);
    json!({
        "command": command,
        "resolved": resolved,
        "rc": rc.map(|c| json!(c)).unwrap_or(Value::Null),
        "stderr": &trimmed[start..],
    })
}

fn launch_error(command: &str, resolved: &str, message: &str) -> Value {
    json!({
        "command": command,
        "resolved": resolved,
        "rc": Value::Null,
        "stderr": message.chars().take(300).collect::<String>(),
    })
}

pub(crate) fn pre_push_hook_report() -> Value {
    let Some(src) = resolve_source(None) else {
        return json!({"status": "unchecked"});
    };
    let script = src.join("scripts/install-pre-push-hook.sh");
    if !script.is_file() {
        return json!({"status": "unchecked"});
    }
    let argv = vec![
        "bash".to_string(),
        script.to_string_lossy().into_owned(),
        "--check".to_string(),
    ];
    match super::super::bounded_command(&argv) {
        Some((_code, stdout, stderr)) => {
            let out = format!("{stdout}{stderr}");
            if out.contains("legacy") {
                json!({"status": "legacy"})
            } else {
                json!({"status": "ok"})
            }
        }
        None => json!({"status": "unchecked"}),
    }
}

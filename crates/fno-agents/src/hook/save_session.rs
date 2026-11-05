//! PreCompact / SessionEnd: save a readable transcript with session metadata
//! under the `saved-sessions` state folder, as
//! `{session}-{YYYYmmdd-HHMMSS}-{trigger}.md`. Model, cost and context come
//! from Claude Code's statusline sidecar (`~/.claude/.session-context.json`)
//! when it is fresh. Always exits 0: a hook that saves nothing never blocks.

use serde_json::Value;
use std::path::{Path, PathBuf};

const SKIP_TYPES: [&str; 2] = ["progress", "file-history-snapshot"];
const NOISE: [&str; 4] = [
    "Base directory for this skill:",
    "<task-notification>",
    "<command-message>",
    "<system-reminder>",
];
/// A sidecar older than this belongs to another session's last draw.
const SIDECAR_FRESH_SECS: u64 = 300;
/// A second save of one session inside this window is the other trigger of
/// the same exit (PreCompact then SessionEnd), not new content.
const DEDUP_SECS: u64 = 60;

pub fn run(_args: &[String]) -> i32 {
    let input: Value = match serde_json::from_str(super::read_stdin().trim()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("save-session: invalid stdin JSON: {e}");
            return 0;
        }
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let fno_home = std::env::var_os("FNO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".fno"));
    let dir = crate::state_layout::place_dir(&fno_home, "saved-sessions")
        .unwrap_or_else(|| fno_home.join("db/saved-sessions"));
    save(&input, &dir, &home.join(".claude/.session-context.json"));
    0
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(t)) => !t.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn num(v: Option<&Value>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "0".into())
}

fn is_noise(text: &str) -> bool {
    let t = text.trim();
    NOISE.iter().any(|p| t.starts_with(p))
}

/// The readable text of one message: a user turn carrying a tool result or a
/// harness injection is not a user turn at all.
fn extract_text(message: &Value, user: bool) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => {
            (!text.trim().is_empty() && !(user && is_noise(text))).then(|| text.clone())
        }
        Some(Value::Array(blocks)) => {
            let mut parts = Vec::new();
            for block in blocks {
                match s(block, "type") {
                    "tool_result" => return None,
                    "text" => {
                        let text = s(block, "text");
                        if user && is_noise(text) {
                            return None;
                        }
                        if !text.trim().is_empty() {
                            parts.push(text.to_string());
                        }
                    }
                    "thinking" => {
                        let thinking = s(block, "thinking");
                        if !thinking.trim().is_empty() {
                            parts.push(format!("<thinking>\n{thinking}\n</thinking>"));
                        }
                    }
                    _ => {}
                }
            }
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

type Round = (Option<String>, String);

pub(crate) fn parse_rounds(transcript: &str) -> Vec<Round> {
    let mut rounds = Vec::new();
    let mut user: Option<String> = None;
    let mut assistant: Vec<String> = Vec::new();
    let mut flush = |user: &mut Option<String>, assistant: &mut Vec<String>| {
        if !assistant.is_empty() {
            rounds.push((user.take(), assistant.join("\n")));
        }
        *user = None;
        assistant.clear();
    };
    for line in transcript.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = s(&entry, "type");
        if SKIP_TYPES.contains(&kind) || truthy(entry.get("isCompactSummary")) {
            continue;
        }
        if kind == "system" {
            if matches!(s(&entry, "subtype"), "turn_duration" | "stop_hook_summary") {
                flush(&mut user, &mut assistant);
            }
            continue;
        }
        let message = entry.get("message").cloned().unwrap_or(Value::Null);
        match s(&message, "role") {
            "user" => {
                if let Some(text) = extract_text(&message, true) {
                    if !assistant.is_empty() {
                        flush(&mut user, &mut assistant);
                    }
                    user = Some(text);
                }
            }
            "assistant" => assistant.extend(extract_text(&message, false)),
            _ => {}
        }
    }
    flush(&mut user, &mut assistant);
    rounds
}

/// A fence never legally crosses a role heading, so an odd fence count in a
/// message is closed there rather than inverting the rest of the file.
fn balanced(text: &str, lines: &mut Vec<String>) {
    lines.push(text.to_string());
    if text
        .lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count()
        % 2
        == 1
    {
        lines.push("```".into());
    }
}

pub(crate) fn format_transcript(rounds: &[Round], frontmatter: &str) -> String {
    let mut lines = vec![frontmatter.to_string()];
    for (user, assistant) in rounds {
        lines.extend(["---".into(), String::new()]);
        if let Some(user) = user {
            lines.push("## User".into());
            balanced(user, &mut lines);
            lines.push(String::new());
        }
        lines.push("## Assistant".into());
        balanced(assistant, &mut lines);
        lines.push(String::new());
    }
    lines.join("\n")
}

fn first_model(transcript: &str) -> String {
    transcript
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(|e| e.pointer("/message/role").and_then(Value::as_str) == Some("assistant"))
        .map(|e| {
            e.pointer("/message/model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default()
}

fn git_branch(cwd: &str) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd);
    crate::bounded_cmd::output_with_timeout(cmd, 5)
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn frontmatter(
    session: &str,
    cwd: &str,
    trigger: &str,
    transcript: &str,
    sidecar: &Value,
) -> String {
    let empty = Value::Null;
    let obj = |key: &str| sidecar.get(key).unwrap_or(&empty);
    let mut model = s(obj("model"), "id").to_string();
    if model.is_empty() {
        model = first_model(transcript);
    }
    let project = s(obj("workspace"), "project_dir");
    let worktree = obj("worktree");
    let mut branch = s(worktree, "branch").to_string();
    if branch.is_empty() && !cwd.is_empty() {
        branch = git_branch(cwd);
    }
    let now = chrono::Local::now()
        .naive_local()
        .format("%Y-%m-%dT%H:%M:%S%.6f");
    let mut p = vec![
        "---".to_string(),
        format!("sessionId: {session}"),
        format!("timestamp: {now}"),
        format!("trigger: {trigger}"),
    ];
    if !cwd.is_empty() {
        p.push(format!("cwd: {cwd}"));
    }
    if !project.is_empty() && project != cwd {
        p.push(format!("projectDir: {project}"));
    }
    if !model.is_empty() {
        p.push(format!("model: {model}"));
    }
    if !branch.is_empty() {
        p.push(format!("gitBranch: {branch}"));
    }
    if !s(sidecar, "version").is_empty() {
        p.push(format!("version: \"{}\"", s(sidecar, "version")));
    }
    if truthy(worktree.get("name")) {
        p.extend([
            "worktree:".into(),
            format!("  name: {}", s(worktree, "name")),
        ]);
        if truthy(worktree.get("path")) {
            p.push(format!("  path: {}", s(worktree, "path")));
        }
        if truthy(worktree.get("original_branch")) {
            p.push(format!(
                "  originalBranch: {}",
                s(worktree, "original_branch")
            ));
        }
    }
    let cost = obj("cost");
    if truthy(cost.get("total_cost_usd")) {
        p.extend([
            "cost:".into(),
            format!("  usd: {}", num(cost.get("total_cost_usd"))),
        ]);
        if let Some(ms) = cost
            .get("total_duration_ms")
            .and_then(Value::as_f64)
            .filter(|ms| *ms != 0.0)
        {
            p.push(format!(
                "  durationSeconds: {}",
                (ms / 1000.0).round() as i64
            ));
        }
        for (key, label) in [
            ("total_lines_added", "linesAdded"),
            ("total_lines_removed", "linesRemoved"),
        ] {
            if truthy(cost.get(key)) {
                p.push(format!("  {label}: {}", num(cost.get(key))));
            }
        }
    }
    let ctx = obj("context_window");
    if truthy(ctx.get("used_percentage")) {
        p.extend([
            "contextWindow:".into(),
            format!("  usedPercent: {}", num(ctx.get("used_percentage"))),
            format!("  totalInputTokens: {}", num(ctx.get("total_input_tokens"))),
            format!(
                "  totalOutputTokens: {}",
                num(ctx.get("total_output_tokens"))
            ),
        ]);
        if let Some(cu) = ctx.get("current_usage").filter(|v| truthy(Some(v))) {
            p.push("  currentUsage:".into());
            for (key, label) in [
                ("input_tokens", "inputTokens"),
                ("output_tokens", "outputTokens"),
                ("cache_creation_input_tokens", "cacheCreation"),
                ("cache_read_input_tokens", "cacheRead"),
            ] {
                p.push(format!("    {label}: {}", num(cu.get(key))));
            }
        }
    }
    p.extend(["---".into(), String::new()]);
    p.join("\n")
}

fn age_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

fn read_sidecar(path: &Path) -> Value {
    if age_secs(path).is_none_or(|age| age >= SIDECAR_FRESH_SECS) {
        return Value::Null;
    }
    match std::fs::read_to_string(path).map(|t| serde_json::from_str(&t)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            eprintln!("save-session: sidecar read failed: {e}");
            Value::Null
        }
        Err(e) => {
            eprintln!("save-session: sidecar read failed: {e}");
            Value::Null
        }
    }
}

pub(crate) fn save(input: &Value, dir: &Path, sidecar: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let transcript_path = s(input, "transcript_path");
    let session = s(input, "session_id");
    let cwd = s(input, "cwd");
    let trigger = [s(input, "trigger"), s(input, "reason")]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or("unknown");
    if transcript_path.is_empty() || !Path::new(transcript_path).is_file() {
        eprintln!("save-session: no transcript at {transcript_path:?}");
        return None;
    }
    let transcript = match std::fs::read(transcript_path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            eprintln!("save-session: failed to read transcript: {e}");
            return None;
        }
    };
    let rounds = parse_rounds(&transcript);
    if rounds.is_empty() {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    // Both are filename parts: keep only safe characters, so no input can
    // walk out of the folder.
    let prefix: String = session.chars().filter(|c| c.is_alphanumeric()).collect();
    let prefix = if prefix.is_empty() {
        "session".to_string()
    } else {
        prefix
    };
    let safe_trigger: String = trigger
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let safe_trigger = if safe_trigger.is_empty() {
        "unknown".to_string()
    } else {
        safe_trigger
    };
    let recent = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&format!("{prefix}-"))
                && name.ends_with(".md")
                && age_secs(&e.path()).is_some_and(|a| a < DEDUP_SECS)
        });
    if recent {
        return None;
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let out = dir.join(format!("{prefix}-{stamp}-{safe_trigger}.md"));
    let content = format_transcript(
        &rounds,
        &frontmatter(session, cwd, trigger, &transcript, &read_sidecar(sidecar)),
    );
    match std::fs::write(&out, content) {
        Ok(()) => {
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600));
            eprintln!("Session saved: {}", out.display());
            Some(out)
        }
        Err(e) => {
            eprintln!("Failed to save session: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn save_writes_once_per_window_with_safe_names() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("t.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({"message": {"role": "user", "content": "hi"}}),
                json!({"message": {"role": "assistant", "content": "yo"}})
            ),
        )
        .unwrap();
        let input =
            json!({"transcript_path": transcript, "session_id": "../ab-12", "trigger": "auto/x"});
        let out_dir = dir.path().join("saved");
        let saved = save(&input, &out_dir, &dir.path().join("none.json")).unwrap();
        let name = saved.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("ab12-") && name.ends_with("-autox.md"),
            "{name}"
        );
        let text = std::fs::read_to_string(&saved).unwrap();
        assert!(
            text.starts_with("---\nsessionId: ../ab-12\n")
                && text.contains("## User\nhi\n\n## Assistant\nyo\n"),
            "{text}"
        );
        assert!(
            save(&input, &out_dir, &dir.path().join("none.json")).is_none(),
            "dedup window"
        );
    }
}

//! `fno-agents hook rules --event prompt|skill|pre-tool`: the hook decision
//! points. Each fire builds one `crate::rules::Event` from the payload, asks
//! `rules_source::rule`, and prints the harness shape: a PreToolUse deny for
//! block, `additionalContext` for warn and inject, `{}` for allow. A fire
//! never fails the tool on its own error: a garbage payload allows, and an
//! unreadable source rides the context as an `Unread:` line.

use crate::rules::{Decision, Event};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn run(event: &str) -> i32 {
    let payload = crate::hook::read_stdin();
    let Ok(hook) = serde_json::from_str::<Value>(&payload) else {
        return crate::hook::emit_allow();
    };
    let cwd = hook
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let Some(ev) = event_of(event, &hook, &cwd) else {
        return crate::hook::emit_allow();
    };
    let mut ruling = crate::rules_source::rule(&cwd, &ev);
    if ev.kind == "pre-tool" {
        drop_seen(&mut ruling, ev.session.as_deref());
    }
    let hook_event = hook
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match render(&ev, &ruling.decision, &ruling.unread, &hook_event) {
        Rendered::Block(reason) => crate::hook::emit_block(&reason),
        Rendered::Context(out) => {
            println!("{out}");
            0
        }
        Rendered::Allow => crate::hook::emit_allow(),
    }
}

/// The event a payload describes, or `None` for a payload with nothing to
/// decide (an empty prompt, a pre-tool call with no tool name).
fn event_of(kind: &str, hook: &Value, cwd: &Path) -> Option<Event> {
    // One PreToolUse registration covers Skill too: a Skill call is the
    // skill event.
    let kind = match hook.get("tool_name").and_then(Value::as_str) {
        Some("Skill") if kind == "pre-tool" => "skill",
        _ => kind,
    };
    let input = hook.get("tool_input");
    let read = |k: &str| input.and_then(|i| i.get(k)).and_then(Value::as_str);
    let mut ev = Event {
        kind: kind.to_string(),
        session: hook
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        harness: Some(
            if std::env::var_os("CODEX_THREAD_ID").is_some() {
                "codex"
            } else {
                "claude"
            }
            .to_string(),
        ),
        ..Event::default()
    };
    match kind {
        "prompt" => {
            let prompt = hook.get("prompt").and_then(Value::as_str)?;
            let head = prompt.split_whitespace().next().unwrap_or("");
            if let Some(skill) = head.strip_prefix('/').or_else(|| head.strip_prefix('$')) {
                ev.skill = Some(skill.to_string());
            }
            ev.text = Some(prompt.to_string());
        }
        "skill" => {
            let name = read("skill")
                .or_else(|| read("name"))
                .or_else(|| read("command"))?;
            let name = name.trim_start_matches(['/', '$']);
            ev.skill = Some(name.to_string());
            ev.text = Some(format!("{name} {}", read("args").unwrap_or("")));
        }
        "pre-tool" => {
            ev.tool = Some(hook.get("tool_name").and_then(Value::as_str)?.to_string());
            ev.command = read("command").map(str::to_owned);
            ev.paths = crate::hook::write_gate::write_targets(
                read("file_path").unwrap_or(""),
                read("command").unwrap_or(""),
            )
            .into_iter()
            .map(|p| repo_relative(&p, cwd))
            .collect();
        }
        _ => return None,
    }
    ev.node = ev
        .text
        .as_deref()
        .and_then(crate::rules_source::node_in_text);
    ev.project =
        crate::law_match::resolve_project(Some(cwd), &crate::law_match::settings_sources()).ok();
    Some(ev)
}

/// A rule's path globs are repo-relative; an absolute target under the cwd's
/// repo is cut to its relative form, any other path rides as written.
fn repo_relative(path: &str, cwd: &Path) -> String {
    let mut root = cwd.to_path_buf();
    while !root.join(".git").exists() {
        if !root.pop() {
            return path.to_string();
        }
    }
    Path::new(path)
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// An inform fire or an unread source at a pre-tool event prints once per
/// session: it is the same on every tool call, and repeating it is noise.
/// Warn and block always print.
fn drop_seen(ruling: &mut crate::rules_source::Ruling, session: Option<&str>) {
    let Some(session) = session else { return };
    let root = crate::decision_index::default_state_path("rules-seen")
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut seen = crate::announce::load_cursor(&root, "rules-seen", session);
    let before = seen.len();
    let decision = &mut ruling.decision;
    decision
        .fires
        .retain(|f| f.action != "inform" || seen.insert(f.rule.clone()));
    ruling
        .unread
        .retain(|reason| seen.insert(format!("unread:{reason}")));
    if seen.len() != before {
        crate::announce::save_cursor(&root, "rules-seen", session, &seen);
    }
    if decision.verdict == "inject" && decision.fires.iter().all(|f| f.deliver != "context") {
        decision.verdict = "allow";
    }
}

enum Rendered {
    Block(String),
    Context(Value),
    Allow,
}

fn render(ev: &Event, decision: &Decision, unread: &[String], hook_event: &str) -> Rendered {
    if decision.verdict == "block" {
        return Rendered::Block(format!("Refused by rule: {}", decision.reasons.join("; ")));
    }
    let lines: Vec<String> = decision
        .fires
        .iter()
        .filter(|f| f.deliver == "context")
        .map(|f| {
            let mut line = format!("- {} ({}): {}", f.rule, f.action, f.text);
            if !f.instead.is_empty() {
                line.push_str(&format!(" Instead: {}", f.instead));
            }
            line
        })
        .collect();
    if lines.is_empty() && unread.is_empty() {
        return Rendered::Allow;
    }
    let mut text = format!("## Rules governing this {}\n\n", ev.kind);
    for line in &lines {
        text.push_str(line);
        text.push('\n');
    }
    for reason in unread {
        text.push_str(&format!("Unread: {reason}\n"));
    }
    Rendered::Context(json!({
        "hookSpecificOutput": {
            "hookEventName": hook_event,
            "additionalContext": text,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warn_renders_context_and_a_block_renders_the_deny() {
        let rule = |action: &str| {
            crate::rules::parse_file(&format!(
                "[[rule]]\nid = \"local-build-freeze\"\naction = \"{action}\"\nwhy = \"local cargo builds are frozen\"\n[rule.when]\nevents = [\"pre-tool\"]\ntools = [\"Bash\"]\ncommands = [\"cargo build*\"]\n"
            ))
            .unwrap()
            .rule
        };
        let ev = Event {
            kind: "pre-tool".into(),
            tool: Some("Bash".into()),
            command: Some("cargo build -p fno".into()),
            ..Event::default()
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let warn = crate::rules::decide(&rule("warn"), &ev, today);
        let Rendered::Context(out) = render(&ev, &warn, &[], "PreToolUse") else {
            panic!("a warn renders context");
        };
        let text = out["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(text.contains("local-build-freeze"), "{text}");
        let block = crate::rules::decide(&rule("block"), &ev, today);
        let Rendered::Block(reason) = render(&ev, &block, &[], "PreToolUse") else {
            panic!("an uncapped block denies");
        };
        assert!(reason.contains("local-build-freeze"), "{reason}");
        assert!(matches!(
            render(
                &ev,
                &crate::rules::decide(&[], &ev, today),
                &[],
                "PreToolUse"
            ),
            Rendered::Allow
        ));
        let unread = render(
            &ev,
            &crate::rules::decide(&[], &ev, today),
            &["the rule set (x)".into()],
            "",
        );
        assert!(
            matches!(unread, Rendered::Context(_)),
            "an unread source is said, never silent"
        );
    }

    #[test]
    fn a_payload_with_nothing_to_decide_has_no_event() {
        let cwd = Path::new("/");
        assert!(event_of("pre-tool", &json!({"tool_input": {}}), cwd).is_none());
        assert!(event_of("prompt", &json!({}), cwd).is_none());
        let ev = event_of("prompt", &json!({"prompt": "/fno:blueprint x-aaaa"}), cwd).unwrap();
        assert_eq!(ev.skill.as_deref(), Some("fno:blueprint"));
        assert_eq!(ev.node.as_deref(), Some("x-aaaa"));
    }
}

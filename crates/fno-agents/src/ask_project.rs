//! `ask --to-project`: which live peer an anycast ask reaches. The client
//! parses the project, and the peer resolves before any harness lane reads
//! the name.

use serde_json::{Map, Value};

/// The anycast flags on ask: `--to-project <project>` and `--any`.
pub fn take_flag(
    flag: &str,
    it: &mut impl Iterator<Item = String>,
    params: &mut Map<String, Value>,
) -> Result<(), String> {
    if flag == "--any" {
        params.insert("any".into(), Value::Bool(true));
    } else {
        let project = it
            .next()
            .filter(|p| !p.is_empty())
            .ok_or("--to-project needs a project")?;
        params.insert("to_project".into(), Value::String(project));
    }
    Ok(())
}

/// The ask arm of the request parse. A project names no peer, so every
/// positional is the message; otherwise the first positional is the name.
pub fn parse(params: &mut Map<String, Value>, positional: &[String]) -> Result<(), String> {
    if !params.contains_key("to_project") {
        if params.contains_key("any") {
            return Err("--any only breaks a --to-project tie".into());
        }
        let name = positional.first().ok_or("ask needs a <name>")?;
        params.insert("name".into(), Value::String(name.clone()));
        if !params.contains_key("message") && positional.len() > 1 {
            params.insert("message".into(), Value::String(positional[1..].join(" ")));
        }
    } else if !params.contains_key("message") {
        if positional.is_empty() {
            return Err("usage: ask --to-project <project> <message>".into());
        }
        params.insert("message".into(), Value::String(positional.join(" ")));
    }
    Ok(())
}

/// Swap the parsed project for its one live peer: 16 for no live peer, 17
/// for an untied many, 12 for a bad registry.
pub fn resolve(params: &mut Value) -> Result<(), (String, i32)> {
    let Some(obj) = params.as_object_mut() else {
        return Ok(());
    };
    let Some(project) = obj.remove("to_project") else {
        return Ok(());
    };
    let any = obj.remove("any").is_some();
    let project = project.as_str().unwrap_or_default().to_string();
    let name = crate::mail_cli::send::resolve_ask_recipient(&project, any)?;
    obj.insert("name".into(), Value::String(name));
    Ok(())
}

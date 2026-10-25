//! The dispatch-flag answers behind route-slot's dispatch modes: one JSON
//! payload in, one answer out.
//!
//! Ported from `fno.dispatch_flags` when the route family's Python leg
//! deleted; the standalone door action folded into `route-slot`, which owns
//! the transport (payload file or stdin) and routes the payload's `mode`
//! key here. The modes:
//!
//! - `resolve`   - `{explicit, env?, flag?}` -> `{harness, source}` or
//!   `{refusal}`. Precedence: explicit flag > invoking-harness inference >
//!   builtin `claude` (the decision_source vocabulary is a wire contract the
//!   spawn receipt carries; the values stay verbatim). Inference folds the
//!   ambient harness markers from the caller's env mapping the same way
//!   `fno.harness_identity.resolve_harness_identity` folds them: the
//!   canonical stamp leads, the vendor fold answers an absent stamp, and a
//!   contradiction or invalid stamp answers nothing rather than guessing.
//! - `configured` - `{command?, cwd?}` -> `{harness, note, route}`. The
//!   stage table (`agents.profiles.<command>.provider`) over the deprecated
//!   `dispatch.harness` key, with the shadow note naming the losing spelling
//!   when both were set and disagreed. Read-only and total: a missing block
//!   or field reads as unset, never fails.
//! - `validate-model` - `{model?, flag?}` -> `{model}` or `{refusal}`; an
//!   absent pin passes as null.

use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// The payload's answer; route-slot calls this for the `resolve`,
/// `configured`, and `validate-model` modes. A payload with no `mode` key
/// answers `resolve`, the ported default.
pub fn answer(payload: &Value) -> Value {
    match payload.get("mode").and_then(Value::as_str) {
        Some("configured") => configured_mode(payload),
        Some("validate-model") => validate_model_mode(payload),
        _ => resolve_mode(payload),
    }
}

fn resolve_mode(payload: &Value) -> Value {
    let flag = payload
        .get("flag")
        .and_then(Value::as_str)
        .unwrap_or("--harness");
    if let Some(explicit) = payload.get("explicit") {
        if !explicit.is_null() {
            let raw = explicit.as_str().unwrap_or_default().trim();
            if raw.is_empty() {
                return json!({"refusal": format!("{flag} must not be empty")});
            }
            return json!({"harness": raw, "source": "explicit"});
        }
    }
    let empty = Map::new();
    let env = payload
        .get("env")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if let Some(harness) = infer_from_env(env) {
        return json!({"harness": harness, "source": "harness-inferred"});
    }
    json!({"harness": "claude", "source": "builtin-default"})
}

/// The marker fold behind `fno.dispatch_flags.infer_invoking_harness`: the
/// canonical resolution's harness answers a canonical stamp; an absent stamp
/// answers the vendor family; an invalid or contradictory marker set answers
/// nothing (inference never guesses).
fn infer_from_env(env: &Map<String, Value>) -> Option<String> {
    let get = |key: &str| -> Option<String> {
        env.get(key)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let resolution = crate::spawn_context::resolve_canonical_resolution(&get);
    match resolution.class {
        crate::spawn_context::CanonicalClass::Canonical => resolution.harness,
        crate::spawn_context::CanonicalClass::Absent => {
            let (family, _session) = crate::spawn_context::vendor_identity(&resolution.markers);
            family
        }
        _ => None,
    }
}

/// The pin validator behind the deleted `fno.dispatch_flags.validate_dispatch_model`:
/// an absent pin passes as null, a whitespace-only pin refuses naming the flag,
/// and any other token passes verbatim.
fn validate_model_mode(payload: &Value) -> Value {
    match payload.get("model") {
        None | Some(Value::Null) => json!({"model": null}),
        Some(model) => {
            let raw = model.as_str().unwrap_or_default();
            if raw.trim().is_empty() {
                let flag = payload
                    .get("flag")
                    .and_then(Value::as_str)
                    .unwrap_or("--model");
                return json!({"refusal": format!("{flag} must not be empty")});
            }
            json!({"model": model})
        }
    }
}

fn configured_mode(payload: &Value) -> Value {
    let command = payload
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("target");
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let profile_key = canonical_profile_key(command);
    let profile = crate::agents_config::config_value_deep(
        &cwd,
        &["agents", "profiles", profile_key.as_str()],
    )
    .map(|v| serde_json::to_value(&v).expect("TOML serializes"));
    let stage = profile
        .as_ref()
        .and_then(|p| p.get("provider"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let route = profile
        .as_ref()
        .and_then(|p| p.get("route"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let legacy = crate::agents_config::config_value_deep(&cwd, &["dispatch"])
        .and_then(|d| serde_json::to_value(&d).ok())
        .and_then(|d| d.get("harness").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
        .trim()
        .to_string();
    if !stage.is_empty() {
        if !legacy.is_empty() && legacy != stage {
            return json!({
                "harness": stage,
                "note": format!(
                    "harness=deprecated dispatch.harness={legacy:?} ignored; \
                     agents.profiles.{profile_key}.provider={stage:?} is the home"
                ),
                "route": route,
            });
        }
        return json!({"harness": stage, "note": Value::Null, "route": route});
    }
    if !legacy.is_empty() {
        return json!({"harness": legacy, "note": Value::Null, "route": route});
    }
    json!({"harness": Value::Null, "note": Value::Null, "route": route})
}

/// `/fno:x`, `$fno:x`, `/x` and `$x` -> the bare profile key `x`; a body the
/// parser rejects keeps the legacy strip (mirror of
/// `fno.config._dispatch_commands.canonical_command_key` followed by the
/// stage profile's `lstrip("/")`).
fn canonical_profile_key(command: &str) -> String {
    let trimmed = command.trim();
    let body = trimmed
        .strip_prefix('/')
        .or_else(|| trimmed.strip_prefix('$'))
        .unwrap_or(trimmed);
    let body = body.strip_prefix("fno:").unwrap_or(body);
    let well_formed = !body.is_empty()
        && body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if well_formed {
        return body.to_string();
    }
    let k = trimmed.trim_start_matches('/');
    let k = k.strip_prefix("fno:").unwrap_or(k);
    k.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(explicit: Option<&str>, env: Value) -> Value {
        let mut payload = json!({});
        if let Some(e) = explicit {
            payload["explicit"] = json!(e);
        } else {
            payload["explicit"] = Value::Null;
        }
        payload["env"] = env;
        resolve_mode(&payload)
    }

    #[test]
    fn explicit_wins_and_empty_refuses() {
        assert_eq!(resolve(Some("codex"), json!({}))["source"], "explicit");
        let refused = resolve(Some("   "), json!({}));
        assert_eq!(
            refused["refusal"],
            "--harness must not be empty".to_string()
        );
    }

    #[test]
    fn inference_folds_markers_and_never_guesses() {
        let one = resolve(None, json!({"CODEX_THREAD_ID": "th_golden"}));
        assert_eq!(one["harness"], "codex");
        assert_eq!(one["source"], "harness-inferred");
        // CLAUDECODE alone is not a competing family; the thread id leads.
        let mixed = resolve(
            None,
            json!({"CODEX_THREAD_ID": "th_golden", "CLAUDECODE": "1"}),
        );
        assert_eq!(mixed["harness"], "codex");
        // Two families carrying ids ARE a contradiction: nothing answers.
        let bad = resolve(
            None,
            json!({
                "CODEX_THREAD_ID": "th_golden",
                "CLAUDE_CODE_SESSION_ID": "ses_golden"
            }),
        );
        assert_eq!(bad["harness"], "claude");
        assert_eq!(bad["source"], "builtin-default");
        assert_eq!(resolve(None, json!({}))["source"], "builtin-default");
    }

    #[test]
    fn configured_reads_the_stage_table_over_the_legacy_key() {
        let tmp =
            std::env::temp_dir().join(format!("dispatch-harness-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.toml"),
            "[dispatch]\nharness = \"codex\"\n\n[agents.profiles.target]\nprovider = \"opencode\"\nroute = \"zai/glm-5.3\"\n",
        )
        .unwrap();
        let payload =
            json!({"mode": "configured", "command": "target", "cwd": tmp.to_str().unwrap()});
        let _guard = crate::claims::test_env_lock();
        std::env::set_var("FNO_CONFIG", tmp.join("config.toml"));
        let answer = configured_mode(&payload);
        std::env::remove_var("FNO_CONFIG");
        assert_eq!(answer["harness"], "opencode");
        assert_eq!(answer["route"], "zai/glm-5.3");
        let note = answer["note"].as_str().unwrap();
        assert!(
            note.contains("dispatch.harness=\"codex\" ignored"),
            "{note}"
        );
        assert!(
            note.contains("agents.profiles.target.provider=\"opencode\" is the home"),
            "{note}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn profile_keys_strip_the_sigils() {
        assert_eq!(canonical_profile_key("target"), "target");
        assert_eq!(canonical_profile_key("/target"), "target");
        assert_eq!(canonical_profile_key("$fno:target"), "target");
    }
}

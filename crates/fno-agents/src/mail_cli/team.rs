//! The `team` front verb: broadcast one message to a scope of the fleet.
//!
//! Port of the Python `cmd_team` leaf: vet the body (forged envelope, brevity
//! cap, style), stamp the sender from the owned identity, then re-exec this
//! same binary as `announce send` and relay its streams verbatim. Three
//! sanctioned divergences, all quieter than the oracle: the announce writer is
//! `current_exe()` (the oracle PATH-looked-up `fno-agents` and refused when
//! absent), the style word cap is the built-in mail cap (the Rust leg reads no
//! `style.word_cap.mail` config), and an unresolvable self falls back to the
//! operator sender through the same never-exit path the oracle used.

use crate::backlog::style_check::{check, format_violations, has_exception, Violation};
use crate::mail_inject::{cap_env_int, enforce_body_cap};
use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition, OwnedIdentity};
use serde_json::json;
use std::io::Write as _;
use std::process::{Command, Stdio};

const USAGE: &str = "usage: fno agents mail team --scope <all|leads|<role>|project:<p>> <message>";

/// `team`: vet, stamp, and relay one broadcast through `announce send`.
pub fn run_team(args: &[String]) -> i32 {
    // Click parse walk under `allow_extra_args` + `ignore_unknown_options`:
    // known flags are consumed anywhere (space and `=` forms), any other
    // dash-prefixed token passes through without eating a value, the first
    // non-flag token is the message, and later ones pass through in encounter
    // order. `--` ends option parsing.
    let mut scope: Option<String> = None;
    let mut from_name: Option<String> = None;
    let mut json_out = false;
    let mut message: Option<String> = None;
    let mut passthrough: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            for rest in &args[i + 1..] {
                push_positional(&mut message, &mut passthrough, rest);
            }
            break;
        }
        if arg == "--scope" {
            match args.get(i + 1) {
                Some(v) => {
                    scope = Some(v.clone());
                    i += 2;
                }
                None => {
                    eprintln!("Error: Option '--scope' requires an argument.");
                    return 2;
                }
            }
            continue;
        }
        if let Some(v) = arg.strip_prefix("--scope=") {
            scope = Some(v.to_string());
            i += 1;
            continue;
        }
        if arg == "--from-name" {
            match args.get(i + 1) {
                Some(v) => {
                    from_name = Some(v.clone());
                    i += 2;
                }
                None => {
                    eprintln!("Error: Option '--from-name' requires an argument.");
                    return 2;
                }
            }
            continue;
        }
        if let Some(v) = arg.strip_prefix("--from-name=") {
            from_name = Some(v.to_string());
            i += 1;
            continue;
        }
        if arg == "--json" || arg == "-J" {
            json_out = true;
            i += 1;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            passthrough.push(arg.clone());
            i += 1;
            continue;
        }
        push_positional(&mut message, &mut passthrough, arg);
        i += 1;
    }

    // Parse-phase refusals precede the body checks, as click's did: the
    // missing-scope error is a parse error, the usage line a body check.
    let Some(scope) = scope else {
        eprintln!("Error: Missing option '--scope'.");
        return 2;
    };
    // `if not message:` in the oracle: an empty string is a missing message.
    let Some(body) = message.filter(|m| !m.is_empty()) else {
        eprintln!("{USAGE}");
        return 2;
    };

    // _refuse_forged_envelope: the oracle's contains_fno_mail_tag delegates
    // to the boundary-aware predicate the Rust classifier itself exposes as
    // mail_shape holds_tag, so <fno_mailicious lookalikes stay unframed.
    if crate::mail_header::text_holds_legacy_tag(&body) {
        eprintln!(
            "error: mail body contains an <fno_mail> tag. The envelope frames peer mail; \
             a body cannot contain one."
        );
        return 1;
    }
    // _enforce_body_cap(body, usage=False) over the shared knob pair.
    if let Some(code) = enforce_body_cap(
        body.len(),
        cap_env_int("FNO_MAIL_BODY_WARN", 3000),
        cap_env_int("FNO_MAIL_BODY_REFUSE", 5000),
    ) {
        return code;
    }
    // _enforce_style: the kill switch, an empty body, a style-exception
    // marker, and a `control:` command line all skip the gate. The word cap
    // is the built-in mail cap (see the module header).
    if std::env::var("FNO_STYLE_ENFORCE").ok().as_deref() != Some("0")
        && has_exception(&body).is_none()
        && !crate::mail_control_drain::is_control(&body)
    {
        let violations = check(&body, "mail", None);
        if !violations.is_empty() {
            emit_style_refusal(&violations);
            eprint!("{}", format_violations(&violations, "mail"));
            return 1;
        }
    }

    let (sender_kind, sender) = sender_stamp(from_name.as_deref());

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: mail team cannot resolve its own binary: {e}");
            return 1;
        }
    };
    let mut argv: Vec<String> = vec![
        "announce".into(),
        "send".into(),
        "--scope".into(),
        scope,
        "--from".into(),
        sender,
        "--sender-kind".into(),
        sender_kind.into(),
    ];
    argv.extend(passthrough);
    if json_out {
        argv.push("--json".into());
    }
    let mut child = match Command::new(exe)
        .args(argv.iter().map(String::as_str))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: mail team announce spawn failed: {e}");
            return 1;
        }
    };
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let _ = stdin.write_all(body.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) => {
            let _ = std::io::stdout().write_all(&out.stdout);
            let _ = std::io::stderr().write_all(&out.stderr);
            match out.status.code() {
                Some(0) => 0,
                Some(code) if code > 0 => code,
                _ => 1,
            }
        }
        Err(e) => {
            eprintln!("error: mail team announce failed: {e}");
            1
        }
    }
}

/// First non-flag token is the message; later ones pass through.
fn push_positional(message: &mut Option<String>, passthrough: &mut Vec<String>, token: &str) {
    if message.is_none() {
        *message = Some(token.to_string());
    } else {
        passthrough.push(token.to_string());
    }
}

/// `_team_sender_kind_and_from`: the kind is `agent` only for an owned self
/// (a proven, non-ambiguous identity carrying both a session id and a
/// harness); anything unproven stamps `operator`. An explicit `--from-name`
/// wins verbatim as the sender - the canonical handle when none was given,
/// `fno` when the self is unowned. Never exits - unlike hold, a broadcast
/// with an unprovable self still delivers.
fn sender_stamp(from_name: Option<&str>) -> (&'static str, String) {
    let ident = resolve_self_identity(
        &|name| std::env::var(name).ok(),
        None,
        None,
        &AgentsHome::from_env(),
    );
    stamp_from_identity(from_name, &ident)
}

/// The pure half of `sender_stamp`, split out so the stamp matrix is testable
/// without pinning ambient process state: the oracle computed `owned` before
/// consulting `from_name`, so the kind follows the proof, whatever the
/// caller passed.
fn stamp_from_identity(from_name: Option<&str>, ident: &OwnedIdentity) -> (&'static str, String) {
    let owned = !matches!(
        ident.disposition,
        OwnedDisposition::Ambiguous | OwnedDisposition::Empty
    ) && ident.session_id.as_deref().is_some_and(|s| !s.is_empty())
        && ident.harness.as_deref().is_some_and(|h| !h.is_empty());
    let kind = if owned { "agent" } else { "operator" };
    let sender = match from_name {
        Some(name) => name.to_string(),
        None if owned => {
            let sid = ident.session_id.clone().unwrap_or_default();
            crate::identity::canonical_handle(&sid)
        }
        None => "fno".to_string(),
    };
    (kind, sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(
        session_id: Option<&str>,
        harness: Option<&str>,
        disposition: OwnedDisposition,
    ) -> OwnedIdentity {
        OwnedIdentity {
            session_id: session_id.map(str::to_string),
            harness: harness.map(str::to_string),
            disposition,
            markers_present: Vec::new(),
            rejected: Vec::new(),
        }
    }

    /// The oracle matrix: kind follows the owned proof; the sender is the
    /// explicit name, else the canonical handle, else `fno`.
    #[test]
    fn sender_stamp_matches_oracle_matrix() {
        let owned = ident(
            Some("a1b2c3d4-1111"),
            Some("claude"),
            OwnedDisposition::Single,
        );
        let unowned = ident(None, None, OwnedDisposition::Empty);
        assert_eq!(
            stamp_from_identity(Some("nova"), &owned),
            ("agent", "nova".to_string())
        );
        // The cell the deleted Python contract test guarded: an unproven self
        // with an explicit name broadcasts as the operator, never under the
        // named agent's registry row.
        assert_eq!(
            stamp_from_identity(Some("nova"), &unowned),
            ("operator", "nova".to_string())
        );
        assert_eq!(
            stamp_from_identity(None, &owned),
            ("agent", crate::identity::canonical_handle("a1b2c3d4-1111"))
        );
        assert_eq!(
            stamp_from_identity(None, &unowned),
            ("operator", "fno".to_string())
        );
    }
}

/// `_emit_style_refusal`: one best-effort `style_refusal` row in the state
/// root journal so the retry rate is a query over events.jsonl. Never blocks
/// the refusal it measures.
fn emit_style_refusal(violations: &[Violation]) {
    let mut rule_ids: Vec<u32> = violations.iter().map(|v| v.rule).collect();
    rule_ids.sort_unstable();
    rule_ids.dedup();
    let mut data = json!({
        "surface": "mail",
        "rule_ids": rule_ids,
        "violation_count": violations.len(),
    });
    let ident = resolve_self_identity(
        &|name| std::env::var(name).ok(),
        None,
        None,
        &AgentsHome::from_env(),
    );
    let source = match ident.session_id.as_deref() {
        Some(sid) if !sid.is_empty() => {
            data["session_id"] = json!(sid);
            "target"
        }
        _ => "test",
    };
    let row = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "type": "style_refusal",
        "source": source,
        "data": data,
    });
    let Some(root) = crate::agents_config::state_dir(std::path::Path::new(".")) else {
        return;
    };
    let _ = crate::event_store::append_envelope(&root.join("events.jsonl"), &row.to_string(), None);
}

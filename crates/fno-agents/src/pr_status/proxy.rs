//! The gh proxy policy for Footnote-launched processes. Ported from
//! `cli/src/fno/pr/gh_proxy.py` (pr port 7): classify the gh argv, delegate
//! real commands to the real gh with the quota markers scrubbed, and route
//! broker-shaped reads through the serialized quota transport. The re-entry
//! guard is pid-stamped, so a successor that sees its own marker knows it is
//! us again.

use std::os::unix::process::CommandExt;
use std::process::Command;

use serde_json::json;

use crate::pr_status::quota;

/// Exit 2 for both self-protection refusals (re-entry and the draft guard).
/// Exit 1 is indistinguishable from the real gh's own ordinary failure, so a
/// caller cannot tell "gh said no" from "the proxy refused to run gh".
pub(crate) const REFUSE_EXIT: i32 = 2;

/// What the proxy does with one gh argv.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Delegate,
    Broker,
}

/// The command words the broker serves: pr checks/list/status/view and api
/// graphql. (The `gh_proxy.classify` port.)
pub(crate) fn classify(args: &[String]) -> Action {
    let command = quota::command_args(args);
    let pair: Option<(&str, &str)> = match command.as_slice() {
        [first, second, ..] => Some((first.as_str(), second.as_str())),
        _ => None,
    };
    if matches!(
        pair,
        Some(("pr", "checks") | ("pr", "list") | ("pr", "status") | ("pr", "view"))
    ) {
        return Action::Broker;
    }
    if pair == Some(("api", "graphql"))
        || (command.first().map(String::as_str) == Some("api")
            && command.iter().skip(1).any(|token| token == "graphql"))
    {
        return Action::Broker;
    }
    Action::Delegate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn broker_words_classify_broker_and_others_delegate() {
        assert_eq!(classify(&argv(&["pr", "checks", "5"])), Action::Broker);
        assert_eq!(
            classify(&argv(&["pr", "list", "--state", "open"])),
            Action::Broker
        );
        assert_eq!(classify(&argv(&["pr", "status"])), Action::Broker);
        assert_eq!(classify(&argv(&["pr", "view", "5"])), Action::Broker);
        assert_eq!(
            classify(&argv(&["-R", "o/r", "pr", "view", "5"])),
            Action::Broker
        );
        assert_eq!(
            classify(&argv(&["api", "graphql", "-f", "query=q"])),
            Action::Broker
        );
        assert_eq!(
            classify(&argv(&["pr", "checks", "5", "--watch"])),
            Action::Broker
        );
        assert_eq!(classify(&argv(&["api", "repos/o/r"])), Action::Delegate);
        assert_eq!(classify(&argv(&["pr", "edit", "5"])), Action::Delegate);
    }

    /// The door is asked only for a draft intent: `pr create --draft` and
    /// both `pr ready --draft` forms. Everything else - including other
    /// --draft bearers like `pr edit` - delegates with no door call and no
    /// stderr line, so ordinary gh reads never pay the graph read.
    #[test]
    fn the_draft_door_is_asked_only_for_a_draft_intent() {
        let draft = |words: &[&str]| is_draft_intent(&argv(words));
        assert!(draft(&["pr", "create", "--draft"]));
        assert!(draft(&["pr", "ready", "--draft"]));
        assert!(draft(&["pr", "ready", "5", "--draft"]));
        assert!(draft(&["-R", "o/r", "pr", "create", "--draft"]));
        assert!(!draft(&["pr", "create", "t"]));
        assert!(!draft(&["pr", "ready", "5"]));
        assert!(!draft(&["pr", "edit", "5", "--draft"]));
        assert!(!draft(&["api", "repos/o/r"]));
        assert!(!draft(&["pr", "view", "5"]));
    }
}

/// True only for a draft intent: `pr create ... --draft` or `pr ready
/// ... --draft` (the numberless DWIM form included). Everything else
/// delegates with no door call and no stderr line, so ordinary gh reads
/// pay no graph read and never print the door's lagging-binary noise.
/// Global gh flags ahead of the subcommand strip away the same way
/// `classify` strips them.
fn is_draft_intent(args: &[String]) -> bool {
    if !args.iter().any(|a| a == "--draft") {
        return false;
    }
    let command = quota::command_args(args);
    let pair: Option<(&str, &str)> = match command.as_slice() {
        [first, second, ..] => Some((first.as_str(), second.as_str())),
        _ => None,
    };
    matches!(pair, Some(("pr", "create")) | Some(("pr", "ready")))
}

/// Replace the proxy so untouched gh commands keep TTY and streaming
/// semantics: build the scrubbed env, stamp the re-entry marker, then
/// execve the real gh. Never returns. (The `gh_proxy.delegate` port.)
fn delegate(real: &str, args: &[String]) -> ! {
    if let Some(refusal) = crate::gh_budget::admit_spawn_for_origin(args) {
        eprintln!("{refusal}");
        std::process::exit(quota::REFUSED);
    }
    // The gh-side provenance: every delegated argv feeds record_request,
    // which self-filters to merge requests, so a bare `gh pr merge` from an
    // agent still leaves its `merge_requested` row. This used to ride the
    // draft door's check; the door is now asked only for a draft intent, so
    // the recording moved here to keep merge provenance complete. Disjoint
    // from the draft guard: one argv is never both a merge and a draft
    // intent, so nothing records twice.
    let cwd = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::merge_provenance::record_request(
        &quota::command_args(args),
        Some(cwd.as_str()),
        "gh_proxy",
    );
    // The draft guard: only a draft intent reaches the door. The sweep is
    // the second gate, and an unreachable door still admits a real draft
    // intent (the Python leg rode the same door through command_call).
    if is_draft_intent(args) {
        let ask = json!({
            "pr_draft_ready": {
                "check": args,
                "cwd": cwd,
            }
        });
        let reply = crate::pr_draft_ready::run_door(&ask);
        if reply.get("admitted").and_then(serde_json::Value::as_bool) != Some(true) {
            let refusal = reply
                .get("refusal")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("gh proxy: --draft refused");
            eprintln!("{refusal}");
            std::process::exit(REFUSE_EXIT);
        }
    }
    let env = match quota::delegate_environment() {
        Ok(env) => env,
        Err(exc) => {
            eprintln!("{}", quota::proxy_identity_refusal(&exc));
            std::process::exit(REFUSE_EXIT);
        }
    };
    let mut env_map: std::collections::BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(k, v)| k.into_string().ok().zip(v.into_string().ok()))
        .collect();
    env_map.remove("FNO_REAL_GH");
    env_map.remove(quota::PROXY_DIR_ENV);
    // `delegate` re-stamps this immediately for its own execve. Every other
    // caller runs the real gh directly, and its descendants must not inherit
    // a marker that makes a later first entry look like a loop.
    env_map.remove(quota::PROXY_DEPTH_ENV);
    env_map.insert(
        quota::PROXY_DEPTH_ENV.to_string(),
        std::process::id().to_string(),
    );
    if let Some(path) = env.get("PATH") {
        env_map.insert("PATH".to_string(), path.clone());
    }
    let err = Command::new(real).args(args).envs(env_map).exec();
    eprintln!("gh proxy: exec {real} failed: {err}");
    std::process::exit(REFUSE_EXIT);
}

/// The binary's entry: re-entry guard, classify, resolve the real gh,
/// broker or delegate. (The `gh_proxy.main` port.)
pub fn run_proxy_main(args: Vec<String>) -> i32 {
    // The marker carries our PID, and that is what makes it safe: execve
    // preserves the pid and a child never shares it, so comparing the marker
    // to our own pid separates "I am my own successor" from "an unrelated
    // descendant inherited a stale value".
    if std::env::var(quota::PROXY_DEPTH_ENV).is_ok_and(|v| v == std::process::id().to_string()) {
        eprintln!(
            "gh proxy: re-entered itself. resolve_real_gh() gave back the proxy \
shim instead of a real gh binary. Refusing to loop."
        );
        return REFUSE_EXIT;
    }
    let action = classify(&args);
    let real = match quota::resolve_real_gh() {
        Ok(Some(real)) => real,
        Ok(None) => {
            eprintln!("gh proxy: real gh executable not found");
            return 127;
        }
        Err(exc) => {
            eprintln!("{}", quota::proxy_identity_refusal(&exc));
            return REFUSE_EXIT;
        }
    };
    if action == Action::Broker {
        let result = quota::execute_graphql(
            "discretionary",
            &args,
            Some(&real),
            None,
            None,
            std::time::Duration::from_secs(120),
        );
        if !result.stdout.is_empty() {
            print!("{}", result.stdout);
        }
        if !result.stderr.is_empty() {
            eprint!("{}", result.stderr);
        }
        return result.returncode;
    }
    delegate(&real, &args)
}

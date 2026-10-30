//! The worker-command seam, in process. Every non-spawn client argv passes
//! here before the client parses it: `ask` gains `--prompt-file`, the
//! commands that build a worker argv publish the writable-dir grant and take
//! the headroom account pick, `rm` warns that it forfeits a resume handle,
//! and a foreign `--cwd` pins the worktree policy. `spawn` owns its own seam
//! ([`crate::spawn_seam`]).

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::spawn_admission::{has_flag, is_value_flag, pick_account_arg};
use crate::spawn_seam::{export_worker_dirs, pin_worktree_policy, spawn_cwd};

/// An operator answered no at `rm`'s prompt. Distinct from every daemon code,
/// so "you declined" never reads as "it failed": nothing was removed.
pub const RM_DECLINED_EXIT: i32 = 3;

/// The commands other than `spawn` that build a worker argv.
const WORKER_DIR_COMMANDS: &[&str] = &["resume", "ask", "revive", "wake"];

/// Stamped once the notice is written, for a later writer below the seam.

/// Stops the prompt WITHOUT opting into `--force`, which also drops a live
/// row and leaves a named orphan when teardown fails.
const ASSUME_YES_ENV: &str = "FNO_RM_ASSUME_YES";

/// The harnesses whose `rm` tears a session record down. opencode is
/// registry-only by design and gemini has no teardown arm, so warning for
/// either would report a loss that does not happen.
const TEARDOWN_HARNESSES: &[&str] = &["claude", "codex"];

const RM_AUDIT_VALUE_FLAGS: &[&str] = &[
    "--audit-actor",
    "--audit-reason",
    "--audit-request-id",
    "--audit-reclaimed-bytes",
];

/// Prepare one argv. `Ok` is the argv to parse; `Err` is the exit code of a
/// refusal whose lines are already printed. `args` carries `command` first.
pub fn prepare(command: &str, args: Vec<String>) -> Result<Vec<String>, i32> {
    if args.is_empty() || matches!(command, "spawn" | "-h" | "--help") {
        return Ok(args);
    }
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut args = args;
    if command == "ask" {
        args = shape_ask(args)?;
    }
    if WORKER_DIR_COMMANDS.contains(&command) {
        export_worker_dirs(&spawn_cwd(&args, &here));
        args = pick_account_arg(&args, &here);
    } else if command == "rm" && !gate_rm(&args) {
        return Err(RM_DECLINED_EXIT);
    }
    pin_worktree_policy(&args, &here);
    Ok(args)
}

/// What the client's flag loop cannot do for `ask`: read `--prompt-file` into
/// the `--message=` seed, and make an explicit `--cwd` absolute, because the
/// daemon reading it stands in another directory. A leading-dash prompt is
/// data: the equals form keeps it bound to its flag.
fn shape_ask(args: Vec<String>) -> Result<Vec<String>, i32> {
    let mut out: Vec<String> = Vec::with_capacity(args.len() + 1);
    let mut prompt_file: Option<String> = None;
    let mut inline = false;
    let mut to_project = false;
    let mut positionals: usize = 0;
    let mut boundary: Option<usize> = None;
    let mut it = args.into_iter();
    out.extend(it.next());
    while let Some(tok) = it.next() {
        if tok == "--" || tok == "--argv" {
            boundary = Some(out.len());
            let fenced = tok == "--";
            out.push(tok);
            let rest: Vec<String> = it.by_ref().collect();
            if fenced {
                positionals += rest.len();
            }
            out.extend(rest);
            break;
        }
        if tok == "--prompt-file" {
            match it.next() {
                Some(path) => prompt_file = Some(path),
                None => {
                    eprintln!("fno-agents: --prompt-file needs a value");
                    return Err(2);
                }
            }
            continue;
        }
        if let Some(path) = tok.strip_prefix("--prompt-file=") {
            prompt_file = Some(path.to_string());
            continue;
        }
        if tok == "--cwd" || tok == "-c" {
            out.push(tok);
            if let Some(value) = it.next() {
                out.push(absolute_cwd(&value));
            }
            continue;
        }
        if let Some(value) = tok.strip_prefix("--cwd=") {
            out.push("--cwd".to_string());
            out.push(absolute_cwd(value));
            continue;
        }
        if tok == "--message" || tok.starts_with("--message=") {
            inline = true;
        }
        if tok == "--to-project" || tok.starts_with("--to-project=") {
            to_project = true;
        }
        let takes_value =
            is_value_flag(&tok) || matches!(tok.as_str(), "--to-project" | "--answer");
        if !tok.starts_with('-') {
            positionals += 1;
        }
        out.push(tok);
        if takes_value {
            out.extend(it.next());
        }
    }
    let Some(path) = prompt_file else {
        return Ok(out);
    };
    // A project names no peer, so its first positional is already the prompt.
    let name_slots: usize = if to_project { 0 } else { 1 };
    if inline || positionals > name_slots {
        eprintln!("error: provide the prompt once - inline or as a file, not both");
        return Err(1);
    }
    let text = read_prompt(&path)?;
    let at = boundary.unwrap_or(out.len());
    out.insert(at, format!("--message={text}"));
    Ok(out)
}

/// An empty `--cwd` stays empty: the client reads it as absent.
fn absolute_cwd(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    crate::spawn_cwd::canonicalize_cwd(value)
        .to_string_lossy()
        .into_owned()
}

/// The prompt file's text; `-` reads stdin.
fn read_prompt(path: &str) -> Result<String, i32> {
    if path == "-" {
        let mut text = String::new();
        return match std::io::stdin().read_to_string(&mut text) {
            Ok(_) => Ok(text),
            Err(exc) => {
                eprintln!("error: cannot read {path}: {exc}");
                Err(1)
            }
        };
    }
    std::fs::read_to_string(path).map_err(|exc| {
        let text = crate::research::deliverable::os_error_text(&exc, Path::new(path));
        eprintln!("error: cannot read {path}: {text}");
        1
    })
}

/// `rm`'s name, never mistaking an internal audit value for it. Stops at the
/// `--argv` payload and the bare `--` fence.
fn rm_target_name(tail: &[String]) -> Option<String> {
    let mut i = 0;
    while i < tail.len() {
        let token = tail[i].as_str();
        if token == "--argv" || token == "--" {
            return None;
        }
        if RM_AUDIT_VALUE_FLAGS.contains(&token) {
            i += 2;
            continue;
        }
        if token.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(token.to_string());
    }
    None
}

/// False only on a declined prompt. A `--help`, or a call with no name, is
/// left to the parser: warning about a reap that is not about to happen is
/// noise.
fn gate_rm(args: &[String]) -> bool {
    let tail = &args[1..];
    let Some(name) = rm_target_name(tail) else {
        return true;
    };
    if has_flag(tail, "", &["--help"]) || tail.iter().any(|a| a == "-h") {
        return true;
    }
    warn_and_confirm(&name, has_flag(tail, "-F", &["--force"]))
}

/// The row `rm` would resolve for `name`, or None. An unreadable registry or
/// an ambiguous token degrades to silence: the real command gives the real
/// error.
fn lookup_row(name: &str) -> Option<Value> {
    let home = crate::paths::AgentsHome::from_env();
    let rows = crate::client_commands::read_registry_entries(&home.registry_json()).ok()?;
    crate::client_commands::find_agent_entry(&rows, name)
        .ok()
        .cloned()
}

/// The row's harness, provider as the fallback for a row not yet backfilled.
fn row_harness(row: &Value) -> &str {
    row.get("harness")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .or_else(|| row.get("provider").and_then(Value::as_str))
        .unwrap_or("")
}

/// The id `adopt` would take back: the full session id first, because only it
/// is collision-free. A row with neither id has no handle to forfeit.
fn resume_handle_for(row: &Value) -> Option<String> {
    ["harness_session_id", "short_id"].iter().find_map(|key| {
        row.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
    })
}

/// The stderr block warning that a reap forfeits a resume handle.
fn resume_handle_notice(name: &str, harness: &str, handle: &str) -> String {
    let is_short_id = handle.len() == 8 && handle.chars().all(|c| "0123456789abcdef".contains(c));
    let recovery_line = if is_short_id {
        "      This short id is a best-effort lookup only while durable harness evidence still resolves it.\n"
    } else {
        "      Keep this full harness session UUID as the resume handle and recovery handle.\n"
    };
    format!(
        "WARN: removing the {harness} session record for {name} ({handle}).\n\
         \x20     The transcript stays on disk.\n\
         {recovery_line}\
         \x20     Reverse it with: fno agents adopt {handle}\n"
    )
}

/// The notice for one row, or None when the removal forfeits nothing.
fn notice_for(row: &Value, name: &str) -> Option<String> {
    let harness = row_harness(row);
    if !TEARDOWN_HARNESSES.contains(&harness) {
        return None;
    }
    let handle = resume_handle_for(row)?;
    let shown = row.get("name").and_then(Value::as_str).unwrap_or(name);
    Some(resume_handle_notice(shown, harness, &handle))
}

/// Write the notice and decide whether the reap may proceed. The prompt needs
/// BOTH stdin and stderr on a terminal: an unattended reap must not wedge, and
/// `rm >/dev/null 2>&1` with stdin inherited would otherwise block on a
/// question whose text went nowhere.
fn warn_and_confirm(name: &str, force: bool) -> bool {
    let Some(row) = lookup_row(name) else {
        return true;
    };
    let Some(notice) = notice_for(&row, name) else {
        return true;
    };
    let mut err = std::io::stderr();
    let _ = err.write_all(notice.as_bytes());
    let _ = err.flush();

    let interactive = std::io::stdin().is_terminal() && err.is_terminal();
    let assume_yes = std::env::var_os(ASSUME_YES_ENV).is_some_and(|v| !v.is_empty());
    if force || assume_yes || !interactive {
        return true;
    }
    let shown = row.get("name").and_then(Value::as_str).unwrap_or(name);
    let _ = write!(
        err,
        "Remove {shown} anyway? [y/N] (set {ASSUME_YES_ENV}=1 to stop asking) "
    );
    let _ = err.flush();
    let mut answer = String::new();
    // A closed or unreadable stdin is not consent.
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return false;
    }
    answer_consents(&answer)
}

fn answer_consents(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    /// Oracle: `fno.agents.rm_notice`.
    #[test]
    fn the_rm_notice_matches_the_python_oracle() {
        let golden: Vec<Value> =
            serde_json::from_str(include_str!("../tests/golden/agents_seam/rm_notice.json"))
                .unwrap();
        for case in &golden {
            let row = &case["row"];
            assert_eq!(
                resume_handle_for(row).as_deref(),
                case["handle"].as_str(),
                "{row}"
            );
            assert_eq!(
                notice_for(row, "ignored").as_deref(),
                case["notice"].as_str(),
                "{row}"
            );
        }
        assert!(answer_consents(" YES\n") && !answer_consents("") && !answer_consents("n\n"));
        assert_eq!(
            rm_target_name(&argv(&["--audit-actor", "lead", "-F", "w1"])).as_deref(),
            Some("w1")
        );
        assert_eq!(rm_target_name(&argv(&["--", "w1"])), None);
    }

    #[test]
    fn ask_reads_a_prompt_file_before_the_fence_and_refuses_two_prompts() {
        let file = std::env::temp_dir().join(format!("fno-ask-seed-{}.txt", std::process::id()));
        std::fs::write(&file, "--not a flag").unwrap();
        let path = file.to_string_lossy().into_owned();

        assert_eq!(
            shape_ask(argv(&["ask", "--prompt-file", &path, "--", "w1"])),
            Ok(argv(&["ask", "--message=--not a flag", "--", "w1"]))
        );
        assert_eq!(
            shape_ask(argv(&["ask", "w1", &format!("--prompt-file={path}")])),
            Ok(argv(&["ask", "w1", "--message=--not a flag"]))
        );
        assert_eq!(
            shape_ask(argv(&["ask", "--to-project", "p", "--prompt-file", &path])),
            Ok(argv(&[
                "ask",
                "--to-project",
                "p",
                "--message=--not a flag"
            ]))
        );
        assert_eq!(
            shape_ask(argv(&["ask", "w1", "hi", "--prompt-file", &path])),
            Err(1)
        );
        assert_eq!(
            shape_ask(argv(&["ask", "w1", "--message=hi", "--prompt-file", &path])),
            Err(1)
        );
        assert_eq!(
            shape_ask(argv(&["ask", "w1", "--prompt-file", "/no/such/fno-file"])),
            Err(1)
        );
        // No prompt file: every token rides through in place, a bare name included.
        assert_eq!(
            shape_ask(argv(&["ask", "w1", "--answer", "allow_once"])),
            Ok(argv(&["ask", "w1", "--answer", "allow_once"]))
        );
        std::fs::remove_file(&file).ok();
    }
}

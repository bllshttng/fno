//! The native `fno inbox outstanding` subcommands: `mine`, `ask`, `clear`,
//! `reindex`.
//!
//! The deleted Python package (`cli/src/fno/outstanding/`) answered these
//! behind the native listing; this module is the one leg now. Everything the
//! shells resolved Python-side - identity, live law rows, storage root,
//! session id, the lane store, the mail delivery - resolves here, so the
//! front door can claim the spellings outright with no forward shell.
//!
//! Parity contract: every command spelling, refusal line and exit code
//! matches the deleted Python leg byte for byte; the characterization tests
//! pin the lines the operator or a script can be mid-read on.

use crate::graph_store::BoundedLock;
use crate::org_board::queues::parse_lane;
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// Route one `fno inbox outstanding <word>` tail. `None` is not a
/// subcommand: the caller falls through to the listing parse.
pub fn dispatch(word: &str, rest: &[String]) -> Option<i32> {
    match word {
        "mine" => Some(run_mine(rest)),
        "reindex" => Some(run_reindex(rest)),
        "clear" => Some(run_clear_cmd(rest)),
        "ask" => Some(run_ask(rest)),
        _ => None,
    }
}

/// The delivery body's style-gate bypass: the answer is quoted decision text
/// passing through, not prose the sender authored.
const STYLE_EXCEPTION: &str = "operator answer verbatim: quoted decision text, not authored prose";

/// The system-sender arm the delivery mails from: a named `fno/<arm>` voice
/// the recipient can answer, ending the bare `unknown` stamp the Python leg
/// produced whenever no session resolved.
const DELIVERY_SENDER: &str = "fno/questions";

/// The clear arm: validate the authority claim, resolve provenance once,
/// close through the clear core, then deliver every answer to its asker.
fn run_clear_cmd(args: &[String]) -> i32 {
    let mut ids: Vec<String> = Vec::new();
    let mut answer_text: Option<String> = None;
    let mut authority: Option<String> = None;
    let mut origin: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                println!("usage: fno inbox outstanding clear <qid>... [--answer <text>] [--authority <source>] [--origin <origin>]");
                return 0;
            }
            "--answer" => match args.get(index + 1) {
                Some(value) => {
                    answer_text = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --answer needs a value");
                    return 2;
                }
            },
            "--authority" => match args.get(index + 1) {
                Some(value) => {
                    authority = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --authority needs a value");
                    return 2;
                }
            },
            "--origin" => match args.get(index + 1) {
                Some(value) => {
                    origin = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --origin needs a value");
                    return 2;
                }
            },
            other if other.starts_with('-') => {
                eprintln!("outstanding: unknown argument {other:?}");
                return 2;
            }
            other => ids.push(other.to_string()),
        }
        index += 1;
    }
    if ids.is_empty() {
        eprintln!("usage: fno inbox outstanding clear <qid>... [--answer <text>] [--authority <source>] [--origin <origin>]");
        return 2;
    }
    if let Some(source) = &authority {
        if !matches!(source.as_str(), "operator" | "role" | "agent" | "beastmode") {
            eprintln!(
                "outstanding: --authority '{}' is not one of operator, role, agent, beastmode. Nothing was closed.",
                source
            );
            return 2;
        }
    }
    // The truncation note rides stderr before anything is written; the clear
    // core cuts at the cap on its side.
    if let Some(text) = &answer_text {
        let chars = text.chars().count();
        let cap = crate::question_intake::QUESTION_CAP;
        if chars > cap {
            eprintln!(
                "outstanding: recorded truncated: the answer is {chars} characters, the event stores {cap}."
            );
        }
    }
    let provenance = match &answer_text {
        Some(_) => {
            let enforced_origin = crate::decide_door::enforce_origin_floor(
                origin.as_deref(),
                crate::decide_door::DecideIdentity::Ambient,
            );
            match crate::decide_door::resolve_decider_lanes(
                crate::decide_door::DecideIdentity::Ambient,
                None,
                authority.as_deref(),
                enforced_origin.as_deref(),
            ) {
                Ok(p) => json!({
                    "decided_by": p.decided_by,
                    "authority_source": p.authority_source,
                    "attested_by": p.attested_by,
                    "relayed_by": p.relayed_by,
                    "origin": enforced_origin,
                }),
                Err(refusal) => {
                    let (kind, text, refused_origin) = provenance_refusal(refusal);
                    let remedy = provenance_remedy(&kind, refused_origin.as_deref().unwrap_or(""));
                    let mut line = format!(
                        "outstanding: refused: {text}. Nothing was closed; all {} question(s) stay open.",
                        ids.len()
                    );
                    if !remedy.is_empty() {
                        line.push('\n');
                        line.push_str(&remedy);
                    }
                    eprintln!("{line}");
                    return 3;
                }
            }
        }
        None => json!({}),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = crate::paths::AgentsHome::from_env();
    let request = crate::question_clear::ClearRequest {
        ids,
        answer: answer_text.clone(),
        cap: crate::question_intake::QUESTION_CAP,
        provenance,
        closed_by: crate::outstanding::session_id(&cwd),
        caller: None,
        journal_path: crate::paths::events_path(&cwd),
        index_path: crate::provider_cap::questions_path(&home),
        decisions_path: crate::decision_index::default_state_path("decisions.jsonl"),
        graph: crate::graph_get::default_graph_path(),
        repo_root: cwd,
    };
    let result = crate::question_clear::run_clear(&request);
    if !result.lines.is_empty() {
        println!("{}", result.lines.join("\n"));
    }
    // Delivery runs outside the close's own error path: the close is durable
    // before delivery starts, so one bad recipient never costs the count.
    let raw_answer = answer_text.unwrap_or_default();
    for delivery in &result.deliveries {
        eprintln!(
            "{}",
            deliver_answer(
                &delivery.qid,
                &delivery.question,
                &delivery.asker,
                delivery.session_id.as_deref(),
                &delivery.decision_id,
                &raw_answer,
            )
        );
    }
    result.exit_code
}

/// The remedy line a provenance refusal carries after its refusal sentence:
/// one named fix per kind, the refused origin quoted Python-repr style.
fn provenance_remedy(kind: &str, refused_origin: &str) -> String {
    match kind {
        "unknown-origin" => String::new(),
        "unattributed" => "This process has no session identity and no terminal, so it is not an agent and has no chat to compose in. Run it from an attended terminal, or from a real agent session, and answer again.".to_string(),
        "authority" => "An agent answers as agent or role. The superuser lane is not an agent's to claim. Drop --authority operator and answer again.".to_string(),
        "origin-authority" => format!(
            "The refusal is about the claimed --origin {}, not about who you are. Drop --origin (or drop --authority operator) and answer again.",
            crate::mail_cli::send_raw::py_repr(refused_origin)
        ),
        _ => String::new(),
    }
}

/// The refusal the provenance resolver raised, as the (kind, text, origin)
/// triple the remedy lines key on.
fn provenance_refusal(
    refusal: crate::decide_door::DecideRefusal,
) -> (String, String, Option<String>) {
    match refusal {
        crate::decide_door::DecideRefusal::UnknownOrigin(origin) => (
            "unknown-origin".to_string(),
            format!(
                "mail origin '{origin}' is unknown; use one of {}",
                crate::decide_door::MAIL_ORIGINS.join(", ")
            ),
            None,
        ),
        crate::decide_door::DecideRefusal::RefusedAuthority { agent, origin } => (
            if origin.is_some() {
                "origin-authority"
            } else {
                "authority"
            }
            .to_string(),
            match &origin {
                Some(o) => {
                    format!("agent {agent} cannot record under superuser authority from {o} origin")
                }
                None => format!("agent {agent} cannot record under superuser authority"),
            },
            origin,
        ),
        crate::decide_door::DecideRefusal::UnattributedAuthority => (
            "unattributed".to_string(),
            "the provenance resolver refused".to_string(),
            None,
        ),
    }
}

/// The posture line for one answered close. Never fails: every way delivery
/// cannot happen is a stated line, because the close is already durable and
/// one bad recipient must not cost the remaining count.
fn deliver_answer(
    qid: &str,
    question: &str,
    asker: &str,
    session_id: Option<&str>,
    decision_id: &str,
    answer: &str,
) -> String {
    let did = if decision_id.is_empty() {
        "the decision record"
    } else {
        decision_id
    };
    let recovery = format!("Decision {did}; recover it via: fno backlog decisions");
    if asker.is_empty() {
        return format!(
            "outstanding: {qid} answered; no asker on record (lead escalation), nobody to wake. {recovery}"
        );
    }
    // Resolve by the full session id first, falling back to the eight-digit
    // handle: the full id was in hand at ask time and is unique by
    // construction.
    let token = session_id.filter(|s| !s.is_empty()).unwrap_or(asker);
    let reach = crate::mail_cli::send::resolve::resolve_reachable(token);
    if !reach.ambiguous.is_empty() {
        return format!(
            "outstanding: {qid} answered; asker {asker} is ambiguous across {} stored sessions, not delivered (never guess). Decision {did}",
            reach.ambiguous.len()
        );
    }
    let Some(session) = reach.hit else {
        return format!(
            "outstanding: {qid} answered; asker {asker} has no stored session, nobody to wake. {recovery}"
        );
    };
    let full_id = if session.session_id.is_empty() {
        token.to_string()
    } else {
        session.session_id.clone()
    };
    if crate::mail_cli::send::resolve::resolve_reachable(&full_id)
        .hit
        .is_none()
    {
        return format!(
            "outstanding: {qid} answered; asker {asker} is not live, so nobody was woken. {recovery}"
        );
    }
    let body = format!("Answer to your question {qid} \"{question}\": {answer}.");
    let argv = vec![
        crate::mail_cli::send_raw::agents_binary(),
        "mail".to_string(),
        "send".to_string(),
        full_id.clone(),
        body,
        "--style-exception".to_string(),
        STYLE_EXCEPTION.to_string(),
        "--origin".to_string(),
        "operator".to_string(),
        "--from-name".to_string(),
        DELIVERY_SENDER.to_string(),
    ];
    let (code, detail) = mail_send_captured(&argv);
    if code != 0 {
        return format!(
            "outstanding: {qid} answered; delivery failed ({detail}). Retry: fno agents mail send {full_id} \"<the answer>\" --style-exception \"operator answer verbatim\""
        );
    }
    let evidence = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    format!("outstanding: {qid} answered; mail to {asker}{evidence} (decision {did})")
}

/// One bounded `fno-agents mail send` with both pipes captured: the child's
/// stderr is the failure detail the posture lines quote, and its last stdout
/// line is the evidence the delivered posture leads with. Returns
/// `(code, detail)` with Python's failure codes: 124 timed out, 127 spawn.
fn mail_send_captured(argv: &[String]) -> (u32, String) {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return (127, "fno-agents mail send could not be started".to_string()),
    };
    // Feed and close stdin so a prompt-reading child cannot hang the send.
    if let Some(stdin) = child.stdin.take() {
        drop(stdin);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(30.0);
    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = out.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf
        })
    });
    let mut code = 0u32;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code().unwrap_or(1).max(0) as u32;
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                code = 127;
                break;
            }
        }
    }
    let _ = stdout_handle;
    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if timed_out {
        return (124, "mail send timed out after 30s".to_string());
    }
    if code != 0 {
        let first = stderr
            .lines()
            .chain(stdout.lines())
            .find(|l| !l.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "no detail".to_string());
        return (code, first);
    }
    // The child's own evidence line is the delivery truth; exit 0 alone
    // cannot tell a live landing from a durable park.
    let detail = stdout.trim().lines().last().unwrap_or("").to_string();
    (0, detail)
}

/// The reindex arm: fold every ask/close identity from graph-named project
/// journals into the machine index. A missing project journal degrades to
/// zero rows; the one fatal read is the machine index itself.
fn run_reindex(args: &[String]) -> i32 {
    for a in args {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: fno inbox outstanding reindex");
                return 0;
            }
            other => {
                eprintln!("outstanding: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let src = crate::outstanding::sources(&cwd);
    let index = src.questions.clone();
    let journals: Vec<PathBuf> = crate::outstanding::capture_project_roots(&src)
        .iter()
        .map(|root| crate::outstanding::project_log(root, "events.jsonl"))
        .collect();
    match reindex_fold(&index, &journals) {
        Ok((added, already, total)) => println!(
            "{} event(s) added; {} already indexed; {} total",
            added, already, total
        ),
        Err(e) => {
            eprintln!("outstanding: failed to read: {e}");
            return 1;
        }
    }
    0
}

/// The fold behind the reindex arm: every ask/close identity from the named
/// journals lands in the machine index exactly once. Missing or unreadable
/// project journals degrade to zero rows; the machine index is the one store
/// a failed read refuses, because reporting an unreadable index as an empty
/// one would tell the operator the fold is current when it never ran.
fn reindex_fold(
    index: &std::path::Path,
    journals: &[PathBuf],
) -> Result<(usize, usize, usize), String> {
    let query = crate::event_store::EventQuery::of_types(&[
        "operator_question",
        "operator_question_closed",
    ]);
    let existing = crate::event_store::journal_text_checked(index, &query)?;
    let mut known: std::collections::HashSet<(String, String)> =
        existing.lines().filter_map(question_identity).collect();
    let preexisting = known.clone();
    let mut added = 0usize;
    let mut already: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    for journal in journals {
        // The store beside the journal is the write boundary: a journal whose
        // bytes never landed still owns a readable store, so the dedupe key
        // stats the store when the live file is absent.
        let live = crate::event_store::live_journal(journal);
        let probe = if live.exists() {
            live
        } else {
            crate::event_store::store_path(journal)
        };
        let Ok(meta) = std::fs::metadata(&probe) else {
            continue;
        };
        let key = (meta.dev(), meta.ino());
        if !seen.insert(key) {
            continue;
        }
        let text = match crate::event_store::journal_text_checked(journal, &query) {
            Ok(text) => text,
            Err(_) => continue,
        };
        for line in text.lines() {
            let Some(identity) = question_identity(line) else {
                continue;
            };
            if known.contains(&identity) {
                if preexisting.contains(&identity) {
                    already.insert(identity);
                }
                continue;
            }
            if crate::event_store::append_envelope(index, line, None).is_err() {
                continue;
            }
            known.insert(identity);
            added += 1;
        }
    }
    Ok((added, already.len(), known.len()))
}

/// The ask/close identity one journal row folds into the dedupe set: the
/// event type plus the question id. Decision rows carry no identity here.
fn question_identity(line: &str) -> Option<(String, String)> {
    let rec: Value = serde_json::from_str(line).ok()?;
    let kind = rec.get("type").and_then(Value::as_str)?;
    if kind != "operator_question" && kind != "operator_question_closed" {
        return None;
    }
    let qid = rec
        .pointer("/data/question_id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())?;
    Some((kind.to_string(), qid.to_string()))
}

/// The mine arm: list the operator's ranked lane, or apply one collapsed
/// mutation through `do`.
fn run_mine(args: &[String]) -> i32 {
    let path = lane_path();
    mine_at(&path, args)
}

/// The mine arm against one lane path; tests drive sandboxes through this
/// instead of moving the process cwd.
fn mine_at(path: &std::path::Path, args: &[String]) -> i32 {
    match args.split_first() {
        None => mine_list_at(path, false),
        Some((word, rest)) if word == "do" => mine_do_at(path, rest),
        Some((_, _)) => {
            let mut as_json = false;
            for a in args {
                match a.as_str() {
                    "--json" | "-J" => as_json = true,
                    "-h" | "--help" => {
                        println!("usage: fno inbox outstanding mine [--json|-J] | do add TEXT | do done N | do drop N | do link N NODE_ID");
                        return 0;
                    }
                    other => {
                        eprintln!("mine: unknown argument {other:?}");
                        return 2;
                    }
                }
            }
            mine_list_at(path, as_json)
        }
    }
}

/// The collapsed mutation door: `do add TEXT | done N | drop N | link N ID`.
/// One dispatched leaf instead of four: the CLI-surface ratchet caps total
/// registered leaves, and four commands doubled by the bare listing would
/// blow that budget.
fn mine_do_at(path: &std::path::Path, args: &[String]) -> i32 {
    let Some((action, rest)) = args.split_first() else {
        eprintln!("mine: failed: unknown action '' (add|done|drop|link)");
        return 2;
    };
    let want: usize = match action.as_str() {
        "add" | "done" | "drop" => 1,
        "link" => 2,
        other => {
            eprintln!(
                "mine: failed: unknown action {} (add|done|drop|link)",
                crate::mail_cli::send_raw::py_repr(other)
            );
            return 2;
        }
    };
    if rest.len() != want {
        eprintln!(
            "mine: failed: {action} takes {want} argument(s), got {}",
            rest.len()
        );
        return 2;
    }
    // Format validation runs before the lock, exactly as the Python leg did:
    // a malformed argument refuses without touching the file.
    match action.as_str() {
        "add" => {
            let text = &rest[0];
            if text.trim().is_empty() || text.contains('\n') || text.contains('\r') {
                eprintln!("mine: failed: item text must be one non-empty line");
                return 2;
            }
        }
        "done" | "drop" | "link" => {
            if parse_index(&rest[0]).is_none() {
                return 2;
            }
            if action == "link" && !is_node_id(&rest[1]) {
                eprintln!("mine: failed: invalid node id: {}", rest[1]);
                return 2;
            }
        }
        _ => unreachable!(),
    }
    match mutate_lane_at(path, action, rest) {
        Ok(count) => {
            println!("{count}");
            0
        }
        Err(err) => {
            eprintln!("mine: failed: {err}");
            1
        }
    }
}

fn parse_index(raw: &str) -> Option<usize> {
    match raw.parse::<usize>() {
        Ok(n) => Some(n),
        Err(_) => {
            eprintln!("mine: failed: not a number: {raw}");
            None
        }
    }
}

fn is_node_id(node: &str) -> bool {
    let body = crate::org_board::queues::NODE_ID_BODY;
    regex::Regex::new(&format!("^(?:{body})$"))
        .ok()
        .is_some_and(|re| re.is_match(node))
}

fn lane_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::org_board::scope::operator_lane_path(&cwd)
}

/// The one lock-and-rewrite transaction: parse, read raw, apply the line
/// change, atomic replace in the same directory, re-read. The bounded lock
/// holds the lane's sidecar `<lane>.lock` for ten seconds, the same budget
/// the Python `filelock.FileLock` took.
fn mutate_lane_at(path: &std::path::Path, action: &str, rest: &[String]) -> Result<usize, String> {
    let path = path.to_path_buf();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot write operator lane {}: {e}", path.display()))?;
    }
    let _lock = BoundedLock::acquire(&path, std::time::Duration::from_secs(10))
        .map_err(|e| format!("cannot write operator lane {}: {e}", path.display()))?;
    let items = parse_lane(&path)?;
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read operator lane {}: {e}", path.display())),
    };
    let mut lines: Vec<String> = split_keepends(&content);
    match action {
        "add" => {
            let text = rest[0].trim();
            if let Some(last) = lines.last_mut() {
                if !last.ends_with(['\n', '\r']) {
                    last.push('\n');
                }
            }
            lines.push(format!("- [ ] {text}\n"));
        }
        "done" | "drop" | "link" => {
            let index = rest[0].parse::<usize>().unwrap_or(0);
            let Some(item) = items.get(index.wrapping_sub(1)) else {
                return Err(format!(
                    "mine item {index} does not exist; choose 1 through {}",
                    items.len()
                ));
            };
            let idx = item.line - 1;
            let raw = lines.get(idx).ok_or_else(|| {
                format!(
                    "cannot read operator lane {}: line {index} is gone",
                    path.display()
                )
            })?;
            let owned = raw.clone();
            match action {
                "done" => {
                    let mark = if item.done { " " } else { "x" };
                    let mut flipped = String::with_capacity(owned.len());
                    flipped.push_str(&owned[..3]);
                    flipped.push_str(mark);
                    flipped.push_str(&owned[4..]);
                    lines[idx] = flipped;
                }
                "drop" => {
                    let _ = lines.remove(idx);
                }
                _ => {
                    // A link rewrites the whole line, so a parked reason on it
                    // is lost - the same overwrite the Python leg performed.
                    let ending = line_ending(&owned);
                    let mark = if item.done { "x" } else { " " };
                    lines[idx] = format!("- [{}] {} -> {}{}", mark, item.text, rest[1], ending);
                }
            }
        }
        _ => unreachable!(),
    }
    atomic_replace(&path, &lines.join(""))?;
    // The post-write read is the proof: a lane that cannot parse after the
    // rewrite is a failed write, not a quiet one.
    let updated = parse_lane(&path)?;
    Ok(updated.len())
}

/// `content` split with line endings kept, the `splitlines(keepends=True)`
/// shape the mutation edits.
fn split_keepends(content: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in content.chars() {
        current.push(ch);
        if ch == '\n' || ch == '\r' {
            lines.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// `\r\n`, `\n`, `\r`, or nothing: the ending the raw line carried.
fn line_ending(raw: &str) -> &str {
    if raw.ends_with("\r\n") {
        "\r\n"
    } else if raw.ends_with('\n') {
        "\n"
    } else if raw.ends_with('\r') {
        "\r"
    } else {
        ""
    }
}

/// Write through a same-directory temp file and rename: an interrupted
/// writer leaves the old lane, never a truncated one.
fn atomic_replace(path: &std::path::Path, content: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lane".to_string());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, content)
        .map_err(|e| format!("cannot write operator lane {}: {e}", path.display()))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!(
                "cannot write operator lane {}: {e}",
                path.display()
            ))
        }
    }
}

/// The lane read for the list arms: missing file reads as empty; an
/// unreadable one fails with the parse error.
fn mine_list_at(path: &std::path::Path, as_json: bool) -> i32 {
    let items = match parse_lane(path) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("mine: failed: {e}");
            return 1;
        }
    };
    if as_json {
        let rows: Vec<Value> = items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                json!({
                    "n": i + 1,
                    "text": item.text,
                    "done": item.done,
                    "node": item.node,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({ "mine": rows })).unwrap_or_default()
        );
        return 0;
    }
    for (i, item) in items.iter().enumerate() {
        let mark = if item.done { "x" } else { " " };
        let node = item
            .node
            .as_deref()
            .map(|n| format!(" -> {n}"))
            .unwrap_or_default();
        println!("{}. [{}] {}{}", i + 1, mark, item.text, node);
    }
    0
}

// ---------------------------------------------------------------------------
// ask
// ---------------------------------------------------------------------------

/// The laws the intake's law gate matches against (d-0fa92eb9: no agent asks
/// a question the operator already settled): the live law-lane rows, read
/// through the same decisions door the decide flow shells. A failed read is
/// fail-open by design - the question is recorded anyway, with the failure
/// named on stderr.
fn live_law_rows() -> Vec<crate::law_match::LawRow> {
    let argv = vec![
        crate::mail_cli::send_raw::agents_binary(),
        "backlog".to_string(),
        "decisions".to_string(),
        "--lane".to_string(),
        "law".to_string(),
        "--state".to_string(),
        "live".to_string(),
        "--json".to_string(),
    ];
    let (_, stdout) = match captured_stdout(&argv, 30.0) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("outstanding: live-law lookup failed ({e}); recording anyway");
            return Vec::new();
        }
    };
    let parsed: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("outstanding: live-law lookup failed ({e}); recording anyway");
            return Vec::new();
        }
    };
    let rows = parsed
        .get("decisions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| serde_json::from_value::<crate::law_match::LawRow>(row.clone()).ok())
        .collect()
}

/// One bounded self-exec reading stdout: `(code, stdout)` on a completed
/// child, `Err` naming the failure on spawn error or timeout.
fn captured_stdout(argv: &[String], timeout_secs: f64) -> Result<(i32, String), String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("{e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);
    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = out.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf);
            buf
        })
    });
    let mut code = 0;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code().unwrap_or(1);
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                code = 127;
                break;
            }
        }
    }
    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if timed_out {
        return Err("timed out".to_string());
    }
    if code != 0 {
        let detail = stderr
            .lines()
            .chain(stdout.lines())
            .find(|l| !l.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("exit {code}"));
        return Err(detail);
    }
    Ok((code, stdout))
}

/// The ask arm: every fact the deleted shim resolved Python-side - identity,
/// live law rows, storage root, session id, the user's stated name -
/// resolves here, then one intake runs and its answer echoes verbatim.
fn run_ask(args: &[String]) -> i32 {
    let mut question: Option<String> = None;
    let mut question_file: Option<String> = None;
    let mut ask: Option<String> = None;
    let mut options: Vec<String> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut node: Option<String> = None;
    let mut subject: Option<String> = None;
    let mut images: Vec<PathBuf> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                println!("usage: fno inbox outstanding ask [QUESTION] [--question-file <path>|-|template] [--ask <action>] [--option <choice>]... [--blocks <node>]... [--node <id>] [--subject <subject>] [--image <path>]...");
                return 0;
            }
            "--question-file" => match args.get(index + 1) {
                Some(value) => {
                    question_file = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --question-file needs a value");
                    return 2;
                }
            },
            "--ask" => match args.get(index + 1) {
                Some(value) => {
                    ask = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --ask needs a value");
                    return 2;
                }
            },
            "--option" => match args.get(index + 1) {
                Some(value) => {
                    options.push(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --option needs a value");
                    return 2;
                }
            },
            "--blocks" => match args.get(index + 1) {
                Some(value) => {
                    blocks.push(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --blocks needs a value");
                    return 2;
                }
            },
            "--node" => match args.get(index + 1) {
                Some(value) => {
                    node = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --node needs a value");
                    return 2;
                }
            },
            "--subject" => match args.get(index + 1) {
                Some(value) => {
                    subject = Some(value.clone());
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --subject needs a value");
                    return 2;
                }
            },
            "--image" => match args.get(index + 1) {
                Some(value) => {
                    images.push(PathBuf::from(value));
                    index += 1;
                }
                None => {
                    eprintln!("outstanding: --image needs a value");
                    return 2;
                }
            },
            other if other.starts_with('-') => {
                eprintln!("outstanding: unknown argument {other:?}");
                return 2;
            }
            other => {
                if question.is_some() {
                    eprintln!(
                        "error: got an extra argument: {other:?}. Provide the question once."
                    );
                    return 2;
                }
                question = Some(other.to_string());
            }
        }
        index += 1;
    }
    // 'template' rides --question-file: the flag surface is shrink-only.
    let template = question_file.as_deref() == Some("template");
    if template {
        question_file = None;
    }
    let text = match (&question, &question_file) {
        (Some(_), Some(_)) => {
            eprintln!("error: provide the question once - inline or as a file, not both");
            return 1;
        }
        // Template mode needs no text: nothing is recorded, the intake
        // ignores the field.
        (None, None) if !template => {
            eprintln!("error: provide the question - positionally or --question-file");
            return 2;
        }
        (None, None) => String::new(),
        (Some(q), None) => q.clone(),
        (None, Some(file)) => {
            if file == "-" {
                let mut buf = String::new();
                use std::io::Read;
                if std::io::stdin().read_to_string(&mut buf).is_err() {
                    eprintln!("error: cannot read -: stdin failed");
                    return 1;
                }
                buf
            } else {
                match std::fs::read_to_string(file) {
                    Ok(text) => text,
                    Err(e) => {
                        eprintln!("error: cannot read {file}: {e}");
                        return 1;
                    }
                }
            }
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = crate::paths::AgentsHome::from_env();
    let ident = crate::spawn_context::resolve_self_identity(
        &|name| std::env::var(name).ok(),
        None,
        None,
        &home,
    );
    let session = crate::outstanding::session_id(&cwd);
    let asker = match (&ident.session_id, &ident.harness) {
        (Some(s), Some(_)) => Some(crate::identity::canonical_handle(s)),
        _ => None,
    };
    // Nothing is recorded on a template request, so no law row can refuse it.
    let laws = if template {
        Vec::new()
    } else {
        live_law_rows()
    };
    let who = crate::backlog::birth::display_name(&crate::paths::worktree_repo_root(&cwd));
    let request = crate::question_intake::IntakeRequest {
        question: text,
        template,
        ask,
        options,
        blocks,
        node,
        subject,
        session_id: session,
        cwd: Some(cwd.to_string_lossy().into_owned()),
        asker,
        laws,
        storage_root: crate::outstanding::storage_root(&cwd),
        index_path: Some(crate::provider_cap::questions_path(&home)),
        journal_path: Some(crate::paths::events_path(&cwd)),
        display_name: Some(who),
        render_cap: None,
        op: None,
        event: None,
        screens_dir: (!images.is_empty())
            .then(|| crate::escalation::questions_dir(&cwd).join("screens")),
        images,
    };
    let answer = crate::question_intake::run_intake(&request, &home);
    for line in &answer.lines {
        if template {
            print!("{line}");
        } else {
            eprintln!("{line}");
        }
    }
    if answer.exit_code != 0 {
        return answer.exit_code;
    }
    if let Some(qid) = &answer.qid {
        if !template {
            println!("{qid}");
        }
    }
    0
}

#[cfg(test)]
#[path = "outstanding_cmds_tests.rs"]
mod tests;

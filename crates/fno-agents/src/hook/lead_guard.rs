//! `fno-agents hook lead-guard` - the native PreToolUse org guard.
//!
//! Port of `hooks/lead-delegation-guard.sh` policy: a session whose registry
//! row carries a team and whose lead manifest declares shape `org` is
//! refused Edit/Write/NotebookEdit and shell writes to source. Inverted
//! predicate, never delegation advice: SOURCE is any path realpath-inside
//! the repo root, except the repo's `.fno` state tree, build output, and
//! the `lead.write_roots` entries the operator lists; everything else - the
//! vault wherever it lives, memory wherever the harness keeps it - allows.
//! Any failure to READ (payload, registry, manifest, config) allows - the
//! never-block contract.
//!
//! The one behavior change the blueprint names: a missing `fno-agents`
//! binary now allows with one stderr line (the wrapper's business), and the
//! shell's five Python CLI round trips (93% of a 9,961ms org trace) are
//! gone - the guard answers in-process.

use serde_json::Value;
use std::path::{Path, PathBuf};

use super::lead_guard_wire as wire;
use crate::agents_config::config_lookup;

/// Entry: read the payload once, decide, print, always exit 0. `--wire agy`
/// reads agy's PreToolUse payload and prints agy's decision shape instead of
/// the claude hook JSON.
pub fn run(args: &[String]) -> i32 {
    let wire_agy = args.windows(2).any(|w| w[0] == "--wire" && w[1] == "agy");
    let raw: Value = serde_json::from_str(super::read_stdin().trim()).unwrap_or(Value::Null);
    let payload = if wire_agy {
        wire::agy_to_claude(raw)
    } else {
        raw
    };
    let trace = std::env::var_os("FNO_GUARD_TRACE").is_some();
    let emit_allow = || -> i32 {
        if wire_agy {
            println!("{{\"decision\":\"allow\"}}");
            0
        } else {
            super::emit_allow()
        }
    };
    let allow = |why: &str| -> i32 {
        if !why.is_empty() {
            eprintln!("lead-delegation-guard: {why}");
        }
        emit_allow()
    };
    let allow_at = |stage: &str| -> i32 {
        if trace {
            eprintln!("lead-delegation-guard: allow at {stage}");
        }
        emit_allow()
    };

    // 1. Empty or unparseable payload: not a refusal.
    if payload.is_null() {
        return allow_at("payload-null");
    }
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let ti = payload.get("tool_input").cloned().unwrap_or(Value::Null);

    // 2. Only the five implemented tools are judged. `apply_patch` is what
    //    codex reports as the raw tool name; the patch body rides in
    //    `tool_input.command` (judged at step 10).
    if !matches!(
        tool,
        "Edit" | "Write" | "NotebookEdit" | "Bash" | "apply_patch"
    ) {
        return allow_at("tool-not-judged");
    }

    // 3. Bash first classifies writes: a call with no write target allows
    //    before any registry, manifest or config read.
    let targets: Vec<String> = if tool == "Bash" {
        let cmd = ti.get("command").and_then(Value::as_str).unwrap_or("");
        write_targets(cmd)
            .into_iter()
            .filter(|t| t != "/dev" && !t.starts_with("/dev/"))
            .collect()
        // A malformed shell never executes: no targets, nothing to judge.
    } else {
        Vec::new()
    };
    if tool == "Bash" && targets.is_empty() {
        return allow_at("bash-no-write");
    }

    // 4. Session id: payload, transcript basename, then env markers.
    let sid = resolve_sid(
        payload
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or(""),
        payload
            .get("transcript_path")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    if sid.is_empty() {
        return allow("");
    }

    // 5. Registry: the team row.
    let rows = crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .map(|r| r.entries)
        .map_err(|e| e.to_string());
    let Some(row) = rows
        .as_ref()
        .ok()
        .and_then(|r| crate::lead_state::find_by_session(r, &sid, None))
    else {
        if let Err(e) = &rows {
            eprintln!("lead-delegation-guard: registry unreadable ({e}); allowing");
        }
        return allow("");
    };
    let role_scope = row
        .role_scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if row.role_level.is_none() && role_scope.is_empty() {
        return allow("");
    }

    // 6. Manifest: the lead declaration. Registry row, not file presence,
    //    proved authority; the manifest must name org for THIS session.
    //    Keyed on the team row's cwd (the writer's key, as stop.rs's
    //    lead_manifest_in), never the payload cwd: a org whose shell sits
    //    outside the repo still meets its org, and the deny region below
    //    draws from the same root. Relative write targets still resolve
    //    against the payload cwd, where the shell stands.
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let root = if row.cwd.is_empty() {
        cwd.clone()
    } else {
        PathBuf::from(&row.cwd)
    };
    let manifest = match crate::lead_state::manifest_path(&super::events_space(&root), role_scope) {
        Ok(m) => m,
        // An unsafe or empty scope never names a manifest; never-block allows.
        Err(_) => return allow(""),
    };
    let Ok(content) = std::fs::read_to_string(&manifest) else {
        return allow("");
    };
    let Some(km) = crate::loopcheck::parse_lead_manifest(&content) else {
        return allow("");
    };
    if km.shape != "org" || km.harness_session_id.as_deref() != Some(sid.as_str()) {
        return allow("");
    }

    // 7. Mode knob: refuse (default) | warn | off.
    let mode = config_lookup(&root, &["lead", "implementation_guard"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "refuse".to_string());
    if mode == "off" {
        return allow("");
    }

    // 8. The repo root is the only DENY region. Realpath containment has no
    //    unresolvable state, so the never-block contract needs no escape
    //    hatch here: outside the repo allows, whatever it is.
    let repo_root = crate::paths::worktree_repo_root(&root);
    let roots = write_roots(config_lookup(&root, &["lead", "write_roots"]), &repo_root);

    // 9. Limb carve-outs (checked after the roots resolve, like the shell).
    let agent_id = payload
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let transcript = payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !agent_id.is_empty() {
        return allow(&format!(
            "limb (agent_id {agent_id}) of teamed session {sid}; allowing"
        ));
    }
    if wire::is_subagent_transcript(transcript, &sid) {
        return allow(&format!("limb of teamed session {sid}; allowing"));
    }
    if wire::transcript_is_open_spawn(transcript) {
        return allow(&format!(
            "limb of teamed session {sid} (open Task/Agent tool_use in the parent transcript); allowing"
        ));
    }

    // 10. Decide: deny SOURCE, allow everything else, one predicate for
    //     every tool.
    let allowed = |t: &str| !write_denied(t, &cwd, &repo_root, &roots);
    let denied: Option<String> = wire::read_denied(tool, &ti, &targets, &allowed);

    // 11. Telemetry: one row, one file, failure ignored.
    super::emit_guard_decision(&cwd, "lead-delegation-guard", tool, denied.is_some());

    let Some(denied) = denied else {
        return allow("");
    };
    if mode == "warn" {
        eprintln!("{}", wire::deny_text(&denied, &repo_root));
        return allow("");
    }
    let text = wire::deny_text(&denied, &repo_root);
    eprint!("{text}");
    if wire_agy {
        println!(
            "{}",
            serde_json::json!({"decision": "deny", "reason": text})
        );
        return 0;
    }
    super::emit_block(&text)
}

// ── Shell write classification (the tokenizer port) ──────────────────────────

/// Write targets a Bash command binds: redirects, tee/sponge/truncate
/// operands, cp/mv/install destinations, dd of=, in-place sed/perl files,
/// ed/ex files. A `lex` failure (unterminated quote) means no targets - a
/// malformed shell never executes.
fn write_targets(command: &str) -> Vec<String> {
    let Some(tokens) = lex(command) else {
        return Vec::new();
    };
    // Python's re.fullmatch over `[&\d]+`: a token made only of `&` and digits.
    let is_fd = |t: &str| !t.is_empty() && t.bytes().all(|b| b == b'&' || b.is_ascii_digit());
    let bound = |t: &str| {
        matches!(t, ";" | ";;" | "|" | "||" | "&&" | "&" | "(" | ")" | "\n") || t.starts_with('<')
    };
    let flush = |command_name: &Option<String>, pool: &[String], targets: &mut Vec<String>| {
        let Some(command_name) = command_name else {
            return;
        };
        let files: Vec<&str> = pool
            .iter()
            .map(String::as_str)
            .filter(|t| !t.is_empty() && !t.starts_with('-'))
            .collect();
        match command_name.as_str() {
            "tee" | "sponge" | "truncate" | "ed" | "ex" => {
                targets.extend(files.into_iter().map(str::to_string));
            }
            "cp" | "mv" | "install" => {
                if let Some(last) = files.last() {
                    targets.push((*last).to_string());
                }
            }
            "dd" => {
                targets.extend(
                    pool.iter()
                        .filter_map(|t| t.strip_prefix("of="))
                        .map(str::to_string),
                );
            }
            "sed" | "perl" => {
                let inplace = pool
                    .iter()
                    .filter(|t| t.starts_with('-'))
                    .any(|t| is_inplace(t));
                if inplace {
                    if let Some(last) = files.last() {
                        targets.push((*last).to_string());
                    }
                }
            }
            _ => {}
        }
    };
    let mut targets: Vec<String> = Vec::new();
    let mut command_name: Option<String> = None;
    let mut pool: Vec<String> = Vec::new();
    let mut nxt = false;
    let mut val = false;
    let mut at_command = true;
    for tok in &tokens {
        if bound(tok) {
            flush(&command_name, &pool, &mut targets);
            command_name = None;
            pool.clear();
            nxt = false;
            val = false;
            at_command = true;
        } else if nxt {
            nxt = false;
            if !tok.contains('>') && !is_fd(tok) {
                targets.push(tok.clone());
            }
        } else if is_redirect(tok) {
            // `lex` emits redirects as their own tokens (`2>`, `>`, `>>`,
            // `>&`, `&>`, `>|`, `>`!), so every shape here just arms `nxt`
            // and the next word is judged as the redirect target.
            let rest = tok.trim_start_matches(|c: char| c.is_ascii_digit());
            let plain = rest.strip_prefix('&').unwrap_or(rest);
            if rest == "&>" || matches!(plain, ">" | ">>" | ">|" | ">!" | ">&") {
                nxt = true;
            }
        } else if val {
            val = false;
        } else if at_command {
            // The command table only applies in command position, so `install`
            // in `fno config plugin install claude` stays a positional word.
            // An assignment (`B=/path mv a b`) and a wrapper word (sudo, env,
            // command, nohup, time) hand command position to the next word.
            if !is_assignment(tok)
                && !matches!(tok.as_str(), "sudo" | "env" | "command" | "nohup" | "time")
            {
                at_command = false;
                if matches!(
                    tok.as_str(),
                    "tee"
                        | "sponge"
                        | "truncate"
                        | "cp"
                        | "mv"
                        | "install"
                        | "dd"
                        | "sed"
                        | "perl"
                        | "ed"
                        | "ex"
                ) {
                    command_name = Some(tok.clone());
                    pool.clear();
                }
            }
        } else if command_name.is_some() {
            if matches!(tok.as_str(), "-e" | "-f" | "-i") {
                val = true;
            }
            pool.push(tok.clone());
        }
    }
    flush(&command_name, &pool, &mut targets);
    targets.retain(|t| !t.is_empty());
    targets
}

/// Lex a command into words and operator tokens; `None` on an unterminated
/// quote or an unclosed substitution (a malformed shell never executes).
/// Unlike `shlex::split`, unquoted `;` `|` `&` `(` `)` newline and redirects
/// arrive as their own tokens, so `2>&1|tail` can never read as one word.
///
/// `<<DELIM`/`<<-DELIM` heredoc bodies are swallowed whole, never re-lexed as
/// more shell text - unless the reading command is a shell (bash/sh/zsh), in
/// which case the body is lexed again and its tokens spliced in, so `bash
/// <<EOF` still judges a real write in its body, but a heredoc mailed as a
/// file body never donates a phantom write target.
///
/// Each command substitution - `$( )` and backticks, quoted or not - stays
/// one word in place (so `mv $(pick_build x) /tmp/out` still binds
/// `/tmp/out`) and its body is lexed again, appended after a `\n` token so
/// callers read it as one more command.
pub(super) fn lex(command: &str) -> Option<Vec<String>> {
    lex_at_depth(command, 0)
}

/// Substitution nesting past this bound lexes to `None`: a hostile nest
/// never executes, and the recursion must stay bounded.
const MAX_NEST: usize = 128;

fn lex_at_depth(command: &str, nest: usize) -> Option<Vec<String>> {
    let mut bodies: Vec<Vec<String>> = Vec::new();
    let mut toks = lex_until(&mut command.chars().peekable(), false, &mut bodies, nest)?;
    for body in bodies {
        toks.push("\n".to_string());
        toks.extend(body);
    }
    Some(toks)
}

type Src<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Entered just past a glued `(`: the group lexes by the same rules up to
/// its matching `)`. A `$( )` body is recorded as a command; `$(( ))`
/// arithmetic and a bare group such as `arr=(a b)` stay words only.
fn group(
    chars: &mut Src,
    bodies: &mut Vec<Vec<String>>,
    command: bool,
    nest: usize,
) -> Option<String> {
    if nest >= MAX_NEST {
        return None;
    }
    let arith = chars.peek() == Some(&'(');
    let body = lex_until(chars, true, bodies, nest + 1)?;
    let word = format!("({})", body.join(" "));
    if command && !arith {
        bodies.push(body);
    }
    Some(word)
}

/// Entered just past an opening backtick: raw text to the next unescaped
/// backtick, lexed again as its own command.
fn backtick(chars: &mut Src, bodies: &mut Vec<Vec<String>>, nest: usize) -> Option<String> {
    let mut text = String::new();
    loop {
        match chars.next()? {
            '`' => break,
            '\\' if matches!(chars.peek(), Some('`' | '\\' | '$')) => text.push(chars.next()?),
            c => text.push(c),
        }
    }
    bodies.push(lex_at_depth(&text, nest + 1)?);
    Some(format!("`{text}`"))
}

/// The lexer worker: tokens of one command, stopping at the matching `)`
/// when entered inside a substitution. Operator parens opened here nest
/// through `depth`, so a `case` pattern or arithmetic never ends a
/// substitution body early.
fn lex_until(
    chars: &mut Src,
    in_subst: bool,
    bodies: &mut Vec<Vec<String>>,
    nest: usize,
) -> Option<Vec<String>> {
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    // Set right after a `<<`/`<<-` token: (strip_tabs, reader_is_shell), for
    // the newline arm to act on once the delimiter word lands in `toks`.
    let mut heredoc: Option<(bool, bool)> = None;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(&n) = chars.peek() {
                    cur.push(n);
                    chars.next();
                } else {
                    cur.push('\\');
                }
            }
            '\'' => loop {
                match chars.next() {
                    Some('\'') => break,
                    Some(ch) => cur.push(ch),
                    None => return None,
                }
            },
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    // Inside double quotes a backslash keeps its special
                    // meaning only before these four characters.
                    Some('\\') if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                        cur.push(chars.next()?);
                    }
                    // A substitution inside double quotes lexes as its own
                    // command too; its body would otherwise ride inside one
                    // literal word, unread by any caller.
                    Some('$') if chars.peek() == Some(&'(') => {
                        chars.next();
                        cur.push('$');
                        cur.push_str(&group(chars, bodies, true, nest)?);
                    }
                    Some('`') => cur.push_str(&backtick(chars, bodies, nest)?),
                    Some(ch) => cur.push(ch),
                    None => return None,
                }
            },
            ' ' | '\t' | '\r' => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            ';' | '|' | '&' | '\n' => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                if c == '\n' && heredoc.is_some() {
                    let (strip_tabs, reader_is_shell) = heredoc.take().unwrap();
                    let delim = toks.last().cloned().unwrap_or_default();
                    let mut body = String::new();
                    let mut line = String::new();
                    while let Some(bc) = chars.next() {
                        if bc != '\n' {
                            line.push(bc);
                            continue;
                        }
                        let bare = if strip_tabs {
                            line.trim_start_matches('\t')
                        } else {
                            &line[..]
                        };
                        if bare == delim {
                            break;
                        }
                        body.push_str(&line);
                        body.push('\n');
                        line.clear();
                    }
                    if reader_is_shell {
                        if let Some(sub) = lex(&body) {
                            toks.extend(sub);
                        }
                    }
                    toks.push("\n".to_string());
                    continue;
                }
                let mut op = c.to_string();
                while chars.peek() == Some(&c) {
                    op.push(c);
                    chars.next();
                }
                // `&>` redirects both streams; it is not the background `&`.
                if c == '&' && chars.peek() == Some(&'>') {
                    chars.next();
                    op.push('>');
                }
                toks.push(op);
            }
            '(' if !cur.is_empty() => {
                // A glued `(` keeps the word one token (`mv $(pick x)` binds
                // `/tmp/out`); the body lexes as its own command.
                let command = cur.ends_with('$');
                cur.push_str(&group(chars, bodies, command, nest)?);
            }
            ')' if in_subst && depth == 0 => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                return Some(toks);
            }
            '(' | ')' => {
                // Parens are operators at a word boundary; an open group is
                // never glued into a word, so no closer ever rides in one.
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                if c == '(' {
                    depth += 1;
                } else {
                    depth = depth.saturating_sub(1);
                }
                toks.push(c.to_string());
            }
            '`' => cur.push_str(&backtick(chars, bodies, nest)?),
            '<' | '>' => {
                let mut redir = String::new();
                if c == '>' && !cur.is_empty() && cur.bytes().all(|b| b.is_ascii_digit()) {
                    // The digits are the fd prefix of `2>`.
                    redir.push_str(&cur);
                    cur.clear();
                } else if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                redir.push(c);
                while matches!(chars.peek(), Some('>' | '<' | '&' | '|' | '!')) {
                    redir.push(chars.next()?);
                }
                if redir == "<<" && chars.peek() == Some(&'-') {
                    redir.push('-');
                    chars.next();
                }
                // The reader is just the word right before `<<DELIM`: real
                // heredocs are always `bash <<EOF`, never preceded by a
                // redirect target, so the immediate predecessor suffices.
                if redir == "<<" || redir == "<<-" {
                    let shell = toks.last().is_some_and(|t| {
                        matches!(t.rsplit('/').next().unwrap_or(t), "bash" | "sh" | "zsh")
                    });
                    heredoc = Some((redir == "<<-", shell));
                }
                toks.push(redir);
            }
            _ => cur.push(c),
        }
    }
    if in_subst {
        // An unclosed `$( ` never executes, like an unterminated quote.
        return None;
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    Some(toks)
}

/// `^[A-Za-z_][A-Za-z0-9_]*=`: a leading assignment keeps command position,
/// so `B=/path mv a b` is a mv, not a command named `B=/path`.
fn is_assignment(t: &str) -> bool {
    let Some(eq) = t.find('=') else {
        return false;
    };
    let name = &t[..eq];
    let mut ch = name.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `^\d*&?>`: optional leading fd digits, optional `&`, then a redirect.
fn is_redirect(tok: &str) -> bool {
    let rest = tok.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix('&').unwrap_or(rest);
    rest.starts_with('>')
}

/// `--in-place|-[a-zA-Z]*i[a-zA-Z.]*`, fullmatch. The first `i` is the only
/// split a fullmatch can use: a later `i` puts the same bad byte in `head`.
fn is_inplace(t: &str) -> bool {
    if t == "--in-place" {
        return true;
    }
    let Some(rest) = t.strip_prefix('-') else {
        return false;
    };
    let Some(idx) = rest.find('i') else {
        return false;
    };
    let (head, tail) = (&rest[..idx], &rest[idx + 1..]);
    head.bytes().all(|b| b.is_ascii_alphabetic())
        && tail.bytes().all(|b| b.is_ascii_alphabetic() || b == b'.')
}

// ── Session identity ─────────────────────────────────────────────────────────

/// `scripts/lib/postcompact-carrier.sh` port: payload id, transcript basename
/// (without .jsonl), then harness env markers with the same-family rule.
fn resolve_sid(payload_sid: &str, transcript: &str) -> String {
    let mut sid = payload_sid.trim().to_string();
    if sid.is_empty() && !transcript.is_empty() {
        let name = Path::new(transcript)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        sid = name.strip_suffix(".jsonl").unwrap_or(&name).to_string();
    }
    if sid.is_empty() {
        let claude_root = std::env::var("CLAUDE_PLUGIN_ROOT").is_ok_and(|v| !v.is_empty());
        if std::env::var("FNO_PLATFORM").as_deref() == Ok("claude") || claude_root {
            return std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default();
        }
        let mut sid = String::new();
        for candidate in [
            std::env::var("CODEX_THREAD_ID").unwrap_or_default(),
            std::env::var("CLAUDE_CODE_SESSION_ID").unwrap_or_default(),
            std::env::var("CODEX_SESSION_ID").unwrap_or_default(),
        ] {
            if candidate.is_empty() {
                continue;
            }
            if sid.is_empty() {
                sid = candidate;
            } else if sid != candidate {
                return String::new();
            }
        }
        return sid;
    }
    sid
}

// ── Containment ──────────────────────────────────────────────────────────────

/// Lexical normalization (Python's os.path.normpath): collapse //, . and ..
/// without touching the filesystem.
fn normpath(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn abs_join(p: &str, cwd: &Path) -> PathBuf {
    // A leading `~/` is $HOME (bash expands it unquoted); without this the
    // tilde path joined the cwd and read as a repo-relative write.
    let path = crate::org_board::scope::expand_home(p);
    if path.is_absolute() {
        path
    } else {
        let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        cwd.join(path)
    }
}

/// realpath with Python's not-yet-existing-file semantics: canonicalize the
/// deepest EXISTING prefix (so a /var -> /private/var symlink on the root
/// resolves the same way for a file that does not exist yet), then append the
/// non-existent tail.
fn real_of(p: &str, cwd: &Path) -> PathBuf {
    let joined = abs_join(p, cwd);
    if let Ok(real) = std::fs::canonicalize(&joined) {
        return real;
    }
    let (mut prefix, mut tail) = (joined.clone(), Vec::new());
    while let Some(parent) = prefix.parent().filter(|p| !p.as_os_str().is_empty()) {
        tail.push(
            prefix
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_default(),
        );
        prefix = parent.to_path_buf();
        if let Ok(real) = std::fs::canonicalize(&prefix) {
            let mut real = real;
            real.extend(tail.iter().rev().cloned());
            return real;
        }
    }
    normpath(&joined)
}

/// Realpath prefix containment.
fn real_prefix(p: &str, cwd: &Path, root: &Path) -> bool {
    let p = real_of(p, cwd);
    let root = real_of(&root.to_string_lossy(), cwd);
    p == root || p.starts_with(&root)
}

/// `lead.write_roots`: in-repo paths the operator lets a org session write.
/// A bare string is one root. A relative entry joins the repo root, a `~/`
/// entry joins $HOME, and a blank entry is dropped (it would name the repo).
fn write_roots(v: Option<toml::Value>, repo_root: &Path) -> Vec<PathBuf> {
    let raw = match v {
        Some(toml::Value::String(s)) => vec![s],
        Some(toml::Value::Array(a)) => a
            .into_iter()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    raw.iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| repo_root.join(crate::org_board::scope::expand_home(s)))
        .collect()
}

/// The inverted step-10 predicate (2026-09-17 ruling): SOURCE is any path
/// realpath-inside the repo root, with carve-outs for the repo's `.fno`
/// state tree, for build output, and for the operator's `lead.write_roots`
/// entries. The vault is not source; a write outside the repo allows
/// wherever it lands.
fn write_denied(t: &str, cwd: &Path, repo_root: &Path, roots: &[PathBuf]) -> bool {
    real_prefix(t, cwd, repo_root)
        && !real_prefix(t, cwd, &repo_root.join(".fno"))
        && !is_build_output(t, cwd)
        && !roots.iter().any(|r| real_prefix(t, cwd, r))
}

/// A path whose nearest ancestor directory holds a `CACHEDIR.TAG` is build
/// output, not source. Cargo target dirs sit inside the repo and outside
/// `.fno`; matched by the tag file, never by the name `target`, which is
/// also a source directory name (`.claude/rules/worktrees.md`).
fn is_build_output(t: &str, cwd: &Path) -> bool {
    let mut dir = real_of(t, cwd).parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d.join("CACHEDIR.TAG").is_file() {
            return true;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    false
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::lead_guard_wire::{deny_text, patch_targets, transcript_is_open_spawn};
    use super::*;

    fn targets(cmd: &str) -> Vec<String> {
        write_targets(cmd)
    }

    #[test]
    fn redirects_bind_targets() {
        assert_eq!(targets("echo hi > /tmp/a"), vec!["/tmp/a"]);
        assert_eq!(targets("echo hi >> /tmp/a"), vec!["/tmp/a"]);
        assert_eq!(targets("cmd 2> /tmp/err"), vec!["/tmp/err"]);
        assert_eq!(targets("cmd &> /tmp/both"), vec!["/tmp/both"]);
        assert_eq!(targets("cmd &>/tmp/both"), vec!["/tmp/both"]);
        assert_eq!(targets("cmd >& /tmp/both"), vec!["/tmp/both"]);
        assert_eq!(targets("cmd >| /tmp/a"), vec!["/tmp/a"]);
        assert_eq!(targets("cmd >! /tmp/a"), vec!["/tmp/a"]);
        assert_eq!(targets("cmd 2>&1; echo x > /tmp/a"), vec!["/tmp/a"]);
        assert!(targets("echo hi").is_empty(), "no redirect binds nothing");
        assert!(
            targets("cmd 2>&1 | tee").is_empty(),
            "fd dup is not a write"
        );
        // A dup inside a command substitution: `lex` glues the closers
        // into the word while `$( ` is open, so `1)` must still read as fd.
        assert!(
            targets("N=$(cmd 2>&1)").is_empty(),
            "subst dup is not a write"
        );
        assert!(
            targets("N=`cmd 2>&1`").is_empty(),
            "backtick dup is not a write"
        );
        assert!(
            targets("N=$(a $(b 2>&1))").is_empty(),
            "nested subst dup is not a write"
        );
        assert!(
            targets("N=$(cmd >&2)").is_empty(),
            "reversed dup is not a write"
        );
        assert_eq!(
            targets("X=$(cmd > out.txt)"),
            vec!["out.txt"],
            "a real write inside a substitution still binds"
        );
        assert_eq!(
            targets("N=$(cmd | tee out.txt)"),
            vec!["out.txt"],
            "tee inside a substitution still binds"
        );
        assert_eq!(
            targets("printf x > '1)'"),
            vec!["1)"],
            "a quoted literal closer is part of the path"
        );
        // Quoted "a > b" is one word, not an operator.
        assert!(targets("echo \"a > b\"").is_empty());
    }

    #[test]
    fn command_operands_bind() {
        // `<` now lexes as a read operator, so the stdin file is no
        // longer bound as a tee write (deliberate narrowing of the old
        // over-broad rule; the real writes still bind).
        assert_eq!(
            targets("tee /tmp/x /tmp/y < /tmp/in"),
            vec!["/tmp/x", "/tmp/y"]
        );
        // `-s`'s value is treated as a file too (the shell policy never
        // modelled option values; over-broad, fail-closed).
        assert_eq!(targets("truncate -s 0 /tmp/x"), vec!["0", "/tmp/x"]);
        assert_eq!(
            targets("cp /tmp/a /tmp/b"),
            vec!["/tmp/b"],
            "only the destination writes"
        );
        assert_eq!(targets("mv a b && mv c d"), vec!["b", "d"]);
        assert_eq!(targets("dd if=/x of=/tmp/y"), vec!["/tmp/y"]);
        assert_eq!(targets("sed -i 's/a/b/' /tmp/f"), vec!["/tmp/f"]);
        assert_eq!(
            targets("sed 's/a/b/' /tmp/f"),
            Vec::<String>::new(),
            "no -i, no write"
        );
        assert_eq!(targets("perl -i -pe s/a/b/ /tmp/f"), vec!["/tmp/f"]);
        assert_eq!(targets("printf x | sponge /tmp/f"), vec!["/tmp/f"]);
        assert_eq!(targets("printf x | ed /tmp/f"), vec!["/tmp/f"]);
        assert_eq!(targets("printf x | ex /tmp/f"), vec!["/tmp/f"]);
    }

    /// A cwd-only helper for the heredoc specimens below: session cwd is
    /// already the repo root, so a relative path resolves inside it exactly
    /// as a real teamed session would see it.
    fn bash_allowed_in(repo: &Path, cmd: &str) -> bool {
        targets(cmd)
            .iter()
            .all(|t| !write_denied(t, repo, repo, &[]))
    }

    #[test]
    fn shell_reading_heredoc_body_still_refuses_a_real_write() {
        // `bash <<'EOF'` DOES read its stdin as commands, so a real write
        // inside that body still refuses.
        let repo = std::env::temp_dir().join(format!("kgd-heredoc-shell-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&repo);
        let cmd = "bash <<'EOF'\ncat > crates/fno-agents/src/lib.rs\nEOF";
        assert!(!bash_allowed_in(&repo, cmd));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn command_position_still_binds_real_writes() {
        // A genuine redirect binds, today and after (positive control).
        assert_eq!(
            targets("echo hi > cli/src/fno/x.py"),
            vec!["cli/src/fno/x.py"]
        );
        // A real `install` in command position binds its destination.
        assert_eq!(targets("install -d /tmp/x"), vec!["/tmp/x"]);
        // The keep words hand command position to the write command.
        assert_eq!(targets("sudo mv a /tmp/b"), vec!["/tmp/b"]);
        assert_eq!(targets("command mv a /tmp/b"), vec!["/tmp/b"]);
        assert_eq!(targets("env FOO=bar cp /tmp/a /tmp/b"), vec!["/tmp/b"]);
        assert_eq!(targets("B=/path mv a b"), vec!["b"]);
        // A codex apply_patch body binds exactly its header paths: the four
        // prefixes hooks/lib/write-targets.sh reads, CR stripped, an empty
        // path dropped.
        assert_eq!(
            patch_targets(concat!(
                "*** Begin Patch\n",
                "*** Update File: crates/fno-agents/src/lib.rs\n",
                "+fn x() {}\n",
                "*** Add File: docs/new.md\r\n",
                "+hi\n",
                "*** Delete File: docs/old.md\n",
                "*** Move to: docs/renamed.md\n",
                "*** End Patch\n",
            )),
            vec![
                "crates/fno-agents/src/lib.rs",
                "docs/new.md",
                "docs/old.md",
                "docs/renamed.md",
            ]
        );
        assert!(
            patch_targets("no headers here\n*** Begin Patch\n*** End Patch").is_empty(),
            "a patch with no file headers binds nothing"
        );
    }

    #[test]
    fn substitution_bodies_bind_their_writes() {
        // Every substitution spelling binds exactly its one write target:
        // the body lexes as its own command, the outer word keeps its place.
        assert_eq!(targets("N=$(cp a b)"), vec!["b"]);
        assert_eq!(targets("$(cp a b)"), vec!["b"]);
        assert_eq!(targets("echo $(cp a b)"), vec!["b"]);
        assert_eq!(targets("echo \"$(cp a b)\""), vec!["b"]);
        assert_eq!(targets("\"$(cmd > out)\""), vec!["out"]);
        assert_eq!(targets("N=`cp a b`"), vec!["b"]);
        assert_eq!(targets("echo \"`cmd > out`\""), vec!["out"]);
        assert_eq!(targets("N=$(echo $(cp a b))"), vec!["b"]);
        assert_eq!(targets("echo \"$(printf \"%s\" x > out)\""), vec!["out"]);
    }

    #[test]
    fn substitution_lookalikes_bind_nothing() {
        // Quoted text is inert; arithmetic has no redirect; a heredoc body
        // read by a non-shell never donates a target; an unclosed `$( `
        // never executes.
        assert!(targets("echo '$(cp a b)'").is_empty());
        assert!(targets("echo \"a > b\"").is_empty());
        assert!(targets("N=$(git status 2>&1)").is_empty());
        assert!(targets("echo \"$((a > b))\"").is_empty());
        assert!(targets("echo $((a > b))").is_empty());
        assert!(targets("git commit -m \"$(cat <<'EOF'\nfix: a > b, cp x y\nEOF\n)\"").is_empty());
        assert!(targets("N=$(cat <<'EOF'\n1) cp x y\nEOF\n)").is_empty());
        assert!(
            targets("N=$(cp a b").is_empty(),
            "unclosed substitution never executes"
        );
        // A relative patch header resolves against the payload cwd exactly
        // like a Bash target: judged through the same allowed() closure, so
        // `docs/x.md` inside the repo denies and an outside cwd allows. The
        // deny names the first denied path, in header order.
        let repo = std::env::temp_dir().join(format!("kgd-patch-cwd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&repo);
        let patch = "*** Begin Patch\n*** Update File: crates/fno-agents/src/lib.rs\n+fn y() {}\n*** Update File: docs/first.md\n+x\n*** End Patch\n";
        let first_denied = |cwd: &Path| -> Option<String> {
            patch_targets(patch)
                .iter()
                .find(|t| write_denied(t, cwd, &repo, &[]))
                .map(|t| t.to_string())
        };
        assert_eq!(
            first_denied(&repo).as_deref(),
            Some("crates/fno-agents/src/lib.rs"),
            "the first in-repo header path denies"
        );
        assert!(
            first_denied(&std::env::temp_dir()).is_none(),
            "the same patch from an outside cwd allows"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Characterization fixtures for `transcript_is_open_spawn`: plant a
    /// transcript whose tail matches the named shape.
    fn plant_transcript(tag: &str, lines: &[String]) -> String {
        let dir = std::env::temp_dir().join(format!("kgd-spawn-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path.to_string_lossy().into_owned()
    }

    fn spawn_line(id: &str, name: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}]}}}}"#
        )
    }

    // (a) The newest tool_use is an unanswered Task call: the limb is allowed.
    #[test]
    fn open_task_at_the_tail_is_an_open_spawn() {
        let path = plant_transcript(
            "open-task",
            &[spawn_line("t1", "Bash"), spawn_line("t2", "Task")],
        );
        assert!(transcript_is_open_spawn(&path));
    }

    // (e) A tail that is not valid UTF-8 is unreadable evidence: fail closed
    // the way the deleted private leg did, never a repaired guess.
    #[test]
    fn corrupt_tail_fails_closed() {
        let dir = std::env::temp_dir().join(format!("kgd-spawn-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut body = spawn_line("t1", "Task").into_bytes();
        body.push(0xFF);
        std::fs::write(&path, body).unwrap();
        assert!(!transcript_is_open_spawn(&path.to_string_lossy()));
    }

    /// Fixture: a repo checkout plus a vault OUTSIDE it; the internal
    /// symlink is what a vault-backed checkout looks like on disk.
    struct Roots {
        base: PathBuf,
        repo: PathBuf,
    }

    fn roots(tag: &str) -> Roots {
        let base = std::env::temp_dir().join(format!("kgd-{tag}-{}", std::process::id()));
        Roots {
            repo: base.join("repo"),
            base,
        }
    }

    fn allowed(r: &Roots, target: &Path) -> bool {
        !write_denied(&target.to_string_lossy(), &r.base, &r.repo, &[])
    }

    #[test]
    fn source_write_is_denied() {
        // Required test 2: the guard exists to stop a lead writing SOURCE.
        let r = roots("source");
        assert!(!allowed(&r, &r.repo.join("cli/src/fno/anything.py")));
        assert!(!allowed(&r, &r.repo.join("crates/fno-agents/src/lib.rs")));
        // Required test 4 folded in (no vault configured anywhere): the
        // predicate only knows the repo root, so with `&[]` anything
        // outside the repo allows.
        assert!(allowed(&r, &r.base.join("anywhere/else/foo.md")));
        let _ = std::fs::remove_dir_all(&r.base);
    }

    #[test]
    fn refusal_names_the_rule_in_two_lines() {
        let text = deny_text("cli/src/fno/x.py", Path::new("/repo"));
        assert_eq!(text.lines().count(), 2, "two lines: {text:?}");
        assert!(text.contains("operates the machine and does not author it"));
        assert!(text.contains("fno config plugin install"));
        assert!(text.contains("Delegate the edit or escalate"));
        assert!(text.contains("config.lead.write_roots"));
    }

    #[test]
    fn listed_write_root_allows_its_subtree_only() {
        // An operator-named root allows its realpath subtree and nothing
        // else: docs allows docs/guide.md, not docsx/ and not cli/src.
        let r = roots("writeroots");
        let _ = std::fs::create_dir_all(r.repo.join("docs"));
        let listed = write_roots(
            Some(toml::Value::Array(vec![toml::Value::from("docs")])),
            &r.repo,
        );
        assert!(!write_denied(
            &r.repo.join("docs/guide.md").to_string_lossy(),
            &r.base,
            &r.repo,
            &listed
        ));
        assert!(write_denied(
            &r.repo.join("docs/guide.md").to_string_lossy(),
            &r.base,
            &r.repo,
            &[]
        ));
        assert!(write_denied(
            &r.repo.join("docsx/guide.md").to_string_lossy(),
            &r.base,
            &r.repo,
            &listed
        ));
        assert!(write_denied(
            &r.repo.join("cli/src/fno/x.py").to_string_lossy(),
            &r.base,
            &r.repo,
            &listed
        ));
        assert!(allowed(&r, &r.base.join("elsewhere/x.md")));
        let _ = std::fs::remove_dir_all(&r.base);
    }
}

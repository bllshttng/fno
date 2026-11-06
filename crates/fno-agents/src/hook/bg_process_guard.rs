//! `fno-agents hook bg-process-guard` - refuse a Bash command that can never
//! end on its own and carries no time bound, and a background Bash task that
//! arrives without a supported wall-clock bound.
//!
//! On 2026-08-13 one session left 73 `yes > /dev/null` processes at PPID 1,
//! 797% CPU combined, 71 core-hours over 7.5 hours. `yes` writing to
//! /dev/null never receives SIGPIPE - there is no reader to go away and the
//! sink always accepts - so the normal death path for an abandoned pipeline
//! does not exist. See docs/architecture/background-process-hygiene.md.
//!
//! This is the port of the retired Python guard, token for token: the lexer
//! below reproduces `shlex` in posix mode with `punctuation_chars`, because
//! the command-position walk was hardened against that tokenization (a
//! `);` is ONE token, `((;;));` is ONE token, `2>&1` is not a separator).
//! The shared `lead_guard::lex` splits operators differently and would
//! reopen holes the guard's corpus closed.
//!
//! Parse-only. Unbalanced quotes and any unreadable shape allow, except a
//! background task whose bound cannot be verified, which is refused.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use super::test_run_guard::wrapper_takes_value;

/// Characters the lexer accumulates into one operator token.
const PUNCT: &str = "();<>|&\n";
/// The ones that END a command. `|` is absent: `yes | head -c 1M` is ONE
/// pipeline and the `head` bounds it.
const CONTROL: &str = ";&\n";

/// Wrappers transparent to command position: `nohup yes` still runs `yes`.
/// `timeout` is absent on purpose: here it is a bound, not a wrapper.
const TRANSPARENT: &[&str] = &[
    "nohup",
    "setsid",
    "exec",
    "time",
    "env",
    "command",
    "builtin",
    "nice",
    "ionice",
    "taskpolicy",
    "stdbuf",
    "caffeinate",
    "sudo",
    "do",
    "then",
    "else",
    "elif",
    "{",
    "(",
    "!",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

const REPLACEMENT_HELPER: &str = "bash scripts/lib/loadgen.sh start <label> <seconds> [count]";
const REPLACEMENT: &str =
    "timeout 300 bash -c 'exec -a fno-load-<node-or-session> yes > /dev/null'";

/// Entry: read the payload once, judge, print, always exit 0.
pub fn run(_args: &[String]) -> i32 {
    let payload: Value = serde_json::from_str(super::read_stdin().trim()).unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    if !crate::agents_config::guard_enabled(&cwd, "bg-process") {
        return super::emit_allow();
    }
    let refusal = judge(&payload);
    super::emit_guard_decision(&cwd, "bg-process-guard", "Bash", refusal.is_some());
    match refusal {
        Some(reason) => super::emit_block(&reason),
        None => super::emit_allow(),
    }
}

/// The whole verdict for one payload: a refusal, or None to allow. A null
/// payload, a non-Bash tool and a blank command allow.
pub(super) fn judge(payload: &Value) -> Option<String> {
    if payload.get("tool_name").and_then(Value::as_str) != Some("Bash") {
        return None;
    }
    let tool_input = payload.get("tool_input")?;
    let command = tool_input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())?;
    if tool_input.get("run_in_background") == Some(&Value::Bool(true))
        && !has_background_bound(command, 0)
    {
        return Some(background_refusal(command));
    }
    // Denied whether or not run_in_background is set: a foreground unbounded
    // `yes` is orphaned just as surely when the session exits.
    decide(command)
}

/// The whole verdict for one command string: a refusal, or None to allow.
/// Unbalanced quotes cannot tell command position, so they allow.
fn decide(command: &str) -> Option<String> {
    match find_unbounded(command, false, 0) {
        Ok(Some((reason, segment))) => Some(refusal(&reason, &segment)),
        Ok(None) | Err(Unreadable) => None,
    }
}

/// The input could not be tokenized (an open quote or a trailing escape).
struct Unreadable;

// ─── lexer ──────────────────────────────────────────────────────────────

fn is_punct(c: char) -> bool {
    PUNCT.contains(c)
}

/// Shell tokens with operators kept as their own tokens: `shlex` in posix
/// mode with `punctuation_chars="();<>|&\n"`, whitespace `" \t\r"`, word
/// splitting on whitespace only, and no comment character. A newline is
/// punctuation, so it ends a command the way `;` does and a newline inside a
/// quoted string stays part of that string. `None` on an open quote or a
/// trailing escape, which the caller turns into an allow.
pub(super) fn shlex_tokens(text: &str) -> Option<Vec<String>> {
    shlex_lex(
        text,
        Lex {
            punct: true,
            comments: false,
        },
    )
}

/// The two `shlex` knobs the guards disagree on. `punct: false` is
/// `shlex.split` (a newline is whitespace, operators stay inside words);
/// `comments: true` makes an unquoted `#` drop the rest of the line.
#[derive(Clone, Copy)]
pub(super) struct Lex {
    pub punct: bool,
    pub comments: bool,
}

/// The one shell lexer behind [`shlex_tokens`], with its knobs exposed.
pub(super) fn shlex_lex(text: &str, opts: Lex) -> Option<Vec<String>> {
    #[derive(Clone, Copy)]
    enum State {
        Word,
        Single,
        Double,
        Escape { in_double: bool },
    }
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\r') || (!opts.punct && c == '\n');
    let is_punct = |c: char| opts.punct && is_punct(c);
    let chars: Vec<char> = text.chars().collect();
    let skip_comment = |mut i: usize| {
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        i
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_space(c) {
            i += 1;
            continue;
        }
        if opts.comments && c == '#' {
            i = skip_comment(i);
            continue;
        }
        if is_punct(c) {
            let mut token = String::new();
            while i < chars.len() && is_punct(chars[i]) {
                token.push(chars[i]);
                i += 1;
            }
            out.push(token);
            continue;
        }
        let mut token = String::new();
        let mut quoted = false;
        let mut state = State::Word;
        loop {
            match state {
                State::Word => match chars.get(i).copied() {
                    None => break,
                    Some(ch) if is_space(ch) => {
                        i += 1;
                        break;
                    }
                    Some('#') if opts.comments => {
                        i = skip_comment(i);
                        break;
                    }
                    Some(ch) if is_punct(ch) => break,
                    Some('\'') => {
                        quoted = true;
                        state = State::Single;
                        i += 1;
                    }
                    Some('"') => {
                        quoted = true;
                        state = State::Double;
                        i += 1;
                    }
                    Some('\\') => {
                        state = State::Escape { in_double: false };
                        i += 1;
                    }
                    Some(ch) => {
                        token.push(ch);
                        i += 1;
                    }
                },
                State::Single => match chars.get(i).copied()? {
                    '\'' => {
                        state = State::Word;
                        i += 1;
                    }
                    ch => {
                        token.push(ch);
                        i += 1;
                    }
                },
                State::Double => match chars.get(i).copied()? {
                    '"' => {
                        state = State::Word;
                        i += 1;
                    }
                    '\\' => {
                        state = State::Escape { in_double: true };
                        i += 1;
                    }
                    ch => {
                        token.push(ch);
                        i += 1;
                    }
                },
                State::Escape { in_double } => {
                    let ch = chars.get(i).copied()?;
                    // Inside double quotes only the quote and the escape
                    // itself are escapable; any other char keeps its slash.
                    if in_double && ch != '\\' && ch != '"' {
                        token.push('\\');
                    }
                    token.push(ch);
                    i += 1;
                    state = if in_double {
                        State::Double
                    } else {
                        State::Word
                    };
                }
            }
        }
        if !token.is_empty() || quoted {
            out.push(token);
        }
    }
    Some(out)
}

/// True when this token ends one command and starts the next.
///
/// Membership in a fixed set is not enough: the lexer accumulates ADJACENT
/// punctuation into one token, so `x=$(echo 1); yes` yields `);`.
pub(super) fn is_separator(tok: &str) -> bool {
    if tok.is_empty() || !tok.chars().all(is_punct) {
        return false;
    }
    // A `&` next to a redirect arrow is part of the redirect (`2>&1`), and
    // `|&` is bash's pipe-including-stderr: a pipe, not a control operator.
    if tok.contains('>') || tok.contains('<') || tok == "|&" {
        return false;
    }
    // A bare `)` opens a `case` arm's body or closes a subshell: command
    // position restarts there.
    if tok == ")" {
        return true;
    }
    tok.chars().any(|c| CONTROL.contains(c)) || tok.contains("||")
}

/// Split a token list on command separators; each segment carries its start
/// index in `tokens`.
fn segment_spans(tokens: &[String]) -> Vec<(Vec<String>, usize)> {
    let mut out = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut start = 0;
    for (idx, tok) in tokens.iter().enumerate() {
        if is_separator(tok) {
            if !current.is_empty() {
                out.push((std::mem::take(&mut current), start));
            }
            start = idx + 1;
        } else {
            if current.is_empty() {
                start = idx;
            }
            current.push(tok.clone());
        }
    }
    if !current.is_empty() {
        out.push((current, start));
    }
    out
}

fn is_pipe(tok: &str) -> bool {
    tok == "|" || tok == "|&"
}

/// The commands of one pipeline, each with its absolute token index. `split`
/// is false inside a `case` region, where `|` separates patterns.
fn pipe_spans(segment: &[String], start: usize, split: bool) -> Vec<(Vec<String>, usize)> {
    if !split {
        return if segment.is_empty() {
            Vec::new()
        } else {
            vec![(segment.to_vec(), start)]
        };
    }
    let mut parts = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut cur_start = start;
    for (offset, tok) in segment.iter().enumerate() {
        if is_pipe(tok) {
            if !current.is_empty() {
                parts.push((std::mem::take(&mut current), cur_start));
            }
            cur_start = start + offset + 1;
        } else {
            if current.is_empty() {
                cur_start = start + offset;
            }
            current.push(tok.clone());
        }
    }
    if !current.is_empty() {
        parts.push((current, cur_start));
    }
    parts
}

/// `[A-Za-z_][A-Za-z0-9_]*=.*` full match: a leading assignment prefix.
fn is_assignment(tok: &str) -> bool {
    let Some((name, value)) = tok.split_once('=') else {
        return false;
    };
    let mut ch = name.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !value.contains('\n')
}

fn base_of(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// The command-position token of `segment` plus its remaining argv. Walks
/// past transparent wrappers, their flag values and assignment prefixes.
/// `greedy` reads an UNLISTED wrapper flag as value-taking; the caller reads
/// both, because neither reading is safe alone.
fn head_of(segment: &[String], greedy: bool) -> (Option<String>, Vec<String>) {
    let mut i = 0;
    let mut wrapper = "";
    while i < segment.len() {
        let tok = segment[i].as_str();
        // A leading all-punctuation token is shell grammar, not a command.
        if !tok.is_empty() && tok.chars().all(is_punct) {
            i += 1;
            continue;
        }
        let base = base_of(tok);
        if TRANSPARENT.contains(&base) {
            wrapper = base;
            i += 1;
            continue;
        }
        if is_assignment(tok) {
            i += 1;
            continue;
        }
        if !wrapper.is_empty() && (tok == "-v" || tok == "-V") {
            // `command -v yes` prints a path, it runs nothing.
            return (None, Vec::new());
        }
        if !wrapper.is_empty() && tok.starts_with('-') {
            let takes_value = greedy || wrapper_takes_value(wrapper, tok);
            i += if takes_value && i + 1 < segment.len() {
                2
            } else {
                1
            };
            continue;
        }
        return (Some(base.to_string()), segment[i + 1..].to_vec());
    }
    (None, Vec::new())
}

/// `\d+[smhdSMHD]?` full match.
fn is_duration(s: &str) -> bool {
    let body = s.strip_suffix(|c| "smhdSMHD".contains(c)).unwrap_or(s);
    !body.is_empty() && body.chars().all(|c| c.is_ascii_digit())
}

/// True when this segment carries something that makes the process end. Read
/// per segment and in COMMAND POSITION: a `timeout` argument or a trailing
/// comment naming it bounds nothing.
fn has_bound(segment: &[String]) -> bool {
    let (Some(head), argv) = head_of(segment, false) else {
        return false;
    };
    if head == "timeout" || head == "gtimeout" {
        return true;
    }
    if head == "stress" || head == "stress-ng" {
        for (i, tok) in argv.iter().enumerate() {
            if (tok == "-t" || tok == "--timeout")
                && argv.get(i + 1).is_some_and(|v| is_duration(v))
            {
                return true;
            }
            if tok.strip_prefix("-t").is_some_and(is_duration)
                || tok.strip_prefix("--timeout=").is_some_and(is_duration)
            {
                return true;
            }
        }
    }
    // `count=` is dd's own operand: safe to read positionally.
    head == "dd" && operands(&argv).iter().any(|a| a.starts_with("count="))
}

/// The script text a shell was handed with `-c`. Short options bundle:
/// `bash -lc '...'` is the same call.
fn payload_of(head: &str, argv: &[String]) -> Option<String> {
    if !SHELLS.contains(&head) {
        return None;
    }
    argv.iter().enumerate().find_map(|(idx, tok)| {
        let is_c_bundle = tok.len() >= 2
            && tok.starts_with('-')
            && tok.ends_with('c')
            && tok[1..tok.len() - 1]
                .chars()
                .all(|c| c.is_ascii_alphabetic());
        if is_c_bundle {
            argv.get(idx + 1).cloned()
        } else {
            None
        }
    })
}

/// `argv` without redirect operators and the filenames they name. WRITING to
/// an endless device is ordinary; only READING from one never ends, so `>`
/// targets drop and `<` sources stay as operands.
fn operands(argv: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, tok) in argv.iter().enumerate() {
        if tok.contains('>') {
            continue;
        }
        if i > 0 && argv[i - 1].contains('>') {
            continue;
        }
        if tok.contains('<') {
            let stripped = tok.rsplit('<').next().unwrap_or("");
            if !stripped.is_empty() {
                out.push(stripped.to_string());
            }
            continue;
        }
        out.push(tok.clone());
    }
    out
}

// ─── heredocs ───────────────────────────────────────────────────────────

/// The delimiter of the first `<<-?\s*(["']?)IDENT\1` in `line`, if any.
fn heredoc_delim(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let ident_start = |c: char| c.is_ascii_alphabetic() || c == '_';
    for p in 0..chars.len().saturating_sub(1) {
        if chars[p] != '<' || chars[p + 1] != '<' {
            continue;
        }
        let mut q = p + 2;
        if chars.get(q) == Some(&'-') {
            q += 1;
        }
        while chars.get(q).is_some_and(|c| c.is_whitespace()) {
            q += 1;
        }
        let quote = chars.get(q).copied().filter(|c| matches!(c, '"' | '\''));
        if quote.is_some() {
            q += 1;
        }
        if !chars.get(q).copied().is_some_and(ident_start) {
            continue;
        }
        let mut end = q;
        while chars
            .get(end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
        {
            end += 1;
        }
        if quote.is_some() && chars.get(end).copied() != quote {
            continue;
        }
        return Some(chars[q..end].iter().collect());
    }
    None
}

/// Drop heredoc BODIES: data written to a file, not commands. Strips only
/// when the terminator is found, so a `<<` that was really a quoted string
/// or a shift cannot swallow the rest of the command.
pub(super) fn strip_heredocs(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line);
        i += 1;
        let Some(delim) = heredoc_delim(line) else {
            continue;
        };
        let mut end = i;
        while end < lines.len() && lines[end].trim() != delim {
            end += 1;
        }
        if end < lines.len() {
            i = end + 1;
        }
    }
    out.join("\n")
}

// ─── background bound ───────────────────────────────────────────────────

/// Whether a background command carries a supported wall bound. A shell `-c`
/// payload counts only when its whole text is bounded; `with_timeout` counts
/// only when its helper was sourced in the same command text.
fn has_background_bound(text: &str, depth: usize) -> bool {
    if depth > 2 {
        return false;
    }
    let Some(tokens) = shlex_tokens(&strip_heredocs(text)) else {
        return false;
    };
    let segments: Vec<Vec<String>> = segment_spans(&tokens).into_iter().map(|(s, _)| s).collect();
    let piped = |segment: &Vec<String>| segment.iter().any(|t| is_pipe(t));

    if segments.len() == 1 {
        let segment = &segments[0];
        let (head, argv) = head_of(segment, false);
        if let Some(payload) = head.as_deref().and_then(|h| payload_of(h, &argv)) {
            if !payload.is_empty() {
                return has_background_bound(&payload, depth + 1);
            }
        }
        if piped(segment) {
            return false;
        }
        if head.as_deref() == Some("fno") && argv.len() >= 3 && argv[..3] == ["do", "pr", "wait"] {
            return argv.iter().enumerate().any(|(index, arg)| {
                (arg == "--timeout" && argv.get(index + 1).is_some_and(|v| is_duration(v)))
                    || arg.strip_prefix("--timeout=").is_some_and(is_duration)
            });
        }
        return false;
    }

    if segments.len() != 2 || segments.iter().any(piped) {
        return false;
    }
    let (source_head, source_argv) = head_of(&segments[0], false);
    let (call_head, call_argv) = head_of(&segments[1], false);
    let sourced_helper = matches!(source_head.as_deref(), Some("source" | "."))
        && source_argv
            .iter()
            .any(|a| a.ends_with("scripts/lib/with-timeout.sh"));
    let bounded_call = call_head.as_deref() == Some("with_timeout")
        && call_argv
            .first()
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
    sourced_helper && bounded_call
}

fn background_refusal(command: &str) -> String {
    format!(
        "Refusing a background Bash task without a recognized hard timeout.\n\n  {command}\n\nUse the portable helper:\n  bash -c 'source scripts/lib/with-timeout.sh; with_timeout 1800 <command> [args...]'\nor a native bounded wait such as `fno do pr wait <N> --timeout 15m`."
    )
}

// ─── unbounded generators ───────────────────────────────────────────────

/// Why this segment can never end, or None when it can.
fn generator_reason(head: &str, argv: &[String]) -> Option<String> {
    if head == "yes" {
        return Some(
            "`yes` never ends. Writing to /dev/null it never even receives SIGPIPE, because there is no reader to go away and the sink always accepts."
                .to_string(),
        );
    }
    // The truth table INVERTS between the keywords: `while` repeats while
    // the test succeeds, `until` repeats while it fails.
    let never_ends: &[&str] = match head {
        "while" => &["true", ":"],
        "until" => &["false"],
        _ => &[],
    };
    if let Some(first) = argv.first() {
        if never_ends.contains(&first.as_str()) {
            return Some(format!("`{head} {first}` is an unbounded loop header."));
        }
    }
    if head == "sleep" && matches!(argv.first().map(String::as_str), Some("infinity" | "inf")) {
        return Some("`sleep infinity` never returns.".to_string());
    }
    if head == "stress" || head == "stress-ng" {
        return Some(format!("`{head}` runs until killed unless given `-t`."));
    }
    let operands = operands(argv);
    if head == "dd"
        && operands
            .iter()
            .any(|a| a == "if=/dev/zero" || a == "if=/dev/urandom")
    {
        return Some("`dd` from an endless device never reaches EOF without `count=`.".to_string());
    }
    if matches!(
        head,
        "cat" | "sha256sum" | "shasum" | "md5" | "md5sum" | "base64" | "wc"
    ) && operands
        .iter()
        .any(|a| matches!(a.as_str(), "/dev/zero" | "/dev/urandom" | "/dev/random"))
    {
        return Some("reading an endless device never reaches EOF.".to_string());
    }
    None
}

/// One bool per segment: is it inside a `case ... esac`? Read in COMMAND
/// POSITION, so a comment carrying the word `case` opens nothing.
fn case_regions(segments: &[Vec<String>]) -> Vec<bool> {
    let mut out = Vec::new();
    let mut depth: usize = 0;
    for segment in segments {
        let head = head_of(segment, false).0;
        let opens = usize::from(head.as_deref() == Some("case"));
        let closes = usize::from(head.as_deref() == Some("esac"));
        out.push(depth > 0 || opens > 0);
        depth = (depth + opens).saturating_sub(closes);
    }
    out
}

/// Can the `for` at `for_index` never end? Bash reads three `;`-separated
/// arithmetic expressions and the MIDDLE one is the condition; an empty one
/// never ends. The header is rebuilt from tokens (it is punctuation, so the
/// segment walk dropped it), and only when the arithmetic starts right after
/// `for` and no token in it carries whitespace.
fn arith_endless(tokens: &[String], for_index: usize) -> bool {
    let rest = &tokens[(for_index + 1).min(tokens.len())..];
    if !rest.first().is_some_and(|t| t.starts_with("((")) {
        return false;
    }
    let mut joined = String::new();
    for nxt in rest {
        if nxt == "do" || nxt.chars().any(char::is_whitespace) {
            break;
        }
        joined.push_str(nxt);
        if nxt.contains("))") {
            break;
        }
    }
    let Some(end) = joined.get(2..).and_then(|h| h.find("))")).map(|p| p + 2) else {
        return false;
    };
    let parts: Vec<&str> = joined[2..end].split(';').collect();
    parts.len() == 3 && parts[1].trim().is_empty()
}

struct OpenLoop {
    idx: usize,
    endless: bool,
    escaped: bool,
}

/// First (reason, segment text) that can never end and is unbounded.
/// Recurses into `bash -c '...'` payloads, carrying the enclosing bound down:
/// `timeout 300 bash -c 'yes'` IS bounded, and the bound lives outside the
/// payload it bounds.
fn find_unbounded(
    text: &str,
    mut inherited_bound: bool,
    depth: usize,
) -> Result<Option<(String, String)>, Unreadable> {
    if depth > 2 {
        return Ok(None);
    }
    let all_tokens = shlex_tokens(&strip_heredocs(text)).ok_or(Unreadable)?;
    let spans = segment_spans(&all_tokens);
    let all_segments: Vec<Vec<String>> = spans.iter().map(|(s, _)| s.clone()).collect();
    let in_case = case_regions(&all_segments);
    // (head, absolute token index of its `for`) per pipe stage, per segment.
    // The index lets a `for` read its OWN arithmetic header instead of being
    // paired to one by counting.
    let heads: Vec<Vec<(Option<String>, usize)>> = spans
        .iter()
        .zip(&in_case)
        .map(|((segment, seg_start), cased)| {
            pipe_spans(segment, *seg_start, !*cased)
                .into_iter()
                .map(|(part, part_start)| {
                    let at = part.iter().position(|t| t == "for").unwrap_or(0);
                    (head_of(&part, false).0, part_start + at)
                })
                .collect()
        })
        .collect();

    // An escape belongs to the loop it sits INSIDE, which needs a stack.
    let mut escaped_at: HashMap<usize, bool> = HashMap::new();
    let mut endless_for: Vec<usize> = Vec::new();
    let mut stack: Vec<OpenLoop> = Vec::new();
    for (idx, hs) in heads.iter().enumerate() {
        for (head, for_index) in hs {
            match head.as_deref() {
                Some("while" | "until" | "for") => {
                    let endless =
                        head.as_deref() == Some("for") && arith_endless(&all_tokens, *for_index);
                    stack.push(OpenLoop {
                        idx,
                        endless,
                        escaped: false,
                    });
                }
                Some("done") if !stack.is_empty() => {
                    let closed = stack.pop().expect("stack checked non-empty");
                    escaped_at.insert(closed.idx, closed.escaped);
                    if closed.endless && !closed.escaped {
                        endless_for.push(closed.idx);
                    }
                }
                Some("break" | "exit" | "return") => {
                    for open_loop in &mut stack {
                        open_loop.escaped = true;
                    }
                }
                _ => {}
            }
        }
    }
    // A loop with no `done` is malformed; read it as escaped rather than refuse.
    for open_loop in &stack {
        escaped_at.insert(open_loop.idx, true);
    }
    // A bound anywhere in the command clears an endless `for` header.
    let command_bounded = inherited_bound || all_segments.iter().any(|s| has_bound(s));
    if let Some(first) = endless_for.first() {
        if !command_bounded {
            return Ok(Some((
                "`for ((;;))` is an unbounded loop header.".to_string(),
                all_segments[*first].join(" "),
            )));
        }
    }
    for (seg_index, segment) in all_segments.iter().enumerate() {
        let parts: Vec<Vec<String>> = pipe_spans(segment, 0, !in_case[seg_index])
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        for (idx, part) in parts.iter().enumerate() {
            // A downstream reader bounds the stage feeding it, read PER
            // STAGE; the LAST stage has no reader, so the pipe bounds nothing
            // for it.
            let bounded = inherited_bound || has_bound(part) || idx + 1 < parts.len();
            let (Some(head), argv) = head_of(part, false) else {
                continue;
            };
            if head == "ulimit" {
                // `ulimit -t` applies to the whole shell, so it can only sit
                // in an EARLIER segment than the process it bounds.
                if argv.iter().any(|a| a == "-t") {
                    inherited_bound = true;
                }
                continue;
            }
            if (head == "while" || head == "until") && escaped_at.get(&seg_index) == Some(&true) {
                continue;
            }
            if let Some(reason) = generator_reason(&head, &argv) {
                if !bounded {
                    return Ok(Some((reason, segment.join(" "))));
                }
            }
            // The same part read with an unlisted wrapper flag consuming its
            // value. Neither reading is safe alone.
            if let (Some(greedy_head), greedy_argv) = head_of(part, true) {
                if greedy_head != head {
                    if let Some(reason) = generator_reason(&greedy_head, &greedy_argv) {
                        if !bounded {
                            return Ok(Some((reason, segment.join(" "))));
                        }
                    }
                }
            }
            if let Some(payload) = payload_of(&head, &argv).filter(|p| !p.is_empty()) {
                if let Some(found) = find_unbounded(&payload, bounded, depth + 1)? {
                    return Ok(Some(found));
                }
            }
        }
    }
    Ok(None)
}

fn refusal(reason: &str, segment: &str) -> String {
    format!(
        "Refusing an unbounded process that cannot end on its own.\n\n  {segment}\n\n{reason}\nOn 2026-08-13 that shape cost 71 core-hours and wedged a preflight lock for 50+ minutes.\n\nBounded and named instead:\n  {REPLACEMENT_HELPER}\nor, with no checkout at hand, the same guarantee by hand:\n  {REPLACEMENT}\n\n`loadgen.sh` makes the bound and the `fno-load-` name mandatory, which is why it comes first: a hand-rolled copy missing either half is the orphan this refusal exists to prevent. The bound is the death path. `exec -a fno-...` is the name, so a survivor answers \"whose is this?\" in `top` instead of by lsof archaeology, and `fno agents fleet orphans --reap` may kill it unattended. Use bash explicitly for the hand form: zsh has no `exec -a`.\nAny of `timeout`, `gtimeout`, `count=`, `ulimit -t` in an earlier command, or `-t <seconds>` satisfies this guard. So does piping INTO a reader that exits, as in `yes | head -c 1M`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn denied(cmd: &str) {
        assert!(decide(cmd).is_some(), "should have refused: {cmd:?}");
    }

    fn allowed(cmd: &str) {
        assert!(decide(cmd).is_none(), "should have allowed: {cmd:?}");
    }

    #[test]
    fn refuses_each_unbounded_shape() {
        for cmd in [
            "cat > f.txt <<'EOF'\nhello\nEOF\nyes > /dev/null",
            "echo 'a << b'\nyes > /dev/null",
            "while true; do echo break; done",
            "cd /tmp || exit 1; while true; do sleep 60; done &",
            "timeout 1 true | yes > /dev/null",
            "yes > /dev/null  # no timeout needed",
            "yes -t 5 > /dev/null",
            "cat < /dev/zero",
            "cat 0< /dev/zero",
            "case $x in a) yes > /dev/null;; esac",
            "while true; do sleep 60; done; exit 0",
            "for ((i=0;;)); do sleep 1; done",
            "for f in *; do [ -e $f ] && break; done; while true; do sleep 60; done &",
            "/usr/bin/time -o /tmp/t.log yes > /dev/null",
            "sudo -E yes > /dev/null",
            "# handle the drained case\n: | yes > /dev/null",
            "( for ((;;)); do :; done ) &",
            "( for f in a b; do break; done ); for ((;;)); do :; done &",
            "for i in $(seq 1 24); do yes > /dev/null & done",
            "true | for ((;;)); do :; done",
            "x=$(for f in a b; do break; done); for ((;;)); do :; done",
            "case $x in a|for) echo hi;; esac; for ((;;)); do :; done",
            "case \"$m\" in a) true | for f in 1 2; do echo $f; done;; esac; for ((;;)); do :; done",
            "until false; do timeout 300 yes; done",
            "until false; do echo hi; done",
            "while true; do sleep 1; done",
            "bash -lc 'yes > /dev/null'",
            "sh -ec 'while true; do sleep 1; done'",
            "yes 2> /tmp/gtimeout",
            "yes > /tmp/timeout",
        ] {
            denied(cmd);
        }
    }

    #[test]
    fn allows_bounded_and_ordinary_commands() {
        for cmd in [
            "timeout 300 bash -c 'exec -a fno-load-x1 yes > /dev/null'",
            "nohup bash -c 'source \"$1/eval-sweep-throttle.sh\" || exit 0; _eval_sweep_run_stages \"$2\" \"$3\" \"$4\"' _ a b c d >/dev/null 2>&1 &",
            "git commit -m \"fix\nyes it works\"",
            "printf x\ntimeout 5 yes",
            "ls  # yes > /dev/null",
            "while true; do sleep 5; if gh pr checks; then break; fi; done",
            "cat > poll.sh <<'EOF'\nwhile true; do :; done\nEOF",
            "command -v yes",
            "yes |& head -c 1M",
            "while false; do echo hi; done",
            "wc -l < report.txt",
            "for ((i=0;i<10;i++)); do sleep 1; done",
            "echo \"for ((;;))\"",
            "for f in \"for ((;;))\"; do echo \"$f\"; done",
            "case $a in y|yes) echo go;; esac",
            "while true; do for n in 1 2; do echo $n; done; gh pr view && break; sleep 30; done",
            "stress --timeout 60",
            "stress --timeout=60 --cpu 1",
            "# handle the drained case\nyes | head -c 1M",
            "case $a in # y|yes esac\n y|yes) echo go;; esac",
            "echo hi | grep for",
            "x=$(date); for f in a b; do echo $f; done",
            "case $x in a|for) echo hi;; esac",
            "cmd --opt=value; for f in a b; do echo $f; done",
            "case \"$m\" in a) true | for f in 1 2; do echo $f; done;; esac",
            "timeout 5 bash -lc 'yes > /dev/null'",
            "timeout 300 yes > /dev/null",
            "head -c 1M /dev/zero > /dev/null",
            "echo \"oops\nyes > /dev/null &",
        ] {
            allowed(cmd);
        }
    }

    #[test]
    fn every_advertised_remedy_actually_works() {
        let reason = decide("yes > /dev/null &").expect("refused");
        for (phrase, remedy) in [
            ("loadgen.sh", "bash scripts/lib/loadgen.sh start x1 300 8"),
            ("timeout", "timeout 300 yes > /dev/null"),
            ("gtimeout", "gtimeout 300 yes > /dev/null"),
            ("count=", "dd if=/dev/zero of=/dev/null count=10"),
            ("ulimit -t", "ulimit -t 60; yes > /dev/null"),
            ("-t <seconds>", "stress -c 8 -t 60"),
            ("yes | head -c 1M", "yes | head -c 1M"),
        ] {
            assert!(reason.contains(phrase), "no longer advertised: {phrase}");
            allowed(remedy);
        }
    }

    fn bash(command: &str, background: bool) -> Value {
        json!({"tool_name": "Bash", "tool_input": {"command": command, "run_in_background": background}})
    }

    #[test]
    fn other_tools_and_blank_input_allow() {
        assert!(judge(&Value::Null).is_none());
        assert!(judge(&json!({"tool_name": "Read", "tool_input": {"command": "yes"}})).is_none());
        assert!(judge(&bash("   ", true)).is_none());
    }

    #[test]
    fn lexer_matches_shlex_punctuation_shapes() {
        let toks = shlex_tokens("x=$(echo 1); yes").expect("lexes");
        assert_eq!(toks, ["x=$", "(", "echo", "1", ");", "yes"]);
        let toks = shlex_tokens("for ((i=0;;)); do :").expect("lexes");
        assert_eq!(toks, ["for", "((", "i=0", ";;));", "do", ":"]);
        assert_eq!(shlex_tokens("a '' b").expect("lexes"), ["a", "", "b"]);
        assert_eq!(shlex_tokens("\"a\\$b\\\"c\"").expect("lexes"), ["a\\$b\"c"]);
        assert!(shlex_tokens("echo \"open").is_none());
        assert!(shlex_tokens("echo trailing\\").is_none());
    }
}

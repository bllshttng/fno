//! Command position. Gate keywords (`git`, `gh pr merge`) fire only at the
//! start of a command or right after a shell separator, never inside a
//! quoted argument. Lines split outside quotes first, heredoc bodies are
//! content, and `$( ... )` bodies are re-segmented.

use super::super::bg_process_guard::{shlex_lex, Lex};
use regex::Regex;
use std::sync::OnceLock;

pub(super) type Segment = Vec<String>;

/// An open quote or a trailing escape: command position is unknowable, so
/// the caller takes the deny-leaning whole-string path.
#[derive(Debug)]
pub(super) struct Unbalanced;

const SEPARATORS: &[&str] = &[";", ";;", "&&", "||", "|", "|&", "&", ")"];
// Keywords occupy command position, so the real command is the next token:
// `! git push origin main` and `if git push ...; then` hide nothing.
const KEYWORDS: &[&str] = &[
    "then", "do", "if", "elif", "else", "while", "until", "!", "{", "}", "fi", "done", "esac",
    "case", "in", "select", "for",
];
const CMD_WRAPPERS: &[&str] = &[
    "sudo", "env", "command", "time", "nice", "builtin", "exec", "xargs", "nohup", "stdbuf",
    "eval", "coproc", "timeout", "gtimeout", "setsid", "doas",
];
const SHELL_RUNNERS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];
const WRAPPER_VALUE_OPTS: &[&str] = &[
    "-u",
    "-g",
    "-n",
    "-C",
    "-s",
    "-k",
    "-p",
    "--user",
    "--group",
    "--chdir",
    "--signal",
    "--kill-after",
];
const GIT_GLOBAL_VALUE_OPTS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
    "--super-prefix",
];
const GIT_GLOBAL_FLAGS: &[&str] = &[
    "-p",
    "--paginate",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "--no-optional-locks",
];
const GIT_CONFIG_READ_FLAGS: &[&str] = &[
    "--get",
    "--get-all",
    "--get-regexp",
    "--list",
    "-l",
    "--show-origin",
    "--unset",
    "--unset-all",
];
/// Output redirection as whole operator tokens. `<` is absent on purpose:
/// input redirection runs nothing, and `<<` is the recommended escape.
pub(super) const REDIRECT_TOKENS: &[&str] = &[">", ">>", ">|", ">&", "&>", "&>>"];

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

fn assign_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^[A-Za-z_][A-Za-z0-9_]*=")
}

fn duration_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^\d+(\.\d+)?[smhd]?$")
}

/// The executable's basename, lowercased: `/usr/bin/GH` reads as `gh`.
pub(super) fn base_lower(tok: &str) -> String {
    tok.rsplit('/').next().unwrap_or(tok).to_lowercase()
}

/// `str.isdigit` on ASCII input: non-empty and every char a digit.
pub(super) fn is_digits(tok: &str) -> bool {
    !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit())
}

/// `shlex.split`: posix words, operators kept inside words, no comments.
pub(super) fn split_words(text: &str) -> Option<Vec<String>> {
    shlex_lex(
        text,
        Lex {
            punct: false,
            comments: false,
        },
    )
}

/// The first heredoc opener on `line` outside quotes and comments, as
/// `(delimiter, is_dash)`. `<<<` and a `<<` with no word after are not
/// openers, so the line keeps no body exemption.
pub(super) fn find_heredoc_opener(line: &str) -> Option<(String, bool)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < n {
        let ch = chars[i];
        if let Some(q) = quote {
            if ch == '\\' && q == '"' && i + 1 < n {
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            i += 1;
            continue;
        }
        if ch == '\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if ch == '#' && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')) {
            break;
        }
        if ch == '<' && i + 1 < n && chars[i + 1] == '<' {
            if let Some(opener) = parse_heredoc_at(&chars, i + 2) {
                return Some(opener);
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

fn parse_heredoc_at(chars: &[char], mut j: usize) -> Option<(String, bool)> {
    let n = chars.len();
    let mut dash = false;
    if j < n && chars[j] == '-' {
        dash = true;
        j += 1;
    }
    if j < n && (chars[j] == '\'' || chars[j] == '"') {
        j += 1;
    }
    let first = *chars.get(j)?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let word: String = chars[j..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_' || **c == '-')
        .collect();
    Some((word, dash))
}

fn heredoc_ends(line: &str, delim: &str, dash: bool) -> bool {
    let term = if dash {
        line.trim_start_matches('\t')
    } else {
        line
    };
    term == delim
}

/// Split on newlines outside quotes. A newline inside a quote is one
/// argument (a commit body, a mail text); inside a heredoc body quoting does
/// not apply, so an apostrophe in prose cannot swallow the terminator.
pub(super) fn split_lines_outside_quotes(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let n = chars.len();
    let mut lines = Vec::new();
    let mut buf = String::new();
    let mut quote: Option<char> = None;
    let mut heredoc: Option<(String, bool)> = None;
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        if let Some((delim, dash)) = &heredoc {
            if ch == '\n' {
                let line = std::mem::take(&mut buf);
                let done = heredoc_ends(&line, delim, *dash);
                lines.push(line);
                if done {
                    heredoc = None;
                }
            } else {
                buf.push(ch);
            }
            i += 1;
            continue;
        }
        if let Some(q) = quote {
            if ch == '\\' && q == '"' && i + 1 < n {
                buf.push(ch);
                buf.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
            buf.push(ch);
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            buf.push(ch);
            i += 1;
            continue;
        }
        if ch == '\\' && i + 1 < n {
            buf.push(ch);
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if ch == '\n' {
            let line = std::mem::take(&mut buf);
            heredoc = find_heredoc_opener(&line);
            lines.push(line);
            i += 1;
            continue;
        }
        buf.push(ch);
        i += 1;
    }
    lines.push(buf);
    lines
}

/// The body of each `$( ... )` the shell executes on `line`, double quotes
/// included. Single quotes expand nothing, and backticks are left alone:
/// markdown code spans in prose use them.
fn substitution_bodies(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            }
            i += 1;
            continue;
        }
        if quote == Some('"') && ch == '\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if ch == '$' && i + 1 < n && chars[i + 1] == '(' {
            let mut depth = 1i32;
            let mut j = i + 2;
            while j < n && depth != 0 {
                depth += i32::from(chars[j] == '(') - i32::from(chars[j] == ')');
                j += 1;
            }
            if depth == 0 {
                out.push(chars[i + 2..j - 1].iter().collect());
                i = j;
                continue;
            }
        }
        if quote.is_none() && (ch == '\'' || ch == '"') {
            quote = Some(ch);
        } else if Some(ch) == quote {
            quote = None;
        }
        i += 1;
    }
    out
}

/// Every command-position segment of `command`. A backslash-newline joins
/// lines first, so `git push \<nl>origin main` is one push. An unterminated
/// heredoc fails closed: every line is judged as a command again.
pub(super) fn command_segments(command: &str) -> Result<Vec<Segment>, Unbalanced> {
    command_segments_at(command, 0)
}

fn command_segments_at(command: &str, depth: usize) -> Result<Vec<Segment>, Unbalanced> {
    static CONT: OnceLock<Regex> = OnceLock::new();
    let command = re(&CONT, r"\\\r?\n").replace_all(command, "");
    let lines = split_lines_outside_quotes(&command);
    let mut segments = Vec::new();
    let mut heredoc: Option<(String, bool)> = None;
    for line in &lines {
        if let Some((delim, dash)) = &heredoc {
            if heredoc_ends(line, delim, *dash) {
                heredoc = None;
            }
            continue;
        }
        if !line.trim().is_empty() {
            segments.extend(segments_one_line(line)?);
            if depth < 3 {
                for body in substitution_bodies(line) {
                    segments.extend(command_segments_at(&body, depth + 1)?);
                }
            }
        }
        if let Some(opener) = find_heredoc_opener(line) {
            heredoc = Some(opener);
        }
    }
    if heredoc.is_some() {
        segments.clear();
        for line in &lines {
            if !line.trim().is_empty() {
                segments.extend(segments_one_line(line)?);
            }
        }
    }
    Ok(segments)
}

fn segments_one_line(line: &str) -> Result<Vec<Segment>, Unbalanced> {
    let tokens = shlex_lex(
        line,
        Lex {
            punct: true,
            comments: true,
        },
    )
    .ok_or(Unbalanced)?;
    let mut segments = Vec::new();
    let mut current: Segment = Vec::new();
    let mut at_start = true;
    for tok in tokens {
        if SEPARATORS.contains(&tok.as_str()) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            at_start = true;
            continue;
        }
        if at_start && KEYWORDS.contains(&tok.as_str()) {
            continue;
        }
        current.push(tok);
        at_start = false;
    }
    if !current.is_empty() {
        segments.push(current);
    }
    Ok(segments)
}

fn is_dash_c(tok: &str) -> bool {
    tok.starts_with('-') && !tok.starts_with("--") && tok[1..].contains('c')
}

pub(super) fn effective_argv(seg: &[String]) -> Vec<String> {
    effective_argv_and_assignments(seg).0
}

/// The real argv plus the `NAME=value` assignments a shell applies. Strips
/// subshell `(`, assignments, wrappers with their own options, and a shell
/// runner's `-c`. A wrapper whose one argument holds a whole command
/// (`eval "git push origin main"`) is re-tokenized so the command surfaces.
pub(super) fn effective_argv_and_assignments(seg: &[String]) -> (Vec<String>, Vec<String>) {
    let n = seg.len();
    let mut i = 0;
    let mut saw_wrapper = false;
    let mut assignments = Vec::new();
    while i < n {
        let tok = &seg[i];
        if tok == "(" || assign_re().is_match(tok) {
            if tok != "(" {
                assignments.push(tok.clone());
            }
            i += 1;
            continue;
        }
        let base = base_lower(tok);
        if CMD_WRAPPERS.contains(&base.as_str()) {
            i += 1;
            saw_wrapper = true;
            continue;
        }
        if SHELL_RUNNERS.contains(&base.as_str()) {
            let mut j = i + 1;
            while j < n && seg[j].starts_with('-') && !is_dash_c(&seg[j]) {
                j += 1;
            }
            if j < n && is_dash_c(&seg[j]) && j + 1 < n {
                i = j + 1;
                continue;
            }
        }
        if saw_wrapper && WRAPPER_VALUE_OPTS.contains(&tok.as_str()) && i + 1 < n {
            i += 2;
            continue;
        }
        if saw_wrapper && (tok.starts_with('-') || duration_re().is_match(tok)) {
            i += 1;
            continue;
        }
        break;
    }
    let argv: Vec<String> = seg[i..].to_vec();
    if argv.len() == 1 && argv[0].chars().any(char::is_whitespace) {
        let inner = split_words(&argv[0])
            .unwrap_or_else(|| argv[0].split_whitespace().map(str::to_string).collect());
        if !inner.is_empty() {
            let (inner_argv, inner_assignments) = effective_argv_and_assignments(&inner);
            assignments.extend(inner_assignments);
            return (inner_argv, assignments);
        }
    }
    (argv, assignments)
}

/// Drop git's global options so argv[1] is the subcommand:
/// `git -C /repo push origin main` reads as `git push origin main`.
pub(super) fn strip_git_global_opts(argv: &[String]) -> Vec<String> {
    let rest = &argv[argv.len().min(1)..];
    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i].as_str();
        if GIT_GLOBAL_VALUE_OPTS.contains(&tok) {
            i += 2;
            continue;
        }
        let attached = tok.starts_with("--")
            && tok
                .split_once('=')
                .is_some_and(|(k, _)| GIT_GLOBAL_VALUE_OPTS.contains(&k));
        if GIT_GLOBAL_FLAGS.contains(&tok) || attached {
            i += 1;
            continue;
        }
        break;
    }
    let mut out: Vec<String> = argv.iter().take(1).cloned().collect();
    out.extend(rest.iter().skip(i).cloned());
    out
}

fn has_config_value(rest: &[String]) -> bool {
    let mut skip = false;
    for (i, tok) in rest.iter().enumerate() {
        if skip {
            skip = false;
        } else if REDIRECT_TOKENS.contains(&tok.as_str())
            || matches!(tok.as_str(), "<" | "<<" | "<<<" | "<&")
        {
            skip = true;
        } else if is_digits(tok)
            && rest
                .get(i + 1)
                .is_some_and(|next| REDIRECT_TOKENS.contains(&next.as_str()))
        {
            continue;
        } else if !tok.starts_with('-') {
            return true;
        }
    }
    false
}

/// True when this git call points core.hooksPath elsewhere. That disables
/// .git/hooks/pre-push, the protected-branch guard, so it is a
/// --no-verify by another name. Positional, so a commit message naming
/// the key is not a setting.
pub(super) fn sets_hooks_path(argv: &[String]) -> bool {
    for (idx, tok) in argv.iter().enumerate() {
        let low = tok.to_lowercase();
        if low.starts_with("core.hookspath=") {
            return true;
        }
        if low == "core.hookspath" {
            let before: Vec<String> = argv[..idx].iter().map(|a| a.to_lowercase()).collect();
            if before.last().map(String::as_str) == Some("-c") {
                return true;
            }
            if before.iter().skip(1).any(|a| a == "config") {
                if before
                    .iter()
                    .any(|a| GIT_CONFIG_READ_FLAGS.contains(&a.as_str()))
                {
                    continue;
                }
                if has_config_value(&argv[idx + 1..]) {
                    return true;
                }
            }
        }
    }
    false
}

/// True when a redirection writes a file. `2>&1` and `>&2` duplicate a
/// descriptor and write nothing.
pub(super) fn writes_a_file(tokens: &[String]) -> bool {
    for (idx, tok) in tokens.iter().enumerate() {
        if !REDIRECT_TOKENS.contains(&tok.as_str()) {
            continue;
        }
        if tok == ">&" || tok == "&>" {
            let next = tokens.get(idx + 1).map(String::as_str).unwrap_or("");
            if is_digits(next) || is_digits(next.trim_end_matches('-')) {
                continue;
            }
        }
        return true;
    }
    false
}

/// True when a real heredoc opener has an unquoted delimiter, whose body
/// the shell expands. Quoted `<<'EOF'` is literal: the recommended escape.
/// Body lines are skipped, so a message mentioning `<<EOF` opens nothing.
pub(super) fn has_unquoted_heredoc(command: &str) -> bool {
    let lines = split_lines_outside_quotes(command);
    let mut i = 0;
    while i < lines.len() {
        let Some((delim, dash)) = find_heredoc_opener(&lines[i]) else {
            i += 1;
            continue;
        };
        let unquoted = Regex::new(&format!(r"<<-?[ \t]*{}(\s|$)", regex::escape(&delim)))
            .map(|r| r.is_match(&lines[i]))
            .unwrap_or(false);
        i += 1;
        while i < lines.len() {
            let done = heredoc_ends(&lines[i], &delim, dash);
            i += 1;
            if done {
                break;
            }
        }
        if unquoted {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(command: &str) -> Vec<String> {
        command_segments(command)
            .expect("balanced")
            .iter()
            .map(|s| s.join(" "))
            .collect()
    }

    #[test]
    fn segments_split_at_separators_keywords_and_lines() {
        assert_eq!(
            segs("cd /tmp && git push origin main"),
            ["cd /tmp", "git push origin main"]
        );
        assert_eq!(
            segs("true |& git push origin main"),
            ["true", "git push origin main"]
        );
        assert_eq!(
            segs("if git push origin main; then true; fi"),
            ["git push origin main", "true"]
        );
        assert_eq!(
            segs("git status\ngh pr merge 1"),
            ["git status", "gh pr merge 1"]
        );
        assert_eq!(segs("git push \\\norigin main"), ["git push origin main"]);
        assert_eq!(
            segs("git push origin feat # main"),
            ["git push origin feat"]
        );
    }

    #[test]
    fn quoted_text_and_heredoc_bodies_are_not_command_positions() {
        assert_eq!(
            segs("fno backlog idea \"x && gh pr merge 1\""),
            ["fno backlog idea x && gh pr merge 1"]
        );
        assert_eq!(segs("git commit -m \"a\nb\""), ["git commit -m a\nb"]);
        assert_eq!(
            segs("cat <<'EOF'\ngit push origin main\nEOF"),
            ["cat << EOF"]
        );
        // An unterminated body fails closed: its lines are commands again.
        assert!(
            segs("cat <<EOF\ngit push origin main").contains(&"git push origin main".to_string())
        );
        assert!(segs("echo \"$(\ngh pr merge 1)\"").contains(&"gh pr merge 1".to_string()));
    }

    #[test]
    fn an_unbalanced_quote_is_err() {
        assert!(command_segments("git commit -m 'oops").is_err());
    }

    #[test]
    fn effective_argv_strips_wrappers_runners_and_assignments() {
        let argv = |s: &str| effective_argv_and_assignments(&split_words(s).unwrap());
        assert_eq!(argv("sudo -u x git push").0, ["git", "push"]);
        assert_eq!(
            argv("timeout 10 GH_TOKEN=t gh pr merge 1").0,
            ["gh", "pr", "merge", "1"]
        );
        assert_eq!(
            argv("bash -lc \"git push origin main\"").0,
            ["git", "push", "origin", "main"]
        );
        assert_eq!(
            argv("eval 'X=1 gh pr create'"),
            (
                vec!["gh".into(), "pr".into(), "create".into()],
                vec!["X=1".into()]
            )
        );
    }

    #[test]
    fn hooks_path_reads_are_not_settings() {
        let words = |s: &str| split_words(s).unwrap();
        assert!(sets_hooks_path(&words(
            "git -c core.hooksPath=/dev/null push"
        )));
        assert!(sets_hooks_path(&words("git config core.hooksPath /tmp/h")));
        assert!(!sets_hooks_path(&words("git config core.hooksPath 2>&1")));
        assert!(!sets_hooks_path(&words("git config --get core.hooksPath")));
        assert!(!sets_hooks_path(&words("git commit -m core.hooksPath")));
        assert_eq!(
            strip_git_global_opts(&words("git -C /r --no-pager push o m")),
            ["git", "push", "o", "m"]
        );
    }

    #[test]
    fn redirects_and_heredocs_that_can_carry_a_second_action() {
        let toks = |s: &str| split_words(s).unwrap();
        assert!(!writes_a_file(&toks("git commit -m ok 2 >& 1")));
        assert!(writes_a_file(&toks("git commit -m ok > /tmp/log")));
        assert!(has_unquoted_heredoc("git commit -F - <<EOF\nmsg\nEOF"));
        assert!(!has_unquoted_heredoc(
            "git commit -F - <<'EOF'\nfix <<EOF parsing\nEOF"
        ));
        assert!(!has_unquoted_heredoc("git commit -m \"shift << 2\""));
    }
}

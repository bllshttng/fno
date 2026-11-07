//! The one parser and renderer for the PR-body closure line.
//!
//! The line format lives here and nowhere else:
//! readers forward to this module through `fno-agents pr closure parse|render`,
//! so Python and Rust can never disagree about what a body claims. Writers
//! emit only the `Fixes` spelling; readers also accept the retired
//! `Backlog-Closure:` spelling until no open PR carries it.
//!
//! Grammar, one rule for both spellings: the keyword sits at the START of a
//! line and matches case-insensitively, with or without a colon. Every token
//! after it is a well-formed node id, split by commas and/or spaces. If any
//! token is not a node id, the line is prose and claims nothing. A number's
//! annotation tail, `(<hash>)`, is skipped rather than poisoning the line:
//! render writes `Fixes x-5555 (x-aaaa)`, and the number token resolves to
//! the same hash the tail names. The LAST matching line wins; a prose line
//! is not a match and never erases an earlier good one.
//!
//! A body may also carry ONE approval hand-off line, `Retarget <from> <to>
//! <approval>` (same keyword rule), whose grammar lives in `retarget` below.

use super::queues::NODE_ID_BODY;
use serde_json::{json, Value};

/// The keywords a closure line may start with. `Fixes` is the only spelling
/// writers render; `Backlog-Closure` is read for PRs opened before the rename.
const KEYWORDS: [&str; 2] = ["fixes", "backlog-closure"];

/// The well-formed, deduplicated ids ONE line claims; empty when the line is
/// prose (no keyword, or any malformed token).
pub(crate) fn line_ids(line: &str) -> Vec<String> {
    line_ids_with(line, &crate::numbers::resolve_node_ref)
}

/// `line_ids` over an injected resolver, the test seam. Production passes
/// `numbers::resolve_node_ref`; a stub keeps unit tests off the machine's
/// store.
pub(crate) fn line_ids_with(line: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let Some(rest) = closure_line_rest(line) else {
        return Vec::new();
    };
    parse_line_tokens_with(rest, resolve).unwrap_or_default()
}

/// The rest of `line` after a closure keyword, when the line opens with one.
/// `None` on any line whose head is not keyword + (colon or whitespace).
fn closure_line_rest(line: &str) -> Option<&str> {
    for kw in KEYWORDS {
        // get(), never [..len]: a line opening with a multibyte character
        // makes the keyword length a non-boundary, and a bare slice panics.
        if line.len() >= kw.len()
            && line
                .get(..kw.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(kw))
        {
            let rest = &line[kw.len()..];
            return match rest.chars().next() {
                Some(':') => Some(&rest[1..]),
                Some(c) if c.is_whitespace() => Some(rest),
                None => Some(rest),
                // Glued text ("fixated") is a word, not the keyword.
                Some(_) => None,
            };
        }
    }
    None
}

/// The well-formed, deduplicated ids in the token soup after the keyword.
/// `None` when any token is malformed: the line is prose and claims nothing.
fn parse_line_tokens_with(
    rest: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Option<Vec<String>> {
    let id_re = regex::Regex::new(&format!("^{NODE_ID_BODY}$")).expect("static regex");
    let mut ids: Vec<String> = Vec::new();
    for token in rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        // The number's annotation tail, `(<hash>)`: a reader of the number
        // form skips it instead of poisoning the line as prose. The number
        // token beside it resolves to the same node, so the pair reads as
        // one claim.
        if let Some(inner) = token.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            if !id_re.is_match(inner) {
                return None;
            }
            continue;
        }
        if !id_re.is_match(token) {
            return None;
        }
        let id = resolve(token).unwrap_or_else(|| token.to_string());
        if !ids.iter().any(|i| i == &id) {
            ids.push(id);
        }
    }
    Some(ids)
}

/// Well-formed node ids claimed by the LAST well-formed closure line of
/// `body`, order-preserved, deduplicated. A candidate line carrying any
/// malformed token is prose: it claims nothing and does not win.
pub(crate) fn parse(body: &str) -> Vec<String> {
    parse_with(body, &crate::numbers::resolve_node_ref)
}

/// `parse` over an injected resolver, the test seam beside `line_ids_with`.
pub(crate) fn parse_with(body: &str, resolve: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut last: Option<Vec<String>> = None;
    for line in body.lines() {
        let Some(rest) = closure_line_rest(line) else {
            continue;
        };
        if let Some(ids) = parse_line_tokens_with(rest, resolve) {
            last = Some(ids);
        }
    }
    last.unwrap_or_default()
}

/// One approved hand-off of a PR from the branch's node to the right one:
/// `Retarget <from> <to> <approval>` on its own line, where approval is a
/// mail id (`fmail-...`, or the older `msg-...`) or a ruling id (`d-...`).
pub(crate) struct Retarget {
    pub from: String,
    pub to: String,
    pub approval: String,
}

/// The rest of `line` after a `retarget` keyword, mirroring
/// `closure_line_rest`: keyword + colon or whitespace, glued text is prose.
fn retarget_line_rest(line: &str) -> Option<&str> {
    let kw = "retarget";
    if line.len() >= kw.len()
        && line
            .get(..kw.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(kw))
    {
        let rest = &line[kw.len()..];
        return match rest.chars().next() {
            Some(':') => Some(&rest[1..]),
            Some(c) if c.is_whitespace() => Some(rest),
            None => Some(rest),
            Some(_) => None,
        };
    }
    None
}

/// The LAST well-formed Retarget line of `body`. Any other shape is prose
/// and never erases an earlier good line, as in `parse`.
pub(crate) fn retarget(body: &str) -> Option<Retarget> {
    retarget_with(body, &crate::numbers::resolve_node_ref)
}

/// `retarget` over an injected resolver, the test seam beside `parse_with`.
/// A number token in the from or to slot resolves to the hash it names, so
/// a number-form Retarget line binds the same node its hash spells.
pub(crate) fn retarget_with(
    body: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Option<Retarget> {
    let id_re = regex::Regex::new(&format!("^{NODE_ID_BODY}$")).expect("static regex");
    let approval_re =
        regex::Regex::new(r"^((fmail|msg)-[0-9a-f]{6,}|d-[0-9a-f]{8})$").expect("static regex");
    let mut last: Option<Retarget> = None;
    for line in body.lines() {
        let Some(rest) = retarget_line_rest(line) else {
            continue;
        };
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        let (Some(from), Some(&to), Some(approval)) = (
            tokens.first().copied(),
            tokens.get(1),
            tokens.get(2).copied(),
        ) else {
            continue;
        };
        if tokens.len() != 3
            || !id_re.is_match(from)
            || !id_re.is_match(to)
            || from == to
            || !approval_re.is_match(approval)
        {
            continue;
        }
        // A number token resolves to the hash it names; an unresolvable one
        // keeps its raw text, the same fail-open the closure line reads with.
        let from = resolve(from).unwrap_or_else(|| from.to_string());
        let to = resolve(to).unwrap_or_else(|| to.to_string());
        if from == to {
            continue;
        }
        last = Some(Retarget {
            from,
            to,
            approval: approval.to_string(),
        });
    }
    last
}

/// The branch node id a body's approved Retarget line hands away: `[from]`
/// only when the line is well formed, the closure line claims `to`, and it
/// does not claim `from`. Otherwise the line claims nothing.
pub(crate) fn retargeted_from(body: &str) -> Vec<String> {
    retargeted_from_with(body, &crate::numbers::resolve_node_ref)
}

/// `retargeted_from` over an injected resolver, the test seam.
pub(crate) fn retargeted_from_with(
    body: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let Some(r) = retarget_with(body, resolve) else {
        return Vec::new();
    };
    let ids = parse_with(body, resolve);
    if ids.contains(&r.to) && !ids.contains(&r.from) {
        vec![r.from]
    } else {
        Vec::new()
    }
}

/// The one spelling writers emit: `Fixes <ids>`, deduplicated, well-formed
/// ids only; empty when nothing well-formed remains, so a caller can append
/// the result to a body unconditionally. An id whose number exists reads as
/// `x-5555 (x-aaaa)`: the number leads, the hash stays beside it as the
/// durable key. A qualified (`@store`) or missing label falls back to the
/// hash alone.
pub(crate) fn render<I, S>(node_ids: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    render_with(node_ids, &crate::numbers::label)
}

/// `render` over an injected label fn, the test seam beside `parse_with`.
pub(crate) fn render_with<I, S>(node_ids: I, label: &dyn Fn(&str) -> Option<String>) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let id_re = regex::Regex::new(&format!("^{NODE_ID_BODY}$")).expect("static regex");
    let mut ids: Vec<String> = Vec::new();
    for id in node_ids {
        let id = id.as_ref();
        if id_re.is_match(id) && !ids.iter().any(|i| i == id) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        String::new()
    } else {
        let tokens: Vec<String> = ids
            .iter()
            .map(|id| match label(id) {
                // A qualified (@store) or missing label falls back to the
                // hash: only an unqualified number rides the line, so a
                // reader on another machine never sees a token the grammar
                // refuses.
                Some(number) if !number.contains('@') && number != *id => {
                    format!("{number} ({id})")
                }
                _ => id.clone(),
            })
            .collect();
        format!("Fixes {}", tokens.join(" "))
    }
}

/// `fno-agents pr-closure-parse|pr-closure-render`: the Python forwarders'
/// door. One JSON payload on stdin, one JSON answer on stdout (the house
/// transport `fno.rust_binary.command_call` speaks). Parse reads `{"body": s}`
/// and answers `{"ids": [...]}`; render reads `{"ids": [...]}` and answers
/// `{"line": s}` (empty when nothing well-formed remains).
pub fn run(args: &[String]) -> i32 {
    use std::io::Read;

    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        eprint!("pr-closure: cannot read stdin\n");
        return 2;
    }
    let parsed: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprint!("pr-closure: bad payload: {e}\n");
            return 2;
        }
    };
    match args.first().map(String::as_str) {
        Some("pr-closure-parse") => {
            let body = parsed.get("body").and_then(Value::as_str).unwrap_or("");
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "ids": parse(body),
                    "retargeted_from": retargeted_from(body),
                }))
                .unwrap_or_else(|_| "{\"ids\":[]}".into())
            );
            0
        }
        Some("pr-closure-render") => {
            let ids: Vec<String> = parsed
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            println!(
                "{}",
                serde_json::to_string(&json!({ "line": render(ids) }))
                    .unwrap_or_else(|_| "{\"line\":\"\"}".into())
            );
            0
        }
        _ => {
            eprint!(
                "usage: fno-agents pr-closure-parse   (JSON {{body}} in; {{ids, retargeted_from}} out)\n       fno-agents pr-closure-render  (JSON {{ids}} in; {{line}} out)\n"
            );
            2
        }
    }
}

/// The whole `pr-closure-*` command door. parse/render stay on the JSON
/// forwarder transport above; the produce commands (branch-ids, ensure,
/// trailer, bind-created, context) live in [`super::pr_closure_produce`].
/// The trailer leg is argv-driven and must never block on stdin.
pub fn run_command(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("pr-closure-parse" | "pr-closure-render") => run(args),
        _ => super::pr_closure_produce::run(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/pr-closure-cases.json")
    }

    /// The shared corpus is the one contract both legs read. A missing or
    /// malformed fixture fails here, never silently passes.
    #[test]
    fn the_shared_corpus_parses_as_the_claims_column_says() {
        let text = std::fs::read_to_string(corpus_path()).expect("shared corpus fixture");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("corpus is JSON");
        let cases = parsed
            .as_object()
            .expect("corpus object")
            .get("cases")
            .expect("cases array");
        let cases = cases.as_array().expect("cases array");
        assert!(
            cases.len() >= 7,
            "the corpus carries at least the plan's seven cases"
        );
        for case in cases {
            let body = case
                .get("body")
                .and_then(|b| b.as_str())
                .expect("case body");
            let labels: std::collections::HashMap<String, String> = case
                .get("labels")
                .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
                .unwrap_or_default();
            let resolve = |t: &str| labels.get(t).map(|s| s.to_string());
            let want: Vec<String> = case
                .get("claims")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                parse_with(body, &resolve),
                want,
                "corpus case failed: {body:?}"
            );
            let want_gone: Vec<String> = case
                .get("retargeted_from")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                retargeted_from_with(body, &resolve),
                want_gone,
                "corpus retargeted_from failed: {body:?}"
            );
        }
    }

    #[test]
    fn render_emits_only_the_new_spelling_and_round_trips() {
        let none = |_: &str| None;
        assert_eq!(
            render_with(["x-aaaa", "x-bbbb"], &none),
            "Fixes x-aaaa x-bbbb"
        );
        assert_eq!(
            parse_with(&render_with(["x-aaaa", "x-bbbb"], &none), &none),
            vec!["x-aaaa", "x-bbbb"]
        );
        assert_eq!(
            render_with(["not-an-id", "x-aaaa", "x-aaaa"], &none),
            "Fixes x-aaaa"
        );
        assert_eq!(render_with(Vec::<String>::new(), &none), "");
    }

    #[test]
    fn render_writes_the_number_beside_the_hash_when_a_label_exists() {
        let labels = |id: &str| match id {
            "x-aaaa" => Some("x-5555".to_string()),
            _ => None,
        };
        assert_eq!(render_with(["x-aaaa"], &labels), "Fixes x-5555 (x-aaaa)");
        assert_eq!(render_with(["x-bbbb"], &labels), "Fixes x-bbbb");
    }

    #[test]
    fn parse_reads_the_number_pair_and_skips_the_paren_tail() {
        let labels = |t: &str| match t {
            "x-5555" => Some("x-aaaa".to_string()),
            _ => None,
        };
        assert_eq!(parse_with("Fixes x-5555 (x-aaaa)", &labels), vec!["x-aaaa"]);
        assert_eq!(parse_with("Fixes x-5555", &labels), vec!["x-aaaa"]);
        assert_eq!(parse_with("Fixes x-aaaa", &labels), vec!["x-aaaa"]);
    }

    #[test]
    fn a_multibyte_line_never_panics_the_keyword_slice() {
        // A body line opening with a multibyte character puts a non-char
        // boundary at the keyword length; the prefix check must read None,
        // not panic.
        let body = "日本語の行です。\nFixes x-aaaa\n";
        assert_eq!(parse(body), vec!["x-aaaa"]);
        assert_eq!(line_ids("日本語の行です。"), Vec::<String>::new());
    }
}

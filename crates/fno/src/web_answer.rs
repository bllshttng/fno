//! `POST /command/answer/{id}`: the pure half of the question drawer's answer.
//! The route in `web.rs` orders the checks; this module holds the parts that
//! need no listener. The write itself is the answer door
//! (`fno-agents needs --answer ... --sink command_page`), spawned through
//! `needs_overlay::answer_as`, so the page never records a decision itself.

use axum::http::{header, HeaderMap};
use serde::Deserialize;
use serde_json::Value;

use crate::needs_overlay::AnswerPick;

/// Longest free-text answer the page may send.
const WORDS_CAP: usize = 4000;
/// Most parents the scope walk follows: a cycle in the rows ends here.
const CHAIN_CAP: usize = 16;

/// The page's body: exactly one of the three fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnswerBody {
    #[serde(default)]
    option: Option<u32>,
    #[serde(default)]
    words: Option<String>,
    #[serde(default)]
    done: Option<bool>,
}

/// The one pick the body names. A page cannot delegate: the key is unknown.
pub(crate) fn pick_of(body: AnswerBody) -> Result<AnswerPick, &'static str> {
    let named = [
        body.option.is_some(),
        body.words.is_some(),
        body.done.is_some(),
    ];
    if named.iter().filter(|n| **n).count() != 1 {
        return Err("name exactly one of option, words or done");
    }
    if let Some(n) = body.option {
        return if n > 0 {
            Ok(AnswerPick::Option(n))
        } else {
            Err("option is a number from 1")
        };
    }
    if let Some(words) = body.words {
        let words = words.trim();
        return if words.is_empty() {
            Err("words is empty")
        } else if words.chars().count() > WORDS_CAP {
            Err("words is too long")
        } else {
            Ok(AnswerPick::Words(words.to_string()))
        };
    }
    if body.done == Some(true) {
        Ok(AnswerPick::Done)
    } else {
        Err("done must be true")
    }
}

/// A browser sends Origin on a cross-origin POST. The request passes when
/// Origin is absent (a script, not a page) or names the same authority as
/// Host. The `null` origin never matches.
pub(crate) fn same_origin(headers: &HeaderMap) -> Result<(), &'static str> {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else {
        return Ok(());
    };
    let authority = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"));
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    match (authority, host) {
        (Some(a), Some(h)) if a.eq_ignore_ascii_case(h) => Ok(()),
        _ => Err("write refused: Origin does not match Host"),
    }
}

/// A question id the door can look up: the ledger's own alphabet, bounded.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The node and its ancestors, nearest first. A scope on an epic covers the
/// epic's children, so the invite check walks the `parent` links.
pub(crate) fn chain_of(rows: &[Value], node: Option<&str>) -> Vec<String> {
    let mut chain = Vec::new();
    let mut next = node.map(str::to_string);
    while let Some(id) = next.take() {
        if chain.len() >= CHAIN_CAP || chain.contains(&id) {
            break;
        }
        next = rows
            .iter()
            .find(|r| r.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .and_then(|r| r.get("parent").and_then(Value::as_str))
            .map(str::to_string);
        chain.push(id);
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn body(json: &str) -> Result<AnswerBody, serde_json::Error> {
        serde_json::from_str(json)
    }

    fn web_answer_pick_names_exactly_one() {
        let pick = |j: &str| pick_of(body(j).unwrap());
        assert_eq!(pick(r#"{"option":2}"#), Ok(AnswerPick::Option(2)));
        assert_eq!(
            pick(r#"{"words":"  ship it "}"#),
            Ok(AnswerPick::Words("ship it".into()))
        );
        assert_eq!(pick(r#"{"done":true}"#), Ok(AnswerPick::Done));
        for bad in [
            r#"{}"#,
            r#"{"option":0}"#,
            r#"{"option":1,"words":"x"}"#,
            r#"{"words":"   "}"#,
            r#"{"done":false}"#,
        ] {
            assert!(pick(bad).is_err(), "{bad}");
        }
        // A page cannot delegate: the key is not in the body.
        assert!(body(r#"{"delegate":true}"#).is_err());
        let long = format!(r#"{{"words":"{}"}}"#, "a".repeat(WORDS_CAP + 1));
        assert!(pick(&long).is_err());
    }

    fn web_answer_origin_must_match_host_when_sent() {
        let with = |origin: Option<&str>| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_static("box.tail1.ts.net"));
            if let Some(o) = origin {
                h.insert(header::ORIGIN, HeaderValue::from_str(o).unwrap());
            }
            same_origin(&h)
        };
        assert!(with(None).is_ok());
        assert!(with(Some("https://box.tail1.ts.net")).is_ok());
        assert!(with(Some("https://evil.example")).is_err());
        assert!(with(Some("null")).is_err());
    }

    fn web_answer_ids_and_scope_chain() {
        assert!(valid_id("q-door"));
        assert!(!valid_id(""));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(&"a".repeat(129)));
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"x-3","parent":"x-2"},{"id":"x-2","parent":"x-1"},
                {"id":"x-1"},{"id":"loop-a","parent":"loop-b"},{"id":"loop-b","parent":"loop-a"}]"#,
        )
        .unwrap();
        assert_eq!(chain_of(&rows, Some("x-3")), ["x-3", "x-2", "x-1"]);
        assert_eq!(chain_of(&rows, Some("x-9")), ["x-9"]);
        assert_eq!(chain_of(&rows, Some("loop-a")), ["loop-a", "loop-b"]);
        assert!(chain_of(&rows, None).is_empty());
    }

    #[test]
    fn web_answer_pure_checks() {
        web_answer_pick_names_exactly_one();
        web_answer_origin_must_match_host_when_sent();
        web_answer_ids_and_scope_chain();
    }
}

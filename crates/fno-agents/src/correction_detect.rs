//! The correction detector's shared vocabulary, moved out of
//! `intel_corpus.rs` so the daemon's correction arm and the corpus builder
//! read one rule: the typed-reaction regex, the filtered correction rule,
//! and the scrub. The corpus keeps its own labels by calling the unfiltered
//! [`reaction_rule`]; the event detector applies the precision filters on
//! top ([`detect_user`]). Rule versions: rates compare only within one
//! version, so a filter change bumps [`RULE_VERSION`].

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// The filtered-rule version stamped on every `agent_corrected` row.
pub(crate) const RULE_VERSION: &str = "v1";

fn rx(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

pub(crate) fn email_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    rx(
        &RX,
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
    )
}

pub(crate) fn sensitive_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    rx(
        &RX,
        r#"(?i)(?:^|[\s/'"=:(])\.env(?:\.[\w-]+)?\b|credentials|\.pem\b|keychain|security\s+find-(?:generic|internet)-password|\.netrc\b|\bid_(?:rsa|ed25519)\b"#,
    )
}

/// The unfiltered rule label of a typed reply, `None` when no rule is sure.
/// This is the corpus's rule, verbatim from `intel_corpus.rs`; row counts
/// must not shift.
pub(crate) fn reaction_rule(text: &str) -> Option<&'static str> {
    static APPROVE: OnceLock<Regex> = OnceLock::new();
    static REJECT: OnceLock<Regex> = OnceLock::new();
    static CORRECT: OnceLock<Regex> = OnceLock::new();
    let t = text.trim().to_lowercase();
    if t.is_empty() || t.ends_with('?') {
        return None;
    }
    if rx(
        &REJECT,
        r"^(?:no|nope|nah|stop|don't|do not|cancel|abort|never ?mind)[\s.!]*$",
    )
    .is_match(&t)
    {
        return Some("reject");
    }
    if rx(
        &CORRECT,
        r"^(?:no|nope|actually|wait|instead|rather|not quite|that's not|thats not|that is not|wrong|incorrect)\b[\s,.:!-]+\S",
    )
    .is_match(&t)
    {
        return Some("correct");
    }
    if t.chars().count() <= 60
        && !t.contains(" but ")
        && !t.contains("instead")
        && rx(
            &APPROVE,
            r"^(?:y|yes|yep|yeah|yup|ok|okay|k|sure|go|go ahead|go for it|do it|ship it|lgtm|sounds good|looks good|perfect|great|nice|proceed|continue|keep going|approved?|merge it|please do|👍)\b",
        )
        .is_match(&t)
    {
        return Some("approve");
    }
    None
}

/// One detected user correction, with the filter facts the row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserCorrection {
    /// `correct` or `reject`, the corpus rule's own names.
    pub kind: &'static str,
    /// The corrected assistant turn ended in a question.
    pub after_question: bool,
}

/// True when the turn's FIRST sentence carries a `?`: a question is a
/// question, not a correction, however it starts. The unfiltered rule only
/// checks the last character; this filter reads the whole first sentence.
fn first_sentence_asks(text: &str) -> bool {
    text.trim_start()
        .split(['.', '!', '\n'])
        .next()
        .is_some_and(|s| s.contains('?'))
}

/// The filtered user-correction rule the event detector runs:
///
/// - a `?` in the first sentence is a question, never a correction;
/// - a bare `no` right after an assistant question is an answer (`no, X`
///   still counts, carrying `after_question: true`);
/// - interrupt is the caller's own kind: it never passes through here.
pub(crate) fn detect_user(
    text: &str,
    assistant_ended_with_question: bool,
) -> Option<UserCorrection> {
    let t = text.trim();
    if t.is_empty() || first_sentence_asks(t) {
        return None;
    }
    let bare_no = t.to_lowercase();
    let bare_no = bare_no.trim();
    if assistant_ended_with_question && bare_no == "no" {
        return None;
    }
    match reaction_rule(t) {
        Some("reject") => Some(UserCorrection {
            kind: "reject",
            after_question: assistant_ended_with_question,
        }),
        Some("correct") => Some(UserCorrection {
            kind: "correct",
            after_question: assistant_ended_with_question,
        }),
        _ => None,
    }
}

/// A status lead an agent mail opens with: a report, not a correction.
pub(crate) fn agent_status_lead(body: &str) -> bool {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    rx(
        &LEAD,
        r"^(?i)(?:no change|no new|no review yet|nothing to|still waiting|no findings?)\b",
    )
    .is_match(body.trim_start())
}

/// The mail body inside an `<fno_mail>` tag, else the text after the first
/// line of a bus or peer envelope, else the text itself.
fn mail_body(text: &str) -> &str {
    if let Some((_, tail)) = text.split_once("<fno_mail") {
        if let Some((_, body)) = tail.split_once('>') {
            return body.split("</fno_mail>").next().unwrap_or(body).trim();
        }
    }
    if text.trim_start().starts_with("`@") {
        if let Some((_, body)) = text.trim_start().split_once('\n') {
            return body.trim();
        }
    }
    text.trim()
}

/// One agent's correction of another, on a delivered mail body: the corpus
/// rule, or a wider lead disputing the recipient's claim. A status lead is a
/// report. Precision on this rule is measured under `rule_version`, and a
/// source under the floor reads `below_floor`.
pub(crate) fn detect_agent(text: &str) -> Option<&'static str> {
    static DISPUTE: OnceLock<Regex> = OnceLock::new();
    let body = mail_body(text);
    if body.is_empty() || agent_status_lead(body) || first_sentence_asks(body) {
        return None;
    }
    if let Some(label @ ("reject" | "correct")) = reaction_rule(body) {
        return Some(label);
    }
    rx(
        &DISPUTE,
        r"(?i)^(?:your (?:claim|fix|diff|reading|report|test|pr|commit)\b.{0,60}\b(?:is wrong|is false|is incorrect|does not|doesn't|did not|didn't|misses|fails|breaks)|that (?:claim|fix|reading|report) is (?:wrong|false|incorrect)|you (?:are wrong|misread|missed|broke|did not|didn't)\b|this is wrong\b|re-?read\b)",
    )
    .is_match(body)
    .then_some("correct")
}

// --------------------------------------------------------------- scrubbing

#[derive(Default)]
pub(crate) struct ScrubCounts {
    pub(crate) secrets_masked: u64,
    pub(crate) paths_masked: u64,
    pub(crate) emails_masked: u64,
    pub(crate) residual_drops: u64,
    pub(crate) sensitive_drops: u64,
}

pub(crate) fn scrub_string(s: &str, counts: &mut ScrubCounts) -> String {
    let secret = crate::intel_html::secret_rx();
    let mut out = s.to_string();
    let n = secret.find_iter(&out).count();
    if n > 0 {
        out = secret
            .replace_all(&out, |caps: &regex::Captures| match caps.get(2) {
                Some(key) => format!("{}[redacted]", key.as_str()),
                None => "[redacted]".to_string(),
            })
            .into_owned();
        counts.secrets_masked += n as u64;
    }
    let paths = crate::intel_html::path_rx();
    let n = paths.find_iter(&out).count();
    if n > 0 {
        out = paths.replace_all(&out, "~").into_owned();
        counts.paths_masked += n as u64;
    }
    let n = email_rx().find_iter(&out).count();
    if n > 0 {
        out = email_rx().replace_all(&out, "[email]").into_owned();
        counts.emails_masked += n as u64;
    }
    out
}

pub(crate) fn scrub_value(v: &mut Value, counts: &mut ScrubCounts) {
    match v {
        Value::String(s) => *s = scrub_string(s, counts),
        Value::Array(items) => items.iter_mut().for_each(|i| scrub_value(i, counts)),
        Value::Object(map) => map.values_mut().for_each(|i| scrub_value(i, counts)),
        _ => {}
    }
}

/// Mask every string, then rescan the serialized row: any secret shape left
/// that is not a mask is a residual, and the row is dropped.
pub(crate) fn scrub_row(mut row: Value, counts: &mut ScrubCounts) -> Option<Value> {
    scrub_value(&mut row, counts);
    let text = row.to_string();
    let residual = crate::intel_html::secret_rx()
        .find_iter(&text)
        .any(|m| !m.as_str().contains("[redacted]"))
        || email_rx().is_match(&text);
    if residual {
        counts.residual_drops += 1;
        return None;
    }
    Some(row)
}

/// The quote a correction row carries: capped at 280 chars, scrubbed, and
/// dropped whole when a secret shape survives the pass. The row still lands
/// with `quote: null, quote_status: dropped`.
pub(crate) fn scrub_quote(quote: &str, counts: &mut ScrubCounts) -> (Option<String>, &'static str) {
    // A pasted block is someone else's words, not the correction.
    let own = quote
        .split("<pasted_content")
        .next()
        .unwrap_or(quote)
        .trim_end();
    let capped: String = own.chars().take(280).collect();
    if sensitive_rx().is_match(&capped) {
        counts.sensitive_drops += 1;
        return (None, "dropped");
    }
    let scrubbed = scrub_string(&capped, counts);
    let residual = crate::intel_html::secret_rx()
        .find_iter(&scrubbed)
        .any(|m| !m.as_str().contains("[redacted]"))
        || email_rx().is_match(&scrubbed);
    if residual {
        counts.residual_drops += 1;
        return (None, "dropped");
    }
    (Some(scrubbed), "kept")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac1_ac2_questions_stay_out_the_corpus_rule_stays_exact_and_no_secret_lands() {
        assert!(detect_user("wait, did you run it?", false).is_none());
        assert!(detect_user("actually, why not the other branch?", false).is_none());
        assert!(detect_user("no", true).is_none());
        assert_eq!(
            detect_user("no, use the other branch", false).unwrap().kind,
            "correct"
        );
        assert!(
            detect_user("no, use the other branch", true)
                .unwrap()
                .after_question
        );
        assert_eq!(detect_user("stop", false).unwrap().kind, "reject");
        assert_eq!(reaction_rule("yes go"), Some("approve"));
        assert_eq!(reaction_rule("nope"), Some("reject"));
        assert_eq!(reaction_rule("what about the tests?"), None);

        assert_eq!(
            detect_agent("<fno_mail from=\"p\">No change: ask open</fno_mail>"),
            None
        );
        assert_eq!(detect_agent("No new findings this round."), None);
        assert_eq!(detect_agent("no, the PR is still red"), Some("correct"));
        assert_eq!(
            detect_agent("`@m · id`\nYour fix does not cover pi."),
            Some("correct")
        );
        assert_eq!(detect_agent("Did your fix cover pi? Please check."), None);

        // AC2: the scrub.
        for secret in [
            "sk-abcdefghijklmnopqrstuvwxyz012345",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV",
            "AKIAIOSFODNN7EXAMPLE",
            concat!("xox", "b-123456789-abcdefghijklmnop"),
            "ZAI_API_KEY=0123456789abcdef0123456789abcdef",
        ] {
            let mut counts = ScrubCounts::default();
            let (quote, status) = scrub_quote(&format!("use {secret} now"), &mut counts);
            assert_eq!(status, "kept", "{secret}");
            let quote = quote.unwrap();
            assert!(quote.contains("[redacted]"), "{secret}: {quote}");
            assert!(
                !quote.contains(&secret[secret.len() - 10..]),
                "{secret}: {quote}"
            );
        }
        let mut counts = ScrubCounts::default();
        assert_eq!(
            scrub_quote("no, read ~/.netrc first", &mut counts),
            (None, "dropped")
        );
        assert_eq!(counts.sensitive_drops, 1);
        // A pasted block stays out of the quote.
        let (quote, _) = scrub_quote(
            "no, this:\n<pasted_content>bank</pasted_content>",
            &mut counts,
        );
        assert_eq!(quote.as_deref(), Some("no, this:"));
    }
}

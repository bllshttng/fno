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
    let capped: String = quote.chars().take(280).collect();
    let scrubbed = scrub_string(&capped, counts);
    let text = scrubbed.to_string();
    let residual = crate::intel_html::secret_rx()
        .find_iter(&text)
        .any(|m| !m.as_str().contains("[redacted]"))
        || email_rx().is_match(&text);
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
    fn a_question_is_never_a_correction_and_a_leading_marker_is() {
        // AC1
        assert!(detect_user("wait, did you run it?", false).is_none());
        let c = detect_user("no, use the other branch", false).expect("a correction");
        assert_eq!(c.kind, "correct");
        // The question mark sits mid-turn: the first sentence decides.
        assert!(detect_user("actually, why not the other branch?", false).is_none());
    }

    #[test]
    fn a_bare_no_after_a_question_is_an_answer_but_no_comma_still_counts() {
        assert!(detect_user("no", true).is_none());
        let c = detect_user("no, use the other branch", true).expect("still a correction");
        assert_eq!(c.kind, "correct");
        assert!(c.after_question);
    }

    #[test]
    fn reject_keeps_its_kind_and_the_rule_stays_corpus_exact() {
        let c = detect_user("stop", false).expect("a refusal");
        assert_eq!(c.kind, "reject");
        // The unfiltered rule is unchanged: the corpus's own labels hold.
        assert_eq!(reaction_rule("yes go"), Some("approve"));
        assert_eq!(reaction_rule("nope"), Some("reject"));
        assert_eq!(reaction_rule("no, use the other branch"), Some("correct"));
        assert_eq!(reaction_rule("what about the tests?"), None);
    }

    #[test]
    fn a_secret_shape_never_reaches_the_quote_and_an_env_form_drops_it() {
        // AC2: every shape class the machine's routes carry.
        for secret in [
            "sk-abcdefghijklmnopqrstuvwxyz012345",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV",
            "AKIAIOSFODNN7EXAMPLE",
            concat!("xox", "b-123456789-abcdefghijklmnop"),
            "ZAI_API_KEY=0123456789abcdef0123456789abcdef",
            "https://user:hunter2secret@example.com/x",
        ] {
            let mut counts = ScrubCounts::default();
            let (quote, status) = scrub_quote(&format!("use {secret} now"), &mut counts);
            assert_eq!(status, "kept", "masked, not dropped: {secret}");
            assert!(
                quote.unwrap_or_default().contains("[redacted]"),
                "shape masked: {secret}"
            );
        }
        // A residual the masks cannot name drops the quote; the row lives.
        let mut counts = ScrubCounts::default();
        let raw = "token abc123def456 - keep going";
        let (quote, status) = scrub_quote(raw, &mut counts);
        assert_eq!(status, "dropped");
        assert!(quote.is_none());
        assert_eq!(counts.residual_drops, 1);
    }

    #[test]
    fn agent_status_leads_never_read_as_corrections() {
        assert!(agent_status_lead("No change: ask open"));
        assert!(agent_status_lead("No new findings this round."));
        assert!(!agent_status_lead("no, the PR is still red"));
    }
}

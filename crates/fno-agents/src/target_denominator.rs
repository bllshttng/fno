//! The scope denominator and its derivation from node details (the port of
//! `fno/target/denominator.py`). Multi-deliverable scope cannot be DETECTED
//! from node prose with high recall, so the derivation reads only the
//! unambiguous structures (parenthesized ordinals, a numbered list, a
//! two-member construction) and answers 1 when none fires. A non-fire of
//! [`enumerated_scope`] asserts nothing.

use std::sync::OnceLock;

use regex::Regex;

const CARDINAL_WORDS: &[&str] = &[
    "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];
// Words ending in 's' that are not plural nouns. Small on purpose: the
// controls carry no cardinals, so this only shapes organic-node false
// positives, which the derived-1 ratio measurement watches after landing.
const NON_PLURAL: &[&str] = &[
    "this", "thus", "was", "has", "his", "its", "plus", "versus", "always", "else", "since",
    "once", "some", "more", "less", "over", "into", "onto", "because", "across", "towards", "is",
    "as", "us",
];

fn ordinal_paren() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\((\d{1,2})\)").unwrap())
}

fn ordinal_list() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // The line-start anchor keeps "see section 21." and a sentence ending in a
    // node id from reading as enumeration.
    RE.get_or_init(|| Regex::new(r"(?m)^\s*(\d{1,2})\.\s").unwrap())
}

fn both_and() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bboth\b\s+[A-Za-z][^.]{0,60}?\s+and\s+[A-Za-z]").unwrap())
}

fn word_or_digits() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z]+|\d+").unwrap())
}

/// A coarse plural-noun test: a content word ending in 's' (not 'ss').
fn plural_noun(tok: &str) -> bool {
    let t = tok
        .trim()
        .trim_matches(['.', ',', ';', ':', '(', ')', '"', '\''])
        .to_ascii_lowercase();
    t.len() >= 3 && t.ends_with('s') && !t.ends_with("ss") && !NON_PLURAL.contains(&t.as_str())
}

/// Every distinct ordinal marker: `(1) ... (2)` or a numbered list.
fn ordinal_numbers(text: &str) -> Vec<u32> {
    let mut nums: Vec<u32> = Vec::new();
    for cap in ordinal_paren().captures_iter(text) {
        let n = cap[1].parse::<u32>().unwrap_or(0);
        if !nums.contains(&n) {
            nums.push(n);
        }
    }
    for cap in ordinal_list().captures_iter(text) {
        let n = cap[1].parse::<u32>().unwrap_or(0);
        if !nums.contains(&n) {
            nums.push(n);
        }
    }
    nums
}

/// Two or more distinct ordinal markers.
fn ordinal_run(text: &str) -> bool {
    ordinal_numbers(text).len() >= 2
}

/// A cardinal 2-10 (word or digit) with a plural noun within three tokens.
fn cardinal_governs_plural(text: &str) -> bool {
    let tokens: Vec<&str> = word_or_digits()
        .find_iter(text)
        .map(|m| m.as_str())
        .collect();
    for (i, tok) in tokens.iter().enumerate() {
        let lower = tok.to_ascii_lowercase();
        let is_cardinal = CARDINAL_WORDS.contains(&lower.as_str())
            || (tok.chars().all(|c| c.is_ascii_digit())
                && matches!(tok.parse::<u32>(), Ok(v) if (2..=10).contains(&v)));
        if is_cardinal
            && tokens[i + 1..(i + 4).min(tokens.len())]
                .iter()
                .any(|w| plural_noun(w))
        {
            return true;
        }
    }
    false
}

/// True iff title+details carry an unambiguous multi-deliverable enumeration.
/// The weak detectors (cardinal-governs-plural, both-and) read the TITLE only:
/// in a details body they fire on measured incident narrative. Ordinals keep
/// the full text, where they stay unambiguous.
pub fn enumerated_scope(title: &str, details: &str) -> bool {
    let text = format!("{title}\n{details}");
    ordinal_run(&text) || cardinal_governs_plural(title) || both_and().is_match(title)
}

/// The deliverables count a plan-less code node stamps at init. The node's
/// own enumeration decides: the highest ordinal marker wins (gaps count), a
/// two-member construction reads as 2, and anything else is the honest floor
/// of one deliverable.
pub fn derive_deliverables(title: &str, details: &str) -> i64 {
    let text = format!("{title}\n{details}");
    let nums = ordinal_numbers(&text);
    if !nums.is_empty() {
        return *nums.iter().max().unwrap() as i64;
    }
    if both_and().is_match(title) || cardinal_governs_plural(title) {
        return 2;
    }
    1
}

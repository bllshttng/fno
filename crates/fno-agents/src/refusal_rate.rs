//! Refusal rate over the trailing N tool calls: the cheapest available proxy
//! for context degradation, no model introspection needed. Feeds the
//! `refusal_rate` reading in `lead_checkin.rs`.
//!
//! The bucket set is the union of the 7 patterns proven against a live
//! lead transcript (gate
//! refusals, usage errors, style-lint refusals, fno guard refusals,
//! timeouts, parse errors, operator rejections). The reading reports one
//! rate, not a per-bucket breakdown.
//!
//! Reads claude-shaped and codex rollout texts. While machine_watch reads
//! hot, `discount_load` drops the two load-caused buckets - `timeout` and
//! `gate_refusal` - from the numerator: they rise with machine load, not
//! lead quality (a 14.5 to 18 percent rise measured under load 100 to 200).
//!
//! A result marked `is_error: true` is always refused. Anything else -
//! `is_error: false`, or no flag at all (codex has none) - still has to
//! pass a text check, because a clean exit is not proof of a clean answer:
//! `spawn-gate` exits 0 even when its own answer IS a refusal ("a refusal
//! is data now, not a process exit", `spawn_gate_verb.rs`), so the words
//! are the whole, short output. What a `false`/absent flag DOES rule out
//! is matching those same words buried deep in an unrelated, multi-line
//! successful read (the lead check-in's own capacity line, which quotes a
//! past gate verdict such as "gate refused on provider_cap") - so the text
//! check only looks at a short HEADER lead, not the whole thing. One
//! function, `is_refused`, makes this call for both this reading and
//! `lead_eval.rs`'s identical one, so the two cannot drift apart.

use serde_json::{json, Value};
use std::sync::OnceLock;

pub(crate) const REFUSAL_BUCKETS: [(&str, &str); 7] = [
    ("style_lint", r"style-exception|rule \d+ \("),
    ("fno_guard", r"guard\]|lead-delegation-guard"),
    (
        "gate_refusal",
        r#""status": "refused"|spawn-gate:|provider_cap|gate_mutex|cpu_share_undecidable|registry_schema|refused:"#,
    ),
    (
        "usage_error",
        r"No such option|Usage:|command not found|Error: No such|validation error|is required",
    ),
    (
        "timeout",
        r"timed out|did not complete within|Command timed out",
    ),
    (
        "parse_error",
        r"Traceback|JSONDecodeError|SyntaxError|KeyError|zsh:",
    ),
    ("user_reject", r"doesn't want to proceed"),
];

/// The first N chars of a tool_result's content text that the regex reads.
/// Matches the 4000-char lead the lead-control retro measured against.
/// `buckets_of` uses this WIDE lead to CATEGORIZE a call already known to
/// be refused; `is_refused` below uses the much narrower `HEADER_LEAD_CHARS`
/// to decide refused-or-not in the first place.
const CONTENT_LEAD_CHARS: usize = 4000;

/// The lead checked to decide whether an `is_error: false` (or absent)
/// result is itself a refusal. A genuine refusal answer fronts its marker
/// at the very start - `spawn-gate:`, a guard tag, `"status": "refused"` -
/// because the marker-bearing line IS the whole short answer. A successful
/// multi-line read that merely reports on a past gate event (the check-in's
/// capacity line) buries the same words well past this lead, which is what
/// tells the two apart.
const HEADER_LEAD_CHARS: usize = 100;

static BUCKET_SET: OnceLock<regex::RegexSet> = OnceLock::new();

fn bucket_set() -> &'static regex::RegexSet {
    BUCKET_SET.get_or_init(|| {
        regex::RegexSet::new(REFUSAL_BUCKETS.map(|(_, pattern)| pattern))
            .expect("refusal bucket patterns are valid")
    })
}

pub(crate) fn buckets_of(text: &str) -> impl Iterator<Item = &'static str> + '_ {
    let matches = bucket_set().matches(lead(text, CONTENT_LEAD_CHARS));
    REFUSAL_BUCKETS
        .iter()
        .enumerate()
        .filter_map(move |(idx, (bucket, _))| matches.matched(idx).then_some(*bucket))
}

/// `is_error: true` is always refused, whatever the text says. Everything
/// else - `false`, or no flag at all - still runs the HEADER check: a clean
/// exit is not proof of a clean answer (`spawn-gate` exits 0 even when its
/// own answer is a refusal), but it DOES rule out a match that only shows
/// up once the header lead is behind it.
pub(crate) fn is_refused(text: &str, is_error: Option<bool>) -> bool {
    is_error == Some(true) || bucket_set().is_match(lead(text, HEADER_LEAD_CHARS))
}

/// The first `max_chars` characters of `text`, split on a char boundary.
/// A byte-offset slice (`&text[..N]`) panics when `N` lands inside a
/// multi-byte UTF-8 character - real tool output routinely carries one
/// (an em dash, a checkmark, a box-drawing glyph), so this reader must
/// never assume ASCII.
fn lead(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &text[..byte_idx],
        None => text,
    }
}

/// Each trailing call paired with its result text and, when the transcript
/// shape carries one, the result's structural `is_error` flag.
fn trailing_calls(harness: &str, text: &str, window: usize) -> Vec<Option<(String, Option<bool>)>> {
    if harness == "codex" {
        codex_trailing_calls(text, window)
            .into_iter()
            .map(|call| call.map(|text| (text, None)))
            .collect()
    } else {
        claude_trailing_calls(text, window)
    }
}

/// The claude-shaped pairing: calls join to results by tool_use id, and a
/// result may land rows after its call.
fn claude_trailing_calls(text: &str, window: usize) -> Vec<Option<(String, Option<bool>)>> {
    let mut order: Vec<String> = Vec::new();
    let mut results: std::collections::HashMap<String, (String, Option<bool>)> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let mut entries = Vec::new();
        crate::lead_hygiene::claude_row_entries(&value, &mut entries);
        order.extend(
            entries
                .iter()
                .filter(|entry| entry.kind == "tool_use")
                .filter_map(|entry| entry.tool_use_id.clone()),
        );
        if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    continue;
                }
                let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                    continue;
                };
                let text = match block.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(""),
                    _ => String::new(),
                };
                let is_error = block.get("is_error").and_then(Value::as_bool);
                results.insert(id.to_string(), (text, is_error));
            }
        }
    }
    let start = order.len().saturating_sub(window);
    order
        .into_iter()
        .skip(start)
        .map(|id| results.remove(&id))
        .collect()
}

/// The codex rollout pairing: call and output rows share a `call_id`, so
/// each output row answers the call carrying its id; rows without one (the
/// legacy shapes, `local_shell_call`) fall back to answering the newest
/// call still missing a result.
fn codex_trailing_calls(text: &str, window: usize) -> Vec<Option<String>> {
    let mut calls: Vec<Option<String>> = Vec::new();
    let mut call_ids: Vec<Option<String>> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = value.get("payload") else {
            continue;
        };
        match payload.get("type").and_then(Value::as_str).unwrap_or("") {
            "function_call" | "custom_tool_call" | "local_shell_call" => {
                calls.push(None);
                call_ids.push(
                    payload
                        .get("call_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                );
            }
            "custom_tool_call_output" | "function_call_output" => {
                let out = crate::session_activity::codex_output_text(payload).into_owned();
                let call_id = payload.get("call_id").and_then(Value::as_str);
                let slot = match call_id {
                    Some(id) => call_ids
                        .iter()
                        .position(|slot_id| slot_id.as_deref() == Some(id))
                        .and_then(|idx| calls.get_mut(idx)),
                    None => calls.iter_mut().rev().find(|slot| slot.is_none()),
                };
                if let Some(slot) = slot {
                    *slot = Some(out);
                }
            }
            _ => {}
        }
    }
    let start = calls.len().saturating_sub(window);
    calls.into_iter().skip(start).collect()
}

/// Refusal rate over the trailing `window` tool calls in one transcript
/// text of `harness` ("claude" or "codex").
///
/// Never errors on a transcript that merely has fewer than `window` calls -
/// it reports against however many exist, and `"window"` in the return
/// value is that ACTUAL count, so the printed line never claims a
/// denominator it did not have.
///
/// `discount_load` (machine_watch reads hot) makes the headline `rate` the
/// load-free one: the `timeout` and `gate_refusal` buckets leave the
/// numerator, and `rate_full` keeps the raw rate so a reader can see both.
pub fn rate_from_text(
    harness: &str,
    text: &str,
    window: usize,
    discount_load: bool,
) -> Result<Value, String> {
    let trailing = trailing_calls(harness, text, window);
    let total = trailing.len();
    // `refused` decides per-call via `is_refused` (the structural flag when
    // there is one); `buckets_of` is also collected per call to CATEGORIZE
    // an already-refused call for the load discount below, but on its own
    // it never decides refused-or-not, so a successful read's buried gate
    // words can no longer flip a call that carries `is_error: false`.
    let calls: Vec<(bool, Vec<&'static str>)> = trailing
        .iter()
        .map(|call| match call {
            None => (false, Vec::new()),
            Some((text, is_error)) => (is_refused(text, *is_error), buckets_of(text).collect()),
        })
        .collect();
    let refused_full = calls.iter().filter(|(refused, _)| *refused).count();
    let (refused, discounted) = if discount_load {
        (
            calls
                .iter()
                .filter(|(refused, buckets)| {
                    *refused
                        && (buckets.is_empty()
                            || buckets
                                .iter()
                                .any(|bucket| *bucket != "timeout" && *bucket != "gate_refusal"))
                })
                .count(),
            true,
        )
    } else {
        (refused_full, false)
    };
    let ratio = |refused: usize| {
        if total == 0 {
            0.0
        } else {
            refused as f64 / total as f64
        }
    };
    Ok(json!({
        "rate": ratio(refused),
        "rate_full": ratio(refused_full),
        "refused": refused,
        "total": total,
        "window": total,
        "load_discounted": discounted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn transcript_line(tool_use_id: &str, name: &str, result: Option<&str>) -> String {
        let mut lines = Vec::new();
        lines.push(
            json!({
                "message": {
                    "content": [{"type": "tool_use", "id": tool_use_id, "name": name}]
                }
            })
            .to_string(),
        );
        if let Some(result) = result {
            lines.push(
                json!({
                    "message": {
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": result,
                        }]
                    }
                })
                .to_string(),
            );
        }
        lines.join("\n")
    }

    /// Like `transcript_line`, but the result row also carries the
    /// structural `is_error` flag a real claude transcript always sets.
    fn transcript_line_with_error(
        tool_use_id: &str,
        name: &str,
        result: &str,
        is_error: bool,
    ) -> String {
        let call = json!({
            "message": {
                "content": [{"type": "tool_use", "id": tool_use_id, "name": name}]
            }
        })
        .to_string();
        let outcome = json!({
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "is_error": is_error,
                    "content": result,
                }]
            }
        })
        .to_string();
        format!("{call}\n{outcome}")
    }

    fn write_transcript(lines: &[String]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", lines.join("\n")).unwrap();
        file
    }

    #[test]
    fn counts_refusals_by_combined_bucket_regex() {
        let lines = vec![
            transcript_line("t1", "Bash", Some("Usage: fno backlog get <id>")),
            transcript_line("t2", "Bash", Some("all good, done")),
            transcript_line("t3", "Bash", Some("[fno recursive-grep guard] refused")),
        ];
        let file = write_transcript(&lines);
        let result = rate_from_text(
            "claude",
            &std::fs::read_to_string(file.path()).unwrap(),
            200,
            false,
        )
        .unwrap();
        assert_eq!(result["total"], 3);
        assert_eq!(result["refused"], 2);
        assert_eq!(result["window"], 3);
        assert!((result["rate"].as_f64().unwrap() - (2.0 / 3.0)).abs() < 1e-9);
        assert_eq!(result["load_discounted"], false);
        // The tool-activity fold shares this fixture: a claude pair counts one
        // call, the result's is_error flag grades the failure, appended bytes
        // parse once, a partial tail holds, and a shrunken file resets.
        use crate::tool_activity::ToolFold;
        let dir = std::env::temp_dir().join("fno-refusal-tool-claude");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fold.jsonl");
        std::fs::write(&path, transcript_line("t1", "Bash", None) + "\n").unwrap();
        let mut fold = ToolFold::default();
        fold.absorb(&path, "claude");
        assert_eq!(
            (fold.calls, fold.errors),
            (1, 0),
            "a call with no result never reads as failed"
        );
        let mut with_flag = serde_json::json!({
            "message": {"content": [
                {"type": "tool_use", "id": "t9", "name": "Bash"},
                {"type": "tool_result", "tool_use_id": "t9", "is_error": true, "content": "boom"}
            ]}
        })
        .to_string();
        with_flag.push('\n');
        let base = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &(base.clone() + &with_flag)).unwrap();
        fold.absorb(&path, "claude");
        assert_eq!((fold.calls, fold.errors), (2, 1));
        let held = fold.offset;
        std::fs::write(&path, &(base + &with_flag + "{\"partial")).unwrap();
        fold.absorb(&path, "claude");
        assert_eq!(fold.offset, held, "a partial tail never parses half a row");
        std::fs::write(&path, "").unwrap();
        fold.absorb(&path, "claude");
        assert_eq!(
            (fold.calls, fold.errors, fold.offset),
            (0, 0, 0),
            "a rotated file resets the fold"
        );
    }

    /// The codex rollout pairing: an output answers its `call_id` when the
    /// rows carry one (real rollouts do), positionally otherwise, and the
    /// load discount drops the timeout bucket while the usage-error bucket
    /// stays counted.
    #[test]
    fn codex_rows_pair_by_call_id_and_the_load_discount_drops_load_buckets() {
        let codex_line = |ptype: &str, call_id: &str, text: &str| {
            let mut payload = json!({"type": ptype});
            if !call_id.is_empty() {
                payload["call_id"] = json!(call_id);
            }
            if !text.is_empty() {
                payload["output"] = json!(text);
            }
            json!({"payload": payload}).to_string()
        };
        // Out-of-order outputs: the first output answers the SECOND call by
        // id; the id-less output falls back to the newest unanswered call.
        let text = [
            codex_line("function_call", "c1", ""),
            codex_line("function_call", "c2", ""),
            codex_line("function_call_output", "c2", "Usage: bad args"),
            codex_line("function_call_output", "", "Command timed out after 30m"),
        ]
        .join("\n");
        let full = rate_from_text("codex", &text, 200, false).unwrap();
        assert_eq!(full["total"], 2);
        assert_eq!(full["refused"], 2);
        let discounted = rate_from_text("codex", &text, 200, true).unwrap();
        assert_eq!(discounted["load_discounted"], true);
        assert_eq!(discounted["refused"], 1, "the timeout bucket left");
        assert_eq!(discounted["rate_full"], full["rate"]);
        // The tool-activity fold shares this rollout fixture: call rows count,
        // and an output grades failed by the same buckets.
        use crate::tool_activity::ToolFold;
        let dir = std::env::temp_dir().join("fno-refusal-tool-codex");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fold.jsonl");
        std::fs::write(&path, text + "\n").unwrap();
        let mut fold = ToolFold::default();
        fold.absorb(&path, "codex");
        assert_eq!(fold.calls, 2);
        assert_eq!(fold.errors, 2, "both outputs matched a bucket");
    }

    #[test]
    fn trailing_window_drops_older_calls() {
        let mut lines = Vec::new();
        for i in 0..5 {
            lines.push(transcript_line(
                &format!("t{i}"),
                "Bash",
                Some("Usage: bad"),
            ));
        }
        lines.push(transcript_line("t5", "Bash", Some("clean result")));
        let file = write_transcript(&lines);
        let result = rate_from_text(
            "claude",
            &std::fs::read_to_string(file.path()).unwrap(),
            1,
            false,
        )
        .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["refused"], 0);
    }

    #[test]
    fn an_interrupted_call_with_no_result_counts_toward_total_never_refused() {
        let lines = vec![transcript_line("t1", "Bash", None)];
        let file = write_transcript(&lines);
        let result = rate_from_text(
            "claude",
            &std::fs::read_to_string(file.path()).unwrap(),
            200,
            false,
        )
        .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["refused"], 0);
    }

    /// A lead's own check-in read of its capacity line ("gate refused on
    /// provider_cap") is a SUCCESSFUL Bash result reporting on a past gate
    /// event, not a refusal of the read itself - the words are buried well
    /// past the header lead, in a multi-line report. The same words as a
    /// call's OWN short answer, though, must still count: `spawn-gate`
    /// exits 0 even when its answer is a refusal, so `is_error: false`
    /// alone can never clear a call either - only the header position does.
    #[test]
    fn is_error_false_clears_a_buried_refusal_but_not_the_calls_own_refusal_answer() {
        let checkin_report = "machine: beat 12 clean, context 42%, drift 1.2, tokens 128 out\n\
             telemetry: ok, queue depth 0, otel exporter healthy\n\
             board: open_prs 2, free_claim_no_driver 0, blocked 0, ready 4\n\
             capacity: footprint admit / gate refused on provider_cap, cpu pass";
        let lines = vec![transcript_line_with_error(
            "t1",
            "Bash",
            checkin_report,
            false,
        )];
        let file = write_transcript(&lines);
        let result = rate_from_text(
            "claude",
            &std::fs::read_to_string(file.path()).unwrap(),
            200,
            false,
        )
        .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(
            result["refused"], 0,
            "a successful read is never refused by gate words buried past its header"
        );

        // spawn-gate's own answer IS a refusal, as its first words, despite
        // exiting 0 (is_error: false) by design - the header still catches it.
        let own_answer_line = vec![transcript_line_with_error(
            "t2",
            "Bash",
            "spawn-gate: refusing reserve w1: provider_cap already held",
            false,
        )];
        let own_answer_file = write_transcript(&own_answer_line);
        let own_answer_result = rate_from_text(
            "claude",
            &std::fs::read_to_string(own_answer_file.path()).unwrap(),
            200,
            false,
        )
        .unwrap();
        assert_eq!(
            own_answer_result["refused"], 1,
            "an exit-0 refusal answer still counts - is_error: false is not proof of a clean answer"
        );

        // The same words from an is_error: true call count too - the flag
        // alone is also sufficient, whatever the text says.
        let refused_line = vec![transcript_line_with_error("t3", "Bash", "boom", true)];
        let refused_file = write_transcript(&refused_line);
        let refused_result = rate_from_text(
            "claude",
            &std::fs::read_to_string(refused_file.path()).unwrap(),
            200,
            false,
        )
        .unwrap();
        assert_eq!(refused_result["refused"], 1);
    }

    // A byte-offset slice at exactly CONTENT_LEAD_CHARS bytes would panic
    // here (multi-byte char straddling the cut); `lead` must split on a
    // char boundary instead.
    #[test]
    fn a_multibyte_character_at_the_lead_boundary_does_not_panic() {
        let mut content = "x".repeat(CONTENT_LEAD_CHARS - 1);
        content.push('—'); // em dash: 3 UTF-8 bytes, straddles the cut
        content.push_str("Usage: this text is past the lead and unread");
        let lines = vec![transcript_line("t1", "Bash", Some(&content))];
        let file = write_transcript(&lines);
        let result = rate_from_text(
            "claude",
            &std::fs::read_to_string(file.path()).unwrap(),
            200,
            false,
        )
        .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["refused"], 0, "the matching text sits past the lead");
    }
}

//! Observer digest writers: the human-readable `.md` a sweep or replay
//! leaves under the observer-reports digests (vault `internal/<owner>/`
//! when a vault is configured, `~/.fno/observer-reports/<owner>/` when not).
//! Ported from `cli/src/fno/observer/cli.py` (`_write_digest`,
//! `_write_target_digest`, `_falsifier_crosstab`).
//!
//! The body builders are pure functions of the summary/coverage/meta, so the
//! digests are testable without touching disk; the write is one
//! `std::fs::write` in the caller.

use serde_json::{json, Map, Value};

/// `_OUTCOMES` (cli.py:694).
const OUTCOMES: [&str; 3] = ["merged_clean", "bounced", "reverted"];

/// The per-outcome tally cell for one process bucket.
fn tally(scored: &[(Value, Value)], good: bool) -> Map<String, Value> {
    let mut cell: Map<String, Value> = Map::new();
    for (item, scores) in scored {
        let Some(outcome) = item.get("outcome").and_then(Value::as_str) else {
            continue;
        };
        if !OUTCOMES.contains(&outcome) {
            continue;
        }
        let item_good = scores.get("first_try_green").and_then(Value::as_str) == Some("pass")
            && scores.get("converged").and_then(Value::as_str) == Some("pass");
        if item_good == good {
            let key = outcome.to_string();
            let n = cell.get(&key).and_then(Value::as_u64).unwrap_or(0);
            cell.insert(key, json!(n + 1));
        }
    }
    cell
}

/// The PR numbers of the falsifier cell: good process AND a bad outcome -
/// a PR that scored clean yet bounced or reverted, the observation that
/// falsifies the good process score (Locked Decision 5).
fn falsifier_prs(scored: &[(Value, Value)]) -> Vec<i64> {
    let mut prs: Vec<i64> = Vec::new();
    for (item, scores) in scored {
        let outcome = item.get("outcome").and_then(Value::as_str);
        if outcome != Some("bounced") && outcome != Some("reverted") {
            continue;
        }
        let ftg = scores.get("first_try_green").and_then(Value::as_str);
        let cv = scores.get("converged").and_then(Value::as_str);
        let good = ftg == Some("pass") && cv == Some("pass");
        if !good {
            continue;
        }
        if let Some(n) = item.get("pr_number").and_then(Value::as_i64) {
            prs.push(n);
        }
    }
    prs
}

/// `_falsifier_crosstab`'s public shape (cli.py:697).
pub(crate) fn crosstab(scored: &[(Value, Value)]) -> Value {
    json!({
        "good_process": Value::Object(tally(scored, true)),
        "other": Value::Object(tally(scored, false)),
        "falsifier_prs": falsifier_prs(scored),
    })
}

/// The ranking lines of every digest: `- <dimension>: <n> fail`, or the
/// explicit no-failures line (cli.py's `ranking` join).
fn ranking_lines(summary: &Value) -> String {
    let default = vec![];
    let ranking = summary
        .get("failure_ranking")
        .and_then(Value::as_array)
        .unwrap_or(&default);
    if ranking.is_empty() {
        return "- (no failing dimensions)".to_string();
    }
    ranking
        .iter()
        .map(|r| {
            let dimension = r.get("dimension").and_then(Value::as_str).unwrap_or("?");
            let count = r.get("fail_count").and_then(Value::as_u64).unwrap_or(0);
            format!("- {dimension}: {count} fail")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `_write_digest`'s body (cli.py:261). `date` is the `%Y-%m-%d` stamp.
pub(crate) fn sweep_digest_body(summary: &Value, skill: &str, mode: &str, date: &str) -> String {
    let skill_id = summary
        .get("skill_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let run_id = summary.get("run_id").and_then(Value::as_str).unwrap_or("");
    let coverage_pct = summary
        .get("coverage_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let corpus_size = summary
        .get("corpus_size")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let pass = summary
        .get("pass_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let degraded = summary
        .get("degraded_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let fail = summary
        .get("fail_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let ref_line = summary
        .get("skill_ref")
        .and_then(Value::as_str)
        .map(|r| format!("\nskill_ref: `{r}`"))
        .unwrap_or_default();
    let caveat = if mode == "replay" {
        "\n> Replay scores structural dimensions only (Review Amendment A1); \
         shipped-outcome is confirmed by subsequent sweeps, not this run.\n"
    } else {
        ""
    };
    let evidence_line = summary
        .get("evidence_line")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut body = String::new();
    body.push_str(&format!("# Observer {mode}: {skill_id} ({date})\n\n"));
    body.push_str(&format!("run_id: `{run_id}`{ref_line}\n"));
    body.push_str(&format!(
        "coverage: {coverage_pct}% ({corpus_size} attributable items)\n"
    ));
    body.push_str(&format!(
        "verdicts: pass={pass} degraded={degraded} fail={fail}\n"
    ));
    if !evidence_line.is_empty() {
        body.push_str(&format!("{evidence_line}\n"));
    }
    body.push_str(caveat);
    body.push('\n');
    body.push_str("## Failure ranking\n");
    body.push_str(&ranking_lines(summary));
    body.push('\n');
    body
}

/// `_write_target_digest`'s body (cli.py:721). `date` is the `%Y-%m-%d`
/// stamp. The falsifier cross-tab and the no-PR class render in the summary
/// block directly under the verdict counts (the headline), never below the
/// failure ranking - a falsifier a reader must scroll for goes unread.
pub(crate) fn target_digest_body(
    summary: &Value,
    coverage: &Value,
    meta: &Value,
    crosstab: &Value,
    date: &str,
) -> String {
    let mut body = String::new();
    body.push_str(&format!("# Observer sweep: fno:target ({date})\n\n"));
    let run_id = summary.get("run_id").and_then(Value::as_str).unwrap_or("");
    body.push_str(&format!("run_id: `{run_id}`\n"));
    let prs_total = coverage
        .get("prs_total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    body.push_str(&format!(
        "denominator: {prs_total} merged+closed PR(s) in window \
         (anchor = real deliveries, NOT the ledger)\n"
    ));
    let pct = summary
        .get("coverage_pct")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let attributed = coverage
        .get("attributed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    body.push_str(&format!(
        "coverage: {pct}% ({attributed} attributable PR item(s))\n"
    ));
    let pass = summary
        .get("pass_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let degraded = summary
        .get("degraded_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let fail = summary
        .get("fail_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    body.push_str(&format!(
        "verdicts: pass={pass} degraded={degraded} fail={fail}\n\n"
    ));
    append_falsifier_block(&mut body, crosstab);
    append_attempt_block(&mut body, summary, coverage);
    append_caveat_block(&mut body, coverage, meta);
    body.push_str("## Failure ranking\n");
    body.push_str(&ranking_lines(summary));
    body.push('\n');
    body
}

/// The falsifier headline block: the falsifier line, then the process x
/// outcome table.
fn append_falsifier_block(body: &mut String, crosstab: &Value) {
    let fals = crosstab
        .get("falsifier_prs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    body.push_str("## Falsifier cross-tab (process x outcome)\n\n");
    if fals.is_empty() {
        body.push_str("0 good-process / bad-outcome (no falsifier this window)\n\n");
    } else {
        let list = fals
            .iter()
            .map(|p| format!("#{}", p))
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&format!(
            "**{}** good-process / bad-outcome (FALSIFIER): {}\n\n",
            fals.len(),
            list
        ));
    }
    body.push_str("| process | merged_clean | bounced | reverted |\n");
    body.push_str("|---|---|---|---|\n");
    let good = crosstab.get("good_process").cloned().unwrap_or(json!({}));
    let other = crosstab.get("other").cloned().unwrap_or(json!({}));
    body.push_str(&crosstab_row("good (first_try_green + converged)", &good));
    body.push_str(&crosstab_row("other", &other));
    body.push('\n');
}

/// One `| label | merged_clean | bounced | reverted |` table row
/// (cli.py's `_row`).
fn crosstab_row(label: &str, cell: &Value) -> String {
    let count = |k: &str| cell.get(k).and_then(Value::as_u64).unwrap_or(0);
    format!(
        "| {label} | {} | {} | {} |\n",
        count("merged_clean"),
        count("bounced"),
        count("reverted")
    )
}

/// The attempt -> PR (survivorship-corrected) block (cli.py:780).
fn append_attempt_block(body: &mut String, summary: &Value, coverage: &Value) {
    body.push_str("## Attempt -> PR (survivorship-corrected)\n\n");
    let no_pr = coverage
        .get("no_pr_attempts")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let attributed = coverage
        .get("attributed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let denom = attributed + no_pr;
    let attempt_to_pr = if denom > 0 {
        crate::observer::fold::pct(attributed as usize, denom as usize)
    } else {
        0
    };
    let stop = coverage
        .get("no_pr_stop_cause")
        .cloned()
        .unwrap_or(json!({}));
    let mut pairs: Vec<(String, u64)> = stop
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let stop_line = if pairs.is_empty() {
        "(none)".to_string()
    } else {
        pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    body.push_str(&format!(
        "no_pr_attempts: {no_pr} (stop-cause: {stop_line})\n"
    ));
    body.push_str(&format!(
        "attempt -> PR: {attempt_to_pr}% ({attributed} PRs / {denom} attempts)\n\n"
    ));
}

/// The coverage-gap caveats block (cli.py:753): `> coverage gap: ...` lines
/// between the attempt block and the failure ranking; a lone blank line when
/// nothing to state.
fn append_caveat_block(body: &mut String, coverage: &Value, meta: &Value) {
    let unattributed = coverage
        .get("unattributed_pr")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if unattributed > 0 {
        body.push_str(&format!(
            "> coverage gap: {unattributed} unattributed PR(s) (no resolvable node)\n"
        ));
    }
    let dropped = meta
        .get("dropped_repos")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if dropped > 0 {
        body.push_str(&format!(
            "> coverage gap: {dropped} repo(s) dropped by the gh fan-out cap\n"
        ));
    }
    let truncated = meta
        .get("truncated_repos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !truncated.is_empty() {
        let list = truncated
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&format!(
            "> coverage gap: repos hitting the list limit (possible more PRs): {list}\n"
        ));
    }
    // Python: caveat_line + "\n" always, plus a second newline when any
    // caveat rendered; when none did, the always-newline is the lone blank
    // line the layout leaves before the failure ranking.
    body.push('\n');
}

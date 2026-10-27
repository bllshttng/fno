//! The plan-fidelity join, ported from `cli/src/fno/scoreboard/fold.py`
//! (`_plan_key`, `_score_fidelity`, `_declared_probes`, `_probe_evidence`,
//! the diff/plan/summary readers, `evaluate_context_comparison`, and
//! `build_plan_fidelity`). One join, two readers: the plan-fidelity VIEW and
//! the Python gate (`fno.plan.fidelity`, which calls through the keeper op
//! `scoreboard_plan_fidelity` with `unmeasurable: refuse`).

use super::readers::{event_data, event_kind, parse_ts, pct};
use super::views::{is_planned_row, node_maps, node_outcome, row_shipped, RunInputs};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, HashSet};

/// `_AC_ID_RE`: `AC`, one or more digits, `-`, one or more ASCII capitals.
pub(crate) fn parse_ac_ids(doc: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let bytes = doc.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'A' {
            let mut j = i + 1;
            let mut digits = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
                digits += 1;
            }
            if digits > 0 && j < bytes.len() && bytes[j] == b'-' {
                let mut k = j + 1;
                let mut caps = 0;
                while k < bytes.len() && bytes[k].is_ascii_uppercase() {
                    k += 1;
                    caps += 1;
                }
                if caps > 0 {
                    ids.insert(doc[i..k].to_string());
                }
            }
        }
        i += 1;
    }
    ids
}

/// `_OWNERSHIP_FILE_RE` matches a backticked path: `` `([^`]+/[^`]+|[^`]+\.\w+)` ``.
/// `_parse_ownership_files`: file paths in the plan's `File Ownership Map`
/// table rows naming modify/create/delete.
pub(crate) fn parse_ownership_files(doc: &str) -> BTreeSet<String> {
    let Some(idx) = doc.find("File Ownership Map") else {
        return BTreeSet::new();
    };
    let section = &doc[idx..];
    let mut files = BTreeSet::new();
    for line in section.split('\n') {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            continue;
        }
        let lower = line.to_lowercase();
        if !lower.contains("modify") && !lower.contains("create") && !lower.contains("delete") {
            continue;
        }
        for mt in scan_backticked(line) {
            if is_ownership_path(&mt) {
                files.insert(mt);
            }
        }
    }
    files
}

/// Backticked spans in one line: `` `[^`]+` ``.
fn scan_backticked(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        let inner = &after[..end];
        if !inner.is_empty() {
            out.push(inner.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

/// `` [^`]+/[^`]+ `` (a path with a slash) or `` [^`]+\.\w+ `` (an extension).
fn is_ownership_path(candidate: &str) -> bool {
    if candidate.contains('/') {
        return candidate.matches('/').count() >= 1
            && candidate.split('/').all(|part| !part.is_empty());
    }
    match candidate.rfind('.') {
        Some(dot) if dot > 0 && dot + 2 <= candidate.len() => candidate[dot + 1..]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}

/// `_DATA_MODEL_RE` (case-insensitive): `migration|schema|(?:^|/)models?\.py|
/// \.sql|models?/|migrations?/`. A path is a data-model file when any
/// alternative appears as a substring at the right position.
pub(crate) fn is_data_model_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("migration")
        || lower.contains("schema")
        || contains_at_boundary(&lower, "models.py")
        || contains_at_boundary(&lower, "model.py")
        || lower.contains(".sql")
        || lower.contains("model/")
        || lower.contains("models/")
        || lower.contains("migration/")
}

/// `(?:^|/)models?\.py`: the name must start the path or follow a slash.
fn contains_at_boundary(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    while let Some(found) = haystack[start..].find(needle) {
        let at = start + found;
        if at == 0 || haystack.as_bytes()[at - 1] == b'/' {
            return true;
        }
        start = at + 1;
    }
    false
}

/// `_path_match`: equal, or one is the other's tail on a path-component
/// boundary (the guard that stops `some_other_fold.py` matching `fold.py`).
pub(crate) fn path_match(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// `_plan_key`: last two path segments, scoped by a `project` prefix, so a
/// recurring folder-plan filename does not collapse unrelated plans.
pub(crate) fn plan_key(plan_path: Option<&Value>, project: Option<&Value>) -> Option<String> {
    let raw = plan_path.and_then(Value::as_str)?;
    if raw.is_empty() {
        return None;
    }
    let file = raw.split('#').next().unwrap_or(raw);
    let mut parts: Vec<&str> = file.split('/').filter(|p| !p.is_empty()).collect();
    let name = parts.pop()?;
    let tail = match parts.pop() {
        Some(parent) => format!("{parent}/{name}"),
        None => name.to_string(),
    };
    if tail.is_empty() || tail == "/" {
        return None;
    }
    Some(format!(
        "{}::{tail}",
        project.and_then(Value::as_str).unwrap_or_default()
    ))
}

/// `_score_fidelity`: the three AC-required metrics + deviation load. Each
/// degrades to null when its input is missing - an unmeasurable plan is
/// never a 0% plan.
pub(crate) fn score_fidelity(
    plan_doc: Option<&str>,
    summary: Option<&str>,
    diff_files: Option<&[String]>,
) -> Value {
    let mut out = Map::new();
    let ac_cov = match (plan_doc, summary) {
        (Some(plan), Some(summary_text)) => {
            let acs = parse_ac_ids(plan);
            if acs.is_empty() {
                Value::Null
            } else {
                let total = acs.len() as i64;
                let verified = acs
                    .iter()
                    .filter(|ac| summary_text.contains(ac.as_str()))
                    .count() as i64;
                json!({"verified": verified, "total": total, "pct": pct(verified, total)})
            }
        }
        _ => Value::Null,
    };
    out.insert("ac_coverage".to_string(), ac_cov);
    let scope: (Value, Value) = match (plan_doc, diff_files) {
        (Some(plan), Some(files)) => {
            let owned = parse_ownership_files(plan);
            let unplanned: Vec<String> = files
                .iter()
                .filter(|f| !owned.iter().any(|o| path_match(f, o)))
                .cloned()
                .collect();
            let untouched: Vec<String> = owned
                .iter()
                .filter(|o| !files.iter().any(|f| path_match(f, o)))
                .cloned()
                .collect();
            let dm = unplanned.iter().filter(|f| is_data_model_file(f)).count() as i64;
            (
                json!({"unplanned": unplanned, "untouched": untouched}),
                json!(dm),
            )
        }
        _ => (Value::Null, Value::Null),
    };
    let (scope_drift, dm_surprise) = scope;
    out.insert("scope_drift".to_string(), scope_drift);
    out.insert("data_model_surprise".to_string(), dm_surprise);
    // Reasoned deviations are signal, not a penalty: counted, not scored.
    // The three counts ride different case rules, exactly as the Python
    // leg spelled them.
    let deviations: Value = match summary {
        Some(text) => json!(
            text.to_lowercase().matches("deviation").count() as i64
                + text.matches("<help").count() as i64
                + text.matches("STOP").count() as i64
        ),
        None => Value::Null,
    };
    out.insert("deviation_load".to_string(), deviations);
    Value::Object(out)
}

/// `_default_read_plan_doc`.
pub(crate) fn read_plan_doc(plan_path: Option<&Value>) -> Option<String> {
    let raw = plan_path.and_then(Value::as_str)?;
    let file = raw.split('#').next().unwrap_or(raw);
    std::fs::read_to_string(file).ok()
}

/// `_default_read_summary`: the summary_path file wins, the row's `summary`
/// field answers otherwise.
pub(crate) fn read_summary(row: &Value) -> Option<String> {
    if let Some(sp) = row.get("summary_path").and_then(Value::as_str) {
        if !sp.is_empty() {
            if let Ok(text) = std::fs::read_to_string(sp) {
                return Some(text);
            }
        }
    }
    row.get("summary")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `_default_read_diff`: `gh pr diff <n> --name-only`, repo-pinned from the
/// row's pr_url so the diff always targets the right repo.
pub(crate) fn read_diff(row: &Value) -> Option<Vec<String>> {
    let pr_number = row.get("pr_number")?;
    let falsy = pr_number.is_null()
        || pr_number.as_i64() == Some(0)
        || pr_number.as_str().map(str::is_empty).unwrap_or(false);
    if falsy {
        return None;
    }
    let pr_text = match pr_number {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["pr", "diff", &pr_text, "--name-only"]);
    if let Some(url) = row.get("pr_url").and_then(Value::as_str) {
        if let Some(slug) = repo_slug_from_url(url) {
            cmd.args(["--repo", &slug]);
        }
    }
    let out = crate::gh_budget::output(cmd).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// The `github.com/([^/]+/[^/]+?)(?:\.git)?/pull/` pin.
fn repo_slug_from_url(url: &str) -> Option<String> {
    let idx = url.find("github.com/")?;
    let rest = &url[idx + "github.com/".len()..];
    let cut = rest.find("/pull/")?;
    let mut slug = &rest[..cut];
    if let Some(stripped) = slug.strip_suffix(".git") {
        if slug.len() > 4 {
            slug = stripped;
        }
    }
    (!slug.is_empty() && slug.matches('/').count() == 1).then(|| slug.to_string())
}

/// `_declared_probes`: `done_probes` in the plan doc's frontmatter. Malformed
/// frontmatter declares nothing readable.
pub(crate) fn declared_probes(plan_doc: Option<&str>) -> Vec<String> {
    let Some(doc) = plan_doc else {
        return Vec::new();
    };
    let trimmed = doc.trim_start();
    if !trimmed.starts_with("---") {
        return Vec::new();
    }
    let body = match trimmed[3..].split_once("\n---") {
        Some((head, _)) => head,
        None => return Vec::new(),
    };
    let Ok(fm) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(body) else {
        return Vec::new();
    };
    let Some(probes) = fm.get("done_probes") else {
        return Vec::new();
    };
    let Some(list) = probes.as_sequence() else {
        return Vec::new();
    };
    list.iter()
        .map(|p| match p {
            serde_yaml_ng::Value::String(s) => s.clone(),
            other => serde_yaml_ng::to_string(other)
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        })
        .collect()
}

/// `_probe_evidence`: the delivery session's LAST recorded probe results,
/// keyed command to state. Last wins: only the fire that granted done is
/// evidence.
pub(crate) fn probe_evidence(loop_check_events: &[Value], session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|s| !s.is_empty()) else {
        return json!({});
    };
    let mut latest = serde_json::Map::new();
    for e in loop_check_events {
        let data = event_data(e);
        if data.get("session_id").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        if let Some(probes) = data.get("done_probes").filter(|p| p.is_object()) {
            latest = probes.as_object().cloned().unwrap_or_default();
        }
    }
    Value::Object(latest)
}

/// `_comparison_contract_from_events`: the last declared contract event.
pub(crate) fn comparison_contract_from_events(events: &[Value]) -> Option<Value> {
    let mut contracts: Vec<(Value, Option<Value>)> = Vec::new();
    for event in super::context_trace::normalized_events(events) {
        if !matches!(
            event_kind(&event),
            Some("context_comparison_declared") | Some("context_snapshot")
        ) {
            continue;
        }
        if let Some(contract) = event
            .get("data")
            .and_then(|d| d.get("comparison_contract"))
            .filter(|c| c.is_object())
        {
            let mut declared = contract.clone();
            if let Some(obj) = declared.as_object_mut() {
                obj.insert(
                    "recorded_at".to_string(),
                    event.get("ts").cloned().unwrap_or(Value::Null),
                );
            }
            contracts.push((declared, event.get("ts").cloned()));
        }
    }
    let (mut declared, _) = match contracts.pop() {
        Some(pair) => pair,
        None => return None,
    };
    let recorded_at = declared.get("recorded_at").cloned().unwrap_or(Value::Null);
    // Python's dict copy-then-assign keeps an existing key's position and
    // appends a new one; the preserve_order Map does the same.
    if let Some(obj) = declared.as_object_mut() {
        obj.insert("recorded_at".to_string(), recorded_at);
    }
    Some(declared)
}

/// `evaluate_context_comparison`: label a comparison only after its
/// falsifiable contract is complete. The rejection chain runs in the
/// Python leg's order, so both legs reject on the same first failure.
pub(crate) fn evaluate_context_comparison(
    traces: &[Value],
    contract: Option<&Value>,
    observation_complete: bool,
) -> Value {
    let rejected = |reason: &str| json!({"label": "rejected", "reason": reason});
    let Some(contract) = contract.filter(|c| c.is_object()) else {
        return rejected("comparison_contract_missing");
    };
    if !observation_complete {
        return rejected("event_observation_incomplete");
    }
    let required = [
        "declared_at",
        "recorded_at",
        "cohorts",
        "window",
        "exclusions",
        "budgets",
    ];
    if required
        .iter()
        .any(|key| contract.get(key).map(Value::is_null).unwrap_or(true))
    {
        return rejected("comparison_contract_incomplete");
    }
    let cohorts = contract.get("cohorts").filter(|c| c.is_object());
    let window = contract.get("window").filter(|w| w.is_object());
    let exclusions = contract.get("exclusions").filter(|e| e.is_array());
    let budgets = contract.get("budgets").filter(|b| b.is_object());
    if cohorts.is_none() || window.is_none() || exclusions.is_none() || budgets.is_none() {
        return rejected("comparison_contract_invalid");
    }
    // `budgets` was filtered to an object above, so the empty fallback is
    // unreachable; `&Map` has no Default to unwrap into.
    let empty_budgets = Map::new();
    let budgets_map = budgets.and_then(Value::as_object).unwrap_or(&empty_budgets);
    if budgets_map.is_empty() {
        return rejected("comparison_contract_invalid");
    }
    let baseline_ids = cohorts
        .and_then(|c| c.get("baseline").filter(|b| b.is_array()))
        .map(|b| b.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let candidate_ids = cohorts
        .and_then(|c| c.get("candidate").filter(|b| b.is_array()))
        .map(|b| b.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    if baseline_ids.is_empty() || candidate_ids.is_empty() {
        return rejected("cohort_members_missing");
    }
    let cohort_ok = |ids: &[Value]| -> bool {
        ids.iter()
            .all(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
            && ids
                .iter()
                .filter_map(Value::as_str)
                .collect::<HashSet<_>>()
                .len()
                == ids.len()
    };
    if !cohort_ok(&baseline_ids) || !cohort_ok(&candidate_ids) {
        return rejected("comparison_cohorts_invalid");
    }
    let declared = parse_ts(contract.get("declared_at"));
    let recorded = parse_ts(contract.get("recorded_at"));
    let start = parse_ts(window.and_then(|w| w.get("start")));
    let end = parse_ts(window.and_then(|w| w.get("end")));
    if declared.is_none() || recorded.is_none() || start.is_none() || end.is_none() {
        return rejected("comparison_window_invalid");
    }
    let (declared, recorded, start, end) = (
        declared.unwrap(),
        recorded.unwrap(),
        start.unwrap(),
        end.unwrap(),
    );
    if start > end {
        return rejected("comparison_window_invalid");
    }
    if declared > recorded || recorded > start {
        return rejected("comparison_not_predeclared");
    }
    if !budgets_map.values().all(finite_nonneg_number) {
        return rejected("comparison_budgets_invalid");
    }
    let supported = [
        "max_candidate_latency_minutes",
        "max_candidate_review_findings",
        "max_candidate_reverts",
        "max_candidate_spend_usd",
    ];
    if budgets_map.keys().any(|k| !supported.contains(&k.as_str())) {
        return rejected("comparison_budgets_unsupported");
    }
    let excluded: HashSet<String> = exclusions
        .and_then(Value::as_array)
        .map(|a| a.iter().map(json_str).collect())
        .unwrap_or_default();
    let members = |ids: &[Value]| -> (Vec<&Value>, HashSet<String>, bool) {
        let mut result = Vec::new();
        let mut realized: HashSet<String> = HashSet::new();
        let mut duplicate = false;
        for trace in traces {
            let sid = trace
                .get("worker_session")
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .unwrap_or_else(|| "None".to_string());
            let completed = parse_ts(trace.get("completed_at"));
            if let Some(done) = completed {
                if excluded.contains(&sid) {
                    continue;
                }
                if ids
                    .iter()
                    .map(json_str)
                    .collect::<HashSet<_>>()
                    .contains(&sid)
                    && start <= done
                    && done <= end
                {
                    if !realized.insert(sid.clone()) {
                        duplicate = true;
                    }
                    result.push(trace);
                }
            }
        }
        (result, realized, duplicate)
    };
    let (baseline, baseline_realized, baseline_duplicate) = members(&baseline_ids);
    let (candidate, candidate_realized, candidate_duplicate) = members(&candidate_ids);
    let disjoint = baseline_ids
        .iter()
        .map(json_str)
        .collect::<HashSet<_>>()
        .is_disjoint(&candidate_ids.iter().map(json_str).collect::<HashSet<_>>());
    if !disjoint {
        return rejected("cohorts_overlap");
    }
    if baseline_duplicate || candidate_duplicate {
        return rejected("cohort_members_ambiguous");
    }
    let wanted = |ids: &[Value]| -> HashSet<String> {
        ids.iter()
            .map(json_str)
            .collect::<HashSet<_>>()
            .difference(&excluded)
            .cloned()
            .collect()
    };
    if baseline_realized != wanted(&baseline_ids)
        || candidate_realized != wanted(&candidate_ids)
        || baseline.is_empty()
        || candidate.is_empty()
    {
        return rejected("cohort_members_missing");
    }
    let all_falsifiable = baseline
        .iter()
        .chain(candidate.iter())
        .all(|t| t.get("falsifiable") == Some(&Value::Bool(true)));
    if !all_falsifiable {
        return rejected("no_falsifiable_downstream_observation");
    }
    let complete_context = baseline.iter().chain(candidate.iter()).all(|t| {
        t.get("context")
            .filter(|c| c.is_object())
            .and_then(|c| c.get("measurement_complete"))
            .map(|v| v.as_bool() == Some(true))
            .unwrap_or(true)
    });
    if !complete_context {
        return rejected("context_measurement_incomplete");
    }
    let base_bytes = median_metric(&baseline, &["context", "bytes"]);
    let cand_bytes = median_metric(&candidate, &["context", "bytes"]);
    let base_latency = median_metric(&baseline, &["outcomes", "latency_minutes"]);
    let cand_latency = median_metric(&candidate, &["outcomes", "latency_minutes"]);
    let mut budget_observations = serde_json::Map::new();
    let mut budget_satisfied = true;
    if let Some(limit) = budgets_map.get("max_candidate_spend_usd") {
        let mut total = 0.0f64;
        for trace in &candidate {
            match finite_nonneg_float(
                trace
                    .get("outcomes")
                    .and_then(|o| o.get("spend_usd"))
                    .unwrap_or(&Value::Null),
            ) {
                Some(v) => total += v,
                None => return rejected("comparison_budget_measurement_missing"),
            }
        }
        if !total.is_finite() {
            return rejected("comparison_budget_measurement_missing");
        }
        budget_observations.insert(
            "max_candidate_spend_usd".to_string(),
            json!(super::views::round2(total)),
        );
        if total > limit.as_f64().unwrap_or(f64::INFINITY) {
            budget_satisfied = false;
        }
    }
    if let Some(limit) = budgets_map.get("max_candidate_latency_minutes") {
        let latencies: Vec<Option<&Value>> = candidate
            .iter()
            .map(|t| t.get("outcomes").and_then(|o| o.get("latency_minutes")))
            .collect();
        if latencies
            .iter()
            .any(|v| !finite_nonneg_number(v.unwrap_or(&Value::Null)))
        {
            return rejected("comparison_budget_measurement_missing");
        }
        let max = latencies
            .iter()
            .filter_map(|v| v.and_then(Value::as_f64))
            .fold(f64::NEG_INFINITY, f64::max);
        budget_observations.insert("max_candidate_latency_minutes".to_string(), json!(max));
        if max > limit.as_f64().unwrap_or(f64::INFINITY) {
            budget_satisfied = false;
        }
    }
    if let Some(limit) = budgets_map.get("max_candidate_review_findings") {
        let reviews: Vec<Value> = candidate
            .iter()
            .map(|t| {
                t.get("outcomes")
                    .and_then(|o| o.get("review"))
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            })
            .collect();
        let ok = reviews.iter().all(|r| {
            r.get("observed").and_then(Value::as_bool).unwrap_or(false)
                && r.get("findings")
                    .map(|f| f.is_i64() && f.as_i64().unwrap() >= 0)
                    .unwrap_or(false)
        });
        if !ok {
            return rejected("comparison_budget_measurement_missing");
        }
        let total_findings: i64 = reviews
            .iter()
            .filter_map(|r| r.get("findings").and_then(Value::as_i64))
            .sum();
        match finite_nonneg_float(&json!(total_findings)) {
            Some(v) => {
                budget_observations.insert("max_candidate_review_findings".to_string(), json!(v))
            }
            None => return rejected("comparison_budget_measurement_missing"),
        };
        if total_findings as f64 > limit.as_f64().unwrap_or(f64::INFINITY) {
            budget_satisfied = false;
        }
    }
    if let Some(limit) = budgets_map.get("max_candidate_reverts") {
        let reverts: Vec<Value> = candidate
            .iter()
            .map(|t| {
                t.get("outcomes")
                    .and_then(|o| o.get("revert"))
                    .cloned()
                    .unwrap_or_else(|| json!({}))
            })
            .collect();
        let ok = reverts.iter().all(|r| {
            r.get("observed").and_then(Value::as_bool).unwrap_or(false)
                && r.get("reverted").map(Value::is_boolean).unwrap_or(false)
        });
        if !ok {
            return rejected("comparison_budget_measurement_missing");
        }
        let total_reverts: i64 = reverts
            .iter()
            .filter_map(|r| r.get("reverted").and_then(Value::as_bool).map(|b| b as i64))
            .sum();
        budget_observations.insert(
            "max_candidate_reverts".to_string(),
            json!(total_reverts as f64),
        );
        if total_reverts as f64 > limit.as_f64().unwrap_or(f64::INFINITY) {
            budget_satisfied = false;
        }
    }
    let claim = contract
        .get("claim")
        .and_then(Value::as_str)
        .unwrap_or("context_pruning");
    let improved = match claim {
        "context_pruning" => {
            if base_bytes.is_none() || cand_bytes.is_none() {
                return rejected("context_measurement_missing");
            }
            cand_bytes.unwrap() < base_bytes.unwrap()
        }
        "graph_widening" => {
            if base_latency.is_none() || cand_latency.is_none() {
                return rejected("latency_measurement_missing");
            }
            cand_latency.unwrap() < base_latency.unwrap()
        }
        _ => return rejected("comparison_claim_invalid"),
    };
    let mut reduction: Option<i64> = None;
    if let (Some(base), Some(cand)) = (base_bytes, cand_bytes) {
        if base != 0.0 {
            reduction = Some(super::readers::py_round((1.0 - cand / base) * 100.0));
        }
    }
    json!({
        "label": if improved && budget_satisfied { "improvement" } else { "no_improvement" },
        "claim": claim,
        "cohorts": {"baseline": baseline.len(), "candidate": candidate.len()},
        "window": {"start": window.and_then(|w| w.get("start")), "end": window.and_then(|w| w.get("end"))},
        "exclusions": exclusions,
        "budgets": budgets,
        "context_bytes": {
            "baseline_median": base_bytes,
            "candidate_median": cand_bytes,
            "reduction_pct": reduction,
        },
        "latency_minutes": {
            "baseline_median": base_latency,
            "candidate_median": cand_latency,
        },
        "budget_observations": Value::Object(budget_observations),
        "budget_satisfied": budget_satisfied,
    })
}

/// Python `str(item)` on a JSON value: strings bare, True/False/None
/// Pythonic, numbers via repr.
fn json_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// `build_plan_fidelity`: join each planned row to its delivery and score
/// plan fidelity. An unjoined plan is `unjoined`, never scored 0%.
/// `unmeasurable="refuse"` adds the gate key; the join is identical, so the
/// scoreboard and the gate cannot drift into two definitions of planned.
pub(crate) fn build_plan_fidelity(
    inputs: &RunInputs,
    since_days: i64,
    now: chrono::DateTime<chrono::Local>,
    loop_check_events: &[Value],
    trace_events: &[Value],
    event_coverage: Option<&Value>,
    unmeasurable: &str,
) -> Result<Value, String> {
    if unmeasurable != "unjoined" && unmeasurable != "refuse" {
        return Err(format!(
            "unmeasurable must be 'unjoined' or 'refuse', got {unmeasurable:?}"
        ));
    }
    let cutoff = now - chrono::Duration::days(since_days);
    let windowed: Vec<&Value> = inputs
        .rows
        .iter()
        .filter(|r| matches!(parse_ts(r.get("completed")), Some(dt) if cutoff <= dt && dt <= now))
        .collect();
    if windowed.is_empty() {
        return Ok(json!({"state": "no_data", "since_days": since_days, "rows": 0}));
    }
    let maps = node_maps(&inputs.graph_nodes);
    let deliveries = &inputs.by_node;

    let mut planned_keys_by_path: HashMap<String, BTreeSet<String>> = HashMap::new();
    for r in &windowed {
        if !is_planned_row(r) {
            continue;
        }
        let key = plan_key(r.get("plan_path"), r.get("project"));
        let path_key = plan_key(r.get("plan_path"), None);
        if let (Some(key), Some(path_key)) = (key, path_key) {
            planned_keys_by_path
                .entry(path_key)
                .or_default()
                .insert(key);
        }
    }
    let mut shipped_by_plan: HashMap<String, Vec<&Value>> = HashMap::new();
    for r in &windowed {
        if !row_shipped(r, Some(deliveries)) {
            continue;
        }
        let mut plan_path = r.get("plan_path").cloned().unwrap_or(Value::Null);
        let mut project = r.get("project").cloned().unwrap_or(Value::Null);
        let missing_plan_path = plan_path.is_null();
        if missing_plan_path {
            let node_id = r.get("graph_node_id").and_then(Value::as_str);
            if let Some(node) = node_id.and_then(|n| maps.by_id.get(n)) {
                plan_path = node.get("plan_path").cloned().unwrap_or(Value::Null);
                if project.is_null() {
                    project = node.get("project").cloned().unwrap_or(Value::Null);
                }
            }
        }
        let key = plan_key(Some(&plan_path), Some(&project));
        let path_key = plan_key(Some(&plan_path), None);
        let candidates: BTreeSet<String> = path_key
            .as_ref()
            .and_then(|pk| planned_keys_by_path.get(pk))
            .cloned()
            .unwrap_or_default();
        let key = match key {
            // The Python leg's one-candidate rescue: a shipped row with no
            // plan_path borrows the only planned key for that path.
            Some(key) if missing_plan_path && !candidates.contains(&key) => {
                if candidates.len() == 1 {
                    candidates.into_iter().next()
                } else {
                    Some(key)
                }
            }
            other => other,
        };
        if let Some(key) = key {
            shipped_by_plan.entry(key).or_default().push(r);
        }
    }
    let mut results: Vec<Value> = Vec::new();
    let mut context_traces: Vec<Value> = Vec::new();
    let mut joined = 0i64;
    for r in &windowed {
        if !is_planned_row(r) {
            continue;
        }
        let key = plan_key(r.get("plan_path"), r.get("project"));
        let plan_deliveries = key
            .as_ref()
            .and_then(|k| shipped_by_plan.get(k))
            .cloned()
            .unwrap_or_default();
        let sid = r.get("session_id").cloned().unwrap_or(Value::Null);
        let plan_path_out = r.get("plan_path").cloned().unwrap_or(Value::Null);
        if plan_deliveries.is_empty() {
            results.push(json!({
                "session_id": sid,
                "plan_path": plan_path_out,
                "status": "unjoined",
            }));
            continue;
        }
        joined += 1;
        let d = plan_deliveries[0];
        let plan_doc = read_plan_doc(r.get("plan_path")).map(|s| s.to_string());
        let summary = read_summary(d);
        let diff = read_diff(d);
        let score = score_fidelity(plan_doc.as_deref(), summary.as_deref(), diff.as_deref());
        let declared = declared_probes(plan_doc.as_deref());
        let probes: Value = if declared.is_empty() {
            Value::Null
        } else {
            let evidence = probe_evidence(
                loop_check_events,
                d.get("session_id").and_then(Value::as_str),
            );
            json!({
                "declared": declared.len(),
                "passed": declared.iter().filter(|c| evidence.get(c.as_str()) == Some(&json!("pass"))).count(),
            })
        };
        let nid = d
            .get("graph_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let trace = super::context_trace::build_context_outcome_trace(
            d,
            nid.and_then(|n| maps.by_id.get(n).map(|node| *node)),
            trace_events,
        );
        context_traces.push(trace.clone());
        results.push({
            let mut entry = Map::new();
            entry.insert("session_id".to_string(), sid);
            entry.insert("plan_path".to_string(), plan_path_out.clone());
            entry.insert("status".to_string(), json!("joined"));
            entry.insert(
                "pr_number".to_string(),
                d.get("pr_number").cloned().unwrap_or(Value::Null),
            );
            entry.insert("outcome".to_string(), {
                let outcome = match (nid, maps.by_id.contains_key(nid.unwrap_or_default())) {
                    (Some(n), true) => {
                        node_outcome(n, parse_ts(d.get("completed")), &maps.by_id, &maps.fixes)
                    }
                    _ => "",
                };
                if outcome.is_empty() {
                    Value::Null
                } else {
                    json!(outcome)
                }
            });
            entry.insert("probes".to_string(), probes);
            entry.insert("context_outcome_trace".to_string(), trace);
            for (k, v) in score.as_object().map(|o| o.clone()).unwrap_or_default() {
                entry.insert(k, v);
            }
            Value::Object(entry)
        });
    }
    let planned_total = results.len() as i64;
    let unjoined_rows: Vec<&Value> = results
        .iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("unjoined"))
        .collect();
    let declared_contract = comparison_contract_from_events(trace_events);
    let mut out = Map::new();
    out.insert("state".to_string(), json!("ok"));
    out.insert("since_days".to_string(), json!(since_days));
    out.insert("results".to_string(), Value::Array(results.clone()));
    out.insert(
        "coverage".to_string(),
        json!({
            "planned_rows": planned_total,
            "joined_pct": pct(joined, planned_total),
        }),
    );
    let coverage = event_coverage.cloned().unwrap_or_else(|| {
        json!({
            "complete": true,
            "malformed_lines": 0,
            "unreadable_paths": 0,
            "paths": [],
        })
    });
    out.insert("event_coverage".to_string(), coverage.clone());
    out.insert(
        "context_comparison".to_string(),
        evaluate_context_comparison(
            &context_traces,
            declared_contract.as_ref(),
            coverage
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        ),
    );
    if unmeasurable == "refuse" {
        out.insert(
            "gate".to_string(),
            json!({
                "disposition": "refuse",
                "planned": planned_total,
                "delivered": planned_total - unjoined_rows.len() as i64,
                "unjoined_count": unjoined_rows.len(),
                "unjoined": unjoined_rows.iter().map(|r| json!({
                    "session_id": r.get("session_id"),
                    "plan_path": r.get("plan_path"),
                })).collect::<Vec<_>>(),
                "would_refuse": !unjoined_rows.is_empty(),
            }),
        );
    }
    Ok(Value::Object(out))
}

/// `_finite_nonnegative_float`.
fn finite_nonneg_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| *f >= 0.0 && f.is_finite()),
        _ => None,
    }
}

/// `_finite_nonnegative_number`.
fn finite_nonneg_number(value: &Value) -> bool {
    finite_nonneg_float(value).is_some()
}

/// `_median_metric`: the values at a path through each trace; one missing
/// value rejects the whole metric.
fn median_metric(traces: &[&Value], path: &[&str]) -> Option<f64> {
    let mut values = Vec::new();
    for trace in traces {
        let mut current = *trace;
        for key in path {
            current = match current.get(key) {
                Some(v) => v,
                None => return None,
            };
        }
        match finite_nonneg_float(current) {
            Some(v) => values.push(v),
            None => return None,
        }
    }
    if values.is_empty() {
        return None;
    }
    Some(python_median(&values))
}

/// Python `statistics.median`: the middle value, or the mean of the two
/// middle values (a float) for even counts.
fn python_median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    match n % 2 {
        1 => sorted[n / 2],
        _ => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
    }
}

/// The `scoreboard_plan_fidelity` keeper op: the gate ships its
/// pre-filtered rows + entries, the join runs here so the fold has one
/// implementation. The gate passes no events, matching the Python leg's
/// fold defaults (empty trace, complete coverage).
pub(crate) fn plan_fidelity_op(params: &Value) -> Result<Value, String> {
    let rows = params
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let graph_nodes = params
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let since_days = params
        .get("since_days")
        .and_then(Value::as_i64)
        .unwrap_or(180);
    let now = match params.get("now").and_then(Value::as_str) {
        Some(iso) => super::readers::parse_ts(Some(&Value::String(iso.to_string())))
            .unwrap_or_else(chrono::Local::now),
        None => chrono::Local::now(),
    };
    let unmeasurable = params
        .get("unmeasurable")
        .and_then(Value::as_str)
        .unwrap_or("refuse");
    let classified = super::views::classify_deliveries(&graph_nodes, &rows, None, None, None)?;
    let inputs = super::views::RunInputs {
        events_paths: Vec::new(),
        by_node: classified
            .get("by_node")
            .cloned()
            .unwrap_or_else(|| json!({})),
        rows,
        graph_nodes,
        classified,
    };
    build_plan_fidelity(&inputs, since_days, now, &[], &[], None, unmeasurable)
}

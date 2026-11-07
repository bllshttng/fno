//! The vision kill list: a maintain leg that scores the deferred pile
//! against the project vision, judges the worst band through the shared
//! mission_fit grader, and lands the batch-approval surface. Report is the
//! default; `--apply` defers the judged kill list in one batch and nothing
//! defers without it.

use serde_json::{json, Value};
use std::path::Path;

use super::{Flags, Out, Seams};

/// How many worst-fitting rows the judged band takes.
const VISION_SWEEP_BAND: usize = 100;

/// The batch reason every `--apply` deferral carries.
fn batch_reason() -> String {
    let stamp = crate::graph_store::now_isoformat();
    let date: String = stamp.chars().take(10).collect();
    format!("vision kill list {date}, batch approved")
}

/// Run the sweep: score, judge the band, report, land the artifact, and
/// apply only under `--apply`. Returns the exit code.
pub(crate) fn run_vision_sweep(
    flags: Flags,
    seams: &Seams,
    out: &mut Out,
    entries: &[Value],
    cwd: &Path,
) -> i32 {
    // Apply never re-grades: the artifact the report landed is the approval
    // surface, so apply reads the newest one and defers exactly its fail
    // rows. Re-judging here could flip a verdict and defer a row the user
    // never approved.
    if flags.apply {
        return apply_from_artifact(seams, out);
    }
    let Some(vision) = crate::blueprint_judge::vision_source_text(&[], cwd) else {
        out.line("vision-sweep: skipped (no vision source configured)");
        return 0;
    };
    let candidates: Vec<&Value> = entries.iter().filter(|e| is_sweep_candidate(e)).collect();
    if candidates.is_empty() {
        out.line("vision-sweep: no deferred rows to score");
        return 0;
    }
    let mut scored: Vec<(f64, &Value)> = candidates
        .into_iter()
        .map(|e| (vision_presize(&row_text(e), &vision), e))
        .collect();
    // Worst first; a tie settles by id so the list stays deterministic.
    scored.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| id_of(a.1).cmp(&id_of(b.1)))
    });
    let rows: Vec<Value> = scored
        .iter()
        .take(VISION_SWEEP_BAND)
        .map(|(score, e)| {
            let (verdict, reason) = (seams.vision_grade)(&row_text(e), &cwd)
                .unwrap_or_else(|| ("none".to_string(), "judge skipped or faulted".to_string()));
            json!({
                "id": id_of(e),
                "title": title_of(e),
                "prescore": score,
                "verdict": verdict,
                "reason": reason,
            })
        })
        .collect();
    let kill: Vec<&Value> = rows.iter().filter(|r| r["verdict"] == "fail").collect();
    out.line(format!(
        "vision-sweep: {} deferred rows scored, {} judged, {} on the kill list",
        scored.len(),
        rows.len(),
        kill.len()
    ));
    for r in &rows {
        out.line(format!(
            "  {}  {:.3}  {:>5}  {}  {}",
            r["id"].as_str().unwrap_or("?"),
            r["prescore"].as_f64().unwrap_or(0.0),
            r["verdict"].as_str().unwrap_or("?"),
            r["reason"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(48)
                .collect::<String>(),
            r["title"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>(),
        ));
    }
    write_artifact(seams, out, &rows);
    0
}

/// A sweep candidate: a deferred row the closing kinds have not already
/// ended (`wont_do` and `retracted` rows are dead; a re-judged dead row
/// teaches nothing).
fn is_sweep_candidate(entry: &Value) -> bool {
    if entry.get("status").and_then(Value::as_str) != Some("deferred") {
        return false;
    }
    let kind = entry.get("deferred_kind").and_then(Value::as_str);
    !crate::graph_store::CLOSING_DEFER_KINDS.contains(&kind.unwrap_or(""))
}

/// The text the prescore and the judge both read: title plus details.
fn row_text(entry: &Value) -> String {
    let title = entry.get("title").and_then(Value::as_str).unwrap_or("");
    let details = entry.get("details").and_then(Value::as_str).unwrap_or("");
    format!("{title} {details}")
}

fn id_of(entry: &Value) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string()
}

fn title_of(entry: &Value) -> String {
    entry
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Cheap deterministic pre-score: the share of the row's distinctive words
/// (four characters or longer) that appear in the vision text. 0.0 means
/// nothing matches; 1.0 means every word matches. The judge grades the
/// worst band, so this only orders the queue.
fn vision_presize(row_text: &str, vision: &str) -> f64 {
    let vision_words = word_set(vision);
    if vision_words.is_empty() {
        return 0.0;
    }
    let words = word_set(row_text);
    if words.is_empty() {
        return 0.0;
    }
    let hits = words.iter().filter(|w| vision_words.contains(*w)).count();
    hits as f64 / words.len() as f64
}

fn word_set(text: &str) -> std::collections::BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Land the artifact beside the health history: one JSON file naming the
/// band, the scores, and the verdicts. The report above is the readable
/// surface; this file is the batch-approval surface.
fn write_artifact(seams: &Seams, out: &mut Out, rows: &[Value]) {
    let Some(dir) = &seams.history_dir else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        out.err(format!("vision-sweep artifact skipped (mkdir: {e})"));
        return;
    }
    let stamp = crate::graph_store::now_isoformat();
    let compact: String = stamp
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let path = dir.join(format!("vision-kill-list-{compact}.json"));
    let payload = json!({
        "generated_at": stamp,
        "band": VISION_SWEEP_BAND,
        "rows": rows,
    });
    match serde_json::to_string_pretty(&payload) {
        Ok(body) => {
            if let Err(e) = std::fs::write(&path, body) {
                out.err(format!("vision-sweep artifact skipped (write: {e})"));
            } else {
                out.line(format!("vision-sweep artifact: {}", path.display()));
            }
        }
        Err(e) => out.err(format!("vision-sweep artifact skipped (serialize: {e})")),
    }
}

/// `--apply`: read the newest kill-list artifact and defer its fail rows
/// `wont_do` with the batch reason, through the same mutate door the defer
/// command uses. No artifact means nothing defers: the report run is the
/// only producer of the approval surface. A claimed node is skipped, not
/// fought: the sweep never releases another session's claim.
fn apply_from_artifact(seams: &Seams, out: &mut Out) -> i32 {
    let Some(dir) = &seams.history_dir else {
        out.line("vision-sweep apply: no artifact dir; nothing deferred");
        return 0;
    };
    let newest = std::fs::read_dir(dir).ok().and_then(|entries| {
        entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("vision-kill-list-") && n.ends_with(".json"))
                    .unwrap_or(false)
            })
            .max_by_key(|p| {
                p.metadata()
                    .and_then(|m| m.modified())
                    .map(|m| m)
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            })
    });
    let Some(path) = newest else {
        out.line(
            "vision-sweep apply: no kill list artifact to apply; run the report first; \
             nothing deferred",
        );
        return 0;
    };
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            out.err(format!("vision-sweep apply refused (artifact read: {e})"));
            return 1;
        }
    };
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            out.err(format!("vision-sweep apply refused (artifact parse: {e})"));
            return 1;
        }
    };
    let empty: Vec<Value> = Vec::new();
    let rows = parsed
        .get("rows")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let kill: Vec<&Value> = rows.iter().filter(|r| r["verdict"] == "fail").collect();
    out.line(format!(
        "vision-sweep apply: applying {} ({} fail rows)",
        path.display(),
        kill.len()
    ));
    apply_kill_list(seams, out, &kill)
}

/// Defer exactly the handed kill list, skipping claimed rows.
fn apply_kill_list(seams: &Seams, out: &mut Out, kill: &[&Value]) -> i32 {
    if kill.is_empty() {
        out.line("vision-sweep apply: the kill list is empty; nothing to defer");
        return 0;
    }
    let claimed = match (seams.claims)() {
        Ok(c) => c,
        Err(e) => {
            out.err(format!("vision-sweep apply refused (claims read: {e})"));
            return 1;
        }
    };
    let ids: Vec<String> = kill
        .iter()
        .map(|r| r["id"].as_str().unwrap_or("").to_string())
        .filter(|id| !id.is_empty())
        .collect();
    let skipped: Vec<&String> = ids.iter().filter(|id| claimed.contains(*id)).collect();
    let applying: Vec<&String> = ids.iter().filter(|id| !claimed.contains(*id)).collect();
    for id in &skipped {
        out.line(format!(
            "vision-sweep apply: {id} carries a live claim; skipped"
        ));
    }
    if applying.is_empty() {
        out.line("vision-sweep apply: every listed row carries a claim; nothing deferred");
        return 0;
    }
    let reason = batch_reason();
    let applying_inner = applying.clone();
    let applied = crate::graph_store::mutate_rows(
        &seams.graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for id in &applying_inner {
                let Some(node) = working
                    .iter_mut()
                    .find(|e| e.get("id").and_then(Value::as_str) == Some(id.as_str()))
                else {
                    return Err(crate::graph_store::StoreError::Invalid(format!(
                        "kill-list row vanished: {id}"
                    )));
                };
                let obj = node.as_object_mut().expect("row is an object");
                obj.insert("status".into(), json!("deferred"));
                obj.insert(
                    "deferred_at".into(),
                    json!(crate::graph_store::now_isoformat()),
                );
                obj.insert("deferred_reason".into(), json!(reason));
                obj.insert("deferred_kind".into(), json!("wont_do"));
            }
            Ok(true)
        },
    );
    match applied {
        Ok(_) => {
            out.line(format!(
                "vision-sweep apply: {} rows deferred wont_do ({})",
                applying.len(),
                reason
            ));
            0
        }
        Err(e) => {
            out.err(format!("vision-sweep apply refused (graph write: {e})"));
            1
        }
    }
}

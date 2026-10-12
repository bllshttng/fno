//! The pr-heal drive loop's per-sweep world: what one sweep saw, and the
//! skip that lets the next one not look again. Kept beside `heal` so the
//! drive loop stays under the file budget while the skip's digest, the
//! listing parse and the journal-read baseline move as one question.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// One PR's world as a sweep saw it: the head sha plus a digest over the
/// checks' latest per-name buckets. A later sweep whose live read matches
/// both is looking at the same PR, so every read past the one checks call
/// (the pulls read, the job logs, the flake scan) is repeated work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PrSweepState {
    pub(crate) head: String,
    pub(crate) digest: String,
}

/// The PR numbers in a slurped REST pulls listing (an array of pages, each a
/// JSON array of PRs).
pub(crate) fn open_pr_numbers(pages: &[Value]) -> Vec<String> {
    open_pr_sweeps(pages)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect()
}

/// Flatten a slurped REST pulls listing into single pull rows. Consumers that
/// walk rows one action deep (`pr_steward`) take rows, never pages.
pub(crate) fn pull_rows(pages: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for page in pages {
        match page {
            Value::Array(rows) => out.extend(rows.iter().cloned()),
            other => out.push(other.clone()),
        }
    }
    out
}

/// The open PRs in a slurped REST pulls listing as `(number, head sha, row)`:
/// everything the unchanged-skip gate reads without a per-PR pulls call. The
/// listing is the same PR object schema the single-pull read answers, so
/// `head.sha`, `head.ref`, `base.ref` and `mergeable` ride along free. A row
/// without a head sha still lists (the legacy per-PR read owns it).
pub(crate) fn open_pr_sweeps(pages: &[Value]) -> Vec<(String, Option<String>, Value)> {
    let mut out = Vec::new();
    for page in pages {
        let rows = match page {
            Value::Array(rows) => rows.clone(),
            other => vec![other.clone()],
        };
        for row in rows {
            let Some(n) = row.get("number").and_then(|v| v.as_u64()) else {
                continue;
            };
            let head = row
                .pointer("/head/sha")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            out.push((n.to_string(), head, row));
        }
    }
    out
}

/// sha256 over the latest-per-name check rows as `name` then `bucket` lines,
/// sorted by name so row order never moves the hash. A rerun that reaches the
/// same verdict hashes equal (nothing heal can do changed); a rerun that
/// flips a bucket hashes different and the PR is processed. `None` when the
/// payload carries no row array: an unreadable read is never evidence of
/// unchanged.
pub(crate) fn checks_digest(checks: &Value) -> Option<String> {
    let rows = crate::check_supersession::latest_per_name(checks);
    let rows = rows.as_array()?;
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{}\u{1f}{}",
                r.get("name").and_then(Value::as_str).unwrap_or(""),
                r.get("bucket").and_then(Value::as_str).unwrap_or("")
            )
        })
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    hasher.update(lines.join("\n").as_bytes());
    Some(format!("{:x}", hasher.finalize()))
}

/// The last sweep's per-PR states for this root: the newest `pr_heal_tick`
/// row that names this root AND carries a `pr_states` map. Rows from other
/// roots (one process writes one row per root) and rows from a binary older
/// than the field never contribute. An unreadable store reads as no baseline,
/// so the sweep after a fault processes every PR.
pub(crate) fn last_sweep_states(
    path: &std::path::Path,
    root: &str,
) -> std::collections::BTreeMap<String, PrSweepState> {
    let mut out = std::collections::BTreeMap::new();
    let _ = crate::event_store::import_all(path);
    let Ok(rows) = crate::event_store::query_events(
        path,
        &crate::event_store::EventQuery {
            types: vec!["pr_heal_tick".to_string()],
            ..Default::default()
        },
    ) else {
        return out;
    };
    for r in rows {
        let Ok(row) = serde_json::from_str::<Value>(&r.line) else {
            continue;
        };
        let data = row.get("data");
        if data.and_then(|d| d.get("root")).and_then(Value::as_str) != Some(root) {
            continue;
        }
        let Some(states) = data
            .and_then(|d| d.get("pr_states"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let mut map = std::collections::BTreeMap::new();
        for (pr, v) in states {
            let (Some(head), Some(digest)) = (
                v.get("head").and_then(Value::as_str),
                v.get("digest").and_then(Value::as_str),
            ) else {
                continue;
            };
            map.insert(
                pr.clone(),
                PrSweepState {
                    head: head.to_string(),
                    digest: digest.to_string(),
                },
            );
        }
        // Rows arrive in commit order, so the last match is the newest.
        out = map;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checks_digest_rows() {
        // The shape read_checks answers: a flat rows array carrying name and
        // bucket. Same rows, different payload order: the digest is the PR's
        // world, never the listing's row order.
        let green_world = json!([
            {"name":"ci","bucket":"pass","link":""},
            {"name":"fmt","bucket":"fail","link":""}
        ]);
        let same_world = json!([
            {"name":"fmt","bucket":"fail","link":""},
            {"name":"ci","bucket":"pass","link":""}
        ]);
        assert_eq!(checks_digest(&green_world), checks_digest(&same_world));

        // A bucket flip changes the digest: a rerun that reached a verdict
        // reopens the PR to the loop.
        let flipped = json!([
            {"name":"ci","bucket":"fail","link":""},
            {"name":"fmt","bucket":"fail","link":""}
        ]);
        assert_ne!(checks_digest(&green_world), checks_digest(&flipped));

        // An unreadable payload is never evidence of unchanged.
        assert_eq!(checks_digest(&json!("not an array")), None);
        assert_eq!(checks_digest(&json!(null)), None);
    }

    #[test]
    fn pull_rows_flatten_pages() {
        let page = |nums: &[u64]| {
            json!(nums
                .iter()
                .map(|n| json!({"number": n}))
                .collect::<Vec<_>>())
        };
        let pages = vec![page(&[1, 2]), page(&[3])];
        let nums: Vec<u64> = pull_rows(&pages)
            .iter()
            .map(|r| r["number"].as_u64().unwrap())
            .collect();
        assert_eq!(nums, vec![1, 2, 3]);

        // A non-array page (gh_api_pages wraps a lone body) is one row.
        let lone = vec![json!({"number": 9})];
        assert_eq!(pull_rows(&lone).len(), 1);
    }
}

//! The yard identity fold: species, rarity, first-sighting for the fleet -
//! the Rust owner behind `fno agents yard`, ported from the deleted
//! `cli/src/fno/yard.py`.
//!
//! Read-only over two files that already exist - the agent registry and the
//! graph archive - and nothing else. The mux's yard overlay shells
//! `fno agents yard --json` and renders what this fold emits. The fold
//! computes no liveness and no status: the overlay reads those from the
//! roster row it already holds, so one status value feeds both the row and
//! the sprite.
//!
//! Two rules from the yard design that this module must never break:
//!
//! - Species identity is the FULL unique id, hashed stably. The short mail
//!   handle and the per-restart display prefix collide (three live sessions
//!   once shared the handle `01a00328`), and a salted per-process hash would
//!   reassign a citizen's species on every render.
//! - Rarity is a rank, shown as outcome only. The original proposal named a
//!   four-axis tuple (harness, provider, model, account); the registry has
//!   carried only `harness` since schema v10, so v1 ranks the one axis that
//!   is recorded - "put out the codex bowl and a different cat shows up" is
//!   still the whole mechanic. Widen the tuple when the registry grows axes
//!   back; never invent them.
//!
//! The first-sighting mark is species-dimension ONLY: rarity and hat are not
//! recomputed for archive rows (`source_harness` survives on roughly 40% of
//! them) and are never faked.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::backlog::settings::graph_path;
use crate::paths::AgentsHome;

const SPECIES_COUNT: u64 = 18;
const RARITY_TIERS: [&str; 5] = ["common", "uncommon", "rare", "epic", "legendary"];
/// Cumulative population-share boundaries matching RARITY_WEIGHT's
/// 60/25/10/4/1 shape: tuples covering up to 60% of the population read
/// common, the next 25% uncommon, and so on; whatever the boundaries never
/// reach reads legendary.
const RARITY_BOUNDARIES: [f64; 4] = [0.60, 0.85, 0.95, 0.99];

/// Stable species index for a full unique id (sha256, never a salted hash).
fn species_for(full_id: &str) -> u64 {
    let digest = Sha256::digest(full_id.as_bytes());
    let head = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    head % SPECIES_COUNT
}

/// The full unique key behind a registry row - never the short handle.
/// `harness_session_id` is the canonical full id; a legacy row without one
/// degrades to `name@created_at`, which is still stable per row and does not
/// collide across restarts the way a display prefix does.
fn citizen_id(row: &Value) -> String {
    if let Some(sid) = row.get("harness_session_id").and_then(Value::as_str) {
        if !sid.is_empty() {
            return sid.to_string();
        }
    }
    format!(
        "{}@{}",
        row.get("name").and_then(Value::as_str).unwrap_or("?"),
        row.get("created_at").and_then(Value::as_str).unwrap_or("")
    )
}

/// Rank `population` values by frequency, bucket against the weight shape.
/// Deterministic: count-desc, then lex on the value for ties. The value that
/// crosses a boundary opens the next bucket, so a majority value always reads
/// common and a value seen once among many reads legendary.
fn rarity_tiers(population: &[String]) -> std::collections::BTreeMap<String, String> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for v in population {
        if v.is_empty() {
            continue;
        }
        *counts.entry(v.as_str()).or_insert(0) += 1;
    }
    let total: usize = counts.values().sum();
    let mut tiers = std::collections::BTreeMap::new();
    if total == 0 {
        return tiers;
    }
    let mut ordered: Vec<(&str, usize)> = counts.into_iter().collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let mut tier_idx = 0usize;
    let mut cum = 0usize;
    for (value, count) in ordered {
        cum += count;
        tiers.insert(value.to_string(), RARITY_TIERS[tier_idx].to_string());
        while tier_idx < RARITY_BOUNDARIES.len()
            && cum as f64 >= RARITY_BOUNDARIES[tier_idx] * total as f64
        {
            tier_idx += 1;
        }
    }
    tiers
}

/// Species hashes already recorded in the album (the archive's sessions).
fn seen_species(archive: &[Value]) -> std::collections::BTreeSet<u64> {
    let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in archive {
        let Some(map) = e.as_object() else {
            continue;
        };
        for key in ["source_session_id", "session_id"] {
            if let Some(v) = map.get(key).and_then(Value::as_str) {
                if !v.is_empty() {
                    ids.insert(v.to_string());
                }
            }
        }
        if let Some(sessions) = map.get("sessions").and_then(Value::as_array) {
            for s in sessions {
                if let Some(v) = s.get("session_id").and_then(Value::as_str) {
                    if !v.is_empty() {
                        ids.insert(v.to_string());
                    }
                }
            }
        }
    }
    ids.iter().map(|v| species_for(v)).collect()
}

/// Fold registry rows into yard citizens, joined to the album for the mark.
/// Sorted by name for a deterministic wire order (the overlay joins by name;
/// a stable order keeps the spotlight cursor from jumping between renders).
fn fold(rows: &[Value], archive: &[Value]) -> Vec<Value> {
    let seen = seen_species(archive);
    let harnesses: Vec<String> = rows
        .iter()
        .filter_map(|r| r.get("harness").and_then(Value::as_str).map(str::to_string))
        .collect();
    let tiers = rarity_tiers(&harnesses);
    let mut citizens: Vec<Value> = rows
        .iter()
        .map(|r| {
            let sp = species_for(&citizen_id(r));
            let harness = r.get("harness").and_then(Value::as_str);
            json!({
                "id": citizen_id(r),
                "name": r.get("name").and_then(Value::as_str).unwrap_or(""),
                "harness": harness,
                "species": sp,
                "rarity": harness
                    .and_then(|h| tiers.get(h))
                    .cloned()
                    .unwrap_or_else(|| RARITY_TIERS[0].to_string()),
                "role_level": r.get("role_level").and_then(Value::as_i64).unwrap_or(0),
                "first_sighting": !seen.contains(&sp),
            })
        })
        .collect();
    citizens.sort_by_key(|c| {
        c.get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });
    citizens
}

/// The graph archive's rows: strict on purpose. This is a truth-bound fold,
/// not a browse command - the lenient reader turns a corrupt archive into
/// `[]`, which would mark every citizen a first-sighting (fabricated outcome
/// on the machine surface the mux renders). Unreadable history fails the
/// fold so the overlay degrades.
fn read_archive_strict() -> Result<Vec<Value>, String> {
    let Some(parent) = graph_path().parent().map(Path::to_path_buf) else {
        return Ok(Vec::new());
    };
    read_archive_strict_at(&parent.join("graph-archive.json"))
}

fn read_archive_strict_at(path: &Path) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("archive unreadable, fold refused: {e}"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("archive unreadable, fold refused: {e}"))?;
    match value {
        Value::Array(rows) => Ok(rows),
        _ => Err("archive unreadable, fold refused: not a list".to_string()),
    }
}

/// The registry's rows, fail-loud: a malformed registry is the same
/// truth-bound refusal as the archive's.
fn read_registry_rows() -> Result<Vec<Value>, String> {
    crate::registry_store::read(&AgentsHome::from_env().registry_json())
        .ok()
        .and_then(|v| v.get("agents").and_then(Value::as_array).cloned())
        .ok_or_else(|| "registry unreadable, fold refused".to_string())
}

/// `fno-agents yard [--json]`: the machine surface the mux overlay shells
/// and the front's `fno agents yard` claim execs. Text mode is a
/// one-line-per-citizen summary; outcome only - never the rank, the
/// frequency, or the boundary.
pub fn run(args: &[String]) -> i32 {
    let as_json = args.iter().any(|a| a == "--json" || a == "-J");
    let rows = match read_registry_rows() {
        Ok(rows) => rows,
        Err(msg) => {
            eprintln!("agents yard: {msg}");
            return 1;
        }
    };
    let archive = match read_archive_strict() {
        Ok(rows) => rows,
        Err(msg) => {
            eprintln!("agents yard: {msg}");
            return 1;
        }
    };
    let citizens = fold(&rows, &archive);
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "citizens": citizens }))
                .unwrap_or_else(|_| "{\"citizens\": []}".into())
        );
        return 0;
    }
    if citizens.is_empty() {
        println!("the yard is empty (no registry rows)");
        return 0;
    }
    for c in &citizens {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        let species = c.get("species").and_then(Value::as_u64).unwrap_or(0);
        let rarity = c.get("rarity").and_then(Value::as_str).unwrap_or("");
        let role = c.get("role_level").and_then(Value::as_i64).unwrap_or(0);
        let first = c
            .get("first_sighting")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let role_part = if role != 0 {
            format!(" role {role}")
        } else {
            String::new()
        };
        let mark = if first { " NEW" } else { "" };
        println!("{name:<24} species {species:>2}  {rarity:<9}{role_part}{mark}");
    }
    let n_new = citizens
        .iter()
        .filter(|c| {
            c.get("first_sighting")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    println!(
        "{} citizens, {} first sighting(s); tiers: {}",
        citizens.len(),
        n_new,
        RARITY_TIERS.join("/")
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, harness: &str, sid: &str, role: i64) -> Value {
        json!({"name": name, "harness": harness, "harness_session_id": sid, "role_level": role})
    }

    #[test]
    fn fold_matches_the_captured_python_golden() {
        // Golden captured from cli/src/fno/yard.py before its deletion
        // (port protocol, dual-implementation-inventory.md).
        let rows = vec![
            row("alpha", "claude", "sess-alpha", 2),
            row("beta", "claude", "sess-beta", 0),
            row("gamma", "codex", "sess-gamma", 0),
        ];
        let legacy =
            json!({"name": "legacy", "harness": "codex", "created_at": "2026-01-01T00:00:00Z"});
        let archive = vec![
            json!({"source_session_id": "sess-alpha"}),
            json!({"sessions": [{"session_id": "sess-gamma"}]}),
            json!("not-a-dict"),
        ];
        let mut all_rows = rows.clone();
        all_rows.push(legacy);
        let citizens = fold(&all_rows, &archive);
        let by_name = |n: &str| {
            citizens
                .iter()
                .find(|c| c.get("name").and_then(Value::as_str) == Some(n))
                .cloned()
                .unwrap()
        };
        assert_eq!(by_name("alpha")["species"], json!(9u64));
        assert_eq!(by_name("alpha")["first_sighting"], json!(false));
        assert_eq!(by_name("beta")["first_sighting"], json!(true));
        assert_eq!(by_name("beta")["rarity"], json!("common"));
        assert_eq!(by_name("gamma")["first_sighting"], json!(false));
        assert_eq!(by_name("gamma")["role_level"], json!(0));
        assert_eq!(
            by_name("legacy")["id"],
            json!("legacy@2026-01-01T00:00:00Z")
        );
        assert_eq!(by_name("legacy")["species"], json!(14u64));
        assert_eq!(
            by_name("legacy")["species"],
            json!(species_for("legacy@2026-01-01T00:00:00Z"))
        );
        // Sorted by name for a deterministic wire order.
        let names: Vec<&str> = citizens
            .iter()
            .map(|c| c.get("name").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma", "legacy"]);
    }

    #[test]
    fn rarity_tiers_rank_against_the_weight_shape() {
        let population: Vec<String> = std::iter::repeat("claude")
            .take(60)
            .chain(std::iter::repeat("codex").take(25))
            .chain(std::iter::repeat("agy").take(10))
            .chain(std::iter::repeat("opencode").take(4))
            .chain(std::iter::once("zai".to_string()))
            .collect();
        let tiers = rarity_tiers(&population);
        assert_eq!(tiers.get("claude").unwrap(), "common");
        assert_eq!(tiers.get("codex").unwrap(), "uncommon");
        assert_eq!(tiers.get("agy").unwrap(), "rare");
        assert_eq!(tiers.get("opencode").unwrap(), "epic");
        assert_eq!(tiers.get("zai").unwrap(), "legendary");
    }

    #[test]
    fn seen_species_reads_only_string_sessions() {
        let archive = vec![
            json!({"source_session_id": "a"}),
            json!({"sessions": [{"session_id": "b"}, "junk"]}),
            json!("not-a-dict"),
            json!({"session_id": 42}),
        ];
        let seen = seen_species(&archive);
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn a_corrupt_archive_fails_the_fold_not_fabricates() {
        // The Python suite's contract: the lenient reader would turn a
        // corrupt archive into [] and mark every citizen a first-sighting;
        // the strict reader refuses instead (test_yard_cli_corrupt_archive_
        // fails_the_fold_not_fabricates, pre-deletion).
        let dir = std::env::temp_dir().join(format!("fno-yard-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let corrupt = dir.join("graph-archive.json");
        std::fs::write(&corrupt, "{not json").unwrap();

        // A raw parse of the corrupt bytes is the refusal the fold surfaces.
        let text = std::fs::read_to_string(&corrupt).unwrap();
        let parsed: Result<Value, _> = serde_json::from_str(&text);
        assert!(parsed.is_err(), "corrupt archive parses nowhere");

        // The strict reader surfaces that refusal as the fold's error, and
        // the door exits 1 rather than emitting fabricated first-sightings.
        assert!(read_archive_strict_at(&corrupt).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_citizen_json_shape_carries_the_seven_fields() {
        // The mux overlay reads these keys off `fno agents yard --json`
        // (test_yard_cli_json_emits_citizens, pre-deletion).
        let rows = vec![row("alpha", "claude", "sess-alpha", 2)];
        let citizens = fold(&rows, &[]);
        let c = &citizens[0];
        for key in [
            "id",
            "name",
            "harness",
            "species",
            "rarity",
            "role_level",
            "first_sighting",
        ] {
            assert!(c.get(key).is_some(), "citizen key {key} missing");
        }
        assert_eq!(c["id"], json!("sess-alpha"));
        assert_eq!(c["role_level"], json!(2));
    }
}

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::event_store::{gc_ephemeral, GcReceipt, MINIMUM_EPHEMERAL_TTL_HOURS};

fn collect(
    journal: &Path,
    global: &Path,
    ttl: i64,
    dry_run: bool,
    now_ms: i64,
) -> Result<GcReceipt, String> {
    if ttl < MINIMUM_EPHEMERAL_TTL_HOURS {
        return Err(format!(
            "ttl_hours is shorter than the schema minimum retention horizon ({} hours)",
            MINIMUM_EPHEMERAL_TTL_HOURS
        ));
    }
    if crate::config_cli::resolve_path(journal) == crate::config_cli::resolve_path(global) {
        return Err("refusing to compact the global daemon journal: it is shared across projects, so one project's TTL policy must not rewrite it".into());
    }
    let cutoff = now_ms.saturating_sub(ttl.saturating_mul(3_600_000));
    gc_ephemeral(journal, cutoff, dry_run)
}

pub(super) fn run(args: &[OsString], json: bool) -> i32 {
    let mut journal: Option<PathBuf> = None;
    let mut ttl = MINIMUM_EPHEMERAL_TTL_HOURS;
    let mut dry_run = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--events") => match iter.next() {
                Some(value) => journal = Some(PathBuf::from(value)),
                None => {
                    eprintln!("error: --events requires a path");
                    return 2;
                }
            },
            Some("--ttl-hours") => {
                match iter.next().and_then(|v| v.to_str()?.parse::<i64>().ok()) {
                    Some(value) if value > 0 => ttl = value,
                    _ => {
                        eprintln!("error: --ttl-hours requires a positive integer");
                        return 2;
                    }
                }
            }
            Some("--dry-run" | "-N") => dry_run = true,
            Some("--help") => {
                println!("Usage: fno-py doctor event gc [OPTIONS]\n\n  Delete expired explicit-ephemeral rows while preserving all other rows.\n\nOptions:\n  --events <path>          path to events.jsonl (default: worktree root)\n  --ttl-hours <int range>  ephemeral retention horizon in hours  [x>=1]\n  -N, --dry-run            report without rewriting\n  --help                   Show this message and exit.");
                return 0;
            }
            _ => {
                eprintln!("error: unrecognized gc option {}", arg.to_string_lossy());
                return 2;
            }
        }
    }
    let explicit = journal.is_some();
    let journal = journal.unwrap_or_else(crate::config_cli::history::project_journal);
    let global = crate::config_cli::history::global_journal();
    if !explicit {
        if let Err(error) = super::hermetic::check(&journal) {
            eprintln!("error: event gc failed: {error}");
            return 1;
        }
    }
    if let Err(error) = super::hermetic::check(&global) {
        eprintln!("error: event gc failed: {error}");
        return 1;
    }
    let receipt = match collect(&journal, &global, ttl, dry_run, super::now_ms()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: event gc failed: {error}");
            return 1;
        }
    };
    let deleted = if dry_run { 0 } else { receipt.expired };
    let kept = if dry_run {
        receipt.scanned
    } else {
        receipt.scanned - deleted
    };
    if json {
        let path = crate::doctor::python_json(&serde_json::Value::String(
            journal.to_string_lossy().into_owned(),
        ));
        println!("{{\"scanned\": {}, \"deleted\": {}, \"kept\": {}, \"malformed\": {}, \"events\": {}, \"ttl_hours\": {}, \"dry_run\": {}}}", receipt.scanned, deleted, kept, receipt.malformed, path, ttl, dry_run);
    } else {
        println!(
            "event gc: scanned={} deleted={} kept={} malformed={}",
            receipt.scanned, deleted, kept, receipt.malformed
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_respects_retention_dry_run_and_global_journal() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("events.jsonl");
        let global = root.path().join("global.jsonl");
        let ephemeral = r#"{"ts":"2020-01-01T00:00:00Z","type":"claim_acquired","source":"fno-loop","data":{"key":"node:test","holder":"test","pid":1,"host":"test","acquired_at":1}}"#;
        let durable = r#"{"ts":"2020-01-01T00:00:00Z","type":"operator_decision","source":"test","data":{"decision_id":"test","decision":"approve"}}"#;
        crate::event_store::append_envelope(&journal, ephemeral, None).unwrap();
        crate::event_store::append_envelope(&journal, durable, None).unwrap();
        let now = 1_800_000_000_000;
        let dry = collect(&journal, &global, 672, true, now).unwrap();
        assert_eq!((dry.scanned, dry.expired), (2, 1));
        let applied = collect(&journal, &global, 672, false, now).unwrap();
        assert_eq!((applied.scanned, applied.expired), (2, 1));
        let after = collect(&journal, &global, 672, true, now).unwrap();
        assert_eq!((after.scanned, after.expired), (1, 0));
        assert!(collect(&journal, &global, 671, false, now)
            .unwrap_err()
            .contains("shorter than the schema minimum"));
        assert!(collect(&journal, &journal, 672, false, now)
            .unwrap_err()
            .contains("global daemon journal"));
        #[cfg(unix)]
        {
            let link = root.path().join("alias");
            std::os::unix::fs::symlink(root.path(), &link).unwrap();
            assert!(
                collect(&link.join("events.jsonl"), &journal, 672, false, now)
                    .unwrap_err()
                    .contains("global daemon journal")
            );
        }
    }
}

//! `fno do review --assess-assurance`: the review assurance verdict and the
//! kinds read, ported from the Python legs and frozen byte-for-byte against
//! goldens captured from them before deletion (`tests/review_policy_parity.rs`).
//!
//! The port composes existing readers and adds no second copy of any: the
//! exhausted verdict is `fallback_chain::headroom` (the one port of
//! `runtime_state.headroom`), the lock is `spawn_gate_lanes::account_locked_until`
//! over `route_capacity::runtime_state_payload`, the ambient harness is
//! `claims::resolve_harness`, the ledger path is `paths_cli::ledger_json`, and
//! the record line renders through `client_verbs::to_python_json`.
//!
//! The account records come from `accounts::records::load`, the one port of
//! `loader.load_providers`. The cross-model switch read is local because no
//! Rust reader shares its shape: it mirrors `fno.config.load_settings`' layered
//! read STRICTLY (the shared `config_value_deep` skips an unparseable file,
//! which would fail closed where Python fails open): any layer that fails to
//! parse, or an `enabled` leaf that is not a bool, answers `true`.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::client_verbs::{expand_eq, to_python_json};
use crate::fallback_chain::{
    headroom, health_from_map, parse_provider_health, parse_usage, snapshot_from_map, Verdict,
};

pub(crate) const CLAUDE: &str = "claude";

/// Provider kinds the assurance gate counts as reviewers. The record
/// literal (`model.py::_HARNESS_LITERAL`) is wider; only these dispatch.
pub(crate) const DISPATCHABLE_PROVIDERS: [&str; 3] = ["claude", "codex", "gemini"];

const HIGH_ASSURANCE_SURFACES: [&str; 9] = [
    "auth",
    "security",
    "secrets",
    "merge-gate",
    "review-gate",
    "loopcheck",
    "migration",
    "money",
    "payments",
];

/// Select the review policy from plan `size` and named `risk_surfaces`.
/// Deterministic and total (unknown size -> `portable`): any named
/// high-assurance surface wins over size, then L -> `full_sigma`,
/// M -> `diverse_preferred`, else `portable`.
pub(crate) fn classify_review_policy(size: Option<&str>, risk_surfaces: &[String]) -> &'static str {
    let hit = risk_surfaces.iter().any(|s| {
        let t = s.trim().to_ascii_lowercase();
        !t.is_empty() && HIGH_ASSURANCE_SURFACES.contains(&t.as_str())
    });
    if hit {
        return "high_assurance";
    }
    match size.unwrap_or("").trim().to_ascii_uppercase().as_str() {
        "L" => "full_sigma",
        "M" => "diverse_preferred",
        _ => "portable",
    }
}

/// True when a reviewer RESOLVED TO DISPATCH differs from the implementer.
/// `effective_reviewer_kinds` is what the run can actually dispatch to
/// (exhausted and non-serving kinds already removed), so a record that
/// exists but cannot serve never certifies diversity.
pub(crate) fn has_diverse_capacity(
    effective_reviewer_kinds: &[String],
    implementer_provider: &str,
) -> bool {
    let implementer = if implementer_provider.trim().is_empty() {
        CLAUDE.to_string()
    } else {
        implementer_provider.trim().to_ascii_lowercase()
    };
    effective_reviewer_kinds.iter().any(|cand| {
        let cand = cand.trim().to_ascii_lowercase();
        DISPATCHABLE_PROVIDERS.contains(&cand.as_str()) && cand != implementer
    })
}

/// The verdict `assess_assurance` answers: satisfied, the effective mode and
/// the reason string, byte-identical to the Python leg's four outputs.
pub(crate) struct AssuranceVerdict {
    pub satisfied: bool,
    pub effective: &'static str,
    pub reason: &'static str,
}

/// Resolve `policy` against the reviewer resolved to dispatch. Never
/// panics. A PREFLIGHT availability verdict, not a proof a cross-family
/// review completed. `satisfied` and `effective` are correlated, not
/// independent: unresolved iff not satisfied.
pub(crate) fn assess_assurance(
    policy: &str,
    effective_reviewer_kinds: &[String],
    implementer_provider: &str,
    identity_known: bool,
) -> AssuranceVerdict {
    let diverse = has_diverse_capacity(effective_reviewer_kinds, implementer_provider);
    if policy == "high_assurance" {
        if !identity_known {
            return AssuranceVerdict {
                satisfied: false,
                effective: "unresolved",
                reason: "high-assurance requires a known implementer family; identity unknown",
            };
        }
        if !diverse {
            return AssuranceVerdict {
                satisfied: false,
                effective: "unresolved",
                reason: "high-assurance requires different-family capacity; none available",
            };
        }
        return AssuranceVerdict {
            satisfied: true,
            effective: "diverse",
            reason: "different-family reviewer available",
        };
    }
    if diverse {
        return AssuranceVerdict {
            satisfied: true,
            effective: "diverse",
            reason: "different-family reviewer available",
        };
    }
    AssuranceVerdict {
        satisfied: true,
        effective: "portable",
        reason: "same-family fresh-context review (no different-family capacity)",
    }
}

/// The target manifest's `session_id` line, tolerating absence: the first
/// `^session_id:` line wins, its value trimmed, `None` when the file is
/// missing or names nothing.
fn read_state_session_id(state_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_path).ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("session_id:") else {
            continue;
        };
        let trimmed = rest.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Resolve the session nonce: explicit arg (when non-empty), else the state
/// file's value. The ONE place assurance resolution reads the session.
pub(crate) fn resolve_session_id(session_id: Option<&str>, state_path: &Path) -> Option<String> {
    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        return Some(sid.to_string());
    }
    read_state_session_id(state_path)
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The declared `(id, harness)` pairs in merge order, `None` when the load
/// refuses (the caller fails closed, claude-only).
pub(crate) fn load_provider_records(cwd: &Path) -> Option<Vec<(String, String)>> {
    let accounts = crate::accounts::records::load(None, cwd).ok()?;
    Some(
        accounts
            .records
            .into_iter()
            .map(|r| (r.id, r.harness))
            .collect(),
    )
}

/// `runtime_state.is_in_cooldown(id)` for a `None` model: the shared
/// record-level health TTL applies, then `rate_limited_until > now`.
fn is_locked(payload: &Value, account: &str, now: f64) -> bool {
    crate::spawn_gate_lanes::account_locked_until(payload, account, now).is_some()
}

/// One account's `headroom(pid).state is EXHAUSTED` verdict through the
/// shared `fallback_chain` port.
fn account_exhausted(payload: &Value, account: &str, now: f64) -> bool {
    let health_map = parse_provider_health(payload);
    let usage_map = parse_usage(payload);
    let health = health_from_map(health_map.get(account));
    let usage = snapshot_from_map(usage_map.get(account));
    headroom(health.as_ref(), usage.as_ref(), now).verdict == Verdict::Exhausted
}

/// The dispatchable kinds whose provider records are ALL exhausted, `None`
/// when the read failed (the caller fails closed). A kind with no record
/// (e.g. the claude fallback) is never exhausted.
pub(crate) fn exhausted_provider_kinds(cwd: &Path, now: f64) -> Option<Vec<String>> {
    let records = load_provider_records(cwd)?;
    let payload = crate::route_capacity::runtime_state_payload(cwd);
    let mut by_kind: Vec<(String, Vec<String>)> = Vec::new();
    for (id, harness) in &records {
        if !DISPATCHABLE_PROVIDERS.contains(&harness.as_str()) {
            continue;
        }
        match by_kind.iter_mut().find(|(k, _)| *k == *harness) {
            Some((_, ids)) => ids.push(id.clone()),
            None => by_kind.push((harness.clone(), vec![id.clone()])),
        }
    }
    Some(
        by_kind
            .into_iter()
            .filter(|(_, ids)| ids.iter().all(|id| account_exhausted(&payload, id, now)))
            .map(|(k, _)| k)
            .collect(),
    )
}

/// The ordered dispatchable provider kinds available right now: claude
/// first, then one entry per dispatchable harness in declared order with at
/// least one unlocked account, exhausted kinds stably demoted to the end.
pub(crate) fn available_provider_kinds(cwd: &Path, now: f64) -> Vec<String> {
    let mut kinds = vec![CLAUDE.to_string()];
    let Some(records) = load_provider_records(cwd) else {
        return kinds;
    };
    let payload = crate::route_capacity::runtime_state_payload(cwd);
    for (id, harness) in &records {
        if !DISPATCHABLE_PROVIDERS.contains(&harness.as_str()) || kinds.contains(harness) {
            continue;
        }
        if !is_locked(&payload, id, now) {
            kinds.push(harness.clone());
        }
    }
    let Some(exhausted) = exhausted_provider_kinds(cwd, now) else {
        return kinds;
    };
    let mut head = Vec::with_capacity(kinds.len());
    let mut tail = Vec::new();
    for kind in kinds {
        if exhausted.contains(&kind) {
            tail.push(kind);
        } else {
            head.push(kind);
        }
    }
    head.extend(tail);
    head
}

/// `config.review.cross_model.enabled` through a STRICT layered read: every
/// readable layer must parse (one that fails answers `true`, exactly where
/// Python's whole-load settings refusal fails open), then the first layer
/// defining the leaf wins, a non-bool leaf fails open, and no definition
/// anywhere is `false`.
fn cross_model_enabled(cwd: &Path) -> bool {
    let mut parsed: Vec<toml::Table> = Vec::new();
    for path in crate::agents_config::config_candidates(cwd) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return true;
        };
        parsed.push(table);
    }
    for table in &parsed {
        let leaf = table
            .get("review")
            .and_then(|v| v.get("cross_model"))
            .and_then(|v| v.get("enabled"));
        if let Some(leaf) = leaf {
            return leaf.as_bool().unwrap_or(true);
        }
    }
    false
}

/// `policy._local_runtime_kind`: the harness this session runs on, `None`
/// when no marker resolves (an ambiguous two-family env included).
fn local_runtime_kind() -> Option<String> {
    crate::claims::resolve_harness().map(|h| h.trim().to_ascii_lowercase())
}

/// `policy.load_implementer_identity` via the ledger: `(kind, established)`,
/// established only when a row carried a provider_id mapping to a real
/// dispatchable kind. An unmappable id defaults to claude WITHOUT being an
/// established family. Never fails.
pub(crate) fn load_implementer_identity(session_id: &str, cwd: &Path) -> (String, bool) {
    if session_id.is_empty() {
        return (CLAUDE.to_string(), false);
    }
    let ledger_path = crate::paths_cli::ledger_json(cwd);
    let Ok(text) = std::fs::read_to_string(&ledger_path) else {
        return (CLAUDE.to_string(), false);
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return (CLAUDE.to_string(), false);
    };
    let Some(entries) = data.get("entries").and_then(Value::as_array) else {
        return (CLAUDE.to_string(), false);
    };
    let mut provider_id: Option<&str> = None;
    for row in entries {
        let Some(row) = row.as_object() else { continue };
        if row.get("session_id").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        if let Some(pid) = row
            .get("provider_id")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
        {
            provider_id = Some(pid);
        }
    }
    let Some(provider_id) = provider_id else {
        return (CLAUDE.to_string(), false);
    };
    // A value that is already a dispatchable kind is used directly.
    let lowered = provider_id.trim().to_ascii_lowercase();
    if DISPATCHABLE_PROVIDERS.contains(&lowered.as_str()) {
        return (lowered, true);
    }
    // Otherwise looked up in the records table by the RAW id; its harness is
    // the kind when dispatchable.
    if let Some(records) = load_provider_records(cwd) {
        for (id, harness) in records {
            if id == provider_id && DISPATCHABLE_PROVIDERS.contains(&harness.as_str()) {
                return (harness, true);
            }
        }
    }
    (CLAUDE.to_string(), false)
}

#[derive(Serialize)]
struct AssuranceRecord {
    policy: &'static str,
    satisfied: bool,
    effective: &'static str,
    reason: &'static str,
    implementer_provider: String,
    identity_known: bool,
    effective_reviewer_kinds: Vec<String>,
    exhausted_kinds: Vec<String>,
    headroom_unknown: bool,
}

/// The full assurance read as one pure function over (cwd, session inputs),
/// returning the exact stdout bytes and exit the door prints. The parity
/// test calls this under the same hermetic pins the capture driver froze.
pub fn assess_outcome(
    cwd: &Path,
    session_id: Option<&str>,
    state_path: Option<&Path>,
    size: Option<&str>,
    risk_surfaces: &[String],
) -> (String, i32) {
    let now = now_epoch();
    let state = state_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.join(".fno/target-state.md"));
    let sid = resolve_session_id(session_id, &state).unwrap_or_default();
    let (mut implementer, mut identity_known) = load_implementer_identity(&sid, &cwd);
    let ambient = local_runtime_kind();
    if !identity_known {
        if let Some(ambient) = &ambient {
            implementer = ambient.clone();
            identity_known = true;
        }
    }
    let exhausted_opt = exhausted_provider_kinds(&cwd, now);
    let headroom_unknown = exhausted_opt.is_none();
    let exhausted: Vec<String> = exhausted_opt.unwrap_or_default();
    let mut effective: Vec<String> = vec![ambient.clone().unwrap_or_else(|| implementer.clone())];
    effective.sort();
    effective.dedup();
    if cross_model_enabled(&cwd) {
        for kind in available_provider_kinds(&cwd, now) {
            let kind = kind.trim().to_ascii_lowercase();
            if (exhausted.contains(&kind) || headroom_unknown) && kind != CLAUDE {
                continue;
            }
            if !effective.contains(&kind) {
                effective.push(kind);
            }
        }
        effective.sort();
    }
    let policy = classify_review_policy(size, risk_surfaces);
    let verdict = assess_assurance(policy, &effective, &implementer, identity_known);
    let record = AssuranceRecord {
        policy,
        satisfied: verdict.satisfied,
        effective: verdict.effective,
        reason: verdict.reason,
        implementer_provider: implementer,
        identity_known,
        effective_reviewer_kinds: effective,
        exhausted_kinds: {
            let mut e = exhausted;
            e.sort();
            e
        },
        headroom_unknown,
    };
    let exit = if verdict.satisfied { 0 } else { 3 };
    (format!("{}\n", to_python_json(&record)), exit)
}

#[derive(Serialize)]
struct KindsRecord {
    available: Vec<String>,
    dispatchable: Vec<&'static str>,
}

/// The kinds read in the door's record shape, frozen against the same
/// `available_provider_kinds` oracle the assess goldens used.
pub fn kinds_record(cwd: &Path) -> String {
    let available = available_provider_kinds(cwd, now_epoch());
    let record = KindsRecord {
        available,
        dispatchable: DISPATCHABLE_PROVIDERS.to_vec(),
    };
    format!("{}\n", to_python_json(&record))
}

#[derive(Debug, Default)]
pub struct AssessArgs {
    assess_flag: bool,
    size: Option<String>,
    surfaces: Vec<String>,
    session: Option<String>,
    state: Option<String>,
}

/// Door argv for `assess`: flags only, the Python command had no positional.
/// An unknown flag or a stray word is a usage error (exit 2, empty stdout).
/// Public for the parity test's usage-error case.
pub fn parse_assess_args(args: &[String]) -> Result<AssessArgs, String> {
    let expanded = expand_eq(args);
    let mut out = AssessArgs::default();
    let mut i = 0;
    while i < expanded.len() {
        let arg = &expanded[i];
        match arg.as_str() {
            "--assess-assurance" => out.assess_flag = true,
            "--policy-size" => {
                i += 1;
                out.size = Some(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--policy-size needs a value".to_string())?
                        .clone(),
                );
            }
            "--risk-surface" => {
                i += 1;
                out.surfaces.push(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--risk-surface needs a value".to_string())?
                        .clone(),
                );
            }
            "--session-id" => {
                i += 1;
                out.session = Some(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--session-id needs a value".to_string())?
                        .clone(),
                );
            }
            "--state" => {
                i += 1;
                out.state = Some(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--state needs a value".to_string())?
                        .clone(),
                );
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("unknown flag: {other}"));
                }
                return Err(format!("unexpected argument: {other}"));
            }
        }
        i += 1;
    }
    Ok(out)
}

fn print_usage() {
    println!("usage: fno-agents review-policy <assess|kinds> [flags]");
    println!();
    println!("Review assurance verdict and reviewer kinds, the native fno do review root.");
    println!("  assess [--assess-assurance] [--policy-size S] [--risk-surface X]...");
    println!("         [--session-id ID] [--state PATH]");
    println!("    Print the assurance record; exit 3 when a high-assurance change is");
    println!("    unsatisfied, 2 with the refusal line when --assess-assurance is absent.");
    println!("  kinds");
    println!("    Print one record of the available and dispatchable reviewer kinds.");
    println!("  -h, --help    Print this help and exit 0");
}

/// The `fno do review` root's refusal, byte-identical to the Python
/// `typer.secho(..., err=True)` line the goldens froze.
fn print_refusal() {
    eprintln!("{REFUSAL}");
}

/// The refusal line, public for the parity test's refusal case.
pub(crate) const REFUSAL: &str =
    "sigma is removed: the review is bare /fno:review (the fno review lane), \
which runs inline on every harness and emits its own attestation. \
`fno do review classify` and `fno do review resolve-level` remain for \
the lane's emit step.";

/// `review-policy` review door: `assess` answers the verdict or the refusal;
/// `kinds` prints the kinds record; usage errors print one line on stderr
/// and exit 2 with empty stdout.
pub fn run_review_policy(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return 0;
    }
    let Some((action, rest)) = args.split_first() else {
        eprintln!("usage: fno-agents review-policy <assess|kinds> [flags] (missing action)");
        return 2;
    };
    match action.as_str() {
        "kinds" => match parse_kinds_args(rest) {
            Ok(()) => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                print!("{}", kinds_record(&cwd));
                0
            }
            Err(msg) => {
                eprintln!("usage: fno-agents review-policy kinds ({msg})");
                2
            }
        },
        "assess" => {
            let parsed = match parse_assess_args(rest) {
                Ok(p) => p,
                Err(msg) => {
                    eprintln!("usage: fno-agents review-policy assess [flags] ({msg})");
                    return 2;
                }
            };
            if !parsed.assess_flag {
                print_refusal();
                return 2;
            }
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let (stdout, exit) = assess_outcome(
                &cwd,
                parsed.session.as_deref(),
                parsed.state.as_deref().map(Path::new),
                parsed.size.as_deref(),
                &parsed.surfaces,
            );
            print!("{stdout}");
            exit
        }
        other => {
            eprintln!(
                "usage: fno-agents review-policy <assess|kinds> [flags] (unknown action: {other})"
            );
            2
        }
    }
}

/// `kinds` takes no flags: anything present is a usage error.
fn parse_kinds_args(args: &[String]) -> Result<(), String> {
    let expanded = expand_eq(args);
    match expanded.first() {
        None => Ok(()),
        Some(arg) => Err(format!("unexpected argument: {arg}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_and_assess_points() {
        // The size matrix and the padded/unknown tokens.
        assert_eq!(classify_review_policy(Some("S"), &[]), "portable");
        assert_eq!(classify_review_policy(Some("M"), &[]), "diverse_preferred");
        assert_eq!(classify_review_policy(Some("L"), &[]), "full_sigma");
        assert_eq!(classify_review_policy(None, &[]), "portable");
        assert_eq!(classify_review_policy(Some("XL"), &[]), "portable");
        assert_eq!(classify_review_policy(Some(" l "), &[]), "full_sigma");
        // Surfaces win over size; padded case-insensitive; unknown surfaces don't.
        assert_eq!(
            classify_review_policy(Some("S"), &["auth".to_string()]),
            "high_assurance"
        );
        assert_eq!(
            classify_review_policy(Some("S"), &["docs".to_string(), " auth ".to_string()]),
            "high_assurance"
        );
        assert_eq!(
            classify_review_policy(Some("L"), &[" Auth ".to_string()]),
            "high_assurance"
        );
        assert_eq!(
            classify_review_policy(Some("L"), &["docs".to_string()]),
            "full_sigma"
        );

        let kinds =
            |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };
        // High assurance is the only blocker.
        let v = assess_assurance("high_assurance", &kinds(&["claude"]), "claude", false);
        assert!(!v.satisfied && v.effective == "unresolved");
        let v = assess_assurance("high_assurance", &kinds(&["claude"]), "claude", true);
        assert!(!v.satisfied && v.effective == "unresolved");
        let v = assess_assurance(
            "high_assurance",
            &kinds(&["claude", "codex"]),
            "claude",
            true,
        );
        assert!(v.satisfied && v.effective == "diverse");
        // portable / diverse_preferred / full_sigma never block.
        for policy in ["portable", "diverse_preferred", "full_sigma"] {
            let v = assess_assurance(policy, &kinds(&["claude"]), "claude", false);
            assert!(v.satisfied && v.effective == "portable", "{policy}");
            let v = assess_assurance(policy, &kinds(&["codex"]), "claude", true);
            assert!(v.satisfied && v.effective == "diverse", "{policy}");
        }
        // Diversity needs a DISPATCHABLE kind that differs from the implementer:
        // opencode is not dispatchable, an unknown kind is not either.
        let v = assess_assurance(
            "high_assurance",
            &kinds(&["claude", "opencode"]),
            "claude",
            true,
        );
        assert!(!v.satisfied);
        let v = assess_assurance("high_assurance", &kinds(&["codex", "wat"]), "codex", true);
        assert!(!v.satisfied);
        // A blank implementer defaults to claude.
        assert!(has_diverse_capacity(&kinds(&["codex"]), ""));
        assert!(!has_diverse_capacity(&kinds(&["claude"]), ""));
        // Verdict correlation: unresolved iff not satisfied, on every input above.
    }
}

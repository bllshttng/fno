//! `fno do review resolve-level`: resolve a review effort level to the
//! (band, effort, model) route pick, ported from the Python leaf and frozen
//! byte-for-byte against goldens captured from it before deletion.
//!
//! The output contract is one JSON line on stdout, keys in the Python dict
//! order (level, level_source, band, effort, model, provider, chain,
//! degraded_max), serialized with json.dumps default separators. The
//! serialization is hand-rolled because a serde struct would sort or
//! re-order keys and break byte parity.
//!
//! Tier asks ride the one-resolver law (`route_gather::tier_leg`, which asks
//! `route_slot::resolve_slot_payload`); this module never re-implements tier
//! resolution. The parity test is `tests/review_level_parity.rs`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const FALLBACK_LEVEL: &str = "medium";

/// Explicit level -> (band, effort). Mirrors the Python leaf's map; the
/// degraded-max walk reads these spellings from chain entries, so the
/// strings are contract, not display.
fn level_to_band_effort(level: &str) -> Option<(&'static str, &'static str)> {
    match level {
        "low" => Some(("low", "high")),
        "medium" => Some(("medium", "high")),
        "high" => Some(("high", "high")),
        "xhigh" => Some(("high", "max")),
        "max" => Some(("max", "max")),
        _ => None,
    }
}

/// Route provider -> harness the tier ask resolves under. A provider with
/// no row routes under no harness (its family never applies).
fn provider_harness(route_provider: &str) -> Option<&'static str> {
    match route_provider {
        "zai" => Some("claude"),
        "anthropic" => Some("claude"),
        "openai" => Some("codex"),
        "opencode" => Some("opencode"),
        _ => None,
    }
}

/// Route providers whose model family may override the harness pick.
fn provider_model_families(route_provider: &str) -> Option<&'static [&'static str]> {
    match route_provider {
        "zai" => Some(&["glm-"]),
        _ => None,
    }
}

/// Static-tier fallthrough order per band: where a provider family may pick
/// when the harness band holds no family model.
fn static_fallthrough(band: &str) -> Option<&'static [&'static str]> {
    match band {
        "max" => Some(&["max", "high", "medium", "low"]),
        "high" => Some(&["high", "medium", "low"]),
        "medium" => Some(&["medium", "high", "low"]),
        "low" => Some(&["low", "medium", "high"]),
        _ => None,
    }
}

/// Session provider stamp: FNO_ROUTE_PROVIDER trimmed, "unknown" when unset
/// or blank.
fn session_provider() -> String {
    let raw = std::env::var("FNO_ROUTE_PROVIDER").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The route provider for one ask: an explicit `--provider` value wins
/// verbatim (an empty value is "unknown", it does NOT fall through to the
/// session stamp), else the session stamp.
fn route_provider(flag: Option<&str>) -> String {
    match flag {
        Some(p) => {
            if p.is_empty() {
                "unknown".to_string()
            } else {
                p.to_string()
            }
        }
        None => session_provider(),
    }
}

/// Mirror of the leaf's `static ([a-z]+) -> ` scanner: finds the leftmost
/// `static <band> -> ` occurrence and returns <band>. Tried one byte past a
/// failed occurrence, which for a non-overlapping needle is the same set of
/// start positions the regex tries.
fn static_pick_band(entry: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(rel) = entry[from..].find("static ") {
        let start = from + rel;
        let after = &entry[start + "static ".len()..];
        let end = after
            .find(|c: char| !c.is_ascii_lowercase())
            .unwrap_or(after.len());
        if end > 0 && after[end..].starts_with(" -> ") {
            return Some(&after[..end]);
        }
        from = start + 1;
    }
    None
}

/// True when a max-band route degraded off a true max model. Scans the chain
/// in reverse: a `static <band> -> ` entry answers "degraded" when its pick
/// band is not max; a band-snapshot/inventory entry answers when it carries
/// a `degrade ->` marker; anything else keeps scanning.
fn degraded_max(band: &str, model: Option<&str>, chain: &[Value]) -> bool {
    if band != "max" || model.is_none() {
        return false;
    }
    for entry in chain.iter().rev() {
        let Some(s) = entry.as_str() else { continue };
        if let Some(pick) = static_pick_band(s) {
            return pick != "max";
        }
        if s.starts_with("snapshot band(") || s.starts_with("inventory band(") {
            return s.contains("degrade ->");
        }
    }
    false
}

/// When the route provider declares a model family and the harness pick is
/// not one of its models, walk the STATIC tier table (not the declared
/// inventory: reachable() answers from the inventory, and asking the two
/// sources one question misses on every install that declares none) for the
/// first family model reachable under the provider's harness. The match is
/// appended to the chain so the pick stays auditable.
fn prefer_provider_family(
    route_provider: &str,
    band: &str,
    model: Option<&str>,
    chain: &mut Vec<Value>,
) -> Option<String> {
    let families = provider_model_families(route_provider)?;
    if model.is_some_and(|m| families.iter().any(|f| m.starts_with(f))) {
        return None;
    }
    let harness = provider_harness(route_provider)?;
    let (static_tiers, reachability) = crate::route_gather::load_model_tiers();
    let bands: Vec<&str> = static_fallthrough(band)
        .map(|order| order.to_vec())
        .unwrap_or_else(|| vec![band]);
    for cand_band in bands {
        let names: &[String] = static_tiers
            .iter()
            .find(|(b, _)| b.as_str() == cand_band)
            .map(|(_, names)| names.as_slice())
            .unwrap_or(&[]);
        for name in names {
            let reach = reachability
                .get(name)
                .and_then(Value::as_array)
                .and_then(|r| r.first())
                .and_then(Value::as_str);
            // Python's `if reach and reach[0] == harness`: a missing row
            // skips, it never matches an absent harness.
            if reach.is_some_and(|h| h == harness) && families.iter().any(|f| name.starts_with(f)) {
                chain.push(json!(format!(
                    "provider family({route_provider}) static {cand_band} -> {name}"
                )));
                return Some(name.clone());
            }
        }
    }
    None
}

/// Diff-size tiers: (level, max changed files, max changed lines). First row
/// where BOTH fits wins; anything larger is max.
const LEVEL_TIERS: &[(&str, u64, u64)] = &[
    ("low", 3, 150),
    ("medium", 10, 600),
    ("high", 25, 2500),
    ("xhigh", 60, 8000),
];

fn level_for_diff(changed_files: u64, diff_lines: u64) -> &'static str {
    for (level, files_cap, lines_cap) in LEVEL_TIERS {
        if changed_files <= *files_cap && diff_lines <= *lines_cap {
            return level;
        }
    }
    "max"
}

/// One git read with a hard 15s deadline. Stdout goes to a temp file, not a
/// pipe: a numstat larger than the pipe buffer would deadlock a
/// read-after-exit drain (the Python leg's communicate() drained
/// concurrently, so parity requires the file). The capture path is created
/// exclusively (create_new), never following a pre-planted or symlinked
/// temp name. Any failure - spawn, wait, nonzero exit, timeout, undecodable
/// output, empty output - is None.
fn git_out(cwd: &Path, git_args: &[&str]) -> Option<String> {
    use std::fs::{File, OpenOptions};
    use std::io::Read as _;
    static NEXT_STDOUT: AtomicUsize = AtomicUsize::new(0);
    // create_new refuses a symlink (or any existing file) at the final
    // component, which a plain File::create would follow; on a collision
    // the read degrades to None, like any other git failure.
    let stdout_path = std::env::temp_dir().join(format!(
        "fno-review-level-gitout-{}-{}",
        std::process::id(),
        NEXT_STDOUT.fetch_add(1, Ordering::Relaxed)
    ));
    let out_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stdout_path)
        .ok()?;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(git_args)
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&stdout_path);
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = std::fs::remove_file(&stdout_path);
                return None;
            }
        }
    };
    if !status.success() {
        let _ = std::fs::remove_file(&stdout_path);
        return None;
    }
    let mut text = String::new();
    let read_ok = File::open(&stdout_path)
        .and_then(|mut f| f.read_to_string(&mut text))
        .is_ok();
    let _ = std::fs::remove_file(&stdout_path);
    if !read_ok {
        return None;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Diff-sized level for a checkout: first candidate whose ref verifies wins
/// (the loop breaks on it even when merge-base then fails), its merge-base
/// with HEAD is diffed as numstat, and the first tier fitting both caps
/// names the level. No repo, no merge base, no rows, or any git failure is
/// None.
fn diff_review_level(project_root: &Path) -> Option<&'static str> {
    let mut candidate_name: Option<&str> = None;
    for candidate in ["origin/main", "origin/master"] {
        let probe_ref = format!("{candidate}^{{commit}}");
        let probe = ["rev-parse", "--verify", probe_ref.as_str()];
        if git_out(project_root, &probe).is_some() {
            candidate_name = Some(candidate);
            break;
        }
    }
    let candidate = candidate_name?;
    let base = git_out(project_root, &["merge-base", "HEAD", candidate])?;
    let numstat = git_out(project_root, &["diff", "--numstat", &base])?;
    let mut files = 0u64;
    let mut lines = 0u64;
    for row in numstat.lines() {
        let parts: Vec<&str> = row.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        files += 1;
        for cell in parts.iter().take(2) {
            let cell = cell.trim();
            if cell == "-" {
                continue;
            }
            lines += if cell.is_empty() {
                0
            } else {
                cell.parse::<u64>().ok()?
            };
        }
    }
    if files == 0 {
        None
    } else {
        Some(level_for_diff(files, lines))
    }
}

/// The full resolve + render as one pure function over (level, provider,
/// project root), returning the exact stdout bytes the door prints. The
/// parity test calls this directly; `run_review_level` wraps it.
pub fn resolve_level_stdout(
    level_or_none: Option<&str>,
    provider: Option<&str>,
    project_root: &Path,
) -> String {
    let explicit = level_or_none.is_some();
    let mut level: String = level_or_none
        .map(|l| l.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mut level_source = if explicit { "explicit" } else { "fallback" };
    if !explicit {
        match diff_review_level(project_root) {
            Some(sized) => {
                level = sized.to_string();
                level_source = "diff-sized";
            }
            None => {
                level = FALLBACK_LEVEL.to_string();
                level_source = "fallback";
            }
        }
    }
    if level_to_band_effort(&level).is_none() {
        // An explicit unknown token keeps source honest: provenance would be
        // false if it stayed "explicit".
        level = FALLBACK_LEVEL.to_string();
        level_source = "fallback";
    }
    let Some((band, effort)) = level_to_band_effort(&level) else {
        return render(
            &level,
            level_source,
            FALLBACK_LEVEL,
            "high",
            None,
            "unknown",
            &[],
            false,
        );
    };
    let route_provider = route_provider(provider);
    let ask = json!({"provider": provider_harness(&route_provider)});
    let (model_value, chain_values) = crate::route_gather::tier_leg(band, &ask, project_root);
    let mut model: Option<String> = model_value.as_str().map(str::to_string);
    let mut chain: Vec<Value> = chain_values;
    if let Some(preferred) =
        prefer_provider_family(&route_provider, band, model.as_deref(), &mut chain)
    {
        model = Some(preferred);
    }
    let degraded = degraded_max(band, model.as_deref(), &chain);
    render(
        &level,
        level_source,
        band,
        effort,
        model,
        &route_provider,
        &chain,
        degraded,
    )
}

/// Hand-rolled serialization in the Python dict key order with json.dumps
/// default separators (", " between items, ": " after keys); strings escape
/// through serde_json's string writer, matching ensure_ascii=False bytes.
#[allow(clippy::too_many_arguments)]
fn render(
    level: &str,
    level_source: &str,
    band: &str,
    effort: &str,
    model: Option<String>,
    provider: &str,
    chain: &[Value],
    degraded_max: bool,
) -> String {
    let esc = |s: &str| Value::String(s.to_string()).to_string();
    let model_json = match &model {
        Some(m) => esc(m),
        None => "null".to_string(),
    };
    let chain_items: Vec<String> = chain
        .iter()
        .map(|v| esc(v.as_str().unwrap_or_default()))
        .collect();
    format!(
        "{{\"level\": {}, \"level_source\": {}, \"band\": {}, \"effort\": {}, \"model\": {}, \"provider\": {}, \"chain\": [{}], \"degraded_max\": {}}}\n",
        esc(level),
        esc(level_source),
        esc(band),
        esc(effort),
        model_json,
        esc(provider),
        chain_items.join(", "),
        if degraded_max { "true" } else { "false" }
    )
}

#[derive(Debug)]
struct ReviewLevelArgs {
    level: Option<String>,
    provider: Option<String>,
    root: Option<String>,
}

/// Door argv: `[level] [--provider P] [--root PATH]`. The leaf's own front
/// door had no `--root` (it used the cwd); the flag exists for the Python
/// caller that replaces the sizing leg with this verb. Anything else - an
/// unknown flag, a flag missing its value, a second positional - is a usage
/// error, matching the typer door's exit 2 with empty stdout.
pub fn parse_args(args: &[String]) -> Result<ReviewLevelArgs, String> {
    let mut out = ReviewLevelArgs {
        level: None,
        provider: None,
        root: None,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--provider" {
            i += 1;
            out.provider = Some(
                args.get(i)
                    .ok_or_else(|| "--provider needs a value".to_string())?
                    .clone(),
            );
        } else if arg == "--root" {
            i += 1;
            out.root = Some(
                args.get(i)
                    .ok_or_else(|| "--root needs a value".to_string())?
                    .clone(),
            );
        } else if arg.starts_with('-') {
            return Err(format!("unknown flag: {arg}"));
        } else if out.level.is_none() {
            out.level = Some(arg.clone());
        } else {
            return Err(format!("unexpected argument: {arg}"));
        }
        i += 1;
    }
    Ok(out)
}

/// `fno do review resolve-level` binary door: print the JSON line, exit 0;
/// `-h`/`--help` prints the usage text to stdout and exits 0 (the typer
/// door's posture); usage errors print to stderr and exit 2 with empty
/// stdout.
pub fn run_review_level(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: fno do review resolve-level [level] [--provider P] [--root PATH]");
        println!();
        println!("Resolve a review level to the routing record and print it as one JSON line.");
        println!("  [level]       low|medium|high|xhigh|max; default sizes from the diff");
        println!("  --provider P  Override the provider axis for this resolution");
        println!("  --root PATH   Resolve the diff at this project root");
        println!("  -h, --help    Print this help and exit 0");
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!(
                "usage: fno do review resolve-level [level] [--provider P] [--root PATH] ({msg})"
            );
            return 2;
        }
    };
    let root = parsed
        .root
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let level = parsed.level.as_deref().and_then(|l| {
        if l.is_empty() {
            None
        } else {
            Some(l.trim().to_ascii_lowercase())
        }
    });
    print!(
        "{}",
        resolve_level_stdout(level.as_deref(), parsed.provider.as_deref(), &root)
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn chain(items: &[&str]) -> Vec<Value> {
        items
            .iter()
            .map(|s| Value::String((*s).to_string()))
            .collect()
    }

    #[test]
    fn level_for_diff_boundaries() {
        assert_eq!(level_for_diff(0, 0), "low");
        assert_eq!(level_for_diff(3, 150), "low");
        assert_eq!(level_for_diff(3, 151), "medium");
        assert_eq!(level_for_diff(4, 150), "medium");
        assert_eq!(level_for_diff(10, 600), "medium");
        assert_eq!(level_for_diff(10, 601), "high");
        assert_eq!(level_for_diff(25, 2500), "high");
        assert_eq!(level_for_diff(25, 2501), "xhigh");
        assert_eq!(level_for_diff(60, 8000), "xhigh");
        assert_eq!(level_for_diff(61, 0), "max");
        assert_eq!(level_for_diff(1, 8001), "max");
    }

    #[test]
    fn degraded_max_scans_reversed_chain() {
        // Harness pick held a true max model: not degraded.
        assert!(!degraded_max(
            "max",
            Some("fable"),
            &chain(&["tier(max)", "inventory band(>=max) -> claude-fable"])
        ));
        // Family pick from a lower band: degraded.
        assert!(degraded_max(
            "max",
            Some("glm-5.3[1m]"),
            &chain(&[
                "tier(max)",
                "inventory band(>=max) -> claude-fable",
                "provider family(zai) static medium -> glm-5.3[1m]",
            ])
        ));
        // Family pick that still held the max band: not degraded.
        assert!(!degraded_max(
            "max",
            Some("glm-x"),
            &chain(&["tier(max)", "provider family(zai) static max -> glm-x"])
        ));
        // Snapshot marker answers the degrade flag, not the band.
        assert!(degraded_max(
            "max",
            Some("x"),
            &chain(&["tier(max)", "snapshot band(max) degrade -> high"])
        ));
        assert!(!degraded_max(
            "max",
            Some("x"),
            &chain(&["tier(max)", "snapshot band(max) -> high"])
        ));
        // Non-max band and absent model short-circuit false.
        assert!(!degraded_max(
            "high",
            Some("x"),
            &chain(&["provider family(zai) static max -> y"])
        ));
        assert!(!degraded_max(
            "max",
            None,
            &chain(&["provider family(zai) static max -> y"])
        ));
    }
}

//! Read-only machine detection for both setup hosts - the agent host
//! `fno config setup run --dry-run` and the human host `fno do wizard` (and
//! the `--check` mode that follows both): which harnesses are installed,
//! what the machine sample says, whether gh can ship, what config exists.
//! Every probe is a file read or a bounded subprocess: a detection pass
//! must finish on a loaded or half-broken machine, so each subprocess is
//! killed at its deadline and a failure is recorded as an `unreadable`
//! row carrying the error, never a failed run. Nothing here writes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::process_admission::{std_command, std_spawn};

/// One probe subprocess, capped: spawn, poll, kill at the deadline. std
/// has no timeout, so the poll loop is the whole implementation. `Err`
/// names the command and its stderr; the caller turns that into an
/// `unreadable` detection row.
pub(crate) fn bounded_output(argv: &[&str]) -> Result<String, String> {
    const CAP: Duration = Duration::from_secs(3);
    let mut cmd = std_command(argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = std_spawn(&mut cmd).map_err(|e| format!("{}: {e}", argv[0]))?;
    let deadline = Instant::now() + CAP;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{}: timed out after 3s", argv[0]));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{}: {e}", argv[0])),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!(
            "`{}` exited {}: {detail}",
            argv.join(" "),
            out.status.code().unwrap_or(-1)
        ))
    }
}

/// The live band number from the machine sample's printed clause, the
/// integer after `budget max_live`. The one parser both the run step and
/// detection use, so they can never disagree about what a real run would
/// write (ruling: the budget formula itself stays in machine_load; every
/// reader parses the printed clause).
pub(crate) fn parse_budget_clause(text: &str) -> Option<i64> {
    let i = text.find("budget max_live ")?;
    let rest = &text[i + "budget max_live ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i64>().ok()
}

/// The band clause exactly as the machine sample prints it, read from
/// `fno-agents status`. `None` when the sample has not run yet or the
/// agents binary is missing - the caller says so rather than guessing.
pub(crate) fn budget_clause() -> Option<String> {
    let bin = crate::digest_overlay::fno_agents_bin();
    let bin = bin.to_str()?;
    bounded_output(&[bin, "status"])
        .ok()?
        .lines()
        .find(|l| l.contains("budget max_live"))
        .map(|l| l.trim().to_string())
}

/// The one detection pass behind `--dry-run` (and the wizard): read-only,
/// every probe bounded, a failing probe an `unreadable` row instead of a
/// failed run. `scan` gates the repo walk: `None` leaves the section out
/// entirely, so a fresh machine answers fast and carries no repo list;
/// `Some(roots)` walks the named roots, an empty slice meaning the usual
/// code roots under HOME.
pub fn detect(root: &Path, scan: Option<&[PathBuf]>) -> Value {
    let mut doc = json!({
        "harnesses": detect_harnesses(),
        "machine": detect_machine(),
        "gh": detect_gh(),
        "accounts": detect_accounts(),
        "tools": detect_tools(),
        "services": detect_services(),
        "config": detect_config(root),
    });
    if let Some(roots) = scan {
        doc["repos"] = detect_repos(root, roots);
    }
    doc
}

/// Every harness the auto-wire pass knows, and whether its binary is on
/// PATH with a readable `--version` first line. Detection only looks;
/// the wiring verb still owns every write.
fn detect_harnesses() -> Value {
    Value::Array(
        crate::setup_autowire::HARNESS_LIST
            .iter()
            .map(|h| {
                let on = crate::setup_autowire::on_path(h.name);
                let version = if on {
                    bounded_output(&[h.name, "--version"])
                        .ok()
                        .and_then(|o| o.lines().next().map(|l| l.trim().to_string()))
                } else {
                    None
                };
                json!({
                    "name": h.name,
                    "label": h.label,
                    "on_path": on,
                    "version": version,
                })
            })
            .collect(),
    )
}

fn detect_machine() -> Value {
    let cores = std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u64);
    json!({
        "cores": cores,
        "total_mem_gb": probe_total_mem_gb(),
        "budget": budget_clause(),
    })
}

const GB: f64 = 1_073_741_824.0;

/// Total memory in GB: `sysctl -n hw.memsize` on macOS, `/proc/meminfo`'s
/// MemTotal on Linux. `None` when neither reads - the caller shows the
/// row as unknown rather than inventing a number.
fn probe_total_mem_gb() -> Option<f64> {
    #[cfg(target_os = "macos")]
    let bytes: Option<u64> = bounded_output(&["sysctl", "-n", "hw.memsize"])
        .ok()
        .and_then(|s| s.trim().parse().ok());
    #[cfg(not(target_os = "macos"))]
    let bytes: Option<u64> = std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| {
        t.lines().find_map(|l| {
            l.strip_prefix("MemTotal:")
                .and_then(|r| r.trim().strip_suffix("kB"))
                .and_then(|r| r.trim().parse().ok())
                .map(|kb| kb * 1024)
        })
    });
    bytes.map(|b| b as f64 / GB)
}

const GH_REQUIRED_SCOPES: [&str; 2] = ["repo", "workflow"];

/// gh login state: the scopes parsed off the `Token scopes:` line (`read:org`
/// carries a colon, so the split anchors on the label, never on `:`), and
/// which of the scopes the ship path needs are absent. A missing or
/// unsigned-in gh is an `unreadable` row with the error, never a failed run.
fn detect_gh() -> Value {
    let scopes = match bounded_output(&["gh", "auth", "status"]) {
        Err(e) => {
            return json!({
                "status": "unreadable",
                "error": e,
                "scopes": [],
                "missing": GH_REQUIRED_SCOPES,
            });
        }
        Ok(out) => out
            .lines()
            .find_map(|l| l.split_once("Token scopes:").map(|(_, rest)| rest))
            .map(|rest| {
                rest.split(',')
                    .map(|s| s.trim().trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default(),
    };
    let missing: Vec<&str> = GH_REQUIRED_SCOPES
        .into_iter()
        .filter(|s| !scopes.iter().any(|have| have == s))
        .collect();
    json!({
        "status": "ok",
        "scopes": scopes,
        "missing": missing,
    })
}

/// Configured provider-account rows, counted from the global config file
/// (an `[[accounts]]` array of tables, or `accounts.<n>` flattened rows).
/// A count, never the records: the accounts verb owns reading those.
fn detect_accounts() -> Value {
    let path = crate::config_defaults::global_config_path();
    match crate::config_defaults::read_flat(&path) {
        Err(e) => json!({ "rows": null, "error": e }),
        Ok(None) => json!({ "rows": 0 }),
        Ok(Some(map)) => {
            let rows = match map.get("accounts") {
                Some(toml::Value::Array(rows)) => rows.len(),
                _ => {
                    let mut idxs: Vec<String> = map
                        .keys()
                        .filter_map(|k| k.strip_prefix("accounts."))
                        .map(|rest| rest.split('.').next().unwrap_or(rest).to_string())
                        .collect();
                    idxs.sort();
                    idxs.dedup();
                    idxs.len()
                }
            };
            json!({ "rows": rows })
        }
    }
}

/// The optional machine tools setup may name. `macmon` feeds the resource
/// meter; absent is a normal state, not an error.
fn detect_tools() -> Value {
    json!({ "macmon": crate::setup_autowire::on_path("macmon") })
}

const SERVICE_LABELS: [&str; 2] = ["sh.fno.pr-watcher", "sh.fno.groom"];

/// The launchd services and the daemon, one pass: plists are file reads,
/// `launchctl list` is one bounded call for every label (the label is the
/// last whitespace column), and the daemon state rides
/// `fno-agents status --json`'s `daemon.state`.
fn detect_services() -> Value {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let agents = crate::digest_overlay::fno_agents_bin();
    let daemon = agents
        .to_str()
        .and_then(|bin| bounded_output(&[bin, "status", "--json"]).ok())
        .and_then(|out| serde_json::from_str::<Value>(out.trim()).ok())
        .and_then(|v| {
            v.get("daemon")
                .and_then(|d| d.get("state"))
                .and_then(Value::as_str)
                .map(String::from)
        })
        .unwrap_or_else(|| "unknown".into());
    let plist = |label: &str| {
        !home.as_os_str().is_empty()
            && home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{label}.plist"))
                .exists()
    };
    let ctl = bounded_output(&["launchctl", "list"]).ok();
    let loaded = |label: &str| {
        ctl.as_deref()
            .map(|out| {
                out.lines()
                    .any(|l| l.split_whitespace().last() == Some(label))
            })
            .unwrap_or(false)
    };
    let rows: Vec<Value> = SERVICE_LABELS
        .iter()
        .map(|label| {
            json!({
                "label": label,
                "plist": plist(label),
                "loaded": loaded(label),
            })
        })
        .collect();
    json!({ "services": rows, "daemon_state": daemon })
}

/// Git checkouts one and two levels under each scan root, capped so
/// detection stays bounded on a full disk. An empty `roots` slice means the
/// usual code roots under HOME; explicit roots (`--scan-dirs`) are taken as
/// given and need no HOME. A repo is `registered` when any flattened global
/// config value names its path (work.workspaces entries carry it as a bare
/// string or a list row's `path`).
fn detect_repos(root: &Path, roots: &[PathBuf]) -> Value {
    const SCAN_ROOTS: [&str; 5] = ["code", "src", "dev", "projects", "work"];
    const DIR_READ_CAP: usize = 200;
    let global = crate::config_defaults::global_config_path();
    let flat = crate::config_defaults::read_flat(&global)
        .unwrap_or_default()
        .unwrap_or_default();
    let registered = |path: &str| flat.values().any(|v| config_mentions(v, path));
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let bases: Vec<PathBuf> = if roots.is_empty() {
        if home.as_os_str().is_empty() {
            Vec::new()
        } else {
            SCAN_ROOTS.iter().map(|b| home.join(b)).collect()
        }
    } else {
        roots.to_vec()
    };
    let mut found: Vec<Value> = Vec::new();
    let mut reads = 0usize;
    for base in &bases {
        if reads >= DIR_READ_CAP {
            break;
        }
        let Ok(l1) = std::fs::read_dir(base) else {
            continue;
        };
        reads += 1;
        for e1 in l1.flatten() {
            if reads >= DIR_READ_CAP {
                break;
            }
            let p1 = e1.path();
            if !p1.is_dir() {
                continue;
            }
            if p1.join(".git").exists() {
                push_repo(&mut found, &p1, root, &registered);
                continue;
            }
            let Ok(l2) = std::fs::read_dir(&p1) else {
                continue;
            };
            reads += 1;
            for e2 in l2.flatten() {
                if reads >= DIR_READ_CAP {
                    break;
                }
                let p2 = e2.path();
                if p2.is_dir() && p2.join(".git").exists() {
                    push_repo(&mut found, &p2, root, &registered);
                }
            }
        }
    }
    json!({ "scan_cap": DIR_READ_CAP, "found": found })
}

/// Whether a flattened config value names `path`: a bare string, an array
/// of strings, or an array of tables each carrying a `path` string.
fn config_mentions(value: &toml::Value, path: &str) -> bool {
    match value {
        toml::Value::String(s) => s == path,
        toml::Value::Array(a) => a.iter().any(|v| {
            v.as_str() == Some(path) || v.get("path").and_then(toml::Value::as_str) == Some(path)
        }),
        _ => false,
    }
}

fn push_repo(out: &mut Vec<Value>, path: &Path, root: &Path, registered: &dyn Fn(&str) -> bool) {
    let display = path.display().to_string();
    out.push(json!({
        "path": display,
        "this_repo": path == root,
        "registered": registered(&display),
    }));
}

/// Config leaf counts for both files, so the skill can say "your config is
/// essentially empty" without reading any value.
fn detect_config(root: &Path) -> Value {
    json!({
        "global": count_leaves(&crate::config_defaults::global_config_path()),
        "project": count_leaves(&root.join(".fno").join("config.toml")),
    })
}

fn count_leaves(path: &Path) -> Value {
    match crate::config_defaults::read_flat(path) {
        Ok(None) => json!({ "leaves": 0, "absent": true }),
        Ok(Some(map)) => json!({ "leaves": map.len() }),
        Err(e) => json!({ "leaves": null, "error": e }),
    }
}

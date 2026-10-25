//! Front commands for the Rust route family. Config mutations keep the validated writer.

use serde_json::Value;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    // Every spelling the retired Python mounts answered: the config group's
    // two (route and its routing alias), the bare root group, and the
    // doctor's hidden read-only spelling (which pinned inventory mode).
    let first = args.first().and_then(|s| s.to_str());
    let second = args.get(1).and_then(|s| s.to_str());
    match (first, second) {
        (Some("config"), Some("route")) | (Some("config"), Some("routing")) => {
            Some(args[2..].to_vec())
        }
        (Some("route"), _) => Some(args[1..].to_vec()),
        (Some("doctor"), Some("route")) => Some(
            std::iter::once(OsString::from("inventory"))
                .chain(args[2..].iter().cloned())
                .collect(),
        ),
        _ => None,
    }
}

pub fn run(args: &[OsString]) -> i32 {
    let op = args.first().and_then(|s| s.to_str()).unwrap_or("");
    if args.is_empty() || args.iter().any(|s| s == "--help" || s == "-h") {
        println!("Provider route lanes: ls / set ROLE PROVIDER/MODEL / unset ROLE / env SPEC / inventory / init / settings ls");
        return if args.is_empty() { 2 } else { 0 };
    }
    let mut door = Vec::new();
    match op {
        "ls" => door.push(OsString::from("table")),
        "set" | "unset" | "init" => {
            door.push(OsString::from("plan-write"));
            door.push(OsString::from(op));
        }
        "env" | "inventory" | "settings" => door.push(OsString::from(op)),
        _ => {
            eprintln!("route: unknown command {op}");
            return 2;
        }
    }
    door.extend_from_slice(&args[1..]);
    // route-slot is the routing ops' only entry: the argv rides one payload
    // file as `{mode: config-route, argv: [...]}`.
    let payload = serde_json::json!({
        "mode": "config-route",
        "argv": door
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    });
    let payload_path = std::env::temp_dir().join(format!(
        "fno-route-door-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    if let Err(e) = std::fs::write(&payload_path, payload.to_string()) {
        eprintln!("fno config route: could not write door payload: {e}");
        return 1;
    }
    let ran = Command::new(fno::digest_overlay::fno_agents_bin())
        .arg("route-slot")
        .arg(&payload_path)
        .stdin(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&payload_path);
    let result = match ran {
        Ok(result) => result,
        Err(e) => {
            eprintln!("fno config route: could not run fno-agents: {e}");
            return 1;
        }
    };
    let out = String::from_utf8_lossy(&result.stdout);
    let err = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() {
        print!("{out}");
        eprint!("{err}");
        return result.status.code().unwrap_or(1);
    }
    eprint!("{err}");
    if op == "ls" {
        let rows: Value = match serde_json::from_str(&out) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("route ls: invalid answer: {e}");
                return 1;
            }
        };
        if args.iter().any(|a| a == "--json" || a == "-J") {
            print!("{out}");
        } else {
            print!("{}", table(&rows));
        }
        return 0;
    }
    if !matches!(op, "set" | "unset" | "init") {
        print!("{out}");
        return 0;
    }
    let plan: Value = match serde_json::from_str(&out) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("route {op}: invalid write plan: {e}");
            return 1;
        }
    };
    let field = |key: &str| plan.get(key).and_then(Value::as_str).unwrap_or("");
    eprint!("{}", field("err"));
    match field("action") {
        "noop" => {}
        "init" => return append_sample(Path::new(field("path"))),
        action @ ("set" | "unset") => {
            let mut command = match std::env::current_exe() {
                Ok(exe) => Command::new(exe),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            command.args(["config", action, field("key")]);
            if action == "set" {
                command.arg(plan.get("value").unwrap_or(&Value::Null).to_string());
            }
            command.arg(if field("scope") == "project" {
                "--local"
            } else {
                "--global"
            });
            let write = match command.stdin(Stdio::null()).output() {
                Ok(result) => result,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            if !write.status.success() {
                eprint!("{}", String::from_utf8_lossy(&write.stderr));
                return write.status.code().unwrap_or(1);
            }
        }
        other => {
            eprintln!("route {op}: invalid write action {other}");
            return 1;
        }
    }
    print!("{}", field("out"));
    0
}

fn table(rows: &Value) -> String {
    let cols = ["role", "provider_model", "protocol", "key", "assigned_by"];
    let headers = ["ROLE", "PROVIDER/MODEL", "PROTOCOL", "KEY", "ASSIGNED-BY"];
    let rows = rows.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let widths: Vec<_> = cols
        .iter()
        .zip(headers)
        .map(|(c, h)| {
            rows.iter()
                .map(|r| {
                    r.get(*c)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .count()
                })
                .chain([h.len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let render = |cells: Vec<&str>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(s, w)| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count()))))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
            + "\n"
    };
    let mut out = render(headers.to_vec());
    for row in rows {
        out.push_str(&render(
            cols.iter()
                .map(|c| row.get(*c).and_then(Value::as_str).unwrap_or(""))
                .collect(),
        ));
    }
    out
}

fn append_sample(path: &Path) -> i32 {
    let result = (|| -> std::io::Result<bool> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = path.with_extension("toml.lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)?;
        lock.lock()?;
        let existing = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        if existing.contains("# SAMPLE routing inventory") {
            return Ok(false);
        }
        let sample = include_str!("routing_sample.toml");
        let commented = sample
            .lines()
            .map(|line| {
                if line.trim().is_empty() {
                    "#".into()
                } else {
                    format!("# {}", line.trim_end())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(format!("\n\n{commented}\n").as_bytes())?;
        Ok(true)
    })();
    match result {
        Ok(true) => {
            println!(
                "routing sample appended (commented out) to {}",
                path.display()
            );
            0
        }
        Ok(false) => {
            println!(
                "routing sample already present in {}; nothing to do.",
                path.display()
            );
            0
        }
        Err(e) => {
            eprintln!("error: cannot update {}: {e}", path.display());
            1
        }
    }
}

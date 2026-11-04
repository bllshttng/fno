//! PreToolUse join partition guard: judge one Edit/Write payload against a
//! joined worktree's per-band policy. `hooks/join-partition-write-guard.sh`
//! runs this only after its stat fast path saw `<cwd>/.fno/join-partition/`.
//! Prints one decision and always exits 0: a crash must read as "not jailed",
//! never as a wedge.

use serde_json::Value;
use std::path::{Component, Path, PathBuf};

const PATCH_PREFIXES: [&str; 4] = [
    "*** Add File: ",
    "*** Update File: ",
    "*** Delete File: ",
    "*** Move to: ",
];

pub fn run(_args: &[String]) -> i32 {
    match judge(&super::read_stdin()) {
        Some(reason) => super::emit_block(&reason),
        None => super::emit_allow(),
    }
}

fn targets(input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(path) = input
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
    {
        out.push(path.to_string());
    }
    let command = input.get("command").and_then(Value::as_str).unwrap_or("");
    for line in command.lines() {
        for prefix in PATCH_PREFIXES {
            if let Some(rest) = line.strip_prefix(prefix) {
                let target = rest.trim().trim_end_matches('\r');
                if !target.is_empty() {
                    out.push(target.to_string());
                }
            }
        }
    }
    out
}

/// `FNO_WORKER_NAME`, then the roster binding `fno whoami` prints, cached per
/// session: the lookup walks the process tree, so a joined worktree pays it
/// once. A `-` entry caches "unresolvable"; identity trouble is never a jail.
fn worker_name(policy_dir: &Path, session: &str) -> String {
    if let Some(name) = std::env::var("FNO_WORKER_NAME")
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
    {
        return name;
    }
    let cache = (!session.is_empty()).then(|| policy_dir.join(format!(".session-{session}")));
    if let Some(cached) = cache.as_ref().and_then(|c| std::fs::read_to_string(c).ok()) {
        let cached = cached.trim();
        return if cached == "-" {
            String::new()
        } else {
            cached.to_string()
        };
    }
    let mut cmd = std::process::Command::new("fno");
    cmd.arg("whoami");
    let name = crate::bounded_cmd::output_with_timeout(cmd, 5)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .find_map(|l| {
                    l.strip_prefix("agent:")
                        .filter(|_| l.contains("(registry)"))
                        .map(|r| r.replace("(registry)", "").trim().to_string())
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    if let Some(cache) = cache {
        let _ = std::fs::write(cache, if name.is_empty() { "-" } else { &name });
    }
    name
}

/// Physical path without requiring it to exist: canonicalize the longest
/// existing ancestor, then append the rest lexically.
fn resolve(path: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(real) = probe.canonicalize() {
            let mut out = real;
            for part in tail.iter().rev() {
                match Path::new(part).components().next() {
                    Some(Component::ParentDir) => {
                        out.pop();
                    }
                    Some(Component::CurDir) => {}
                    _ => out.push(part),
                }
            }
            return Some(out);
        }
        tail.push(probe.file_name()?.to_os_string());
        if !probe.pop() {
            return None;
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub(crate) fn judge(payload: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(payload).ok()?;
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())?;
    let policy_dir = Path::new(cwd).join(".fno/join-partition");
    // Re-checked: the shell stat may have raced a join teardown.
    if !policy_dir.is_dir() {
        return None;
    }
    let targets = targets(payload.get("tool_input").unwrap_or(&Value::Null));
    if targets.is_empty() {
        return None;
    }
    let session = payload
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let name = worker_name(&policy_dir, session);
    // No policy for this name: the holder or an unattributed session, never
    // jailed at this layer.
    let policy = (!name.is_empty())
        .then(|| read_json(&policy_dir.join(format!("{name}.json"))))
        .flatten()
        .filter(Value::is_object)?;
    let deny: Vec<&str> = policy
        .get("deny_edit")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if deny.is_empty() {
        return None;
    }
    let band = policy
        .get("band")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty())
        .unwrap_or("your band");
    let base = Path::new(cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(cwd));
    let owner = |rel: &str| -> String {
        let mut siblings: Vec<PathBuf> = std::fs::read_dir(&policy_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "json")
                    && p.file_name().is_some_and(|f| *f != *format!("{name}.json"))
            })
            .collect();
        siblings.sort();
        siblings
            .iter()
            .filter_map(|p| read_json(p))
            .find(|other| {
                other
                    .get("allow_write")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(rel)))
            })
            .map(|other| match other.get("band") {
                Some(Value::String(b)) if !b.is_empty() => b.clone(),
                _ => "another".into(),
            })
            .unwrap_or_else(|| "another".into())
    };
    for raw in &targets {
        let given = Path::new(raw);
        let joined = if given.is_absolute() {
            given.to_path_buf()
        } else {
            base.join(given)
        };
        // A target that will not resolve physically is unknown, not safe.
        let Some(phys) = resolve(&joined) else {
            return Some(reason(raw, band, band));
        };
        // Outside the worktree is no band's surface: the OS layer owns it.
        let Ok(rel) = phys.strip_prefix(&base) else {
            continue;
        };
        let rel = rel.to_string_lossy();
        let hit = deny.iter().any(|d| {
            let d = d.trim_end_matches('/');
            rel == d || rel.starts_with(&format!("{d}/"))
        });
        if hit {
            return Some(reason(&rel, &owner(&rel), band));
        }
    }
    None
}

fn reason(rel: &str, owner: &str, band: &str) -> String {
    format!("join partition: '{rel}' belongs to band '{owner}', not your band '{band}'. Edit only your own band's files; a Bash write there is refused by the sandbox too.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_worker_is_jailed_to_its_band_and_the_holder_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let policies = cwd.join(".fno/join-partition");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(
            policies.join("w-low.json"),
            json!({"band": "low", "deny_edit": ["src/"]}).to_string(),
        )
        .unwrap();
        std::fs::write(
            policies.join("w-high.json"),
            json!({"band": "high", "allow_write": ["src/a.rs"]}).to_string(),
        )
        .unwrap();
        let payload =
            |input: Value| json!({"cwd": cwd, "session_id": "s1", "tool_input": input}).to_string();
        std::env::set_var("FNO_WORKER_NAME", "w-low");
        let refused = judge(&payload(json!({"file_path": "src/a.rs"}))).unwrap();
        assert!(
            refused.contains("'src/a.rs' belongs to band 'high', not your band 'low'"),
            "{refused}"
        );
        let patch = json!({"command": "*** Begin Patch\n*** Add File: src/new.rs\n"});
        assert!(judge(&payload(patch)).unwrap().contains("band 'another'"));
        assert_eq!(judge(&payload(json!({"file_path": "docs/x.md"}))), None);
        std::env::set_var("FNO_WORKER_NAME", "holder");
        assert_eq!(judge(&payload(json!({"file_path": "src/a.rs"}))), None);
        std::env::remove_var("FNO_WORKER_NAME");
        assert_eq!(judge("not json"), None);
    }
}

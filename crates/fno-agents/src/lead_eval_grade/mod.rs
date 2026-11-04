//! Grades a written lead eval. Readers (headless one-shot model calls) read
//! the window digests `lead_eval` wrote under `data/out/`, then one grader
//! turns their reports into parts 3 to 5 and a PENDING-NODES file. Only a
//! hand-off runs this (`intel --write --read`); the daemon arm keeps the
//! mechanical fold.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const READER_PROMPT: &str = include_str!("reader.md");
const GRADER_PROMPT: &str = include_str!("grader.md");
const DEFAULT_MODEL: &str = "opus";
const CALL_TIMEOUT_S: u64 = 1800;
/// Digest bytes one reader takes: about 150k tokens, which leaves room for
/// its report inside a 200k window. A run over the budget gets more readers.
const READER_BYTE_BUDGET: u64 = 600_000;
const SECTIONS: [&str; 4] = [
    "part3-failures.md",
    "part4-reforms.md",
    "part5-leadership.md",
    "PENDING-NODES.md",
];

/// One model call: prompt, working dir, timeout seconds, model.
pub(crate) type Spawn<'a> =
    &'a (dyn Fn(&str, &Path, u64, &str) -> Result<(i32, String, String), String> + Sync);

/// Grade the eval in `dir`. A failure leaves parts 3 and 4 pending and stamps
/// the index `graded_by: failed: <reason>`.
pub(crate) fn grade(dir: &Path, value: &Value, cwd: &Path) -> Result<(), String> {
    let model = crate::agents_config::config_lookup(cwd, &["lead_eval", "reader_model"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    grade_with(dir, value, &model, &spawn)
}

fn spawn(
    prompt: &str,
    cwd: &Path,
    timeout_secs: u64,
    model: &str,
) -> Result<(i32, String, String), String> {
    crate::blueprint_judge::headless_spawn(
        &spawn_argv(prompt, cwd, timeout_secs, model),
        timeout_secs,
    )
}

/// The judge's `--once` shape with one change: the reader keeps the Read
/// tool, because a digest is far too large for argv. The working directory
/// is the eval dir, so Read needs no extra directory grant. No `--name`: the
/// spawn names the one-shot from the prompt.
fn spawn_argv(prompt: &str, cwd: &Path, timeout_secs: u64, model: &str) -> Vec<String> {
    [
        "agents",
        "spawn",
        prompt,
        "--harness",
        "claude",
        "--once",
        "--cwd",
        &cwd.display().to_string(),
        "--timeout",
        &timeout_secs.to_string(),
        "--",
        "--model",
        model,
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--tools",
        "Read",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn grade_with(dir: &Path, value: &Value, model: &str, spawn: Spawn) -> Result<(), String> {
    match run_grade(dir, value, model, spawn) {
        Ok(()) => Ok(()),
        Err(reason) => {
            set_graded_by(dir, &format!("failed: {}", reason.replace('\n', " ")));
            Err(reason)
        }
    }
}

fn run_grade(dir: &Path, value: &Value, model: &str, spawn: Spawn) -> Result<(), String> {
    let digests = digest_files(dir)?;
    let groups = split_windows(&digests);
    let reports = std::thread::scope(|scope| {
        let handles: Vec<_> = groups
            .iter()
            .enumerate()
            .map(|(i, group)| {
                let label = char::from(b'A' + i as u8);
                let total = groups.len();
                scope.spawn(move || read_group(dir, label, total, group, model, spawn))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("a reader thread panicked".to_string()))
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    let reply = grader_reply(dir, value, model, spawn)?;
    let sections = parse_sections(&reply)?;
    let sources = std::iter::once("data/fold.json".to_string())
        .chain(
            reports
                .iter()
                .map(|label| format!("data/out/reader-{label}.md")),
        )
        .collect::<Vec<_>>()
        .join(", ");
    write_graded(dir, value, model, &sources, &sections)
}

/// Each `data/out/window-NN.md` with its size, in window order.
fn digest_files(dir: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
    let out = dir.join("data/out");
    let entries = std::fs::read_dir(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut files: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("window-") && name.ends_with(".md")
        })
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.len())))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no window digests under {}", out.display()));
    }
    Ok(files)
}

/// Contiguous groups balanced by digest bytes: two readers, more when the
/// run passes the per-reader budget, one for a one-window run.
fn split_windows(files: &[(PathBuf, u64)]) -> Vec<Vec<PathBuf>> {
    let total: u64 = files.iter().map(|(_, n)| n).sum();
    let wanted = if files.len() <= 1 {
        1
    } else {
        (total.div_ceil(READER_BYTE_BUDGET) as usize).max(2)
    };
    let count = wanted.min(files.len()).min(26);
    let mut groups: Vec<Vec<PathBuf>> = vec![Vec::new(); count];
    let mut before = 0u64;
    for (path, size) in files {
        let middle = before + size / 2;
        let at = if total == 0 {
            0
        } else {
            ((middle as u128 * count as u128) / total as u128) as usize
        };
        groups[at.min(count - 1)].push(path.clone());
        before += size;
    }
    groups.retain(|g| !g.is_empty());
    groups
}

fn read_group(
    dir: &Path,
    label: char,
    total: usize,
    group: &[PathBuf],
    model: &str,
    spawn: Spawn,
) -> Result<char, String> {
    let names: Vec<String> = group
        .iter()
        .filter_map(|p| {
            p.file_name()
                .map(|n| format!("data/out/{}", n.to_string_lossy()))
        })
        .collect();
    let prompt = format!(
        "Retro reader {label} of {total}.\n\n{READER_PROMPT}\n\nYour window files: {}.\n",
        names.join(", ")
    );
    let report = one_call(&prompt, dir, model, spawn, &format!("reader {label}"))?;
    let path = dir.join(format!("data/out/reader-{label}.md"));
    std::fs::write(&path, format!("{report}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(label)
}

fn grader_reply(dir: &Path, value: &Value, model: &str, spawn: Spawn) -> Result<String, String> {
    let keys = serde_json::json!({
        "harness": value["harness"],
        "windows": value["windows"].as_array().map_or(0, Vec::len),
        "totals": value["totals"],
        "bound_error_rates": value["bound_error_rates"],
        "repeated_asks": value["repeated_asks"],
        "repeated_refusals": value["repeated_refusals"],
        "freeze_merges": value["freeze_merges"],
        "user_corrections": value["user_corrections"],
    });
    let prompt = format!("{GRADER_PROMPT}\n\nThe fold's key numbers:\n\n{keys}\n");
    one_call(&prompt, dir, model, spawn, "grader")
}

/// One call, its stdout as the answer. A non-zero exit, a timeout or an
/// empty reply is an error that names the call.
fn one_call(
    prompt: &str,
    dir: &Path,
    model: &str,
    spawn: Spawn,
    who: &str,
) -> Result<String, String> {
    let (code, out, err) =
        spawn(prompt, dir, CALL_TIMEOUT_S, model).map_err(|e| format!("{who}: {e}"))?;
    if code != 0 {
        let tail: String = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        return Err(format!("{who} exited {code}: {tail}"));
    }
    let out = out.trim();
    if out.is_empty() {
        return Err(format!("{who} answered nothing"));
    }
    Ok(out.to_string())
}

/// The grader's four files, each opened by a `=== <name> ===` line. A
/// missing or empty section fails the grade.
fn parse_sections(reply: &str) -> Result<BTreeMap<String, String>, String> {
    let mut sections: BTreeMap<String, String> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in reply.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed
            .strip_prefix("=== ")
            .and_then(|rest| rest.strip_suffix(" ==="))
        {
            current = Some(name.trim().to_string());
            sections.entry(name.trim().to_string()).or_default();
        } else if let Some(name) = &current {
            let body = sections.entry(name.clone()).or_default();
            body.push_str(line);
            body.push('\n');
        }
    }
    for name in SECTIONS {
        if sections.get(name).is_none_or(|body| body.trim().is_empty()) {
            return Err(format!("the grader reply has no {name} section"));
        }
    }
    Ok(sections)
}

fn head_is_pending(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.get(..64).unwrap_or(&text).contains("status: pending"))
        .unwrap_or(false)
}

fn stamped(model: &str, sources: &str, body: &str) -> String {
    format!(
        "---\nstatus: graded\ngraded_by: {model} (lead_eval_grade)\nsources: {sources}\n---\n\n{}\n",
        body.trim()
    )
}

/// The stub's mechanical tables, kept under the graded text so the numbers
/// the grader read stay in the file.
fn mechanical_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    match text.find("## Repeated refusals") {
        Some(at) => format!("\n\n## Mechanical tables\n\n{}", text[at..].trim_end()),
        None => String::new(),
    }
}

fn write_graded(
    dir: &Path,
    value: &Value,
    model: &str,
    sources: &str,
    sections: &BTreeMap<String, String>,
) -> Result<(), String> {
    let write = |path: &Path, text: &str| {
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    // A hand-graded part is never touched: parts 3 and 4 give way only while
    // they still read pending; part 5 and the pending nodes only when absent.
    let part3 = dir.join("part3-failures.md");
    if head_is_pending(&part3) {
        let body = format!(
            "{}{}",
            sections["part3-failures.md"].trim(),
            mechanical_tail(&part3)
        );
        write(&part3, &stamped(model, sources, &body))?;
    }
    let part4 = dir.join("part4-reforms.md");
    if head_is_pending(&part4) {
        write(
            &part4,
            &stamped(model, sources, &sections["part4-reforms.md"]),
        )?;
    }
    let part5 = dir.join("part5-leadership.md");
    if !part5.exists() {
        write(
            &part5,
            &stamped(model, sources, &sections["part5-leadership.md"]),
        )?;
    }
    let pending = pending_nodes_path(dir, value);
    if let Some(path) = pending.as_ref().filter(|p| !p.exists()) {
        write(path, &format!("{}\n", sections["PENDING-NODES.md"].trim()))?;
    }
    set_graded_by(dir, &format!("{model} (lead_eval_grade)"));
    link_index(dir, pending.as_deref());
    Ok(())
}

/// `<leads>/PENDING-NODES-from-<sid8>-retro.md`, beside the eval dirs.
fn pending_nodes_path(dir: &Path, value: &Value) -> Option<PathBuf> {
    let session = value["session"].as_str()?;
    Some(dir.parent()?.join(format!(
        "PENDING-NODES-from-{}-retro.md",
        &session[..session.len().min(8)]
    )))
}

fn index_path(dir: &Path) -> Option<PathBuf> {
    let base = dir.file_name()?.to_str()?;
    Some(dir.with_file_name(format!("{base}.md")))
}

/// Rewrite the index's `graded_by:` line. No index, no line: nothing to say.
fn set_graded_by(dir: &Path, text: &str) {
    let Some(index) = index_path(dir) else { return };
    let Ok(raw) = std::fs::read_to_string(&index) else {
        return;
    };
    let mut replaced = false;
    let lines: Vec<String> = raw
        .lines()
        .map(|line| {
            // Only this module's own stamps give way: a hand-graded index
            // keeps its `graded_by`.
            let ours = line.strip_prefix("graded_by: ").is_some_and(|v| {
                v == "pending" || v.starts_with("failed:") || v.contains("(lead_eval_grade)")
            });
            if !replaced && ours {
                replaced = true;
                format!("graded_by: {text}")
            } else {
                line.to_string()
            }
        })
        .collect();
    if replaced {
        let _ = std::fs::write(&index, format!("{}\n", lines.join("\n")));
    }
}

/// Add the Part 5 and pending-nodes links to the index once.
fn link_index(dir: &Path, pending: Option<&Path>) {
    let (Some(index), Some(base)) = (index_path(dir), dir.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let Ok(mut raw) = std::fs::read_to_string(&index) else {
        return;
    };
    let part5 = format!("- [Part 5: leadership]({base}/part5-leadership.md)\n");
    if !raw.contains(&part5) {
        if !raw.ends_with('\n') {
            raw.push('\n');
        }
        raw.push_str(&part5);
    }
    if let Some(name) = pending.and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        let line = format!("- [Pending nodes]({name})\n");
        if !raw.contains(&line) {
            raw.push_str(&line);
        }
    }
    let _ = std::fs::write(&index, raw);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    const REPLY: &str = "=== part3-failures.md ===\nCause: stale claims (window 1, 10:00 UTC)\n=== part4-reforms.md ===\nReform one\n=== part5-leadership.md ===\nWait for CI\n=== PENDING-NODES.md ===\n# Pending\n- [ ] Reform one - skills/lead - (window 1)\n";

    fn eval_dir(root: &Path) -> PathBuf {
        let dir = root.join("leads/lead-fno-abcdef12");
        std::fs::create_dir_all(dir.join("data/out")).unwrap();
        std::fs::write(
            dir.join("data/out/window-01.md"),
            "# Window 1\n\n### USER t\n\nhi\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("data/out/window-02.md"),
            "# Window 2\n\n### LEAD t\n\nok\n",
        )
        .unwrap();
        std::fs::write(dir.join("part3-failures.md"), "---\nstatus: pending\n---\n\n# Part 3: failures\n\n## Repeated refusals\n\n| a | 2 |\n").unwrap();
        std::fs::write(
            dir.join("part4-reforms.md"),
            "---\nstatus: pending\n---\n\n# Part 4: reforms\n",
        )
        .unwrap();
        std::fs::write(
            root.join("leads/lead-fno-abcdef12.md"),
            "---\ngraded_by: pending\n---\n\n# Lead eval\n",
        )
        .unwrap();
        dir
    }

    fn spawn_with(
        reply: &'static str,
    ) -> impl Fn(&str, &Path, u64, &str) -> Result<(i32, String, String), String> + Sync {
        let calls = Mutex::new(0usize);
        move |prompt: &str, _cwd: &Path, _t: u64, _m: &str| {
            *calls.lock().unwrap() += 1;
            let out = if prompt.starts_with("Retro grader") {
                reply.to_string()
            } else {
                format!("report for: {}", prompt.lines().next().unwrap_or(""))
            };
            Ok((0, out, String::new()))
        }
    }

    #[test]
    fn grade_writes_graded_parts_and_never_clobbers_a_graded_one() {
        let root = tempfile::tempdir().unwrap();
        let dir = eval_dir(root.path());
        let value = json!({"session": "abcdef1234567890", "totals": {}});
        let fake = spawn_with(REPLY);
        grade_with(&dir, &value, "opus", &fake).unwrap();
        for part in [
            "part3-failures.md",
            "part4-reforms.md",
            "part5-leadership.md",
        ] {
            let text = std::fs::read_to_string(dir.join(part)).unwrap();
            assert!(text.contains("status: graded"), "{part}: {text}");
        }
        assert!(std::fs::read_to_string(dir.join("part3-failures.md"))
            .unwrap()
            .contains("## Mechanical tables"));
        let pending = root
            .path()
            .join("leads/PENDING-NODES-from-abcdef12-retro.md");
        assert!(std::fs::read_to_string(&pending)
            .unwrap()
            .contains("- [ ] Reform one"));
        let index =
            std::fs::read_to_string(root.path().join("leads/lead-fno-abcdef12.md")).unwrap();
        assert!(
            index.contains("graded_by: opus (lead_eval_grade)"),
            "{index}"
        );
        assert!(
            index.contains("part5-leadership.md")
                && index.contains("PENDING-NODES-from-abcdef12-retro.md"),
            "{index}"
        );

        // A part 3 the user graded by hand stays byte-identical.
        let hand = "---\nstatus: graded\n---\n\nby hand\n";
        std::fs::write(dir.join("part3-failures.md"), hand).unwrap();
        grade_with(&dir, &value, "opus", &fake).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("part3-failures.md")).unwrap(),
            hand
        );
    }

    #[test]
    fn a_reply_missing_a_section_leaves_parts_pending_and_marks_the_index_failed() {
        let root = tempfile::tempdir().unwrap();
        let dir = eval_dir(root.path());
        let value = json!({"session": "abcdef1234567890"});
        let fake = spawn_with("=== part3-failures.md ===\nonly this\n");
        let err = grade_with(&dir, &value, "opus", &fake).unwrap_err();
        assert!(err.contains("part4-reforms.md"), "{err}");
        assert!(std::fs::read_to_string(dir.join("part3-failures.md"))
            .unwrap()
            .contains("status: pending"));
        let index =
            std::fs::read_to_string(root.path().join("leads/lead-fno-abcdef12.md")).unwrap();
        assert!(index.contains("graded_by: failed: "), "{index}");
    }
}

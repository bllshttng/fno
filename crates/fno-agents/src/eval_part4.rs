//! Which of a repo's lead evals still owe their Part 4 reforms: the
//! question the check-in beat answers so a successor files what a
//! predecessor left open.

use std::path::{Path, PathBuf};

/// The repo's unfilled eval part4 files: `status: pending` under
/// `<plans>/../evals/leads/*/part4-reforms.md`, minus an auto-filed twin of a
/// graded eval, plus each `PENDING-NODES-from-*-retro.md` with an open box.
/// The eval writer stamps
/// pending and the lead flips the status when the reforms are filed, so a
/// beat can name what a predecessor left open. Best effort: an unreadable
/// eval tree names nothing.
pub fn unfilled_part4s(cwd: &Path) -> Vec<PathBuf> {
    let Some(plans) = crate::plans_path::plans_content_dir(cwd) else {
        return Vec::new();
    };
    let leads = plans.join("..").join("evals").join("leads");
    let Ok(entries) = std::fs::read_dir(&leads) else {
        return Vec::new();
    };
    // (dir name, part4 path, pending) for every eval dir that has a part 4.
    let mut parts = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("part4-reforms.md");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // get, not slice: byte 64 can land inside a multibyte character,
        // and a panic here would take the whole check-in beat down.
        let head = text.get(..64).unwrap_or(&text);
        let name = entry.file_name().to_string_lossy().into_owned();
        parts.push((name, path, head.contains("status: pending")));
    }
    let mut unfilled = Vec::new();
    for (name, path, pending) in &parts {
        if !pending {
            continue;
        }
        // An auto-filed twin of a hand-graded eval shares the session handle:
        // the graded sibling already filed its reforms.
        let handle = session_handle(name);
        let graded_twin = handle.is_some()
            && parts
                .iter()
                .any(|(n, _, p)| !p && n != name && session_handle(n) == handle);
        if !graded_twin {
            unfilled.push(path.clone());
        }
    }
    // A PENDING-NODES file is the retro's list of reforms with no node yet.
    if let Ok(files) = std::fs::read_dir(&leads) {
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("PENDING-NODES-from-") && name.ends_with("-retro.md")) {
                continue;
            }
            let open = std::fs::read_to_string(file.path())
                .map(|t| t.lines().any(|l| l.trim_start().starts_with("- [ ]")))
                .unwrap_or(false);
            if open {
                unfilled.push(file.path());
            }
        }
    }
    unfilled.sort();
    unfilled
}

/// The check-in line for one path `unfilled_part4s` returned. A PENDING-NODES
/// file is a list of reforms with no node, so it reads `open reforms`.
pub fn beat_line(path: &Path) -> String {
    let pending_nodes = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("PENDING-NODES"));
    let label = if pending_nodes {
        "open reforms"
    } else {
        "unfilled part4"
    };
    format!("{label}: {}", path.display())
}

/// The trailing 8-hex session handle of an eval dir name, when it has one.
fn session_handle(dir_name: &str) -> Option<&str> {
    let tail = dir_name.rsplit('-').next()?;
    (tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_hexdigit())).then_some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfilled_part4s_names_pending_and_skips_done() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("proj");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        let plans = base.path().join("plans");
        std::fs::write(
            repo.join(".claude/settings.local.json"),
            format!(r#"{{"plansDirectory": "{}"}}"#, plans.display()),
        )
        .unwrap();
        let leads = plans.join("..").join("evals").join("leads");
        std::fs::create_dir_all(leads.join("lead-a-11111111")).unwrap();
        std::fs::create_dir_all(leads.join("lead-b-22222222")).unwrap();
        std::fs::create_dir_all(leads.join("lead-c-33333333")).unwrap();
        std::fs::write(
            leads.join("lead-a-11111111/part4-reforms.md"),
            "---\nstatus: pending\n---\n\n# Part 4: reforms\n",
        )
        .unwrap();
        std::fs::write(
            leads.join("lead-b-22222222/part4-reforms.md"),
            "---\nstatus: done\n---\n\n# Part 4: reforms\n",
        )
        .unwrap();
        // A multibyte character straddles byte 64 (27 header bytes + 36
        // ASCII): reading the head must not panic and must still find the
        // status line.
        let straddle = format!("---\nstatus: pending\n---\n\n# {}部after\n", "a".repeat(36));
        std::fs::write(leads.join("lead-c-33333333/part4-reforms.md"), straddle).unwrap();
        assert_eq!(
            unfilled_part4s(&repo),
            vec![
                leads.join("lead-a-11111111/part4-reforms.md"),
                leads.join("lead-c-33333333/part4-reforms.md"),
            ]
        );
    }

    #[test]
    fn open_reforms_skip_graded_twins_and_ticked_pending_nodes() {
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("proj");
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        let plans = base.path().join("plans");
        std::fs::write(
            repo.join(".claude/settings.local.json"),
            format!(r#"{{"plansDirectory": "{}"}}"#, plans.display()),
        )
        .unwrap();
        let leads = plans.join("..").join("evals").join("leads");
        for (dir, status) in [
            ("lead-fno-05dddd48", "pending"),
            ("lead-fno-vellum-05dddd48", "graded"),
        ] {
            std::fs::create_dir_all(leads.join(dir)).unwrap();
            std::fs::write(
                leads.join(dir).join("part4-reforms.md"),
                format!("---\nstatus: {status}\n---\n"),
            )
            .unwrap();
        }
        std::fs::write(
            leads.join("PENDING-NODES-from-05dddd48-retro.md"),
            "- [x] done\n- [ ] open\n",
        )
        .unwrap();
        std::fs::write(
            leads.join("PENDING-NODES-from-99999999-retro.md"),
            "- [x] done\n",
        )
        .unwrap();
        assert_eq!(
            unfilled_part4s(&repo),
            vec![leads.join("PENDING-NODES-from-05dddd48-retro.md")]
        );
    }
}

//! The graduate command: retag a capability task to regression, in place,
//! line-level so comments survive (the deleted Python leg's
//! report.py::graduate_task_file, ported). The goldens pin the bytes.
//! `--demote` adds the `demoted` tag with the same line-level rewrite.

use regex::Regex;

pub fn run(args: &[String]) -> i32 {
    let Some(task_id) = args.first().map(String::as_str) else {
        eprintln!("usage: fno-agents evals graduate <task-id> [--demote] [--bank <dir>]");
        return 2;
    };
    let bank_dir = super::resolve_bank_dir(super::flag_lookup(args, "--bank"));
    let tasks = match super::bank::discover_bank(&bank_dir) {
        Ok(tasks) => tasks,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };
    let Some(task) = tasks.iter().find(|t| t.id == task_id) else {
        let bank = bank_dir.display().to_string();
        eprintln!("Error: no bank task '{task_id}' in {bank}");
        return 1;
    };
    let demote = args.iter().any(|a| a == "--demote");
    let rewrite = if demote {
        demote_task_file(&task.source_path)
    } else {
        graduate_task_file(&task.source_path)
    };
    match rewrite {
        Ok(()) => {
            let source = task.source_path.display().to_string();
            if demote {
                println!("demoted '{task_id}' ({source})");
            } else {
                println!("graduated '{task_id}' -> regression ({source})");
                println!("the daemon opens this PR after its weekly capability run; with the daemon off, commit the edit yourself");
            }
            0
        }
        Err(text) => {
            eprintln!("Error: {text}");
            2
        }
    }
}

/// Rewrite tier: capability to regression in place; a line-level rewrite so
/// comments and formatting survive. The Python regex at seed semantics:
/// ^(\s*tier:\s*)capability(\s*(?:#.*)?)$ with (?m).
pub fn graduate_task_file(task_path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(task_path).map_err(|e| e.to_string())?;
    let new_text = graduate_text(&text).map_err(|e| format!("{}: {e}", task_path.display()))?;
    std::fs::write(task_path, new_text).map_err(|e| e.to_string())?;
    Ok(())
}

/// The same retag on the file's text, for a caller that writes elsewhere.
pub fn graduate_text(text: &str) -> Result<String, String> {
    let re =
        Regex::new(r"(?m)^([ \t]*tier:[ \t]*)capability([ \t]*(?:#.*)?)$").expect("static regex");
    if re.find_iter(text).count() == 0 {
        return Err("no `tier: capability` line to graduate (already regression, or non-standard formatting)".into());
    }
    Ok(re.replace_all(text, "${1}regression${2}").into_owned())
}

/// Append `demoted` to the flow-style `tags: [...]` line in place. A task
/// already demoted refuses by name and the file stays as it was.
pub fn demote_task_file(task_path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(task_path).map_err(|e| e.to_string())?;
    let shown = task_path.display().to_string();
    let re = Regex::new(r"(?m)^([ \t]*tags:[ \t]*\[)([^\]\n]*)(\][ \t]*(?:#.*)?)$")
        .expect("static regex");
    let Some(caps) = re.captures(&text) else {
        return Err(format!(
            "{shown}: no flow-style `tags: [...]` line to demote"
        ));
    };
    let tags: Vec<&str> = caps[2]
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if tags.contains(&super::bank::DEMOTED_TAG) {
        return Err(format!("{shown}: already demoted"));
    }
    let mut kept = tags.clone();
    kept.push(super::bank::DEMOTED_TAG);
    let line = format!("{}{}{}", &caps[1], kept.join(", "), &caps[3]);
    let range = caps.get(0).expect("whole match").range();
    let mut new_text = text.clone();
    new_text.replace_range(range, &line);
    std::fs::write(task_path, new_text).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rewrite_keeps_comments_and_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.yaml");
        std::fs::write(
            &p,
            "id: t\ntier: capability   # the reviewed retag target\ngrade: []\n",
        )
        .unwrap();
        graduate_task_file(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("tier: regression   # the reviewed retag target"),
            "{text}"
        );
    }

    #[test]
    fn an_already_regression_task_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.yaml");
        std::fs::write(&p, "id: t\ntier: regression\n").unwrap();
        let err = graduate_task_file(&p).unwrap_err();
        assert!(err.contains("no `tier: capability` line"), "{err}");
    }

    #[test]
    fn demote_appends_the_tag_once_and_refuses_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.yaml");
        std::fs::write(&p, "id: t\ntags: [golden, gate]   # why\ngrade: []\n").unwrap();
        demote_task_file(&p).unwrap();
        let once = std::fs::read_to_string(&p).unwrap();
        assert!(
            once.contains("tags: [golden, gate, demoted]   # why"),
            "{once}"
        );
        let err = demote_task_file(&p).unwrap_err();
        assert!(err.contains("already demoted"), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
    }
}

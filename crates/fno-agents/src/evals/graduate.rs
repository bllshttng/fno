//! The graduate command: retag a capability task to regression, in place,
//! line-level so comments survive (the deleted Python leg's
//! report.py::graduate_task_file, ported). The goldens pin the bytes.

use regex::Regex;

const USAGE_EXIT: i32 = 2;

pub fn run(args: &[String]) -> i32 {
    let Some(task_id) = args.first().map(String::as_str) else {
        eprintln!("usage: fno-agents evals graduate <task-id> [--bank <dir>]");
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
    match graduate_task_file(&task.source_path) {
        Ok(()) => {
            let source = task.source_path.display().to_string();
            println!("graduated '{task_id}' -> regression ({source})");
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
    let re =
        Regex::new(r"(?m)^([ \t]*tier:[ \t]*)capability([ \t]*(?:#.*)?)$").expect("static regex");
    let count = re.find_iter(&text).count();
    if count == 0 {
        let shown = task_path.display().to_string();
        return Err(format!("{shown}: no `tier: capability` line to graduate (already regression, or non-standard formatting)"));
    }
    let new_text = re.replace_all(&text, "${1}regression${2}").into_owned();
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
}

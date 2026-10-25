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
            eprintln!("Error: {e} BankError display");
            return 1;
        }
    };
    let Some(task) = tasks.iter().find(|t| t.id == task_id) else {
        eprintln!("Error: no bank task '{task_id}' in {}", bank_dir.display());
        return 1;
    };
    match graduate_task_file(&task.source_path) {
        Ok(()) => {
            println!(
                "graduated '{task_id}' -> regression ({})",
                task.source_path.display()
            );
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
        return Err(format!("{0}: no `tier: capability` line to graduate (already regression, or non-standard formatting)", task_path.display()));
    }
    let new_text = re.replace_all(&text, "${1}regression${2}").into_owned();
    std::fs::write(task_path, new_text).map_err(|e| e.to_string())?;
    Ok(())
}

//! The user surfaces of the lead read commands: `fno agents org chart
//! manifest-path`, `fno agents org faq list|add`, the caller ladder behind
//! `fno agents org chart shape`, and the `fno inbox board` text page.
//!
//! The front door (`crates/fno`) execs these through arguments of existing
//! actions, because law d-fe66560a bars a new one: `lead-state
//! --manifest-path`, `lead-state --faq`, `lead-shape --user-command` and
//! `board --user-command`. The words each one prints are the retired Python
//! commands' words, frozen in `tests/golden/lead_reads/`.

use serde_json::Value;
use std::path::{Path, PathBuf};

fn registry_path() -> PathBuf {
    crate::paths::AgentsHome::from_env().registry_json()
}

/// Python's `str()` of a JSON scalar inside an f-string.
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(other) => other.to_string(),
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ---- manifest-path --------------------------------------------------------

/// This live session's promoted manifest, or why there is none. The row, not
/// file presence, proves authority. Each refusal names WHICH reading failed.
pub(crate) fn resolve_manifest_path(
    session: &str,
    harness: Option<&str>,
    state_root: Option<&Path>,
    registry: &Path,
) -> Result<PathBuf, String> {
    if session.is_empty() {
        return Err("no harness session id was given".to_string());
    }
    let rows = crate::state::load_registry(registry)
        .map_err(|e| format!("the agent registry could not be read: {e}"))?;
    let row =
        crate::lead_state::find_by_session(&rows.entries, session, harness).ok_or_else(|| {
            match harness {
                Some(h) => format!("no registry row names session {session} under harness {h}"),
                None => format!("no registry row names session {session}"),
            }
        })?;
    if crate::lead_state::is_terminal(row) {
        let status = py_str(serde_json::to_value(&row.status).ok().as_ref());
        return Err(format!(
            "the registry row for {session} is {status}, a terminal state"
        ));
    }
    let scope = row
        .role_scope
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "the registry row for {session} carries no role_scope: the row is \
                 unstamped, so no role proves this session's authority"
            )
        })?;
    // The read keys on the role row's cwd, the same key the writer arms under.
    let root = match state_root {
        Some(root) => root.to_path_buf(),
        None => owner_space(&row.cwd),
    };
    let path = crate::lead_state::manifest_path(&root, scope)?;
    if !path.is_file() {
        let mut reason = format!(
            "role scope {scope} is stamped but no manifest exists at {}.",
            path.display()
        );
        if state_root.is_some() {
            reason.push_str(" Manifests live under the space dir; omit --state-root to take it.");
        }
        return Err(reason);
    }
    Ok(path)
}

/// The space a role row's cwd keys; a row with no cwd keys the process cwd.
fn owner_space(cwd: &str) -> PathBuf {
    let cwd = if cwd.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(cwd)
    };
    crate::paths::space_dir(&cwd)
}

/// `lead-state --manifest-path --harness-session-id ID [--harness H]
/// [--state-root DIR]`: print the path (rc 0), or the reason on stderr (rc 1).
pub fn run_manifest_path(args: &[String]) -> i32 {
    let mut session: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut state_root: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match (args[i].as_str(), args.get(i + 1)) {
            ("--harness-session-id", Some(v)) => session = Some(v.clone()),
            ("--harness", Some(v)) => harness = Some(v.clone()).filter(|h| !h.is_empty()),
            ("--state-root", Some(v)) => state_root = Some(PathBuf::from(v)),
            (other, _) => {
                eprintln!("fno-agents lead-state --manifest-path: unknown flag {other}");
                eprintln!(
                    "fno-agents lead-state --manifest-path: --harness-session-id ID \
                     [--harness H] [--state-root DIR]"
                );
                return 2;
            }
        }
        i += 2;
    }
    let Some(session) = session else {
        eprintln!("fno-agents lead-state --manifest-path: --harness-session-id is required");
        return 2;
    };
    match resolve_manifest_path(
        &session,
        harness.as_deref(),
        state_root.as_deref(),
        &registry_path(),
    ) {
        Ok(path) => {
            println!("{}", path.display());
            0
        }
        Err(reason) => {
            eprintln!("lead manifest-path: {reason}");
            1
        }
    }
}

/// The caller's own manifest, or None: an unreadable, terminal or unpromoted
/// reading resolves nothing.
fn caller_manifest() -> Option<PathBuf> {
    let (session, harness) = crate::claims::resolve_identity();
    let session = session.filter(|s| !s.is_empty())?;
    resolve_manifest_path(&session, harness.as_deref(), None, &registry_path()).ok()
}

// ---- faq ------------------------------------------------------------------

/// Entries the reinject hook re-teaches per role, so context cost stays flat.
const DEFAULT_MAX_FAQ_ENTRIES: usize = 20;

fn faqs_dir() -> Option<PathBuf> {
    crate::backlog::autobrief::briefs_dir().map(|d| d.join("lead-faqs"))
}

/// `lead-state --faq list --scope S [--max-entries N]` and `lead-state --faq
/// add --question Q --answer A --specimen S --exit E [--scope S]`.
pub fn run_faq(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => faq_list(&args[1..]),
        Some("add") => faq_add(&args[1..]),
        _ => {
            eprintln!("fno-agents lead-state --faq: list|add");
            2
        }
    }
}

fn flag_pairs(args: &[String], known: &[&str], sub: &str) -> Result<Vec<(String, String)>, i32> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args.get(i + 1) {
            Some(v) if known.contains(&args[i].as_str()) => {
                pairs.push((args[i].clone(), v.clone()))
            }
            _ => {
                eprintln!(
                    "fno-agents lead-state --faq {sub}: unknown flag {}",
                    args[i]
                );
                return Err(2);
            }
        }
        i += 2;
    }
    Ok(pairs)
}

fn flag<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn faq_list(args: &[String]) -> i32 {
    // `--paths` prints each entry's file, one per line: the post-compact hook
    // points the lead at the files instead of pasting their text.
    let paths_only = args.iter().any(|a| a == "--paths");
    let args: Vec<String> = args.iter().filter(|a| *a != "--paths").cloned().collect();
    let pairs = match flag_pairs(&args, &["--scope", "--max-entries"], "list") {
        Ok(p) => p,
        Err(code) => return code,
    };
    let Some(scope) = flag(&pairs, "--scope") else {
        eprintln!("fno-agents lead-state --faq list: --scope is required");
        return 2;
    };
    let max = match flag(&pairs, "--max-entries").map(str::parse::<usize>) {
        None => DEFAULT_MAX_FAQ_ENTRIES,
        Some(Ok(n)) => n,
        Some(Err(_)) => {
            eprintln!("fno-agents lead-state --faq list: --max-entries needs an integer");
            return 2;
        }
    };
    let entries = faqs_dir()
        .map(|dir| crate::lead_checkin::faq_files_for_scope(&dir, scope))
        .unwrap_or_default();
    // Python's `matched[-max:]`, where 0 keeps every entry.
    let skip = if max == 0 {
        0
    } else {
        entries.len().saturating_sub(max)
    };
    for (path, entry) in &entries[skip..] {
        if paths_only {
            println!("{}", path.display());
        } else {
            println!("{entry}");
            println!("---");
        }
    }
    0
}

fn faq_add(args: &[String]) -> i32 {
    let known = ["--question", "--answer", "--specimen", "--exit", "--scope"];
    let pairs = match flag_pairs(args, &known, "add") {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (Some(question), Some(answer), Some(specimen)) = (
        flag(&pairs, "--question"),
        flag(&pairs, "--answer"),
        flag(&pairs, "--specimen"),
    ) else {
        eprintln!(
            "fno-agents lead-state --faq add: --question, --answer and --specimen are required"
        );
        return 2;
    };
    let Some(exit) = flag(&pairs, "--exit").filter(|e| !e.trim().is_empty()) else {
        eprintln!("refused, no --exit: a workaround with no plan to stop needing it.");
        return 2;
    };
    let scope = match flag(&pairs, "--scope").filter(|s| !s.is_empty()) {
        Some(scope) => scope.to_string(),
        None => match crate::lead_verdict_inputs::resolve_scope(None, &registry_path()) {
            Ok(scope) => scope,
            Err(_) => {
                eprintln!("refused, no role held and no --scope given.");
                return 2;
            }
        },
    };
    let Some(dir) = faqs_dir() else {
        eprintln!("fno-agents lead-state --faq add: no state dir resolves for the faq directory");
        return 1;
    };
    let lead = crate::identity::ambient_agent_handle().unwrap_or_else(|| "unknown".into());
    let session = crate::claims::resolve_identity()
        .0
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let slug = Some(crate::naming::slug_component(Some(scope.as_str()), 24))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "faq".into());
    let path = dir.join(format!(
        "lead-{slug}-{}.md",
        crate::law_match::random_hex8()
    ));
    let created = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let body = format!(
        "---\ncreated: {created}\nlead: {lead}\nsession: {session}\nscope: {scope}\n---\n\n\
         # {question}\n\n## Answer\n\n{answer}\n\n## Specimen\n\n{specimen}\n\n## Exit\n\n{exit}\n"
    );
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, body)) {
        eprintln!(
            "fno-agents lead-state --faq add: cannot write {}: {e}",
            path.display()
        );
        return 1;
    }
    println!("{}", path.display());
    0
}

// ---- shape ----------------------------------------------------------------

/// The caller ladder a holder's own-term write needs: the caller's own role
/// scope, its session, and the space root its manifest lives under. A
/// foreign `--scope` refuses, because a holder declares only its own term.
pub(crate) fn own_role(scope: &str) -> Result<(String, String, PathBuf), String> {
    let (session, harness) = crate::claims::resolve_identity();
    let Some(session) = session.filter(|s| !s.is_empty()) else {
        return Err(
            "lead: an attended shell holds no role; the promoted session \
                    declares its own term from inside it."
                .to_string(),
        );
    };
    let unresolved = "lead: cannot resolve the caller's role: this session carries an agent \
                      identity the registry does not resolve to a row. Run /fno-me or retry.";
    let rows = crate::state::load_registry(&registry_path()).map_err(|_| unresolved.to_string())?;
    let row = crate::lead_state::find_by_session(&rows.entries, &session, harness.as_deref())
        .ok_or_else(|| unresolved.to_string())?;
    let own = row
        .role_scope
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "lead: this session holds no role, so there is no term to declare on.".to_string()
        })?;
    if !scope.trim().is_empty() && scope != own {
        return Err(format!(
            "lead: refusing to act on '{scope}': this session's role is '{own}', and a \
             holder declares only its own term."
        ));
    }
    let row_session = row
        .harness_session_id
        .clone()
        .or_else(|| row.cc_session_id.clone())
        .unwrap_or_default();
    let root = owner_space(&row.cwd);
    let root = match root.file_name().and_then(|n| n.to_str()) {
        Some(".fno") => root.parent().map(Path::to_path_buf).unwrap_or(root),
        _ => root,
    };
    Ok((own, row_session, root))
}

/// `lead-shape --user-command pass|team [--scope S]`: `fno agents org chart shape`.
pub fn run_shape_command(args: &[String]) -> i32 {
    let mut shape: Option<&str> = None;
    let mut scope = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--user-command" => i += 1,
            "--scope" if i + 1 < args.len() => {
                scope = args[i + 1].clone();
                i += 2;
            }
            word if shape.is_none() && !word.starts_with('-') => {
                shape = Some(word);
                i += 1;
            }
            other => {
                eprintln!("fno-agents lead-shape --user-command: unknown argument {other}");
                return 2;
            }
        }
    }
    if !matches!(shape, Some("pass" | "team")) {
        eprintln!("lead: shape must be 'pass' or 'team'.");
        return 2;
    }
    let (own, session, root) = match own_role(&scope) {
        Ok(found) => found,
        Err(refusal) => {
            eprintln!("{refusal}");
            return 2;
        }
    };
    let expect = Some(session.as_str()).filter(|s| !s.is_empty());
    match crate::lead_state::set_manifest_shape(&root, &own, shape.unwrap_or("pass"), expect) {
        Ok(declared) => {
            println!("lead: shape declared: {declared}");
            println!("scope:  {own}");
            0
        }
        Err(e) => {
            eprintln!("lead: {}", e.trim());
            1
        }
    }
}

// ---- board ----------------------------------------------------------------

/// Per-queue render cap for the human board view.
pub(crate) const DEFAULT_BOARD_ROWS: usize = 25;

/// The board's default scope at the user command: a lead reads its own board, so
/// the role it holds is the scope it means. Anything else reads fleet-wide.
pub(crate) fn board_default_state() -> Option<PathBuf> {
    caller_manifest()
}

/// The `fno inbox board` text page: `(stdout, stderr)`.
pub(crate) fn render_board(board: &Value, max_rows: usize) -> (String, String) {
    let mut out = format!("actionable: {}\n", py_str(board.get("actionable")));
    let empty = Vec::new();
    for q in board
        .get("queues")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        let name = py_str(q.get("name"));
        let error = py_str(q.get("error"));
        match q.get("status").and_then(Value::as_str) {
            Some("unreadable") => out.push_str(&format!("  {name:<20} UNREADABLE  {error}\n")),
            Some("over_budget") => out.push_str(&format!("  {name:<20} NOT READ    {error}\n")),
            _ => {
                let count = q.get("count");
                let mark = if truthy(q.get("actionable")) && truthy(count) {
                    "*"
                } else {
                    " "
                };
                let command = if truthy(q.get("verb")) {
                    format!("  -> {}", py_str(q.get("verb")))
                } else {
                    String::new()
                };
                let note = if truthy(q.get("note")) && truthy(count) {
                    format!("  ({})", py_str(q.get("note")))
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    " {mark}{name:<20} {}{command}{note}\n",
                    py_str(count)
                ));
                let rows = q.get("rows").and_then(Value::as_array).unwrap_or(&empty);
                for row in rows.iter().take(max_rows) {
                    out.push_str(&format!("      {}\n", py_str(Some(row))));
                }
                let hidden = rows.len().saturating_sub(max_rows);
                if hidden > 0 {
                    out.push_str(&format!("      ... {hidden} more not shown\n"));
                }
            }
        }
        out.push_str(&format!("      source: {}\n", py_str(q.get("source"))));
    }
    let mut err = String::new();
    for warning in board
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
    {
        err.push_str(&format!("warning: {}\n", py_str(Some(warning))));
    }
    (out, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The goldens were captured from the Python `_render` over this payload.
    #[test]
    fn the_board_page_matches_the_python_render() {
        let board: Value =
            serde_json::from_str(include_str!("../tests/fixtures/lead_reads/board.json")).unwrap();
        let golden = |key: &str| match key {
            "board_text_page.out" => include_str!("../tests/golden/lead_reads/board_text_page.out"),
            "board_text_page.err" => include_str!("../tests/golden/lead_reads/board_text_page.err"),
            "max.out" => {
                include_str!("../tests/golden/lead_reads/board_text_page_max_rows_one.out")
            }
            "max.err" => {
                include_str!("../tests/golden/lead_reads/board_text_page_max_rows_one.err")
            }
            _ => unreachable!("{key}"),
        };
        let page = render_board(&board, DEFAULT_BOARD_ROWS);
        assert_eq!(
            (page.0.as_str(), page.1.as_str()),
            (golden("board_text_page.out"), golden("board_text_page.err"))
        );
        let page = render_board(&board, 1);
        assert_eq!(
            (page.0.as_str(), page.1.as_str()),
            (golden("max.out"), golden("max.err"))
        );
    }
}

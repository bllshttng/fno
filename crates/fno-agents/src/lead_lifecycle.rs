//! The user surfaces of the lead lifecycle commands: `fno agents org init`, `cancel`
//! and `term`, riding `lead-state --init`, `--cancel` and `--term`. The words
//! each one prints are the retired Python commands' words, frozen in
//! `tests/golden/lead_lifecycle/`.

use serde_json::json;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

/// Run one `fno-agents` argv; a failure answers Python `call_binary_json`'s
/// error text: the trimmed stderr, capped at 200 characters.
fn call_self(argv: &[String]) -> Result<std::process::Output, String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno-agents"));
    let out = Command::new(exe)
        .args(argv)
        .output()
        .map_err(|e| e.to_string().chars().take(200).collect::<String>())?;
    if out.status.success() {
        return Ok(out);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = match stderr.trim() {
        "" => "command failed",
        t => t,
    };
    Err(text.chars().take(200).collect())
}

// ---- init -----------------------------------------------------------------

/// `lead-state --init --scope S... --harness-session-id ID [--max-iterations N]
/// [--respawn-ceiling N] [--force]`: write the scope's manifest, which the
/// lead loop arms read. Write-once; a successor needs --force only when the
/// predecessor died without stepping down.
pub fn run_init(args: &[String]) -> i32 {
    let mut scopes: Vec<String> = Vec::new();
    let mut session = String::new();
    let mut max_iterations: u64 = 40;
    let mut respawn_ceiling: u64 = 4;
    let mut force = false;
    let mut rebind_from: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1);
        match (args[i].as_str(), value) {
            ("--scope", Some(v)) => scopes.push(v.clone()),
            ("--harness-session-id", Some(v)) => session = v.clone(),
            ("--max-iterations", Some(v)) | ("--respawn-ceiling", Some(v)) => {
                let Ok(n) = v.parse::<u64>() else {
                    eprintln!("lead: {} needs a whole number, got '{v}'.", args[i]);
                    return 2;
                };
                if args[i] == "--max-iterations" {
                    max_iterations = n;
                } else {
                    respawn_ceiling = n;
                }
            }
            ("--force" | "-F", _) => {
                force = true;
                i += 1;
                continue;
            }
            ("--rebind-from", Some(v)) => rebind_from = Some(v.clone()),
            (other, _) => {
                eprintln!("fno-agents lead-state --init: unknown argument {other}");
                return 2;
            }
        }
        i += 2;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // The one chokepoint for `config.lead.enabled`: every arm reads the
    // manifest's existence, so gating the manifest gates them all.
    if !crate::lead_state::lead_loop_enabled(&cwd) {
        eprintln!(
            "lead: config.lead.enabled is false, so no lead is promoted. Enable it with \
             `fno config set lead.enabled true`."
        );
        return 3;
    }
    if session.trim().is_empty() {
        eprintln!(
            "lead: --harness-session-id is required. The stop hook gates the session the \
             manifest NAMES, so an unattributable manifest roles nobody and risks holding \
             unrelated sessions open."
        );
        return 2;
    }
    // Every spelling of one role must arm the SAME file, aliases resolved.
    let scope = crate::lead_verdict_inputs::canonical_members_with_aliases(&scopes.join(","), &cwd);
    if scope.is_empty() {
        eprintln!("lead: --scope needs a role scope: name an epic or a project.");
        return 2;
    }
    if let Some(old) = rebind_from.as_deref() {
        if force {
            eprintln!(
                "lead: --rebind-from and --force refuse each other: one moves the role, \
                 the other re-arms fresh."
            );
            return 2;
        }
        return crate::lead_rebind::run(&scope, &session, Some(old));
    }
    let admit = [
        "loop",
        "readiness",
        "--scope",
        &scope,
        "--session",
        &session,
        "--ensure-goal",
    ]
    .map(String::from);
    if let Err(error) = call_self(&admit) {
        eprintln!("lead: refusing role admission for '{scope}': {error}");
        return 2;
    }
    let manifest = match crate::lead_state::manifest_path(&crate::paths::space_dir(&cwd), &scope) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let spec: crate::lead_state::ManifestSpec = match serde_json::from_value(json!({
        "scope": scope, "harness_session_id": session, "max_iterations": max_iterations,
        "respawn_ceiling": respawn_ceiling, "shape": "pass",
    })) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("lead manifest payload: {e}");
            return 1;
        }
    };
    let fields = match crate::lead_state::write_lead_manifest(&manifest, &spec, force) {
        Ok(fields) => fields,
        Err(
            crate::lead_state::ManifestRefusal::Exists(m)
            | crate::lead_state::ManifestRefusal::Failed(m),
        ) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let _ = std::fs::remove_file(manifest.with_extension("cancelled"));
    let field = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    println!("lead: manifest written: {}", manifest.display());
    println!("fno_id: {}", field("fno_id"));
    println!("scope:  {}", field("scope"));
    print_settled_children(&scope, &cwd);
    warn_unpromoted_row(&scope, &cwd);
    0
}

/// The role scope's done or superseded children as titles, or nothing: a
/// role lands on an epic whose history predates the session, and every board
/// read filters to open rows. Nothing prints when the graph cannot be read.
fn print_settled_children(scope: &str, cwd: &std::path::Path) {
    let Ok(rows) = crate::graph_store::read_rows(&crate::paths_cli::graph_json(cwd)) else {
        return;
    };
    let text = |row: &serde_json::Value, key: &str| row[key].as_str().unwrap_or("").to_string();
    let mut lines = Vec::new();
    for member in scope.split(',') {
        for row in &rows {
            if row["parent"].as_str() != Some(member)
                || !matches!(row["status"].as_str(), Some("done" | "superseded"))
            {
                continue;
            }
            let title = Some(text(row, "title"))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| text(row, "id"));
            lines.push(format!("- {title} ({})", text(row, "id")));
        }
    }
    if lines.is_empty() {
        return;
    }
    println!();
    println!(
        "Settled findings under this role (done or superseded children; read before the \
         first check-in, never re-derive):"
    );
    for line in lines {
        println!("{line}");
    }
}

/// Warn, never refuse, when the manifest is armed but this session's row
/// holds no role over the scope: `done`, `chart manifest-path` and the
/// post-compact reinject hook key on an EXACT `role_scope` and fail closed
/// and quietly without one.
fn warn_unpromoted_row(scope: &str, cwd: &std::path::Path) {
    let (session, harness) = crate::claims::resolve_identity();
    let rows = crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json());
    let row = match (session.filter(|s| !s.is_empty()), &rows) {
        (Some(sid), Ok(rows)) => {
            crate::lead_state::find_by_session(&rows.entries, &sid, harness.as_deref())
        }
        _ => None,
    };
    let Some(row) = row else {
        eprintln!(
            "lead: warning: manifest armed, but this session resolves to no registry row, so \
             its role cannot be checked. Run `/fno-me` to register, then ask your lead to \
             grant it: `fno agents org promote <handle> --scope {scope}`."
        );
        return;
    };
    let handle = Some(row.name.as_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("<handle>");
    let held = row.role_scope.as_deref().unwrap_or("");
    let territory = |s: &str| crate::lead_verdict_inputs::canonical_members_with_aliases(s, cwd);
    if row.role_level.is_some() && !held.is_empty() && territory(held) == territory(scope) {
        return;
    }
    let consequence = "`fno agents lead done` will refuse, `fno agents lead manifest-path` will \
                       exit non-zero without printing a path so the stop hook leaves \
                       LEAD_STATE_FILE unset, and the post-compact lead brief will never arrive.";
    let Some(level) = row.role_level else {
        eprintln!(
            "lead: warning: manifest armed for '{scope}', but this row carries NO role, so \
             {consequence} Ask your lead to grant it: `fno agents org promote {handle} \
             --scope {scope}`. Never hand that line to the user. {}",
            crate::lead_verdict_inputs::chat_grant_line(scope, handle)
        );
        return;
    };
    let label = format!(
        "L{level} {}",
        Some(held).filter(|h| !h.is_empty()).unwrap_or("?")
    );
    eprintln!(
        "lead: warning: manifest armed for '{scope}', but this row's role is '{label}', which \
         is not that scope. These readers key on an EXACT role_scope, so a role over wider \
         territory does not satisfy them any more than a role over unrelated territory: \
         {consequence} Ask your lead to re-scope it with `fno agents org promote \
         {handle} --scope {scope}`."
    );
}

// ---- cancel ---------------------------------------------------------------

/// `lead-state --cancel --scope S [--clear]`: set or clear the cancel signal
/// beside the scope's manifest in this checkout's space.
pub fn run_cancel(args: &[String]) -> i32 {
    let mut scope: Option<String> = None;
    let mut clear = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--scope" if i + 1 < args.len() => {
                scope = Some(args[i + 1].clone());
                i += 2;
            }
            "--clear" => {
                clear = true;
                i += 1;
            }
            other => {
                eprintln!("fno-agents lead-state --cancel: unknown argument {other}");
                return 2;
            }
        }
    }
    let Some(scope) = scope else {
        eprintln!("lead: cancel needs --scope <scope>.");
        return 2;
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let manifest = match crate::lead_state::manifest_path(&crate::paths::space_dir(&cwd), &scope) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("lead: {e}");
            return 2;
        }
    };
    if !manifest.is_file() {
        eprintln!(
            "lead: refusing cancel for '{scope}'; no lead manifest at canonical path {}",
            manifest.display()
        );
        return 1;
    }
    let sentinel = manifest.with_extension("cancelled");
    let written = if clear {
        match std::fs::remove_file(&sentinel) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sentinel)
            .map(|_| ())
    };
    if let Err(e) = written {
        eprintln!(
            "lead: could not update cancel signal {}: {e}",
            sentinel.display()
        );
        return 1;
    }
    if clear {
        println!("lead: cancel signal cleared: {}", sentinel.display());
        return 0;
    }
    // The sentinel on disk is the authority; the event only records it.
    let envelope = json!({
        "ts": crate::events::now_rfc3339(),
        "type": "cancel_signal_set",
        "source": "agents",
        "data": {"lane": "lead", "path": sentinel.display().to_string(),
                 "scope": scope, "reason": "operator"},
    });
    if let Some(space) = manifest.parent().and_then(|leads| leads.parent()) {
        if let Err(e) = crate::event_store::append_envelope(
            &space.join("events.jsonl"),
            &envelope.to_string(),
            None,
        ) {
            eprintln!("lead: WARNING: cancel signal event not emitted: {e}");
        }
    }
    println!("lead: cancel signal set: {}", sentinel.display());
    0
}

// ---- term -----------------------------------------------------------------

/// `span:<N>[smhd]` or `compactions:<N>`.
fn legal_term_spec(spec: &str) -> bool {
    if let Some(n) = spec.strip_prefix("compactions:") {
        return !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    }
    let Some(rest) = spec.strip_prefix("span:") else {
        return false;
    };
    match rest.as_bytes().split_last() {
        Some((unit, digits)) => {
            b"smhd".contains(unit) && !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
        }
        None => false,
    }
}

/// `lead-state --term SPEC [--reason TEXT] [--scope S]`: declare or extend
/// this holder's own term through `lead-shape --term`.
pub fn run_term(args: &[String]) -> i32 {
    let mut spec: Option<String> = None;
    let mut reason = String::new();
    let mut scope = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reason" if i + 1 < args.len() => {
                reason = args[i + 1].clone();
                i += 2;
            }
            "--scope" if i + 1 < args.len() => {
                scope = args[i + 1].clone();
                i += 2;
            }
            word if spec.is_none() && !word.starts_with('-') => {
                spec = Some(word.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }
    let Some(spec) = spec else {
        eprintln!("lead: term needs a spec: span:<N>[smhd] or compactions:<N>.");
        return 2;
    };
    if !legal_term_spec(spec.trim()) {
        eprintln!("lead: bad term spec '{spec}'; legal forms: span:<N>[smhd], compactions:<N>");
        return 2;
    }
    let (own, session, root) = match crate::lead_reads::own_role(&scope) {
        Ok(found) => found,
        Err(refusal) => {
            eprintln!("{refusal}");
            return 2;
        }
    };
    let mut argv = vec![
        "lead-shape".to_string(),
        "--scope".to_string(),
        own.clone(),
        "--root".to_string(),
        root.display().to_string(),
    ];
    if !session.is_empty() {
        argv.extend(["--session".to_string(), session]);
    }
    argv.extend(["--term".to_string(), spec]);
    if !reason.trim().is_empty() {
        argv.extend(["--reason".to_string(), reason]);
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno-agents"));
    let out = match Command::new(exe).args(&argv).output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("lead: could not run the term write: {e}");
            return 1;
        }
    };
    if !out.status.success() {
        eprintln!("lead: {}", String::from_utf8_lossy(&out.stderr).trim());
        return match (out.status.code(), out.status.signal()) {
            (Some(code), _) => code,
            (None, Some(signal)) => 128 + signal,
            (None, None) => 1,
        };
    }
    println!(
        "lead: term declared: {}",
        String::from_utf8_lossy(&out.stdout).trim()
    );
    println!("scope:  {own}");
    0
}

#[cfg(test)]
mod tests {
    use super::legal_term_spec;

    #[test]
    fn a_term_spec_is_a_span_or_a_compaction_count() {
        for good in ["span:4h", "span:96h", "span:30m", "compactions:3"] {
            assert!(legal_term_spec(good), "{good}");
        }
        for bad in [
            "span:4x",
            "span:h",
            "span:",
            "compactions:",
            "compactions:3h",
            "4h",
        ] {
            assert!(!legal_term_spec(bad), "{bad}");
        }
    }
}

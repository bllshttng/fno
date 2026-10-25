//! The user surfaces of the lead lifecycle commands: `fno agents org cancel`
//! and `term`, riding `lead-state --cancel` and `lead-state --term`. The words
//! each one prints are the retired Python commands' words, frozen in
//! `tests/golden/lead_lifecycle/`.

use serde_json::json;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Command;

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

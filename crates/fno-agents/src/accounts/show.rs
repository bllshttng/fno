//! `fno config accounts show <id> [--print-binding]`: every field of one
//! account record, or its launch binding as `KEY=VALUE` lines. The binding
//! projection names identity and namespace facts only, never a record's env,
//! so a re-entry resolver can compose it into receipts and logs.

use crate::accounts::env::{self, Sources};
use crate::accounts::records::{self, Accounts, Record};
use std::path::{Path, PathBuf};

/// Python's `repr` of a str: single quotes unless the text holds a single
/// quote and no double quote.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// The text view, one aligned `field: value` line per set field.
pub fn render(record: &Record, accounts: &Accounts) -> String {
    let mut lines = vec![
        format!("id:                  {}", record.id),
        format!("name:                {}", record.name),
        format!("harness:             {}", record.harness),
        format!("auth:                {}", record.auth),
        format!("priority:            {}", record.priority),
    ];
    if let Some(source) = &record.credentials_source {
        lines.push(format!("credentials_source:  {}", source.display()));
    }
    if let Some(env) = record.env.as_ref().filter(|env| !env.is_empty()) {
        let pairs: Vec<String> = env
            .iter()
            .map(|(k, v)| format!("{}: {}", py_repr(k), py_repr(v)))
            .collect();
        lines.push(format!("env:                 {{{}}}", pairs.join(", ")));
    }
    if let Some(route) = record.route.as_deref().filter(|r| !r.is_empty()) {
        lines.push(format!("route:               {route}"));
    }
    if !record.account_id.is_empty() {
        lines.push(format!("account_id:          {}", record.account_id));
    }
    if !record.tags.is_empty() {
        let tags: Vec<String> = record.tags.iter().map(|t| py_repr(t)).collect();
        lines.push(format!("tags:                [{}]", tags.join(", ")));
    }
    if let Some(description) = record.description.as_deref().filter(|d| !d.is_empty()) {
        lines.push(format!("description:         {description}"));
    }
    if accounts.active.as_deref() == Some(record.id.as_str()) {
        lines.push("active:              yes".to_string());
    }
    lines.join("\n")
}

/// The `--print-binding` lines: `ACCOUNT_ID`, `LANE`, and `CLAUDE_CONFIG_DIR`
/// when the lane has one.
pub fn binding_lines(overlay: &env::Overlay) -> String {
    let mut lines = vec![
        format!("ACCOUNT_ID={}", overlay.account_id),
        format!("LANE={}", overlay.lane),
    ];
    if let Some(dir) = overlay
        .env
        .get("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
    {
        lines.push(format!("CLAUDE_CONFIG_DIR={dir}"));
    }
    lines.join("\n")
}

/// The working dir the Python verb read: `$PWD`, else the process cwd.
pub(crate) fn pwd() -> PathBuf {
    std::env::var_os("PWD")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// An account id's launch binding for the re-entry resolver: its config dir
/// when the lane has one, `Ok(None)` for a lane on the default namespace.
/// `Err` is the refusal receipt.
pub fn binding(account_id: &str, cwd: &Path, src: &Sources<'_>) -> Result<Option<String>, String> {
    let accounts = records::load(Some(cwd), cwd).map_err(|e| e.to_string())?;
    if accounts.by_id(account_id).is_none() {
        return Err(format!("account '{account_id}' not found"));
    }
    let overlay = env::overlay(account_id, None, None, cwd, src)?;
    Ok(overlay
        .env
        .get("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .cloned())
}

/// `fno-agents accounts show <id> [--print-binding]`.
pub fn run(args: &[String]) -> i32 {
    let mut id = None;
    let mut print_binding = false;
    for arg in args {
        match arg.as_str() {
            "--print-binding" => print_binding = true,
            "-h" | "--help" => {
                println!("Usage: fno config accounts show <id> [--print-binding]\n\n  Show all fields of a single account record.\n\n  --print-binding  the account's launch binding as KEY=VALUE lines (ACCOUNT_ID,\n                   LANE, CLAUDE_CONFIG_DIR when the lane has one); never the\n                   record's env or credentials.");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: no such option: {flag}");
                return 2;
            }
            word if id.is_none() => id = Some(word.to_string()),
            word => {
                eprintln!("error: unexpected extra argument ({word})");
                return 2;
            }
        }
    }
    let Some(id) = id else {
        eprintln!("error: missing argument 'PROVIDER_ID'");
        return 2;
    };
    let cwd = pwd();
    let accounts = match records::load(Some(&cwd), &cwd) {
        Ok(accounts) => accounts,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let Some(record) = accounts.by_id(&id) else {
        eprintln!("error: account '{id}' not found");
        return 1;
    };
    if !print_binding {
        println!("{}", render(record, &accounts));
        return 0;
    }
    match env::overlay(&id, None, None, &cwd, &Sources::system()) {
        Ok(overlay) => {
            println!("{}", binding_lines(&overlay));
            0
        }
        Err(receipt) => {
            eprintln!("error: {receipt}");
            1
        }
    }
}

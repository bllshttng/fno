//! `fno config accounts remove <id> [--force] [--scope global|project]`:
//! drop one account record from one scope's own config file, refusing an id
//! any layer's `active` names without `--force`.

use crate::accounts::add::split_flag;
use crate::accounts::records::{self, Scope};
use crate::accounts::show::pwd;
use crate::accounts::writer;

const HELP: &str = "Usage: fno config accounts remove [OPTIONS] PROVIDER_ID

  Remove an account record. Refuses to remove the active account without --force.

  -F, --force       Allow removing the active provider
      --scope TEXT  global|project  [default: global]";

#[derive(Default)]
struct Args {
    id: Option<String>,
    scope: Option<String>,
    force: bool,
}

fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = split_flag(arg);
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "-F" | "--force" if inline.is_none() => out.force = true,
            "--scope" => {
                let value = inline.or_else(|| it.next().cloned());
                out.scope = Some(value.ok_or("error: option '--scope' requires an argument")?);
            }
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("error: no such option: {f}"))
            }
            _ if out.id.is_none() => out.id = Some(arg.clone()),
            _ => return Err(format!("error: unexpected extra argument ({arg})")),
        }
    }
    Ok(Some(out))
}

pub fn run(args: &[String]) -> i32 {
    let args = match parse(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{HELP}");
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let Some(id) = args.id else {
        eprintln!("error: missing argument 'PROVIDER_ID'");
        return 2;
    };
    let scope_word = args.scope.unwrap_or_else(|| "global".to_string());
    let scope = if scope_word == "project" {
        Scope::Project
    } else {
        Scope::Global
    };
    let cwd = pwd();

    // Scope-own read: removal edits ONE file's records; the merged view would
    // freeze every other layer's records into the file being written.
    let config = match records::load_scope(scope, &cwd) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if config.by_id(&id).is_none() {
        eprintln!("error: account '{id}' not found");
        return 1;
    }

    // A project may point at a global record, so the refusal reads every
    // layer's own `active`: deleting a record any layer names bricks that
    // layer's loader.
    if !args.force {
        let pointed_at = records::candidates(None, &cwd).iter().any(|path| {
            records::extract_accounts_block(&records::read_layer(path))
                .and_then(|block| block.get("active"))
                .and_then(toml::Value::as_str)
                == Some(id.as_str())
        });
        if pointed_at {
            eprintln!(
                "error: '{id}' is an active provider (some config layer points at it). Use --force to remove it."
            );
            return 1;
        }
    }

    let Some(target) = records::scope_path(scope, &cwd) else {
        eprintln!("error: no HOME to resolve the global config.toml from");
        return 1;
    };
    let written = writer::mutate(&target, |doc| -> Result<(), String> {
        let data: toml::Table = doc
            .to_string()
            .parse()
            .map_err(|e: toml::de::Error| e.to_string())?;
        let config = records::scope_accounts(&data).map_err(|e| e.to_string())?;
        let kept: Vec<_> = config.records.into_iter().filter(|r| r.id != id).collect();
        let active = config.active.filter(|a| *a != id);
        writer::set_records(doc, &kept, active.as_deref());
        Ok(())
    });
    let refusal = match written {
        Ok(Ok(())) => None,
        Ok(Err(msg)) | Err(writer::WriteError::Unreadable(msg)) => Some(format!("error: {msg}")),
        Err(writer::WriteError::Io(e)) => {
            Some(format!("error: failed to write {}: {e}", target.display()))
        }
    };
    if let Some(msg) = refusal {
        eprintln!("{msg}");
        return 1;
    }
    println!("Account '{id}' removed (scope={scope_word}).");
    0
}

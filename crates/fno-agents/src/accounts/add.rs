//! `fno config accounts add <id> -H <harness> -a <auth> ...`: validate one
//! account record and write it into one scope's own config file, refusing an
//! existing id without `--force`.

use crate::accounts::records::{self, Scope};
use crate::accounts::show::pwd;
use crate::accounts::writer;

const HELP: &str = "Usage: fno config accounts add [OPTIONS] PROVIDER_ID

  Add a new account record. Refuses to overwrite without --force.

  -H, --harness TEXT           claude|codex|opencode|agy|trae|openclaw|hermes  [required]
  -a, --auth TEXT              oauth_dir|api_key|managed  [required]
      --credentials-source PATH
      --env TEXT               KEY=VALUE pairs for api_key auth (repeatable)
      --route TEXT             provider/model route used as the api_key credential source
  -p, --priority INTEGER       [default: 100]
      --name TEXT
      --account-id TEXT
  -s, --scope TEXT             global|project  [default: global]
  -F, --force                  Overwrite existing record";

#[derive(Default)]
struct Args {
    id: Option<String>,
    harness: Option<String>,
    auth: Option<String>,
    credentials_source: Option<String>,
    env: Vec<String>,
    route: Option<String>,
    priority: Option<String>,
    name: Option<String>,
    account_id: Option<String>,
    scope: Option<String>,
    force: bool,
}

/// Click's parse: `--opt value`, `--opt=value`, `-X value`, `-Xvalue`.
fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ if arg.len() > 2
                && arg.starts_with('-')
                && !arg.starts_with("--")
                && arg.is_char_boundary(2) =>
            {
                (arg[..2].to_string(), Some(arg[2..].to_string()))
            }
            _ => (arg.clone(), None),
        };
        let slot = match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "-F" | "--force" if inline.is_none() => {
                out.force = true;
                continue;
            }
            "-H" | "--harness" => &mut out.harness,
            "-a" | "--auth" => &mut out.auth,
            "--credentials-source" => &mut out.credentials_source,
            "--route" => &mut out.route,
            "-p" | "--priority" => &mut out.priority,
            "--name" => &mut out.name,
            "--account-id" => &mut out.account_id,
            "-s" | "--scope" => &mut out.scope,
            "--env" => {
                let value = inline.or_else(|| it.next().cloned());
                out.env
                    .push(value.ok_or("error: option '--env' requires an argument")?);
                continue;
            }
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("error: no such option: {f}"))
            }
            _ if out.id.is_none() => {
                out.id = Some(arg.clone());
                continue;
            }
            _ => return Err(format!("error: unexpected extra argument ({arg})")),
        };
        let value = inline.or_else(|| it.next().cloned());
        *slot = Some(value.ok_or_else(|| format!("error: option '{flag}' requires an argument"))?);
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
    let missing = |what: &str| {
        eprintln!("error: missing {what}");
        2
    };
    let Some(id) = args.id else {
        return missing("argument 'PROVIDER_ID'");
    };
    let Some(harness) = args.harness else {
        return missing("option '--harness' / '-H'");
    };
    let Some(auth) = args.auth else {
        return missing("option '--auth' / '-a'");
    };
    let priority = match args
        .priority
        .as_deref()
        .map(str::trim)
        .map(str::parse::<i64>)
    {
        None => 100,
        Some(Ok(p)) => p,
        Some(Err(_)) => {
            eprintln!(
                "error: Invalid value for '--priority' / '-p': '{}' is not a valid integer.",
                args.priority.unwrap_or_default()
            );
            return 2;
        }
    };
    let scope_word = args.scope.unwrap_or_else(|| "global".to_string());
    let scope = if scope_word == "project" {
        Scope::Project
    } else {
        Scope::Global
    };

    let mut env = toml::Table::new();
    for pair in &args.env {
        let Some((key, value)) = pair.split_once('=') else {
            eprintln!("error: malformed --env entry '{pair}' (expected KEY=VALUE format)");
            return 1;
        };
        env.insert(key.to_string(), toml::Value::String(value.to_string()));
    }

    let mut fields = toml::Table::new();
    let name = args
        .name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.clone());
    for (key, value) in [
        ("id", Some(id)),
        ("name", Some(name)),
        ("harness", Some(harness)),
        ("auth", Some(auth)),
        ("credentials_source", args.credentials_source),
        ("route", args.route),
        ("account_id", args.account_id),
    ] {
        if let Some(value) = value {
            fields.insert(key.to_string(), toml::Value::String(value));
        }
    }
    fields.insert("priority".to_string(), toml::Value::Integer(priority));
    if !env.is_empty() {
        fields.insert("env".to_string(), toml::Value::Table(env));
    }
    let record = match records::build_record(&fields) {
        Ok(record) => record,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };

    let cwd = pwd();
    let Some(target) = records::scope_path(scope, &cwd) else {
        eprintln!("error: no HOME to resolve the global config.toml from");
        return 1;
    };
    let force = args.force;
    let record_id = record.id.clone();
    // The scope read, the duplicate check and the write share one lock hold,
    // so a concurrent account write cannot land between them and be lost.
    let outcome = writer::mutate(&target, |doc| -> Result<(), String> {
        let data: toml::Table = doc
            .to_string()
            .parse()
            .map_err(|e| format!("error loading existing config: {e}"))?;
        if records::reads_legacy_block(&data) {
            eprintln!(
                "config.providers is the pre-rename name for config.accounts; the next account write migrates it"
            );
        }
        let config = records::scope_accounts(&data)
            .map_err(|e| format!("error loading existing config: {e}"))?;
        if config.by_id(&record.id).is_some() && !force {
            return Err(format!(
                "error: account '{}' already exists. Use --force to overwrite.",
                record.id
            ));
        }
        let mut kept: Vec<_> = config
            .records
            .into_iter()
            .filter(|r| r.id != record.id)
            .collect();
        kept.push(record);
        writer::set_records(doc, &kept, config.active.as_deref());
        Ok(())
    });
    let refusal = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(msg)) => Some(msg),
        Err(writer::WriteError::Unreadable(msg)) => Some(format!("error: {msg}")),
        Err(writer::WriteError::Io(e)) => {
            Some(format!("error: failed to write {}: {e}", target.display()))
        }
    };
    if let Some(msg) = refusal {
        eprintln!("{msg}");
        return 1;
    }
    println!("Account '{record_id}' added (scope={scope_word}).");
    0
}

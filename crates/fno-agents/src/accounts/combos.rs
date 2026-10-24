//! `fno config accounts combos <add|list|remove|test|use|update>`: named
//! ordered account lists with a rotation strategy. `list` and `test` read the
//! merged combos and the runtime-state file; the four writers edit one
//! scope's own config file under the settings lock.

use crate::accounts::add::split_flag;
use crate::accounts::records::{self, py_float, py_list_repr, Scope};
use crate::accounts::show::pwd;
use crate::accounts::writer;
use crate::claude_ask::py_repr;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table};

const USAGE: &str = "Usage: fno config accounts combos [OPTIONS] COMMAND [ARGS]...

  Manage account combos (named ordered lists with rotation strategies).

Commands:
  add     Add a new combo.
  list    List all configured combos with cursor + member-count info.
  remove  Remove a combo.
  test    Validate a combo: shape, member existence, per-member cooldown state.
  use     Set the active combo (used as default when no --combo flag is passed).
  update  Atomically replace an existing combo's members/strategy in one write.";

const LEGACY_NOTICE: &str =
    "config.providers is the pre-rename name for config.accounts; the next account write migrates it";

/// One argument's parsed flags. `None` fields were not given.
#[derive(Default)]
struct Args {
    name: Option<String>,
    strategy: Option<String>,
    sticky: Option<i64>,
    providers: Option<String>,
    scope: Option<String>,
    json: bool,
}

fn parse(args: &[String], value_flags: &[&str], json_flag: bool) -> Result<Option<Args>, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = split_flag(arg);
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "-J" | "--json" if json_flag && inline.is_none() => out.json = true,
            f if value_flags.contains(&f) => {
                let value = inline
                    .or_else(|| it.next().cloned())
                    .ok_or_else(|| format!("error: option '{f}' requires an argument"))?;
                match f {
                    "--strategy" => out.strategy = Some(value),
                    "--providers" => out.providers = Some(value),
                    "--scope" => out.scope = Some(value),
                    _ => {
                        let sticky = value.trim().parse().map_err(|_| {
                            format!("error: invalid value for '--sticky': '{value}' is not a valid integer")
                        })?;
                        out.sticky = Some(sticky);
                    }
                }
            }
            f if f.starts_with('-') && f.len() > 1 => {
                return Err(format!("error: no such option: {f}"))
            }
            _ if out.name.is_none() && !json_flag => out.name = Some(arg.clone()),
            _ => return Err(format!("error: unexpected extra argument ({arg})")),
        }
    }
    Ok(Some(out))
}

/// The args after the argument word: `0` with stdout and stderr, or the exit code.
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn new() -> Out {
        Out {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }
    fn out(&mut self, line: &str) {
        self.stdout.push_str(line);
        self.stdout.push('\n');
    }
    fn err(&mut self, line: &str) {
        self.stderr.push_str(line);
        self.stderr.push('\n');
    }
    fn fail(mut self, line: &str) -> Out {
        self.err(line);
        self.code = 1;
        self
    }
}

pub fn run(args: &[String]) -> i32 {
    let out = run_in(args, &pwd());
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    out.code
}

fn run_in(args: &[String], cwd: &Path) -> Out {
    let mut out = Out::new();
    let Some(word) = args.first().map(String::as_str) else {
        out.err(USAGE);
        out.code = 2;
        return out;
    };
    if word == "-h" || word == "--help" {
        out.out(USAGE);
        return out;
    }
    let rest = &args[1..];
    let (value_flags, json_flag): (&[&str], bool) = match word {
        "add" | "update" => (&["--strategy", "--sticky", "--providers", "--scope"], false),
        "remove" | "use" => (&["--scope"], false),
        "list" => (&[], true),
        "test" => (&[], false),
        other => {
            out.err(&format!("error: no such command '{other}'"));
            out.code = 2;
            return out;
        }
    };
    let parsed = match parse(rest, value_flags, json_flag) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            out.out(&format!(
                "Usage: fno config accounts combos {word} [OPTIONS]"
            ));
            return out;
        }
        Err(msg) => {
            out.err(&msg);
            out.code = 2;
            return out;
        }
    };
    if word != "list" && parsed.name.is_none() {
        out.err("error: missing argument 'NAME'");
        out.code = 2;
        return out;
    }
    if matches!(word, "add" | "update") && parsed.providers.is_none() {
        out.err("error: missing option '--providers'");
        out.code = 2;
        return out;
    }
    if matches!(word, "list" | "test" | "use") {
        legacy_notice(cwd, &mut out);
    }
    match word {
        "add" => add(parsed, cwd, out),
        "list" => list(parsed.json, cwd, out),
        "remove" => remove(parsed, cwd, out),
        "test" => test(&parsed.name.unwrap_or_default(), cwd, out),
        "use" => use_combo(parsed, cwd, out),
        _ => update(parsed, cwd, out),
    }
}

/// The loader announces a legacy `providers` block once, the first time a
/// merged read returns one; `remove` reads only its own file raw, so it never does.
fn legacy_notice(cwd: &Path, out: &mut Out) {
    if records::candidates(Some(cwd), cwd)
        .iter()
        .any(|path| records::reads_legacy_block(&records::read_layer(path)))
    {
        out.err(LEGACY_NOTICE);
    }
}

/// `rotation.compute_providers_hash`: the cursor key of an ordered member list.
fn providers_hash(providers: &[String]) -> String {
    let digest = Sha256::digest(providers.join("|").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}

/// The `--providers` csv, trimmed and with blanks dropped, checked against the
/// merged records. `Err` is the stderr line.
fn members(name: &str, csv: &str, cwd: &Path, out: &mut Out) -> Result<Vec<String>, String> {
    let list: Vec<String> = csv
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if list.is_empty() {
        return Err("error: --providers must list at least one provider id".to_string());
    }
    legacy_notice(cwd, out);
    let accounts = records::load(Some(cwd), cwd).map_err(|e| format!("error: {e}"))?;
    let unknown: Vec<&String> = list
        .iter()
        .filter(|p| accounts.by_id(p).is_none())
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "error: combo {} references unknown provider id(s) {} (not in config.providers.records)",
            py_repr(name),
            py_list_repr(&unknown)
        ));
    }
    Ok(list)
}

fn scope_of(word: &str) -> Scope {
    if word == "project" {
        Scope::Project
    } else {
        Scope::Global
    }
}

/// Run `edit` on the scope file's accounts block under the settings lock. The
/// shadowed-block warnings go to stderr whether or not the edit lands.
fn write_scope(
    scope_word: &str,
    cwd: &Path,
    out: &mut Out,
    edit: impl FnOnce(&mut Table) -> Result<(), String>,
) -> Result<(), String> {
    let Some(target) = records::scope_path(scope_of(scope_word), cwd) else {
        return Err("error: no HOME to resolve the global config.toml from".to_string());
    };
    let mut warnings = Vec::new();
    let written = writer::mutate(&target, |doc: &mut DocumentMut| {
        warnings = writer::accounts_block(doc);
        let Some(block) = doc.get_mut("accounts").and_then(Item::as_table_mut) else {
            unreachable!("accounts_block leaves a table at accounts");
        };
        edit(block)
    });
    for line in warnings {
        out.err(&line);
    }
    match written {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) | Err(writer::WriteError::Unreadable(msg)) => Err(format!("error: {msg}")),
        Err(writer::WriteError::Io(e)) => Err(format!("error: failed to write settings.yaml: {e}")),
    }
}

/// The `combos` table of an accounts block, created when absent.
fn combos_table(block: &mut Table) -> &mut Table {
    let as_table = match block.remove("combos").map(Item::into_table) {
        Some(Ok(table)) => table,
        _ => Table::new(),
    };
    block.insert("combos", Item::Table(as_table));
    block
        .get_mut("combos")
        .and_then(Item::as_table_mut)
        .expect("combos was just inserted as a table")
}

fn combo_table(strategy: &str, sticky: i64, providers: &[String]) -> Item {
    let mut table = Table::new();
    table.insert("strategy", toml_edit::value(strategy));
    table.insert("sticky_limit", toml_edit::value(sticky));
    let list: toml_edit::Array = providers.iter().map(String::as_str).collect();
    table.insert("providers", toml_edit::value(list));
    Item::Table(table)
}

fn add(args: Args, cwd: &Path, out: Out) -> Out {
    let name = args.name.unwrap_or_default();
    let strategy = args.strategy.unwrap_or_else(|| "fallback".to_string());
    let sticky = args.sticky.unwrap_or(1);
    let scope = args.scope.unwrap_or_else(|| "project".to_string());
    let mut out = out;
    let providers = match members(
        &name,
        args.providers.as_deref().unwrap_or_default(),
        cwd,
        &mut out,
    ) {
        Ok(list) => list,
        Err(line) => return out.fail(&line),
    };
    if let Err(msg) = records::combo(&name, &strategy, sticky, providers.clone()) {
        return out.fail(&format!("error: {msg}"));
    }
    // The raw --sticky is written; only the loader clamps it.
    let written = write_scope(&scope, cwd, &mut out, |block| {
        let combos = combos_table(block);
        if combos.contains_key(&name) {
            return Err(format!(
                "combo {} already exists in {scope} settings; remove it first or use a different name",
                py_repr(&name)
            ));
        }
        combos.insert(&name, combo_table(&strategy, sticky, &providers));
        Ok(())
    });
    if let Err(line) = written {
        return out.fail(&line);
    }
    out.out(&format!(
        "Combo {} added (strategy={strategy}, sticky={sticky}, providers={}, scope={scope}).",
        py_repr(&name),
        py_list_repr(&providers)
    ));
    out
}

/// `json.dumps`' default `ensure_ascii`: every non-ASCII char as `\uXXXX`.
fn ascii_json(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            escaped.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                escaped.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    escaped
}

fn list(as_json: bool, cwd: &Path, out: Out) -> Out {
    let combos = match records::load_combos(Some(cwd), cwd) {
        Ok(combos) => combos,
        Err(e) => return out.fail(&format!("error: {e}")),
    };
    let mut out = out;
    if combos.is_empty() {
        out.out(if as_json {
            "[]"
        } else {
            "No combos configured. Run `fno config accounts combos add` to add one."
        });
        return out;
    }
    let active = records::load_active_combo(Some(cwd), cwd);
    let state_path = crate::provider_cap::runtime_state_path(cwd);
    let now = now();
    let rows: Vec<Value> = combos
        .iter()
        .map(|combo| {
            let cursor = crate::accounts::state::read_cursor(
                &state_path,
                &combo.name,
                &providers_hash(&combo.providers),
                now,
            );
            json!({
                "name": combo.name,
                "strategy": combo.strategy,
                "sticky_limit": combo.sticky_limit,
                "members": combo.providers,
                "active": active.as_deref() == Some(combo.name.as_str()),
                "cursor_index": cursor.as_ref().map(|c| c.cursor_index),
                "consecutive_use_count": cursor.as_ref().map(|c| c.consecutive_use_count),
            })
        })
        .collect();
    if as_json {
        let text = serde_json::to_string_pretty(&rows).unwrap_or_default();
        out.out(&ascii_json(&text));
        return out;
    }
    for row in &rows {
        let cursor = match (&row["cursor_index"], &row["consecutive_use_count"]) {
            (Value::Number(i), count) => format!("idx={i}/cnt={count}"),
            _ => "no-cursor".to_string(),
        };
        let members: Vec<&str> = row["members"]
            .as_array()
            .map(|m| m.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        out.out(&format!(
            "{}  [{}, sticky={}]  members={}  {cursor}",
            row["name"].as_str().unwrap_or_default(),
            row["strategy"].as_str().unwrap_or_default(),
            row["sticky_limit"],
            members.join(",")
        ));
    }
    out
}

fn remove(args: Args, cwd: &Path, out: Out) -> Out {
    let name = args.name.unwrap_or_default();
    let scope = args.scope.unwrap_or_else(|| "project".to_string());
    let mut out = out;
    let mut cleared = false;
    let written = write_scope(&scope, cwd, &mut out, |block| {
        if !block
            .get("combos")
            .and_then(Item::as_table_like)
            .is_some_and(|combos| combos.contains_key(&name))
        {
            return Err(format!(
                "combo {} not found in {scope} settings",
                py_repr(&name)
            ));
        }
        // An emptied table stays on disk as `[accounts.combos]`, as Python leaves it.
        let combos = combos_table(block);
        combos.remove(&name);
        combos.set_implicit(false);
        if block.get("active_combo").and_then(Item::as_str) == Some(name.as_str()) {
            block.remove("active_combo");
            cleared = true;
        }
        Ok(())
    });
    if let Err(line) = written {
        return out.fail(&line);
    }
    let mut msg = format!("Combo {} removed (scope={scope}).", py_repr(&name));
    if cleared {
        msg.push_str(" active_combo cleared.");
    }
    out.out(&msg);
    out
}

fn test(name: &str, cwd: &Path, out: Out) -> Out {
    let combos = match records::load_combos(Some(cwd), cwd) {
        Ok(combos) => combos,
        Err(e) => return out.fail(&format!("error: {e}")),
    };
    let Some(combo) = combos.iter().find(|c| c.name == name) else {
        return out.fail(&format!("error: combo {} not found", py_repr(name)));
    };
    let now = now();
    let state =
        crate::accounts::state::read_state(&crate::provider_cap::runtime_state_path(cwd), now);
    let mut out = out;
    out.out(&format!(
        "Combo {}: strategy={}, sticky={}",
        py_repr(name),
        combo.strategy,
        combo.sticky_limit
    ));
    let mut cooled = 0;
    for pid in &combo.providers {
        let until = state
            .provider_health
            .get(pid)
            .and_then(|h| h.rate_limited_until)
            .filter(|until| *until > 0.0 && *until > now);
        match until {
            Some(until) => {
                cooled += 1;
                out.out(&format!("  {pid}: in_cooldown until {}", py_float(until)));
            }
            None => out.out(&format!("  {pid}: ok")),
        }
    }
    let verdict = if cooled == 0 {
        "all_healthy"
    } else if cooled == combo.providers.len() {
        "all_in_cooldown"
    } else {
        "partial_cooldown"
    };
    out.out(&format!("verdict: {verdict}"));
    out
}

fn use_combo(args: Args, cwd: &Path, out: Out) -> Out {
    let name = args.name.unwrap_or_default();
    let scope = args.scope.unwrap_or_else(|| "project".to_string());
    match records::load_combos(Some(cwd), cwd) {
        Ok(combos) if combos.iter().any(|c| c.name == name) => {}
        Ok(_) => return out.fail(&format!("error: combo {} not found", py_repr(&name))),
        Err(e) => return out.fail(&format!("error: {e}")),
    }
    let mut out = out;
    let written = write_scope(&scope, cwd, &mut out, |block| {
        block.insert("active_combo", toml_edit::value(name.as_str()));
        Ok(())
    });
    if let Err(line) = written {
        return out.fail(&line);
    }
    out.out(&format!(
        "Active combo set to {} (scope={scope}).",
        py_repr(&name)
    ));
    out
}

fn update(args: Args, cwd: &Path, out: Out) -> Out {
    let name = args.name.unwrap_or_default();
    let scope = args.scope.unwrap_or_else(|| "project".to_string());
    let mut out = out;
    let providers = match members(
        &name,
        args.providers.as_deref().unwrap_or_default(),
        cwd,
        &mut out,
    ) {
        Ok(list) => list,
        Err(line) => return out.fail(&line),
    };
    let mut applied = (String::new(), 0);
    let written = write_scope(&scope, cwd, &mut out, |block| {
        let combos = combos_table(block);
        let Some(existing) = combos.get(&name).and_then(Item::as_table_like) else {
            return Err(format!(
                "combo {} not found in {scope} settings; use `combos add` to create it",
                py_repr(&name)
            ));
        };
        // An omitted flag keeps the stored value, so a reorder never resets
        // a round_robin combo to fallback/1.
        let strategy = match &args.strategy {
            Some(s) => s.clone(),
            None => existing
                .get("strategy")
                .map(|item| {
                    item.as_str()
                        .map_or_else(|| item.to_string(), str::to_string)
                })
                .unwrap_or_else(|| "fallback".to_string()),
        };
        let sticky = args.sticky.unwrap_or_else(|| {
            existing
                .get("sticky_limit")
                .and_then(Item::as_integer)
                .unwrap_or(1)
        });
        records::combo(&name, &strategy, sticky, providers.clone())?;
        combos.insert(&name, combo_table(&strategy, sticky, &providers));
        applied = (strategy, sticky);
        Ok(())
    });
    if let Err(line) = written {
        return out.fail(&line);
    }
    out.out(&format!(
        "Combo {} updated (strategy={}, sticky={}, providers={}, scope={scope}).",
        py_repr(&name),
        applied.0,
        applied.1,
        py_list_repr(&providers)
    ));
    out
}

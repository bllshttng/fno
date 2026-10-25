//! `fno project`: isolated per-project fno environments, ported from the
//! Python project family (the project-family port).
//!
//! Two commands behind the spawn-overlay door (no new fno-agents root command,
//! the same nesting the plugins family uses):
//!
//! - `spawn-overlay project init <ID> [--repo-root <path>]` writes
//!   `state_dir` + `backlog.id_prefix` into the repo-local fno config, then
//!   creates the state root and prints the isolation receipt. The receipt
//!   and the refusals (a pinned `state_dir` elsewhere, a corrupt config, an
//!   id that fails the prefix shape or hits a reserved family) are the
//!   contract, frozen in `tests/golden/project/` from the Python leg before
//!   its deletion.
//! - `spawn-overlay project resolve <name>` maps a project name or
//!   short_name to its canonical name over the global fno config (legacy
//!   settings.yaml fallback, read-only, `config:` wrapper unwrapped for
//!   yaml). `role.py`'s `_canonical_project` calls it through the
//!   rust_binary door and keeps the None-on-error posture.
//!
//! The write preserves the rest of the file where the Python writer
//! canonicalized it through tomli_w; the two keys land identically and a
//! fresh file renders the exact frozen bytes. The whole-model block
//! validation the shared Python writer ran on the final state stays with the
//! config family: this door refuses only what it can see.

use std::path::{Path, PathBuf};
use toml_edit::{value, DocumentMut, Item, Table};

/// Where an isolated state root lives (`~` expands against `$HOME`).
const PROJECTS_ROOT: &str = "~/.fno/projects";

/// The id_prefix validator's reserved families.
const RESERVED_PREFIXES: [&str; 3] = ["cv-", "fu-", "tgt-"];

/// Entry for the spawn-overlay door: `project` commands answer natively.
pub fn run(args: &[String]) -> i32 {
    match args.split_first() {
        Some((cmd, rest)) if cmd == "init" => cmd_init(rest),
        Some((cmd, rest)) if cmd == "resolve" => cmd_resolve(rest),
        Some((cmd, _rest)) if cmd == "map" => cmd_map(),
        _ => {
            eprintln!(
                "usage: fno config project init <ID> [--repo-root <path>]\n       fno config project resolve <name>\n       fno config project map"
            );
            2
        }
    }
}

/// `project map`: the whole alias registry as one JSON object, the machine
/// answer the rivals scanner consumes (the deleted Python module's cache
/// dump, carried by the binary that owns the registry now).
fn cmd_map() -> i32 {
    match load_registry() {
        Ok(map) => {
            let object = serde_json::Map::from_iter(
                map.iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))),
            );
            println!("{}", serde_json::Value::Object(object));
            0
        }
        Err(message) => {
            eprint!("{message}");
            1
        }
    }
}

/// Parse `--repo-root <path>` / `--repo-root=<path>` plus one positional ID.
fn parse_init_args(args: &[String]) -> Option<(String, Option<String>)> {
    let mut id: Option<String> = None;
    let mut repo_root: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--repo-root" {
            if i + 1 >= args.len() {
                eprintln!("fno config project init: --repo-root needs a path");
                return None;
            }
            repo_root = Some(args[i + 1].clone());
            i += 2;
        } else if let Some(val) = arg.strip_prefix("--repo-root=") {
            repo_root = Some(val.to_string());
            i += 1;
        } else if arg.starts_with('-') {
            eprintln!("fno config project init: unknown flag {arg}");
            return None;
        } else if id.is_none() {
            id = Some(arg.clone());
            i += 1;
        } else {
            eprintln!("fno config project init: unexpected argument {arg}");
            return None;
        }
    }
    id.map(|id| (id, repo_root))
}

/// `project init <ID> [--repo-root <path>]`.
fn cmd_init(args: &[String]) -> i32 {
    let Some((id, repo_root)) = parse_init_args(args) else {
        return 2;
    };
    // Normalize BEFORE anything derives from it: `backlog.id_prefix` is
    // lowercased by its own validator at read time, so an id typed `WEB`
    // would mint `web-` nodes while the root sits at `.../projects/WEB`.
    let id = id.trim().to_lowercase();
    let target = format!("{PROJECTS_ROOT}/{id}");

    let root: PathBuf = match repo_root {
        Some(p) => PathBuf::from(p),
        None => match std::env::var_os("FNO_REPO_ROOT") {
            Some(p) => PathBuf::from(p),
            None => crate::paths::worktree_repo_root(
                &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
        },
    };
    let cfg_path = root.join(".fno").join("config.toml");
    let doc = match std::fs::read_to_string(&cfg_path) {
        Ok(text) => match text.parse::<DocumentMut>() {
            Ok(doc) => Some(doc),
            Err(err) => {
                // The refusal read degrades a corrupt file to no-refusal,
                // but the write leg refuses: overwriting a file the parser
                // cannot read would drop whatever it holds.
                eprintln!("refusing: cannot parse {}: {err}", cfg_path.display());
                return 2;
            }
        },
        Err(_) => None,
    };

    // Refusal: a state_dir already pinned somewhere else. Overwriting it
    // would silently move a live environment's graph, ledger and mail bus.
    if let Some(doc) = &doc {
        if let Some(existing) = doc.get("state_dir").and_then(Item::as_str) {
            let existing = existing.trim();
            if !existing.is_empty() && existing != target {
                eprintln!(
                    "refusing: {} already sets\n  state_dir = '{existing}'\nand this would change it to '{target}'. Nothing was written.\nEdit that key yourself if the move is what you want.",
                    cfg_path.display()
                );
                return 1;
            }
        }
    }

    if let Some(reason) = id_prefix_violation(&id) {
        eprintln!("refusing: invalid value for .backlog.id_prefix: Value error, {reason}");
        return 2;
    }

    if let Err(err) = write_project_config(doc, &cfg_path, &target, &id) {
        eprintln!("{err}");
        return 1;
    }

    let expanded = match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".fno/projects").join(&id),
        // Python's expanduser crashed here; this door refuses instead.
        None => {
            eprintln!("refusing: HOME is not set; cannot create {target}");
            return 1;
        }
    };
    if let Err(err) = std::fs::create_dir_all(&expanded) {
        eprintln!("refusing: could not create {}: {err}", expanded.display());
        return 1;
    }

    println!("{}", receipt(&id, &target));
    0
}

/// The writer's id_prefix validation, verbatim in wording.
fn id_prefix_violation(id: &str) -> Option<String> {
    let bytes = id.as_bytes();
    let valid = !bytes.is_empty() && bytes[0].is_ascii_lowercase() && {
        // The trailing dash is the validator's optional `-?` and never counts
        // against the body: `abcdefg` and `abcdefg-` are both 7 letters.
        let body = if bytes.len() > 1 && bytes[bytes.len() - 1] == b'-' {
            &bytes[1..bytes.len() - 1]
        } else {
            &bytes[1..]
        };
        body.len() <= 6
            && body
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    };
    if !valid {
        return Some(format!(
            "config.backlog.id_prefix must be a lowercase token of 1-7 letters/digits (letter-led) with an optional trailing '-', got: '{id}'"
        ));
    }
    let normalized = if id.ends_with('-') {
        id.to_string()
    } else {
        format!("{id}-")
    };
    if RESERVED_PREFIXES.contains(&normalized.as_str()) {
        return Some(format!(
            "config.backlog.id_prefix must not collide with a reserved family ['cv-', 'fu-', 'tgt-'], got: '{id}'"
        ));
    }
    None
}

/// The success receipt: which half moved and which half cannot.
fn receipt(project_id: &str, state_dir: &str) -> String {
    format!(
        "project {project_id}: isolated\n  state_dir  {state_dir}   \
         (graph, ledger, briefs, agent registry, mail bus)\n  \
         The only fno file left in the checkout is .fno/config.toml; session\n  \
         state lives in the repo's space.\n  \
         NOT isolated: the claude daemon namespace and any codex app-server.\n  \
         Those are machine-global and fno does not own them. A session started here\n  \
         shares them with your real fleet, so an ambient identity marker crosses this\n  \
         boundary as if it were not there.\n  \
         mail: this environment has its OWN bus. Your real sessions cannot reach a\n  \
         worker here, and a worker here cannot reach them."
    )
}

/// Set the two keys and write atomically, preserving the rest of the file.
fn write_project_config(
    doc: Option<DocumentMut>,
    cfg_path: &Path,
    target: &str,
    id: &str,
) -> Result<(), String> {
    let mut doc = doc.unwrap_or_default();
    doc["state_dir"] = value(target);
    if !matches!(doc.get("backlog"), Some(Item::Table(_))) {
        let mut table = Table::new();
        // A fresh document renders `state_dir = "..."` then a blank line
        // before the header, the exact bytes the Python leg froze.
        table.decor_mut().set_prefix("\n");
        doc.insert("backlog", Item::Table(table));
    }
    doc["backlog"]["id_prefix"] = value(id);

    let rendered = doc.to_string();
    // Write through a symlink (a worktree's link to the canonical config) so
    // the rename replaces the real file, not the link.
    let real = std::fs::canonicalize(cfg_path).unwrap_or_else(|_| cfg_path.to_path_buf());
    let parent = real
        .parent()
        .ok_or_else(|| format!("refusing: {} has no parent", real.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("refusing: could not create {}: {err}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        real.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".into()),
        std::process::id()
    ));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(rendered.as_bytes())?;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &real)
    };
    write().map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("refusing: failed to write {}: {err}", real.display())
    })?;
    Ok(())
}

/// `project resolve <name>`: the machine door `role.py` calls.
fn cmd_resolve(args: &[String]) -> i32 {
    let Some(name) = args.first() else {
        eprintln!("usage: fno config project resolve <name>");
        return 2;
    };
    if args.len() > 1 {
        eprintln!(
            "fno config project resolve: unexpected argument {}",
            args[1]
        );
        return 2;
    }
    match load_registry().and_then(|map| resolve_in(&map, name)) {
        Ok(canonical) => {
            println!("{canonical}");
            0
        }
        Err(message) => {
            eprint!("{message}");
            1
        }
    }
}

/// Read the global registry: global fno config first, the legacy settings
/// sibling as a read-only fallback, both into one `{alias: canonical}` list
/// in file order.
fn load_registry() -> Result<Vec<(String, String)>, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let settings = home.as_ref().map(|h| h.join(".fno").join("config.toml"));
    let legacy = home.as_ref().map(|h| h.join(".fno").join("settings.yaml"));

    // Python chose the legacy file whenever config.toml was absent
    // (exists(), so an unreadable path still counts as chosen and degrades
    // to an empty registry with a warning) and refused only when NEITHER
    // existed, naming the config.toml path.
    let chosen: Option<(&PathBuf, bool)> = match (&settings, &legacy) {
        (Some(p), _) if p.exists() => Some((p, false)),
        (_, Some(l)) if l.exists() => Some((l, true)),
        _ => None,
    };
    let Some((path, is_yaml)) = chosen else {
        let shown = settings.unwrap_or_else(|| PathBuf::from("~/.fno/config.toml"));
        return Err(format!(
            "config.toml not found at {}; run 'fno config setup' or create ~/.fno/config.toml\n",
            shown.display()
        ));
    };

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!(
                "config file at {} could not be read: {err}; using defaults",
                path.display()
            );
            return Ok(Vec::new());
        }
    };
    if is_yaml {
        registry_from_yaml(&text, path)
    } else {
        registry_from_toml(&text, path)
    }
}

/// Canonical name for `name`, or the Python-leg error with the sorted
/// known canonical names.
fn resolve_in(map: &[(String, String)], name: &str) -> Result<String, String> {
    if let Some((_, canonical)) = map.iter().find(|(k, _)| k == name) {
        return Ok(canonical.clone());
    }
    let mut known: Vec<&str> = map.iter().map(|(_, c)| c.as_str()).collect();
    known.sort();
    known.dedup();
    let list = known
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "unknown project name '{name}'; known canonical names: [{list}]\n"
    ))
}

/// Build the alias list from a config.toml body, in document order. The
/// reader also lifts a `config = { work = ... }` wrapper (the Python reader
/// unwrapped both shapes), wrapper values winning nested conflicts.
fn registry_from_toml(text: &str, path: &Path) -> Result<Vec<(String, String)>, String> {
    let doc: DocumentMut = match text.parse() {
        Ok(doc) => doc,
        Err(err) => {
            eprintln!(
                "config file at {} failed to parse: {err}; using defaults",
                path.display()
            );
            return Ok(Vec::new());
        }
    };
    let mut map: Vec<(String, String)> = Vec::new();
    let Some(work) = merged_work_toml(&doc) else {
        return Ok(map);
    };
    let Some(workspaces) = work.get("workspaces").and_then(Item::as_table) else {
        return Ok(map);
    };
    for (_ws, ws_item) in workspaces.iter() {
        let Some(ws) = ws_item.as_table() else {
            continue;
        };
        let Some(projects) = ws.get("projects") else {
            continue;
        };
        if let Some(arr) = projects.as_array() {
            for project in arr.iter() {
                let get = |key: &str| {
                    project
                        .as_inline_table()
                        .and_then(|t| t.get(key))
                        .and_then(toml_edit::Value::as_str)
                        .map(str::to_owned)
                };
                register_lookup(&mut map, get)?;
            }
        } else if let Some(tables) = projects.as_array_of_tables() {
            for table in tables.iter() {
                let get = |key: &str| table.get(key).and_then(Item::as_str).map(str::to_owned);
                register_lookup(&mut map, get)?;
            }
        }
    }
    Ok(map)
}

/// The `work` table of a config.toml, a `config = { work = ... }` wrapper
/// lifted and merged over any top-level `work` (wrapper values win on
/// nested-table conflicts, lists and scalars replace wholesale), matching
/// the Python reader's unwrap. Both shapes arrive either as standard tables
/// or as inline ones (`work = { ... }`); tomllib saw no difference, so each
/// level is normalized to a standard table before the walk.
fn merged_work_toml(doc: &DocumentMut) -> Option<Table> {
    let top = item_work_table(doc.get("work"));
    let wrapped = doc
        .get("config")
        .and_then(|c| item_work_table(c.get("work")));
    match (top, wrapped) {
        (None, None) => None,
        (Some(one), None) | (None, Some(one)) => Some(one),
        (Some(base), Some(wins)) => Some(merge_tables(&base, &wins)),
    }
}

/// The `work` item as a standard table, an inline table converted (recursively,
/// so nested `{ ... }` levels walk like spread-out ones), or None for any
/// other kind (a non-table `work` degrades to no registry, as the Python
/// reader's isinstance gate did).
fn item_work_table(item: Option<&Item>) -> Option<Table> {
    match item? {
        Item::Table(table) => Some(inline_to_tables(table.clone())),
        Item::Value(toml_edit::Value::InlineTable(inline)) => {
            Some(inline_to_tables(inline.clone().into_table()))
        }
        _ => None,
    }
}

/// Convert every inline-table value in `table` into a standard table, down
/// the whole nesting; arrays of inline tables stay arrays (the projects walk
/// already reads both spellings).
fn inline_to_tables(mut table: Table) -> Table {
    for (_key, item) in table.iter_mut() {
        if let Item::Value(toml_edit::Value::InlineTable(inline)) = item {
            *item = Item::Table(inline_to_tables(inline.clone().into_table()));
        }
    }
    table
}

/// Recursive table merge with the Python reader's direction: `wins` replaces
/// non-table values wholesale and merges table values key by key.
fn merge_tables(base: &Table, wins: &Table) -> Table {
    let mut out = base.clone();
    for (key, item) in wins.iter() {
        let merged = match (out.get(key).and_then(Item::as_table), item.as_table()) {
            (Some(existing), Some(wins_table)) => Item::Table(merge_tables(existing, wins_table)),
            _ => item.clone(),
        };
        out.insert(key, merged);
    }
    out
}

/// Register one project entry from its key lookup, canonical first.
fn register_lookup<F>(map: &mut Vec<(String, String)>, get: F) -> Result<(), String>
where
    F: Fn(&str) -> Option<String>,
{
    let Some(canonical) = get("name").filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let short = get("short_name").filter(|s| !s.is_empty() && *s != canonical);
    register_pair(map, &canonical, short.as_deref())
}

/// Register canonical then short alias, refusing a cross-claim.
fn register_pair(
    map: &mut Vec<(String, String)>,
    canonical: &str,
    short: Option<&str>,
) -> Result<(), String> {
    register_one(map, canonical, canonical)?;
    if let Some(short) = short {
        register_one(map, short, canonical)?;
    }
    Ok(())
}

/// One alias registration; a cross-claim is the Python DuplicateShortName.
fn register_one(map: &mut Vec<(String, String)>, key: &str, canonical: &str) -> Result<(), String> {
    if let Some((_, existing)) = map.iter().find(|(k, _)| k == key) {
        if existing != canonical {
            return Err(format!(
                "short_name/name '{key}' is claimed by both '{existing}' and '{canonical}'; remove one of the duplicate aliases from settings.yaml"
            ));
        }
        return Ok(());
    }
    map.push((key.to_string(), canonical.to_string()));
    Ok(())
}

/// Build the alias list from a legacy settings.yaml body, in document order.
/// The `config:` wrapper is unwrapped first (wrapper values win on conflict),
/// mirroring the Python reader.
fn registry_from_yaml(text: &str, path: &Path) -> Result<Vec<(String, String)>, String> {
    let raw: serde_yaml_ng::Value = match serde_yaml_ng::from_str(text) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!(
                "config file at {} failed to parse: {err}; using defaults",
                path.display()
            );
            return Ok(Vec::new());
        }
    };
    let work = merged_work(&raw);
    let Some(workspaces) = work
        .as_ref()
        .and_then(|w| w.get("workspaces"))
        .filter(|w| w.is_mapping())
    else {
        return Ok(Vec::new());
    };
    let mut map: Vec<(String, String)> = Vec::new();
    let Some(pairs) = workspaces
        .as_mapping()
        .map(|m| m.iter().collect::<Vec<_>>())
    else {
        return Ok(map);
    };
    for (_ws, ws_data) in pairs {
        if !ws_data.is_mapping() {
            continue;
        }
        let Some(projects) = ws_data.get("projects").filter(|p| p.is_sequence()) else {
            continue;
        };
        let Some(seq) = projects.as_sequence() else {
            continue;
        };
        for project in seq {
            let get = |key: &str| {
                project
                    .as_mapping()
                    .and_then(|m| m.get(serde_yaml_ng::Value::String(key.into())))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            };
            register_lookup(&mut map, get)?;
        }
    }
    Ok(map)
}

/// The `work` block of a legacy settings.yaml, `config:` wrapper unwrapped
/// (wrapper values win; nested tables merge), or None when neither shape
/// carries one.
fn merged_work(raw: &serde_yaml_ng::Value) -> Option<serde_yaml_ng::Value> {
    let rest = raw.get("work");
    let wrapped = raw.get("config").and_then(|c| c.get("work"));
    match (rest, wrapped) {
        (None, None) => None,
        (Some(one), None) | (None, Some(one)) => Some(one.clone()),
        (Some(base), Some(wins)) => Some(merge_yaml(base, wins)),
    }
}

fn merge_yaml(base: &serde_yaml_ng::Value, wins: &serde_yaml_ng::Value) -> serde_yaml_ng::Value {
    match (base.as_mapping(), wins.as_mapping()) {
        (Some(base_map), Some(wins_map)) => {
            let mut out = base_map.clone();
            for (k, v) in wins_map {
                let merged = match out.get(k) {
                    Some(existing) => merge_yaml(existing, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            serde_yaml_ng::Value::Mapping(out)
        }
        _ => wins.clone(),
    }
}

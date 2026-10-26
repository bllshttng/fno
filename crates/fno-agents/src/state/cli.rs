//! The `fno do state` command bodies, ported from `cli/src/fno/state/cli.py`
//!. One op per subcommand: `show`, `set`, `init`, `archive`, served
//! through the front door's lexical claim with the same argv shape.
//!
//! Contract notes, pinned by `tests/golden/target_family/`:
//!
//! - `show` renders the frontmatter as `yaml.safe_dump(sorted)` with the
//!   trailing newline stripped; `--field NAME` prints `str(value)` alone;
//!   `--json` prints one compact-ish JSON object (`json.dumps` separators,
//!   re-compacted by the goldens' normalizer).
//! - `set` enforces the control-plane write-once rule: on an IMMUTABLE
//!   manifest (target type, no `status:` field) only first-fill of an empty
//!   `plan_path` is legal; every other write exits 5 with the Python message
//!   verbatim. A successful first-fill rewrites only the one line it owns, so
//!   the manifest bytes outside that line never move.
//! - `init` refuses a bare target bootstrap (redirect to `fno do target
//!   init`, exit 2) unless `--allow-stub` or an explicit `--output` says
//!   otherwise, then writes a minimal default stub the stop hook archives.
//!   The Python stub carried the full pydantic default set; this writer
//!   emits the core fields only, a documented narrowing (a stub's content
//!   has no reader: the archiver renames it).
//! - `archive` renames to `<stem>.archived.<epoch>.md` and prints both paths.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

/// The exit code every typer-level usage error carries.
const USAGE_ERROR: i32 = 2;
/// The control-plane write-once refusal.
const IMMUTABLE_EXIT: i32 = 5;

const IMMUTABLE_MANIFEST_MSG: &str = "target-state.md is an immutable session manifest (control-plane collapse); only first-fill of plan_path is allowed";

/// Split a state file into `(frontmatter_map, body)` the way
/// `fno.state.io.read_frontmatter` does: `---` fence, YAML-ish `key: value`
/// lines, body after the closing fence. Returns `None` when the file is
/// missing (the caller prints the Python `error:` line and exits 1).
pub fn read_frontmatter(path: &Path) -> Option<(BTreeMap<String, String>, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse_frontmatter_text(&text))
}

pub fn parse_frontmatter_text(text: &str) -> (BTreeMap<String, String>, String) {
    let mut map = BTreeMap::new();
    if !text.starts_with("---") {
        return (map, text.to_string());
    }
    let rest = text.strip_prefix("---").unwrap_or("");
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = match rest.find("\n---") {
        Some(idx) => idx,
        None => return (map, text.to_string()),
    };
    let block = &rest[..end];
    let mut body = &rest[end + 4..];
    body = body.strip_prefix('\r').unwrap_or(body);
    body = body.strip_prefix('\n').unwrap_or(body);
    for line in block.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            map.insert(key.trim().to_string(), unquote(value));
        }
    }
    (map, body.to_string())
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('\'') && value.ends_with('\''))
            || (value.starts_with('"') && value.ends_with('"')))
    {
        let inner = &value[1..value.len() - 1];
        return inner.replace("''", "'");
    }
    value.to_string()
}

/// Render one scalar the way `yaml.safe_dump` would for the string maps this
/// module reads and writes: plain when unambiguous, single-quoted (with `''`
/// doubling) otherwise.
fn yaml_scalar(value: &str) -> String {
    let needs_quote = value.is_empty()
        || value.starts_with(' ')
        || value.ends_with(' ')
        || value.contains(": ")
        || value.contains(" #")
        || value.contains('\n')
        || value.starts_with('&')
        || value.starts_with('*')
        || value.starts_with('!')
        || value.starts_with('|')
        || value.starts_with('>')
        || value.starts_with('%')
        || value.starts_with('@')
        || value.starts_with('`')
        || value.starts_with('"')
        || value.starts_with('\'')
        || value.starts_with('[')
        || value.starts_with(']')
        || value.starts_with('{')
        || value.starts_with('}')
        || value.starts_with('-')
        || value.starts_with('?')
        || value.starts_with(',')
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "" | "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off"
        )
        || value.parse::<f64>().is_ok();
    if !needs_quote {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

/// Serialize a frontmatter map the way the Python `write_frontmatter` did:
/// sorted keys, `key: value` lines between `---` fences, then the body.
pub fn serialize_frontmatter(map: &BTreeMap<String, String>, body: &str) -> String {
    let mut out = String::from("---\n");
    for (key, value) in map {
        out.push_str(key);
        out.push_str(": ");
        out.push_str(&yaml_scalar(value));
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

/// Atomic replace: temp file in the same directory, fsync, rename. The
/// calling command holds the `<manifest>.lock` flock across the whole
/// read-check-write transaction (the Python `atomic_write` flocked the same
/// path), so a reader sees the old or the new file, never torn, and two
/// writers never interleave.
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
        std::process::id()
    ));
    for attempt in 0.. {
        if !tmp.exists() {
            break;
        }
        tmp = parent.join(format!(
            ".{}.{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("state"),
            std::process::id(),
            attempt
        ));
    }
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn resolve_repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("FNO_REPO_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = cwd.as_path();
    while let Some(parent) = dir.parent() {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        dir = parent;
    }
    cwd
}

/// Resolve the state file: explicit `--path` wins; otherwise the one
/// `target-state` resolver (the worktree space manifest, falling back to the
/// pre-space checkout manifest) for the target type, or the typed file
/// beside it.
fn resolve_state_path(path: Option<&str>, type_: Option<&str>) -> PathBuf {
    if let Some(path) = path {
        return PathBuf::from(path);
    }
    let root = resolve_repo_root();
    if type_.is_none() || type_ == Some("target") {
        return crate::state_path::resolve("target-state", &root).unwrap_or_default();
    }
    root.join(".fno")
        .join(format!("{}-state.md", type_.unwrap_or("target")))
}

fn emit_json(map: &BTreeMap<String, String>) {
    // The goldens re-compact json.dumps output, so key order follows the
    // sort the normalizer applied: BTreeMap order.
    let body: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{}:{}", json_string(k), json_string(v)))
        .collect();
    println!("{{{}}}", body.join(","));
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn usage_fail(message: &str) -> ! {
    eprintln!("{message}");
    exit(USAGE_ERROR);
}

/// `fno-agents state-cli show [--path P] [--type T] [--field F] [--json]`
pub fn run_show(args: &[String]) -> i32 {
    let mut path = None;
    let mut type_ = None;
    let mut field = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--path" => path = it.next().map(String::as_str),
            "--type" => type_ = it.next().map(String::as_str),
            "--field" => field = it.next().map(String::as_str),
            "--json" | "-J" => json = true,
            other => usage_fail(&format!("state show: unknown argument {other:?}")),
        }
    }
    let state_path = resolve_state_path(path, type_);
    let (map, _body) = match read_frontmatter(&state_path) {
        Some(pair) => pair,
        None => {
            eprintln!("error: state file not found: {}", state_path.display());
            return 1;
        }
    };
    if let Some(field) = field {
        match map.get(field) {
            Some(value) if json => {
                println!(
                    "{{{}}}",
                    format!("{}:{}", json_string(field), json_string(value))
                );
            }
            Some(value) => println!("{value}"),
            None => println!(""),
        }
        return 0;
    }
    if json {
        emit_json(&map);
    } else {
        let rendered = serialize_frontmatter(&map, "");
        let body = rendered
            .trim_start_matches("---\n")
            .trim_end_matches("---\n")
            .trim_end_matches('\n');
        println!("{body}");
    }
    0
}

/// `fno-agents state-cli set --field F --value V [--path P] [--type T]`
pub fn run_set(args: &[String]) -> i32 {
    let mut path = None;
    let mut type_ = None;
    let mut field = String::new();
    let mut value = String::new();
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--path" => path = it.next().map(String::as_str),
            "--type" => type_ = it.next().map(String::as_str),
            "--field" => field = it.next().cloned().unwrap_or_default(),
            "--value" => value = it.next().cloned().unwrap_or_default(),
            "--json" | "-J" => json = true,
            "--provider" => {
                it.next();
            }
            other => usage_fail(&format!("state set: unknown argument {other:?}")),
        }
    }
    if field.is_empty() {
        usage_fail("state set: --field is required");
    }
    let state_path = resolve_state_path(path, type_);
    // The read-check-write transaction holds the same `<manifest>.lock` flock
    // the claim handover's refresh takes (`claim_cli::manifest_refresh`):
    // atomic rename prevents torn reads, not lost updates, and a concurrent
    // refresh racing a first-fill must not erase either side's write. A
    // contended lock is a failed write, not a race.
    let _lock = match crate::claim_cli::manifest_refresh::lock_manifest(&state_path) {
        Some(lock) => lock,
        None => {
            eprintln!("error: state write failed: manifest stayed locked");
            return 1;
        }
    };
    let (mut map, body) = match read_frontmatter(&state_path) {
        Some(pair) => pair,
        None => {
            eprintln!("error: state file not found: {}", state_path.display());
            return 1;
        }
    };
    let detected = type_
        .map(String::from)
        .unwrap_or_else(|| "target".to_string());
    let is_immutable_target = detected == "target" && !map.contains_key("status");
    if is_immutable_target {
        if field != "plan_path" {
            eprintln!("{IMMUTABLE_MANIFEST_MSG}");
            return IMMUTABLE_EXIT;
        }
        let current = map.get("plan_path").map(String::as_str).unwrap_or("");
        let empty = matches!(current.trim(), "" | "\"\"" | "''");
        if !empty {
            eprintln!("{IMMUTABLE_MANIFEST_MSG}");
            return IMMUTABLE_EXIT;
        }
    }
    // Coerce to the existing field's scalar type when one is present.
    let coerced = coerce_value(&value, map.get(&field).map(String::as_str));
    if is_immutable_target {
        // First-fill of an immutable manifest rewrites only the line it owns,
        // so every byte outside `plan_path:` stays exactly as init wrote it.
        if let Err(e) = rewrite_single_field(&state_path, &field, &coerced) {
            eprintln!("error: state write failed: {e}");
            return 1;
        }
    } else {
        map.insert(field.clone(), coerced.clone());
        if let Err(e) = atomic_write(&state_path, &serialize_frontmatter(&map, &body)) {
            eprintln!("error: state write failed: {e}");
            return 1;
        }
    }
    if json {
        println!(
            "{{{}:{},{}:{}}}",
            json_string("field"),
            json_string(&field),
            json_string("value"),
            json_string(&coerced)
        );
    } else {
        println!("set {field} = '{}'", coerced.replace('\'', "''"));
    }
    0
}

fn coerce_value(value: &str, existing: Option<&str>) -> String {
    // The Python layer coerced to the existing field's int/float/bool type;
    // the manifest fields this command legally writes are strings, and the
    // immutable path is the only one the control plane exercises. Keep the
    // string form; a typed coercion of legacy files goes with the legacy
    // file format itself.
    let _ = existing;
    value.to_string()
}

/// Replace exactly one `key: value` line in the fenced frontmatter, leaving
/// every other byte (frontmatter order, body, blank lines) untouched.
fn rewrite_single_field(path: &Path, field: &str, value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut out = String::with_capacity(text.len() + value.len());
    let mut in_frontmatter = false;
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        if line.trim_end() == "---" {
            in_frontmatter = !in_frontmatter;
            out.push_str(line);
            continue;
        }
        if in_frontmatter && !replaced {
            if let Some((key, _)) = line.split_once(':') {
                if key.trim() == field {
                    out.push_str(&format!("{field}: {}\n", yaml_scalar(value)));
                    replaced = true;
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    atomic_write(path, &out)
}

/// `fno-agents state-cli init [--type T] [--output P] [--force] [--allow-stub]`
pub fn run_init(args: &[String]) -> i32 {
    let mut type_ = "target".to_string();
    let mut output: Option<PathBuf> = None;
    let mut force = false;
    let mut allow_stub = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--type" => type_ = it.next().cloned().unwrap_or_else(|| "target".into()),
            "--output" => output = it.next().map(PathBuf::from),
            "--force" | "-F" => force = true,
            "--allow-stub" => allow_stub = true,
            other => usage_fail(&format!("state init: unknown argument {other:?}")),
        }
    }
    if type_ == "target" && output.is_none() && !allow_stub {
        eprintln!(
            "target sessions must bootstrap with 'fno do target init' \
             (records input/plan_path/owner_cwd binding); 'fno do state init' \
             writes a stub the stop hook will archive."
        );
        return USAGE_ERROR;
    }
    let out_path = output.unwrap_or_else(|| {
        let root = resolve_repo_root();
        root.join(".fno").join(format!("{type_}-state.md"))
    });
    if out_path.exists() && !force {
        eprintln!(
            "error: {} already exists (use --force to overwrite)",
            out_path.display()
        );
        return 1;
    }
    let _lock = match crate::claim_cli::manifest_refresh::lock_manifest(&out_path) {
        Some(lock) => lock,
        None => {
            eprintln!("error: state write failed: manifest stayed locked");
            return 1;
        }
    };
    let mut defaults = BTreeMap::new();
    defaults.insert("fno_id".to_string(), String::new());
    defaults.insert("harness".to_string(), String::new());
    defaults.insert("harness_session_id".to_string(), String::new());
    defaults.insert("input".to_string(), String::new());
    defaults.insert("owner_cwd".to_string(), String::new());
    defaults.insert("plan_path".to_string(), String::new());
    defaults.insert("status".to_string(), String::new());
    if let Err(e) = atomic_write(&out_path, &serialize_frontmatter(&defaults, "")) {
        eprintln!("error: state write failed: {e}");
        return 1;
    }
    println!("created: {}", display_relative(&out_path));
    0
}

fn display_relative(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    path.strip_prefix(&cwd)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string_lossy().to_string())
}

/// `fno-agents state-cli archive [--path P] [--type T]`
pub fn run_archive(args: &[String]) -> i32 {
    let mut path = None;
    let mut type_ = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--path" => path = it.next().map(String::as_str),
            "--type" => type_ = it.next().map(String::as_str),
            "--json" | "-J" => json = true,
            other => usage_fail(&format!("state archive: unknown argument {other:?}")),
        }
    }
    let state_path = resolve_state_path(path, type_);
    if !state_path.exists() {
        eprintln!("error: {} does not exist", state_path.display());
        return 1;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let _lock = match crate::claim_cli::manifest_refresh::lock_manifest(&state_path) {
        Some(lock) => lock,
        None => {
            eprintln!("error: archive rename failed: manifest stayed locked");
            return 1;
        }
    };
    let stem = state_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("state");
    let archive_path = state_path.with_file_name(format!("{stem}.archived.{ts}.md"));
    if let Err(e) = std::fs::rename(&state_path, &archive_path) {
        eprintln!("error: archive rename failed: {e}");
        return 1;
    }
    if json {
        println!(
            "{{{}:{}}}",
            json_string("archived"),
            json_string(&archive_path.to_string_lossy())
        );
    } else {
        println!(
            "archived: {} -> {}",
            state_path.display(),
            archive_path.display()
        );
    }
    0
}

/// The whole `state-cli` door: `fno-agents state-cli <sub> <args...>`.
pub fn run(args: &[String]) -> i32 {
    // The front door's group-level `--json` arrives before the subcommand
    // (`do state --json show`); re-order it into the subcommand's own args,
    // which accept it anywhere.
    if matches!(
        args.first().map(String::as_str),
        Some("--json") | Some("-J")
    ) && args.len() >= 2
    {
        let mut reordered: Vec<String> = Vec::with_capacity(args.len());
        reordered.push(args[1].clone());
        reordered.push("--json".to_string());
        reordered.extend(args[2..].iter().cloned());
        return run(&reordered);
    }
    let (sub, rest) = match args.split_first() {
        Some(pair) => pair,
        None => usage_fail("state-cli: a subcommand is required: show|set|init|archive"),
    };
    // A --help that reaches the door prints the native usage line: the
    // Python group help died with the wheel.
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: fno do state show|set|init|archive [--path <file>] [--field <name>] [--value <v>] [--type <t>] [--allow-stub] [--force] [--json]");
        return 0;
    }
    match sub.as_str() {
        "show" => run_show(rest),
        "set" => run_set(rest),
        "init" => run_init(rest),
        "archive" => run_archive(rest),
        other => usage_fail(&format!(
            "state-cli: unknown subcommand {other:?} (show|set|init|archive)"
        )),
    }
}

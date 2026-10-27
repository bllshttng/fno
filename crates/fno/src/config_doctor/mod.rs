//! `fno config doctor`: the path diagnostic, its reporters and the post-merge
//! oracle, answered natively on the front door.
//!
//! Bare `fno config doctor` runs the path diagnostic ([`checks`]) and then the
//! advisory reporters ([`reporters`]); `--post-merge [--json]` prints the
//! [`post_merge`] verdict; `--review` reports reviewer capability. Read-only:
//! the one write is the claim-store probe, which creates and removes one temp
//! file. Config comes from the one native reader in `crate::config_cli`; the
//! schema walk ([`schema_walk`]) reads the committed `config_schema.json`.
//! Reads the fno-agents binary owns (route slots, the harness roster, review
//! capability, spawn overlays) ride [`agents_call`], and an unreachable binary
//! degrades each one the way the Python leg did.

pub mod checks;
pub mod post_merge;
pub mod reporters;
pub mod schema_walk;

use serde_json::{Map, Value};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const USAGE: &str = "Usage: fno config doctor [OPTIONS]";

const HELP: &str = concat!(
    "Usage: fno config doctor [OPTIONS]\n",
    "\n",
    "  Inspect resolved paths; flag suspicious values. Read-only.\n",
    "\n",
    "  With ``--post-merge`` (or ``--json``), instead report whether\n",
    "  ``config.post_merge.parking_lot_path`` is set for this repo - the gate the\n",
    "  /fno:ship pr merged ritual needs. With ``--review``, report whether every\n",
    "  configured local reviewer can run in this session. Bare ``fno config doctor``\n",
    "  runs the path diagnostic and appends a one-line post-merge readiness summary\n",
    "  plus the resolved ship gates (each probe with its source, each reviewer with\n",
    "  the rung it asserts).\n",
    "\n",
    "Options:\n",
    "  --post-merge  Report this repo's post-merge config readiness (read-only).\n",
    "  --review      Report whether every config.review.reviewers entry can actually\n",
    "                run in this session (read-only). Diagnostic twin of the refusal\n",
    "                `fno do target init` performs.\n",
    "  -J, --json    With --post-merge, emit the verdict as a single JSON object.\n",
    "  --help        Show this message and exit.\n",
);

/// The door: argv after `config doctor`.
pub fn run(args: &[OsString]) -> i32 {
    let mut post_merge = false;
    let mut review = false;
    let mut json_out = false;
    for arg in args {
        let word = arg.to_string_lossy();
        match word.as_ref() {
            "--help" => {
                print!("{HELP}");
                return 0;
            }
            "--post-merge" => post_merge = true,
            "--review" => review = true,
            "--json" | "-J" => json_out = true,
            other if other.starts_with('-') => {
                return usage_error(&format!("No such option: {other}"));
            }
            other => return usage_error(&format!("Got unexpected extra argument ({other})")),
        }
    }
    let mut out = String::new();
    let code = if review {
        match reporters::review_capability(json_out, &mut out) {
            Ok(code) => code,
            Err(err) => {
                flush(&out);
                eprintln!("fno config doctor --review: {err}");
                return 1;
            }
        }
    } else if post_merge || json_out {
        let verdict = post_merge::readiness(&post_merge::repo_root());
        if json_out {
            out.push_str(&verdict.to_json());
        } else {
            out.push_str(&verdict.summary_line());
        }
        out.push('\n');
        0
    } else {
        bare(&mut out)
    };
    flush(&out);
    code
}

/// Bare `fno config doctor` into `out`: the diagnostic's exit code, with every
/// advisory reporter appended. A reporter never changes the code.
pub fn bare(out: &mut String) -> i32 {
    let rc = checks::run_doctor(out);
    out.push_str(&post_merge::readiness(&post_merge::repo_root()).summary_line());
    out.push('\n');
    reporters::gates(out);
    reporters::state_roots(out);
    reporters::deprecated_auto_merge(out);
    reporters::deprecated_dispatch_harness(out);
    reporters::deprecated_active_backlog_mission(out);
    reporters::band_routing(out);
    reporters::harness_overlays(out);
    rc
}

/// The setup verify's in-process read: Ok with the report when clean, else
/// the same `` `fno config doctor` exited N: <report> `` detail the child
/// spawn produced.
pub fn verify() -> Result<String, String> {
    let mut out = String::new();
    match bare(&mut out) {
        0 => Ok(out),
        rc => Err(format!("`fno config doctor` exited {rc}: {}", out.trim())),
    }
}

fn flush(out: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

fn usage_error(message: &str) -> i32 {
    eprintln!("{USAGE}");
    eprintln!("Try 'fno config doctor --help' for help.");
    eprintln!();
    eprintln!("Error: {message}");
    2
}

// ---------------------------------------------------------------------------
// The fno-agents round trip
// ---------------------------------------------------------------------------

/// One `fno-agents <command...>` round trip: the JSON payload on stdin (when
/// given), the parsed JSON answer out. The error strings are the ones the
/// Python transport raised, so a reporter that prints one prints the same
/// line.
pub(crate) fn agents_call(command: &[&str], payload: Option<&Value>) -> Result<Value, String> {
    let display = command.join(" ");
    let Some(binary) = crate::digest_overlay::fno_agents_bin_auto() else {
        return Err("the fno-agents binary was not found; reinstall fno, run `fno doctor update --rust`, or set FNO_AGENTS_BIN".to_string());
    };
    let mut cmd = std::process::Command::new(&binary);
    cmd.args(command)
        .stdin(if payload.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("fno-agents {display} failed: {e}"))?;
    if let Some(payload) = payload {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.to_string().as_bytes());
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("fno-agents {display} failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr.trim().chars().take(200).collect();
        return Err(format!(
            "fno-agents {display} exited {}: {detail}",
            output.status.code().unwrap_or(-1)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("fno-agents {display} bad output: {e}"))
}

// ---------------------------------------------------------------------------
// Settings: the merged tree, read with schema defaults
// ---------------------------------------------------------------------------

/// The merged config the loader would validate, plus the committed schema
/// table for defaults. One read per doctor run is enough; each reporter
/// re-reads because a reporter must stand alone.
pub(crate) struct Settings {
    pub raw: Value,
    table: Value,
}

impl Settings {
    pub fn load() -> Settings {
        Settings {
            raw: crate::config_cli::merged_config(&crate::config_cli::repo_root()),
            table: crate::config_schema::schema_table(),
        }
    }

    /// The value at a dotted leaf: the merged config's, else the schema
    /// default, else null.
    pub fn get(&self, dotted: &str) -> Value {
        if let Some(value) = crate::config_cli::raw_get(&self.raw, dotted) {
            if !value.is_null() {
                return value.clone();
            }
        }
        self.table["leaves"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["path"].as_str() == Some(dotted)))
            .map(|row| row["default"].clone())
            .unwrap_or(Value::Null)
    }

    pub fn str_list(&self, dotted: &str) -> Vec<String> {
        match self.get(dotted) {
            Value::Array(items) => items.iter().map(py_str).collect(),
            _ => Vec::new(),
        }
    }

    pub fn json_schema(&self) -> &Value {
        &self.table["json_schema"]
    }

    pub fn leaves(&self) -> Vec<String> {
        self.table["leaves"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row["path"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Paths: the `fno.paths` accessors the doctor prints
// ---------------------------------------------------------------------------

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// `os.path.expandvars`: `$NAME` and `${NAME}` for set variables; anything
/// else stays as written.
pub(crate) fn expandvars(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let braced = chars.get(i + 1) == Some(&'{');
        let start = if braced { i + 2 } else { i + 1 };
        let mut end = start;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        let closed = !braced || chars.get(end) == Some(&'}');
        let name: String = chars[start..end].iter().collect();
        match (name.is_empty() || !closed, std::env::var(&name)) {
            (false, Ok(value)) => {
                out.push_str(&value);
                i = if braced { end + 1 } else { end };
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// `os.path.expanduser` for `~` and `~/...`.
pub(crate) fn expanduser(raw: &str) -> String {
    let home = crate::config_cli::home_dir();
    if raw == "~" {
        return home.to_string_lossy().into_owned();
    }
    match raw.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home.to_string_lossy().trim_end_matches('/')),
        None => raw.to_string(),
    }
}

/// `Path(p).resolve()`: absolute against the cwd, symlinks collapsed.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    crate::config_cli::resolve_path(&absolute)
}

/// `fno.paths._resolve`: expandvars, the `{{`/`}}` unescape, expanduser,
/// then anchored at `anchor` (or the cwd) and resolved.
pub(crate) fn resolve_configured(raw: &str, anchor: Option<&Path>) -> PathBuf {
    let text = expanduser(&expandvars(raw).replace("{{", "{").replace("}}", "}"));
    let path = PathBuf::from(text);
    match anchor {
        Some(root) if !path.is_absolute() => resolve(&root.join(path)),
        _ => resolve(&path),
    }
}

pub(crate) fn state_dir(settings: &Settings) -> PathBuf {
    if let Some(carrier) = env_nonempty("FNO_STATE_DIR") {
        return resolve(Path::new(&expanduser(&carrier)));
    }
    let raw = settings.get("state_dir");
    resolve_configured(raw.as_str().unwrap_or("~/.fno/"), None)
}

fn override_path(settings: &Settings, key: &str) -> Option<PathBuf> {
    match crate::config_cli::raw_get(&settings.raw, key) {
        Some(Value::String(raw)) => Some(resolve_configured(raw, None)),
        _ => None,
    }
}

/// The `config.paths.<name>` override, else `<state_dir>/<leaf>`.
pub(crate) fn state_child(settings: &Settings, name: &str, leaf: &str) -> PathBuf {
    override_path(settings, &format!("paths.{name}"))
        .unwrap_or_else(|| state_dir(settings).join(leaf))
}

pub(crate) fn graph_json(settings: &Settings) -> PathBuf {
    let root = state_dir(settings);
    if root.join("db").join("graph.db").exists() {
        return root.join("db").join("graph.json");
    }
    if root.join("graph.db").exists() {
        return root.join("graph.json");
    }
    root.join("db").join("graph.json")
}

pub(crate) fn ledger_json(settings: &Settings) -> PathBuf {
    if env_nonempty("FNO_STATE_DIR").is_some() {
        return state_dir(settings).join("ledger.json");
    }
    let home_root = resolve_configured("~/.fno/", None);
    if let Some(Value::String(raw)) = crate::config_cli::raw_get(&settings.raw, "paths.ledger_json")
    {
        let expanded = expanduser(&expandvars(raw));
        if Path::new(&expanded).is_absolute() {
            return resolve_configured(raw, None);
        }
        return resolve_configured(raw, Some(&home_root));
    }
    let raw_state = settings.get("state_dir");
    let expanded = expanduser(&expandvars(raw_state.as_str().unwrap_or("~/.fno/")));
    if Path::new(&expanded).is_absolute() {
        return state_dir(settings).join("ledger.json");
    }
    home_root.join("ledger.json")
}

/// `resolve_canonical_repo_root`: the `FNO_REPO_ROOT` pin, else the main
/// worktree, else the checkout root.
pub(crate) fn canonical_root() -> PathBuf {
    if let Some(pin) = env_nonempty("FNO_REPO_ROOT") {
        return resolve(Path::new(&pin));
    }
    crate::config_cli::canonical_repo_root(&crate::config_cli::repo_root())
}

/// `_canonical_for`: a linked worktree's main checkout, else the root itself.
fn canonical_for(root: &Path) -> PathBuf {
    let root = resolve(root);
    let gitdir = root.join(".git");
    if !gitdir.exists() || gitdir.is_dir() {
        return root;
    }
    crate::config_cli::canonical_repo_root(&root)
}

fn spaces_root(settings: &Settings) -> PathBuf {
    if let Some(pin) = env_nonempty("FNO_SPACES_DIR") {
        return resolve(Path::new(&expanduser(&pin)));
    }
    override_path(settings, "paths.spaces_dir")
        .unwrap_or_else(|| state_dir(settings).join("spaces"))
}

fn slug(root: &Path) -> String {
    root.to_string_lossy().replace('/', "-")
}

pub(crate) fn space_dir(settings: &Settings) -> PathBuf {
    spaces_root(settings).join(slug(&canonical_root()))
}

pub(crate) fn worktree_space_dir(settings: &Settings) -> PathBuf {
    let root = crate::config_cli::repo_root();
    let canonical = canonical_for(&root);
    let space = spaces_root(settings).join(slug(&canonical));
    if resolve(&root) != canonical {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return space.join("worktrees").join(name);
    }
    space
}

pub(crate) fn agents_registry_path(settings: &Settings) -> PathBuf {
    match env_nonempty("FNO_AGENTS_HOME") {
        Some(home) => resolve(Path::new(&expanduser(&home))).join("registry.json"),
        None => state_dir(settings).join("agents").join("registry.json"),
    }
}

/// `global_claims_root()`: the `FNO_CLAIMS_ROOT` pin, else `$HOME`.
pub(crate) fn global_claims_dir() -> PathBuf {
    let root = env_nonempty("FNO_CLAIMS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(crate::config_cli::home_dir);
    root.join(".fno").join("claims")
}

pub(crate) fn bus_dir(settings: &Settings) -> PathBuf {
    if let Some(explicit) = env_nonempty("FNO_BUS_DIR") {
        return PathBuf::from(explicit);
    }
    if let Some(inbox) = env_nonempty("FNO_INBOX_ROOT") {
        return PathBuf::from(inbox).join(".bus");
    }
    override_path(settings, "paths.bus_dir").unwrap_or_else(|| state_dir(settings).join("bus"))
}

pub(crate) fn project_events_json(settings: &Settings) -> PathBuf {
    match env_nonempty("FNO_EVENTS_PATH") {
        Some(pin) => PathBuf::from(pin),
        None => space_dir(settings).join("events.jsonl"),
    }
}

/// `source_note`: `set in <file>` when a config file decides `key`.
pub(crate) fn source_note(key: &str) -> Option<String> {
    crate::config_cli::resolve_source(key, &crate::config_cli::repo_root())
        .map(|(decider, _)| format!("set in {}", decider.to_string_lossy()))
}

// ---------------------------------------------------------------------------
// Python spellings
// ---------------------------------------------------------------------------

/// Python `repr()` over a JSON value.
pub(crate) fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == f.trunc() && f.abs() < 1e16 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// Python `repr()` of a str: single quotes unless the text holds a single
/// quote and no double quote.
pub(crate) fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `str()`: a string raw, anything else its repr.
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python `type(v).__name__` for a parsed config value.
pub(crate) fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// An object's map, or an empty one.
pub(crate) fn obj(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[cfg(test)]
#[path = "config_doctor_tests.rs"]
mod tests;

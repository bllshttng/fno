//! `fno config schema`: every config artifact, rendered from the committed
//! schema table. `fno config setup plan` (setup_cmds) prints its wizard plan
//! through [`wizard_plan`].
//!
//! `config_schema.json` is generated from the pydantic `SettingsModel` plus
//! its presentation registry (`cd cli && uv run python -m
//! fno.config.schema_gen --write`), and the drift test regenerates it and
//! compares, so the model stays the one source. This module only renders:
//! the JSON Schema, the Markdown reference, the example TOML, and the wizard
//! plan, byte for byte as the Python generators printed them.

use serde_json::{Map, Value};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const TABLE: &str = include_str!("../config_schema.json");

/// Claim `config schema ...` lexically, before clap. Every other argv
/// forwards untouched.
pub fn classify(args: &[OsString]) -> Option<Vec<String>> {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return None;
    }
    match words.as_slice() {
        ["config", "schema", rest @ ..] => Some(rest.iter().map(|w| w.to_string()).collect()),
        _ => None,
    }
}

/// The wizard question plan as `json.dumps(..., indent=2)` printed it: the
/// `always` tier, or every asked field with `advanced`.
pub fn wizard_plan(advanced: bool) -> String {
    let plan = wizard_fields(&table(), |tier| advanced || tier == "always");
    py_dumps(&plan, Some(2), false)
}

/// One leaf row of the committed table.
struct Leaf<'a> {
    path: &'a str,
    type_str: &'a str,
    default: &'a Value,
    wizard: &'a str,
    question: &'a str,
    default_source: &'a str,
    doc: &'a str,
}

fn table() -> Value {
    serde_json::from_str(TABLE).expect("config_schema.json is generated valid JSON")
}

fn leaves(table: &Value) -> Vec<Leaf<'_>> {
    table["leaves"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| Leaf {
                    path: row["path"].as_str().unwrap_or(""),
                    type_str: row["type"].as_str().unwrap_or(""),
                    default: &row["default"],
                    wizard: row["wizard"].as_str().unwrap_or("never"),
                    question: row["question"].as_str().unwrap_or(""),
                    default_source: row["default_source"].as_str().unwrap_or(""),
                    doc: row["doc"].as_str().unwrap_or(""),
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Python json.dumps, byte for byte
// ---------------------------------------------------------------------------

/// A JSON string as `json.dumps` writes it under its default
/// `ensure_ascii=True`: serde's escapes for ASCII, `\uXXXX` (surrogate pairs
/// above the BMP) for the rest, and DEL escaped as Python escapes it.
fn py_str(s: &str) -> String {
    let compact = serde_json::to_string(s).unwrap_or_default();
    let mut out = String::with_capacity(compact.len());
    for c in compact.chars() {
        match c {
            '\u{7f}' => out.push_str("\\u007f"),
            c if (c as u32) < 0x80 => out.push(c),
            c if (c as u32) <= 0xFFFF => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let cp = c as u32 - 0x10000;
                let _ = write!(
                    out,
                    "\\u{:04x}\\u{:04x}",
                    0xD800 + (cp >> 10),
                    0xDC00 + (cp & 0x3FF)
                );
            }
        }
    }
    out
}

/// `json.dumps(v, indent=indent, sort_keys=sort)` with Python's separators:
/// `", "` and `": "` inline, `","` and `": "` once an indent is set.
fn py_dumps(v: &Value, indent: Option<usize>, sort: bool) -> String {
    let mut out = String::new();
    dump(v, indent, sort, 0, &mut out);
    out
}

fn dump(v: &Value, indent: Option<usize>, sort: bool, level: usize, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => out.push_str(&py_str(s)),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                separate(i, indent, level + 1, out);
                dump(item, indent, sort, level + 1, out);
            }
            close(indent, level, out);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            if sort {
                keys.sort();
            }
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                separate(i, indent, level + 1, out);
                out.push_str(&py_str(key));
                out.push_str(": ");
                dump(&map[key.as_str()], indent, sort, level + 1, out);
            }
            close(indent, level, out);
            out.push('}');
        }
    }
}

fn separate(i: usize, indent: Option<usize>, level: usize, out: &mut String) {
    match indent {
        Some(width) => {
            if i > 0 {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(width * level));
        }
        None => {
            if i > 0 {
                out.push_str(", ");
            }
        }
    }
}

fn close(indent: Option<usize>, level: usize, out: &mut String) {
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * level));
    }
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn render_json_schema(table: &Value) -> String {
    py_dumps(&table["json_schema"], Some(2), true)
}

fn wizard_fields(table: &Value, include: impl Fn(&str) -> bool) -> Value {
    let fields: Vec<Value> = leaves(table)
        .into_iter()
        .filter(|leaf| leaf.wizard != "never" && include(leaf.wizard))
        .map(|leaf| {
            let mut row = Map::new();
            row.insert("path".into(), Value::from(leaf.path));
            row.insert("type".into(), Value::from(leaf.type_str));
            row.insert("default".into(), leaf.default.clone());
            row.insert("tier".into(), Value::from(leaf.wizard));
            row.insert("question".into(), Value::from(leaf.question));
            row.insert("default_source".into(), Value::from(leaf.default_source));
            row.insert("doc".into(), Value::from(leaf.doc));
            Value::Object(row)
        })
        .collect();
    let mut doc = Map::new();
    doc.insert("fields".into(), Value::Array(fields));
    Value::Object(doc)
}

/// A default as the Markdown reference shows it.
fn fmt_default(v: &Value) -> String {
    match v {
        Value::Null => "_(none)_".to_string(),
        Value::Bool(b) => format!("`{b}`"),
        Value::Array(_) | Value::Object(_) => format!("`{}`", py_dumps(v, None, false)),
        Value::Number(n) => format!("`{n}`"),
        Value::String(s) => format!("`{s}`"),
    }
}

/// A literal `|` in a cell splits one row into extra columns.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

fn render_markdown(table: &Value) -> String {
    let mut lines: Vec<String> = vec![
        "# Configuration reference".into(),
        String::new(),
        "> Generated by `fno config schema --markdown` from the Pydantic `SettingsModel` (the single source of truth). Do not edit by hand; edit the model + `cli/src/fno/config/registry.py` and regenerate.".into(),
        String::new(),
        "Keys live in a flat `config.toml` (`.fno/config.toml` project-local, `~/.fno/config.toml` global). A dotted key like `branch.prefix` is `prefix` under `[branch]`. For a copy-paste template with every key at its default, see [config.example.toml](config.example.toml); read or set individual keys with `fno config get <key>` / `fno config set <key> <value>`.".into(),
        String::new(),
        "| Key | Type | Default | Wizard | Description |".into(),
        "|-----|------|---------|--------|-------------|".into(),
    ];
    for leaf in leaves(table) {
        lines.push(format!(
            "| `{}` | {} | {} | {} | {} |",
            leaf.path,
            cell(leaf.type_str),
            cell(&fmt_default(leaf.default)),
            cell(leaf.wizard),
            cell(leaf.doc)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// A default as a TOML value. TOML has no null, so a `None` field inside an
/// inline table is dropped; the callers emit a top-level `None` commented out.
fn toml_value(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_str(s),
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .filter(|(_, item)| !item.is_null())
                .map(|(key, item)| {
                    let bare = !key.is_empty()
                        && key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                    let key = if bare { key.clone() } else { py_str(key) };
                    format!("{key} = {}", toml_value(item))
                })
                .collect();
            if parts.is_empty() {
                "{}".to_string()
            } else {
                format!("{{ {} }}", parts.join(", "))
            }
        }
        Value::Array(_) | Value::Null => py_dumps(v, None, false),
    }
}

/// One table in the emit tree: direct leaves first, then child tables, in
/// first-seen order. TOML needs a table's keys before its sub-table headers,
/// and the model yields some top-level scalars after nested blocks.
#[derive(Default)]
struct Node<'a> {
    leaves: Vec<&'a Leaf<'a>>,
    tables: Vec<(String, Node<'a>)>,
}

impl<'a> Node<'a> {
    fn child(&mut self, seg: &str) -> &mut Node<'a> {
        let at = match self.tables.iter().position(|(name, _)| name == seg) {
            Some(at) => at,
            None => {
                self.tables.push((seg.to_string(), Node::default()));
                self.tables.len() - 1
            }
        };
        &mut self.tables[at].1
    }
}

fn emit(node: &Node<'_>, prefix: &mut Vec<String>, lines: &mut Vec<String>) {
    if !prefix.is_empty() && !node.leaves.is_empty() {
        lines.push(String::new());
        lines.push(format!("[{}]", prefix.join(".")));
    }
    for leaf in &node.leaves {
        if !leaf.doc.is_empty() {
            lines.push(format!(
                "# {}",
                leaf.doc.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
        let key = leaf.path.rsplit('.').next().unwrap_or(leaf.path);
        if leaf.default.is_null() {
            lines.push(format!("# {key} = <unset>   # optional; no default"));
        } else {
            lines.push(format!("{key} = {}", toml_value(leaf.default)));
        }
    }
    for (seg, child) in &node.tables {
        prefix.push(seg.clone());
        emit(child, prefix, lines);
        prefix.pop();
    }
}

fn render_example_toml(table: &Value) -> String {
    let rows = leaves(table);
    let mut root = Node::default();
    for leaf in &rows {
        let segs: Vec<&str> = leaf.path.split('.').collect();
        let mut node = &mut root;
        for seg in &segs[..segs.len() - 1] {
            node = node.child(seg);
        }
        node.leaves.push(leaf);
    }
    let mut lines: Vec<String> = [
        "# footnote config reference (config.example.toml)",
        "#",
        "# Generated by `fno config schema --toml` from the Pydantic SettingsModel",
        "# (the single source of truth). Do not edit by hand; edit the model +",
        "# cli/src/fno/config/registry.py and regenerate.",
        "#",
        "# Every key is shown set to its DEFAULT. Copy the keys you want to change",
        "# into .fno/config.toml (project-local) or ~/.fno/config.toml (global);",
        "# anything you omit falls back to these defaults. config.toml is flat:",
        "# top-level scalars and [blocks], no wrapper. Optional keys with no",
        "# default are shown commented out (TOML has no null).",
        "#",
        "# For opinionated starters (recommended values, safe opt-ins on), see",
        "# docs/config.global.example.toml and docs/config.local.example.toml.",
        "",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    emit(&root, &mut Vec::new(), &mut lines);
    lines.push(String::new());
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

const SCHEMA_USAGE: &str = "Usage: fno config schema [OPTIONS]

  Generate config artifacts from the model + registry.

  Exactly one of --json-schema / --markdown / --wizard-plan / --toml selects
  the output; --markdown is the default. --write regenerates the committed
  file atomically; --check compares it against the committed file and exits 2
  on drift.

Options:
  --json-schema  Emit the model's JSON Schema.
  --markdown     Emit the COMPLETE settings reference as Markdown.
  --wizard-plan  Emit the wizard-asked fields as JSON.
  --toml         Emit a commented example config.toml (every key at its default).
  --write        With --markdown/--toml: regenerate the committed reference file.
  --check        With --markdown/--toml: exit non-zero if the committed file differs.
  --help         Show this message and exit.
";

fn usage_error(usage: &str, message: &str) -> i32 {
    let first = usage.lines().next().unwrap_or("");
    eprintln!(
        "{first}\nTry '{} --help' for help.\n\nError: {message}",
        first
            .trim_start_matches("Usage: ")
            .trim_end_matches(" [OPTIONS]")
    );
    2
}

/// Git toplevel of the cwd (the repo the docs belong to), else the cwd.
fn repo_root() -> PathBuf {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !text.is_empty() {
            return PathBuf::from(text);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Temp file in the same dir, then rename, so a failed write never
/// truncates the committed file.
fn write_atomic(target: &Path, text: &str) -> std::io::Result<()> {
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

pub fn run(rest: &[String]) -> i32 {
    let (mut json_schema, mut markdown, mut wizard, mut toml, mut write, mut check) =
        (false, false, false, false, false, false);
    for arg in rest {
        match arg.as_str() {
            "--json-schema" => json_schema = true,
            "--markdown" => markdown = true,
            "--wizard-plan" => wizard = true,
            "--toml" => toml = true,
            "--write" => write = true,
            "--check" => check = true,
            "--help" | "-h" => {
                print!("{SCHEMA_USAGE}");
                return 0;
            }
            other => return usage_error(SCHEMA_USAGE, &format!("No such option: {other}")),
        }
    }
    if [json_schema, markdown, wizard, toml]
        .iter()
        .filter(|b| **b)
        .count()
        > 1
    {
        eprintln!("error: pick at most one of --json-schema / --markdown / --wizard-plan / --toml");
        return 2;
    }
    let table = table();
    if json_schema {
        println!("{}", render_json_schema(&table));
        return 0;
    }
    if wizard {
        println!(
            "{}",
            py_dumps(&wizard_fields(&table, |_| true), Some(2), false)
        );
        return 0;
    }
    let root = repo_root();
    let (rendered, targets, regen) = if toml {
        (
            render_example_toml(&table),
            // The front door embeds the crate-local copy: cargo publish packs
            // only the crate, so the docs/ copy cannot be include_str!'d.
            vec![
                root.join("docs/config.example.toml"),
                root.join("crates/fno/config.example.toml"),
            ],
            "fno config schema --toml --write",
        )
    } else {
        (
            render_markdown(&table),
            vec![root.join("docs/configuration-guide.md")],
            "fno config schema --markdown --write",
        )
    };
    if check {
        for target in &targets {
            if std::fs::read_to_string(target).ok().as_deref() != Some(rendered.as_str()) {
                eprintln!("error: {} is stale; run `{regen}`", target.display());
                return 2;
            }
        }
        println!("{} is up to date", targets[0].display());
        return 0;
    }
    if write {
        for target in &targets {
            if let Err(e) = write_atomic(target, &rendered) {
                eprintln!("error: could not write {}: {e}", target.display());
                return 1;
            }
            println!("wrote {}", target.display());
        }
        return 0;
    }
    println!("{rendered}");
    0
}

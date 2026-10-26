//! `executor-route`: the locked-decision parse and the frontend-surface
//! match, the one native owner behind the `fno-agents executor-route`
//! transport door. Ported byte-for-byte from the Python legs
//! (`fno.executor._locked`, `fno.executor._surface`) the same change deletes;
//! the goldens under `tests/golden/executor_{locked,surface}/` freeze the old
//! legs' answers and the parity tests drive the door against them.
//!
//! Direct dispatch, daemon-free: bash callers (infer-has-ui.sh,
//! resolve-executor.sh, resolve-plan-executor.sh and the blueprint-gates.md
//! instructions) exec HERE through the shared binary resolver. The
//! stdin/stdout contracts are the Python modules' byte for byte; an empty
//! answer prints nothing and every valid mode exits 0, and a missing or
//! unknown mode prints usage to stderr and exits 2, the surface-check
//! convention.

use regex::Regex;
use std::io::Read as _;
use std::sync::OnceLock;

const USAGE: &str =
    "usage: fno-agents executor-route locked [--key model] | surface [--has-ui]  (stdin)\n";

const CANONICAL: [&str; 3] = ["tdd", "impeccable", "mixed"];

fn h2_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^##[ \t]").unwrap())
}

fn locked_heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^##[ \t]+locked[ \t]+decisions").unwrap())
}

fn executor_kv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)executor[ \t]*:[ \t]*\**[ \t]*[a-zA-Z_-]+").unwrap())
}

fn executor_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^executor[ \t]*:[ \t]*\**[ \t]*").unwrap())
}

fn override_shape_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"overrid(?:e|es|ing)\b|per-task\b").unwrap())
}

fn list_marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[ \t]*([0-9]+\.|[-*])[ \t]*").unwrap())
}

fn leading_bold_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\*\*").unwrap())
}

fn routing_header_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\*?\*?executor[ \t]+routing\*?\*?[ \t]*:").unwrap())
}

fn new_list_item_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[ \t]*([0-9]+\.|[-*])[ \t]+\*?\*?").unwrap())
}

fn bare_executor_entry_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^[ \t]*(?:[0-9]+\.|[-*])?[ \t]*\**executor\**[ \t]*:").unwrap()
    })
}

fn model_kv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?im)^\s*(?:[0-9]+[.][ \t]+|[-*][ \t]+)?\**model\**[ \t]*:[ \t]*\**[ \t]*(.+)")
            .unwrap()
    })
}

fn model_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9._:/-]{1,64}$").unwrap())
}

fn model_provenance_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[ \t]*\([^)]*\)[ \t]*$").unwrap())
}

/// The body of the `## Locked Decisions` section, or "". Reproduces the awk
/// pass: scan lines; on the first `## ` heading whose lowercased form matches
/// the locked-decisions heading, start including subsequent lines (the
/// heading line itself is consumed); stop at the next `## ` heading or EOF.
fn extract_section(text: &str) -> String {
    let mut inside = false;
    let mut collected: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if h2_re().is_match(line) {
            if inside {
                break;
            }
            if locked_heading_re().is_match(&line.to_lowercase()) {
                inside = true;
                continue;
            }
        }
        if inside {
            collected.push(line);
        }
    }
    collected.join("\n")
}

/// Resolve one buffered entry's `executor:<value>` to a canonical value, or
/// "". The documented mixed shape (plan-level `executor: tdd` with per-task
/// overrides `executor: impeccable`) resolves to `mixed`, because taking the
/// last value there would route the whole plan through the frontend
/// pipeline, the more expensive of the two mistakes. Two canonical values
/// alone do NOT prove that shape: a decision that names its rejected option
/// or its history also carries two values while having plainly chosen one.
/// Require the override wording the real shape always states; otherwise
/// last-wins preserves the stated choice.
fn extract_value(block: &str) -> String {
    let stripped = block.replace('`', "");
    let values: Vec<String> = executor_kv_re()
        .find_iter(&stripped)
        .map(|m| {
            let v = executor_prefix_re().replace(m.as_str(), "").to_lowercase();
            if v == "do" {
                "tdd".to_string()
            } else {
                v
            }
        })
        .collect();
    if values.is_empty() {
        return String::new();
    }
    let distinct_canonical: std::collections::HashSet<&String> = values
        .iter()
        .filter(|v| CANONICAL.contains(&v.as_str()))
        .collect();
    if distinct_canonical.len() > 1 && override_shape_re().is_match(&stripped) {
        return "mixed".to_string();
    }
    let last = values.last().unwrap();
    if CANONICAL.contains(&last.as_str()) {
        last.clone()
    } else {
        String::new()
    }
}

/// True when `line` opens a locked executor entry: the "Executor routing"
/// head /think is prompted to emit, or a bare `Executor:` decision naming
/// the value directly.
fn is_entry_head(line: &str) -> bool {
    if bare_executor_entry_re().is_match(line) {
        return true;
    }
    let stripped = list_marker_re().replace(line, "");
    let stripped = leading_bold_re().replace(&stripped, "");
    routing_header_re().is_match(&stripped)
}

/// Parse the locked executor decision from design-doc `text`:
/// "" | "tdd" | "impeccable" | "mixed". `do` input normalizes to `tdd`.
pub fn parse_locked_executor(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let section = extract_section(text);
    if section.is_empty() {
        return String::new();
    }
    // The bash `read <<< "$SECTION"` iterated the section line by line and a
    // here-string added a trailing newline, so the loop never saw a phantom
    // final line; splitting on \n and dropping ONE trailing empty reproduces
    // that.
    let mut lines: Vec<&str> = section.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let mut last_value = String::new();
    let mut buffer = String::new();
    for line in lines {
        if is_entry_head(line) {
            if !buffer.is_empty() {
                let v = extract_value(&buffer);
                if !v.is_empty() {
                    last_value = v;
                }
            }
            buffer.clear();
            buffer.push_str(line);
            continue;
        }
        if !buffer.is_empty() {
            // A blank line (whitespace-only counts) closes the entry.
            if line.trim().is_empty() {
                let v = extract_value(&buffer);
                if !v.is_empty() {
                    last_value = v;
                }
                buffer.clear();
                continue;
            }
            // A new list item begins; close out the previous entry, then
            // re-evaluate this line in case it is another routing header.
            if new_list_item_re().is_match(line) {
                let v = extract_value(&buffer);
                if !v.is_empty() {
                    last_value = v;
                }
                buffer.clear();
                if is_entry_head(line) {
                    buffer.push_str(line);
                }
                continue;
            }
            buffer.push('\n');
            buffer.push_str(line);
        }
    }
    if !buffer.is_empty() {
        let v = extract_value(&buffer);
        if !v.is_empty() {
            last_value = v;
        }
    }
    last_value
}

/// Parse a locked `Model:` decision from design-doc `text`. Scans the
/// `## Locked Decisions` section for the LAST `Model: <value>` entry and
/// returns the value when it is one shell-safe token of <= 64 chars, else ""
/// (a multi-token or metacharacter value is REJECTED, not truncated). No
/// allowlist: aliases and full provider-model ids pass through verbatim.
pub fn parse_locked_model(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    // Strip CR first: on a CRLF checkout every line ends with \r, which the
    // KV regex's `.` captures into the value and the token check then
    // rejects, so a valid pin would be lost.
    let section = extract_section(&text.replace('\r', ""));
    if section.is_empty() {
        return String::new();
    }
    // A model token never contains a backtick; strip them so
    // ``Model: `fable` `` normalizes before the scan. Bold `*` around the
    // key are consumed by the regex, not stripped, so a value's own `*`
    // survives to be rejected.
    let cleaned = section.replace('`', "");
    let matches: Vec<String> = model_kv_re()
        .captures_iter(&cleaned)
        .filter_map(|c| c.get(1).map(|g| g.as_str().to_string()))
        .collect();
    let Some(val_raw) = matches.last() else {
        return String::new();
    };
    let Some(val_raw) = last else {
        return String::new();
    };
    let val = model_provenance_re()
        .replace_all(val_raw, "")
        .trim()
        .to_string();
    if val.is_empty() {
        return String::new();
    }
    if model_token_re().is_match(&val) {
        val
    } else {
        String::new()
    }
}

/// True when `path` matches the locked frontend surface list. Reproduces the
/// bash `case` statement exactly: `*` matches any run of characters
/// INCLUDING the empty string and `/`, so `components/*` means "starts with
/// `components/`" and `*/components/*` means "contains `/components/`" - no
/// length guard.
pub fn is_frontend_surface_path(path: &str) -> bool {
    if path.ends_with(".tsx") || path.ends_with(".jsx") {
        return true;
    }
    for token in ["components", "routes"] {
        if path.starts_with(&format!("{token}/")) || path.contains(&format!("/{token}/")) {
            return true;
        }
    }
    path.starts_with("src/styles/") || path.contains("/src/styles/")
}

/// True when any nonempty path in `paths` matches the locked surface list.
/// Blank entries are skipped, mirroring the bash `[[ -z "$path" ]] &&
/// continue`.
pub fn any_frontend_surface<'a>(paths: impl IntoIterator<Item = &'a str>) -> bool {
    paths
        .into_iter()
        .filter(|p| !p.is_empty())
        .any(is_frontend_surface_path)
}

fn read_stdin_paths() -> Vec<String> {
    let mut data = String::new();
    let _ = std::io::stdin().read_to_string(&mut data);
    data.split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// `fno-agents executor-route locked [--key model] | surface [--has-ui]`,
/// design doc or newline path list on stdin. Both valid modes exit 0; a
/// missing or unknown mode prints usage to stderr and exits 2.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return 0;
    }
    let mut rest: &[String] = args;
    // Tolerate a leading command token (surface-check does the same).
    if rest.first().map(String::as_str) == Some("executor-route") {
        rest = &rest[1..];
    }
    let Some(mode) = rest.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return 2;
    };
    let flags = &rest[1..];
    match mode {
        "locked" => {
            let mut input = String::new();
            if std::io::stdin().read_to_string(&mut input).is_err() {
                eprint!("{USAGE}");
                return 2;
            }
            // `--key model` selects the model-pin parser; default is the
            // executor lock (the Python checked argv[:2] == ["--key", "model"]).
            let value = if flags.first().map(String::as_str) == Some("--key")
                && flags.get(1).map(String::as_str) == Some("model")
            {
                parse_locked_model(&input)
            } else {
                parse_locked_executor(&input)
            };
            if !value.is_empty() {
                println!("{value}");
            }
            0
        }
        "surface" => {
            let has_ui = flags.iter().any(|a| a == "--has-ui");
            let matched = any_frontend_surface(read_stdin_paths().iter().map(String::as_str));
            println!(
                "{}",
                match (has_ui, matched) {
                    (true, true) => "true",
                    (true, false) => "false",
                    (false, true) => "impeccable",
                    (false, false) => "do",
                }
            );
            0
        }
        _ => {
            eprint!("{USAGE}");
            2
        }
    }
}

//! Read-only census and packet compiler for Footnote-authored context.
//!
//! Port of the deleted `cli/src/fno/context_audit.py`: one report over the
//! (harness x entry_state) matrix, packet compilation, duplicate and
//! conflict grouping. `context-run` calls it in-process for the native
//! manifest; `fno doctor --context-audit` execs the same reading.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) const SUPPORTED_HARNESSES: [&str; 3] = ["claude", "codex", "gemini"];
pub(crate) const SUPPORTED_ENTRY_STATES: [&str; 4] = ["startup", "resume", "clear", "post_compact"];
pub(crate) const DEFAULT_PACKET_BUDGET_BYTES: usize = 32_768;

const HOST_FILES: [(&str, &str); 3] = [
    ("claude", "CLAUDE.md"),
    ("codex", "AGENTS.md"),
    ("gemini", "GEMINI.md"),
];
const FOLDED_CONTEXT_SCRIPTS: [&str; 5] = [
    "session-start-using-fno.sh",
    "inject-project-vision.sh",
    "inject-fno-agent-whoami.sh",
    "setup-nudge-session-start.sh",
    "inject-mail-drain-session-start.sh",
];
const OPERATIONAL_SESSION_SOURCES: [&str; 4] = [
    "attest-model",
    "eval-sweep-session-start",
    "groom-self-heal-session-start",
    "register-session-start",
];
// The gemini SessionStart carrier (setup/cli_hooks.py folded here: its last
// reader was this census).
const GEMINI_CARRIER_SCRIPT: &str = "hooks/session-start.sh";
const GEMINI_CARRIER_ENTRY_STATES: [&str; 3] = ["startup", "resume", "clear"];

/// The byte domain a source record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measurement {
    Directive,
    CarrierTemplate,
}

impl Measurement {
    fn value(self) -> &'static str {
        match self {
            Measurement::Directive => "directive_bytes",
            Measurement::CarrierTemplate => "carrier_template_bytes",
        }
    }
}

/// One measured context source.
#[derive(Debug, Clone)]
struct ContextSource {
    source_id: String,
    harness: String,
    entry_state: String,
    lifecycle: String,
    layer: String,
    provenance: String,
    carrier: String,
    reachability_condition: String,
    content: Option<Vec<u8>>,
    ordinal: i64,
    status: String,
    error: Option<String>,
    packet_eligible: bool,
    measurement: Measurement,
    anchors: BTreeMap<String, Vec<String>>,
}

impl ContextSource {
    fn new(
        source_id: &str,
        harness: &str,
        entry_state: &str,
        lifecycle: &str,
        layer: &str,
        carrier: &str,
        reachability_condition: &str,
        ordinal: i64,
    ) -> Self {
        ContextSource {
            source_id: source_id.to_string(),
            harness: harness.to_string(),
            entry_state: entry_state.to_string(),
            lifecycle: lifecycle.to_string(),
            layer: layer.to_string(),
            provenance: String::new(),
            carrier: carrier.to_string(),
            reachability_condition: reachability_condition.to_string(),
            content: None,
            ordinal,
            status: "reachable".to_string(),
            error: None,
            packet_eligible: true,
            measurement: Measurement::Directive,
            anchors: BTreeMap::new(),
        }
    }

    fn record(&self) -> Value {
        let size = self.content.as_ref().map(Vec::len).unwrap_or(0);
        let directive = self.measurement == Measurement::Directive;
        let digest = self.content.as_ref().map(|c| sha256_hex(c));
        json!({
            "source_id": self.source_id,
            "harness": self.harness,
            "entry_state": self.entry_state,
            "lifecycle": self.lifecycle,
            "layer": self.layer,
            "provenance": self.provenance,
            "carrier": self.carrier,
            "reachability_condition": self.reachability_condition,
            "status": self.status,
            "error": self.error,
            "bytes": if directive { size } else { 0 },
            "estimated_tokens": if directive { (size + 3) / 4 } else { 0 },
            "content_hash": if directive { digest.clone() } else { None::<String> },
            "carrier_bytes": if !directive { Some(size) } else { None },
            "carrier_hash": if !directive { digest } else { None },
            "measurement": self.measurement.value(),
            "packet_eligible": self.packet_eligible,
            "anchors": anchors_json(&self.anchors),
        })
    }
}

fn sha256_hex(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}

fn anchors_json(anchors: &BTreeMap<String, Vec<String>>) -> Value {
    let mut map = Map::new();
    for (key, values) in anchors {
        map.insert(key.clone(), json!(values));
    }
    Value::Object(map)
}

fn regex_include() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?m)^@(?P<path>[^\r\n]+?)[ \t]*(?P<newline>\r?\n|$)").unwrap()
    })
}

fn regex_sentinel() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\b[A-Za-z0-9-]*sentinel[A-Za-z0-9-]*\b").unwrap())
}

fn regex_heading() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?m)^### (.+)$").unwrap())
}

fn regex_plugin_path() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"\$\{(?:CLAUDE_PLUGIN_ROOT|PLUGIN_ROOT)(?::-[^}]*)?}/(?P<path>[A-Za-z0-9_./-]+)",
        )
        .unwrap()
    })
}

fn regex_context_run() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"context-run\.sh["']?\s+(\S+)"#).unwrap())
}

fn relative(path: Option<&Path>, root: &Path) -> String {
    // None ships "" (the gemini branch never receives it on claude/codex-only
    // paths, matching the Python comment).
    let Some(path) = path else {
        return String::new();
    };
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let rooted = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    match resolved.strip_prefix(&rooted) {
        Ok(rest) => rest.to_string_lossy().replace('\\', "/"),
        Err(_) => resolved.to_string_lossy().into_owned(),
    }
}

fn pitfall_section(text: &str) -> String {
    let Some(start) = text.find("## Pitfalls corpus") else {
        return String::new();
    };
    let from = start + "## Pitfalls corpus".len();
    match text[from..].find("\n## ") {
        Some(off) => text[start..from + off].to_string(),
        None => text[start..].to_string(),
    }
}

fn instruction_anchors(content: &[u8]) -> BTreeMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(content);
    let section = pitfall_section(&text);
    let pitfalls: Vec<String> = regex_heading()
        .captures_iter(&section)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect();
    let mut sentinels: Vec<String> = Vec::new();
    for m in regex_sentinel().find_iter(&section) {
        let s = m.as_str().to_string();
        if !sentinels.contains(&s) {
            sentinels.push(s);
        }
    }
    let mut anchors = BTreeMap::new();
    anchors.insert("pitfalls".to_string(), pitfalls);
    anchors.insert("sentinels".to_string(), sentinels);
    anchors
}

fn host_file_for(harness: &str) -> Option<&'static str> {
    HOST_FILES
        .iter()
        .find(|(h, _)| *h == harness)
        .map(|(_, f)| *f)
}

fn is_footnote_source_repository(repo_root: &Path) -> bool {
    let Ok(text) = std::fs::read(repo_root.join(".claude-plugin/plugin.json")) else {
        return false;
    };
    match serde_json::from_slice::<Value>(&text) {
        Ok(Value::Object(map)) => map.get("name").and_then(Value::as_str) == Some("fno"),
        _ => false,
    }
}

/// `@include` expansion with repo-escape and cycle refusal (ValueError
/// becomes Err(String)).
fn expand_host_content(path: &Path, repo_root: &Path, seen: &[PathBuf]) -> Result<String, String> {
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("OSError: {}", e_kind(&e)))?;
    let root = repo_root
        .canonicalize()
        .map_err(|e| format!("OSError: {}", e_kind(&e)))?;
    if resolved != root && !resolved.starts_with(&root) {
        return Err(format!(
            "instruction include escapes repository: {}",
            resolved.display()
        ));
    }
    if seen.contains(&resolved) {
        return Err(format!("instruction include cycle: {}", resolved.display()));
    }
    let text =
        std::fs::read_to_string(&resolved).map_err(|e| format!("OSError: {}", e_kind(&e)))?;
    let mut next_seen = seen.to_vec();
    next_seen.push(resolved.clone());
    let mut result = text.clone();
    // Replace iteratively: the Python re.sub walked nested includes
    // depth-first in one pass; a fixed-point loop with the seen-set growing
    // reproduces that (each pass expands what the previous pass revealed).
    loop {
        let current = result.clone();
        let mut out = String::new();
        let mut last = 0;
        let mut changed = false;
        for caps in regex_include().captures_iter(&current) {
            let whole = caps.get(0).expect("capture 0");
            out.push_str(&current[last..whole.start()]);
            let rel = caps
                .name("path")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let newline = caps.name("newline").map(|m| m.as_str().to_string());
            let target = resolved
                .parent()
                .unwrap_or(Path::new("/"))
                .join(&rel)
                .canonicalize()
                .map_err(|e| format!("OSError: {}", e_kind(&e)))?;
            let mut expanded = expand_host_content(&target, repo_root, &next_seen)?;
            if let Some(nl) = newline {
                if !expanded.ends_with('\n') && !expanded.ends_with('\r') {
                    expanded.push_str(&nl);
                }
            }
            out.push_str(&expanded);
            last = whole.end();
            changed = true;
        }
        if !changed {
            result = current;
            break;
        }
        result = out;
    }
    Ok(result)
}

fn e_kind(e: &std::io::Error) -> String {
    e.to_string()
}

const BEGIN_PREFIX: &str = "<!-- fno:begin";
const END_MARKER: &str = "<!-- fno:end -->";

fn marker_state(text: &str) -> &'static str {
    let begin_count = text.matches(BEGIN_PREFIX).count();
    let end_count = text.matches(END_MARKER).count();
    if begin_count == 0 && end_count == 0 {
        return "none";
    }
    if begin_count == 1
        && end_count == 1
        && text.find(BEGIN_PREFIX).unwrap_or(0) < text.find(END_MARKER).unwrap_or(usize::MAX)
    {
        return "both";
    }
    "malformed"
}

fn extract_block(text: &str) -> Option<String> {
    if marker_state(text) != "both" {
        return None;
    }
    let begin = text.find(BEGIN_PREFIX)?;
    let end = text[begin..].find(END_MARKER)? + begin;
    Some(text[begin..end + END_MARKER.len()].to_string())
}

fn project_instructions_source(
    repo_root: &Path,
    harness: &str,
    entry_state: &str,
    ordinal: i64,
) -> (Option<ContextSource>, Option<Vec<u8>>) {
    let carrier_path = repo_root.join(host_file_for(harness).unwrap_or("CLAUDE.md"));
    let read: Result<Vec<u8>, String> = if harness == "codex" {
        std::fs::read(&carrier_path).map_err(|e| format!("OSError: {}", e_kind(&e)))
    } else {
        expand_host_content(&carrier_path, repo_root, &[]).map(|s| s.into_bytes())
    };
    match read {
        Err(e) => {
            let mut s = ContextSource::new(
                "project-instructions",
                harness,
                entry_state,
                "harness_native",
                "kernel",
                &relative(Some(&carrier_path), repo_root),
                &format!("{harness} loads repository instructions"),
                ordinal,
            );
            s.provenance = relative(Some(&carrier_path), repo_root);
            s.status = "unreadable".to_string();
            s.error = Some(e);
            return (Some(s), None);
        }
        Ok(content) => {
            let anchors = instruction_anchors(&content);
            let mut s = ContextSource::new(
                "project-instructions",
                harness,
                entry_state,
                "harness_native",
                "kernel",
                &relative(Some(&carrier_path), repo_root),
                &format!("{harness} loads repository instructions"),
                ordinal,
            );
            s.provenance = relative(Some(&carrier_path), repo_root);
            s.content = Some(content);
            s.anchors = anchors;
            (Some(s), None)
        }
    }
}

fn managed_block_sources(
    base: &ContextSource,
    content: Option<&[u8]>,
    carrier_path: &Path,
    source_repository: bool,
    ordinal: i64,
) -> Vec<ContextSource> {
    let mut out: Vec<ContextSource> = Vec::new();
    let content = match content {
        Some(c) => c,
        None => {
            if !source_repository && carrier_path.exists() {
                let mut s = ContextSource::new(
                    "managed-footnote-block",
                    &base.harness,
                    &base.entry_state,
                    "harness_native",
                    "kernel",
                    &base.carrier,
                    "managed block carrier is readable",
                    ordinal + 1,
                );
                s.provenance = format!("{}#fno-managed-block", base.provenance);
                s.status = "unreadable".to_string();
                s.error = base.error.clone();
                s.packet_eligible = false;
                s.anchors = empty_anchors();
                out.push(s);
            }
            return out;
        }
    };
    let host_text = String::from_utf8_lossy(content);
    match extract_block(&host_text) {
        Some(managed) => {
            let mut s = ContextSource::new(
                "managed-footnote-block",
                &base.harness,
                &base.entry_state,
                "harness_native",
                "kernel",
                &base.carrier,
                "managed block is present in host instructions",
                ordinal + 1,
            );
            s.provenance = format!("{}#fno-managed-block", base.provenance);
            s.anchors = instruction_anchors(managed.as_bytes());
            s.content = Some(managed.into_bytes());
            s.packet_eligible = !source_repository;
            out.push(s);
        }
        None if !source_repository && marker_state(&host_text) == "malformed" => {
            let mut s = ContextSource::new(
                "managed-footnote-block",
                &base.harness,
                &base.entry_state,
                "harness_native",
                "kernel",
                &base.carrier,
                "managed block fences are well formed",
                ordinal + 1,
            );
            s.provenance = format!("{}#fno-managed-block", base.provenance);
            s.status = "unreadable".to_string();
            s.error = Some("malformed Footnote managed block fences".to_string());
            s.packet_eligible = false;
            s.anchors = empty_anchors();
            out.push(s);
        }
        None => {}
    }
    out
}

fn empty_anchors() -> BTreeMap<String, Vec<String>> {
    BTreeMap::from([
        ("pitfalls".to_string(), Vec::new()),
        ("sentinels".to_string(), Vec::new()),
    ])
}

fn measure_file_source(
    source_id: &str,
    path: &Path,
    harness: &str,
    entry_state: &str,
    lifecycle: &str,
    layer: &str,
    carrier: &str,
    reachability_condition: &str,
    ordinal: i64,
    repo_root: &Path,
) -> ContextSource {
    let (content, status, error) = match std::fs::read(path) {
        Ok(bytes) => (Some(bytes), "reachable".to_string(), None),
        Err(e) => (
            None,
            "unreadable".to_string(),
            Some(format!("OSError: {}", e_kind(&e))),
        ),
    };
    let mut s = ContextSource::new(
        source_id,
        harness,
        entry_state,
        lifecycle,
        layer,
        carrier,
        reachability_condition,
        ordinal,
    );
    s.provenance = relative(Some(path), repo_root);
    s.content = content;
    s.status = status;
    s.error = error;
    s
}

fn claude_rule_sources(
    repo_root: &Path,
    harness: &str,
    entry_state: &str,
    ordinal: i64,
) -> Vec<ContextSource> {
    let mut rules: Vec<PathBuf> = std::fs::read_dir(repo_root.join(".claude/rules"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    rules.sort();
    rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let name = rule
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            measure_file_source(
                &format!("claude-rule:{name}"),
                rule,
                harness,
                entry_state,
                "harness_native",
                "progressive",
                &rule.to_string_lossy(),
                "Claude repository rule discovery",
                ordinal + index as i64 + 2,
                repo_root,
            )
        })
        .collect()
}

fn host_sources(
    repo_root: &Path,
    harness: &str,
    entry_state: &str,
    ordinal: i64,
) -> Vec<ContextSource> {
    let source_repository = is_footnote_source_repository(repo_root);
    let carrier_path = repo_root.join(host_file_for(harness).unwrap_or("CLAUDE.md"));
    let (base, content) = project_instructions_source(repo_root, harness, entry_state, ordinal);
    let mut sources: Vec<ContextSource> = Vec::new();
    if let Some(base) = base {
        sources.extend(managed_block_sources(
            &base,
            content.as_deref(),
            &carrier_path,
            source_repository,
            ordinal,
        ));
        if source_repository {
            sources.insert(0, base);
        }
    }
    if harness == "claude" && source_repository {
        sources.extend(claude_rule_sources(
            repo_root,
            harness,
            entry_state,
            ordinal,
        ));
    }
    sources
}

fn load_hook_commands(
    repo_root: &Path,
    manifest: &Path,
    event_names: &[&str],
    matchers: Option<&[&str]>,
) -> (Vec<String>, Option<String>) {
    let mut commands: Vec<String> = Vec::new();
    let failure: Option<String> = None;
    let parsed: Result<Value, String> = std::fs::read(manifest)
        .map_err(|e| format!("OSError: {}", e_kind(&e)))
        .and_then(|raw| serde_json::from_slice(&raw).map_err(|e| format!("JSONDecodeError: {e}")));
    let parsed = match parsed {
        Ok(v) => v,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!("{}: {}", relative(Some(manifest), repo_root), e)),
            )
        }
    };
    let hooks = parsed.get("hooks").cloned().unwrap_or(json!({}));
    for event_name in event_names {
        let groups = match hooks.get(event_name) {
            Some(Value::Array(items)) => items.clone(),
            Some(_) => {
                let msg = format!("hooks.{event_name} is not an array");
                return (
                    Vec::new(),
                    Some(format!("{}: {}", relative(Some(manifest), repo_root), msg)),
                );
            }
            None => Vec::new(),
        };
        for group in &groups {
            if !group.is_object() {
                let msg = format!("hooks.{event_name} group is not an object");
                return (
                    Vec::new(),
                    Some(format!("{}: {}", relative(Some(manifest), repo_root), msg)),
                );
            }
            if let Some(list) = matchers {
                let group_matcher = group.get("matcher").and_then(Value::as_str).unwrap_or("");
                if !list.contains(&group_matcher) {
                    continue;
                }
            }
            let entries = group.get("hooks").cloned().unwrap_or(json!([]));
            let Value::Array(entries) = entries else {
                let msg = format!("hooks.{event_name} group hooks not an array");
                return (
                    Vec::new(),
                    Some(format!("{}: {}", relative(Some(manifest), repo_root), msg)),
                );
            };
            for entry in &entries {
                if !entry.is_object() {
                    let msg = format!("hooks.{event_name} entry is not an object");
                    return (
                        Vec::new(),
                        Some(format!("{}: {}", relative(Some(manifest), repo_root), msg)),
                    );
                }
                let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
                if !command.is_empty() {
                    commands.push(command.to_string());
                }
            }
        }
    }
    (commands, failure)
}

fn command_path(command: &str, plugin_root: &Path) -> Option<PathBuf> {
    let matches: Vec<_> = regex_plugin_path().captures_iter(command).collect();
    matches
        .last()
        .and_then(|m| m.name("path"))
        .map(|p| plugin_root.join(p.as_str()))
}

fn expand_runner_commands(
    commands: Vec<(String, String)>,
    plugin_root: &Path,
    census_source: &str,
) -> Vec<(String, String)> {
    let mut declaration: Option<Value> = None;
    let declaration_path = plugin_root.join("hooks/context-hooks.json");
    let mut expanded: Vec<(String, String)> = Vec::new();
    for (command, lifecycle) in &commands {
        let Some(caps) = regex_context_run().captures(command) else {
            expanded.push((command.clone(), lifecycle.clone()));
            continue;
        };
        if declaration.is_none() {
            declaration = std::fs::read(&declaration_path)
                .ok()
                .and_then(|raw| serde_json::from_slice(&raw).ok());
        }
        let group_word = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let group = declaration
            .as_ref()
            .and_then(|d| d.get("groups"))
            .and_then(|g| g.get(group_word));
        let Some(group) = group else {
            expanded.push((command.clone(), lifecycle.clone()));
            continue;
        };
        if !group.is_object() {
            expanded.push((command.clone(), lifecycle.clone()));
            continue;
        }
        for producer in group
            .get("producers")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let sources: Vec<String> = producer
                .get("sources")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !sources.is_empty() && !sources.iter().any(|s| s == census_source) {
                continue;
            }
            let argv: Vec<String> = producer
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|item| match item {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !argv.is_empty() {
                expanded.push((argv.join(" "), lifecycle.clone()));
            }
        }
    }
    expanded
}

fn logical_id(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().replace('_', "-"))
        .unwrap_or_default()
}

fn measure_hook(
    path: &Path,
    repo_root: &Path,
    harness: &str,
    entry_state: &str,
    lifecycle: &str,
    ordinal: i64,
    status: &str,
    error: Option<String>,
) -> ContextSource {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let is_using_fno = name == "session-start-using-fno.sh";
    let (source_id, content_path, status, measurement) = if is_using_fno {
        (
            "using-fno".to_string(),
            repo_root.join("skills/using-fno/SKILL.md"),
            status.to_string(),
            Measurement::Directive,
        )
    } else {
        let registered = if status == "reachable" {
            "registered"
        } else {
            status
        };
        (
            logical_id(path),
            path.to_path_buf(),
            registered.to_string(),
            Measurement::CarrierTemplate,
        )
    };
    let packet_eligible = measurement == Measurement::Directive
        && !OPERATIONAL_SESSION_SOURCES.contains(&source_id.as_str());
    let mut s = measure_file_source(
        &source_id,
        &content_path,
        harness,
        entry_state,
        lifecycle,
        if source_id == "using-fno" {
            "kernel"
        } else {
            "progressive"
        },
        &relative(Some(path), repo_root),
        &format!("{lifecycle} registration"),
        ordinal,
        repo_root,
    );
    s.measurement = measurement;
    s.packet_eligible = packet_eligible;
    s.status = status;
    s.error = error;
    s
}

fn folded_sources(
    repo_root: &Path,
    harness: &str,
    entry_state: &str,
    wrapper: &Path,
    ordinal: i64,
) -> Vec<ContextSource> {
    let wrapper_text = match std::fs::read_to_string(wrapper) {
        Ok(t) => t,
        Err(e) => {
            let mut s = ContextSource::new(
                "session-start-wrapper",
                harness,
                entry_state,
                "session_start",
                "progressive",
                &relative(Some(wrapper), repo_root),
                &format!("{harness} folded SessionStart"),
                ordinal,
            );
            s.provenance = relative(Some(wrapper), repo_root);
            s.status = "unreadable".to_string();
            s.error = Some(format!("OSError: {}", e_kind(&e)));
            s.packet_eligible = false;
            return vec![s];
        }
    };
    let mut sources: Vec<ContextSource> = Vec::new();
    for (offset, filename) in FOLDED_CONTEXT_SCRIPTS.iter().enumerate() {
        let path = repo_root.join("hooks").join(filename);
        let marker = format!("${{SCRIPT_DIR}}/{filename}");
        let reachable = wrapper_text.contains(&marker);
        let (status, error) = if reachable {
            ("reachable", None)
        } else {
            (
                "omitted",
                Some("not_reached_by_folded_session_start".to_string()),
            )
        };
        sources.push(measure_hook(
            &path,
            repo_root,
            harness,
            entry_state,
            "session_start",
            ordinal + offset as i64,
            status,
            error,
        ));
    }
    let hygiene_start = wrapper_text.find("# 4. worktree-scope hygiene");
    let hygiene_end = hygiene_start.and_then(|s| {
        wrapper_text[s..]
            .find("# 5. first-run setup nudge")
            .map(|off| s + off)
    });
    if let (Some(start), Some(end)) = (hygiene_start, hygiene_end) {
        let hygiene = wrapper_text[start..end].as_bytes().to_vec();
        let mut s = ContextSource::new(
            "worktree-hygiene",
            harness,
            entry_state,
            "session_start",
            "progressive",
            "hooks/session-start.sh",
            &format!("{harness} folded SessionStart"),
            ordinal + FOLDED_CONTEXT_SCRIPTS.len() as i64,
        );
        s.provenance = "hooks/session-start.sh#worktree-hygiene".to_string();
        s.content = Some(hygiene);
        s.status = "registered".to_string();
        s.measurement = Measurement::CarrierTemplate;
        s.packet_eligible = false;
        sources.push(s);
    }
    sources
}

fn discover_cell_sources(
    host_root: &Path,
    plugin_root: &Path,
    harness: &str,
    entry_state: &str,
) -> Vec<ContextSource> {
    let mut sources = host_sources(host_root, harness, entry_state, 0);
    let mut ordinal = sources.len() as i64 + 10;
    let specs: Vec<(&str, Option<Vec<&str>>, &str)> = if entry_state == "post_compact" {
        let claude_matchers: Vec<&str> = if harness == "claude" {
            vec!["", "compact"]
        } else {
            vec!["compact"]
        };
        vec![
            ("PostCompact", None, "post_compact"),
            ("SessionStart", Some(claude_matchers), "session_start"),
        ]
    } else {
        vec![("SessionStart", Some(vec!["", entry_state]), "session_start")]
    };
    let mut commands: Vec<(String, String)> = Vec::new();
    let mut manifest_error: Option<String> = None;
    let mut manifest: Option<PathBuf> = None;
    if harness == "claude" || harness == "codex" {
        manifest = Some(if harness == "claude" {
            plugin_root.join("hooks/hooks.json")
        } else {
            plugin_root.join("hooks/codex-hooks.json")
        });
        if let Some(mpath) = &manifest {
            for (event_name, matchers, lifecycle) in &specs {
                let (loaded, err) =
                    load_hook_commands(plugin_root, mpath, &[*event_name], matchers.as_deref());
                if let Some(e) = err {
                    manifest_error = Some(e);
                    break;
                }
                for cmd in loaded {
                    commands.push((cmd, lifecycle.to_string()));
                }
            }
        }
    } else {
        let registered = GEMINI_CARRIER_ENTRY_STATES.contains(&entry_state);
        if registered {
            let cmd = format!("${{PLUGIN_ROOT}}/{GEMINI_CARRIER_SCRIPT}");
            commands.push((cmd, "session_start".to_string()));
        }
        if !registered {
            let mut s = measure_file_source(
                "session-start-wrapper",
                &plugin_root.join(GEMINI_CARRIER_SCRIPT),
                harness,
                entry_state,
                "session_start",
                "progressive",
                GEMINI_CARRIER_SCRIPT,
                "Gemini SessionStart registration",
                ordinal,
                plugin_root,
            );
            s.status = "omitted".to_string();
            s.error = Some("entry_state_not_registered".to_string());
            s.packet_eligible = false;
            sources.push(s);
            ordinal += 1;
            if entry_state == "post_compact" {
                // Derived from the hooks dir, not hardcoded: a third reinject
                // hook must appear here as an omission without anyone editing
                // this loop (the manifest-driven census works that way too).
                let mut reinjects: Vec<PathBuf> = std::fs::read_dir(plugin_root.join("hooks"))
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .map(|e| e.path())
                            .filter(|p| {
                                p.file_name()
                                    .map(|n| {
                                        let name = n.to_string_lossy();
                                        name.ends_with("-postcompact-reinject.sh")
                                    })
                                    .unwrap_or(false)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                reinjects.sort();
                for hook in &reinjects {
                    sources.push(measure_hook(
                        hook,
                        plugin_root,
                        harness,
                        entry_state,
                        "post_compact",
                        ordinal,
                        "omitted",
                        Some("no_post_compact_registration".to_string()),
                    ));
                    ordinal += 1;
                }
            }
        }
    }
    let census_source = if entry_state == "post_compact" {
        "compact"
    } else {
        entry_state
    };
    let commands = expand_runner_commands(commands, plugin_root, census_source);
    if let Some(err) = &manifest_error {
        let mut s = ContextSource::new(
            "hook-manifest",
            harness,
            entry_state,
            "session_start",
            "progressive",
            &relative(manifest.as_deref(), plugin_root),
            &format!("{harness} hook manifest parses"),
            ordinal,
        );
        s.carrier = relative(manifest.as_deref(), plugin_root);
        s.status = "unreadable".to_string();
        s.error = Some(err.clone());
        s.packet_eligible = false;
        sources.push(s);
        return sources;
    }
    for (command_index, (command, lifecycle)) in commands.iter().enumerate() {
        let path = command_path(command, plugin_root);
        let Some(path) = path else {
            let mut s = ContextSource::new(
                &format!("unresolved-hook:{command_index}"),
                harness,
                entry_state,
                lifecycle,
                "progressive",
                &relative(manifest.as_deref(), plugin_root),
                &format!("{harness} registered hook resolves"),
                ordinal,
            );
            s.carrier = command.clone();
            s.status = "unreadable".to_string();
            s.error = Some("registered command has no Footnote plugin path".to_string());
            s.packet_eligible = false;
            sources.push(s);
            ordinal += 1;
            continue;
        };
        if path
            .file_name()
            .map(|n| n.to_string_lossy() == "session-start.sh")
            .unwrap_or(false)
        {
            sources.extend(folded_sources(
                plugin_root,
                harness,
                entry_state,
                &path,
                ordinal,
            ));
            ordinal += FOLDED_CONTEXT_SCRIPTS.len() as i64 + 1;
            continue;
        }
        sources.push(measure_hook(
            &path,
            plugin_root,
            harness,
            entry_state,
            lifecycle,
            ordinal,
            "reachable",
            None,
        ));
        ordinal += 1;
    }
    sources
}

fn duplicate_groups(records: &[Value]) -> Vec<Value> {
    let mut by_hash: Vec<(String, Vec<&Value>)> = Vec::new();
    for record in records {
        let hash = record.get("content_hash").and_then(Value::as_str);
        let bytes = record.get("bytes").and_then(Value::as_u64).unwrap_or(0);
        let Some(hash) = hash else { continue };
        if bytes == 0 {
            continue;
        }
        match by_hash.iter_mut().find(|(h, _)| h == hash) {
            Some((_, v)) => v.push(record),
            None => by_hash.push((hash.to_string(), vec![record])),
        }
    }
    by_hash.sort_by(|a, b| a.0.cmp(&b.0));
    by_hash
        .iter()
        .filter(|(_, occurrences)| occurrences.len() > 1)
        .map(|(hash, occurrences)| {
            let mut ids: Vec<String> = occurrences
                .iter()
                .filter_map(|r| r.get("source_id").and_then(Value::as_str))
                .map(String::from)
                .collect();
            ids.sort();
            ids.dedup();
            json!({
                "content_hash": hash,
                "source_ids": ids,
                "occurrences": occurrences.len(),
            })
        })
        .collect()
}

fn conflict_groups(records: &[Value]) -> Vec<Value> {
    let mut by_id: Vec<(String, Vec<&Value>)> = Vec::new();
    for record in records {
        let has_hash = record.get("content_hash").is_some();
        if !has_hash {
            continue;
        }
        let id = record
            .get("source_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match by_id.iter_mut().find(|(k, _)| *k == id) {
            Some((_, v)) => v.push(record),
            None => by_id.push((id, vec![record])),
        }
    }
    by_id.sort_by(|a, b| a.0.cmp(&b.0));
    by_id
        .iter()
        .filter(|(_, occurrences)| {
            let mut hashes: Vec<&str> = occurrences
                .iter()
                .filter_map(|r| r.get("content_hash").and_then(Value::as_str))
                .collect();
            hashes.sort();
            hashes.dedup();
            hashes.len() > 1
        })
        .map(|(id, occurrences)| {
            let mut hashes: Vec<String> = occurrences
                .iter()
                .filter_map(|r| r.get("content_hash").and_then(Value::as_str))
                .map(String::from)
                .collect();
            hashes.sort();
            hashes.dedup();
            let provs: Vec<String> = occurrences
                .iter()
                .filter_map(|r| r.get("provenance").and_then(Value::as_str))
                .map(String::from)
                .collect();
            let mut provs_dedup: Vec<String> = Vec::new();
            for p in provs {
                if !provs_dedup.contains(&p) {
                    provs_dedup.push(p);
                }
            }
            json!({
                "source_id": id,
                "content_hashes": hashes,
                "provenance": provs_dedup,
            })
        })
        .collect()
}

fn merged_anchors<'a>(sources: impl IntoIterator<Item = &'a ContextSource>) -> Value {
    let mut merged: Vec<(String, Vec<String>)> = Vec::new();
    for source in sources {
        for (key, values) in &source.anchors {
            match merged.iter_mut().find(|(k, _)| k == key) {
                Some((_, bucket)) => {
                    for v in values {
                        if !bucket.contains(v) {
                            bucket.push(v.clone());
                        }
                    }
                }
                None => merged.push((key.clone(), values.clone())),
            }
        }
    }
    let mut map = Map::new();
    for (key, values) in merged {
        map.insert(key, json!(values));
    }
    Value::Object(map)
}

fn compile_context(
    sources: Vec<ContextSource>,
    packet_budget: usize,
    node_count: usize,
) -> Result<Value, String> {
    if packet_budget < 1 {
        return Err("packet_budget_bytes must be at least 1".to_string());
    }
    if node_count < 1 {
        return Err("node_count must be at least 1".to_string());
    }
    let mut indexed: Vec<(usize, ContextSource)> = sources.into_iter().enumerate().collect();
    indexed.sort_by_key(|(i, s)| (s.ordinal, *i));
    let ordered: Vec<ContextSource> = indexed.into_iter().map(|(_, s)| s).collect();
    let records: Vec<Value> = ordered.iter().map(|s| s.record()).collect();
    // unique = reachable + packet-eligible + fresh hash, in order
    let mut unique: Vec<(usize, ContextSource)> = Vec::new();
    let mut seen_hashes: Vec<String> = Vec::new();
    for (i, source) in ordered.iter().enumerate() {
        let hash = source.content.as_ref().map(|c| sha256_hex(c));
        let eligible = source.status == "reachable"
            && source.packet_eligible
            && source.measurement == Measurement::Directive
            && hash
                .as_ref()
                .map(|h| !seen_hashes.contains(h))
                .unwrap_or(false);
        if eligible {
            let h = hash.unwrap();
            seen_hashes.push(h.clone());
            unique.push((i, source.clone()));
        }
    }
    // kernel-first, then ordinal (stable keeps the ordinal/index order on ties)
    unique.sort_by_key(|(_, s)| (if s.layer == "kernel" { 0 } else { 1 }, s.ordinal));
    let kernel_candidates: Vec<(usize, &ContextSource)> = unique
        .iter()
        .filter(|(_, s)| s.layer == "kernel")
        .map(|(i, s)| (*i, s))
        .collect();
    let progressive_candidates: Vec<(usize, &ContextSource)> = unique
        .iter()
        .filter(|(_, s)| s.layer != "kernel")
        .map(|(i, s)| (*i, s))
        .collect();
    let kernel_total: usize = kernel_candidates
        .iter()
        .map(|(_, s)| s.content.as_ref().map(Vec::len).unwrap_or(0))
        .sum();
    let mut included: Vec<(usize, &ContextSource)> = Vec::new();
    let mut budget_omitted: Vec<(usize, &ContextSource)> = Vec::new();
    let mut used: usize = 0;
    if kernel_total > packet_budget {
        for (i, s) in &unique {
            budget_omitted.push((*i, s));
        }
    } else {
        // kernel in, then progressive greedy
        for (i, s) in &kernel_candidates {
            included.push((*i, *s));
        }
        used = kernel_total;
        let mut continuation_started = false;
        for (i, s) in &progressive_candidates {
            let bytes = s.content.as_ref().map(Vec::len).unwrap_or(0);
            if continuation_started || used + bytes > packet_budget {
                continuation_started = true;
                budget_omitted.push((*i, *s));
            } else {
                included.push((*i, *s));
                used += bytes;
            }
        }
    }
    let kernel_pairs: Vec<&(usize, &ContextSource)> = included
        .iter()
        .filter(|(_, s)| s.layer == "kernel")
        .collect();
    let progressive_pairs: Vec<&(usize, &ContextSource)> = included
        .iter()
        .filter(|(_, s)| s.layer != "kernel")
        .collect();
    let kernel_sources: Vec<&ContextSource> = kernel_pairs.iter().map(|(_, s)| *s).collect();
    let kernel_hash_input = kernel_pairs
        .iter()
        .map(|(i, _)| {
            records[*i]
                .get("content_hash")
                .and_then(Value::as_str)
                .unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let packet_hashes: Vec<&str> = included
        .iter()
        .map(|(i, _)| {
            records[*i]
                .get("content_hash")
                .and_then(Value::as_str)
                .unwrap_or("")
        })
        .collect();
    let packet_hash_input = packet_hashes.join("\n");
    let kernel_bytes: usize = kernel_sources
        .iter()
        .map(|s| s.content.as_ref().map(Vec::len).unwrap_or(0))
        .sum();
    let mut omitted: Vec<Value> = Vec::new();
    for (source, record) in ordered.iter().zip(records.iter()) {
        if source.status == "omitted" || source.status == "unreadable" {
            omitted.push(json!({
                "source_id": source.source_id,
                "provenance": record.get("provenance"),
                "reason": format!(
                    "{}:{}",
                    source.status,
                    source.error.clone().unwrap_or_else(|| "not reachable".to_string())
                ),
                "bytes": record.get("bytes"),
            }));
        }
    }
    // budget omissions appended after the read-status omissions, same order
    for (i, s) in &budget_omitted {
        omitted.push(json!({
            "source_id": s.source_id,
            "provenance": records[*i].get("provenance"),
            "reason": "packet_budget",
            "bytes": records[*i].get("bytes"),
        }));
    }
    let continuation: Value = if budget_omitted.is_empty() {
        Value::Null
    } else {
        let after = included
            .last()
            .map(|(i, _s)| records[*i].get("source_id").cloned().unwrap_or(Value::Null));
        let remaining_ids: Vec<&str> = budget_omitted
            .iter()
            .map(|(i, _)| {
                records[*i]
                    .get("source_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            })
            .collect();
        let remaining_bytes: usize = budget_omitted
            .iter()
            .map(|(i, _)| {
                records[*i]
                    .get("bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize
            })
            .sum();
        json!({
            "after_source_id": after,
            "remaining_source_ids": remaining_ids,
            "remaining_bytes": remaining_bytes,
        })
    };
    let execution = if node_count == 1 {
        json!({"mode": "existing_single_loop", "graph_compiled": false, "dispatch_changed": false})
    } else {
        json!({"mode": "observational_only", "graph_compiled": false, "dispatch_changed": false})
    };
    Ok(json!({
        "schema_version": 1,
        "execution": execution,
        "packet": {
            "budget_bytes": packet_budget,
            "bytes": used,
            "estimated_tokens": (used + 3) / 4,
            "within_budget": used <= packet_budget,
            "source_hashes": packet_hashes,
            "content_hash": if packet_hash_input.is_empty() {
                None
            } else {
                Some(sha256_hex(packet_hash_input.as_bytes()))
            },
        },
        "kernel": {
            "source_ids": kernel_sources
                .iter()
                .map(|s| s.source_id.clone())
                .collect::<Vec<_>>(),
            "bytes": kernel_bytes,
            "estimated_tokens": (kernel_bytes + 3) / 4,
            "content_hash": if kernel_hash_input.is_empty() {
                None
            } else {
                Some(sha256_hex(kernel_hash_input.as_bytes()))
            },
            "anchors": merged_anchors(kernel_sources.iter().copied()),
        },
        "progressive_sources": progressive_pairs
            .iter()
            .map(|(i, _)| records[*i].clone())
            .collect::<Vec<_>>(),
        "source_manifest": records,
        "duplicates": duplicate_groups(&records),
        "conflicts": conflict_groups(&records),
        "omitted_sources": omitted,
        "continuation": continuation,
    }))
}

pub(crate) fn build_context_report(
    repo_root: &Path,
    plugin_root: &Path,
    harnesses: &[&str],
    entry_states: &[&str],
    packet_budget: usize,
    node_count: usize,
) -> Result<Value, String> {
    for h in harnesses {
        if !SUPPORTED_HARNESSES.contains(h) {
            return Err(format!("unsupported harness: {h}"));
        }
    }
    for e in entry_states {
        if !SUPPORTED_ENTRY_STATES.contains(e) {
            return Err(format!("unsupported entry state: {e}"));
        }
    }
    let mut cells: Vec<Value> = Vec::new();
    let mut all_records: Vec<Value> = Vec::new();
    for harness in harnesses {
        for entry_state in entry_states {
            let sources = discover_cell_sources(repo_root, plugin_root, harness, entry_state);
            let compiled = compile_context(sources, packet_budget, node_count)?;
            if let Some(records) = compiled.get("source_manifest").and_then(Value::as_array) {
                all_records.extend(records.iter().cloned());
            }
            cells.push(json!({
                "harness": harness,
                "entry_state": entry_state,
                "compiled": compiled,
            }));
        }
    }
    let repo_display = repo_root
        .canonicalize()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo_root.to_string_lossy().into_owned());
    let plugin_display = plugin_root
        .canonicalize()
        .unwrap_or_else(|_| plugin_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    Ok(json!({
        "schema_version": 1,
        "repo_root": repo_display,
        "plugin_root": plugin_display,
        "matrix": {
            "harnesses": harnesses,
            "entry_states": entry_states,
        },
        "packet_budget_bytes": packet_budget,
        "cells": cells,
        "duplicates": duplicate_groups(&all_records),
        "conflicts": conflict_groups(&all_records),
    }))
}

pub fn run_context_audit(args: &[String]) -> i32 {
    let mut source: Option<&str> = None;
    let mut host: Option<&str> = None;
    let mut harness_filter: &str = "all";
    let mut entry_filter: &str = "all";
    let mut budget: usize = DEFAULT_PACKET_BUDGET_BYTES;
    let mut json_out = false;
    let mut rest = args.iter();
    while let Some(tok) = rest.next() {
        match tok.as_str() {
            "--source" => source = rest.next().map(String::as_str),
            "--context-host" => host = rest.next().map(String::as_str),
            "--context-harness" => {
                harness_filter = rest.next().map(String::as_str).unwrap_or("all")
            }
            "--context-entry" => entry_filter = rest.next().map(String::as_str).unwrap_or("all"),
            "--context-budget" => match rest.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => budget = n,
                None => {
                    eprintln!("context-run --audit: --context-budget takes an integer");
                    return 2;
                }
            },
            "--json" | "-J" => json_out = true,
            other => {
                eprintln!("context-run --audit: unknown argument `{other}`");
                eprintln!(
                    "usage: fno-agents context-run --audit --source <plugin-root> \
[--context-host <dir>] [--context-harness <harness|all>] \
[--context-entry <state|all>] [--context-budget <n>] [--json]"
                );
                return 2;
            }
        }
    }
    let plugin_root = source.unwrap_or(".");
    let host_root = host.unwrap_or(plugin_root);
    let harnesses: Vec<&str> = if harness_filter == "all" {
        SUPPORTED_HARNESSES.to_vec()
    } else {
        vec![harness_filter]
    };
    let entry_states: Vec<&str> = if entry_filter == "all" {
        SUPPORTED_ENTRY_STATES.to_vec()
    } else {
        vec![entry_filter]
    };
    let report = build_context_report(
        Path::new(host_root),
        Path::new(plugin_root),
        &harnesses,
        &entry_states,
        budget,
        1,
    );
    match report {
        Ok(report) => {
            if json_out {
                println!("{}", report.to_string());
            } else {
                println!("fno doctor --context-audit");
                for cell in report
                    .get("cells")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    let compiled = cell.get("compiled").cloned().unwrap_or(json!({}));
                    let packet = compiled.get("packet").cloned().unwrap_or(json!({}));
                    let dup_count = compiled
                        .get("duplicates")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    let con_count = compiled
                        .get("conflicts")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    let omit_count = compiled
                        .get("omitted_sources")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    let harness_word = cell.get("harness").and_then(Value::as_str).unwrap_or("");
                    let entry_word = cell
                        .get("entry_state")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    println!(
                        "  {harness_word}/{entry_word}: {}/{budget} bytes; \
{dup_count} duplicate group(s); {con_count} conflict(s); {omit_count} omitted",
                        packet
                            .get("bytes")
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        budget = packet
                            .get("budget_bytes")
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("context-run --audit: {e}");
            return 2;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, content: &[u8], layer: &str, ordinal: i64) -> ContextSource {
        let mut s = ContextSource::new(
            id,
            "claude",
            "startup",
            "session_start",
            layer,
            "fixture",
            "fixture",
            ordinal,
        );
        s.content = Some(content.to_vec());
        s
    }

    #[test]
    fn fingerprints_sources_and_deduplicates_content() {
        let shared = b"Never hand-edit managed state.\n".to_vec();
        let sources = vec![
            source("kernel", b"Footnote safety kernel.\n", "kernel", 0),
            source("guard-a", &shared, "progressive", 1),
            source("guard-b", &shared, "progressive", 2),
        ];
        let report = compile_context(sources, 4096, 1).unwrap();
        let records = report
            .get("source_manifest")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(records.len(), 3);
        for record in records {
            assert_eq!(
                record.get("harness").and_then(Value::as_str),
                Some("claude")
            );
            assert_eq!(
                record.get("estimated_tokens").and_then(Value::as_u64),
                Some((record.get("bytes").and_then(Value::as_u64).unwrap_or(0) + 3) / 4)
            );
        }
        let duplicates = report.get("duplicates").and_then(Value::as_array).unwrap();
        assert_eq!(duplicates.len(), 1);
        assert_eq!(
            duplicates[0]
                .get("source_ids")
                .and_then(Value::as_array)
                .map(|a| a.len()),
            Some(2)
        );
        let kernel_ids = report
            .get("kernel")
            .and_then(|k| k.get("source_ids"))
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(kernel_ids.len(), 1);
        let progressive = report
            .get("progressive_sources")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(progressive.len(), 1);
    }

    #[test]
    fn budget_omission_and_continuation() {
        let sources = vec![
            source("kernel", b"1234", "kernel", 0),
            source("first", b"5678", "progressive", 1),
            source("second", b"90", "progressive", 2),
        ];
        let report = compile_context(sources, 8, 1).unwrap();
        let packet = report.get("packet").unwrap();
        assert_eq!(packet.get("bytes").and_then(Value::as_u64), Some(8));
        assert_eq!(
            packet.get("within_budget").and_then(Value::as_bool),
            Some(true)
        );
        let progressive = report
            .get("progressive_sources")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(progressive.len(), 1);
        let omitted = report
            .get("omitted_sources")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(omitted.len(), 1);
        assert_eq!(
            omitted[0].get("reason").and_then(Value::as_str),
            Some("packet_budget")
        );
        let continuation = report.get("continuation").unwrap();
        assert_eq!(
            continuation.get("after_source_id").and_then(Value::as_str),
            Some("first")
        );
        let remaining = continuation
            .get("remaining_source_ids")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn oversized_kernel_never_allows_progressive_content() {
        let sources = vec![
            source("kernel", b"kernel-too-large", "kernel", 0),
            source("small-progressive", b"x", "progressive", 1),
        ];
        let report = compile_context(sources, 4, 1).unwrap();
        assert_eq!(
            report
                .get("packet")
                .and_then(|p| p.get("bytes"))
                .and_then(Value::as_u64),
            Some(0)
        );
        let kernel_ids = report
            .get("kernel")
            .and_then(|k| k.get("source_ids"))
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(kernel_ids.len(), 0);
        let progressive = report
            .get("progressive_sources")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(progressive.len(), 0);
        let remaining = report
            .get("continuation")
            .and_then(|c| c.get("remaining_source_ids"))
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn marker_state_and_extract_block() {
        assert_eq!(marker_state("no markers"), "none");
        let good = "<!-- fno:begin v=1 -->\nbody\n<!-- fno:end -->";
        assert_eq!(marker_state(good), "both");
        assert_eq!(extract_block(good).as_deref(), Some(good));
        let malformed = "<!-- fno:begin v=1 -->\nbody\n<!-- fno:end -->\n<!-- fno:end -->";
        assert_eq!(marker_state(malformed), "malformed");
        assert_eq!(extract_block(malformed), None);
    }
}

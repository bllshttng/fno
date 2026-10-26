//! The `doc` deliverable: a cited brief `<slug>.md` plus its evidence
//! sidecar `<slug>.sources.jsonl`, both written to
//! `config.research.output_dir`.
//!
//! Every claim cites an `[Sn]` marker that resolves in `## Sources` to a URL
//! in the sidecar. The sidecar carries the full store, verified or not, so
//! dead URLs stay visible to the eval. The frontmatter records why the
//! investigation stopped. An unset `output_dir` fails loud: the ship step
//! never guesses a landing path.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::engine::{self, Source};
use super::pyfmt;

const SNIPPET_CHARS: usize = 240;
const PATH_MAX_BYTES: usize = 4096;

pub const OUTPUT_DIR_UNSET: &str = "config.research.output_dir is unset. Set it (e.g. \
     `fno config set config.research.output_dir <vault-area>`) - the research ship step \
     never guesses a landing path.";

#[derive(Debug)]
pub enum DeliverError {
    /// `config.research.output_dir` is unset (exit 5).
    OutputDirUnset,
    /// A write failed (exit 6).
    Io(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeliverResult {
    pub topic: String,
    pub slug: String,
    pub brief_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub found: usize,
    pub verified: usize,
    /// Set when a node was in scope but the plan_path bind failed. The
    /// deliverable ships either way: a lost doc is worse than an unbound one.
    pub bind_warning: Option<String>,
}

/// `OSError.__str__`: `[Errno N] <strerror>: '<path>'`.
pub fn os_error_text(e: &std::io::Error, path: &Path) -> String {
    match e.raw_os_error() {
        Some(code) => {
            let text = std::io::Error::from_raw_os_error(code).to_string();
            let strerror = match text.rfind(" (os error ") {
                Some(i) => text[..i].to_string(),
                None => text,
            };
            format!(
                "[Errno {code}] {strerror}: {}",
                pyfmt::repr(&path.display().to_string())
            )
        }
        None => e.to_string(),
    }
}

/// `Path(text)` spelling: empty and `.` components dropped, repeated
/// slashes folded.
fn pure_path(text: &str) -> PathBuf {
    let parts: Vec<&str> = text
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    let joined = parts.join("/");
    if text.starts_with('/') {
        PathBuf::from(format!("/{joined}"))
    } else if joined.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(joined)
    }
}

/// `Path(dir) / name`: a `.` dir adds no prefix.
fn child(dir: &Path, name: &str) -> PathBuf {
    if dir == Path::new(".") {
        PathBuf::from(name)
    } else {
        dir.join(name)
    }
}

/// `Path.expanduser()` for `~` and `~/...`.
fn expand_user(path: &str) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            let rest = &path[1..];
            return pure_path(&format!("{}{rest}", home.to_string_lossy()));
        }
    }
    pure_path(path)
}

/// Expand and create the configured output dir; unset or blank refuses.
pub fn resolve_output_dir(configured: Option<&str>) -> Result<PathBuf, DeliverError> {
    let trimmed = configured.map(pyfmt::strip).unwrap_or("");
    if trimmed.is_empty() {
        return Err(DeliverError::OutputDirUnset);
    }
    let out = expand_user(trimmed);
    std::fs::create_dir_all(&out).map_err(|e| DeliverError::Io(os_error_text(&e, &out)))?;
    Ok(out)
}

pub fn snippet(text: &str) -> String {
    let joined = pyfmt::split_ws(text).join(" ");
    let capped: String = joined.chars().take(SNIPPET_CHARS).collect();
    pyfmt::strip(&capped).to_string()
}

fn extract_text(v: &Value) -> String {
    if pyfmt::truthy(v) {
        pyfmt::py_str(v)
    } else {
        String::new()
    }
}

/// Render the brief: one cited claim per verified source. `stopped` lands
/// in the frontmatter verbatim ("declared" or "cap N").
pub fn build_brief(
    topic: &str,
    slug: &str,
    sources: &[Source],
    stopped: &str,
    created: &str,
) -> String {
    let verified: Vec<&Source> = sources.iter().filter(|s| s.verified).collect();
    let mut lines: Vec<String> = vec![
        "---".to_string(),
        format!("topic: \"{topic}\""),
        format!("slug: {slug}"),
        format!("created: {created}"),
        format!("stopped: {stopped}"),
        format!("sources: {slug}.sources.jsonl"),
        format!("found: {}", sources.len()),
        format!("verified: {}", verified.len()),
    ];
    if verified.is_empty() {
        lines.push("note: no sources found".to_string());
    }
    lines.push("---".to_string());
    lines.push(String::new());
    lines.push(format!("# {topic}"));
    lines.push(String::new());
    lines.push("## Findings".to_string());
    lines.push(String::new());
    if verified.is_empty() {
        lines.push(
            "No sources found for this topic. The evidence store is empty; this brief is \
             stamped accordingly so the eval reports the gap rather than treating a thin \
             result as complete."
                .to_string(),
        );
        lines.push(String::new());
        return lines.join("\n") + "\n";
    }
    let mut refs = Vec::new();
    for (i, s) in verified.iter().enumerate() {
        let n = i + 1;
        let mut claim = snippet(&extract_text(&s.extract));
        if claim.is_empty() {
            claim = "(extracted source; see citation)".to_string();
        }
        lines.push(format!("- {claim} [S{n}]"));
        refs.push(format!("[S{n}]: {}", pyfmt::py_str(&s.url)));
    }
    lines.push(String::new());
    lines.push("## Sources".to_string());
    lines.push(String::new());
    lines.extend(refs);
    lines.push(String::new());
    lines.join("\n") + "\n"
}

/// Bind the shipped brief to `node` as its plan_path through the backlog
/// update command (the one door every plan_path write goes through). It
/// runs as a child of this binary so its receipt is captured, never
/// printed; a failure comes back as the warning text.
fn bind_plan_path(node: &str, brief_path: &Path) -> Option<String> {
    let fail = |detail: String| {
        Some(format!(
            "could not bind {} to {node}: {detail}",
            brief_path.display()
        ))
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return fail(e.to_string()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args([
        "backlog".to_string(),
        "update".to_string(),
        node.to_string(),
        format!("plan_path={}", brief_path.display()),
    ]);
    match crate::bounded_cmd::output_with_timeout_result(cmd, BIND_TIMEOUT_SECS) {
        Err(e) => fail(e.to_string()),
        Ok(out) if out.status.success() => None,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let detail = if stderr.is_empty() {
                String::from_utf8_lossy(&out.stdout).into_owned()
            } else {
                stderr
            };
            fail(pyfmt::strip(&detail).to_string())
        }
    }
}

const BIND_TIMEOUT_SECS: u64 = 30;

/// Ship the brief and the full-store sidecar to `output_dir`. `node`, when
/// given, gets the brief bound as its plan_path (best-effort).
pub fn deliver(
    topic: &str,
    sources_path: &Path,
    stopped: &str,
    output_dir: Option<&str>,
    node: Option<&str>,
) -> Result<DeliverResult, DeliverError> {
    let out = resolve_output_dir(output_dir)?;
    let slug = engine::slugify(topic);
    let sources = engine::read_sources(sources_path)
        .map_err(|e| DeliverError::Io(os_error_text(&e, sources_path)))?;
    let verified = sources.iter().filter(|s| s.verified).count();
    let brief_path = child(&out, &format!("{slug}.md"));
    let sidecar_path = child(&out, &format!("{slug}.sources.jsonl"));

    let created = pyfmt::isoformat(&chrono::Utc::now());
    let brief = build_brief(topic, &slug, &sources, stopped, &created);
    std::fs::write(&brief_path, brief)
        .map_err(|e| DeliverError::Io(os_error_text(&e, &brief_path)))?;
    let mut sidecar = String::new();
    for s in &sources {
        sidecar.push_str(&s.to_json_line());
        sidecar.push('\n');
    }
    std::fs::write(&sidecar_path, sidecar)
        .map_err(|e| DeliverError::Io(os_error_text(&e, &sidecar_path)))?;

    let bind_warning = node.and_then(|n| bind_plan_path(n, &brief_path));
    Ok(DeliverResult {
        topic: topic.to_string(),
        slug,
        brief_path,
        sidecar_path,
        found: sources.len(),
        verified,
        bind_warning,
    })
}

/// Best-effort DoneAdvisory termination event. A missing journal dir or a
/// failed write is swallowed: the deliverable already landed.
pub fn emit_done_advisory(events_path: &Path, slug: &str) {
    if !events_path.parent().is_some_and(Path::is_dir) {
        return;
    }
    let mut data = serde_json::Map::new();
    data.insert("session_id".into(), Value::String(String::new()));
    data.insert("reason".into(), Value::String("DoneAdvisory".into()));
    data.insert(
        "message".into(),
        Value::String(format!("research deliverable {slug} shipped")),
    );
    data.insert("slug".into(), Value::String(slug.to_string()));
    let emitter = crate::events::EventEmitter::new(events_path, "hook");
    let _ = emitter.emit_fields("termination", data);
}

/// `config.research.output_dir` from one parsed settings document, flat or
/// under the `config:` wrapper. `Some(None)` when the value is present but
/// the loader would refuse it (a glob character, over PATH_MAX, not a
/// string): the command reads a refused settings file as unset.
fn output_dir_in(doc: &Value) -> Option<Option<String>> {
    let value = doc
        .get("research")
        .and_then(|r| r.get("output_dir"))
        .or_else(|| {
            doc.get("config")
                .and_then(|c| c.get("research"))
                .and_then(|r| r.get("output_dir"))
        })?;
    match value {
        Value::Null => None,
        Value::String(s) => {
            let refused = s.contains(['*', '?', '[']) || s.len() > PATH_MAX_BYTES;
            Some(if refused { None } else { Some(s.clone()) })
        }
        _ => Some(None),
    }
}

/// The first settings document that sets `output_dir` decides it.
pub fn output_dir_from_docs(docs: &[Value]) -> Option<String> {
    docs.iter().find_map(output_dir_in).flatten()
}

/// `output_dir` from one settings.yaml text (the golden seam).
pub fn output_dir_from_yaml(text: &str) -> Option<String> {
    let doc = serde_yaml_ng::from_str::<Value>(text).unwrap_or(Value::Null);
    output_dir_from_docs(&[doc])
}

/// `config.research.output_dir` through the shared settings walk.
pub fn configured_output_dir() -> Option<String> {
    output_dir_from_docs(&crate::backlog::settings::config_candidates())
}

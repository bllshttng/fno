//! Postmortem artifacts as a retro source.
//!
//! Two on-disk formats coexist: the legacy target postmortem (YAML
//! frontmatter with `blocked_reason: {kind: ...}`) and the finalize
//! artifact (no frontmatter, a `- termination: **Reason**` bullet). Harvest
//! reads both. The `consumed_at` stamp adds a frontmatter block when none
//! exists, so a re-drain skips what an earlier run dispositioned.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::classify::py_splitlines;
use super::{py_str, take_chars, truthy, RawItem, KIND_POSTMORTEM};

const GIST_LINES: usize = 40;
const GIST_CHARS: usize = 4000;

/// The postmortems dir: `paths.postmortems_dir` from config, else
/// `<state dir>/postmortems`.
pub fn postmortems_dir(cwd: &Path) -> Option<PathBuf> {
    let configured = crate::agents_config::config_lookup(cwd, &["paths", "postmortems_dir"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.trim().is_empty());
    if let Some(raw) = configured {
        let p = crate::backlog::settings::expand_home(&raw);
        return Some(if p.is_absolute() { p } else { cwd.join(p) });
    }
    Some(crate::agents_config::state_dir(cwd)?.join("postmortems"))
}

/// `(frontmatter, body)`. `None` frontmatter when the text opens with no
/// fence or never closes one; a frontmatter that is not a mapping reads as
/// empty. Malformed YAML is an error, never a silent empty.
pub fn split_frontmatter(text: &str) -> Result<(Option<Map<String, Value>>, String), String> {
    let Some(after) = text.strip_prefix("---") else {
        return Ok((None, text.to_string()));
    };
    let rest = after.trim_start_matches('\n');
    if let Some(body) = rest.strip_prefix("---") {
        return Ok((Some(Map::new()), body.trim_start_matches('\n').to_string()));
    }
    let Some(idx) = rest.find("\n---") else {
        return Ok((None, text.to_string()));
    };
    let block = &rest[..idx];
    let fm = if block.trim().is_empty() {
        Map::new()
    } else {
        match serde_yaml_ng::from_str::<Value>(block) {
            Ok(Value::Object(map)) => map,
            Ok(_) => Map::new(),
            Err(e) => return Err(format!("bad frontmatter: {e}")),
        }
    };
    Ok((
        Some(fm),
        rest[idx + 4..].trim_start_matches('\n').to_string(),
    ))
}

fn termination_reason(text: &str) -> String {
    static TERM: OnceLock<Regex> = OnceLock::new();
    TERM.get_or_init(|| {
        Regex::new(r"(?m)^-\s*termination:\s*\*{0,2}([A-Za-z_]+)\*{0,2}")
            .expect("termination regex")
    })
    .captures(text)
    .map(|c| c[1].to_string())
    .unwrap_or_default()
}

/// Unconsumed postmortems in name order, each carrying a bounded gist. A
/// malformed file warns and is skipped, never an abort.
pub fn harvest_postmortems(dir: &Path, warnings: &mut Vec<String>) -> Vec<RawItem> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md") && p.is_file())
        .collect();
    paths.sort();
    let mut items = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| split_frontmatter(&text).map(|(fm, _)| (text, fm)));
        let (text, fm) = match parsed {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("postmortem {name}: {e}; skipped"));
                continue;
            }
        };
        let fm = fm.unwrap_or_default();
        if truthy(fm.get("consumed_at")) {
            continue;
        }
        let mut reason = match fm.get("blocked_reason") {
            Some(Value::Object(b)) if truthy(b.get("kind")) => py_str(b.get("kind")),
            _ => String::new(),
        };
        if reason.is_empty() {
            reason = termination_reason(&text);
        }
        let gist = take_chars(
            &py_splitlines(&text)
                .into_iter()
                .take(GIST_LINES)
                .collect::<Vec<_>>()
                .join("\n"),
            GIST_CHARS,
        );
        // The filename stem (date plus session prefix) keeps titles unique
        // when the finalize format carries no invocation.
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let invocation = if truthy(fm.get("target_invocation")) {
            py_str(fm.get("target_invocation")).trim().to_string()
        } else {
            String::new()
        };
        let invocation = if invocation.is_empty() {
            stem
        } else {
            invocation
        };
        let hint = format!(
            "postmortem {}: {invocation}",
            if reason.is_empty() {
                "unknown"
            } else {
                &reason
            }
        );
        items.push(RawItem {
            kind: KIND_POSTMORTEM.to_string(),
            text: gist,
            source_id: format!("postmortem:{name}"),
            title_hint: Some(hint),
            subkind: (!reason.is_empty()).then_some(reason),
            ..RawItem::default()
        });
    }
    items
}

/// The text with `consumed_at: <ts>` stamped into its frontmatter; `None`
/// when already stamped. No frontmatter gets a minimal block prepended.
pub fn stamped_text(text: &str, ts: &str) -> Result<Option<String>, String> {
    let (fm, _) = split_frontmatter(text)?;
    match fm {
        Some(fm) if truthy(fm.get("consumed_at")) => Ok(None),
        None => Ok(Some(format!("---\nconsumed_at: {ts}\n---\n{text}"))),
        Some(_) => {
            let rest = text[3..].trim_start_matches('\n');
            if rest.starts_with("---") {
                return Ok(Some(format!("---\nconsumed_at: {ts}\n{rest}")));
            }
            let idx = rest.find("\n---").unwrap_or(rest.len());
            let split = text.len() - rest.len() + idx;
            Ok(Some(format!(
                "{}\nconsumed_at: {ts}{}",
                &text[..split],
                &text[split..]
            )))
        }
    }
}

/// Stamp `consumed_at` into a postmortem file (idempotent).
pub fn stamp_consumed(path: &Path, ts: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if let Some(new) = stamped_text(&text, ts)? {
        std::fs::write(path, new).map_err(|e| e.to_string())?;
    }
    Ok(())
}

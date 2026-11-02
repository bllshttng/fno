//! The canonical acceptance-criteria compiler.
//!
//! The port of `cli/src/fno/plan/criteria.py` (deleted 2026-11-02).

use regex::Regex;
use std::sync::OnceLock;

/// One compiled criterion: a stable code, the verbatim statement, a behavior
/// kind from the `-TYPE` suffix (else GENERAL), and a `line N` position.
#[derive(Debug, Clone, PartialEq)]
pub struct Criterion {
    pub code: String,
    pub text: String,
    pub kind: String,
    pub source_ref: String,
}

/// A non-empty Acceptance Criteria body could not be compiled.
#[derive(Debug)]
pub struct CriteriaParseError(pub String);

impl std::fmt::Display for CriteriaParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Compile-once static regexes, the crate's `OnceLock` idiom.
fn leading_code_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new("^(AC\\d+[a-z]*-[A-Z]+|AC\\d+[a-z]*)\\b[\\s:.\\-]*").unwrap())
}

fn code_number_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new("AC(\\d+)").unwrap())
}

fn bold_ac_line_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"^\s*\*\*(AC\d+[a-z]*(?:-[A-Z]+)?)(?:\s*\([^)\n]*\))?\s*:?\s*\*\*\s*(.*)$")
            .unwrap()
    })
}

fn gwt_prefix_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\s*(?:Given|When|Then|And)\b\s+").unwrap())
}

fn list_marker_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\s*(?:[-*+]|\d+\.)\s+").unwrap())
}

fn table_sep_cell_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^:?-+:?$").unwrap())
}

fn fence_open_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^[ \t]*(`{3,}|~{3,})").unwrap())
}

fn heading_hashes_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^#+\s*").unwrap())
}

// --- line classifiers ----------------------------------------------------

fn is_heading(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn is_bold_ac(line: &str) -> bool {
    bold_ac_line_re().is_match(line)
}

fn is_table(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn is_list(line: &str) -> bool {
    list_marker_re().is_match(line)
}

fn is_gwt(line: &str) -> bool {
    gwt_prefix_re().is_match(line)
}

fn fence_delim(line: &str) -> Option<String> {
    fence_open_re()
        .captures(line)
        .map(|c| c.get(1).unwrap().as_str().to_string())
}

fn is_fence(line: &str) -> bool {
    fence_delim(line).is_some()
}

fn is_anchor(line: &str) -> bool {
    is_heading(line) || is_bold_ac(line) || is_table(line) || is_list(line)
}

/// If `text` opens with an explicit AC token, return (code, kind, rest).
fn split_leading_code(text: &str) -> (Option<String>, String, String) {
    let m = match leading_code_re().captures(text) {
        Some(m) => m,
        None => return (None, String::new(), text.to_string()),
    };
    let code = m.get(1).unwrap().as_str().to_string();
    let kind = match code.split_once('-') {
        Some((_, k)) => k.to_string(),
        None => "GENERAL".to_string(),
    };
    let end = m.get(0).unwrap().end();
    let rest = text[end..].trim().to_string();
    (Some(code), kind, rest)
}

fn code_number(code: &str) -> Option<u32> {
    code_number_re()
        .captures(code)
        .and_then(|c| c.get(1))
        .and_then(|g| g.as_str().parse().ok())
}

/// Continuation lines (non-blank, non-anchor) from `start`.
fn gather_until_break(lines: &[&str], start: usize) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut j = start;
    while j < lines.len() {
        let s = lines[j];
        if s.trim().is_empty() || is_anchor(s) || is_fence(s) {
            break;
        }
        out.push(s.trim().to_string());
        j += 1;
    }
    (out, j)
}

/// A contiguous Given/When/Then block from `start`.
fn gather_gwt(lines: &[&str], start: usize) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut j = start;
    while j < lines.len() {
        let s = lines[j];
        if s.trim().is_empty() || !is_gwt(s) || is_fence(s) {
            break;
        }
        out.push(s.trim().to_string());
        j += 1;
    }
    (out, j)
}

// --- table rows ----------------------------------------------------------

fn split_table_row(line: &str) -> Vec<String> {
    let mut s = line.trim().to_string();
    if s.starts_with('|') {
        s.remove(0);
    }
    if s.ends_with('|') {
        s.pop();
    }
    s.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_separator_row(cells: &[String]) -> bool {
    let non_empty: Vec<&String> = cells.iter().filter(|c| !c.trim().is_empty()).collect();
    !non_empty.is_empty()
        && non_empty
            .iter()
            .all(|c| table_sep_cell_re().is_match(c.trim()))
}

/// Compile a contiguous table block into (code, kind, text, line_offset).
fn table_data_rows(block: &[&str]) -> Vec<(Option<String>, String, String, usize)> {
    let rows: Vec<Vec<String>> = block.iter().map(|l| split_table_row(l)).collect();
    let mut sep_idx: Option<usize> = None;
    for (idx, cells) in rows.iter().enumerate() {
        if is_separator_row(cells) {
            sep_idx = Some(idx);
            break;
        }
    }
    let mut out = Vec::new();
    for (idx, cells) in rows.iter().enumerate() {
        if cells.is_empty() || is_separator_row(cells) {
            continue;
        }
        if sep_idx.is_some_and(|s| idx == s - 1) {
            continue; // header row
        }
        let (code, kind, first_rest) = split_leading_code(&cells[0]);
        let mut text_cells: Vec<String> = Vec::new();
        if !first_rest.is_empty() {
            text_cells.push(first_rest);
        }
        for c in cells.iter().skip(1) {
            if !c.trim().is_empty() {
                text_cells.push(c.clone());
            }
        }
        let text = text_cells.join(" ").trim().to_string();
        let kind = if kind.is_empty() {
            "GENERAL".to_string()
        } else {
            kind
        };
        out.push((code, kind, text, idx));
    }
    out
}

/// Next document-order synthesized number, skipping explicit-claimed ones.
fn next_synth(counter: u32, used: &std::collections::HashSet<u32>) -> u32 {
    let mut n = counter + 1;
    while used.contains(&n) {
        n += 1;
    }
    n
}

/// Assign synthesized codes, detect duplicate explicit identifiers, and drop
/// markers that yielded no statement. The Python leg's `_finalize`.
fn finalize(
    raw: Vec<(usize, Option<String>, String, String)>,
    source: &str,
) -> Result<Vec<Criterion>, CriteriaParseError> {
    let raw: Vec<_> = raw.into_iter().filter(|r| !r.3.trim().is_empty()).collect();
    let mut explicit_numbers: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (_line, code, _kind, _text) in &raw {
        if let Some(code) = code {
            if let Some(num) = code_number(code) {
                explicit_numbers.insert(num);
            }
        }
    }
    let mut synth: u32 = 0;
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for (line_no, code, kind, text) in raw {
        let code = match code {
            None => {
                synth = next_synth(synth, &explicit_numbers);
                let c = format!("AC{synth}");
                ("GENERAL".to_string(), c)
            }
            Some(c) => {
                if let Some(prev) = seen.get(&c) {
                    return Err(CriteriaParseError(format!(
                        "duplicate acceptance identifier {:?} in {}: first at line {}, again at line {}. Rename one so task ownership stays unambiguous.",
                        c, source, prev, line_no
                    )));
                }
                (kind, c)
            }
        };
        seen.insert(code.1.clone(), line_no);
        out.push(Criterion {
            code: code.1,
            text,
            kind: code.0,
            source_ref: format!("line {line_no}"),
        });
    }
    Ok(out)
}

/// The refusal text a non-empty body with no recognized shape raises.
fn no_shape_error(source: &str) -> CriteriaParseError {
    CriteriaParseError(format!(
        "{source} has content but no recognized criterion shape. \
Rewrite each criterion in one of the accepted forms:\n  - **AC1-HP:** Given ... when ... then ...   (legacy bold id)\n  - ### AC1-HP: Title  /  ### Title           (heading)\n  - 1. Given ... when ... then ...            (numbered item)\n  - - Given ... when ... then ...             (bullet)\n  - | label | Given | When | Then |           (table row)\n  - A descriptive label followed by contiguous Given/When/Then lines\nOr leave the section truly empty if the work is not yet decomposed."
    ))
}

/// Compile an Acceptance Criteria section body into ordered criteria.
/// `[]` for a truly empty section; the shaped refusals otherwise.
pub fn compile_criteria(
    section_body: &str,
    source: &str,
) -> Result<Vec<Criterion>, CriteriaParseError> {
    let body = section_body;
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let lines: Vec<&str> = body.lines().collect();
    let n = lines.len();
    let mut raw: Vec<(usize, Option<String>, String, String)> = Vec::new();
    let mut i = 0usize;
    let mut in_fence = false;
    let mut fence_close = String::new();
    while i < n {
        let line = lines[i];
        let delim = fence_delim(line);
        if let Some(d) = delim {
            if !in_fence {
                in_fence = true;
                fence_close = d;
            } else if fence_close.starts_with(&d[..1]) && d.len() >= fence_close.len() {
                in_fence = false;
                fence_close.clear();
            }
            i += 1;
            continue;
        }
        if in_fence || line.trim().is_empty() {
            i += 1;
            continue;
        }
        if let Some(m) = bold_ac_line_re().captures(line) {
            let code = m.get(1).unwrap().as_str().to_string();
            let kind = match code.split_once('-') {
                Some((_, k)) => k.to_string(),
                None => "GENERAL".to_string(),
            };
            let mut parts: Vec<String> = Vec::new();
            let g2 = m.get(2).unwrap().as_str().trim().to_string();
            if !g2.is_empty() {
                parts.push(g2);
            }
            let (cont, j) = gather_until_break(&lines, i + 1);
            parts.extend(cont);
            let text = parts.join(" ").trim().to_string();
            raw.push((i + 1, Some(code), kind, text));
            i = j;
            continue;
        }
        if is_heading(line) {
            let title = heading_hashes_re().replace(line.trim(), "").to_string();
            let (code, kind, rest) = split_leading_code(&title);
            let mut parts: Vec<String> = Vec::new();
            if !rest.is_empty() {
                parts.push(rest);
            }
            let (cont, j) = gather_until_break(&lines, i + 1);
            parts.extend(cont);
            let text = parts.join(" ").trim().to_string();
            let kind = if kind.is_empty() {
                "GENERAL".to_string()
            } else {
                kind
            };
            raw.push((i + 1, code, kind, text));
            i = j;
            continue;
        }
        if is_table(line) {
            let mut j = i;
            while j < n && lines[j].trim_start().starts_with('|') {
                j += 1;
            }
            for (rcode, rkind, rtext, offset) in table_data_rows(&lines[i..j]) {
                if !rtext.is_empty() {
                    raw.push((i + 1 + offset, rcode, rkind, rtext));
                }
            }
            i = j;
            continue;
        }
        if is_list(line) {
            let item = list_marker_re().replace(line, "").to_string();
            let (code, kind, rest) = split_leading_code(&item);
            let mut parts: Vec<String> = Vec::new();
            if !rest.is_empty() {
                parts.push(rest);
            }
            let (cont, j) = gather_until_break(&lines, i + 1);
            parts.extend(cont);
            let text = parts.join(" ").trim().to_string();
            let kind = if kind.is_empty() {
                "GENERAL".to_string()
            } else {
                kind
            };
            raw.push((i + 1, code, kind, text));
            i = j;
            continue;
        }
        if is_gwt(line) {
            let (gwt, j) = gather_gwt(&lines, i);
            if !gwt.is_empty() {
                let text = gwt.join(" ").trim().to_string();
                raw.push((i + 1, None, "GENERAL".to_string(), text));
            }
            i = j;
            continue;
        }
        let mut k = i + 1;
        while k < n && lines[k].trim().is_empty() {
            k += 1;
        }
        if k < n && is_gwt(lines[k]) {
            let (code, kind, rest) = split_leading_code(line.trim());
            let (gwt, j) = gather_gwt(&lines, k);
            let mut parts: Vec<String> = Vec::new();
            if !rest.is_empty() {
                parts.push(rest);
            }
            parts.extend(gwt);
            let text = parts.join(" ").trim().to_string();
            let kind = if kind.is_empty() {
                "GENERAL".to_string()
            } else {
                kind
            };
            raw.push((i + 1, code, kind, text));
            i = j;
            continue;
        }
        i += 1;
    }
    let criteria = finalize(raw, source)?;
    if criteria.is_empty() {
        return Err(no_shape_error(source));
    }
    Ok(criteria)
}

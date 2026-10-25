//! The research-brief grader (the deleted Python leg
//! research_grade.py, ported per the dual-implementation protocol).
//! No model in the gate; the goldens under
//! crates/fno-agents/tests/golden/evals/ pin the bytes.

use regex::Regex;

/// Setup errors exit 2, never a verdict.
const USAGE_EXIT: i32 = 2;

pub fn run(args: &[String]) -> i32 {
    let brief = flag_value(args, "--brief");
    let golden = flag_value(args, "--golden");
    let sidecar = flag_value(args, "--sidecar");
    match (brief, golden) {
        (Some(b), Some(g)) => match grade(&b, &g, sidecar.as_deref()) {
            Ok(result) => {
                println!("{}", result.summary());
                if result.green() {
                    0
                } else {
                    1
                }
            }
            Err(text) => {
                eprintln!("Error: {text}");
                USAGE_EXIT
            }
        },
        _ => {
            eprintln!("Error: --brief/--golden are required");
            USAGE_EXIT
        }
    }
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == name {
            return iter.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn normalize(s: &str) -> String {
    let lowered = s.to_lowercase();
    let mut out = String::new();
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// Split a leading --- frontmatter block; only the flat scalar keys we need
/// (sources:) are parsed. Returns (fields, body).
fn strip_frontmatter(text: &str) -> (std::collections::HashMap<String, String>, String) {
    let mut fields = std::collections::HashMap::new();
    if !text.starts_with("---") {
        return (fields, text.to_string());
    }
    let Some(end) = text[3..].find("\n---").map(|i| i + 3) else {
        return (fields, text.to_string());
    };
    let fm = &text[3..end];
    for line in fm.splitlines() {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let key = line[..colon].trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let mut val = line[colon + 1..].trim().to_string();
        for q in ['"', chr(39)] {
            if val.starts_with(q) && val.ends_with(q) && val.len() >= 2 {
                val = val[1..val.len() - 1].to_string();
                break;
            }
        }
        fields.insert(key.to_string(), val);
    }
    let body_start = end + 4;
    let body = if body_start <= text.len() {
        text[body_start..].to_string()
    } else {
        String::new()
    };
    (fields, body)
}

/// (name, section_text) for each `## ` section except Sources.
fn content_sections(body: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<String> = None;
    let h2 = Regex::new(r"^##\s+(.+?)\s*#*$").expect("static regex");
    for line in body.splitlines() {
        if let Some(caps) = h2.captures(line) {
            let name = caps
                .get(1)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            if name.to_lowercase() == "sources" {
                current = None;
                continue;
            }
            current = Some(name.clone());
            sections.push((name, Vec::new()));
        } else if let Some(entry) = sections.last_mut() {
            if current.is_some() {
                entry.1.push(line.to_string());
            }
        }
    }
    sections
        .into_iter()
        .map(|(n, ls)| (n, ls.join("\n")))
        .collect()
}

/// Every #{2,6} heading in the golden body: the checklist.
fn golden_checklist(golden_text: &str) -> Vec<String> {
    let (_, body) = strip_frontmatter(golden_text);
    let mut items = Vec::new();
    let heading = Regex::new(r"^#{2,6}\s+(.+?)\s*#*$").expect("static regex");
    for line in body.splitlines() {
        if let Some(caps) = heading.captures(line) {
            items.push(
                caps.get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default(),
            );
        }
    }
    items
}

fn resolve_sidecar(
    brief_path: &str,
    fields: &std::collections::HashMap<String, String>,
    sidecar: Option<&str>,
) -> std::path::PathBuf {
    if let Some(s) = sidecar {
        return std::path::PathBuf::from(s);
    }
    let p = std::path::Path::new(brief_path);
    if let Some(named) = fields.get("sources") {
        if !named.is_empty() {
            return p.parent().unwrap_or(std::path::Path::new(".")).join(named);
        }
    }
    p.with_file_name(format!("{}.sources.jsonl", stem_of(p)))
}

fn stem_of(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_default()
}

pub struct GradeResult {
    brief: String,
    golden: String,
    sidecar: String,
    pub uncited_claims: u32,
    pub dead_urls: u32,
    pub sections_uncovered: Vec<String>,
    pub detail: Vec<String>,
}

impl GradeResult {
    pub fn green(&self) -> bool {
        self.uncited_claims == 0 && self.dead_urls == 0 && self.sections_uncovered.is_empty()
    }

    /// The leaf's exact stdout: verdict, the three assertions, the details.
    pub fn summary(&self) -> String {
        let verdict = if self.green() { "GREEN" } else { "RED" };
        let mut lines = vec![
            format!("research grade: {verdict}"),
            format!("  (a) uncited claims:     {}", self.uncited_claims),
            format!("  (b) dead source URLs:   {}", self.dead_urls),
            format!(
                "  (c) uncovered sections: {}",
                self.sections_uncovered.len()
            ),
        ];
        if !self.sections_uncovered.is_empty() {
            lines[3] = format!(
                "  (c) uncovered sections: {} ({})",
                self.sections_uncovered.len(),
                self.sections_uncovered.join(", ")
            );
        }
        for d in &self.detail {
            lines.push(format!("    - {d}"));
        }
        lines.join("\n")
    }
}

/// Score a brief against a golden doc via the three mechanical assertions.
/// Err carries the door's `Error:` text (exit 2), the Python bytes.
pub fn grade(
    brief_path: &str,
    golden_path: &str,
    sidecar: Option<&str>,
) -> Result<GradeResult, String> {
    let brief = std::path::Path::new(brief_path);
    let golden = std::path::Path::new(golden_path);
    if !brief.is_file() {
        return Err(format!("brief not found: {brief_path}"));
    }
    if !golden.is_file() {
        return Err(format!("golden doc not found: {golden_path}"));
    }
    let brief_text = std::fs::read_to_string(brief)
        .map_err(|e| format!("brief not found: {brief_path} ({e})"))?;
    let golden_text = std::fs::read_to_string(golden)
        .map_err(|e| format!("golden doc not found: {golden_path} ({e})"))?;
    let (fields, body) = strip_frontmatter(&brief_text);
    let sidecar_path = resolve_sidecar(brief_path, &fields, sidecar);
    if !sidecar_path.exists() {
        return Err(format!(
            "sources sidecar not found: {}",
            sidecar_path.display()
        ));
    }
    let rows = read_sources(&sidecar_path).map_err(|e| {
        format!(
            "failed to read sources sidecar {}: {e}",
            sidecar_path.display()
        )
    })?;
    let sidecar_urls: std::collections::HashSet<&str> =
        rows.iter().map(|r| r.url.as_str()).collect();
    let refs = ref_map(&brief_text);
    let sections = content_sections(&body);
    let mut res = GradeResult {
        brief: brief_path.to_string(),
        golden: golden_path.to_string(),
        sidecar: sidecar_path.display().to_string(),
        uncited_claims: 0,
        dead_urls: 0,
        sections_uncovered: Vec::new(),
        detail: Vec::new(),
    };

    // (a) zero uncited claims.
    let claim = Regex::new(r"^\s*[-*]\s+(.+)$").expect("static regex");
    let cite = Regex::new(r"\[S([0-9]+)\]").expect("static regex");
    for (name, sect) in &sections {
        for line in sect.splitlines() {
            let Some(caps) = claim.captures(line) else {
                continue;
            };
            let cites: Vec<u32> = cite
                .captures_iter(line)
                .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse().ok()))
                .collect();
            let linked = cites.iter().any(|n| {
                refs.get(n)
                    .map(|u| sidecar_urls.contains(u.as_str()))
                    .unwrap_or(false)
            });
            if !linked {
                res.uncited_claims += 1;
                let text: String = caps
                    .get(1)
                    .map(|m| m.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect();
                res.detail
                    .push(format!("uncited claim in '{name}': {text}"));
            }
        }
    }

    // (b) zero dead source URLs.
    for r in &rows {
        let resolvable = r.verified || r.url.contains("web.archive.org");
        if !resolvable {
            res.dead_urls += 1;
            let why = if r.reason.is_empty() {
                "unverified"
            } else {
                r.reason.as_str()
            };
            res.detail.push(format!("dead source: {} ({})", r.url, why));
        }
    }

    // (c) >=1 golden checklist item per section.
    let checklist: Vec<String> = golden_checklist(&golden_text)
        .iter()
        .map(|i| normalize(i))
        .filter(|i| !i.is_empty())
        .collect();
    if checklist.is_empty() {
        return Err(format!(
            "golden doc has no checklist items (headings): {golden_path}"
        ));
    }
    if sections.is_empty() {
        res.sections_uncovered
            .push("(no content sections)".to_string());
    }
    for (name, sect) in &sections {
        let hay = normalize(&format!("{name} {sect}"));
        if !checklist.iter().any(|item| hay.contains(item.as_str())) {
            res.sections_uncovered.push(name.clone());
        }
    }
    Ok(res)
}

/// The ref map [Sn] -> url, parsed across the whole brief.
fn ref_map(text: &str) -> std::collections::HashMap<u32, String> {
    let refs = Regex::new(r"(?m)^\s*\[S([0-9]+)\]:\s*(\S+)").expect("static regex");
    let mut out = std::collections::HashMap::new();
    for caps in refs.captures_iter(text) {
        let n: u32 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let url = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        out.insert(n, url);
    }
    out
}

pub struct Source {
    url: String,
    verified: bool,
    reason: String,
}

/// Parse sources.jsonl, skipping malformed rows (best-effort, like the
/// Python reader the research family keeps).
fn read_sources(path: &std::path::Path) -> Result<Vec<Source>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(url) = d.get("url").and_then(serde_json::Value::as_str) else {
            continue;
        };
        out.push(Source {
            url: url.to_string(),
            verified: d
                .get("verified")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            reason: d
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    Ok(out)
}

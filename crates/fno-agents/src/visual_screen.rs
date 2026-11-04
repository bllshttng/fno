//! Visual-approval support. Two jobs live here.
//!
//! 1. The code-diff test: a paint diff that changes comments and code names
//!    only cannot move a pixel, so the gate clears it from the patch alone.
//! 2. The design critique: `visual critique-prep <node>` pairs the main and PR
//!    shots, boxes the changed regions, and writes a skeleton critique. The
//!    `design-critic` agent fills it. The gate reads it and files a look
//!    only when no high finding stands.
//!
//! Shots pair by name. The main side carries a `before` or `main` token. The
//! PR side carries `after` or no token, so `x-1-settings-dark.png` pairs with
//! `x-1-settings-before-dark.png`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One view at one theme, with the main shot and the PR shot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShotPair {
    pub view: String,
    pub before: String,
    pub after: String,
}

/// The changed rectangle of one pair, in PR-shot pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PairDiff {
    Identical,
    Changed(Region),
    SizeChanged,
}

fn split_token(stem: &str) -> (String, bool) {
    let parts: Vec<&str> = stem.split('-').collect();
    match parts
        .iter()
        .position(|p| matches!(*p, "before" | "main" | "after"))
    {
        Some(at) => {
            let main_side = parts[at] != "after";
            let mut rest = parts.clone();
            rest.remove(at);
            (rest.join("-"), main_side)
        }
        None => (stem.to_string(), false),
    }
}

/// Pair the shot names (file names or `folder/name`). A name with no partner
/// comes back second: the critic cannot compare a view it has half of.
pub(crate) fn pair_shots(shots: &[String]) -> (Vec<ShotPair>, Vec<String>) {
    let mut main: Vec<(String, String)> = Vec::new();
    let mut pr: Vec<(String, String)> = Vec::new();
    for name in shots {
        let (key, main_side) = split_token(name.trim_end_matches(".png"));
        if main_side {
            main.push((key, name.clone()));
        } else {
            pr.push((key, name.clone()));
        }
    }
    let mut pairs = Vec::new();
    let mut loose = Vec::new();
    for (key, b) in main {
        match pr.iter().position(|(k, _)| *k == key) {
            Some(i) => pairs.push(ShotPair {
                view: key,
                before: b,
                after: pr.remove(i).1,
            }),
            None => loose.push(b),
        }
    }
    loose.extend(pr.into_iter().map(|(_, n)| n));
    pairs.sort_by(|a, b| a.view.cmp(&b.view));
    loose.sort();
    (pairs, loose)
}

struct Rgba {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

fn decode(path: &Path) -> Result<Rgba, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("shot too large")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let raw = &buf[..info.buffer_size()];
    let px = match info.color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => raw
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::Grayscale => raw.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => raw
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Indexed => return Err(format!("{}: indexed png", path.display())),
    };
    Ok(Rgba {
        w: info.width,
        h: info.height,
        px,
    })
}

fn compare(a: &Rgba, b: &Rgba) -> PairDiff {
    if (a.w, a.h) != (b.w, b.h) {
        return PairDiff::SizeChanged;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut pixels = 0u64;
    for y in 0..a.h {
        for x in 0..a.w {
            let i = ((y * a.w + x) * 4) as usize;
            if a.px[i..i + 4] != b.px[i..i + 4] {
                pixels += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if pixels == 0 {
        return PairDiff::Identical;
    }
    PairDiff::Changed(Region {
        x: x0,
        y: y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
    })
}

/// Write a copy of the PR shot with a red frame around the changed region.
fn write_boxed(after: &Rgba, region: Region, out: &Path) -> Result<(), String> {
    let mut px = after.px.clone();
    let pad = 2u32;
    let x0 = region.x.saturating_sub(pad);
    let y0 = region.y.saturating_sub(pad);
    let x1 = (region.x + region.w + pad).min(after.w) - 1;
    let y1 = (region.y + region.h + pad).min(after.h) - 1;
    let mut paint = |x: u32, y: u32| {
        let i = ((y * after.w + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
    };
    for x in x0..=x1 {
        paint(x, y0);
        paint(x, y1);
    }
    for y in y0..=y1 {
        paint(x0, y);
        paint(x1, y);
    }
    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), after.w, after.h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(&px))
        .map_err(|e| format!("{}: {e}", out.display()))
}

/// One paired view in the critique file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ViewEntry {
    pub view: String,
    pub theme: String,
    pub before: String,
    pub after: String,
    pub identical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boxed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Finding {
    pub view: String,
    pub region: String,
    pub rule: String,
    pub severity: String,
    #[serde(default)]
    pub fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Stamp {
    pub agent: String,
    #[serde(default)]
    pub ran_at: String,
}

/// `<node>-critique.json`: prep writes the views, the critic fills the rest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Critique {
    pub head: String,
    #[serde(default)]
    pub views: Vec<ViewEntry>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub checked: Vec<String>,
    #[serde(default)]
    pub left: Vec<String>,
    #[serde(default)]
    pub critic: Option<Stamp>,
}

fn theme_of(view: &str) -> String {
    view.rsplit('-')
        .find(|t| matches!(*t, "dark" | "light"))
        .unwrap_or("")
        .to_string()
}

/// Pair the shots in `dir`, compare each pair, and box the changed PR shots
/// under `<node>-shots/boxed/`. A shot that cannot be read goes to `left`
/// and is never `identical`.
pub(crate) fn prep(node: &str, head: &str, shots: &[String], dir: &Path) -> Critique {
    let (pairs, loose) = pair_shots(shots);
    let mut out = Critique {
        head: head.to_string(),
        ..Critique::default()
    };
    for name in loose {
        out.left.push(format!(
            "{name}: no main or PR partner, so it was not compared"
        ));
    }
    let boxed_dir = dir.join(format!("{node}-shots/boxed"));
    for p in pairs {
        let read = decode(&dir.join(&p.before)).and_then(|b| Ok((b, decode(&dir.join(&p.after))?)));
        let (b, a) = match read {
            Ok(both) => both,
            Err(e) => {
                out.left.push(format!("{}: unreadable shot ({e})", p.view));
                continue;
            }
        };
        let diff = compare(&b, &a);
        let (mut boxed, mut region) = (None, None);
        if let PairDiff::Changed(r) = diff {
            let name = format!("{}.png", p.view.replace('/', "_"));
            let path = boxed_dir.join(&name);
            if std::fs::create_dir_all(&boxed_dir).is_ok() && write_boxed(&a, r, &path).is_ok() {
                boxed = Some(format!("{node}-shots/boxed/{name}"));
            }
            region = Some(r);
        }
        out.views.push(ViewEntry {
            theme: theme_of(&p.view),
            view: p.view,
            before: p.before,
            after: p.after,
            identical: diff == PairDiff::Identical,
            boxed,
            region,
        });
    }
    out
}

/// `fno-agents visual critique-prep <node> [--head <sha>]`: write the skeleton
/// critique for the node's shots and print its path.
pub fn run_critique_prep(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("critique-prep") || args.len() < 2 {
        eprintln!("usage: fno-agents visual critique-prep <node> [--head <sha>]");
        return 2;
    }
    let node = &args[1];
    let cwd = std::env::current_dir().unwrap_or_default();
    let flag = args
        .iter()
        .position(|a| a == "--head")
        .and_then(|i| args.get(i + 1));
    let head = match flag {
        Some(h) => h.clone(),
        None => match std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&cwd)
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => {
                eprintln!("cannot read HEAD; pass --head <sha>");
                return 1;
            }
        },
    };
    let root = crate::paths::canonical_repo_root(&cwd).unwrap_or(cwd);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = crate::escalation::vault_dir_with_home(&root, home.as_deref(), "mux");
    let shots = crate::merge_gates::mux_shots(&root, home.as_deref(), node);
    if shots.is_empty() {
        eprintln!(
            "no shots at {}/{node}-*.png: shoot main and the PR first",
            dir.display()
        );
        return 1;
    }
    let critique = prep(node, &head, &shots, &dir);
    let path = dir.join(format!("{node}-critique.json"));
    match serde_json::to_string_pretty(&critique)
        .map_err(|e| e.to_string())
        .and_then(|j| std::fs::write(&path, j).map_err(|e| e.to_string()))
    {
        Ok(()) => {
            println!("{}", path.display());
            0
        }
        Err(e) => {
            eprintln!("cannot write {}: {e}", path.display());
            1
        }
    }
}

/// What the gate finds when it reads a node's critique at a PR head.
#[derive(Debug)]
pub(crate) enum CritiqueRead {
    /// No file, or a file for another head.
    Missing,
    /// Prep wrote it but the critic has not filled it, or it is malformed.
    Unfilled(String),
    /// Findings of severity high stand; each is `view / region / rule`.
    High(Vec<String>),
    /// Filled, valid, no high finding.
    Ready(Critique),
}

/// The `##` and `###` heading texts of the design doc: the rule names.
pub(crate) fn rule_headings(doc: &str) -> Vec<String> {
    doc.lines()
        .filter_map(|l| l.strip_prefix("### ").or_else(|| l.strip_prefix("## ")))
        .map(|h| h.trim().to_string())
        .collect()
}

pub(crate) fn read_critique(
    dir: &Path,
    node: &str,
    head: &str,
    headings: &[String],
) -> CritiqueRead {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("{node}-critique.json"))) else {
        return CritiqueRead::Missing;
    };
    let c: Critique = match serde_json::from_str(&text) {
        Ok(c) => c,
        Err(e) => return CritiqueRead::Unfilled(format!("the critique is not valid JSON ({e})")),
    };
    if c.head != head {
        return CritiqueRead::Missing;
    }
    if c.critic.as_ref().is_none_or(|s| s.agent != "design-critic") {
        return CritiqueRead::Unfilled(
            "no design-critic stamp: the critic has not filled it".into(),
        );
    }
    if c.checked.is_empty() {
        return CritiqueRead::Unfilled("`checked` is empty".into());
    }
    for (i, f) in c.findings.iter().enumerate() {
        let bad = if !matches!(f.severity.as_str(), "high" | "medium" | "low") {
            Some("severity")
        } else if f.view.trim().is_empty() {
            Some("view")
        } else if f.region.trim().is_empty() {
            Some("region")
        } else if !headings.is_empty() && !headings.contains(&f.rule) {
            Some("rule")
        } else {
            None
        };
        if let Some(field) = bad {
            return CritiqueRead::Unfilled(format!("finding {} has a bad `{field}`", i + 1));
        }
    }
    let high: Vec<String> = c
        .findings
        .iter()
        .filter(|f| f.severity == "high")
        .map(|f| format!("{} / {} / {}", f.view, f.region, f.rule))
        .collect();
    if high.is_empty() {
        CritiqueRead::Ready(c)
    } else {
        CritiqueRead::High(high)
    }
}

/// The page text for a ready critique: what was checked, what was left.
pub(crate) fn critique_summary(c: &Critique) -> String {
    let mut s = String::from("Checked:\n");
    for line in &c.checked {
        s.push_str(&format!("- {line}\n"));
    }
    s.push_str("\nLeft:\n");
    let left: Vec<String> = c
        .findings
        .iter()
        .map(|f| format!("{} ({}): {} / {}", f.severity, f.view, f.region, f.rule))
        .chain(c.left.iter().cloned())
        .collect();
    if left.is_empty() {
        s.push_str("- nothing\n");
    }
    for line in left {
        s.push_str(&format!("- {line}\n"));
    }
    s.trim_end().to_string()
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while",
];

/// One changed line with its plain names masked, and those names in order.
/// String literals, numbers, keywords, paths (`a::B`), method and field names
/// (`.fg`), macros and capitalised names stay verbatim, so a string, style or
/// layout edit still differs after masking. Only a plain lowercase name is a
/// code name.
fn mask_names(line: &str) -> (String, Vec<String>) {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#""(?:\\.|[^"\\])*"|[A-Za-z_][A-Za-z0-9_]*|::|\."#).unwrap()
    });
    let (mut out, mut names) = (String::new(), Vec::new());
    let mut last = 0;
    let mut prev_link = false;
    for m in re.find_iter(line) {
        out.push_str(&line[last..m.start()]);
        last = m.end();
        let t = m.as_str();
        let rest = &line[m.end()..];
        let plain = t.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            && !KEYWORDS.contains(&t)
            && !prev_link
            && !rest.starts_with("::")
            && !rest.starts_with('!');
        prev_link = t == "::" || t == ".";
        if plain {
            names.push(t.to_string());
        }
        out.push_str(if plain { "_" } else { t });
    }
    out.push_str(&line[last..]);
    (out.split_whitespace().collect::<Vec<_>>().join(" "), names)
}

/// True when a unified-diff patch changes comments, blank lines and code
/// names only, so no rendered output can move. The removed and added code
/// lines must match in order once plain names are masked, so a reorder holds.
/// The names must also be a true rename: each old name maps to one new name
/// and back, the old name is gone from the added lines, and the new name
/// appears nowhere else in the patch. A swap to a binding that already
/// exists (`narrow` to `wide`) holds. A patch with no changed line is not
/// code-name-only.
pub(crate) fn names_only_patch(patch: &str) -> bool {
    use std::collections::{HashMap, HashSet};
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let mut seen: HashSet<String> = HashSet::new();
    let mut changed = 0;
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let (side, body) = if let Some(b) = line.strip_prefix('+') {
            (Some(&mut added), b)
        } else if let Some(b) = line.strip_prefix('-') {
            (Some(&mut removed), b)
        } else {
            (None, line.strip_prefix(' ').unwrap_or(line))
        };
        let t = body.trim();
        if t.is_empty() || t.starts_with("//") {
            changed += usize::from(side.is_some() && !t.is_empty());
            continue;
        }
        let (masked, names) = mask_names(t);
        match side {
            Some(v) => {
                changed += 1;
                v.push((masked, names));
            }
            None => seen.extend(names),
        }
    }
    if changed == 0 || removed.len() != added.len() {
        return false;
    }
    let (mut fwd, mut back): (HashMap<&str, &str>, HashMap<&str, &str>) = Default::default();
    for ((rm, rn), (am, an)) in removed.iter().zip(&added) {
        if rm != am {
            return false;
        }
        for (o, n) in rn.iter().zip(an) {
            if o == n {
                continue;
            }
            if *fwd.entry(o).or_insert(n) != n || *back.entry(n).or_insert(o) != o {
                return false;
            }
        }
    }
    let old_names: HashSet<&str> = removed
        .iter()
        .flat_map(|(_, n)| n)
        .map(String::as_str)
        .collect();
    let new_names: HashSet<&str> = added
        .iter()
        .flat_map(|(_, n)| n)
        .map(String::as_str)
        .collect();
    fwd.iter()
        .all(|(o, n)| !new_names.contains(o) && !old_names.contains(n) && !seen.contains(*n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) {
        let px: Vec<u8> = (0..h)
            .flat_map(|y| {
                let f = &f;
                (0..w).flat_map(move |x| f(x, y))
            })
            .collect();
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(file, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }

    #[test]
    fn real_shot_names_pair_whatever_token_the_main_side_wears() {
        let names = [
            "x-0000-settings-dark.png",
            "x-0000-settings-before-dark.png",
            "x-0000-connections-main-dark.png",
            "x-0000-connections-dark.png",
            "x-0000-stray.png",
        ]
        .map(String::from);
        let (pairs, loose) = pair_shots(&names);
        let views: Vec<&str> = pairs.iter().map(|p| p.view.as_str()).collect();
        assert_eq!(views, ["x-0000-connections-dark", "x-0000-settings-dark"]);
        assert_eq!(loose, ["x-0000-stray.png"]);
    }

    #[test]
    fn names_only_patches_read_true_and_rendered_edits_read_false() {
        assert!(names_only_patch("@@ @@\n-// box\n+// machine\n context\n"));
        let rename = "@@ @@\n-let tab_row = draw(\"Fleet\", Color::Red);\n+let pane_row = draw(\"Fleet\", Color::Red);\n";
        assert!(names_only_patch(rename));
        assert!(!names_only_patch(
            "@@ @@\n-let a = draw(\"box\");\n+let a = draw(\"machine\");\n"
        ));
        assert!(!names_only_patch(
            "@@ @@\n-x.fg(Color::Red);\n+x.fg(Color::Blue);\n"
        ));
        assert!(!names_only_patch("@@ @@\n-let w = 4;\n+let w = 5;\n"));
        // A reorder moves pixels; a swap to an existing binding is not a rename.
        assert!(!names_only_patch("@@ @@\n-a(1);\n-b(2);\n+b(2);\n+a(1);\n"));
        assert!(!names_only_patch(
            "@@ @@\n context(wide);\n-pane(narrow);\n+pane(wide);\n"
        ));
        assert!(!names_only_patch(""));
    }

    #[test]
    fn prep_boxes_a_changed_pair_and_the_gate_reads_the_critique_by_state() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write_png(&d.join("x-1-a-before-dark.png"), 8, 8, |_, _| {
            [0, 0, 0, 255]
        });
        write_png(&d.join("x-1-a-dark.png"), 8, 8, |x, y| {
            if x == 3 && y == 2 {
                [255; 4]
            } else {
                [0, 0, 0, 255]
            }
        });
        write_png(&d.join("x-1-b-before-light.png"), 4, 4, |_, _| [9; 4]);
        write_png(&d.join("x-1-b-light.png"), 4, 4, |_, _| [9; 4]);
        let shots = [
            "x-1-a-before-dark.png",
            "x-1-a-dark.png",
            "x-1-b-before-light.png",
            "x-1-b-light.png",
            "x-1-c-dark.png",
        ]
        .map(String::from);
        let mut c = prep("x-1", "h1", &shots, d);
        assert_eq!(c.views.len(), 2);
        assert!(!c.views[0].identical && c.views[0].boxed.is_some());
        assert!(c.views[1].identical && c.views[1].boxed.is_none());
        assert_eq!(c.left.len(), 1);
        assert!(d.join("x-1-shots/boxed/x-1-a-dark.png").exists());

        let write = |c: &Critique| {
            std::fs::write(
                d.join("x-1-critique.json"),
                serde_json::to_string(c).unwrap(),
            )
            .unwrap()
        };
        let rules = vec!["Spacing".to_string()];
        write(&c);
        assert!(matches!(
            read_critique(d, "x-1", "h1", &rules),
            CritiqueRead::Unfilled(_)
        ));
        assert!(matches!(
            read_critique(d, "x-1", "h2", &rules),
            CritiqueRead::Missing
        ));
        c.critic = Some(Stamp {
            agent: "design-critic".into(),
            ran_at: String::new(),
        });
        c.checked = vec!["spacing".into()];
        c.findings = vec![Finding {
            view: "a".into(),
            region: "top".into(),
            rule: "Spacing".into(),
            severity: "medium".into(),
            fix: String::new(),
        }];
        write(&c);
        assert!(matches!(
            read_critique(d, "x-1", "h1", &rules),
            CritiqueRead::Ready(_)
        ));
        c.findings[0].severity = "high".into();
        write(&c);
        assert!(matches!(
            read_critique(d, "x-1", "h1", &rules),
            CritiqueRead::High(_)
        ));
        c.findings[0].rule = "Taste".into();
        write(&c);
        assert!(matches!(
            read_critique(d, "x-1", "h1", &rules),
            CritiqueRead::Unfilled(_)
        ));
    }
}

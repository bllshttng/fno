//! Pre-screen for the visual-approval gate: diff the main and PR shots per
//! view and theme before a look question reaches the user.
//!
//! Shots pair by name: `<stem>-before-<theme>.png` (main) with
//! `<stem>-after-<theme>.png` (PR), or the token at the tail
//! (`<stem>-<theme>-before.png`). A pair with identical pixels needs no look.
//! A pair that differs becomes a boxed copy of the PR shot plus one plain
//! line that says where it changed.

use std::path::{Path, PathBuf};

/// One view at one theme, with the main shot and the PR shot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShotPair {
    pub view: String,
    pub before: String,
    pub after: String,
}

/// The changed rectangle of one pair, in PR-shot pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairDiff {
    Identical,
    Changed {
        pixels: u64,
        region: Region,
    },
    SizeChanged {
        before: (u32, u32),
        after: (u32, u32),
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Screen {
    /// Every pair is pixel-identical: no look can judge anything.
    Identical { views: usize },
    /// At least one pair differs, or a shot has no partner.
    NeedsLook(Vec<ViewChange>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewChange {
    pub view: String,
    pub line: String,
    /// The boxed PR shot, when a box could be drawn.
    pub boxed: Option<PathBuf>,
}

fn split_token(stem: &str) -> Option<(String, &'static str)> {
    let parts: Vec<&str> = stem.split('-').collect();
    let at = parts.iter().position(|p| *p == "before" || *p == "after")?;
    let side = if parts[at] == "before" {
        "before"
    } else {
        "after"
    };
    let mut rest = parts.clone();
    rest.remove(at);
    Some((rest.join("-"), side))
}

/// Pair the shot names (file names or `folder/name`). Names with no partner
/// come back second: the screen cannot clear a view it has half of.
pub(crate) fn pair_shots(shots: &[String]) -> (Vec<ShotPair>, Vec<String>) {
    let mut before: Vec<(String, String)> = Vec::new();
    let mut after: Vec<(String, String)> = Vec::new();
    let mut loose = Vec::new();
    for name in shots {
        let stem = name.trim_end_matches(".png");
        match split_token(stem) {
            Some((key, "before")) => before.push((key, name.clone())),
            Some((key, _)) => after.push((key, name.clone())),
            None => loose.push(name.clone()),
        }
    }
    let mut pairs = Vec::new();
    for (key, b) in &before {
        match after.iter().position(|(k, _)| k == key) {
            Some(i) => {
                let (_, a) = after.remove(i);
                pairs.push(ShotPair {
                    view: key.clone(),
                    before: b.clone(),
                    after: a,
                });
            }
            None => loose.push(b.clone()),
        }
    }
    loose.extend(after.into_iter().map(|(_, n)| n));
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
        return PairDiff::SizeChanged {
            before: (a.w, a.h),
            after: (b.w, b.h),
        };
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
    PairDiff::Changed {
        pixels,
        region: Region {
            x: x0,
            y: y0,
            w: x1 - x0 + 1,
            h: y1 - y0 + 1,
        },
    }
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

fn describe(view: &str, diff: &PairDiff) -> String {
    match diff {
        PairDiff::Identical => format!("{view}: no change"),
        PairDiff::Changed { pixels, region } => format!(
            "{view}: {pixels} pixels changed, inside a {}x{} box at ({}, {})",
            region.w, region.h, region.x, region.y
        ),
        PairDiff::SizeChanged { before, after } => format!(
            "{view}: the shot size moved from {}x{} to {}x{}",
            before.0, before.1, after.0, after.1
        ),
    }
}

/// Screen the shots in `dir`. Boxed copies land in `scratch`. A shot that
/// cannot be read, or has no partner, keeps the look: the screen only ever
/// clears on a positive pixel match.
pub(crate) fn screen(shots: &[String], dir: &Path, scratch: &Path) -> Screen {
    let (pairs, loose) = pair_shots(shots);
    let mut changes = Vec::new();
    for name in &loose {
        changes.push(ViewChange {
            view: name.clone(),
            line: format!("{name}: no main/PR partner, so the screen cannot compare it"),
            boxed: None,
        });
    }
    for pair in &pairs {
        let read = decode(&dir.join(&pair.before)).and_then(|b| {
            let a = decode(&dir.join(&pair.after))?;
            Ok((b, a))
        });
        let (b, a) = match read {
            Ok(both) => both,
            Err(e) => {
                changes.push(ViewChange {
                    view: pair.view.clone(),
                    line: format!("{}: unreadable shot ({e})", pair.view),
                    boxed: None,
                });
                continue;
            }
        };
        let diff = compare(&b, &a);
        if diff == PairDiff::Identical {
            continue;
        }
        let boxed = match &diff {
            PairDiff::Changed { region, .. } => {
                let out = scratch.join(format!("boxed-{}.png", pair.view.replace('/', "_")));
                write_boxed(&a, *region, &out).ok().map(|()| out)
            }
            _ => None,
        };
        changes.push(ViewChange {
            view: pair.view.clone(),
            line: describe(&pair.view, &diff),
            boxed,
        });
    }
    if changes.is_empty() && !pairs.is_empty() {
        Screen::Identical { views: pairs.len() }
    } else {
        Screen::NeedsLook(changes)
    }
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while",
];

/// One changed line with its identifiers masked. String literals, numbers,
/// keywords, paths (`a::B`), method and field names (`.fg`), macros and
/// capitalised names stay verbatim, so a string, style or layout edit still
/// differs after masking. Only a plain lowercase name is a code name.
fn mask_names(line: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#""(?:\\.|[^"\\])*"|[A-Za-z_][A-Za-z0-9_]*|::|\."#).unwrap()
    });
    let mut out = String::new();
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
        out.push_str(if plain { "_" } else { t });
    }
    out.push_str(&line[last..]);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when a unified-diff patch changes comments, blank lines and code
/// names only, so no rendered output can move. Removed and added code lines
/// must match once plain lowercase names are masked. A patch with no changed
/// line is not code-name-only.
pub(crate) fn names_only_patch(patch: &str) -> bool {
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let mut changed = 0;
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let (side, body) = if let Some(b) = line.strip_prefix('+') {
            (&mut added, b)
        } else if let Some(b) = line.strip_prefix('-') {
            (&mut removed, b)
        } else {
            continue;
        };
        let t = body.trim();
        if t.is_empty() {
            continue;
        }
        changed += 1;
        if !t.starts_with("//") {
            side.push(mask_names(t));
        }
    }
    removed.sort();
    added.sort();
    changed > 0 && removed == added
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
    fn pairs_take_the_token_in_the_middle_or_at_the_tail() {
        let names = [
            "x-1-before-dark.png",
            "x-1-after-dark.png",
            "x-1-base-light-before.png",
            "x-1-base-light-after.png",
            "x-1-stray.png",
        ]
        .map(String::from);
        let (pairs, loose) = pair_shots(&names);
        let views: Vec<&str> = pairs.iter().map(|p| p.view.as_str()).collect();
        assert_eq!(views, ["x-1-base-light", "x-1-dark"]);
        assert_eq!(loose, ["x-1-stray.png"]);
    }

    #[test]
    fn screen_clears_identical_pairs_and_boxes_a_changed_one() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write_png(&d.join("v-before-dark.png"), 8, 8, |_, _| [0, 0, 0, 255]);
        write_png(&d.join("v-after-dark.png"), 8, 8, |_, _| [0, 0, 0, 255]);
        let same = ["v-before-dark.png", "v-after-dark.png"].map(String::from);
        assert_eq!(screen(&same, d, d), Screen::Identical { views: 1 });

        write_png(&d.join("v-after-dark.png"), 8, 8, |x, y| {
            if (3..5).contains(&x) && y == 2 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        let Screen::NeedsLook(changes) = screen(&same, d, d) else {
            panic!("a changed pair must keep the look");
        };
        assert_eq!(changes.len(), 1);
        assert!(changes[0]
            .line
            .contains("2 pixels changed, inside a 2x1 box at (3, 2)"));
        assert!(changes[0].boxed.as_ref().is_some_and(|p| p.exists()));
    }

    #[test]
    fn a_shot_with_no_partner_or_no_file_never_clears() {
        let dir = tempfile::tempdir().unwrap();
        let lone = ["v-before-dark.png".to_string()];
        assert!(matches!(
            screen(&lone, dir.path(), dir.path()),
            Screen::NeedsLook(_)
        ));
        let missing = ["v-before-dark.png", "v-after-dark.png"].map(String::from);
        assert!(matches!(
            screen(&missing, dir.path(), dir.path()),
            Screen::NeedsLook(_)
        ));
    }

    #[test]
    fn names_only_patches_read_true_and_rendered_edits_read_false() {
        let comments = "@@ -1,2 +1,2 @@\n-// box\n+// machine\n context\n";
        assert!(names_only_patch(comments));
        let rename = "@@ @@\n-let tab_row = draw(\"Fleet\", Color::Red);\n+let pane_row = draw(\"Fleet\", Color::Red);\n";
        assert!(names_only_patch(rename));
        let text = "@@ @@\n-let a = draw(\"box\");\n+let a = draw(\"machine\");\n";
        assert!(!names_only_patch(text));
        let style = "@@ @@\n-x.fg(Color::Red);\n+x.fg(Color::Blue);\n";
        assert!(!names_only_patch(style));
        let layout = "@@ @@\n-let w = 4;\n+let w = 5;\n";
        assert!(!names_only_patch(layout));
        assert!(!names_only_patch(""));
    }
}

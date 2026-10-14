//! The launch-page renderer: one reference per harness under
//! `skills/agent/references/`, joined from a hand-written prose source and
//! the harness's own `--help`, captured verbatim. The flag tables are read
//! from the captured text, never typed, so a harness flag change reaches the
//! page through a recapture alone.

use std::path::Path;

const PROSE_DIR: &str = "docs/harnesses/launch";
const HELP_DIR: &str = "docs/harnesses/help";
const PAGE_DIR: &str = "skills/agent/references";

/// An entry at this indent or less opens a new row; anything deeper
/// continues the row above it. Commander, clap and opencode all indent their
/// entries 2 to 6 columns and their wrapped descriptions further.
const ENTRY_INDENT_MAX: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Flags,
    Commands,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub kind: SectionKind,
    pub title: String,
    pub entries: Vec<(String, String)>,
}

/// One captured `--help`: the snapshot's own header names the command and
/// the version it was captured from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub command: String,
    pub version: String,
    pub sections: Vec<HelpSection>,
}

/// A section header is an unindented line that ends in a colon (commander,
/// clap) or is all capitals (opencode). Every other unindented line, such as
/// `Usage: claude [options]`, closes the section and opens none.
fn section_of(line: &str) -> Option<(SectionKind, String)> {
    let all_caps = line.chars().all(|c| c.is_ascii_uppercase() || c == ' ');
    if !line.ends_with(':') && !all_caps {
        return None;
    }
    let title = line.trim_end_matches(':').to_string();
    let lower = title.to_ascii_lowercase();
    if lower.contains("option") || lower.contains("flag") {
        Some((SectionKind::Flags, title))
    } else if lower.contains("command") {
        Some((SectionKind::Commands, title))
    } else {
        None
    }
}

fn is_entry(kind: SectionKind, body: &str) -> bool {
    let mut chars = body.chars();
    match kind {
        SectionKind::Flags => {
            chars.next() == Some('-')
                && matches!(chars.next(), Some(c) if c == '-' || c.is_ascii_alphanumeric())
        }
        SectionKind::Commands => matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric()),
    }
}

/// Parse the flag and command sections of one `--help` text.
pub fn parse_help(text: &str) -> Vec<HelpSection> {
    let mut sections = Vec::new();
    let mut cur: Option<HelpSection> = None;
    let mut entry_indent: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        let body = line.trim_start();
        if body.is_empty() {
            continue;
        }
        let indent = line.len() - body.len();
        if indent == 0 {
            sections.extend(cur.take().filter(|s| !s.entries.is_empty()));
            entry_indent = None;
            cur = section_of(body).map(|(kind, title)| HelpSection {
                kind,
                title,
                entries: Vec::new(),
            });
            continue;
        }
        let Some(sec) = cur.as_mut() else { continue };
        if indent <= ENTRY_INDENT_MAX && is_entry(sec.kind, body) {
            let (spec, desc) = match body.find("  ") {
                Some(i) => (body[..i].to_string(), body[i..].trim().to_string()),
                None => (body.to_string(), String::new()),
            };
            sec.entries.push((spec, desc));
            entry_indent = Some(indent);
        } else if entry_indent.is_some_and(|ei| indent > ei) {
            if let Some((_, desc)) = sec.entries.last_mut() {
                if !desc.is_empty() {
                    desc.push(' ');
                }
                desc.push_str(body);
            }
        } else {
            entry_indent = None;
        }
    }
    sections.extend(cur.filter(|s| !s.entries.is_empty()));
    sections
}

/// Read one snapshot file: `$ <command> --help`, `version: <v>`, a blank
/// line, then the help verbatim.
pub fn parse_snapshot(name: &str, text: &str) -> Result<Snapshot, String> {
    let mut lines = text.splitn(3, '\n');
    let command = lines
        .next()
        .and_then(|l| l.strip_prefix("$ "))
        .and_then(|l| l.trim_end().strip_suffix(" --help"))
        .ok_or_else(|| format!("{HELP_DIR}/{name}: line 1 must read `$ <command> --help`"))?
        .to_string();
    let version = lines
        .next()
        .and_then(|l| l.strip_prefix("version: "))
        .ok_or_else(|| format!("{HELP_DIR}/{name}: line 2 must read `version: <v>`"))?
        .trim_end()
        .to_string();
    Ok(Snapshot {
        command,
        version,
        sections: parse_help(lines.next().unwrap_or("")),
    })
}

/// One table cell: one line, no dashes the house style bars, no bare pipe.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['\u{2014}', '\u{2013}'], "-")
        .replace('|', "\\|")
}

fn render_page(harness: &str, prose: &str, snaps: &[Snapshot]) -> String {
    let mut out = format!(
        "<!-- GENERATED by `fno doctor harness-matrix --write`. DO NOT EDIT. \
         Edit {PROSE_DIR}/launch-{harness}.md, or recapture {HELP_DIR}/. -->\n"
    );
    out.push_str(prose.trim_end());
    out.push_str("\n\n## Flags from --help\n\n");
    if snaps.is_empty() {
        out.push_str(&format!(
            "No `{harness} --help` is captured yet. The harness-launch-docs workflow captures it once the binary installs.\n"
        ));
        return out;
    }
    out.push_str(
        "These tables are read from the harness's own `--help`. The harness-launch-docs workflow recaptures it every week.\n",
    );
    for snap in snaps {
        out.push_str(&format!(
            "\n### `{} --help`\n\nCaptured from `{}`.\n",
            snap.command, snap.version
        ));
        for sec in &snap.sections {
            let head = match sec.kind {
                SectionKind::Flags => "Flag",
                SectionKind::Commands => "Command",
            };
            out.push_str(&format!(
                "\n#### {}\n\n| {head} | What it does |\n|---|---|\n",
                sec.title
            ));
            for (spec, desc) in &sec.entries {
                out.push_str(&format!("| `{}` | {} |\n", cell(spec), cell(desc)));
            }
        }
    }
    out
}

/// A missing directory reads as empty: a checkout or fixture root with no
/// launch sources renders no pages rather than failing the matrix door.
fn sorted_names(dir: &Path, ext: &str) -> Result<Vec<String>, String> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut names: Vec<String> = read
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(ext))
        .collect();
    names.sort();
    Ok(names)
}

/// Every launch page as `(repo-relative path, text)`: one per prose source,
/// joined with the snapshots whose command starts with that harness.
pub fn render_pages(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut snaps = Vec::new();
    for name in sorted_names(&root.join(HELP_DIR), ".txt")? {
        let text = std::fs::read_to_string(root.join(HELP_DIR).join(&name))
            .map_err(|e| format!("cannot read {HELP_DIR}/{name}: {e}"))?;
        snaps.push(parse_snapshot(&name, &text)?);
    }
    let mut pages = Vec::new();
    for name in sorted_names(&root.join(PROSE_DIR), ".md")? {
        // Sources share the page's `launch-` basename: a bare `claude.md`
        // reads as a CLAUDE.md memory file on a case-insensitive disk.
        let Some(harness) = name
            .strip_prefix("launch-")
            .and_then(|n| n.strip_suffix(".md"))
        else {
            continue;
        };
        let prose = std::fs::read_to_string(root.join(PROSE_DIR).join(&name))
            .map_err(|e| format!("cannot read {PROSE_DIR}/{name}: {e}"))?;
        let own: Vec<Snapshot> = snaps
            .iter()
            .filter(|s| s.command.split_whitespace().next() == Some(harness))
            .cloned()
            .collect();
        pages.push((
            format!("{PAGE_DIR}/{name}"),
            render_page(harness, &prose, &own),
        ));
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three wire shapes: commander (claude) wraps descriptions far right and
    /// drops a long flag's description to the next line; clap (codex) puts
    /// every description on following lines with blank lines between; v2
    /// opencode uses capital headers and one line per entry. A `Usage:` line
    /// mentioning `[options]` must not open a flag section.
    #[test]
    fn parses_commander_clap_and_opencode_help() {
        let commander = "Usage: claude [options] [command]\n\nOptions:\n  --add-dir <dirs...>   Extra dirs\n                        to allow\n  --allowedTools <tools...>\n      Tools to allow\n  -h, --help            Display help\n\nCommands:\n  attach <id>           Open a session\n";
        let s = parse_help(commander);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].kind, SectionKind::Flags);
        assert_eq!(
            s[0].entries,
            vec![
                ("--add-dir <dirs...>".into(), "Extra dirs to allow".into()),
                ("--allowedTools <tools...>".into(), "Tools to allow".into()),
                ("-h, --help".into(), "Display help".into()),
            ]
        );
        assert_eq!(
            s[1].entries,
            vec![("attach <id>".into(), "Open a session".into())]
        );

        let clap = "Usage: codex [OPTIONS]\n\nOptions:\n  -c, --config <key=value>\n          Override a value.\n\n          Examples: one\n\n      --oss\n          Use open-source provider\n\n  -a, --ask-for-approval <POLICY>\n          Possible values:\n          - never: Never ask\n";
        let s = parse_help(clap);
        assert_eq!(
            s[0].entries,
            vec![
                (
                    "-c, --config <key=value>".into(),
                    "Override a value. Examples: one".into()
                ),
                ("--oss".into(), "Use open-source provider".into()),
                (
                    "-a, --ask-for-approval <POLICY>".into(),
                    "Possible values: - never: Never ask".into()
                ),
            ]
        );

        let opencode = "USAGE\n  opencode run [flags]\n\nFLAGS\n  --title string    Session title\n\nGLOBAL FLAGS\n  --help, -h        Show help\n\nSUBCOMMANDS\n  run               Run OpenCode\n";
        let s = parse_help(opencode);
        let titles: Vec<&str> = s.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, ["FLAGS", "GLOBAL FLAGS", "SUBCOMMANDS"]);
        assert_eq!(
            s[0].entries,
            vec![("--title string".into(), "Session title".into())]
        );
    }

    /// A snapshot that lost its header names the file and the line it wants;
    /// a good one renders its version and an escaped, dash-mapped cell.
    #[test]
    fn snapshot_header_is_required_and_cells_are_escaped() {
        let err = parse_snapshot("claude.txt", "Usage: claude\n").unwrap_err();
        assert!(
            err.contains("claude.txt") && err.contains("line 1"),
            "{err}"
        );
        let snap = parse_snapshot(
            "claude.txt",
            "$ claude --help\nversion: 2.1.296 (Claude Code)\n\nOptions:\n  --cloud [a|b]   Off \u{2014} on\n",
        )
        .unwrap();
        let page = render_page("claude", "# Launch claude\n", &[snap]);
        assert!(
            page.contains("Captured from `2.1.296 (Claude Code)`."),
            "{page}"
        );
        assert!(page.contains("| `--cloud [a\\|b]` | Off - on |"), "{page}");
    }
}

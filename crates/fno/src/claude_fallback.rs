//! `fno agents audit claude-fallback --worktrees-base <dir> --since
//! <YYYY-MM-DD> [--days 7]`: count the distinct claude sessions whose cwd
//! sat under the worktrees base within the window.
//!
//! The daily gate's no-fallback part needs this count, and the gate cannot
//! count it itself: the count lives in the claude home, which the footnote
//! harness never reads. fno owns the producer and fnh owns the verdict, so
//! the number prints alone on stdout and the gate invocation reads it with
//! `--claude-sessions "$(fno agents audit claude-fallback ...)"`. A session
//! counts when its transcript mtime falls in the window: mtime is last
//! activity, so a session is in every window it was still working in.
//!
//! The walk is the projects-store walk the discover lane uses, inverted.
//! claude encodes a session's cwd into its transcript dir name, so the
//! sessions on a worktree are the dirs whose encoded name carries the
//! encoded base as a path prefix. The encoding is lossy and has shipped in
//! two variants (the underscore keeper and the full encode), so both slugs
//! of the base match; the boundary check refuses a sibling whose name only
//! extends the prefix's last word (`wt-2` is not under `wt`).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const USAGE: &str =
    "usage: fno agents audit claude-fallback --worktrees-base <dir> --since <YYYY-MM-DD> [--days 7]";

struct Args {
    worktrees_base: PathBuf,
    since: SystemTime,
    days: u64,
}

/// The one answer: the count on stdout, 0 exit; a usage error is stderr and
/// exit 2, the way the gate's own reader answers.
pub fn run(args: &[OsString]) -> i32 {
    match parse_args(args) {
        Ok(Some(args)) => println!("{}", count_in(&projects_dir(), &args)),
        Ok(None) => {}
        Err(msg) => {
            eprintln!("{msg}");
            eprintln!("{USAGE}");
            return 2;
        }
    }
    0
}

fn parse_args(args: &[OsString]) -> Result<Option<Args>, String> {
    let mut base: Option<PathBuf> = None;
    let mut since: Option<SystemTime> = None;
    let mut days: u64 = 7;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(None);
            }
            Some("--worktrees-base") => {
                i += 1;
                let v = args.get(i).ok_or("--worktrees-base needs a dir")?;
                base = Some(PathBuf::from(v));
            }
            Some("--since") => {
                i += 1;
                let v = args.get(i).ok_or("--since needs a date")?;
                let s = v.to_str().ok_or("--since must be utf-8")?;
                since = Some(parse_since(s)?);
            }
            Some("--days") => {
                i += 1;
                let v = args.get(i).ok_or("--days needs a count")?;
                let s = v.to_str().ok_or("--days must be utf-8")?;
                days = s
                    .parse()
                    .map_err(|_| format!("--days {s:?} is not a count"))?;
                if days > 9_999 {
                    return Err(format!("--days {days} is over the 9999 cap"));
                }
            }
            Some(other) => return Err(format!("unknown argument {other:?}")),
            None => return Err("arguments must be utf-8".to_string()),
        }
        i += 1;
    }
    Ok(Some(Args {
        worktrees_base: base.ok_or("--worktrees-base is required")?,
        since: since.ok_or("--since is required")?,
        days,
    }))
}

fn parse_since(s: &str) -> Result<SystemTime, String> {
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| format!("--since {s:?} is not a YYYY-MM-DD date"))?;
    let secs = d
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp()
        .max(0) as u64;
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn count_in(projects: &Path, args: &Args) -> usize {
    let end = args.since + Duration::from_secs(args.days * 86_400);
    let base_slugs = base_slugs(&args.worktrees_base);
    let Ok(dirs) = std::fs::read_dir(projects) else {
        return 0;
    };
    let mut sessions = BTreeSet::new();
    for dir in dirs.flatten() {
        let fname = dir.file_name();
        let Some(name) = fname.to_str() else {
            continue;
        };
        if !under_base(name, &base_slugs) {
            continue;
        }
        for stem in transcript_stems(&dir.path(), args.since, end) {
            sessions.insert(stem);
        }
    }
    sessions.len()
}

/// The slugs the base answers under: the path as given, trailing
/// separators trimmed, plus the resolved path when it differs, so a
/// relative or symlinked spelling still carries the leading `-` the
/// encoded absolute cwds start with.
fn base_slugs(base: &Path) -> Vec<String> {
    let given = base.to_string_lossy();
    let trimmed = given.trim_end_matches('/');
    let spelled = if trimmed.is_empty() {
        given.as_ref()
    } else {
        trimmed
    };
    let mut slugs = candidate_slugs(Path::new(spelled));
    if let Ok(resolved) = base.canonicalize() {
        for slug in candidate_slugs(&resolved) {
            if !slugs.contains(&slug) {
                slugs.push(slug);
            }
        }
    }
    slugs
}

/// True when the encoded dir name is strictly deeper than the encoded base
/// at a path boundary: the base itself is not a worktree under the base.
fn under_base(dir_name: &str, base_slugs: &[String]) -> bool {
    base_slugs.iter().any(|slug| {
        dir_name.len() > slug.len()
            && dir_name.starts_with(slug.as_str())
            && dir_name.as_bytes()[slug.len()] == b'-'
    })
}

/// The two slugs the cwd encoding has shipped for `path`: the full encode
/// (`_` becomes `-` too) and the underscore keeper. Both are lossy, so the
/// caller prefix-matches instead of decoding.
fn candidate_slugs(path: &Path) -> Vec<String> {
    let raw = path.to_string_lossy();
    let mut slugs: Vec<String> = Vec::new();
    for keep_underscore in [false, true] {
        let slug: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || (keep_underscore && c == '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if !slugs.contains(&slug) {
            slugs.push(slug);
        }
    }
    slugs
}

/// The transcript stems in one project dir whose mtime is in the window.
/// Only top-level `*.jsonl` files are transcripts; a dotted stem is a
/// sibling artifact (`<uuid>.sync-conflict-<ts>`, `<uuid>.orphaned-...`),
/// never a session.
fn transcript_stems(dir: &Path, start: SystemTime, end: SystemTime) -> Vec<String> {
    let Ok(files) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stems = Vec::new();
    for file in files.flatten() {
        let Ok(meta) = file.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let name = file.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".jsonl") else {
            continue;
        };
        if stem.contains('.') {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if mtime >= start && mtime < end {
            stems.push(stem.to_string());
        }
    }
    stems
}

/// The claude transcript store, the same resolution the drive lane uses:
/// the test seam env, then an account config dir, then the home store.
fn projects_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_CLAUDE_PROJECTS_DIR") {
        return PathBuf::from(v);
    }
    if let Some(v) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(v).join("projects");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude").join("projects")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINCE: &str = "2026-10-14";

    fn args_for(base: &Path, days: u64) -> Args {
        Args {
            worktrees_base: base.to_path_buf(),
            since: parse_since(SINCE).unwrap(),
            days,
        }
    }

    fn write_at(path: &Path, secs_since_epoch: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs_since_epoch))
            .unwrap();
    }

    fn slug(path: &str) -> String {
        candidate_slugs(Path::new(path)).remove(0)
    }

    fn start_secs() -> u64 {
        parse_since(SINCE)
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// A write time `delta` seconds into the window.
    fn at(delta: u64) -> u64 {
        start_secs() + delta
    }

    #[test]
    fn a_session_on_a_worktree_counts_once_across_slug_variants() {
        let tmp = tempfile::TempDir::new().unwrap();
        // An underscore in the base name is where the two slug variants of
        // one cwd disagree, so the fixture base carries one.
        let base = Path::new("/wt/f_leet");
        let projects = tmp.path();
        let sid = "aaaaaaaa-1111-2222-3333-444444444444";
        for variant in candidate_slugs(base) {
            write_at(
                &projects
                    .join(format!("{variant}-x-aaaa"))
                    .join(format!("{sid}.jsonl")),
                at(0),
            );
        }
        assert_eq!(count_in(projects, &args_for(base, 7)), 1);
    }

    #[test]
    fn window_edges_read_as_activity_start_in_end_out() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = Path::new("/wt/fleet");
        let projects = tmp.path();
        let dir = projects.join(format!("{}-x-bbbb", slug("/wt/fleet")));
        write_at(
            &dir.join("bbbbbbbb-1111-2222-3333-444444444444.jsonl"),
            at(0),
        );
        write_at(
            &dir.join("cccccccc-1111-2222-3333-444444444444.jsonl"),
            at(7 * 86_400 - 1),
        );
        assert_eq!(count_in(projects, &args_for(base, 7)), 2);
        // At the window end and a day past it: out.
        write_at(
            &dir.join("dddddddd-1111-2222-3333-444444444444.jsonl"),
            at(7 * 86_400),
        );
        write_at(
            &dir.join("eeeeeeee-1111-2222-3333-444444444444.jsonl"),
            at(8 * 86_400),
        );
        assert_eq!(count_in(projects, &args_for(base, 7)), 2);
        assert_eq!(count_in(projects, &args_for(base, 0)), 0);
    }

    #[test]
    fn the_base_itself_prefix_siblings_and_off_base_dirs_do_not_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = Path::new("/wt/fleet");
        let projects = tmp.path();
        let mk = |dir: &str, sid: &str| {
            write_at(&projects.join(dir).join(format!("{sid}.jsonl")), at(30));
        };
        let in_window = "11111111-1111-2222-3333-444444444444";
        // The base dir itself: not a worktree under the base.
        mk(&slug("/wt/fleet"), in_window);
        // A sibling whose name only extends the prefix's last word: the
        // encoded root of a neighboring base, `/wt/fleet2`.
        mk(&format!("{}2-x-aaaa", slug("/wt/fleet")), in_window);
        // A session off the base entirely.
        mk(&slug("/home/someone/elsewhere"), in_window);
        assert_eq!(count_in(projects, &args_for(base, 7)), 0);
        // A real worktree one level under the base counts.
        mk(&format!("{}-x-cccc", slug("/wt/fleet")), in_window);
        // And so does a distinct session whose cwd was deeper inside that
        // worktree: dedupe is by session id, so the same id twice stays one.
        mk(
            &format!("{}-x-cccc-sub", slug("/wt/fleet")),
            "22222222-1111-2222-3333-444444444444",
        );
        assert_eq!(count_in(projects, &args_for(base, 7)), 2);
        // A trailing separator on the base still counts the same sessions.
        assert_eq!(count_in(projects, &args_for(Path::new("/wt/fleet/"), 7)), 2);
    }

    #[test]
    fn dotted_stems_and_missing_stores_read_as_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = Path::new("/wt/fleet");
        let projects = tmp.path();
        let dir = projects.join(format!("{}-x-dddd", slug("/wt/fleet")));
        write_at(
            &dir.join("aaaaaaaa-1111-2222-3333-444444444444.sync-conflict-1.jsonl"),
            at(30),
        );
        // tool-results live in a uuid subdir, never at the top level.
        write_at(
            &dir.join("bbbbbbbb-1111-2222-3333-444444444444")
                .join("tool.jsonl"),
            at(30),
        );
        assert_eq!(count_in(projects, &args_for(base, 7)), 0);
        assert_eq!(count_in(&projects.join("nope"), &args_for(base, 7)), 0);
    }

    #[test]
    fn both_slug_variants_match_the_same_base() {
        let underscore = candidate_slugs(Path::new("/wt/f_leet"));
        assert_eq!(
            underscore,
            vec!["-wt-f-leet".to_string(), "-wt-f_leet".to_string()]
        );
        assert!(under_base("-wt-f_leet-x-aaaa", &underscore));
        assert!(under_base("-wt-f-leet-x-aaaa", &underscore));
        assert_eq!(
            candidate_slugs(Path::new("/wt/leet")),
            vec!["-wt-leet".to_string()]
        );
    }

    #[test]
    fn parse_refuses_bad_dates_counts_and_missing_flags() {
        let a: Vec<OsString> = ["--worktrees-base", "/wt", "--since", "14.10.2026"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(parse_args(&a).is_err());
        let a: Vec<OsString> = ["--worktrees-base", "/wt", "--since", SINCE, "--days", "x"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(parse_args(&a).is_err());
        let a: Vec<OsString> = ["--since", SINCE].iter().map(OsString::from).collect();
        assert!(parse_args(&a).is_err());
        let a: Vec<OsString> = ["--worktrees-base", "/wt"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(parse_args(&a).is_err());
        let a: Vec<OsString> = [
            "--worktrees-base",
            "/wt",
            "--since",
            SINCE,
            "--days",
            "10000",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert!(parse_args(&a).is_err());
        let a: Vec<OsString> = ["--worktrees-base", "/wt", "--since", SINCE]
            .iter()
            .map(OsString::from)
            .collect();
        let parsed = parse_args(&a).unwrap().unwrap();
        assert_eq!(parsed.worktrees_base, PathBuf::from("/wt"));
        assert_eq!(parsed.days, 7);
    }
}

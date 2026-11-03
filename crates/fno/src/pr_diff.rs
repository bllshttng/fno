//! A pull request's diff as rows: a pure parser over unified diff text, the
//! pull URL parse, and the two REST reads. REST because `gh pr diff` spends a
//! GraphQL request and this read spends none.

/// The largest diff text parsed; the rest is dropped and marked truncated.
const MAX_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 20_000;

/// A pull request's address, parsed from its URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Hunk,
    Context,
    Added,
    Removed,
}

/// One diff row. A removed row has only its old-side number, an added row only
/// its new-side number, a context row both, and a hunk header neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: RowKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub old_path: String,
    pub new_path: String,
    pub status: FileStatus,
    pub rows: Vec<DiffRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrDiff {
    pub files: Vec<DiffFile>,
    pub truncated: bool,
}

impl PrDiff {
    /// The added and removed row counts across every file.
    pub fn totals(&self) -> (usize, usize) {
        let rows = || self.files.iter().flat_map(|f| f.rows.iter());
        (
            rows().filter(|r| r.kind == RowKind::Added).count(),
            rows().filter(|r| r.kind == RowKind::Removed).count(),
        )
    }
}

/// The start numbers of a `@@ -a,b +c,d @@` header.
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let mut words = line.strip_prefix("@@ ")?.split(' ');
    let side = |word: Option<&str>, sign: char| -> Option<u32> {
        word?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    Some((side(words.next(), '-')?, side(words.next(), '+')?))
}

/// The two paths of a `diff --git a/x b/y` header. A path with a space makes
/// this split ambiguous, so the `---`, `+++` and `rename` lines overwrite it.
fn git_header_paths(rest: &str) -> (String, String) {
    match rest.split_once(" b/") {
        Some((old, new)) => (old.trim_start_matches("a/").to_string(), new.to_string()),
        None => (rest.to_string(), rest.to_string()),
    }
}

/// Parse unified diff text. `\ No newline at end of file` adds no row.
pub fn parse(text: &str) -> PrDiff {
    let mut out = PrDiff::default();
    let bounded = if text.len() > MAX_BYTES {
        out.truncated = true;
        let mut end = MAX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        // Cut at a line end, so the last row is never half a line.
        let end = text[..end].rfind('\n').map_or(end, |nl| nl + 1);
        &text[..end]
    } else {
        text
    };
    let (mut old_no, mut new_no, mut row_count) = (0u32, 0u32, 0usize);
    let mut in_hunk = false;
    for line in bounded.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old_path, new_path) = git_header_paths(rest);
            out.files.push(DiffFile {
                old_path,
                new_path,
                status: FileStatus::Modified,
                rows: Vec::new(),
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = out.files.last_mut() else {
            continue;
        };
        if line.starts_with("@@ ") {
            let Some((old, new)) = hunk_starts(line) else {
                continue;
            };
            (old_no, new_no, in_hunk) = (old, new, true);
            if row_count >= MAX_ROWS {
                out.truncated = true;
                break;
            }
            row_count += 1;
            file.rows.push(DiffRow {
                kind: RowKind::Hunk,
                old: None,
                new: None,
                text: line.to_string(),
            });
            continue;
        }
        if !in_hunk {
            if line.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if line.starts_with("Binary files ") {
                file.status = FileStatus::Binary;
            } else if let Some(path) = line.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = path.to_string();
            } else if let Some(path) = line.strip_prefix("rename to ") {
                file.new_path = path.to_string();
            } else if let Some(path) = line.strip_prefix("--- a/") {
                file.old_path = path.to_string();
            } else if let Some(path) = line.strip_prefix("+++ b/") {
                file.new_path = path.to_string();
            }
            continue;
        }
        let (kind, old, new, body) = match line.as_bytes().first() {
            Some(b'+') => (RowKind::Added, None, Some(new_no), &line[1..]),
            Some(b'-') => (RowKind::Removed, Some(old_no), None, &line[1..]),
            Some(b' ') => (RowKind::Context, Some(old_no), Some(new_no), &line[1..]),
            Some(b'\\') => continue,
            // A trimmed blank context line arrives empty.
            None => (RowKind::Context, Some(old_no), Some(new_no), ""),
            Some(_) => continue,
        };
        old_no += u32::from(old.is_some());
        new_no += u32::from(new.is_some());
        if row_count >= MAX_ROWS {
            out.truncated = true;
            break;
        }
        row_count += 1;
        file.rows.push(DiffRow {
            kind,
            old,
            new,
            text: body.to_string(),
        });
    }
    out
}

/// Parse `https://github.com/<owner>/<repo>/pull/<n>`, with any trailing path.
pub fn parse_pr_url(url: &str) -> Result<PrRef, String> {
    let refuse = || format!("not a GitHub pull URL: {url}");
    let path = url.strip_prefix("https://github.com/").ok_or_else(refuse)?;
    let mut parts = path.split('/');
    let (owner, repo, pull, number) = (parts.next(), parts.next(), parts.next(), parts.next());
    match (owner, repo, pull, number) {
        (Some(owner), Some(repo), Some("pull"), Some(number))
            if !owner.is_empty() && !repo.is_empty() =>
        {
            Ok(PrRef {
                owner: owner.to_string(),
                repo: repo.to_string(),
                number: number
                    .split(['#', '?'])
                    .next()
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| refuse())?,
            })
        }
        _ => Err(refuse()),
    }
}

/// The REST path for this pull request.
pub fn api_path(pr: &PrRef) -> String {
    format!("repos/{}/{}/pulls/{}", pr.owner, pr.repo, pr.number)
}

/// The `gh` argv that reads the diff.
pub fn diff_argv(pr: &PrRef) -> Vec<String> {
    vec![
        "api".into(),
        api_path(pr),
        "-H".into(),
        "Accept: application/vnd.github.diff".into(),
    ]
}

/// The `gh` argv that reads the head sha.
pub fn head_argv(pr: &PrRef) -> Vec<String> {
    vec![
        "api".into(),
        api_path(pr),
        "--jq".into(),
        ".head.sha".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n\
index 111..222 100644\n\
--- a/src/a.rs\n\
+++ b/src/a.rs\n\
@@ -2974,6 +2974,7 @@ fn outer() {\n\
\x20keep one\n\
-gone\n\
+fresh\n\
+fresher\n\
\x20keep two\n\
\\ No newline at end of file\n\
diff --git a/b.bin b/b.bin\n\
Binary files a/b.bin and b/b.bin differ\n";

    #[test]
    fn rows_carry_side_numbers_and_a_bad_url_refuses() {
        let diff = parse(DIFF);
        assert_eq!(diff.files.len(), 2);
        assert_eq!(diff.files[1].status, FileStatus::Binary);
        assert_eq!(diff.totals(), (2, 1));
        let numbered: Vec<_> = diff.files[0]
            .rows
            .iter()
            .map(|r| (r.kind, r.old, r.new))
            .collect();
        assert_eq!(
            numbered,
            vec![
                (RowKind::Hunk, None, None),
                (RowKind::Context, Some(2974), Some(2974)),
                (RowKind::Removed, Some(2975), None),
                (RowKind::Added, None, Some(2975)),
                (RowKind::Added, None, Some(2976)),
                (RowKind::Context, Some(2976), Some(2977)),
            ]
        );

        let pr = parse_pr_url("https://github.com/o/r/pull/12").expect("a pull URL");
        assert_eq!(api_path(&pr), "repos/o/r/pulls/12");
        let bad = "https://github.com/o/r/issues/12";
        let e = parse_pr_url(bad).expect_err("an issue URL refuses");
        assert!(e.contains(bad), "{e}");
    }
}

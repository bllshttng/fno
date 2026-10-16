//! Conditional GETs for the status door: an eligible `gh api` read goes out
//! once as `gh api -i` with the row's ETag in `If-None-Match`, and a 304 is
//! served from the local row. A 304 still costs a request to GitHub's
//! secondary limiter, but it does not spend core quota - the cure this
//! door can be, before the one-poller slice lowers the request rate.
//! A missing, corrupt, or stale row is a miss, never an error: the plain
//! network read stays the truth (the gh_cache.rs rule).

use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Process-wide count of 304s the door served; the spend notes read it.
pub(crate) static NOT_MODIFIED: AtomicUsize = AtomicUsize::new(0);

/// How long a row may seed If-None-Match. After this the row is a miss, so
/// a dead head sha cannot pin stale bodies forever.
const ROW_TTL_S: f64 = 24.0 * 3600.0;

/// The argv shapes the door serves: exactly `["api", path]` where path is a
/// plain REST read under `repos/`. A flag in the second token (write,
/// method, paginate, jq - anything starting `-`) fails the prefix check, so
/// writes never qualify; `graphql` is not under `repos/`.
pub(crate) fn eligible(args: &[String]) -> Option<&str> {
    let [bin, path] = args else {
        return None;
    };
    if bin != "api" {
        return None;
    }
    let path = path.as_str();
    if !path.starts_with("repos/") || path.contains("/logs") || past_first_page(path) {
        return None;
    }
    Some(path)
}

/// Pages past the first stay unconditional: each page's row carries its own
/// generation, so a 304-served page 2 could mix check-runs with a fresh
/// page 1 into one inconsistent assembly. Only a real `page=` query param
/// counts; the `page=` inside `per_page=` must not.
fn past_first_page(path: &str) -> bool {
    let mut rest = path;
    while let Some(i) = rest.find("page=") {
        let is_param = i > 0 && matches!(rest.as_bytes()[i - 1], b'?' | b'&');
        let value = &rest[i + "page=".len()..];
        let digits: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
        if is_param && !digits.is_empty() && digits != "1" {
            return true;
        }
        rest = value;
    }
    false
}

fn row_path(root: &Path, path: &str) -> PathBuf {
    let hex: String = Sha256::digest(path.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    root.join("etag").join(format!("{hex}.json"))
}

struct Row {
    etag: String,
    body: String,
}

fn read_row_at(root: &Path, path: &str, now: f64) -> Option<Row> {
    let text = fs::read_to_string(row_path(root, path)).ok()?;
    let row: serde_json::Value = serde_json::from_str(&text).ok()?;
    let etag = row.get("etag")?.as_str()?.to_string();
    let body = row.get("body")?.as_str()?.to_string();
    let ts = row.get("ts")?.as_f64()?;
    if ts <= 0.0 || now - ts >= ROW_TTL_S {
        return None;
    }
    Some(Row { etag, body })
}

fn write_row_at(root: &Path, path: &str, etag: &str, body: &str) -> bool {
    let file = row_path(root, path);
    let text = json!({"etag": etag, "body": body, "ts": crate::gh_cache::now_secs()}).to_string();
    if fs::create_dir_all(file.parent().unwrap_or(root)).is_err() {
        return false;
    }
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, text).is_ok() && fs::rename(&tmp, &file).is_ok()
}

/// Split a `gh api -i` stdout into (status code, header block, body): the
/// status line is the first line, headers end at the first blank line, the
/// rest is the body. gh exits 1 on a 304, so the caller reads the code, not
/// the exit status.
fn split_http(stdout: &str) -> Option<(u16, String, String)> {
    let (head, body) = match stdout.find("\r\n\r\n") {
        Some(i) => (&stdout[..i], &stdout[i + 4..]),
        None => {
            let i = stdout.find("\n\n")?;
            (&stdout[..i], &stdout[i + 2..])
        }
    };
    let status_line = head.lines().next()?;
    let status: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, head.to_string(), body.to_string()))
}

fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (key, val) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| val.trim().to_string())
    })
}

/// The one entry the status door calls. Eligible GETs run through
/// `gh api -i` with the stored ETag; everything else is a plain read.
pub(crate) fn conditional_api_get(
    cwd: &Path,
    path: &str,
    spawn: impl Fn(&[&str]) -> Result<(bool, Vec<u8>, String), String>,
) -> Result<(bool, String, String), String> {
    match crate::gh_cache::rows_root(cwd) {
        Some(root) => conditional_api_get_at(&root, path, spawn),
        None => plain_get(path, spawn),
    }
}

fn plain_get(
    path: &str,
    spawn: impl Fn(&[&str]) -> Result<(bool, Vec<u8>, String), String>,
) -> Result<(bool, String, String), String> {
    let (ok, stdout, stderr) = spawn(&["api", path])?;
    Ok((ok, String::from_utf8_lossy(&stdout).into_owned(), stderr))
}

/// The door with the row store pinned, so tests can point it at a tempdir
/// without touching the process-global FNO_GH_FACTS_DIR.
pub(crate) fn conditional_api_get_at(
    root: &Path,
    path: &str,
    spawn: impl Fn(&[&str]) -> Result<(bool, Vec<u8>, String), String>,
) -> Result<(bool, String, String), String> {
    let row = read_row_at(root, path, crate::gh_cache::now_secs());
    let mut args: Vec<&str> = vec!["api", "-i", path];
    let cond;
    if let Some(row) = &row {
        // The stored value goes out verbatim; the probe showed a weak
        // W/"..." ETag returns 304.
        cond = format!("If-None-Match: {}", row.etag);
        args.push("-H");
        args.push(&cond);
    }
    let (ok, stdout_bytes, stderr) = spawn(&args)?;
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let Some((status, headers, body)) = split_http(&stdout) else {
        return Ok((ok, stdout, stderr));
    };
    match status {
        304 => match row {
            Some(row) => {
                NOT_MODIFIED.fetch_add(1, Ordering::SeqCst);
                Ok((true, row.body, String::new()))
            }
            None => Ok((ok, body, stderr)),
        },
        200 => {
            if let Some(etag) = header_value(&headers, "etag") {
                write_row_at(root, path, &etag, &body);
            }
            Ok((true, body, stderr))
        }
        _ => Ok((ok, body, stderr)),
    }
}

/// The spend-note suffix: `, {n} served 304` once the process has served a
/// 304, empty before, so callers that parse the note never see a new shape
/// on the plain path.
pub(crate) fn note_suffix() -> String {
    let n = NOT_MODIFIED.load(Ordering::SeqCst);
    if n > 0 {
        format!(", {n} served 304")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_of(
        stdout: &str,
        exit_ok: bool,
    ) -> impl Fn(&[&str]) -> Result<(bool, Vec<u8>, String), String> {
        let stdout = stdout.to_string();
        move |_args| Ok((exit_ok, stdout.clone().into_bytes(), String::new()))
    }

    #[test]
    fn conditional_serves_row_on_304() {
        let root = tempfile::tempdir().unwrap();
        write_row_at(root.path(), "repos/o/r/pulls/1", "W/\"abc\"", "{\"n\":1}");
        let before = NOT_MODIFIED.load(Ordering::SeqCst);
        let spawn = spawn_of(
            "HTTP/2.0 304 Not Modified\r\netag: W/\"abc\"\r\n\r\n",
            false,
        );
        let (ok, body, stderr) =
            conditional_api_get_at(root.path(), "repos/o/r/pulls/1", spawn).unwrap();
        assert!(ok);
        assert_eq!(body, "{\"n\":1}");
        assert_eq!(stderr, "");
        assert_eq!(NOT_MODIFIED.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn first_200_seeds_row_and_returns_body() {
        let root = tempfile::tempdir().unwrap();
        let stdout = "HTTP/2.0 200 OK\r\ncontent-type: application/json\r\nEtag: W/\"zz\"\r\n\r\n{\"y\":2}\n";
        let spawn = spawn_of(stdout, true);
        let (ok, body, _) =
            conditional_api_get_at(root.path(), "repos/o/r/pulls/2", spawn).unwrap();
        assert!(ok);
        assert_eq!(body, "{\"y\":2}\n");
        let now = crate::gh_cache::now_secs();
        let row = read_row_at(root.path(), "repos/o/r/pulls/2", now).expect("row written");
        assert_eq!(row.etag, "W/\"zz\"");
        assert_eq!(row.body, "{\"y\":2}\n");
    }

    #[test]
    fn budget_refusal_propagates_and_writes_no_row() {
        let root = tempfile::tempdir().unwrap();
        write_row_at(root.path(), "repos/o/r/pulls/3", "W/\"old\"", "{\"n\":0}");
        let before = NOT_MODIFIED.load(Ordering::SeqCst);
        let spawn = |_args: &[&str]| -> Result<(bool, Vec<u8>, String), String> {
            Err("pr-status: refused by the fleet request budget".to_string())
        };
        let err = conditional_api_get_at(root.path(), "repos/o/r/pulls/3", spawn).unwrap_err();
        assert!(err.contains("refused"));
        let now = crate::gh_cache::now_secs();
        let row = read_row_at(root.path(), "repos/o/r/pulls/3", now).expect("row untouched");
        assert_eq!(row.etag, "W/\"old\"");
        assert_eq!(NOT_MODIFIED.load(Ordering::SeqCst), before);
    }

    #[test]
    fn eligible_rejects_writes_pages_and_logs() {
        let api = "api".to_string();
        let plain = vec![api.clone(), "repos/o/r/pulls/1".to_string()];
        assert_eq!(eligible(&plain), Some("repos/o/r/pulls/1"));
        let write = vec![
            api.clone(),
            "-X".to_string(),
            "POST".to_string(),
            "repos/o/r/pulls/1".to_string(),
        ];
        assert_eq!(eligible(&write), None);
        let page2 = vec![
            api.clone(),
            "repos/o/r/commits/sha/check-runs?per_page=100&page=2".to_string(),
        ];
        assert_eq!(eligible(&page2), None);
        let page1 = vec![
            api.clone(),
            "repos/o/r/commits/sha/check-runs?per_page=100&page=1".to_string(),
        ];
        assert_eq!(
            eligible(&page1),
            Some("repos/o/r/commits/sha/check-runs?per_page=100&page=1")
        );
        let logs = vec![api.clone(), "repos/o/r/actions/jobs/9/logs".to_string()];
        assert_eq!(eligible(&logs), None);
        let not_repos = vec![api, "graphql".to_string()];
        assert_eq!(eligible(&not_repos), None);
    }
}

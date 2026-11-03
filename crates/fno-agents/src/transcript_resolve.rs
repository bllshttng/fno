//! Resolve a provenance pointer `(harness, session_id, cwd)` to its on-disk
//! transcript: the one owner of the per-harness store layouts, ported from the
//! deleted `cli/src/fno/provenance/resolver.py`. The dispatch-brief tail rung
//! calls it in process; the Python agents runtime reads it through
//! `fno-agents attest resolve-transcript`.
//!
//! Never fails. `resolved` is true only when an actual store entry was found;
//! a miss carries one reason word: `missing-input`, `not-found`,
//! `harness-not-supported`, or `error`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Store-root overrides. `None` reads the host's real store, so tests inject
/// every root they touch.
#[derive(Default, Clone)]
pub struct Roots {
    pub projects_root: Option<PathBuf>,
    pub codex_sessions_dir: Option<PathBuf>,
    pub opencode_db: Option<PathBuf>,
}

/// The transcript-store answer for one provenance pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTranscript {
    pub resolved: bool,
    pub path: Option<PathBuf>,
    /// `jsonl` (a per-session file) or `opencode-db` (the path names the
    /// SQLite store and the session id is the lookup key).
    pub kind: &'static str,
    pub ambiguous: bool,
    pub reason: Option<&'static str>,
}

fn miss(reason: &'static str) -> ResolvedTranscript {
    ResolvedTranscript {
        resolved: false,
        path: None,
        kind: "jsonl",
        ambiguous: false,
        reason: Some(reason),
    }
}

fn hit(path: PathBuf, kind: &'static str, ambiguous: bool) -> ResolvedTranscript {
    ResolvedTranscript {
        resolved: true,
        path: Some(path),
        kind,
        ambiguous,
        reason: None,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The codex rollout store: the env override, `$CODEX_HOME/sessions`, else
/// `~/.codex/sessions`.
fn default_codex_sessions_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_CODEX_SESSIONS_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("sessions"));
    }
    home().map(|h| h.join(".codex").join("sessions"))
}

/// The opencode SQLite store, the sibling of the legacy storage tree.
fn default_opencode_db() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_OPENCODE_STORAGE_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v).parent()?.join("opencode.db"));
    }
    home().map(|h| {
        h.join(".local")
            .join("share")
            .join("opencode")
            .join("opencode.db")
    })
}

pub fn resolve_transcript(
    harness: &str,
    session_id: &str,
    cwd: &str,
    roots: &Roots,
) -> ResolvedTranscript {
    if session_id.is_empty() {
        return miss("missing-input");
    }
    match harness {
        // codex and opencode key on the session id alone; only claude needs
        // cwd, and even claude searches every project dir.
        "codex" => resolve_codex(session_id, roots),
        "opencode" => resolve_opencode(session_id, roots),
        "claude" if cwd.is_empty() => miss("missing-input"),
        "claude" => resolve_claude(session_id, roots),
        _ => miss("harness-not-supported"),
    }
}

/// True iff the transcript holds at least one user/assistant turn: a real
/// transcript, not the metadata-only stub a worktree re-key leaves behind.
fn claude_has_conversation(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        serde_json::from_str::<Value>(line.trim()).is_ok_and(|rec| {
            matches!(
                rec.get("type").and_then(Value::as_str),
                Some("user") | Some("assistant")
            )
        })
    })
}

fn mtime_secs(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A session's transcript can sit in more than one project dir: entering a
/// worktree re-keys it to the worktree cwd's slug and leaves a stub in the
/// canonical one. So the search spans every project dir.
fn resolve_claude(session_id: &str, roots: &Roots) -> ResolvedTranscript {
    let Some(root) = roots
        .projects_root
        .clone()
        .or_else(|| home().map(|h| h.join(".claude").join("projects")))
    else {
        return miss("error");
    };
    let mut matches: Vec<PathBuf> = Vec::new();
    for dir in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let dir = dir.path();
        if !dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = file.path();
            let Some(stem) = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".jsonl"))
            else {
                continue;
            };
            // A dotted stem is a sibling artifact (`<uuid>.orphaned-...`),
            // never a transcript: it would make a full uuid read ambiguous.
            if !stem.contains('.') && stem.starts_with(session_id) {
                matches.push(p);
            }
        }
    }
    matches.sort();
    if matches.is_empty() {
        return miss("not-found");
    }
    let stems: std::collections::BTreeSet<&std::ffi::OsStr> =
        matches.iter().filter_map(|p| p.file_name()).collect();
    if stems.len() > 1 {
        // A short prefix matched two DISTINCT sessions: first-sorted and
        // ambiguous, never a guess across two sessions.
        return hit(matches.swap_remove(0), "jsonl", true);
    }
    if matches.len() == 1 {
        return hit(matches.swap_remove(0), "jsonl", false);
    }
    // One session copied across dirs. Newest mtime alone picks the stub,
    // whose creation can post-date the real transcript's last turn, so
    // prefer the copies that carry conversation and take the newest of those.
    let with_convo: Vec<PathBuf> = matches
        .iter()
        .filter(|p| claude_has_conversation(p))
        .cloned()
        .collect();
    let pool = if with_convo.is_empty() {
        matches
    } else {
        with_convo
    };
    match pool
        .into_iter()
        .filter_map(|p| mtime_secs(&p).map(|t| (t, p)))
        .max_by_key(|(t, _)| *t)
    {
        Some((_, path)) => hit(path, "jsonl", false),
        // Every candidate vanished mid-scan.
        None => miss("not-found"),
    }
}

fn collect_rollouts(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_rollouts(&p, out);
        } else if p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            out.push(p);
        }
    }
}

/// The session id the rollout's first session_meta line carries.
fn codex_meta_session(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    std::io::BufReader::new(file)
        .lines()
        .take(4)
        .find_map(|line| {
            let rec = serde_json::from_str::<Value>(line.ok()?.trim()).ok()?;
            if rec.get("type").and_then(Value::as_str) != Some("session_meta") {
                return None;
            }
            let payload = rec.get("payload").unwrap_or(&rec);
            payload
                .get("id")
                .or_else(|| rec.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn resolve_codex(session_id: &str, roots: &Roots) -> ResolvedTranscript {
    let Some(root) = roots
        .codex_sessions_dir
        .clone()
        .or_else(default_codex_sessions_dir)
    else {
        return miss("not-found");
    };
    let mut rollouts = Vec::new();
    collect_rollouts(&root, &mut rollouts);
    // Fast path: codex embeds the session uuid in the rollout filename.
    if let Some(i) = rollouts.iter().position(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(session_id))
    }) {
        return hit(rollouts.swap_remove(i), "jsonl", false);
    }
    // Fallback: a rollout named by a turn id carries the session id in its
    // first session_meta line; scan newest-first.
    rollouts.sort_by_key(|p| std::cmp::Reverse(mtime_secs(p)));
    match rollouts
        .into_iter()
        .find(|p| codex_meta_session(p).as_deref() == Some(session_id))
    {
        Some(path) => hit(path, "jsonl", false),
        None => miss("not-found"),
    }
}

/// A read-only existence probe: a live opencode holds the store open in WAL
/// mode, and a stray write there is destructive.
fn resolve_opencode(session_id: &str, roots: &Roots) -> ResolvedTranscript {
    let Some(store) = roots.opencode_db.clone().or_else(default_opencode_db) else {
        return miss("not-found");
    };
    if !store.exists() {
        return miss("not-found");
    }
    let Some(conn) = crate::graph_store::open_read_only_wal_safe(&store) else {
        return miss("error");
    };
    let found = conn
        .prepare("SELECT 1 FROM session WHERE id = ?1 LIMIT 1")
        .and_then(|mut stmt| stmt.query([session_id])?.next().map(|row| row.is_some()));
    match found {
        Ok(true) => hit(store, "opencode-db", false),
        Ok(false) => miss("not-found"),
        Err(_) => miss("error"),
    }
}

/// The receipt the door prints: the deleted Python dataclass's dict.
pub fn receipt(harness: &str, session_id: &str, cwd: &str, rt: &ResolvedTranscript) -> Value {
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    json!({
        "harness": opt(harness),
        "session_id": opt(session_id),
        "cwd": opt(cwd),
        "resolved": rt.resolved,
        "transcript_path": rt.path.as_ref().map(|p| p.display().to_string()),
        "reason": rt.reason,
        "ambiguous": rt.ambiguous,
        "kind": rt.kind,
    })
}

/// `fno-agents attest resolve-transcript --harness H --session-id S --cwd C
/// [--projects-root P] [--codex-sessions-dir D] [--opencode-db F]`: one JSON
/// receipt, exit 0 whether or not the pointer resolved.
pub fn run(args: &[String]) -> i32 {
    let (mut harness, mut session_id, mut cwd) = (String::new(), String::new(), String::new());
    let mut roots = Roots::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().unwrap_or_default();
        match arg.as_str() {
            "--harness" => harness = value(),
            "--session-id" => session_id = value(),
            "--cwd" => cwd = value(),
            "--projects-root" => roots.projects_root = Some(PathBuf::from(value())),
            "--codex-sessions-dir" => roots.codex_sessions_dir = Some(PathBuf::from(value())),
            "--opencode-db" => roots.opencode_db = Some(PathBuf::from(value())),
            other => {
                eprintln!("error: unknown resolve-transcript argument {other}");
                return 2;
            }
        }
    }
    let rt = resolve_transcript(&harness, &session_id, &cwd, &roots);
    println!("{}", receipt(&harness, &session_id, &cwd, &rt));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    const UUID: &str = "0123abcd-1111-2222-3333-444455556666";

    fn roots(tmp: &TempDir) -> Roots {
        Roots {
            projects_root: Some(tmp.path().join("projects")),
            codex_sessions_dir: Some(tmp.path().join("codex")),
            opencode_db: Some(tmp.path().join("opencode.db")),
        }
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn degenerate_inputs_name_their_reason() {
        let tmp = TempDir::new().unwrap();
        let r = roots(&tmp);
        assert_eq!(
            resolve_transcript("claude", "", "/x", &r).reason,
            Some("missing-input")
        );
        assert_eq!(
            resolve_transcript("claude", UUID, "", &r).reason,
            Some("missing-input")
        );
        assert_eq!(
            resolve_transcript("gemini", UUID, "/x", &r).reason,
            Some("harness-not-supported")
        );
        assert_eq!(
            resolve_transcript("claude", UUID, "/x", &r).reason,
            Some("not-found")
        );
        assert_eq!(
            resolve_transcript("codex", UUID, "", &r).reason,
            Some("not-found")
        );
        assert_eq!(
            resolve_transcript("opencode", "ses_1", "", &r).reason,
            Some("not-found")
        );
    }

    #[test]
    fn claude_searches_every_project_dir_and_skips_dotted_stems() {
        let tmp = TempDir::new().unwrap();
        let r = roots(&tmp);
        let real = tmp.path().join(format!("projects/-wt/{UUID}.jsonl"));
        write(&real, "{\"type\":\"user\"}\n");
        write(
            &tmp.path()
                .join(format!("projects/-wt/{UUID}.orphaned-1.jsonl")),
            "{}\n",
        );
        let rt = resolve_transcript("claude", UUID, "/canonical", &r);
        assert_eq!((rt.resolved, rt.ambiguous), (true, false));
        assert_eq!(rt.path.as_deref(), Some(real.as_path()));
        // An 8-hex prefix resolves the same file.
        assert_eq!(
            resolve_transcript("claude", &UUID[..8], "/x", &r).path,
            rt.path
        );
    }

    #[test]
    fn a_prefix_matching_two_sessions_is_ambiguous() {
        let tmp = TempDir::new().unwrap();
        let r = roots(&tmp);
        write(&tmp.path().join("projects/-a/0123abcd-aaaa.jsonl"), "{}\n");
        write(&tmp.path().join("projects/-b/0123abcd-bbbb.jsonl"), "{}\n");
        let rt = resolve_transcript("claude", "0123abcd", "/x", &r);
        assert!(rt.resolved && rt.ambiguous);
        assert!(rt.path.unwrap().ends_with("-a/0123abcd-aaaa.jsonl"));
    }

    #[test]
    fn a_copied_session_prefers_the_copy_with_conversation_over_a_newer_stub() {
        let tmp = TempDir::new().unwrap();
        let r = roots(&tmp);
        let real = tmp.path().join(format!("projects/-canon/{UUID}.jsonl"));
        write(&real, "{\"type\":\"assistant\"}\n");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(
            &tmp.path().join(format!("projects/-wt/{UUID}.jsonl")),
            "{\"type\":\"last-prompt\"}\n",
        );
        let rt = resolve_transcript("claude", UUID, "/x", &r);
        assert_eq!(rt.path.as_deref(), Some(real.as_path()));
        assert!(!rt.ambiguous);
    }

    #[test]
    fn codex_matches_the_filename_then_the_session_meta_line() {
        let tmp = TempDir::new().unwrap();
        let r = roots(&tmp);
        let named = tmp
            .path()
            .join(format!("codex/2026/11/rollout-1-{UUID}.jsonl"));
        write(&named, "{}\n");
        assert_eq!(resolve_transcript("codex", UUID, "", &r).path, Some(named));
        let by_meta = tmp.path().join("codex/2026/11/rollout-2-turn.jsonl");
        write(
            &by_meta,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-xyz\"}}\n",
        );
        let rt = resolve_transcript("codex", "sess-xyz", "", &r);
        assert_eq!((rt.path, rt.kind), (Some(by_meta), "jsonl"));
    }

    #[test]
    fn opencode_probes_the_store_read_only() {
        let tmp = TempDir::new().unwrap();
        let r = roots(&tmp);
        let db = tmp.path().join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT); INSERT INTO session VALUES ('ses_Ab');",
        )
        .unwrap();
        drop(conn);
        let rt = resolve_transcript("opencode", "ses_Ab", "", &r);
        assert_eq!((rt.resolved, rt.kind), (true, "opencode-db"));
        // Ids are never case-folded.
        assert_eq!(
            resolve_transcript("opencode", "ses_ab", "", &r).reason,
            Some("not-found")
        );
    }

    #[test]
    fn the_receipt_keeps_the_python_field_set() {
        let rt = miss("not-found");
        let v = receipt("claude", UUID, "", &rt);
        assert_eq!(v["cwd"], Value::Null);
        assert_eq!(v["reason"], "not-found");
        assert_eq!(v["kind"], "jsonl");
        assert_eq!(v["transcript_path"], Value::Null);
    }
}

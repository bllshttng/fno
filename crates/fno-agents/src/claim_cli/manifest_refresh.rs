//! Heal the session manifest a claim handover was once refused under.
//!
//! `fno do target init` writes `target_claim_blocked_reason:
//! claim_held_by_other` (hooks/helpers/init-target-state.sh) when the worker's
//! acquire meets the spawn window claim and the handover adopt does not fire.
//! The window claim stays adoptable, and when the handover later completes the
//! stale line plus the init-authored `.target-cancelled` sentinel keep the
//! stop gate refusing every watching tag (the manifest carries no
//! `target_claim_key`/`target_claim_holder` pair to renew). This module is the
//! refresh at that completion point: called from the `--handover-from`
//! success arm beside `stamp_do_on_acquire`, it drops the blocked line,
//! appends the claim lines the gate scans, and consumes only the init-authored
//! sentinel - a user cancel is never touched.
//!
//! Best-effort end to end, exactly like the stamp: every failure is a named
//! stderr skip and never fails the acquire.

use std::io::Write as _;
use std::path::Path;

/// Read one `field: value` line from anywhere in the manifest, quotes
/// stripped. Mirrors the scan the stop gate runs (`loopcheck`'s
/// `scan_manifest_field`), so the refresh guards on exactly what a reader
/// would see.
fn scan_field(content: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    content.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(&prefix)
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
            .filter(|v| !v.is_empty())
    })
}

/// The flock handle for `<manifest>.lock`. Dropping it closes the file and
/// releases the lock.
struct ManifestLock {
    _file: std::fs::File,
}

/// Take the same `<manifest>.lock` flock `fno do state set` holds while it
/// rewrites the manifest (cli/src/fno/state/io.py uses the filelock package,
/// which flocks this path on unix). Best-effort: `None` after ~5s of
/// contention means the refresh skips rather than racing the other writer.
fn lock_manifest(manifest: &Path) -> Option<ManifestLock> {
    use std::os::fd::AsRawFd;
    let name = manifest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("target-state.md");
    let lock_path = manifest.with_file_name(format!("{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .ok()?;
    let fd = file.as_raw_fd();
    for try_index in 0..10 {
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Some(ManifestLock { _file: file });
        }
        if try_index + 1 < 10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    eprintln!(
        "claim handover: manifest refresh skipped: {} stayed locked",
        lock_path.display()
    );
    None
}

/// The entry point the acquire leaf calls: resolves the manifest for this
/// process's cwd.
pub(crate) fn refresh_manifest_on_handover(key: &str, new_holder: &str, ttl: Option<&str>) {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("claim handover: manifest refresh skipped for {key}: no cwd ({e})");
            return;
        }
    };
    refresh_manifest_on_handover_at(&cwd, key, new_holder, ttl);
}

/// The testable core: heal the session manifest for `cwd`, if it names this
/// node and still carries the poison. `ttl` rides through as the raw `--ttl`
/// string the acquire was given; when absent no ttl line is written and the
/// stop gate's 2h default applies.
pub(crate) fn refresh_manifest_on_handover_at(
    cwd: &Path,
    key: &str,
    new_holder: &str,
    ttl: Option<&str>,
) {
    let Some(node_id) = key.strip_prefix("node:").filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(manifest_path) = crate::state_path::resolve("target-state", cwd) else {
        return;
    };
    if !manifest_path.exists() {
        // No session manifest here: a handover can complete outside any
        // target session, and an absent manifest is a nothing, not a skip
        // worth a line. A poisoned manifest is the only thing this module
        // exists to heal.
        return;
    }
    // Serialize with `fno do state set`, which rewrites the manifest under
    // the same `<path>.lock` flock. A lost update in either direction would
    // either erase a legal plan_path first-fill or resurrect the blocked
    // line.
    let Some(_lock) = lock_manifest(&manifest_path) else {
        return;
    };
    let Ok(content) = std::fs::read_to_string(&manifest_path) else {
        eprintln!(
            "claim handover: manifest refresh skipped for {key}: unreadable {}",
            manifest_path.display()
        );
        return;
    };
    // Identity guard: only the manifest of THIS node's own session heals. A
    // bystander's manifest naming another node is never touched, whatever its
    // state.
    match scan_field(&content, "graph_node_id") {
        Some(id) if id == node_id => {}
        Some(other) => {
            eprintln!(
                "claim handover: manifest refresh skipped for {key}: manifest names node {other}"
            );
            return;
        }
        None => {
            eprintln!("claim handover: manifest refresh skipped for {key}: manifest names no node");
            return;
        }
    }
    // Only the manifest's own session takes over its claim identity. A manual
    // or programmatic acquire under any other holder must not rewrite this
    // manifest's claim lines or consume its init sentinel.
    let own_session = new_holder
        .strip_prefix("target-session:")
        .is_some_and(|holder_session| {
            [
                scan_field(&content, "session_id"),
                scan_field(&content, "harness_session_id"),
            ]
            .iter()
            .flatten()
            .any(|sid| sid == holder_session)
        });
    if !own_session {
        eprintln!(
            "claim handover: manifest refresh skipped for {key}: holder {new_holder} is not this manifest's session"
        );
        return;
    }
    let blocked_marker = "target_claim_blocked_reason:";
    let had_blocked = content
        .lines()
        .any(|l| l.trim().starts_with(blocked_marker));
    if !had_blocked {
        // Nothing this module owns is wrong with the manifest; the healthy
        // claim lines (if any) are already the reader's truth.
        return;
    }
    let kept: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim().starts_with(blocked_marker))
        .collect();
    let mut out = kept.join("\n");
    if content.ends_with('\n') && !out.ends_with('\n') && !out.is_empty() {
        // lines() drops the trailing newline; the rewritten manifest keeps
        // whatever the original ended with.
        out.push('\n');
    }
    if scan_field(&content, "target_claim_key").is_none() {
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("target_claim_key: \"{key}\"\n"));
        out.push_str(&format!("target_claim_holder: \"{new_holder}\"\n"));
        if let Some(t) = ttl.map(str::trim).filter(|t| !t.is_empty()) {
            out.push_str(&format!("target_claim_ttl: \"{t}\"\n"));
        }
    }
    if let Err(e) = atomic_write(&manifest_path, &out) {
        eprintln!(
            "claim handover: manifest refresh failed for {key}: could not write {} ({e})",
            manifest_path.display()
        );
        return;
    }
    eprintln!(
        "claim handover: manifest refreshed for {key} (blocked line dropped, claim lines {})",
        if scan_field(&content, "target_claim_key").is_none() {
            "appended"
        } else {
            "kept"
        }
    );
    consume_init_sentinel(&manifest_path.with_file_name(".target-cancelled"));
    consume_init_sentinel(&cwd.join(".fno").join(".target-cancelled"));
}

/// Consume `.target-cancelled` only when init authored it for this exact
/// refusal (`author: init` + `reason: claim_held_by_other`). Any other author,
/// reason, or a `via: user` cancel stays.
fn consume_init_sentinel(path: &Path) {
    let claim_path = path.with_file_name(format!(
        ".{}.consumed-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("target-cancelled"),
        std::process::id()
    ));
    // Rename first, judge second: the file leaves the sentinel path
    // atomically, so an operator cancel written between the read and the
    // remove can never be consumed - the worst case renames the operator's
    // fresh sentinel aside and best-effort renames it back.
    if std::fs::rename(path, &claim_path).is_err() {
        return;
    }
    let Ok(content) = std::fs::read_to_string(&claim_path) else {
        // The renamed claim vanished; nothing left to restore or judge.
        return;
    };
    let (author, reason, _via) = crate::cancel_sentinel::parse_cancel_payload(&content);
    if author.as_deref() != Some("init") || reason.as_deref() != Some("claim_held_by_other") {
        // Not ours to consume: put it back, best-effort.
        let _ = std::fs::rename(&claim_path, path);
        return;
    }
    if let Err(e) = std::fs::remove_file(&claim_path) {
        eprintln!(
            "claim handover: could not remove the consumed init sentinel copy at {} ({e})",
            claim_path.display()
        );
    }
}

/// Write via a sibling tempfile and rename, so a reader never sees a half
/// manifest.
fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.refresh-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("target-state.md"),
        std::process::id()
    ));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    match write() {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Pin the state roots to a temp home for one test. Mirrors the guard in
    /// `state_path.rs` tests; the shared env lock keeps parallel tests from
    /// flipping the pins under each other.
    struct EnvGuard {
        home: PathBuf,
        spaces: Option<std::ffi::OsString>,
        agents_home: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new(home: &Path) -> Self {
            let spaces = std::env::var_os("FNO_SPACES_DIR");
            let agents_home = std::env::var_os("FNO_AGENTS_HOME");
            std::env::set_var("FNO_AGENTS_HOME", home.join("agents-home"));
            std::env::set_var("FNO_SPACES_DIR", home.join("spaces"));
            Self {
                home: home.to_path_buf(),
                spaces,
                agents_home,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.spaces.take() {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match self.agents_home.take() {
                Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
                None => std::env::remove_var("FNO_AGENTS_HOME"),
            }
            let _ = &self.home;
        }
    }

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let status = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["-C", dir.to_str().unwrap(), "init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init failed for {}", dir.display());
    }

    const POISONED_MANIFEST: &str = "---\nfno_id: f\nsession_id: s1\nharness_session_id: h1\ncreated_at: '2026-10-25T00:00:00Z'\ninput: x-test1\n---\n# Target Session State\n\ntarget_claim_blocked_reason: claim_held_by_other\ngraph_node_id: x-test1\n";

    /// One canonical-repo session: the manifest resolves to
    /// `<spaces>/<slug>/target-state.md` and the sentinel beside it is the
    /// same file the reader checks at `<repo>/.fno/`.
    struct Session {
        repo: PathBuf,
        manifest: PathBuf,
        _guard: EnvGuard,
        base: PathBuf,
    }

    impl Session {
        fn new(tag: &str, manifest_body: &str) -> Self {
            let base =
                std::env::temp_dir().join(format!("fno-handover-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let repo = base.join("repo");
            init_repo(&repo);
            let guard = EnvGuard::new(&base);
            let manifest = crate::state_path::resolve("target-state", &repo).unwrap();
            fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            fs::write(&manifest, manifest_body).unwrap();
            Self {
                repo,
                manifest,
                _guard: guard,
                base,
            }
        }

        fn sentinel_beside_manifest(&self) -> PathBuf {
            self.manifest.parent().unwrap().join(".target-cancelled")
        }

        fn sentinel_at_repo_fno(&self) -> PathBuf {
            let dir = self.repo.join(".fno");
            fs::create_dir_all(&dir).unwrap();
            dir.join(".target-cancelled")
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn write_init_sentinel(path: &Path) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(path, "author: init\nreason: claim_held_by_other\n").unwrap();
    }

    #[test]
    fn heal_drops_blocked_line_appends_claim_pair_consumes_both_init_sentinels() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let s = Session::new("heal", POISONED_MANIFEST);
        write_init_sentinel(&s.sentinel_beside_manifest());
        write_init_sentinel(&s.sentinel_at_repo_fno());

        refresh_manifest_on_handover_at(&s.repo, "node:x-test1", "target-session:s1", Some("2h"));

        let healed = fs::read_to_string(&s.manifest).unwrap();
        assert!(
            !healed.contains("target_claim_blocked_reason:"),
            "blocked line must be gone: {healed}"
        );
        assert!(
            healed.contains("target_claim_key: \"node:x-test1\""),
            "{healed}"
        );
        assert!(
            healed.contains("target_claim_holder: \"target-session:s1\""),
            "{healed}"
        );
        assert!(healed.contains("target_claim_ttl: \"2h\""), "{healed}");
        assert!(healed.contains("graph_node_id: x-test1"), "{healed}");
        assert!(!s.sentinel_beside_manifest().exists());
        assert!(!s.sentinel_at_repo_fno().exists());
    }

    #[test]
    fn heal_spares_a_user_cancel_sentinel() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let s = Session::new("user", POISONED_MANIFEST);
        let user_sentinel = s.sentinel_beside_manifest();
        fs::write(
            &user_sentinel,
            "author: operator\nvia: user\nreason: done for the night\n",
        )
        .unwrap();
        let fno_sentinel = s.sentinel_at_repo_fno();
        write_init_sentinel(&fno_sentinel);

        refresh_manifest_on_handover_at(&s.repo, "node:x-test1", "target-session:s1", None);

        assert!(
            user_sentinel.exists(),
            "a user cancel is never consumed by a claim refresh"
        );
        assert_eq!(
            fs::read_to_string(&user_sentinel).unwrap(),
            "author: operator\nvia: user\nreason: done for the night\n",
            "the user sentinel round-trips byte-identical"
        );
        assert!(!fno_sentinel.exists());
        let litter: Vec<_> = fs::read_dir(s.manifest.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".consumed-"))
            .collect();
        assert!(
            litter.is_empty(),
            "no consumed-copy litter stays behind: {litter:?}"
        );
    }

    #[test]
    fn heal_skips_a_holder_that_is_not_the_manifests_session() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let s = Session::new("foreign", POISONED_MANIFEST);
        let before = fs::read_to_string(&s.manifest).unwrap();
        write_init_sentinel(&s.sentinel_beside_manifest());

        refresh_manifest_on_handover_at(
            &s.repo,
            "node:x-test1",
            "target-session:someone-else",
            None,
        );

        let after = fs::read_to_string(&s.manifest).unwrap();
        assert_eq!(
            before, after,
            "a foreign holder never rewrites the manifest"
        );
        assert!(
            s.sentinel_beside_manifest().exists(),
            "a foreign holder never consumes the init sentinel"
        );
    }

    #[test]
    fn heal_accepts_the_harness_session_id_as_the_own_holder() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let s = Session::new("harness", POISONED_MANIFEST);

        refresh_manifest_on_handover_at(&s.repo, "node:x-test1", "target-session:h1", Some("30m"));

        let healed = fs::read_to_string(&s.manifest).unwrap();
        assert!(
            healed.contains("target_claim_holder: \"target-session:h1\""),
            "the harness-id holder heals: {healed}"
        );
        assert!(healed.contains("target_claim_ttl: \"30m\""), "{healed}");
    }

    #[test]
    fn heal_touches_no_manifest_that_names_another_node() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let other = "---\ninput: x-other\n---\ntarget_claim_blocked_reason: claim_held_by_other\ngraph_node_id: x-other\n";
        let s = Session::new("other", other);
        let before = fs::read_to_string(&s.manifest).unwrap();

        refresh_manifest_on_handover_at(&s.repo, "node:x-test1", "target-session:s1", None);

        let after = fs::read_to_string(&s.manifest).unwrap();
        assert_eq!(before, after, "a bystander manifest is never touched");
    }

    #[test]
    fn heal_without_a_manifest_is_a_named_skip_and_creates_nothing() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("fno-handover-skip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let guard = EnvGuard::new(&base);
        let repo = base.join("repo");
        init_repo(&repo);

        // Must not panic and must not create anything; the skip is named on
        // stderr.
        refresh_manifest_on_handover_at(&repo, "node:x-test1", "target-session:s1", None);

        let slug = crate::paths::space_slug(&fs::canonicalize(&repo).unwrap());
        let space = base.join("spaces").join(slug);
        assert!(
            !space.join("target-state.md").exists(),
            "the refresh never creates a manifest"
        );
        drop(guard);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn heal_keeps_a_healthy_claim_pair_and_adds_no_duplicates() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let with_pair = "---\nsession_id: s1\nharness_session_id: h1\ninput: x-test1\n---\ntarget_claim_blocked_reason: claim_held_by_other\ntarget_claim_key: \"node:x-test1\"\ntarget_claim_holder: \"target-session:orig\"\ntarget_claim_ttl: \"2h\"\ngraph_node_id: x-test1\n";
        let s = Session::new("healthy", with_pair);

        refresh_manifest_on_handover_at(&s.repo, "node:x-test1", "target-session:s1", None);

        let healed = fs::read_to_string(&s.manifest).unwrap();
        assert!(!healed.contains("target_claim_blocked_reason:"), "{healed}");
        assert_eq!(
            healed.matches("target_claim_key:").count(),
            1,
            "no duplicate claim lines: {healed}"
        );
        assert!(
            healed.contains("target_claim_holder: \"target-session:orig\""),
            "the existing pair stays: {healed}"
        );
    }

    #[test]
    fn heal_without_ttl_writes_no_ttl_line() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let s = Session::new("nottl", POISONED_MANIFEST);

        refresh_manifest_on_handover_at(&s.repo, "node:x-test1", "target-session:s1", None);

        let healed = fs::read_to_string(&s.manifest).unwrap();
        assert!(
            healed.contains("target_claim_key: \"node:x-test1\""),
            "{healed}"
        );
        assert!(
            !healed.contains("target_claim_ttl:"),
            "no ttl in, no ttl line: {healed}"
        );
    }
}

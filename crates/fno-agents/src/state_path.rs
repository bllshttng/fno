//! `fno-agents state path <name>` -- the shell hook surface for project-space
//! path resolution, mirroring the accessor table in Python `fno.paths`. Shell
//! hooks call this instead of spelling `<repo>/.fno/<file>`, so the location
//! has one owner per language and the two agree through the slug contract in
//! `paths::space_slug`. Binary-direct (no daemon), resolved from the process
//! cwd. The `escalations` key is Rust-owned: nothing in Python reads the
//! directory, so it has no Python accessor to mirror. Same for the
//! `plans-dirs` subcommand (plans_dirs.rs): Rust-owned, with the plan-location
//! guard as its only consumer.

use std::path::PathBuf;

use crate::paths::{space_dir, worktree_repo_root, worktree_space_dir};

/// Print the resolved space path for one named state file. Unknown names
/// exit 2 with the known set, so a stale hook fails loud instead of writing
/// through a guessed path.
pub fn run(args: &[String]) -> i32 {
    // `state path <name>` and `state <name>` both work; the spelled-out
    // "path" matches how the hooks and docs say it.
    let args = if args.first().map(|a| a.as_str() == "path").unwrap_or(false) {
        &args[1..]
    } else {
        args
    };
    let Some(name) = args.first() else {
        eprintln!("usage: fno-agents state path <target-state|run-log|events|plans|inbox|leads|scratchpad|status-sinks|worktree-log|escalations|questions|plans-dirs|plan-dir|plan-path|migrate|dir|mint-id>");
        return 2;
    };
    // `plans-dirs` is not a single-path accessor: it answers with one dir per
    // REGISTERED project (see plans_dirs.rs), so it dispatches before the
    // name-to-path table. `plan-dir`/`plan-path` are the plans-chain port
    // (plans_path.rs): the Python command and the probe both resolve through it.
    // `migrate` is the state-root migration: same pre-table dispatch, since
    // it answers a receipt, not a path (state_layout.rs).
    if name == "plans-dirs" {
        return crate::plans_dirs::run(&args[1..]);
    }
    if name == "migrate" {
        return crate::state_layout::run_migrate_cli(&args[1..]);
    }
    // `dir` is the state-root folder door (state_layout.rs's dir table): a
    // hook or a Python port asks for a folder by name instead of spelling
    // it, so the layout table stays the one path vocabulary and a fold
    // retargets every caller at once. Same pre-table dispatch as `migrate`:
    // it answers the machine state root, not the per-cwd space table below.
    if name == "dir" {
        return run_dir(&args[1..]);
    }
    if name == "plan-dir" {
        return crate::plans_path::run_plan_dir(&args[1..]);
    }
    if name == "plan-path" {
        return crate::plans_path::run_plan_path(&args[1..]);
    }
    // `mint-id` is the shell door to the crate's one session-id mint: hooks
    // and Python forwarders call it instead of minting their own, so one
    // function owns the id's shape. The mint is unique among the handles the
    // registry's rows already answer to; an unreadable or missing registry
    // mints against an empty set, never an error. Answers a value, not a
    // path, so it rides the same pre-table dispatch as `migrate`.
    if name == "mint-id" {
        // The mint runs while a Python updater can already hold the registry
        // flock: register's load-modify-write mints inside its updater, so a
        // blocking read here would deadlock the mint until the caller's own
        // timeout. A registry locked by a writer reads as empty, same as an
        // unreadable one - the draw is advisory, the write-time fill re-checks.
        let taken =
            crate::state::try_load_registry(&crate::paths::AgentsHome::from_env().registry_json())
                .map(|maybe| {
                    maybe
                        .map(|registry| crate::state::taken_handles(&registry.entries))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
        return match crate::identity::mint_unique_fno_id(&taken) {
            Ok(id) => {
                println!("{id}");
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // Canonicalize so the slug matches what a caller passing the canonical
    // path (e.g. loop-check's resolved --cwd) hashes: /var vs /private/var
    // must never mint two spaces.
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let Some(path) = resolve(name, &cwd) else {
        eprintln!(
            "error: unknown state path {name} (known: dir, escalations, events, inbox, leads, plan-dir, plan-path, plans, plans-dirs, questions, run-log, scratchpad, status-sinks, target-state, worktree-log)"
        );
        return 2;
    };
    println!("{}", path.display());
    0
}

/// `dir <name> [--file <f> | --children] [--root <dir>]`: the shell door to
/// the state-root dir table. Prints `place` for the folder, `place_in` for
/// one file, or `list_dir_children` one per line, so a caller never spells
/// a folder the table owns. A name the table does not carry as a `dir` row
/// exits 2: the door mints no new top-level folder.
fn run_dir(args: &[String]) -> i32 {
    match dir_paths(args) {
        Ok(paths) => {
            for p in paths {
                println!("{}", p.display());
            }
            0
        }
        Err(msg) => {
            eprintln!("{msg}");
            2
        }
    }
}

/// Parse and resolve one `dir` door call; `Err` carries the usage or
/// refusal line. Split from `run_dir` so the test reads paths, not stdout.
fn dir_paths(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut name: Option<&str> = None;
    let mut file: Option<&str> = None;
    let mut children = false;
    let mut root: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => match args.get(i + 1) {
                Some(v) => {
                    file = Some(v.as_str());
                    i += 1;
                }
                None => return Err("dir: --file needs a value".to_string()),
            },
            "--children" => children = true,
            "--root" => match args.get(i + 1) {
                Some(v) => {
                    root = Some(PathBuf::from(v));
                    i += 1;
                }
                None => return Err("dir: --root needs a value".to_string()),
            },
            v if v.starts_with('-') => return Err(format!("dir: unknown flag {v}")),
            v => {
                if name.is_some() {
                    return Err(format!("dir: one name per call, got {name:?} then {v:?}"));
                }
                name = Some(v);
            }
        }
        i += 1;
    }
    let Some(name) = name else {
        return Err(
            "usage: fno-agents state path dir <name> [--file <f> | --children] [--root <dir>]"
                .to_string(),
        );
    };
    if crate::state_layout::find(name).map(|r| r.kind) != Some(crate::state_layout::Kind::Dir) {
        return Err(format!("dir: {name} is not a state-root dir"));
    }
    let root = root.unwrap_or_else(dir_door_root);
    if children {
        return Ok(crate::state_layout::list_dir_children(&root, name));
    }
    Ok(vec![match file {
        Some(f) => crate::state_layout::place_in(&root, name, f),
        None => crate::state_layout::place(&root, name),
    }])
}

/// The hook root chain, spelled `${FNO_HOME:-$HOME/.fno}` everywhere the
/// hooks say it.
fn dir_door_root() -> PathBuf {
    if let Some(v) = std::env::var_os("FNO_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fno")
}

/// The name-to-path table, mirroring the accessor table in Python
/// `fno.paths`. `events` honors `FNO_EVENTS_PATH` and
/// migrates the legacy checkout journal so a hook append never splits the
/// file; `target-state` falls back to the pre-space checkout manifest.
/// `pub(crate)`: the route-slot command resolves its refusal-journal path here,
/// so the pin and the space fallback have one owner.
pub(crate) fn resolve(name: &str, cwd: &std::path::Path) -> Option<PathBuf> {
    if name == "events" {
        if let Some(v) = std::env::var_os("FNO_EVENTS_PATH").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(v));
        }
    }
    let space = space_dir(cwd);
    let wt = worktree_space_dir(cwd);
    match name {
        "target-state" => {
            let path = wt.join("target-state.md");
            if path.exists() {
                return Some(path);
            }
            let legacy = worktree_repo_root(cwd).join(".fno").join("target-state.md");
            Some(if legacy.exists() { legacy } else { path })
        }
        "run-log" => Some(wt.join("run-log.jsonl")),
        "events" => Some(crate::paths::events_path(cwd)),
        "plans" => Some(space.join("plans")),
        "inbox" => Some(space.join("inbox")),
        "leads" => Some(space.join("leads")),
        "scratchpad" => Some(wt.join("scratchpad")),
        "status-sinks" => Some(space.join("status-sinks")),
        "worktree-log" => Some(space.join("worktree-log.jsonl")),
        // Rust-owned: the escalation notes directory (crate::escalation).
        // Nothing in Python reads it, so there is no Python accessor to mirror.
        "escalations" => Some(crate::escalation::dir(cwd)),
        // Rust-owned like `escalations`: the attention arm writes question
        // pages here and the lead check-in reads them. The arm's published
        // folder wins, so a reader in a vault or any other cwd sees the
        // pages the daemon writes.
        "questions" => Some(
            crate::attention_arm::published_questions_dir()
                .unwrap_or_else(|| crate::escalation::questions_dir(cwd)),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::test_env_lock;
    use crate::paths::space_slug;
    use std::fs;
    use std::path::Path;

    struct EnvGuard {
        home: PathBuf,
        spaces: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn new(home: &Path) -> Self {
            let spaces = std::env::var_os("FNO_SPACES_DIR");
            std::env::set_var("FNO_AGENTS_HOME", home.join("agents-home"));
            std::env::set_var("FNO_SPACES_DIR", home.join("spaces"));
            Self {
                home: home.to_path_buf(),
                spaces,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.spaces {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            std::env::remove_var("FNO_AGENTS_HOME");
            let _ = &self.home;
        }
    }

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let status = std::process::Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["-C", dir.to_str().unwrap()])
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init failed for {}", dir.display());
        let steps: &[&[&str]] = &[
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ];
        for args in steps {
            let status = std::process::Command::new("git")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .args(["-C", dir.to_str().unwrap()])
                .args(*args)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "git {args:?} failed for {}",
                dir.display()
            );
        }
    }

    #[test]
    fn state_path_answers_space_paths_from_a_worktree() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("fno-state-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        init_repo(&repo);
        let wt = base.join("wt");
        let status = std::process::Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "worktree",
                "add",
                "-b",
                "wt",
                wt.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success(), "worktree add failed");

        let _env = EnvGuard::new(&base);
        let expected_slug = space_slug(&fs::canonicalize(&repo).unwrap());

        let events = resolve("events", &wt).unwrap();
        assert_eq!(
            events,
            base.join("spaces")
                .join(&expected_slug)
                .join("events.jsonl")
        );
        let manifest = resolve("target-state", &wt).unwrap();
        assert_eq!(
            manifest,
            base.join("spaces")
                .join(&expected_slug)
                .join("worktrees")
                .join("wt")
                .join("target-state.md")
        );
        let scratch = resolve("scratchpad", &wt).unwrap();
        assert!(scratch.starts_with(
            base.join("spaces")
                .join(&expected_slug)
                .join("worktrees")
                .join("wt")
        ));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn state_path_migrates_legacy_events_and_keeps_target_state_fallback() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let base =
            std::env::temp_dir().join(format!("fno-state-path-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        init_repo(&repo);

        let _env = EnvGuard::new(&base);
        let legacy = repo.join(".fno");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("events.jsonl"), "{\"k\":1}\n").unwrap();
        let legacy_manifest = legacy.join("target-state.md");
        fs::write(&legacy_manifest, "_repo").unwrap();

        let events = resolve("events", &repo).unwrap();
        assert_eq!(
            events,
            base.join("spaces")
                .join(space_slug(&fs::canonicalize(&repo).unwrap()))
                .join("events.jsonl")
        );
        assert!(events.exists(), "events migrated onto the space");
        assert!(
            !legacy.join("events.jsonl").exists(),
            "legacy journal moved"
        );
        assert!(legacy.join("MOVED-TO").exists(), "pointer left behind");

        let manifest = resolve("target-state", &repo).unwrap();
        assert_eq!(
            manifest,
            fs::canonicalize(&legacy_manifest).unwrap(),
            "missing space manifest falls back to legacy"
        );
        assert!(
            legacy_manifest.exists(),
            "fallback never migrates the manifest"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn state_dir_door_answers_self_mapped_row_and_refuses_unknown_names() {
        let base = std::env::temp_dir().join(format!("fno-state-path-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        // `locks` is the fixture row: like every dir row it is self-mapped
        // until a group slice retargets it, and it is not one of the five
        // folders the session probe counts spellings of.
        fs::create_dir_all(base.join("locks")).unwrap();
        fs::write(base.join("locks").join("s.json"), b"{}").unwrap();
        let root = base.to_str().unwrap().to_string();
        let dir_args = |rest: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = vec!["--root".to_string(), root.clone()];
            v.extend(rest.iter().map(|s| s.to_string()));
            v
        };
        // A self-mapped dir row answers the legacy folder.
        assert_eq!(
            dir_paths(&dir_args(&["locks"])).unwrap(),
            vec![base.join("locks")]
        );
        // --file rides the place_in ladder: the one sidecar there is wins.
        assert_eq!(
            dir_paths(&dir_args(&["locks", "--file", "s.json"])).unwrap(),
            vec![base.join("locks").join("s.json")]
        );
        // --children lists the folder, one path per line.
        assert_eq!(
            dir_paths(&dir_args(&["locks", "--children"])).unwrap(),
            vec![base.join("locks").join("s.json")]
        );
        // A name with no dir row is refused, exit 2: the door mints no new
        // top-level folder.
        assert!(dir_paths(&dir_args(&["no-such-folder"])).is_err());
        // A name the table carries with a non-dir kind is refused too.
        assert!(dir_paths(&dir_args(&["graph.json"])).is_err());
        assert!(dir_paths(&dir_args(&[])).is_err());
        let _ = fs::remove_dir_all(&base);
    }
}

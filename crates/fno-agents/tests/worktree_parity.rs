//! parity-stage: characterization
//! parity-oracle: cli/src/fno/worktree_paths.py + cli/src/fno/worktree_stranded.py
//!   + cli/src/fno/worktree_status.py + cli/src/fno/worktree_cli/overlaps.py
//!   + cli/src/fno/runtime/registry.py + cli/src/fno/runtime/worktree.py
//!
//! The worktree family's characterization contract. The six Python legs were
//! captured in-process by
//! crates/fno-agents/tests/fixtures/worktree/capture_driver.py before their
//! deletion, with pinned git dates, a pinned fold instant, and machine paths
//! folded to <ROOT>/<HOME>/<BASE>, ISO stamps to <TS>, git ages to <AGE>;
//! this suite rebuilds the same fixture tree and drives the Rust ops the
//! same way, then asserts the frozen goldens under tests/golden/worktree/.
//! Env pins mirror the driver's base_env exactly: HOME, FNO_STATE_DIR and
//! FNO_GLOBAL_SETTINGS_PATH are set per case; FNO_AGENTS_HOME and
//! FNO_AGENTS_RUNTIME are deliberately NOT set, so the ops resolve the same
//! defaults the captured legs resolved. The overlap ops run under
//! FNO_TEST_NOW and FNO_TEST_JOURNAL, the pins their ports read.

mod common;

use common::{assert_golden, Golden};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Pinned git dates keep commit shas stable, so goldens carrying a sha
/// (the apply case) stay valid across runs and machines.
fn run_git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00+00:00")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed in {cwd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Python's `json.dumps(..., sort_keys=True, separators=(",", ":"))`, the
/// serialization the capture driver froze every payload with.
fn sorted(value: &Value) -> String {
    fn write_sorted(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*k).clone()).to_string());
                    out.push(':');
                    write_sorted(&map[*k], out);
                }
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_sorted(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write_sorted(value, &mut out);
    out
}

/// The capture driver's normalization: fold the fixture root, home, and the
/// FNO_STATE_DIR worktrees base, then ISO stamps to <TS> and git ages to
/// <AGE>.
fn normalize(text: &str, root: &Path, home: &Path, base: &Path) -> String {
    let text = text
        .replace(base.to_str().unwrap(), "<BASE>")
        .replace(root.to_str().unwrap(), "<ROOT>")
        .replace(home.to_str().unwrap(), "<HOME>");
    iso_folds(&text)
}

/// ISO-8601 stamps with a +00:00 or Z offset fold to <TS>; git's relative
/// ages ("3 days ago") fold to <AGE>. Mirror of the driver's regexes.
fn iso_folds(text: &str) -> String {
    use std::sync::OnceLock;
    static ISO: OnceLock<regex::Regex> = OnceLock::new();
    static AGE: OnceLock<regex::Regex> = OnceLock::new();
    let iso = ISO
        .get_or_init(|| {
            regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(\+00:00|Z)").unwrap()
        })
        .replace_all(text, "<TS>")
        .into_owned();
    let age = AGE
        .get_or_init(|| {
            regex::Regex::new(r"\b\d+ (?:second|minute|hour|day|week|month|year)s? ago\b").unwrap()
        })
        .replace_all(&iso, "<AGE>")
        .into_owned();
    age
}

/// The `fno-agents` binary the parity drives: every op runs the way a
/// caller runs it, stdio captured, env pinned.
fn fno_agents_bin() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

/// Layer the case pins, run `fno-agents <argv>`, restore the env.
/// Returns (exit, stdout, stderr).
fn drive(
    bin: &str,
    argv: &[String],
    pins: &[(String, String)],
    stdin_text: Option<&str>,
) -> (i32, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in pins {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("fno-agents spawns");
    if let Some(text) = stdin_text {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(text.as_bytes())
            .expect("stdin writes");
    }
    let out = child.wait_with_output().expect("fno-agents waits");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The per-case env pin set, layered over the ambient env and restored after
/// each case so pins never leak between cases. The pins mirror the capture
/// driver's base_env exactly.
struct Pins {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
    absent_global: PathBuf,
    global: PathBuf,
}

impl Pins {
    fn new(root: &Path, home: &Path) -> Pins {
        let _ = std::fs::create_dir_all(home);
        Pins {
            root: root.to_path_buf(),
            home: home.to_path_buf(),
            state: root.join("state"),
            absent_global: home.join("absent-global-settings.yaml"),
            global: home.join("global-config.toml"),
        }
    }

    fn envs(&self, extra: &[(&str, Option<String>)]) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = vec![
            ("HOME".to_string(), self.home.display().to_string()),
            (
                "FNO_STATE_DIR".to_string(),
                self.state.display().to_string(),
            ),
            (
                "FNO_GLOBAL_SETTINGS_PATH".to_string(),
                self.absent_global.display().to_string(),
            ),
        ];
        for (key, value) in extra {
            if let Some(v) = value {
                env.push((key.to_string(), v.clone()));
            }
        }
        env
    }

    fn drive(
        &self,
        argv: &[&str],
        extra: &[(&str, Option<String>)],
        stdin_text: Option<&str>,
    ) -> (i32, String, String) {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        drive(fno_agents_bin(), &argv, &self.envs(extra), stdin_text)
    }
}

/// Build the shared fixture tree, mirroring the driver's build_fixtures.
fn fixture(root: &Path, home: &Path) {
    let _ = std::fs::create_dir_all(root);
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::create_dir_all(root.join("plain"));
    init_repo(&root.join("r1"));
    init_repo(&root.join("r2"));
    run_git(
        &root.join("r2"),
        &["remote", "add", "origin", "git@example.com:org/r2repo.git"],
    );
    init_repo(&root.join("s1"));
    init_repo(&root.join("p1"));
    init_repo(&root.join("p2"));
    init_repo(&root.join("p3"));
    // The parity-only config seed: the bare-hex resolve tier reads the
    // configured node-id prefix from the cwd's config, and the capture ran
    // from the footnote repo whose prefix is x-, so the fixture root carries
    // the same prefix and every resolve case runs with cwd here.
    let _ = std::fs::create_dir_all(root.join(".fno"));
    std::fs::write(
        root.join(".fno").join("config.toml"),
        "[backlog]\nid_prefix = \"x-\"\n",
    )
    .expect("fixture config writes");
    // Ensure/sweep/apply fixtures: clones of local bare remotes, so the
    // fetch/rev-list legs run offline with shared history.
    for name in ["e1", "sw", "ap"] {
        let repo = root.join(format!("{name}1"));
        init_repo(&repo);
        let origin = root.join(format!("{name}-origin.git"));
        let _ = std::fs::create_dir_all(&origin);
        run_git(&repo, &["init", "--bare", "-q", origin.to_str().unwrap()]);
        run_git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        run_git(&repo, &["push", "-q", "-u", "origin", "main"]);
    }
    // Shared status fixture: one live row on wt1, one exited row on wt2;
    // registry cwds spell the resolved macOS prefix git porcelain emits.
    let wt1 = resolved(&root.join("s1-wts").join("wt1"));
    let wt2 = resolved(&root.join("s1-wts").join("wt2"));
    run_git(
        &root.join("s1"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/live-one",
            wt1.to_str().unwrap(),
        ],
    );
    run_git(
        &root.join("s1"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/exited-two",
            wt2.to_str().unwrap(),
        ],
    );
    registry_fixture(
        &root.join("registry.json"),
        &[
            (
                "wt-live-glm",
                "busy",
                &wt1,
                "2026-10-25T10:00:00+00:00",
                "2026-10-25T11:00:00+00:00",
                None,
            ),
            (
                "wt-exited-glm",
                "stopped",
                &wt2,
                "2026-10-25T09:00:00+00:00",
                "2026-10-25T09:30:00+00:00",
                Some("exited_at"),
            ),
        ],
    );
    // Legacy-base tree for the list case: under <repo>/.claude/worktrees.
    let legacy = resolved(
        &root
            .join("s1")
            .join(".claude")
            .join("worktrees")
            .join("legacy-one"),
    );
    run_git(
        &root.join("s1"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/legacy-one",
            legacy.to_str().unwrap(),
        ],
    );
    // A worktree under the FNO_STATE_DIR base too.
    let state_wt = resolved(&root.join("state").join("worktrees").join("state-one"));
    run_git(
        &root.join("s1"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/state-one",
            state_wt.to_str().unwrap(),
        ],
    );
    let _ = std::fs::create_dir_all(root.join("events"));
    let _ = std::fs::create_dir_all(root.join("wtbase"));
}

fn resolved(p: &Path) -> PathBuf {
    let _ = std::fs::create_dir_all(p);
    std::fs::canonicalize(p).expect("fixture dir canonicalizes")
}

fn init_repo(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "user.email", "fixture@example.com"]);
    run_git(dir, &["config", "user.name", "fixture"]);
    std::fs::write(dir.join("file.txt"), "one\n").expect("file.txt writes");
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", "base"]);
}

/// The per-case repo-config isolation the capture driver enforces: the
/// previous case's write must never leak into a later case reading the same
/// fixture repo.
fn clear_repo_config(repo: &Path) {
    for name in ["config.toml", "settings.yaml"] {
        let _ = std::fs::remove_file(repo.join(".fno").join(name));
    }
}

/// Write one case's repo config shape.
fn write_repo_config(repo: &Path, text: &str) {
    let dir = repo.join(".fno");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("config.toml"), text).expect("repo config writes");
}

#[test]
fn policy_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let pins = Pins::new(&root, &home);

    // default answers harness-native for claude / degrades for codex
    policy_case(
        &pins,
        &base,
        "policy_default answers harness-native for claude",
        "r1",
        Some("claude"),
        &[],
        None,
    );
    policy_case(
        &pins,
        &base,
        "policy_default degrades to external for codex",
        "r1",
        Some("codex"),
        &[],
        None,
    );
    // global never wins over the default
    std::fs::write(&pins.global, "[worktree]\npolicy = \"never\"\n").expect("global writes");
    policy_case(
        &pins,
        &base,
        "policy_global never wins over the default",
        "r1",
        Some("claude"),
        &[(
            "FNO_GLOBAL_SETTINGS_PATH",
            Some(pins.global.display().to_string()),
        )],
        None,
    );
    let _ = std::fs::remove_file(&pins.global);
    // per-project path match answers external
    let path_cfg = format!(
        "[[work.workspaces.w1.projects]]\npath = \"{}/r1\"\nworktree = \"external\"\n",
        root.display()
    );
    policy_case(
        &pins,
        &base,
        "policy_per-project path match answers external",
        "r1",
        Some("claude"),
        &[],
        Some(("r1", &path_cfg)),
    );
    // per-project name match answers never (r2's git-derived id is r2repo)
    policy_case(
        &pins,
        &base,
        "policy_per-project name match answers never",
        "r2",
        Some("claude"),
        &[],
        Some((
            "r2",
            "[[work.workspaces.w1.projects]]\nname = \"r2repo\"\nworktree = \"never\"\n",
        )),
    );
    // invalid global policy refuses
    std::fs::write(&pins.global, "[worktree]\npolicy = \"bogus\"\n").expect("global writes");
    policy_case(
        &pins,
        &base,
        "policy_invalid global policy refuses",
        "r1",
        Some("claude"),
        &[(
            "FNO_GLOBAL_SETTINGS_PATH",
            Some(pins.global.display().to_string()),
        )],
        None,
    );
    let _ = std::fs::remove_file(&pins.global);
    // unparseable repo config refuses
    policy_case(
        &pins,
        &base,
        "policy_unparseable repo config refuses",
        "r1",
        Some("claude"),
        &[],
        Some(("r1", "work = [broken\n")),
    );
    // work wrong type refuses
    policy_case(
        &pins,
        &base,
        "policy_work wrong type refuses",
        "r1",
        Some("claude"),
        &[],
        Some(("r1", "work = \"flat\"\n")),
    );
    // env override wins and is validated
    policy_case(
        &pins,
        &base,
        "policy_env override wins and is validated",
        "r1",
        Some("claude"),
        &[("FNO_WORKTREE_POLICY", Some("external".to_string()))],
        None,
    );
    // invalid env override refuses
    policy_case(
        &pins,
        &base,
        "policy_invalid env override refuses",
        "r1",
        Some("claude"),
        &[("FNO_WORKTREE_POLICY", Some("wat".to_string()))],
        None,
    );
    // explicit worktrees base degrades harness-native; the base value is
    // the literal __ROOT__/wtbase token the config carried, expanded.
    let wtbase = root.join("wtbase");
    let _ = std::fs::create_dir_all(&wtbase);
    let wtbase_str = wtbase.display().to_string();
    std::fs::write(
        &pins.global,
        format!("[paths]\nworktrees_base = \"{wtbase_str}\"\n"),
    )
    .expect("global writes");
    policy_case(
        &pins,
        &base,
        "policy_explicit worktrees base degrades harness-native",
        "r1",
        Some("claude"),
        &[(
            "FNO_GLOBAL_SETTINGS_PATH",
            Some(pins.global.display().to_string()),
        )],
        None,
    );
    let _ = std::fs::remove_file(&pins.global);
    // conductor canonical degrades with a note
    std::fs::write(&pins.global, "[worktree]\nuse_conductor_canonical = true\n")
        .expect("global writes");
    policy_case(
        &pins,
        &base,
        "policy_conductor canonical degrades with a note",
        "r1",
        Some("claude"),
        &[(
            "FNO_GLOBAL_SETTINGS_PATH",
            Some(pins.global.display().to_string()),
        )],
        None,
    );
    let _ = std::fs::remove_file(&pins.global);
    // bare string project id resolves (r2, project = "myproject")
    policy_case(
        &pins,
        &base,
        "policy_bare string project id resolves",
        "r2",
        Some("claude"),
        &[],
        Some(("r2", "project = \"myproject\"\n")),
    );
    // project table id resolves (r2, [project] id = "tableid")
    policy_case(
        &pins,
        &base,
        "policy_project table id resolves",
        "r2",
        Some("claude"),
        &[],
        Some(("r2", "[project]\nid = \"tableid\"\n")),
    );
    // state dir fallback anchors the base (codex, no explicit base)
    policy_case(
        &pins,
        &base,
        "policy_state dir fallback anchors the base",
        "r1",
        Some("codex"),
        &[],
        None,
    );
}

#[test]
fn ensure_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let pins = Pins::new(&root, &home);
    let e1 = root.join("e1");
    let name = "zz-fixture-ensure-fresh";

    // fresh cut from origin main
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            e1.to_str().unwrap(),
            "--name",
            name,
            "--harness",
            "claude",
        ],
        &[],
        None,
    );
    check2(
        "ensure_fresh cut from origin main",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // reuse of an existing tree (the tree the fresh cut left behind)
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            e1.to_str().unwrap(),
            "--name",
            name,
            "--harness",
            "claude",
        ],
        &[],
        None,
    );
    check2(
        "ensure_reuse of an existing tree",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // branch switch on a reused tree: the demanded branch does not exist,
    // so the checkout refusal is the contract.
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            e1.to_str().unwrap(),
            "--name",
            name,
            "--branch",
            "feature/switched",
            "--harness",
            "claude",
        ],
        &[],
        None,
    );
    check2(
        "ensure_branch switch on a reused tree",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // never policy launches in place
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            e1.to_str().unwrap(),
            "--name",
            "zz-fixture-ensure-never",
            "--harness",
            "claude",
        ],
        &[("FNO_WORKTREE_POLICY", Some("never".to_string()))],
        None,
    );
    check2(
        "ensure_never policy launches in place",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // never policy conflicts with an explicit branch
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            e1.to_str().unwrap(),
            "--name",
            "zz-fixture-ensure-never2",
            "--branch",
            "feature/blocked",
            "--harness",
            "claude",
        ],
        &[("FNO_WORKTREE_POLICY", Some("never".to_string()))],
        None,
    );
    check2(
        "ensure_never policy conflicts with an explicit branch",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // invalid policy refuses with empty stdout
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            e1.to_str().unwrap(),
            "--name",
            "zz-fixture-ensure-bad",
            "--harness",
            "claude",
        ],
        &[("FNO_WORKTREE_POLICY", Some("wat".to_string()))],
        None,
    );
    check2(
        "ensure_invalid policy refuses with empty stdout",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // linked worktree refuses to nest (the tree the fresh cut created)
    let linked = e1.join(".claude").join("worktrees").join(name);
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            linked.to_str().unwrap(),
            "--name",
            "zz-fixture-ensure-nested",
            "--harness",
            "claude",
        ],
        &[],
        None,
    );
    check2(
        "ensure_linked worktree refuses to nest",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
    // non-git repo refuses
    let plain = root.join("plain");
    let (code, out, err) = pins.drive(
        &[
            "worktree-ensure",
            "--repo",
            plain.to_str().unwrap(),
            "--name",
            "zz-fixture-ensure-plain",
            "--harness",
            "claude",
        ],
        &[],
        None,
    );
    check2(
        "ensure_non-git repo refuses",
        code,
        out,
        err,
        &root,
        &home,
        &base,
    );
}

#[test]
fn status_and_list_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let pins = Pins::new(&root, &home);
    let s1 = root.join("s1");
    let registry = root.join("registry.json");

    // text form joins the registry
    let (code, out, err) = pins.drive(
        &["worktree-status", "--repo", s1.to_str().unwrap()],
        &[(
            "WORKTREE_STATUS_REGISTRY",
            Some(registry.display().to_string()),
        )],
        None,
    );
    let _ = err;
    check(
        "status_text form joins the registry",
        code,
        out,
        &root,
        &home,
        &base,
    );
    // json form carries the summary
    let (code, out, err) = pins.drive(
        &["worktree-status", "--repo", s1.to_str().unwrap(), "--json"],
        &[(
            "WORKTREE_STATUS_REGISTRY",
            Some(registry.display().to_string()),
        )],
        None,
    );
    let _ = err;
    check(
        "status_json form carries the summary",
        code,
        out,
        &root,
        &home,
        &base,
    );
    // missing registry degrades with a warning (no pin, empty fleet)
    let (code, out, err) = pins.drive(
        &["worktree-status", "--repo", s1.to_str().unwrap()],
        &[],
        None,
    );
    let _ = err;
    check(
        "status_missing registry degrades with a warning",
        code,
        out,
        &root,
        &home,
        &base,
    );
    // fno base and legacy base rows only
    let (code, out, err) = pins.drive(
        &["worktree-list", "--repo-root", s1.to_str().unwrap()],
        &[],
        None,
    );
    let _ = err;
    check(
        "list_fno base and legacy base rows only",
        code,
        out,
        &root,
        &home,
        &base,
    );
}

/// The in-process classify payload the driver froze: {class, node, unpushed,
/// age, facts}.
fn classify_payload(row: &fno_agents::worktree::stranded::Row) -> Value {
    json!({
        "class": row.klass,
        "node": row.node,
        "unpushed": row.unpushed,
        "age": row.age,
        "facts": Value::Object(row.facts.clone()),
    })
}

/// The resolve cases run with cwd inside the fixture root: the bare-hex tier
/// reads the configured node-id prefix from the cwd's config.
fn in_caseroot<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    let prev = std::env::current_dir().expect("cwd reads");
    std::env::set_current_dir(root).expect("chdir caseroot");
    let out = f();
    std::env::set_current_dir(prev).expect("chdir back");
    out
}

/// The resolve cases' fixtures: a dir named for the node id, and plain dirs
/// (one carrying a manifest line), like the capture driver built them.
fn resolve_fixtures(root: &Path) {
    let _ = std::fs::create_dir_all(root.join("x-d000"));
    let _ = std::fs::create_dir_all(root.join("plain-dir"));
    let _ = std::fs::create_dir_all(root.join("plain-dir2").join(".fno"));
    std::fs::write(
        root.join("plain-dir2").join(".fno").join("target-state.md"),
        "---\nsession: x\n---\ngraph_node_id: x-d003\n",
    )
    .expect("manifest fixture writes");
    let _ = std::fs::create_dir_all(root.join("plain-dir3"));
}

/// One in-process classify case, payload built like the driver froze it.
fn classify_case(
    root: &Path,
    home: &Path,
    base: &Path,
    label: &str,
    unpushed: i64,
    unpushed_ok: bool,
    node: Option<&str>,
    node_entry: Option<Value>,
    registry_status: Option<&str>,
    has_remote: Option<bool>,
) {
    let row = fno_agents::worktree::stranded::classify(
        "/r/wt",
        Some("feature/x-a"),
        unpushed,
        unpushed_ok,
        node,
        node_entry.as_ref(),
        true,
        registry_status,
        true,
        "unknown",
        has_remote,
    );
    let payload = classify_payload(&row);
    check(
        label,
        0,
        format!("{}\n", sorted(&payload)),
        root,
        home,
        base,
    );
}

#[test]
fn stranded_and_resolve_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    resolve_fixtures(&root);
    let base = resolved(&root.join("wtbase"));
    let entry = |extra: Value| {
        let mut m = Map::new();
        m.insert("id".to_string(), json!("x-a000"));
        m.insert("status".to_string(), json!("in_progress"));
        if let Value::Object(e) = extra {
            for (k, v) in e {
                m.insert(k, v);
            }
        }
        Value::Object(m)
    };
    // zero unpushed reads clean
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_zero unpushed reads clean",
        0,
        true,
        Some("x-a000"),
        Some(entry(json!({}))),
        None,
        None,
    );
    // a failed read answers unknown first
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_a failed read answers unknown first",
        1,
        false,
        Some("x-a000"),
        Some(entry(json!({}))),
        None,
        None,
    );
    // unresolved node answers unknown
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_unresolved node answers unknown",
        1,
        true,
        None,
        None,
        None,
        None,
    );
    // a live worker outranks node status
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_a live worker outranks node status",
        1,
        true,
        Some("x-a000"),
        Some(entry(json!({"status": "done"}))),
        Some("busy"),
        None,
    );
    // done node reads shipped
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_done node reads shipped",
        1,
        true,
        Some("x-a000"),
        Some(entry(json!({"status": "done"}))),
        None,
        None,
    );
    // quiet terminal reads abandoned
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_quiet terminal reads abandoned",
        1,
        true,
        Some("x-a000"),
        Some(entry(json!({"status": "deferred"}))),
        None,
        None,
    );
    // open pr keeps work visible
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_open pr keeps work visible",
        1,
        true,
        Some("x-a000"),
        Some(entry(json!({"pr_number": 12, "status": "in_review"}))),
        None,
        None,
    );
    // in-progress with no pr reads stranded
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_in-progress with no pr reads stranded",
        1,
        true,
        Some("x-a000"),
        Some(entry(json!({}))),
        None,
        None,
    );
    // facts carry pr number and live flag
    classify_case(
        &root,
        &home,
        &base,
        "stranded_classify_facts carry pr number and live flag",
        2,
        true,
        Some("x-a000"),
        Some(entry(json!({"pr_number": 12, "status": "in_review"}))),
        Some("idle"),
        Some(true),
    );
    // resolve_node: the five tiers
    let entries = |id: &str, slug: &str| json!([{"id": id, "slug": slug, "title": "t"}]);
    let resolve_case = |label: &str, wt: &str, branch: Option<&str>, rows: &Value| {
        let wt_path = root.join(wt);
        let (node, entry_hit) = in_caseroot(&root, || {
            fno_agents::worktree::stranded::resolve_node_id(
                wt_path.to_str().unwrap(),
                branch,
                rows.as_array().unwrap(),
            )
        })
        .unwrap_or_default();
        let payload = json!({
            "node": if node.is_empty() { Value::Null } else { json!(node) },
            "entry_id": entry_hit.get("id").cloned().unwrap_or(Value::Null),
        });
        check(
            label,
            0,
            format!("{}\n", sorted(&payload)),
            &root,
            &home,
            &base,
        );
    };
    resolve_case(
        "resolve_node_exact id resolves from the basename",
        "x-d000",
        None,
        &entries("x-d000", "d-slug"),
    );
    resolve_case(
        "resolve_node_slug resolves case-insensitively",
        "plain-dir",
        Some("feature/d-slug"),
        &entries("x-d001", "d-slug"),
    );
    resolve_case(
        "resolve_node_bare hex re-prefixes",
        "plain-dir",
        Some("feature/d002"),
        &entries("x-d002", "d-two"),
    );
    resolve_case(
        "resolve_node_manifest line wins over nothing",
        "plain-dir2",
        None,
        &entries("x-d003", "d-three"),
    );
    resolve_case(
        "resolve_node_no hit answers none",
        "plain-dir3",
        Some("feature/nothing"),
        &entries("x-d004", "d-four"),
    );
}

#[test]
fn sweep_and_apply_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let sw1 = root.join("sw1");
    // The graph the sweep reads, seeded at the layout anchor the strict
    // read resolves (a migrated install keeps the store under db/).
    let graph_rows = vec![json!({
        "id": "x-b00",
        "slug": "sweep-node-xb00",
        "status": "in_progress",
        "title": "sweep node",
    })];
    let _ = fno_agents::graph_store::seed_rows(
        &fno_agents::state_layout::graph_anchor(&root.join("state")),
        &graph_rows,
    );
    // The second worktree ahead of origin: the stranded shape.
    let sweep_wt = resolved(&root.join("sweep-wt"));
    run_git(
        &sw1,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/sweep-node-xb00",
            sweep_wt.to_str().unwrap(),
        ],
    );
    std::fs::write(sweep_wt.join("file.txt"), "two\n").expect("sweep wt writes");
    run_git(&sweep_wt, &["add", "-A"]);
    run_git(&sweep_wt, &["commit", "-q", "-m", "ahead"]);
    let rows = fno_agents::worktree::stranded::sweep(&sw1);
    let payload: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut facts = r.facts.clone();
            facts.remove("path");
            let path = r
                .facts
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let token = if resolved(Path::new(path)).starts_with(&sweep_wt) {
                "wt"
            } else {
                "main"
            };
            json!({
                "class": r.klass,
                "node": r.node,
                "unpushed": r.unpushed,
                "age": r.age,
                "facts": Value::Object(facts),
                "path_token": token,
            })
        })
        .collect();
    check(
        "sweep_sweep classifies ahead and merged rows",
        0,
        format!("{}\n", sorted(&Value::Array(payload))),
        &root,
        &home,
        &base,
    );
}

#[test]
fn apply_case() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let ap1 = root.join("ap1");
    // The apply worktree ahead of origin: the stranded shape the action arm
    // recovers by pushing.
    let apply_wt = resolved(&root.join("apply-wt"));
    run_git(
        &ap1,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/apply-node-xc00",
            apply_wt.to_str().unwrap(),
        ],
    );
    let row = fno_agents::worktree::stranded::classify(
        apply_wt.to_str().unwrap(),
        Some("feature/apply-node-xc00"),
        1,
        true,
        Some("x-c000"),
        Some(&json!({"id": "x-c000", "status": "in_progress"})),
        true,
        None,
        true,
        "unknown",
        None,
    );
    // The sweep event lands in the state-root journal: pin the process env
    // for the in-process action, restore after.
    let prev_state = std::env::var("FNO_STATE_DIR").ok();
    std::env::set_var("FNO_STATE_DIR", root.join("state"));
    let outcome = fno_agents::worktree::stranded::act_on_stranded(&row);
    let journal = root.join("state").join("events.jsonl");
    let journal_text = std::fs::read_to_string(&journal).unwrap_or_default();
    if let Some(prev) = prev_state {
        std::env::set_var("FNO_STATE_DIR", prev);
    } else {
        std::env::remove_var("FNO_STATE_DIR");
    }
    assert!(
        journal_text.contains("stranded_sweep") && journal_text.contains("x-c000"),
        "the apply journal row must record the sweep with its node"
    );
    let pushed = run_git(
        &root.join("ap-origin.git"),
        &["rev-parse", "--verify", "feature/apply-node-xc00"],
    );
    let payload = json!({
        "outcome": outcome,
        "pushed_sha": pushed.trim().chars().take(7).collect::<String>(),
    });
    check(
        "apply_stranded act pushes and stops clean",
        0,
        format!("{}\n", sorted(&payload)),
        &root,
        &home,
        &base,
    );
}

/// The overlap line the record/report pre-seeds: source hook and the true
/// digest, the shape the validator admits.
fn overlap_line() -> String {
    use sha2::{Digest, Sha256};
    let blob = r#"["/fix/repo","/fix/repo/wt","sess-a",["sess-b"]]"#;
    let mut hasher = Sha256::new();
    Digest::update(&mut hasher, blob.as_bytes());
    let oid = format!("{:x}", Sha256::finalize(hasher));
    sorted(&json!({
        "type": "worktree_overlap_observed",
        "ts": "2026-10-20T12:00:00+00:00",
        "source": "hook",
        "data": {
            "observer_session_id": "sess-a",
            "peer_session_ids": ["sess-b"],
            "repository_key": "/fix/repo",
            "worktree_key": "/fix/repo/wt",
            "observation_id": oid,
            "live_window_seconds": 120,
        },
    }))
}

/// The registry JSON the status cases join; driver-shaped rows.
fn registry_fixture(path: &Path, rows: &[(&str, &str, &Path, &str, &str, Option<&str>)]) {
    let mut agents: Vec<Value> = Vec::new();
    for (name, status, cwd, created, stamp, stamp_key) in rows {
        let mut row = Map::new();
        row.insert("name".to_string(), json!(name));
        row.insert("short_id".to_string(), json!(format!("{name}-id")));
        row.insert("harness".to_string(), json!("claude"));
        row.insert("status".to_string(), json!(status));
        row.insert("cwd".to_string(), json!(cwd.display().to_string()));
        row.insert("created_at".to_string(), json!(created));
        if let Some(key) = stamp_key {
            row.insert(key.to_string(), json!(stamp));
        }
        agents.push(Value::Object(row));
    }
    std::fs::write(
        path,
        json!({
            "schema_version": 26,
            "min_writer_version": 26,
            "agents": agents,
        })
        .to_string(),
    )
    .expect("registry fixture writes");
}

/// A golden compare over the driver's normalization.
fn check2(label: &str, code: i32, out: String, err: String, root: &Path, home: &Path, base: &Path) {
    let golden = Golden {
        exit: Some(code),
        streams: vec![
            normalize(&out, root, home, base),
            normalize(&err, root, home, base),
        ],
    };
    assert_golden("worktree", label, &golden, None);
}

/// The out-only form for cases whose capture wrote no .err golden.
fn check(label: &str, code: i32, out: String, root: &Path, home: &Path, base: &Path) {
    let golden = Golden {
        exit: Some(code),
        streams: vec![normalize(&out, root, home, base)],
    };
    assert_golden("worktree", label, &golden, None);
}

/// The `worktree-policy` op driven like the capture ran it: per-case repo
/// config isolation, then the resolver through the binary.
fn policy_case(
    pins: &Pins,
    base: &Path,
    label: &str,
    repo: &str,
    harness: Option<&str>,
    extra: &[(&str, Option<String>)],
    config: Option<(&str, &str)>,
) {
    let repo_path = pins.root.join(repo);
    clear_repo_config(&repo_path);
    if let Some((repo_name, text)) = config {
        write_repo_config(&pins.root.join(repo_name), text);
    }
    let (code, out, err) = match harness {
        Some(h) => pins.drive(
            &[
                "worktree-policy",
                "--repo",
                repo_path.to_str().unwrap(),
                "--json",
                "--harness",
                h,
            ],
            extra,
            None,
        ),
        None => pins.drive(
            &[
                "worktree-policy",
                "--repo",
                repo_path.to_str().unwrap(),
                "--json",
            ],
            extra,
            None,
        ),
    };
    if code == 0 {
        check(label, code, out, &pins.root, &pins.home, base);
    } else {
        check2(label, code, out, err, &pins.root, &pins.home, base);
    }
}

#[test]
fn overlap_dispatch_register_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let pins = Pins::new(&root, &home);
    let now_pin = "2026-10-26T00:00:00+00:00";
    // The fold cases: pure, in-process, pinned now.
    let fold_events = || {
        let line = overlap_line();
        vec![
            serde_json::from_str::<Value>(line.trim()).expect("fold line parses"),
            serde_json::from_str::<Value>(line.trim()).expect("fold line parses"),
        ]
    };
    let events = fold_events();
    let pinned_now = chrono::DateTime::parse_from_rfc3339(now_pin).expect("now pin parses");
    let pinned_now = pinned_now.with_timezone(&chrono::Utc);
    let folded = fno_agents::worktree::overlaps::fold_overlap_events(&events, 28, pinned_now)
        .expect("dedup fold succeeds");
    check(
        "fold_dedup folds one observation to one row",
        0,
        format!("{}\n", sorted(&folded)),
        &root,
        &home,
        &base,
    );
    let single = vec![events[0].clone()];
    let folded = fno_agents::worktree::overlaps::fold_overlap_events(&single, 1, pinned_now)
        .expect("window fold succeeds");
    check(
        "fold_out-of-window observations drop",
        0,
        format!("{}\n", sorted(&folded)),
        &root,
        &home,
        &base,
    );
    // The record op: FNO_TEST_NOW pinned, stdin observation, journal pinned
    // to the fixture events dir.
    let journal = root.join("events").join("global-events.jsonl");
    let _ = std::fs::remove_dir_all(root.join("events"));
    let _ = std::fs::create_dir_all(&journal.parent().expect("events parent"));
    std::fs::write(&journal, overlap_line()).expect("journal seeds");
    let valid_stdin = r#"{"observer_session_id": "sess-a", "peer_session_ids": ["sess-b"], "repository_common_dir": "/fix/repo", "worktree_git_dir": "/fix/repo/wt"}"#;
    let (code, out, err) = pins.drive(
        &["worktree-overlap-record", "--since", "28"],
        &[
            ("FNO_TEST_NOW", Some(now_pin.to_string())),
            ("FNO_TEST_JOURNAL", Some(journal.display().to_string())),
        ],
        Some(valid_stdin),
    );
    let _ = err;
    check(
        "overlap_record_valid payload records and folds",
        code,
        out,
        &root,
        &home,
        &base,
    );
    // invalid json / empty peers / empty observer
    for (label, stdin_text) in [
        (
            "overlap_record_invalid json answers invalid-input",
            "not json",
        ),
        (
            "overlap_record_empty peers answer invalid-input",
            r#"{"observer_session_id": "sess-a", "peer_session_ids": []}"#,
        ),
        (
            "overlap_record_empty observer answers schema-rejected",
            r#"{"observer_session_id": "", "peer_session_ids": ["sess-b"], "repository_common_dir": "/fix/repo", "worktree_git_dir": "/fix/repo/wt"}"#,
        ),
    ] {
        let _ = std::fs::remove_dir_all(root.join("events"));
        let _ = std::fs::create_dir_all(journal.parent().expect("events parent"));
        let (code, out, err) = pins.drive(
            &["worktree-overlap-record", "--since", "28"],
            &[
                ("FNO_TEST_NOW", Some(now_pin.to_string())),
                ("FNO_TEST_JOURNAL", Some(journal.display().to_string())),
            ],
            Some(stdin_text),
        );
        let _ = err;
        check(label, code, out, &root, &home, &base);
    }
    // The report op: missing / complete / malformed, JSON form; the text
    // render case runs last on a complete journal.
    let report_cases: [(&str, usize); 3] = [
        ("overlap_report_missing journal is a valid empty source", 0),
        ("overlap_report_complete journal exits zero", 1),
        ("overlap_report_malformed line makes partial exit one", 2),
    ];
    for (label, lines) in report_cases {
        let _ = std::fs::remove_dir_all(root.join("events"));
        let _ = std::fs::create_dir_all(journal.parent().expect("events parent"));
        if lines == 1 {
            std::fs::write(&journal, overlap_line()).expect("journal seeds");
        }
        if lines == 2 {
            std::fs::write(&journal, format!("{}{{broken\n", overlap_line()))
                .expect("journal seeds");
        }
        let (code, out, err) = pins.drive(
            &["worktree-overlaps", "--since", "28", "--json"],
            &[
                ("FNO_TEST_NOW", Some(now_pin.to_string())),
                ("FNO_TEST_JOURNAL", Some(journal.display().to_string())),
            ],
            None,
        );
        let _ = err;
        check(label, code, out, &root, &home, &base);
    }
    // text render names the counts (complete journal, human form)
    std::fs::write(&journal, overlap_line()).expect("journal seeds");
    let (code, out, err) = pins.drive(
        &["worktree-overlaps", "--since", "28"],
        &[
            ("FNO_TEST_NOW", Some(now_pin.to_string())),
            ("FNO_TEST_JOURNAL", Some(journal.display().to_string())),
        ],
        None,
    );
    let _ = err;
    check(
        "overlap_report_text render names the counts",
        code,
        out,
        &root,
        &home,
        &base,
    );
}

/// The dispatch-pin cases: same identity answers nothing, a foreign
/// undeclared repo pins never, a declared foreign repo answers nothing.
#[test]
fn dispatch_and_register_cases() {
    let _guard = lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, home) = (tmp.path().join("root"), tmp.path().join("home"));
    fixture(&root, &home);
    let base = resolved(&root.join("wtbase"));
    let pins = Pins::new(&root, &home);
    let pin_case = |label: &str,
                    target: &str,
                    caller: &str,
                    harness: Option<&str>,
                    config: Option<(&str, &str)>| {
        let target_path = root.join(target);
        let caller_path = root.join(caller);
        clear_repo_config(&target_path);
        if let Some((repo, text)) = config {
            write_repo_config(&root.join(repo), text);
        }
        let mut argv = vec![
            "worktree-dispatch-pin".to_string(),
            "--target".to_string(),
            target_path.display().to_string(),
            "--caller".to_string(),
            caller_path.display().to_string(),
        ];
        if let Some(h) = harness {
            argv.push("--harness".to_string());
            argv.push(h.to_string());
        }
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (code, out, err) = pins.drive(&argv, &[], None);
        let _ = err;
        check(label, code, out, &root, &home, &base);
    };
    pin_case(
        "dispatch_pin_same repo identity pins nothing",
        "p1",
        "p1",
        None,
        None,
    );
    pin_case(
        "dispatch_pin_foreign undeclared repo pins never",
        "p2",
        "p1",
        Some("claude"),
        None,
    );
    pin_case(
        "dispatch_pin_foreign declared repo pins nothing",
        "p3",
        "p1",
        Some("claude"),
        Some((
            "p3",
            "[[work.workspaces.w1.projects]]\nname = \"p3\"\nworktree = \"external\"\n",
        )),
    );
    // register worker: the entry lands in the jsonl registry
    let workers = root.join("state").join("workers.jsonl");
    let (code, out, err) = pins.drive(
        &[
            "workspace-register-worker",
            "--id",
            "t-fixture-worker",
            "--task",
            "fixture task",
            "--campaign",
            "camp-x",
            "--workers-file",
            workers.to_str().unwrap(),
        ],
        &[],
        None,
    );
    let _ = err;
    check(
        "register_worker_entry lands in the jsonl registry",
        code,
        out,
        &root,
        &home,
        &base,
    );
    let registry_text = std::fs::read_to_string(&workers).unwrap_or_default();
    assert!(
        registry_text.contains("t-fixture-worker") && registry_text.contains("camp-x"),
        "the worker row lands in the registry file"
    );
}

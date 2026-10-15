//! Shared fixture + capture harness for the target init/start parity files.
//! Mirrors `tests/capture/target_capture.py` (the capture driver that froze
//! `tests/golden/target_init/` and `tests/golden/target_start/`): the same
//! scrubbed-env contract, the same front-door shim, the same fixtures, the
//! same normalization tokens. Keep the two in lockstep when a case is added.
//!
//! The leg under test is [`Leg`]: the differential stage drives the live
//! Python CLI through the shim; the characterization stage drives the native
//! `fno-agents` door. `backlog` seeding always rides the native binary
//! directly (the backlog namespace is native-only), and the side-effect
//! listing always reads the claim state through the Python CLI, because the
//! claim JSON's byte shape is pinned by the frozen `.fx` files and the
//! Python claim verb survives the init port.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The `fno-agents` binary under test: the door leg and the backlog seeding
/// both exec it.
pub const AGENTS_BIN: &str = env!("CARGO_BIN_EXE_fno-agents");

/// The repo root (crates/fno-agents/tests -> repo).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../")
}

fn cli_src() -> PathBuf {
    repo_root().join("cli/src")
}

/// Which implementation plays the verb under test.
pub enum Leg {
    /// The live Python CLI through the front-door shim (differential stage).
    PythonCli,
    /// The native hidden door: `fno-agents <door> <args>` (characterization).
    NativeDoor(&'static str),
}

/// Every prefix the capture driver scrubbed before running the leg. The scrub
/// runs per case, so a case's pins (TARGET_SESSION_ID, CLAUDE_PLUGIN_ROOT)
/// are gone again by the next case, exactly as under the subprocess driver.
pub const SCRUB_PREFIXES: [&str; 8] = [
    "FNO_",
    "CLAUDE_",
    "CODEX_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
    "TARGET_",
];

/// A pid that no machine allocates: far above every platform's pid_max, so
/// the claim liveness read always answers dead. Only pre-run helpers (seeding,
/// prewarm) ride it; the captured run pins a live sleeper pid.
pub const DEAD_PID: &str = "2147483647";

/// The claim-owner override every captured run carries: the helper anchors
/// the node claim to this session id, so holder attribution is deterministic.
pub const PINNED_SESSION_ID: &str = "00000000-0000-0000-0000-000000000000";
/// The second session id the foreign-holder case's challenger rides.
pub const OTHER_SESSION_ID: &str = "11111111-1111-1111-1111-111111111111";

/// Which plugin root the case hands the verb.
#[derive(Clone, Copy, PartialEq)]
pub enum Plugin {
    /// A stub plugin whose helper prints the TARGET_*/FNO_TARGET_* env.
    Stub,
    /// The real plugin (the worktree root ships hooks/helpers/).
    Real,
    /// An empty CLAUDE_PLUGIN_ROOT: authoritative when set, no fallthrough,
    /// so the missing-helper refusal fires.
    None,
}

/// What the case builds before the run.
#[derive(Clone, Copy, PartialEq)]
pub enum Setup {
    /// Plain run: fixture, then the verb.
    Normal,
    /// Start once under the pinned session (discarded), then capture the
    /// second run: the bind path answers already-claimed.
    SecondRun,
    /// Start under session A (discarded), capture session B: the bind path
    /// parks on the foreign live claim.
    ForeignHolder,
    /// `git worktree add` a linked tree and run from inside it.
    LinkedWorktree,
    /// A linked worktree holding a hand-written manifest whose node is not
    /// in the graph: the bind path's ghost refusal.
    GhostWorktree,
    /// Start node A (discarded), then start node B from inside A's worktree:
    /// the bind path refuses a manifest naming another node.
    ManifestAnother,
}

pub struct Case {
    pub label: String,
    pub argv: Vec<String>,
    pub fixture: Fixture,
    pub plugin: Plugin,
    pub seed: Option<String>,
    pub env_extra: Vec<(String, String)>,
    pub setup: Setup,
    pub pre: bool,
    pub with_fx: bool,
}

pub fn tables_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/capture/tables.json")
}

/// The generated case tables: read from `tests/capture/tables.json`, the
/// dump of `target_capture.py --dump-tables` (one table, zero hand
/// transcription). Regenerate with the command in the driver's `dump_tables`
/// docstring.
pub fn load_cases(subject: &str) -> Vec<Case> {
    let text = std::fs::read_to_string(tables_path()).expect("tables.json exists");
    let value: serde_json::Value = serde_json::from_str(&text).expect("tables.json parses");
    let rows = value
        .get(subject)
        .and_then(|v| v.as_array())
        .expect("tables.json carries the subject");
    rows.iter().map(build_case).collect()
}

fn build_case(row: &serde_json::Value) -> Case {
    let label = row["label"].as_str().expect("label").to_string();
    let mut argv: Vec<String> = Vec::new();
    for item in row["argv"].as_array().expect("argv") {
        argv.push(item.as_str().expect("argv str").to_string());
    }
    let plugin = match row["plugin"].as_str().expect("plugin") {
        "real" => Plugin::Real,
        "none" => Plugin::None,
        _ => Plugin::Stub,
    };
    let setup = match row["setup"].as_str().expect("setup") {
        "normal" => Setup::Normal,
        "second-run" => Setup::SecondRun,
        "foreign-holder" => Setup::ForeignHolder,
        "linked-worktree" => Setup::LinkedWorktree,
        "ghost-worktree" => Setup::GhostWorktree,
        "manifest-another" => Setup::ManifestAnother,
        other => panic!("unknown setup {other}"),
    };
    let seed = row
        .get("seed")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut env_extra: Vec<(String, String)> = Vec::new();
    if let Some(pairs) = row["env_extra"].as_array() {
        for pair in pairs {
            let k = pair[0].as_str().expect("env key");
            let v = pair[1].as_str().expect("env value");
            env_extra.push((k.to_string(), v.to_string()));
        }
    }
    Case {
        label,
        argv,
        fixture: match row["fixture"].as_str().unwrap_or("repo") {
            "plain" => Fixture::Plain,
            _ => Fixture::Repo,
        },
        plugin,
        seed,
        env_extra,
        setup,
        pre: row["pre"].as_bool().unwrap_or(true),
        with_fx: row["with_fx"].as_bool().unwrap_or(false),
    }
}

/// One parity run: build the per-case roots, play every case, assert each
/// against its frozen golden. `table` selects the JSON table ("init" or
/// "start"); `subject` names the golden directory.
pub fn run_all(table: &str, subject: &str, leg: &Leg) {
    let cases = load_cases(table);
    let top =
        std::env::temp_dir().join(format!("target-parity-{}-{}", subject, std::process::id()));
    let _ = std::fs::remove_dir_all(&top);
    std::fs::create_dir_all(&top).unwrap();
    for case in &cases {
        let case_root = top.join(slug_of(&case.label));
        std::fs::create_dir_all(&case_root).unwrap();
        play(subject, case, &case_root, leg);
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Fixture {
    Repo,
    Plain,
}

/// A live process whose pid anchors the captured run's session identity. A
/// dead pair fails harness provenance closed and refuses the claim, so the
/// captured run pins THIS process's sleeper instead.
pub struct Sleeper(pub std::process::Child);

impl Sleeper {
    pub fn spawn() -> Sleeper {
        let child = Command::new("sleep")
            .arg("180")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep spawns");
        Sleeper(child)
    }

    pub fn pid(&self) -> String {
        self.0.id().to_string()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git spawns");
    assert!(
        status.success(),
        "git {args:?} failed in {}",
        repo.display()
    );
}

/// The temp git repo fixture: base commit on main, a local origin/main ref,
/// and a feature branch as the cwd (the helper's location gate refuses a
/// canonical checkout on its main branch).
pub fn make_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "t@example.test"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("base.txt"), "seed\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["branch", "origin/main"]);
    git(&repo, &["checkout", "-q", "-b", "feature/work"]);
    repo
}

/// The stub plugin: a helper that prints the TARGET_*/FNO_TARGET_* environment
/// (LC_ALL=C so the sort order never depends on the host locale) and exits 0.
pub fn make_stub_plugin(root: &Path) -> PathBuf {
    let stub = root.join("stub-plugin");
    let helper = stub.join("hooks").join("helpers");
    std::fs::create_dir_all(&helper).unwrap();
    let path = helper.join("init-target-state.sh");
    std::fs::write(
        &path,
        "#!/bin/bash\nenv | grep -E '^(TARGET_|FNO_TARGET_)' | LC_ALL=C sort\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    stub
}

/// The front-door shim: worktree Python for the CLI trees, native binary for
/// the backlog namespace. Both spellings (`fno`, `fno-py`) exist because
/// `start` resolves `fno-py` off PATH for its init subprocess.
pub fn make_shim(root: &Path) -> PathBuf {
    let bindir = root.join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let python = crate::common::python_executable()
        .to_string_lossy()
        .to_string();
    let src = cli_src().to_string_lossy().to_string();
    let agents = AGENTS_BIN;
    for name in ["fno", "fno-py"] {
        let body = format!(
            "#!/bin/bash\n\
             if [ \"$1\" = \"backlog\" ]; then\n\
             \x20 exec '{agents}' \"$@\"\n\
             fi\n\
             exec env PYTHONPATH='{src}' '{python}' -m fno.cli \"$@\"\n"
        );
        let path = bindir.join(name);
        std::fs::write(&path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    bindir
}

/// The scrubbed per-case environment. Inherits the test process's env minus
/// the eight scrub prefixes, then pins HOME, PATH, the git config isolation,
/// and the session-identity pair, byte for byte like the capture driver.
pub fn base_env(root: &Path, bindir: &Path, session_pid: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !SCRUB_PREFIXES.iter().any(|p| k.starts_with(p)))
        .collect();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    env.push(("HOME".into(), home.to_string_lossy().to_string()));
    env.push((
        "PATH".into(),
        format!(
            "{}:{}",
            bindir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    ));
    env.push(("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()));
    env.push(("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()));
    // The Python leg's init forward (and any resolve_binary consumer) must
    // resolve THIS test's binary, never whatever a host has installed.
    env.push(("FNO_AGENTS_BIN".into(), AGENTS_BIN.to_string()));
    env.push(("FNO_SESSION_HARNESS".into(), "claude".into()));
    env.push(("FNO_SESSION_PID".into(), session_pid.into()));
    env.push(("TERM".into(), "dumb".into()));
    env
}

/// Whether the Python leg is runnable here: the venv interpreter must import
/// the CLI's own dependency set. When it cannot, the differential stage skips
/// (never fails) - the house rule for a missing other language.
pub fn python_leg_available() -> bool {
    let probe = Command::new(crate::common::python_executable())
        .args(["-c", "import typer, click"])
        .env("PYTHONPATH", cli_src())
        .output();
    matches!(probe, Ok(out) if out.status.success())
}

pub fn spawn_cmd(cmd: &mut Command) -> std::process::Output {
    cmd.output().expect("verb under test spawns")
}

/// The seeded backlog node's id, minted by the native create.
pub fn seed_node(env: &[(String, String)], cwd: &Path, title: &str) -> String {
    // Seed through the python CLI, not AGENTS_BIN: the rust writer lives on
    // the db backend alone, while the python readers this fixture feeds
    // (`load_graph(graph_json())`) still walk the flat-file tiers. The
    // captured world wrote and read the same tier; the fixture keeps that
    // shape until the reader tiers converge.
    let out = Command::new(shim_dir(cwd).join("fno"))
        .args(["backlog", "idea", title, "--difficulty", "medium"])
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .expect("backlog idea spawns");
    assert!(
        out.status.success(),
        "seed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("seed prints a JSON receipt");
    value["id"]
        .as_str()
        .expect("seed receipt carries id")
        .to_string()
}

/// Absorb the one-time path-migration banner a fresh HOME's first Python CLI
/// dispatch prints, so the captured streams hold only scenario output.
pub fn prewarm(leg: &Leg, env: &[(String, String)], cwd: &Path) {
    let argv: Vec<String> = vec!["do".into(), "target".into(), "init".into()];
    let _ = run_verb(leg, &argv, cwd, env);
}

/// One-line-per-file inventory of every graph-shaped file under the case
/// HOME, for the exit-mismatch diagnostic: a refusal that names an unreadable
/// graph is a guess until the probe says which file exists and how big it is.
fn graph_state_probe(root: &Path) -> String {
    let mut lines = Vec::new();
    let home = root.join("home").join(".fno");
    let mut stack = vec![home.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let graphish =
                name.contains("graph") || name.contains("backlog") || name.ends_with(".db");
            if graphish {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                lines.push(format!("{} ({} bytes)", path.display(), size));
            }
        }
    }
    if lines.is_empty() {
        format!("no graph-shaped file under {}", home.display())
    } else {
        lines.join("\n")
    }
}

/// Play the verb under test. The Python leg runs through the shim (so nested
/// `fno` calls resolve to the same source); the native door execs the sibling
/// binary directly.
pub fn run_verb(
    leg: &Leg,
    argv: &[String],
    cwd: &Path,
    env: &[(String, String)],
) -> std::process::Output {
    match leg {
        Leg::PythonCli => {
            let bindir = shim_dir(cwd);
            let mut cmd = Command::new(bindir.join("fno"));
            cmd.args(argv).current_dir(cwd);
            cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            spawn_cmd(&mut cmd)
        }
        Leg::NativeDoor(door) => {
            // The case tables carry the python-CLI argv (`do target <word>
            // ...`); the door takes the rest after its own name. An argv that
            // does not open with the prefix passes through untouched.
            let word = door.strip_prefix("target-").unwrap_or(door);
            let rest: &[String] =
                if argv.len() >= 3 && argv[0] == "do" && argv[1] == "target" && argv[2] == word {
                    &argv[3..]
                } else {
                    argv
                };
            let mut cmd = Command::new(AGENTS_BIN);
            cmd.arg(door).args(rest).current_dir(cwd);
            cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            spawn_cmd(&mut cmd)
        }
    }
}

/// The shim's location for a case: the bindir under the CASE root, found by
/// walking up from cwd until `bin/fno` exists (the cwd may be a worktree or
/// a linked tree under the case root).
fn shim_dir(cwd: &Path) -> PathBuf {
    let mut probe = cwd.to_path_buf();
    loop {
        let candidate = probe.join("bin").join("fno");
        if candidate.is_file() {
            return probe.join("bin");
        }
        if !probe.pop() {
            panic!("shim not found above {}", cwd.display());
        }
    }
}

pub fn slug_of(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut prev_us = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}

pub fn golden_dir(subject: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(subject)
}

/// Every spelling the case root (and its dashed space slug) can appear as:
/// macOS answers /var and /private/var for the same directory, and the
/// space-slice manifest path embeds the repo path with slashes as dashes.
fn path_tokens(root: &Path) -> Vec<(String, &'static str)> {
    let real = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let repo = root.join("repo");
    let real_repo = real.join("repo");
    let s = |p: &Path| p.to_string_lossy().to_string();
    vec![
        (s(&real_repo), "<TMP_REPO>"),
        (s(&repo), "<TMP_REPO>"),
        (s(&real), "<TMP>"),
        (s(root), "<TMP>"),
        (s(&real_repo).replace('/', "-"), "<TMPD_REPO>"),
        (s(&repo).replace('/', "-"), "<TMPD_REPO>"),
        (s(&real).replace('/', "-"), "<TMPD>"),
        (s(root).replace('/', "-"), "<TMPD>"),
    ]
}

fn hostname() -> String {
    // No std hostname; read via the same `uname -n` the shell world uses.
    let out = Command::new("uname").arg("-n").output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// The shared normalization contract with the capture driver: named tokens
/// for temp paths, minted node ids, timestamps, dates, git SHAs, pids, the
/// host name, the OS machine identity, and epoch-milli stamps. The pinned
/// session UUID is NOT normalized: it is deterministic (all zeros), the
/// goldens hold it literally, and a rewriter here would break them. A value
/// that differs between two machines must not reach the comparison.
pub fn normalize(text: &str, root: &Path, node: &str) -> String {
    let mut out = text.to_string();
    for (spelling, token) in path_tokens(root) {
        out = out.replace(&spelling, token);
    }
    if !node.is_empty() {
        for (i, n) in node.split('+').enumerate() {
            let token = if i == 0 { "<NODE>" } else { "<NODE_OTHER>" };
            out = out.replace(n, token);
        }
    }
    let iso_ts = RegexHolder::get().iso_ts.replace_all(&out, "<TS>");
    out = iso_ts.to_string();
    let ts = RegexHolder::get().ts.replace_all(&out, "<TS>");
    out = ts.to_string();
    let date = RegexHolder::get().date.replace_all(&out, "<DATE>");
    out = date.to_string();
    let sha = RegexHolder::get().sha.replace_all(&out, "<SHA>");
    out = sha.to_string();
    let machine = RegexHolder::get()
        .machine_id
        .replace_all(&out, "\"machine_id\": \"<MACHINE>\"")
        .to_string();
    out = machine;
    out = RegexHolder::get()
        .owner_pid
        .replace_all(&out, "owner_pid: <PID>")
        .to_string();
    out = RegexHolder::get()
        .env_pid
        .replace_all(&out, "FNO_SESSION_PID=<PID>")
        .to_string();
    out = RegexHolder::get()
        .json_pid
        .replace_all(&out, "\"pid\": <PID>")
        .to_string();
    out = RegexHolder::get()
        .json_pid_tight
        .replace_all(&out, "\"pid\":<PID>")
        .to_string();
    out = RegexHolder::get()
        .plain_pid
        .replace_all(&out, "pid=<PID>")
        .to_string();
    let host = hostname();
    if !host.is_empty() {
        out = out.replace(&host, "<HOST>");
        if !host.contains('.') {
            out = out.replace(&format!("{host}.local"), "<HOST>");
        }
    }
    out = RegexHolder::get()
        .epoch_keys
        .replace_all(&out, "\"$1\": <TS_MS>")
        .to_string();
    out
}

struct RegexHolder {
    iso_ts: regex::Regex,
    ts: regex::Regex,
    date: regex::Regex,
    sha: regex::Regex,
    machine_id: regex::Regex,
    owner_pid: regex::Regex,
    env_pid: regex::Regex,
    json_pid: regex::Regex,
    json_pid_tight: regex::Regex,
    plain_pid: regex::Regex,
    epoch_keys: regex::Regex,
}

impl RegexHolder {
    fn get() -> &'static RegexHolder {
        static HOLDER: OnceLock<RegexHolder> = OnceLock::new();
        HOLDER.get_or_init(|| RegexHolder {
            iso_ts: regex::Regex::new(
                r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?",
            )
            .unwrap(),
            ts: regex::Regex::new(r"\d{8}T\d{10,}").unwrap(),
            date: regex::Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap(),
            sha: regex::Regex::new(r"\b[0-9a-f]{40}\b").unwrap(),
            machine_id: regex::Regex::new(r#""machine_id": "[^"]*""#).unwrap(),
            owner_pid: regex::Regex::new(r"owner_pid: \d+").unwrap(),
            env_pid: regex::Regex::new(r"FNO_SESSION_PID=\d+").unwrap(),
            json_pid: regex::Regex::new(r#""pid": \d+"#).unwrap(),
            json_pid_tight: regex::Regex::new(r#""pid":\d+"#).unwrap(),
            plain_pid: regex::Regex::new(r"pid=\d+").unwrap(),
            epoch_keys: regex::Regex::new(
                r#""(acquired_at|expires_at|renewed_at|created_at|updated_at)": \d+"#,
            )
            .unwrap(),
        })
    }
}

/// Every manifest the case produced: the space slices under the temp HOME,
/// plus any checkout-local copies under the worktree bases.
fn manifest_paths(root: &Path) -> Vec<PathBuf> {
    let home = root.join("home");
    let mut found: Vec<PathBuf> = Vec::new();
    for base in [home.join(".fno/spaces"), home.join(".fno")] {
        if let Ok(entries) = walk_matches(&base, "target-state.md") {
            found.extend(entries);
        }
    }
    for base in [
        root.join("repo/.claude/worktrees"),
        home.join(".fno/worktrees"),
    ] {
        if let Ok(wts) = std::fs::read_dir(&base) {
            for wt in wts.flatten() {
                let level2 = if wt.path().is_dir() {
                    match std::fs::read_dir(wt.path()) {
                        Ok(rows) => rows.flatten().map(|e| e.path()).collect::<Vec<_>>(),
                        Err(_) => vec![],
                    }
                } else {
                    vec![]
                };
                for leaf in level2 {
                    let candidate = leaf.join(".fno/target-state.md");
                    if candidate.is_file() {
                        found.push(candidate);
                    }
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Recursive search for files named `name` under `base` (bounded depth: the
/// space tree is shallow, and an unbounded walk could descend into scratchpad
/// noise).
fn walk_matches(base: &Path, name: &str) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    let mut depth = 0u32;
    while let Some(dir) = stack.pop() {
        if depth > 6 {
            break;
        }
        if let Ok(rows) = std::fs::read_dir(&dir) {
            for row in rows.flatten() {
                let path = row.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().map(|f| f == name).unwrap_or(false) {
                    found.push(path);
                }
            }
        }
        depth += 1;
    }
    Ok(found)
}

/// The side-effect listing: every manifest's normalized fields plus the live
/// claim state for the node, read through the Python CLI so the JSON byte
/// shape matches the frozen `.fx` files in both stages.
pub fn side_effects(root: &Path, env: &[(String, String)], cwd: &Path, node: &str) -> String {
    let mut lines: Vec<String> = vec!["manifests:".into()];
    for path in manifest_paths(root) {
        let rel = normalize(&path.to_string_lossy(), root, node);
        lines.push(format!("- {rel}"));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for line in text.lines() {
            let stripped = line.trim();
            if !stripped.is_empty() && !stripped.starts_with('#') {
                lines.push(format!("  {}", normalize(stripped, root, node)));
            }
        }
    }
    lines.push("claims:".into());
    // The claim key rides VERBATIM: a two-node case passes "<first>+<second>"
    // and the capture queried that combined key, so the .fx records ITS state.
    let key = if node.is_empty() {
        "node:none".to_string()
    } else {
        format!("node:{node}")
    };
    let bindir = shim_dir(cwd);
    let out = Command::new(bindir.join("fno"))
        .args(["agents", "claim", "status", &key, "--no-roster"])
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .expect("claim status spawns");
    let body = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if body.trim().is_empty() {
        lines.push(format!("  exit={} (no output)", exit_code(&out)));
    } else {
        lines.push(format!("  exit={}", exit_code(&out)));
        for line in body.lines() {
            lines.push(format!("  {}", normalize(line.trim(), root, node)));
        }
    }
    let mut worktrees: Vec<String> = Vec::new();
    for pattern in [
        root.join("repo/.claude/worktrees"),
        root.join("home/.fno/worktrees"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&pattern) {
            for wt in entries.flatten() {
                worktrees.push(normalize(&wt.path().to_string_lossy(), root, node));
            }
            // The external base nests one level deeper (base/<repo-slug>/<name>).
            for slug_dir in std::fs::read_dir(&pattern).into_iter().flatten().flatten() {
                if let Ok(inner) = std::fs::read_dir(slug_dir.path()) {
                    for leaf in inner.flatten() {
                        if leaf.path().is_dir() {
                            worktrees.push(normalize(&leaf.path().to_string_lossy(), root, node));
                        }
                    }
                }
            }
        }
    }
    for name in ["linked", "ghost-wt"] {
        let p = root.join(name);
        if p.exists() {
            worktrees.push(normalize(&p.to_string_lossy(), root, node));
        }
    }
    worktrees.sort();
    worktrees.dedup();
    lines.push("worktrees:".into());
    for wt in worktrees {
        lines.push(format!("- {wt}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn exit_code(out: &std::process::Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

/// One frozen case's on-disk golden, in the `assert_golden` format plus the
/// optional `.fx` side-effect listing.
pub struct Frozen {
    pub exit: Option<i32>,
    pub out: String,
    pub err: Option<String>,
    pub fx: Option<String>,
}

pub fn read_golden(subject: &str, label: &str) -> Frozen {
    let dir = golden_dir(subject);
    let key = slug_of(label);
    let exit = std::fs::read_to_string(dir.join(format!("{key}.exit")))
        .expect("golden .exit exists")
        .trim()
        .parse()
        .expect("golden exit parses");
    let out = std::fs::read_to_string(dir.join(format!("{key}.out"))).expect("golden .out exists");
    let err = std::fs::read_to_string(dir.join(format!("{key}.err"))).ok();
    let fx = std::fs::read_to_string(dir.join(format!("{key}.fx"))).ok();
    Frozen {
        exit: Some(exit),
        out,
        err,
        fx,
    }
}

/// The full case run: fixture, seeding, setup, captured run, side effects,
/// normalization, and the assertion against the frozen golden.
pub fn play(subject: &str, case: &Case, root: &Path, leg: &Leg) {
    let bindir = make_shim(root);

    let mut cwd = match case.fixture {
        Fixture::Repo => make_repo(root),
        Fixture::Plain => {
            let dir = root.join("plain");
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
    };

    let mut node = String::new();
    let mut other = String::new();
    if let Some(title) = &case.seed {
        node = seed_node(&base_env(root, &bindir, DEAD_PID), &cwd, title);
        // Disjoint words: a shared term flips idea into its dedup fold-offer,
        // which prints a notice instead of the JSON receipt.
        other = seed_node(
            &base_env(root, &bindir, DEAD_PID),
            &cwd,
            "zulu quebec foxtrot secondary filler",
        );
    }

    let plugin_root = match case.plugin {
        Plugin::Stub => make_stub_plugin(root).to_string_lossy().to_string(),
        Plugin::Real => repo_root().to_string_lossy().to_string(),
        // CLAUDE_PLUGIN_ROOT is authoritative when set (no fallthrough), so a
        // root without the helper surfaces the missing-plugin refusal.
        Plugin::None => {
            let empty = root.join("empty-plugin");
            std::fs::create_dir_all(&empty).unwrap();
            empty.to_string_lossy().to_string()
        }
    };

    let mut env_extra: Vec<(String, String)> = case.env_extra.clone();
    env_extra.push(("CLAUDE_PLUGIN_ROOT".into(), plugin_root));
    env_extra.push(("TARGET_SESSION_ID".into(), PINNED_SESSION_ID.into()));

    if case.pre {
        let env = {
            let mut e = base_env(root, &bindir, DEAD_PID);
            e.extend(env_extra.iter().cloned());
            e
        };
        prewarm(leg, &env, &cwd);
    }

    let sleeper_a = Sleeper::spawn();
    let mut session_pid = sleeper_a.pid();
    let _keep_b;

    match case.setup {
        Setup::SecondRun => {
            let mut env = base_env(root, &bindir, &session_pid);
            env.extend(env_extra.iter().cloned());
            let argv: Vec<String> =
                vec!["do".into(), "target".into(), "start".into(), node.clone()];
            let _ = run_verb(leg, &argv, &cwd, &env);
        }
        Setup::ForeignHolder => {
            // Session A cold-starts the node (worktree, manifest, live claim),
            // then session B starts the same node: the bind path parks on the
            // foreign live claim.
            let mut env = base_env(root, &bindir, &session_pid);
            env.extend(env_extra.iter().cloned());
            let argv: Vec<String> =
                vec!["do".into(), "target".into(), "start".into(), node.clone()];
            let _ = run_verb(leg, &argv, &cwd, &env);
            let sleeper_b = Sleeper::spawn();
            session_pid = sleeper_b.pid();
            _keep_b = Some(sleeper_b);
            for entry in env_extra.iter_mut() {
                if entry.0 == "TARGET_SESSION_ID" {
                    entry.1 = OTHER_SESSION_ID.into();
                }
            }
        }
        Setup::LinkedWorktree => {
            git(
                &cwd,
                &[
                    "worktree",
                    "add",
                    "-q",
                    &root.join("linked").to_string_lossy(),
                    "-b",
                    "linked",
                ],
            );
            cwd = root.join("linked");
        }
        Setup::GhostWorktree => {
            let wt = root.join("ghost-wt");
            git(
                &cwd,
                &[
                    "worktree",
                    "add",
                    "-q",
                    &wt.to_string_lossy(),
                    "-b",
                    "ghost",
                ],
            );
            let manifest_dir = wt.join(".fno");
            std::fs::create_dir_all(&manifest_dir).unwrap();
            std::fs::write(
                manifest_dir.join("target-state.md"),
                "---\nsession_id: 00000000-0000-0000-0000-000000000000\ngraph_node_id: ab-ghost000\n---\n",
            )
            .unwrap();
            cwd = wt;
        }
        Setup::ManifestAnother => {
            let mut env = base_env(root, &bindir, &session_pid);
            env.extend(env_extra.iter().cloned());
            let argv: Vec<String> =
                vec!["do".into(), "target".into(), "start".into(), node.clone()];
            let _ = run_verb(leg, &argv, &cwd, &env);
            // Start node B from inside node A's worktree.
            let mut wt = None;
            for base in [
                root.join("repo/.claude/worktrees"),
                root.join("home/.fno/worktrees"),
            ] {
                if let Ok(entries) = std::fs::read_dir(&base) {
                    for slug_dir in entries.flatten() {
                        if let Ok(leaves) = std::fs::read_dir(slug_dir.path()) {
                            for leaf in leaves.flatten() {
                                if leaf.path().is_dir() {
                                    wt = Some(leaf.path());
                                }
                            }
                        }
                    }
                }
            }
            cwd = wt.expect("cold start made a worktree");
        }
        Setup::Normal => {}
    }

    // Placeholder resolution: <ROOT> and <NODE> spell the fixture paths and
    // the seeded ids the way the driver's tables do.
    let node_key = if case.setup == Setup::ManifestAnother {
        format!("{node}+{other}")
    } else {
        node.clone()
    };
    let argv: Vec<String> = case
        .argv
        .iter()
        .map(|a| {
            a.replace("<ROOT>", &root.to_string_lossy()).replace(
                "<NODE>",
                if node.is_empty() {
                    "ab-00000000"
                } else if case.setup == Setup::ManifestAnother {
                    other.as_str()
                } else {
                    node.as_str()
                },
            )
        })
        .collect();

    let mut env = base_env(root, &bindir, &session_pid);
    env.extend(env_extra.iter().cloned());
    let out = run_verb(leg, &argv, &cwd, &env);
    let code = exit_code(&out);
    let stdout = normalize(&String::from_utf8_lossy(&out.stdout), root, &node_key);
    let stderr = normalize(&String::from_utf8_lossy(&out.stderr), root, &node_key);
    let fx = if case.with_fx {
        normalize(&side_effects(root, &env, &cwd, &node_key), root, &node_key)
    } else {
        String::new()
    };

    let frozen = read_golden(subject, &case.label);
    assert_eq!(
        frozen.exit,
        Some(code),
        "[{}] exit differs from golden\n--- run stderr ---\n{}\n--- run stdout ---\n{}\n--- graph state ---\n{}",
        case.label,
        stderr,
        stdout,
        graph_state_probe(root)
    );
    assert_eq!(
        frozen.out, stdout,
        "[{}] stdout differs from golden\n--- graph state ---\n{}\n--- golden ---\n{}\n--- run ---\n{}",
        case.label,
        graph_state_probe(root),
        frozen.out,
        stdout
    );
    let golden_err = frozen.err.unwrap_or_default();
    assert_eq!(
        golden_err, stderr,
        "[{}] stderr differs from golden:\n--- golden ---\n{}\n--- run ---\n{}",
        case.label, golden_err, stderr
    );
    if case.with_fx {
        let golden_fx = frozen.fx.expect("side-effect golden exists");
        assert_eq!(
            golden_fx, fx,
            "[{}] side effects differ from golden:\n--- golden ---\n{}\n--- run ---\n{}",
            case.label, golden_fx, fx
        );
    }
}

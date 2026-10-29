//! `fno doctor test smoke` - the one authoritative Python + shell smoke
//! entry, native. Ported from test_cmd.py's smoke half.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::test_command::{AmbientMode, SandboxGuard};
use crate::test_smoke_discovery as disc;
use crate::test_smoke_registry as reg;
use crate::test_smoke_registry::{AMBIENT_MODES, STATE_MODES};

#[derive(Debug)]
pub struct SmokeOpts {
    pub list: bool,
    pub verbose: bool,
    pub keep_going: bool,
    pub retry_failed: bool,
    pub changed: bool,
    pub only: String,
    pub skip: String,
    pub base: String,
    pub head: String,
    pub shard: String,
    pub ambient: String,
    pub state: String,
}

impl Default for SmokeOpts {
    /// The Python leg's unparsed defaults: both lanes start clean. The
    /// derived Default would leave "" in the lane fields, and validate()
    /// refuses "" as not one of the modes - the shard legs died on it.
    fn default() -> Self {
        Self {
            list: false,
            verbose: false,
            keep_going: false,
            retry_failed: false,
            changed: false,
            only: String::new(),
            skip: String::new(),
            base: String::new(),
            head: String::new(),
            shard: String::new(),
            ambient: "clean".to_string(),
            state: "clean".to_string(),
        }
    }
}

/// Parse smoke flags into options; `Err` is the caller's exit-2 line.
pub fn parse_smoke_args(args: &[String]) -> Result<SmokeOpts, String> {
    let mut opts = SmokeOpts::default();
    let mut pending: Option<&'static str> = None;
    for a in args {
        if let Some(key) = pending {
            if a.is_empty() || a.starts_with("--") {
                return Err(format!(
                    "smoke: --{} needs a value{}",
                    key.replace('_', "-"),
                    if a.is_empty() {
                        String::new()
                    } else {
                        format!(" (got the flag {a})")
                    }
                ));
            }
            set_valued(&mut opts, key, a);
            pending = None;
            continue;
        }
        if let Some((flag, val)) = a.split_once('=') {
            if let Some(key) = valued_flag(flag) {
                if val.is_empty() {
                    return Err(format!("smoke: {flag} needs a value"));
                }
                set_valued(&mut opts, key, val);
                pending = None;
                continue;
            }
        }
        if let Some(key) = valued_flag(a) {
            pending = Some(key);
            continue;
        }
        if let Some(on) = boolean_flag(a) {
            match a.as_str() {
                "--list" => opts.list = on,
                "--verbose" => opts.verbose = on,
                "--keep-going" => opts.keep_going = on,
                "--retry-failed" => opts.retry_failed = on,
                _ => opts.changed = on,
            }
            continue;
        }
        return Err(format!("smoke: unknown arg {a:?}"));
    }
    if let Some(key) = pending {
        return Err(format!("smoke: --{} needs a value", key.replace('_', "-")));
    }
    validate(&opts)?;
    Ok(opts)
}

fn valued_flag(a: &str) -> Option<&'static str> {
    match a {
        "--only" => Some("only"),
        "--skip" => Some("skip"),
        "--base" => Some("base"),
        "--head" => Some("head"),
        "--shard" => Some("shard"),
        "--ambient" => Some("ambient"),
        "--state" => Some("state"),
        _ => None,
    }
}

fn set_valued(opts: &mut SmokeOpts, key: &'static str, value: &str) {
    match key {
        "only" => opts.only = value.to_string(),
        "skip" => opts.skip = value.to_string(),
        "base" => opts.base = value.to_string(),
        "head" => opts.head = value.to_string(),
        "shard" => opts.shard = value.to_string(),
        "ambient" => opts.ambient = value.to_string(),
        "state" => opts.state = value.to_string(),
        _ => {}
    }
}

fn validate(opts: &SmokeOpts) -> Result<(), String> {
    // Four subset modes with no defined precedence: refuse rather than let
    // one silently win and mislabel the evidence.
    let mut subsets: Vec<String> = Vec::new();
    if opts.changed {
        subsets.push("--changed".into());
    }
    if !opts.only.is_empty() {
        subsets.push("--only".into());
    }
    if !opts.skip.is_empty() {
        subsets.push("--skip".into());
    }
    if opts.retry_failed {
        subsets.push("--retry-failed".into());
    }
    if subsets.len() > 1 {
        return Err(format!(
            "smoke: {} are separate subset modes - pick one",
            subsets.join(" and ")
        ));
    }
    if !opts.shard.is_empty() && opts.retry_failed {
        return Err(
            "smoke: --retry-failed and --shard are separate subset modes - pick one".into(),
        );
    }
    if (!opts.base.is_empty() || !opts.head.is_empty()) && !opts.changed {
        return Err("smoke: --base/--head only apply to --changed".into());
    }
    if !opts.shard.is_empty() {
        parse_smoke_shard(&opts.shard)?;
    }
    if !AMBIENT_MODES.contains(&opts.ambient.as_str()) {
        return Err(format!(
            "smoke: --ambient {:?} is not one of {}",
            opts.ambient,
            AMBIENT_MODES.join("/")
        ));
    }
    if !STATE_MODES.contains(&opts.state.as_str()) {
        return Err(format!(
            "smoke: --state {:?} is not one of {}",
            opts.state,
            STATE_MODES.join("/")
        ));
    }
    Ok(())
}

/// `--shard must be I/N with 1 <= I <= N`.
pub fn parse_smoke_shard(spec: &str) -> Result<(usize, usize), String> {
    let parts: Vec<&str> = spec.trim().split('/').collect();
    if parts.len() != 2 {
        return Err(format!(
            "smoke: --shard must be I/N with 1 <= I <= N, got {spec:?}"
        ));
    }
    let (index, total) = match (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
        (Ok(i), Ok(n)) => (i, n),
        _ => {
            return Err(format!(
                "smoke: --shard must be I/N with 1 <= I <= N, got {spec:?}"
            ))
        }
    };
    if total < 1 || index < 1 || index > total {
        return Err(format!(
            "smoke: --shard must be I/N with 1 <= I <= N, got {spec:?}"
        ));
    }
    Ok((index, total))
}

/// Boolean smoke flags, outside the valued branches above.
fn boolean_flag(a: &str) -> Option<bool> {
    match a {
        "--list" => Some(true),
        "--verbose" => Some(true),
        "--keep-going" => Some(true),
        "--changed" => Some(true),
        "--retry-failed" => Some(true),
        _ => None,
    }
}

/// The sandbox-relative junit report path for one state lane.
pub fn state_junit_path(mode: &str) -> PathBuf {
    SandboxGuard::shared()
        .map(|s| s.join(format!("state-verdict-{mode}.xml")))
        .unwrap_or_else(|_| PathBuf::from(format!("/tmp/state-verdict-{mode}.xml")))
}

/// Read pytest's positive testcase statuses from a JUnit report.
/// `(classname, name) -> passed|failed|skipped`.
pub fn state_verdicts(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for caps in testcase_re().captures_iter(&text) {
        let tag = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let body = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let classname = attr(tag, "classname").unwrap_or("");
        let name = attr(tag, "name").unwrap_or("");
        let nodeid = if classname.is_empty() {
            name.to_string()
        } else {
            format!("{classname}::{name}")
        };
        let verdict = if body.contains("<failure") || body.contains("<error") {
            "failed"
        } else if body.contains("<skipped") {
            "skipped"
        } else {
            "passed"
        };
        out.insert(nodeid, verdict.to_string());
    }
    out
}

fn testcase_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?s)<testcase\s+([^>]*?)/?>((?:(?!</testcase>).)*?)(?:</testcase>|$)")
            .expect("static regex")
    })
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let re = regex::Regex::new(&format!(r#"{name}="([^"]*)""#)).ok()?;
    re.captures(tag).map(|c| c.get(1).unwrap().as_str())
}

/// The one positive control the state lanes read, matched on its own test
/// name (startswith, so a future parametrisation still matches; the wiring
/// names do not begin with this string).
pub const STATE_CONTROL_TEST: &str = reg::STATE_CONTROL_TEST;

pub fn is_state_control(nodeid: &str) -> bool {
    nodeid
        .rsplit_once("::")
        .map(|x| x.1)
        .unwrap_or(nodeid)
        .starts_with(STATE_CONTROL_TEST)
}

/// Testcase node IDs whose clean/populated verdict changed, sorted.
pub fn state_verdict_diff(
    clean: &BTreeMap<String, String>,
    populated: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut ids: Vec<&String> = clean
        .keys()
        .chain(populated.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    ids.sort();
    ids.into_iter()
        .filter(|id| {
            clean.get(*id).map(String::as_str).unwrap_or("missing")
                != populated.get(*id).map(String::as_str).unwrap_or("missing")
        })
        .cloned()
        .collect()
}

/// The --state both verdict, decided by the junit comparison alone.
///
/// The populated lane is SUPPOSED to end red on the state canary (that is the
/// positive control), so its raw exit code can never decide the verdict. The
/// comparison is the verdict; the raw populated rc is not read. `leaked` is
/// the one thing the discarded rc still carries: the FILESYSTEM canary is a
/// different instrument from the junit control, and a lane that wrote to the
/// operator state root is never green. `broken` answers a different question:
/// not "did a lane write to the operator state root" but "was that root
/// measured at all".
pub fn state_both_exit(
    clean_rc: i32,
    diff: &[String],
    canary_clean: Option<&str>,
    canary_populated: Option<&str>,
    leaked: bool,
    broken: bool,
) -> i32 {
    if leaked || broken {
        return 1;
    }
    if canary_clean != Some("passed") || canary_populated != Some("failed") {
        return 1;
    }
    if clean_rc != 0 {
        return clean_rc;
    }
    if diff.is_empty() {
        return 1;
    }
    if diff.iter().any(|id| !is_state_control(id)) {
        return 1;
    }
    0
}

/// Add testcase receipts to the Python suite for a state lane: the pytest
/// step's command gains `--junitxml`.
pub fn state_steps(steps: &[disc::Step], mode: &str) -> Vec<disc::Step> {
    let report = shell_quote(&state_junit_path(mode).to_string_lossy());
    steps
        .iter()
        .map(|(name, cwd, cmd)| {
            if name == "Pytest (unit + integration)" {
                (
                    name.clone(),
                    cwd.clone(),
                    format!("{cmd} --junitxml={report}"),
                )
            } else {
                (name.clone(), cwd.clone(), cmd.clone())
            }
        })
        .collect()
}

fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_-=.,:@+".contains(c))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// The snapshot path for this process, so two runs on one box never share a
/// FILE: plant and verify run in the SAME process, so the pid keys both
/// halves.
pub(crate) fn state_canary_snapshot() -> String {
    let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    format!(
        "{}/fno-state-canary.{}.snapshot",
        tmp.trim_end_matches('/'),
        std::process::id()
    )
}

/// Run scripts/ci/check-state-canary.sh on the PARENT HOME.
///
/// Deliberately NOT under the smoke env: the sandbox is what the suite is
/// allowed to write, and the parent HOME is the surface the canary exists to
/// protect. A missing script is fatal on verify and non-fatal on plant: a
/// verify that cannot run must never read as a green.
pub fn run_state_canary(root: &Path, command: &str) -> i32 {
    let script = root.join("scripts/ci/check-state-canary.sh");
    if !script.is_file() {
        if command == "verify" {
            eprintln!(
                "smoke: {} is missing - cannot verify the operator state root was \
untouched, refusing to call this green",
                script.to_string_lossy()
            );
            return 1;
        }
        return 0;
    }
    let status = std::process::Command::new("bash")
        .arg(&script)
        .arg(command)
        .current_dir(root)
        .env("FNO_STATE_CANARY_SNAPSHOT", state_canary_snapshot())
        .status();
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 1,
    }
}

/// Recorded step names that still exist in the registry (corrupt -> drop).
fn read_failure_record(path: &str, known: &std::collections::BTreeSet<String>) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && known.contains(*l))
        .map(String::from)
        .collect()
}

/// The parent dir may not exist in a fresh checkout; an unwritable record is
/// non-fatal (last-writer-wins per checkout).
fn write_failure_record(path: &str, failed: &[String]) {
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = failed.iter().map(|n| format!("{n}\n")).collect::<String>();
    let _ = std::fs::write(path, body);
}

/// Lane-scoped failure record: the two lanes fail for different reasons, so a
/// shared record would let a dirty-lane failure list be replayed into a
/// clean-lane --retry-failed.
fn failure_record_path(root: &Path, ambient: &str) -> String {
    let base = std::env::var("SMOKE_FAILURE_RECORD").unwrap_or_else(|_| {
        root.join(reg::SMOKE_FAILURE_RECORD_DEFAULT)
            .to_string_lossy()
            .to_string()
    });
    if ambient == "clean" {
        return base;
    }
    match base.rpartition(".") {
        Some((b, dot, ext)) if dot == "." => format!("{b}-{ambient}.{ext}"),
        _ => format!("{base}-{ambient}"),
    }
}

trait Partition {
    fn rpartition<'a>(&self, sep: &'a str) -> Option<(&str, &'a str, &str)>;
}

impl Partition for str {
    fn rpartition<'a>(&self, sep: &'a str) -> Option<(&str, &'a str, &str)> {
        match self.rfind(sep) {
            Some(i) => Some((&self[..i], sep, &self[i + sep.len()..])),
            None => None,
        }
    }
}

/// Remove the checkout's target/ binaries: the parity-test marker.
pub(crate) fn scrub_target_bins(root: &Path) {
    for rel in reg::TARGET_BIN_RELS {
        let _ = std::fs::remove_file(root.join(rel));
    }
}

/// Pin the claim door: the env name outranks PATH in resolve_binary.
///
/// Never touches PATH. Hook harnesses shell bare `fno-agents` and route on
/// its absence (spaces fallback); a PATH-visible binary would flip those
/// writes onto space paths mid-shard.
fn pin_claim_door(env: &mut BTreeMap<String, String>, binary: &Path) {
    env.insert(
        "FNO_AGENTS_BIN".into(),
        binary.to_string_lossy().to_string(),
    );
}

/// Keep the native claim reader available after the Rust-marker scrub.
///
/// The door always reads a sandbox COPY: a build-dir override in the packet's
/// build step, or a later scrub, must not strand the claim consumers the way
/// a direct target/ pin did.
pub(crate) fn preserve_claim_door(root: &Path, env: &mut BTreeMap<String, String>) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for key in ["FNO_AGENTS_BIN", "FNO_AGENTS_FRONT"] {
        if let Some(v) = std::env::var(key).ok().filter(|v| !v.is_empty()) {
            candidates.push(PathBuf::from(v));
        }
    }
    for rel in reg::TARGET_BIN_RELS {
        candidates.push(root.join(rel));
    }
    let source = candidates.iter().find(|c| c.is_file() && is_executable(c));
    let Some(source) = source else {
        return;
    };
    let dir = match SandboxGuard::shared() {
        Ok(s) => s.join(reg::CLAIM_DOOR_NAME),
        Err(_) => return,
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let preserved = dir.join("fno-agents");
    if std::fs::copy(source, &preserved).is_err() {
        return;
    }
    let _ = set_exec_bit(&preserved);
    // The scrub unlinks the target/ copies this function may have been
    // handed, so a BIN inherited from the job env goes dead at exactly the
    // moment the claim consumers run. Re-point it at the preserved copy.
    pin_claim_door(env, &preserved);
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn set_exec_bit(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path)?.permissions();
        perm.set_mode(perm.mode() | 0o755);
        std::fs::set_permissions(path, perm)
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Lane-scoped ambient + state per full-lane run, held between entry and
/// execution (the Python module globals' typed replacement).
pub struct LaneShape {
    pub ambient: AmbientMode,
    pub ambient_label: &'static str,
    pub state: String,
    /// Whether the caller passed --state at all: a passed `--state clean`
    /// also attaches the junit receipts, for the both-lane comparison.
    pub state_flag: bool,
}

/// The smoke entry: `fno-agents test smoke <args>`.
pub fn run(args: &[String], _stream: bool) -> i32 {
    let root = crate::test_command::repo_root(&std::env::current_dir().unwrap_or_default())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return 0;
    }
    let opts = match parse_smoke_args(args) {
        Ok(o) => o,
        Err(line) => {
            eprintln!("{line}");
            return 2;
        }
    };
    if opts.ambient == "both" {
        return run_ambient_both(&root, args);
    }
    if opts.state == "both" && !opts.list {
        return run_state_both(&root, args);
    }
    let lane = LaneShape {
        ambient: match opts.ambient.as_str() {
            "dirty" => AmbientMode::Dirty,
            _ => AmbientMode::Clean,
        },
        ambient_label: if opts.ambient == "dirty" {
            "dirty"
        } else {
            "clean"
        },
        state: opts.state.clone(),
        state_flag: args
            .iter()
            .any(|a| a == "--state" || a.starts_with("--state=")),
    };
    run_single_lane(&root, &opts, &lane)
}

/// `--ambient both`: run the two ambient lanes and report divergence.
fn run_ambient_both(_root: &Path, args: &[String]) -> i32 {
    let rest = strip_lane_flag(args, "--ambient");
    println!("ambient: clean lane");
    let clean_rc = run(&rest, false);
    println!("ambient: dirty lane");
    let dirty_rc = run(&rest, false);
    if clean_rc == 0 && dirty_rc != 0 {
        eprintln!(
            "ambient divergence: the clean lane is green and the dirty lane is red on the same commit. \
A test above read ambient state that the neutralise surface does not neutralise. The failing test names itself; \
add its channel to the ambient-vocabulary table."
        );
    } else if clean_rc != 0 && dirty_rc == 0 {
        eprintln!(
            "ambient divergence (inverted): the clean lane is red and the dirty lane is green. \
A test DEPENDS on ambient state being present."
        );
    }
    // The Python leg's `clean_rc or dirty_rc`: the first nonzero verdict wins.
    if clean_rc != 0 {
        clean_rc
    } else {
        dirty_rc
    }
}

/// Strip one lane flag (`--ambient`/`--state`) and its value from args, so a
/// lane re-run carries everything else byte-verbatim.
fn strip_lane_flag(args: &[String], flag: &str) -> Vec<String> {
    let mut rest: Vec<String> = Vec::new();
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == flag {
            skip_next = true;
            continue;
        }
        if a.starts_with(&format!("{flag}=")) {
            continue;
        }
        rest.push(a.clone());
    }
    rest
}

/// `--state both`: two state lanes, then the junit comparison is the verdict.
fn run_state_both(_root: &Path, args: &[String]) -> i32 {
    let rest = strip_lane_flag(args, "--state");
    println!("state: clean lane");
    let clean_rc = run(&rest, false);
    println!("state: populated lane");
    let _populated_rc = run(&rest, false);
    let diff = state_verdict_diff(
        &state_verdicts(&state_junit_path("clean")),
        &state_verdicts(&state_junit_path("populated")),
    );
    let canary_clean = state_verdicts(&state_junit_path("clean"))
        .into_iter()
        .find(|(id, _)| is_state_control(id))
        .map(|(_, v)| v);
    let canary_populated = state_verdicts(&state_junit_path("populated"))
        .into_iter()
        .find(|(id, _)| is_state_control(id))
        .map(|(_, v)| v);
    if !diff.is_empty() {
        println!("state verdict diff:");
        for nodeid in &diff {
            println!("  {nodeid}");
        }
    }
    if canary_clean.as_deref() != Some("passed") || canary_populated.as_deref() != Some("failed") {
        eprintln!(
            "state verdict diff: positive control missing or unexpected; require canary clean=passed and populated=failed"
        );
    } else if diff.is_empty() {
        eprintln!(
            "state verdict diff: no changed packet steps ran a canary; verify the populated lane ran its positive control before trusting this result"
        );
    }
    // The filesystem canary's leak/broken flags ride the sandbox sidecar
    // files the single lanes write after their plant/verify bracket (the
    // Python module globals' replacement): a leaked root or an unmeasured
    // root is never green, and the discarded populated rc has no other way
    // to carry them.
    let leaked = sandbox_marker("leaked").unwrap_or(false);
    let broken = sandbox_marker("broken").unwrap_or(false);
    if leaked {
        eprintln!(
            "state: a lane's filesystem canary refused, so a step wrote to the operator state root. \
The populated lane's exit code is discarded by design, so that refusal has no other way out and is carried here instead. The run is NOT green."
        );
    }
    if broken {
        eprintln!(
            "state: a lane's canary could not take a baseline, so that lane's state root was never measured. \
The populated lane's exit code is discarded by design, so this has no other way out and is carried here instead. The run is NOT green, because an unmeasured root is not a clean one."
        );
    }
    state_both_exit(
        clean_rc,
        &diff,
        canary_clean.as_deref(),
        canary_populated.as_deref(),
        leaked,
        broken,
    )
}

/// Read one boolean canary marker from the session sandbox.
fn sandbox_marker(kind: &str) -> Result<bool, String> {
    Ok(crate::test_command::SandboxGuard::shared()?
        .join(format!("state-canary-{kind}"))
        .is_file())
}

/// _child_env + the smoke additions: NO_COLOR kills the SGR escapes that
/// re-inflate an agent log ~7x; cli/.venv/bin on PATH pins every step's
/// python3/fno/pytest to the project env; FORCE_COLOR out. The census
/// shares this env assembly with the full lanes.
pub(crate) fn smoke_env(
    root: &Path,
    ambient: crate::test_command::AmbientMode,
    sandbox: &Path,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let child = crate::test_command::build_child_env(root, ambient, sandbox)?;
    let mut env = child.env;
    env.insert("NO_COLOR".into(), "1".into());
    env.remove("FORCE_COLOR");
    let venv_bin = root.join("cli/.venv/bin").to_string_lossy().to_string();
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    env.insert("PATH".into(), format!("{venv_bin}:{existing_path}"));
    Ok(env)
}

/// One full-lane run: selection, header, prereqs, the canary bracket,
/// execution, and the failure record.
fn run_single_lane(root: &Path, opts: &SmokeOpts, lane: &LaneShape) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = crate::test_command::repo_root(&cwd).unwrap_or_else(|| root.to_path_buf());
    let sandbox = match crate::test_command::SandboxGuard::shared() {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let mut env = match smoke_env(&root, lane.ambient, sandbox) {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    env.insert(
        "FNO_STATE_POPULATED".into(),
        if lane.state == "populated" { "1" } else { "0" }.into(),
    );
    env.insert("STATE_LANE".into(), lane.state.clone());
    if lane.state == "populated" {
        if let Err(msg) = populate_state_profile(sandbox) {
            eprintln!("{msg}");
            return 2;
        }
        env.insert(
            "FNO_CONFIG".into(),
            sandbox
                .join("populated-state/config.toml")
                .to_string_lossy()
                .to_string(),
        );
        env.insert(
            "STATE_PROFILE_DIR".into(),
            sandbox.join("home/.fno").to_string_lossy().to_string(),
        );
        env.insert(
            "CLAUDE_CODE_SESSION_ID".into(),
            "state-canary-session".into(),
        );
    }
    if opts.changed {
        return crate::test_changed::run_changed(&root, opts, &env, sandbox);
    }
    let mut steps: Vec<disc::Step> = match std::env::var("SMOKE_REGISTRY_FILE") {
        Ok(file) => match read_registry_seam(Path::new(&file)) {
            Ok(s) => s,
            Err(msg) => {
                eprintln!("{msg}");
                return 2;
            }
        },
        Err(_) => disc::smoke_steps(&root),
    };
    if lane.state_flag {
        steps = state_steps(&steps, &lane.state);
    }

    let total = steps.len();
    let names: std::collections::BTreeSet<String> = steps.iter().map(|s| s.0.clone()).collect();

    // Selection: build the selected indices from the mode.
    let mut retry_fell_back = false;
    let failure_record = failure_record_path(&root, lane.ambient_label);
    let mut selected: Vec<usize> = if opts.retry_failed {
        let known = names.clone();
        let recorded = read_failure_record(&failure_record, &known);
        if recorded.is_empty() {
            retry_fell_back = true;
            (0..total).collect()
        } else {
            steps
                .iter()
                .enumerate()
                .filter(|(_, s)| recorded.contains(&s.0))
                .map(|(i, _)| i)
                .collect()
        }
    } else if !opts.only.is_empty() {
        let picked: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, s)| disc::name_matches(&s.0, &opts.only))
            .map(|(i, _)| i)
            .collect();
        if picked.is_empty() {
            eprintln!("smoke: --only {:?} matched no steps. Available:", opts.only);
            for s in &steps {
                eprintln!("  {}", s.0);
            }
            return 1;
        }
        picked
    } else if !opts.skip.is_empty() {
        let picked: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, s)| !disc::name_matches(&s.0, &opts.skip))
            .map(|(i, _)| i)
            .collect();
        if picked.is_empty() {
            eprintln!("smoke: --skip {:?} left no steps. Available:", opts.skip);
            for s in &steps {
                eprintln!("  {}", s.0);
            }
            return 1;
        }
        picked
    } else {
        (0..total).collect()
    };

    // `--shard I/N` splits the selection across legs, heaviest first.
    let mut shard_index = 0;
    let mut shard_total = 0;
    if !opts.shard.is_empty() {
        // validate() already parsed this spec; a failure here is the
        // caller bypassing the door, so refuse rather than guess.
        let (idx, tot) = match parse_smoke_shard(&opts.shard) {
            Ok(pair) => pair,
            Err(msg) => {
                eprintln!("{msg}");
                return 2;
            }
        };
        shard_index = idx;
        shard_total = tot;
        if tot > 1 {
            let weights = disc::smoke_step_weights(&root);
            selected = shard_selected_indices(&steps, &selected, idx, tot, &weights);
        }
    }
    if selected.is_empty() {
        eprintln!("smoke: shard selected zero steps - never green");
        return 1;
    }
    if opts.list {
        for i in &selected {
            if opts.verbose {
                let joined = steps[*i].2.replace('\n', ";");
                println!("{}\n  cwd: {}\n  cmd: {}", steps[*i].0, steps[*i].1, joined);
            } else {
                println!("{}", steps[*i].0);
            }
        }
        return 0;
    }
    // Header: a partial run must never read as full.
    let mut label = "FULL";
    if opts.retry_failed {
        label = "RETRY SUBSET";
    } else if !opts.only.is_empty() {
        label = "ONLY SUBSET";
    } else if !opts.skip.is_empty() {
        label = "SKIP SUBSET";
    }
    if shard_total > 1 {
        label = "SHARD SUBSET";
    }
    let kg = if opts.keep_going { " keep-going" } else { "" };
    let mut shard_header = String::new();
    if shard_total > 1 {
        let pytest_marker = if selected
            .iter()
            .any(|i| steps[*i].0 == "Pytest (unit + integration)")
        {
            " pytest=split"
        } else {
            ""
        };
        shard_header = format!(" shard={shard_index}/{shard_total}{pytest_marker}");
    }
    println!(
        "smoke: mode={label}{shard_header} steps={}/{total}{kg}",
        selected.len()
    );
    // One line per planned step: fail-fast means later steps never run, and a
    // reader diffing these lines against the `smoke: pass|fail` lines below
    // can name what was unreached from the log alone. pr_status parses this
    // exact prefix.
    for i in &selected {
        println!("smoke: planned: {}", steps[*i].0);
    }
    if retry_fell_back {
        println!("smoke: no usable failure record - falling back to FULL run");
    }
    if selected.len() != total {
        println!(
            "smoke: SUBSET run - run 'fno doctor test smoke' full before the settle-green push"
        );
    }

    let cmds: Vec<&str> = selected.iter().map(|i| steps[*i].2.as_str()).collect();
    if let Some(miss) = disc::smoke_prereqs(&cmds) {
        eprintln!("smoke: missing prerequisite: {miss}");
        return 2;
    }

    // Faithful-ordering guard: every step BEFORE the dedicated build step runs
    // with the fno-agents binary absent so @requires_rust parity tests skip.
    let delete_triggers = ["Pytest (unit + integration)", reg::RUST_BUILD_STEP];
    let triggers_hit = selected
        .iter()
        .any(|i| delete_triggers.contains(&steps[*i].0.as_str()));
    if triggers_hit {
        preserve_claim_door(&root, &mut env);
        scrub_target_bins(&root);
    }
    // The canary brackets the whole run: plant before the first step, verify
    // after the last.
    let plant_rc = run_state_canary(&root, "plant");
    let (results, first_rc) = execute_steps(
        &root,
        &env,
        &steps,
        &selected,
        opts.keep_going,
        if shard_total > 1 {
            opts.shard.as_str()
        } else {
            ""
        },
    );
    let canary_broken = plant_rc != 0;
    let canary_rc = run_state_canary(&root, "verify");
    if canary_rc != 0 && !canary_broken {
        let _ = std::fs::write(
            crate::test_command::SandboxGuard::shared()
                .map(|s| s.join("state-canary-leaked"))
                .unwrap_or_default(),
            "1",
        );
    }
    if canary_broken {
        let _ = std::fs::write(
            crate::test_command::SandboxGuard::shared()
                .map(|s| s.join("state-canary-broken"))
                .unwrap_or_default(),
            "1",
        );
    }
    // Journey/rust/bash steps leak keepers via CLI subprocesses no conftest
    // reaches; the sweep rides the graph keeper's own module, in process.
    let _swept = crate::graph_keeper::sweep_orphaned(15.0);
    let failed: Vec<String> = results
        .iter()
        .filter(|(_, s, _)| s == "fail")
        .map(|(n, _, _)| n.clone())
        .collect();
    let failed_count = failed.len();
    if first_rc != 0 && !opts.keep_going {
        write_failure_record(&failure_record, &failed);
        return 1;
    }
    println!();
    let kind = if shard_total > 1 {
        "shard-subset"
    } else if opts.retry_failed {
        "retry-subset"
    } else if !opts.only.is_empty() {
        "only-subset"
    } else if !opts.skip.is_empty() {
        "skip-subset"
    } else {
        "full"
    };
    println!("smoke: summary ({kind}, {} steps)", results.len());
    for (n, s, d) in &results {
        println!("  {s:6} {d:4.0}s  {n}");
    }
    write_failure_record(&failure_record, &failed);
    if canary_broken {
        eprintln!(
            "smoke: the state canary could not take a baseline (plant exit {plant_rc}), so nothing was measured. \
This is the INSTRUMENT failing, not a step writing to the operator state root. The run is not called green, because an unmeasured root is not a clean one."
        );
    } else if canary_rc != 0 && failed_count == 0 {
        eprintln!(
            "smoke: every step passed but the state canary refused - a step wrote to the operator state root. The run is NOT green."
        );
    }
    if failed_count > 0 || canary_rc != 0 {
        1
    } else {
        0
    }
}

/// Run the selected steps in order, returning (per-step results, first
/// non-zero child rc). The child's real return code is propagated, never a
/// pipe's or a `tail`'s. Each step is `bash -eo pipefail -c <cmd>` in the
/// step's repo-relative cwd, under the smoke env plus the pytest shard var.
pub(crate) fn execute_steps(
    root: &Path,
    env: &BTreeMap<String, String>,
    steps: &[disc::Step],
    selected: &[usize],
    keep_going: bool,
    pytest_shard: &str,
) -> (Vec<(String, String, f64)>, i32) {
    let in_ci = std::env::var_os("GITHUB_ACTIONS").is_some();
    let mut results: Vec<(String, String, f64)> = Vec::new();
    let mut first_rc = 0;
    for i in selected {
        let (name, cwd, cmd) = (&steps[*i].0, &steps[*i].1, &steps[*i].2);
        if in_ci {
            println!("::group::{name}");
        } else {
            println!();
            println!("=== {name} ===");
        }
        let t0 = std::time::Instant::now();
        let dir = if cwd == "." {
            root.to_path_buf()
        } else {
            root.join(cwd)
        };
        let mut command = std::process::Command::new("bash");
        command.args(["-eo", "pipefail", "-c", cmd]);
        command.current_dir(&dir);
        command.env_clear();
        for (k, v) in env {
            command.env(k, v);
        }
        command.env_remove("FNO_PYTEST_SHARD");
        if !pytest_shard.is_empty() && name == "Pytest (unit + integration)" {
            command.env("FNO_PYTEST_SHARD", pytest_shard);
        }
        let rc = match command.status() {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("smoke: failed to run step: {e}");
                127
            }
        };
        let dur = t0.elapsed().as_secs_f64();
        if in_ci {
            println!("::endgroup::");
        }
        let verdict = if rc == 0 { "pass" } else { "fail" };
        results.push((name.clone(), verdict.to_string(), dur));
        println!("smoke: {verdict}  {dur:4.0}s  {name}");
        if rc != 0 {
            if first_rc == 0 {
                first_rc = rc;
            }
            if !keep_going {
                eprintln!("smoke: step failed, stopping (fail-fast): {name}");
                break;
            }
        }
    }
    (results, first_rc)
}

/// Seed a populated state shape and return nothing; the profile dir and the
/// state root live under the session sandbox (the FNO_CONFIG the lane sets
/// points at the profile).
fn populate_state_profile(sandbox: &Path) -> Result<(), String> {
    use std::fs;
    let profile = sandbox.join("populated-state");
    let state_dir = sandbox.join("home/.fno");
    fs::create_dir_all(state_dir.join("agents")).map_err(|e| e.to_string())?;
    fs::create_dir_all(state_dir.join("claims")).map_err(|e| e.to_string())?;
    fs::create_dir_all(&profile).map_err(|e| e.to_string())?;
    // The fixture README rides along when the checkout carries it.
    let readme = repo_fixture_dir().map(|d| d.join("README.md"));
    if let Some(readme) = readme.filter(|p| p.is_file()) {
        let _ = fs::copy(&readme, profile.join("README.md"));
    }
    fs::write(
        profile.join("config.toml"),
        format!("state_dir = {}\n", json_quote(&state_dir.to_string_lossy())),
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        state_dir.join("graph.json"),
        format!(
            "{{\"entries\": [{{\"id\": {}, \"title\": \"state lane positive control\", \"status\": \"idea\", \"blocked_by\": []}}]}}",
            json_quote(crate::test_smoke::STATE_CONTROL_TEST)
        ),
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        state_dir.join("agents/registry.json"),
        "{\"entries\": [{\"name\": \"state-canary\", \"role_label\": \"state-canary\"}]}",
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        state_dir.join("claims/state-canary.lock"),
        "{\"holder\": \"state-canary\", \"session_id\": \"state-canary-session\"}",
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn json_quote(s: &str) -> String {
    serde_json::json!(s).to_string()
}

/// The repo's cli/tests/fixtures dir, resolved from the current checkout.
fn repo_fixture_dir() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    crate::test_command::repo_root(&cwd).map(|r| r.join("cli/tests/fixtures/populated-state"))
}

/// The test seam: `SMOKE_REGISTRY_FILE` names a JSON file
/// `{"steps": [[name, cwd, cmd], ...]}` exercising the mode machinery without
/// the real ~80 steps or their uv/cargo prerequisites. (The Python seam ran a
/// .py file; the JSON shape needs no interpreter in the reader.)
fn read_registry_seam(path: &Path) -> Result<Vec<disc::Step>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("smoke: cannot read SMOKE_REGISTRY_FILE {path:?}: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("smoke: registry seam not JSON: {e}"))?;
    let rows = value
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "smoke: registry seam carries no steps array".to_string())?;
    let mut steps = Vec::new();
    for row in rows {
        let parts = row
            .as_array()
            .filter(|p| p.len() == 3)
            .ok_or_else(|| "smoke: registry seam rows must be [name, cwd, cmd]".to_string())?;
        let get = |i: usize| parts[i].as_str().unwrap_or_default().to_string();
        steps.push((get(0), get(1), get(2)));
    }
    Ok(steps)
}

/// The smoke --help: the mode machinery's own doc, as the Python printed its
/// docstring.
fn print_help() {
    println!(
        "smoke: run the full CI smoke locally.

Flags: --list [--verbose], --keep-going, --only <globs>, --skip <globs>,
--shard I/N, --retry-failed, and --changed [--base REV --head REV] for the
changed-surface packet (a CHANGED SUBSET; exits 20 when nothing mapped, 21
when the diff is not trustworthy - neither is a green verdict, and the full
run still gates). --shard can combine with --only or --skip, but not with
--changed or --retry-failed.

--ambient clean|dirty|both (default clean) picks the ambient shape every
step's child process runs under. --state clean|populated|both picks the
state-lane shape. hermeticity is applied by THIS runner, when it builds each
step's environment; a bare `bash tests/foo.sh` skips it, so that run reads
your real HOME."
    );
}

/// Put each selected step, heaviest first (unlisted: 2s), on the lightest
/// leg. Shared steps (build, pytest) ride every leg; a leg that drew a
/// later step than the build also keeps the build, because a shell harness
/// after pytest needs the binary the pytest shard deletes.
pub fn shard_selected_indices(
    steps: &[disc::Step],
    selected: &[usize],
    index: usize,
    total: usize,
    weights: &BTreeMap<String, f64>,
) -> Vec<usize> {
    let name_of = |i: usize| steps[i].0.as_str();
    let weight = |name: &str| weights.get(name).copied().unwrap_or(2.0);
    let shared = ["Sync + build", "Pytest (unit + integration)"];
    let mut loads = vec![0.0f64; total];
    let mut mine: std::collections::BTreeSet<usize> = Default::default();
    let mut ordered: Vec<usize> = selected
        .iter()
        .copied()
        .filter(|i| !shared.contains(&name_of(*i)))
        .collect();
    ordered.sort_by(|a, b| {
        weight(name_of(*b))
            .partial_cmp(&weight(name_of(*a)))
            .unwrap()
            .then(a.cmp(b))
    });
    for i in ordered {
        let leg = loads
            .iter()
            .enumerate()
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .map(|(n, _)| n)
            .unwrap_or(0);
        loads[leg] += weight(name_of(i));
        if leg == index - 1 {
            mine.insert(i);
        }
    }
    let mut kept: std::collections::BTreeSet<usize> = mine.clone();
    for i in selected {
        if shared.contains(&name_of(*i)) {
            kept.insert(*i);
        }
    }
    let build_index = steps.iter().position(|s| s.0 == reg::RUST_BUILD_STEP);
    if let Some(bi) = build_index {
        if selected.contains(&bi) && mine.iter().any(|i| *i > bi) {
            kept.insert(bi);
        }
    }
    kept.into_iter().collect()
}

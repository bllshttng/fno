//! The sandbox launch seam: the `[sandbox] provider` key, the launch plan the
//! daemon answers for one worktree (`agent.sandbox-plan`), and the wrap that
//! runs a one-shot lane's command inside the box.
//!
//! Ruling Q7 order is shared-state protection first: a `devcontainer` worker
//! gets its worktree (the devcontainer CLI mounts the workspace folder) and
//! the daemon's socket directory, never the agents home. Registry and claims
//! change through daemon ops, never direct file writes. `none` is today: no
//! isolation, direct state access. The host client keeps every gate and every
//! claim; only the harness process runs in the box.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The daemon's command for the launch plan. The arm and the spawn request
/// both spell it through this const, after `SESSION_REPORT_COMMAND`.
pub const SANDBOX_PLAN_COMMAND: &str = "sandbox-plan";

/// Where the mounted state doors live inside the container. The launch env
/// pins (`FNO_SUPERVISOR_SOCKET`, `FNO_STATE_DIR`) resolve under it, so the
/// container needs no `~/.fno` at all.
pub const CONTAINER_STATE_DIR: &str = "/fno-state";

/// The socket directory inside the container. The host side is
/// `AgentsHome::sandbox_sock_dir`, where the daemon re-links its socket at
/// every bind; a mount of the socket file itself goes dead at the first
/// daemon restart.
pub const CONTAINER_SOCK_DIR: &str = "/fno-state/sock";

/// The one-shot lanes that call [`boxed`]. Any other harness or substrate
/// would launch on the host while the config says it is boxed, so the spawn
/// refuses it under `devcontainer`.
pub const BOXED_HARNESSES: [&str; 5] = ["claude", "codex", "gemini", "opencode", "agy"];

/// One bind-mount of the launch plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BindMount {
    pub host: PathBuf,
    pub container: PathBuf,
}

/// What one sandboxed spawn mounts and exports. `none` produces the empty
/// plan: the spawn command runs unchanged.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxLaunch {
    pub provider: String,
    pub mounts: Vec<BindMount>,
    pub env: BTreeMap<String, String>,
}

/// Which isolation a spawn uses. `none` is today; `devcontainer` isolates
/// the worker behind the bind-mount plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxProvider {
    None,
    Devcontainer,
}

impl SandboxProvider {
    /// Resolve `[sandbox] provider`. Absent reads `none` (today). An unknown
    /// spelling is an error, never a silent downgrade to `none`: the operator
    /// configured isolation and must not believe workers are sandboxed when
    /// they are not.
    pub fn from_config(cwd: &Path) -> Result<Self, String> {
        match crate::agents_config::config_lookup(cwd, &["sandbox", "provider"]) {
            None => Ok(Self::None),
            Some(v) => match v.as_str().map(str::trim) {
                Some("none") => Ok(Self::None),
                Some("devcontainer") => Ok(Self::Devcontainer),
                _ => Err(format!(
                    "[sandbox] provider must be \"none\" or \"devcontainer\", got {}",
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{v:?}"))
                )),
            },
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Devcontainer => "devcontainer",
        }
    }
}

/// The launch plan for one provider. The container paths are fixed, so a
/// launch recipe is reproducible from the plan alone.
pub fn launch_for(provider: SandboxProvider, sock_dir: &Path) -> SandboxLaunch {
    let mut launch = SandboxLaunch {
        provider: provider.as_str().to_string(),
        mounts: Vec::new(),
        env: BTreeMap::new(),
    };
    if provider == SandboxProvider::Devcontainer {
        launch.mounts.push(BindMount {
            host: sock_dir.to_path_buf(),
            container: PathBuf::from(CONTAINER_SOCK_DIR),
        });
        launch.env.insert(
            "FNO_SUPERVISOR_SOCKET".to_string(),
            format!("{CONTAINER_SOCK_DIR}/supervisor.sock"),
        );
        launch
            .env
            .insert("FNO_STATE_DIR".to_string(), CONTAINER_STATE_DIR.to_string());
    }
    launch
}

/// `devcontainer up` for the worktree with the plan's mounts. Each mount is
/// two argv items, `--mount` and `type=bind,source=..,target=..`: the CLI's
/// mount pattern refuses a `readonly` field and a single joined string.
pub fn up_argv(launch: &SandboxLaunch, worktree: &Path) -> Vec<String> {
    let mut argv = vec![
        "devcontainer".to_string(),
        "up".to_string(),
        "--workspace-folder".to_string(),
        worktree.display().to_string(),
        // A linked worktree's `.git` file points at the main checkout's
        // common dir; without it git fails inside the box.
        "--mount-git-worktree-common-dir".to_string(),
    ];
    for m in &launch.mounts {
        argv.push("--mount".to_string());
        argv.push(format!(
            "type=bind,source={},target={}",
            m.host.display(),
            m.container.display()
        ));
    }
    argv
}

/// `devcontainer exec` for the worktree with the plan's env pins, up to but
/// not including the command.
pub fn exec_prefix(launch: &SandboxLaunch, worktree: &Path) -> Vec<String> {
    let mut argv = vec![
        "devcontainer".to_string(),
        "exec".to_string(),
        "--workspace-folder".to_string(),
        worktree.display().to_string(),
    ];
    for (k, v) in &launch.env {
        argv.push("--remote-env".to_string());
        argv.push(format!("{k}={v}"));
    }
    argv
}

static ACTIVE: OnceLock<(SandboxLaunch, PathBuf)> = OnceLock::new();

/// The spawn's first sandbox leg, before the guard and the gate: does this
/// spawn run in a box? `Ok(false)` under `none`, and for a caller that is
/// itself boxed (its socket is pinned; a boxed worker spawning a peer stays
/// in its own box). An exit code and a reason for a spawn the box cannot
/// carry, so nothing launches on the host while the config says boxed.
pub fn check(cwd: &Path, harness: &str, substrate: &str) -> Result<bool, (i32, String)> {
    if crate::client::sock_pinned() {
        return Ok(false);
    }
    match SandboxProvider::from_config(cwd).map_err(|e| (2, e))? {
        SandboxProvider::None => Ok(false),
        SandboxProvider::Devcontainer
            if substrate == "headless" && BOXED_HARNESSES.contains(&harness) =>
        {
            Ok(true)
        }
        SandboxProvider::Devcontainer => Err((
            2,
            format!(
                "[sandbox] provider = \"devcontainer\" boxes only --as headless spawns of {}; \
                 a {harness} spawn on the {substrate} substrate would run on the host. \
                 Spawn with --as headless, or set [sandbox] provider = \"none\".",
                BOXED_HARNESSES.join(", ")
            ),
        )),
    }
}

/// The spawn's second sandbox leg, after the gate: ask the daemon for the
/// plan, bring the box up, and probe the daemon from inside it. Only a
/// passing probe arms [`boxed`]: a mounted socket that answers nobody (a
/// macOS Docker VM that does not pass unix sockets, or a container created
/// before the socket directory was mounted) would run a worker that can
/// report nothing.
pub fn arm(home: &crate::paths::AgentsHome, cwd: &Path) -> Result<(), (i32, String)> {
    use std::process::Stdio;
    let sock = crate::client::client_sock(home);
    let launch: SandboxLaunch = crate::protocol::blocking_call(
        &sock,
        &format!("agent.{SANDBOX_PLAN_COMMAND}"),
        serde_json::json!({ "worktree": cwd.to_string_lossy() }),
        std::time::Duration::from_secs(5),
    )
    .and_then(|v| serde_json::from_value(v).ok())
    .ok_or_else(|| {
        (
            13,
            format!(
                "the daemon at {} answered no sandbox plan; start it with `fno-agents status`",
                sock.display()
            ),
        )
    })?;
    // The daemon reads the key with its own env; a different `FNO_CONFIG`
    // there answers an unmounted plan, and no recreate can fix that.
    if launch.provider != SandboxProvider::Devcontainer.as_str() {
        return Err((
            2,
            format!(
                "the daemon reads [sandbox] provider = \"{}\" for {}, the spawn reads \"devcontainer\"; \
                 restart the daemon under the same config",
                launch.provider,
                cwd.display()
            ),
        ));
    }
    let run = |argv: Vec<String>| {
        Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map(|s| s.code().unwrap_or(1))
            .map_err(|e| (127, format!("{}: {e}", argv[0])))
    };
    match run(up_argv(&launch, cwd))? {
        0 => {}
        code => return Err((code, "devcontainer up failed".to_string())),
    }
    let mut probe = exec_prefix(&launch, cwd);
    probe.extend(["fno-agents".to_string(), "status".to_string()]);
    let inside = launch
        .env
        .get("FNO_SUPERVISOR_SOCKET")
        .cloned()
        .unwrap_or_default();
    match run(probe)? {
        0 => {}
        127 => {
            return Err((
                127,
                "the box image has no fno-agents on PATH; the worker could not report".to_string(),
            ))
        }
        _ => {
            return Err((
                84,
                format!(
                    "the box cannot reach the daemon at {inside}. Recreate the container \
                     with the socket mount: devcontainer up --workspace-folder {} \
                     --remove-existing-container",
                    cwd.display()
                ),
            ))
        }
    }
    let _ = ACTIVE.set((launch, cwd.to_path_buf()));
    Ok(())
}

/// Re-home a lane's composed command inside the box. Call it after the lane
/// sets its env and before it sets stdio or `pre_exec`: those do not carry
/// over to the new command. Unarmed, the command comes back untouched.
///
/// The lane env (route keys, the claim stamp) rides a 0600 file in the
/// mounted socket directory, which the box sources and deletes before the
/// exec: an argv cannot hold a secret. A refusal (a host path the box cannot
/// see) comes back as a command that prints the reason and exits 84, so each
/// lane reports it through its own subprocess-failure path.
pub fn boxed(cmd: Command) -> Command {
    let Some((launch, worktree)) = ACTIVE.get() else {
        return cmd;
    };
    let mut argv = vec![cmd.get_program().to_string_lossy().into_owned()];
    argv.extend(cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    // A removal (`env_remove`, the model scrub) rides as `None`: the box can
    // define the same key, so it must be unset there too.
    let env: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let argv = match box_argv(launch, worktree, strip_qos(argv), &env) {
        Ok(argv) => argv,
        Err(reason) => vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf '%s\\n' \"$0\" >&2; exit 84".to_string(),
            format!("fno-agents spawn: [sandbox] devcontainer refused: {reason}"),
        ],
    };
    let mut out = Command::new(&argv[0]);
    out.args(&argv[1..]);
    if let Some(dir) = cmd.get_current_dir() {
        out.current_dir(dir);
    }
    out
}

/// The QoS prefix (`spawn_gate::qos_wrap`) names a host binary the box may
/// not carry, and it would lower the priority of the host's devcontainer
/// client, not the worker. Drop it.
fn strip_qos(argv: Vec<String>) -> Vec<String> {
    let prefix: &[&str] = match argv.first().map(String::as_str) {
        Some("/usr/sbin/taskpolicy") => &["/usr/sbin/taskpolicy", "-c", "utility", "--"],
        Some("/usr/bin/nice") => &["/usr/bin/nice", "-n", "10"],
        _ => return argv,
    };
    if argv.len() > prefix.len() && argv.iter().zip(prefix).all(|(a, p)| a == p) {
        argv[prefix.len()..].to_vec()
    } else {
        argv
    }
}

fn box_argv(
    launch: &SandboxLaunch,
    worktree: &Path,
    mut argv: Vec<String>,
    env: &[(String, Option<String>)],
) -> Result<Vec<String>, String> {
    // The dispatch cwd is canonical; compare argv paths in the same form.
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let inside = canonical(worktree);
    // The box resolves the harness on its own PATH.
    if let Some(name) = Path::new(&argv[0]).file_name() {
        argv[0] = name.to_string_lossy().into_owned();
    }
    let outside = |a: &str| {
        let p = Path::new(a);
        p.is_absolute() && p.exists() && !canonical(p).starts_with(&inside)
    };
    // The seam's host write grants (`--add-dir ~/.fno`) name roots the box
    // does not mount; inside the box they grant nothing, so drop them.
    let mut kept = vec![argv[0].clone()];
    let mut rest = argv[1..].iter();
    while let Some(a) = rest.next() {
        if a == "--add-dir" {
            match rest.next() {
                Some(dir) if outside(dir) => continue,
                Some(dir) => kept.extend([a.clone(), dir.clone()]),
                None => kept.push(a.clone()),
            }
        } else if a.strip_prefix("--add-dir=").is_some_and(|dir| outside(dir)) {
            continue;
        } else {
            kept.push(a.clone());
        }
    }
    let argv = kept;
    if let Some(host) = argv[1..].iter().find(|a| outside(a)) {
        return Err(format!(
            "the command names {host}, a host path outside the worktree the box cannot see"
        ));
    }
    let mut out = exec_prefix(launch, worktree);
    out.push("--".to_string());
    if !env.is_empty() {
        let sock = launch
            .mounts
            .iter()
            .find(|m| m.container == Path::new(CONTAINER_SOCK_DIR))
            .ok_or("the plan mounts no socket directory to carry the lane env")?;
        let name = env_file_name();
        write_env_file(&sock.host.join(&name), env)
            .map_err(|e| format!("could not write the lane env file: {e}"))?;
        out.extend([
            "/bin/sh".to_string(),
            "-c".to_string(),
            ". \"$0\" && rm -f \"$0\" && exec \"$@\"".to_string(),
            format!("{CONTAINER_SOCK_DIR}/{name}"),
        ]);
    }
    out.extend(argv);
    Ok(out)
}

fn env_file_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("env-{}-{nanos}", std::process::id())
}

/// `export K='V'` per set pair, single-quoted for sh, and `unset K` per
/// removal. A key sh cannot name is skipped.
fn write_env_file(path: &Path, env: &[(String, Option<String>)]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut body = String::new();
    for (k, v) in env {
        let named = k
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            && k.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
        // PWD names the host checkout; the box runs in its own workspace.
        match v {
            _ if !named || k == "PWD" => {}
            Some(v) => body.push_str(&format!("export {k}='{}'\n", v.replace('\'', "'\\''"))),
            None => body.push_str(&format!("unset {k}\n")),
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wrap's shapes: a QoS prefix, an absolute harness path and a host
    /// `--add-dir` grant drop; the lane env rides a 0600 file the box sources,
    /// never the argv; any other host path outside the worktree refuses by name.
    #[test]
    fn box_argv_wraps_env_through_a_file_and_refuses_host_paths() {
        let root = std::env::temp_dir().join(format!("sandbox-launch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sock_dir = root.join("sandbox-sock");
        let wt = root.join("wt");
        std::fs::create_dir_all(&sock_dir).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        let launch = launch_for(SandboxProvider::Devcontainer, &sock_dir);
        let (root_s, wt_s) = (root.display().to_string(), wt.display().to_string());
        let argv = strip_qos(
            [
                "/usr/sbin/taskpolicy",
                "-c",
                "utility",
                "--",
                "/opt/bin/claude",
                "--add-dir",
                root_s.as_str(),
                "--add-dir",
                wt_s.as_str(),
                "-p",
                "hi",
            ]
            .map(String::from)
            .to_vec(),
        );
        let env = [
            ("ROUTE_KEY".to_string(), Some("s3'cret".to_string())),
            ("STALE_MODEL".to_string(), None),
        ];
        let out = box_argv(&launch, &wt, argv, &env).unwrap();
        let joined = out.join(" ");
        assert!(joined.starts_with(&format!(
            "devcontainer exec --workspace-folder {} --remote-env FNO_STATE_DIR=/fno-state \
             --remote-env FNO_SUPERVISOR_SOCKET=/fno-state/sock/supervisor.sock -- /bin/sh -c",
            wt.display()
        )));
        assert!(
            joined.ends_with(&format!("claude --add-dir {wt_s} -p hi")),
            "a host write grant drops, a worktree one stays: {joined}"
        );
        assert!(
            !joined.contains("s3"),
            "a secret never rides the argv: {joined}"
        );
        let file = std::fs::read_dir(&sock_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let body = std::fs::read_to_string(file.path()).unwrap();
        assert_eq!(body, "export ROUTE_KEY='s3'\\''cret'\nunset STALE_MODEL\n");
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);

        let host = root.join("settings.json");
        std::fs::write(&host, "{}").unwrap();
        let argv = vec![
            "claude".to_string(),
            "--settings".into(),
            host.display().to_string(),
        ];
        let err = box_argv(&launch, &wt, argv, &[]).unwrap_err();
        assert!(err.contains(&host.display().to_string()), "{err}");
    }
}

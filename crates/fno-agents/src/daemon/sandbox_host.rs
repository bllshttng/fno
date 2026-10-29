//! The daemon side of the sandbox seam: the `agent.sandbox-plan` op, the
//! socket directory a box mounts, and the `socket` carrier probe the spawn
//! gate runs. Split out of daemon.rs (shrink-only) so sandbox-side additions
//! land here, never back in the daemon file. The plan types and the launch
//! wrap live in `crate::sandbox_launch`, where the spawn client reaches them.

use super::*;

use crate::sandbox_launch::{launch_for, SandboxProvider};

/// The carrier name a sandboxed lane declares in
/// `[harness.<harness>.state_root_grant]`: the lane reaches the state root
/// through the daemon socket, so it needs no host state-root grant. The
/// spawn gate probes the socket before believing the declaration.
pub const SOCKET_CARRIER: &str = "socket";

/// Re-link `<home>/sandbox-sock/supervisor.sock` to the socket the daemon
/// just bound. A box mounts this directory, never the socket file: the
/// daemon unlinks and rebinds its socket at every start, and a file mount
/// keeps the dead inode (ECONNREFUSED after the first restart). A directory
/// mount sees the fresh link. A socket path cannot be rebound in place, and
/// the home directory holds the registry and claims, so a hard link in a
/// directory of its own is the one shape that survives a restart. A failure
/// logs one line; the gate probe and the in-box probe name the dead path.
pub(crate) fn publish_sandbox_sock(home: &AgentsHome) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = home.sandbox_sock_dir();
    let link = dir.join("supervisor.sock");
    let res = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)))
        .and_then(|()| match std::fs::remove_file(&link) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        })
        .and_then(|()| std::fs::hard_link(home.supervisor_sock(), &link));
    if let Err(e) = res {
        eprintln!(
            "fno-agents daemon: sandbox socket link {}: {e}",
            link.display()
        );
    }
}

/// Whether the supervisor socket at `path` answers a connect. The gate's
/// `socket` carrier check: a lane that declares the carrier must find a
/// listener, or the declaration is hiding a worker that can claim, mail, or
/// spawn nothing - the exact mute-worker harm R3 exists for.
///
/// The connect is bounded at 500ms: `UnixStream::connect` has no
/// connect_timeout, and a wedged listener (full backlog) would hang every
/// spawn whose carrier is `socket`. The probe rides one detached thread; a
/// timed-out probe leaves that thread blocked in connect (bounded by the
/// kernel backlog, not by us), while the caller moves on inside 500ms.
pub fn supervisor_sock_reachable(path: &Path) -> Result<(), String> {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    let p = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(&p).map(|_| ()));
    });
    match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("{}: {e}", path.display())),
        Err(_) => Err(format!(
            "{}: connect timed out after 500ms (listener wedged?)",
            path.display()
        )),
    }
}

/// The `socket` carrier leg of the spawn gate's stance check: the only
/// stance that is verified against the world instead of taken as a
/// declaration. Any other stance (or no stance) passes; the missing-stance
/// refusal lives in the gate. `sock` absent means no path to probe, which
/// passes - the same fail-open the empty-roots leg takes.
pub(crate) fn socket_carrier_ok(stance: Option<&str>, sock: Option<&Path>) -> Result<(), String> {
    if stance != Some(SOCKET_CARRIER) {
        return Ok(());
    }
    match sock {
        Some(sock) => supervisor_sock_reachable(sock),
        None => Ok(()),
    }
}

/// `agent.sandbox-plan` - answer the launch plan for one worktree: the
/// configured provider, its mounts, and the env the container-side `fno`
/// reads. The spawn lane (or the operator composing a devcontainer.json)
/// consumes the JSON; the daemon owns the socket paths, so the plan is the
/// one authority a container needs.
pub(super) fn handle_sandbox_plan(ctx: &Ctx, req: &Request) -> Response {
    let worktree = match req.params.get("worktree").and_then(|v| v.as_str()) {
        Some(w) => PathBuf::from(w),
        None => {
            return Response::err(req.id, ErrorCode::InvalidParams, "missing `worktree`");
        }
    };
    let provider = match SandboxProvider::from_config(&worktree) {
        Ok(p) => p,
        Err(e) => return Response::err(req.id, ErrorCode::InvalidParams, e),
    };
    let launch = launch_for(provider, &ctx.home.sandbox_sock_dir());
    Response::ok(
        req.id,
        serde_json::to_value(&launch).unwrap_or_else(|_| json!({"provider": "none"})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sandbox-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The op reads the provider from the worktree config. Absent answers the
    /// empty plan; an unknown spelling refuses; `devcontainer` mounts the
    /// socket directory, never the socket file or the agents home, and pins
    /// the client to the link inside it (AC2).
    #[test]
    fn sandbox_plan_reads_the_provider_and_mounts_the_socket_dir() {
        let worktree = tempdir("op");
        std::fs::create_dir_all(worktree.join(".fno")).unwrap();
        let home = crate::paths::AgentsHome::at(tempdir("op-home"));
        let ctx = super::super::tests::test_ctx(home.clone(), PathBuf::from("fno-agents-worker"));
        let req = Request::new(
            1,
            format!("agent.{}", crate::sandbox_launch::SANDBOX_PLAN_COMMAND),
            json!({ "worktree": worktree.to_string_lossy() }),
        );
        let config = worktree.join(".fno/config.toml");
        let cases = [
            (
                "",
                Some(json!({"provider": "none", "mounts": [], "env": {}})),
            ),
            ("[sandbox]\nprovider = \"pod\"\n", None),
            (
                "[sandbox]\nprovider = \"devcontainer\"\n",
                Some(json!({
                    "provider": "devcontainer",
                    "mounts": [{
                        "host": home.sandbox_sock_dir().to_string_lossy(),
                        "container": "/fno-state/sock",
                    }],
                    "env": {
                        "FNO_STATE_DIR": "/fno-state",
                        "FNO_SUPERVISOR_SOCKET": "/fno-state/sock/supervisor.sock",
                    },
                })),
            ),
        ];
        for (body, want) in cases {
            std::fs::write(&config, body).unwrap();
            let res = handle_sandbox_plan(&ctx, &req);
            match want {
                Some(want) => assert_eq!(res.result().expect("ok"), &want, "{body}"),
                None => assert!(res.result().is_none(), "{body} refuses"),
            }
        }
    }

    /// The socket door a box uses, end to end.
    /// - The `socket` carrier is probed, never taken on faith: a dead path
    ///   refuses naming the socket, a live listener passes, another stance
    ///   is never probed, and no path fails open like empty roots (AC8).
    /// - A daemon restart must not strand the box: the stale link refuses,
    ///   and the publish at every bind points it at the new socket (AC1).
    /// - A pinned client never forks a daemon; it fails as not running, and
    ///   the home path serves again once the pin is gone (AC12).
    #[test]
    fn socket_door_probes_relinks_and_never_forks_under_a_pin() {
        use std::os::unix::fs::MetadataExt as _;
        let home = crate::paths::AgentsHome::at(tempdir("door"));
        let sock = home.supervisor_sock();
        let link = home.sandbox_sock_dir().join("supervisor.sock");
        let err = socket_carrier_ok(Some("socket"), Some(&sock))
            .err()
            .expect("dead socket refuses");
        assert!(
            err.starts_with(&sock.display().to_string()),
            "the refusal names the socket path: {err}"
        );
        assert!(socket_carrier_ok(Some("socket"), None).is_ok());

        let first = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(socket_carrier_ok(Some("socket"), Some(&sock)).is_ok());
        assert!(socket_carrier_ok(Some("unsandboxed"), Some(&link)).is_ok());
        assert!(socket_carrier_ok(None, Some(&link)).is_ok());
        publish_sandbox_sock(&home);
        assert!(supervisor_sock_reachable(&link).is_ok());
        drop(first);
        std::fs::remove_file(&sock).unwrap();
        let _second = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(
            supervisor_sock_reachable(&link).is_err(),
            "the old link holds the dead inode"
        );
        publish_sandbox_sock(&home);
        assert_eq!(
            std::fs::metadata(&link).unwrap().ino(),
            std::fs::metadata(&sock).unwrap().ino()
        );
        assert!(supervisor_sock_reachable(&link).is_ok());

        // A stale socket file at the pin path answers nobody.
        let pin = tempdir("pin-sock").join("supervisor.sock");
        std::fs::write(&pin, b"").unwrap();
        let req = Request::new(1, "agent.status", json!({}));
        // SAFETY: test-process env; only the pin readers read this var and
        // the test restores the unpinned world before it ends.
        std::env::set_var("FNO_SUPERVISOR_SOCKET", &pin);
        let res = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(crate::client::call(
                &home,
                Path::new("/nonexistent/fno-agents"), // a fork would fail here, not with DaemonNotRunning
                &req,
            ));
        std::env::remove_var("FNO_SUPERVISOR_SOCKET");
        assert!(
            matches!(res, Err(crate::client::ClientError::DaemonNotRunning)),
            "got {res:?}"
        );
        assert_eq!(crate::client::client_sock(&home), home.supervisor_sock());
    }
}

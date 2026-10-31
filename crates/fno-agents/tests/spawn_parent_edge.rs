//! A daemon spawn request names the session that typed it: the seam clears the
//! caller's markers from the client, and the edge survives on the wire.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use fno_agents::protocol::{read_request, write_response, ErrorCode, Response};
use serde_json::Value;

/// Answers every request with a refusal and forwards the spawn's params.
fn stub_daemon(sock: PathBuf) -> mpsc::Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).unwrap();
            loop {
                let (mut conn, _) = listener.accept().await.unwrap();
                let Ok(req) = read_request(&mut conn).await else {
                    continue;
                };
                if req.method == "agent.spawn" {
                    let _ = tx.send(req.params.clone());
                }
                let refused = Response::err(req.id, ErrorCode::InvalidParams, "stub daemon");
                let _ = write_response(&mut conn, &refused).await;
            }
        });
    });
    rx
}

#[test]
fn a_thread_spawn_request_carries_the_callers_session_and_harness() {
    let home = PathBuf::from(format!("/tmp/fno-edge-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let spawns = stub_daemon(home.join("supervisor.sock"));

    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["spawn", "edge-probe", "-H", "codex", "--as", "thread"])
        .args(["--message", "say hi"])
        .current_dir(&home)
        .env_clear()
        .envs(fno_agents::test_run::self_owner_env())
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", &home)
        .env("FNO_AGENTS_HOME", &home)
        .env("FNO_HOME", &home)
        .env("FNO_HARNESS_NAME", "opencode")
        .env("FNO_HARNESS_SESSION_ID", "ses_parent")
        .env("OPENCODE_SESSION_ID", "ses_parent")
        .output()
        .unwrap();

    let params = spawns
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| {
            panic!(
                "no agent.spawn reached the daemon: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    std::fs::remove_dir_all(&home).ok();
    assert_eq!(params["spawned_by_session"], "ses_parent", "{params}");
    assert_eq!(params["spawned_by_harness"], "opencode", "{params}");
}

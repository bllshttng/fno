//! parity-stage: characterization
//! parity-oracle: fno.agents.cli._watch_loop
//!
//! The watch command, frozen against goldens captured from its Python leg
//! before that leg was deleted. Each case
//! seeds the registry rows the capture recorded, serves the recorded worker
//! replies from a fake worker socket (one reply per connection, in order), and
//! compares exit code, stdout and stderr byte for byte.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHORT: &str = "w8a1";

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/watch")
}

fn golden(case: &str, ext: &str) -> String {
    let path = golden_dir().join(format!("{case}.{ext}"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Answer one recorded reply per connection, then stop listening.
fn serve(sock: &Path, replies: Vec<serde_json::Value>) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(sock).unwrap();
    std::thread::spawn(move || {
        for reply in replies {
            let (mut conn, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            conn.read_exact(&mut len).unwrap();
            let mut request = vec![0u8; u32::from_le_bytes(len) as usize];
            conn.read_exact(&mut request).unwrap();
            let body = reply.to_string().into_bytes();
            conn.write_all(&(body.len() as u32).to_le_bytes()).unwrap();
            conn.write_all(&body).unwrap();
        }
    })
}

fn run(root: &Path, case: &str, argv: &[&str]) -> (i32, String, String) {
    let home = root.join(case).join("agents");
    std::fs::create_dir_all(&home).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&golden(case, "rows")).unwrap();
    if !rows.is_null() {
        let doc = serde_json::json!({"schema_version": 43, "agents": rows});
        std::fs::write(home.join("registry.json"), doc.to_string()).unwrap();
    }
    let server = golden_dir()
        .join(format!("{case}.replies"))
        .exists()
        .then(|| {
            let replies: Vec<serde_json::Value> =
                serde_json::from_str(&golden(case, "replies")).unwrap();
            std::fs::create_dir_all(home.join(SHORT)).unwrap();
            serve(&home.join(SHORT).join("worker.sock"), replies)
        });
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .env("FNO_AGENTS_HOME", &home)
        .env("FNO_HOME", root.join("state"))
        .env("FNO_STATE_DIR", root.join("state"))
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(root)
        .args(["status", "--watch"])
        .args(argv)
        .output()
        .expect("command ran");
    if let Some(server) = server {
        server.join().unwrap();
    }
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn watch_matches_the_python_goldens() {
    // /tmp, not the default temp dir: a unix socket path is capped near 104 bytes.
    let dir = tempfile::Builder::new()
        .prefix("wr-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::write(dir.path().join("state/.path-migration-done"), "\n").unwrap();
    let cases: [(&str, &[&str]); 7] = [
        ("unknown-name", &["nobody"]),
        ("empty-token", &[""]),
        ("no-short-id", &["alpha"]),
        ("unreachable", &["alpha"]),
        ("lifecycle", &["alpha", "-i", "0"]),
        ("by-short-id", &[SHORT, "--interval", "0"]),
        ("error-reply", &["alpha", "-i", "0"]),
    ];
    for (case, argv) in cases {
        let (code, out, err) = run(dir.path(), case, argv);
        let want_code: i32 = golden(case, "exit").trim().parse().unwrap();
        assert_eq!(code, want_code, "{case}: exit\nstderr={err}");
        assert_eq!(out, golden(case, "out"), "{case}: stdout");
        assert_eq!(err, golden(case, "err"), "{case}: stderr");
    }
}

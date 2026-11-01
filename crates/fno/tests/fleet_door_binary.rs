//! The fleet spellings exec `fno-agents`, never `fno-agents-worker`. The worker
//! answers any of these argvs with its "pass a lane" usage and exit 2, and a
//! caller that fails closed on that exit reads it as a real state: the eval
//! sweep read it as "paused" and stopped running.
//!
//! The front runs as a subprocess with `env_clear`, so no test shares env. Both
//! binaries are stubs: the worker stub proves it was not picked, and the record
//! proves which argv the right binary received.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn stub(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn fleet_doors_exec_fno_agents_with_their_action_words() {
    let dir = std::env::temp_dir().join(format!("fno-fleet-door-binary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let agents = dir.join("fno-agents-stub");
    let worker = dir.join("fno-agents-worker");
    let record = dir.join("argv");
    stub(&agents, "#!/bin/sh\nprintf '%s' \"$*\" > \"$RECORD\"\n");
    stub(&worker, "#!/bin/sh\necho 'pass a lane' >&2\nexit 2\n");

    let cases: [(&[&str], &str); 4] = [
        (&["do", "loops", "status"], "loops status"),
        (&["loops", "status"], "loops status"),
        (&["agents", "yard"], "census yard"),
        (
            &["agents", "autonomy", "status"],
            "spawn-overlay autonomy status",
        ),
    ];
    for (args, want) in cases {
        let _ = std::fs::remove_file(&record);
        let out = Command::new(env!("CARGO_BIN_EXE_fno"))
            .args(args)
            .env_clear()
            .env("HOME", &dir)
            .env("FNO_AGENTS_BIN", &agents)
            .env("FNO_AGENTS_WORKER", &worker)
            .env("RECORD", &record)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {:?} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(std::fs::read_to_string(&record).unwrap(), want, "{args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

//! The fleet sidecar must keep refreshing on a machine with no attached
//! client. The beat ages the file out at 300s (`MAX_AGE_S`), and before the
//! server timer only a client render carried the write, so an unattended
//! machine stopped publishing and far rows fell back to project grouping.

mod common;
use common::{spawn_server, Scratch};

use std::time::{Duration, Instant};

#[test]
fn an_unattended_server_keeps_refreshing_the_fleet_sidecar() {
    let scratch = Scratch::new("workspace_sidecar_unattended");
    let sock = scratch.0.join("work.sock");
    // A cadence the test can watch in seconds; production keeps the 30s
    // default (the same seam shape as FNO_IDLE_EXIT_GRACE_MS).
    let _server = spawn_server(&sock, &[("FNO_SIDECAR_EVERY_MS", "300")]);
    let sidecar = scratch
        .0
        .join("iso-agents")
        .join("fleet")
        .join("workspaces.json");
    // Two writes: the file appears and then is REWRITTEN. One write could be
    // the old client-render path's last gasp; a refresh is the contract.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut first: Option<std::time::SystemTime> = None;
    loop {
        if let Ok(meta) = std::fs::metadata(&sidecar) {
            let mtime = meta.modified().unwrap();
            match first {
                None => first = Some(mtime),
                Some(prev) if mtime > prev => break,
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "sidecar never refreshed with no client attached: {:?}",
            first
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

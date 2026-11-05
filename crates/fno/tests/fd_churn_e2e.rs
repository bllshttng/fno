//! Descriptor accounting under churn, and the wedged-keeper close. These
//! tests pin the contracts that keep the server's fd table flat: a close
//! must release the pane's descriptors even when the keeper wedges, and
//! connection plus pane churn must leave no residue.

mod common;

use common::{connect_with_retry, spawn_server, FakeClient, Scratch};
use std::path::Path;
use std::time::{Duration, Instant};

/// The server's open descriptor count. `lsof -p` on macOS; `/proc/<pid>/fd`
/// entry count on Linux, so CI runners without lsof still count.
fn server_fds(pid: u32) -> usize {
    if cfg!(target_os = "linux") {
        let dir = Path::new("/proc").join(pid.to_string()).join("fd");
        std::fs::read_dir(&dir)
            .expect("procfs fd dir exists")
            .count()
    } else {
        let out = std::process::Command::new("lsof")
            .args(["-p", &pid.to_string()])
            .output()
            .expect("lsof runs");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .count()
            .saturating_sub(1)
    }
}

/// Wait until the predicate holds or the budget dies; returns the final fd
/// count so the assertion can name the settled number, not a race.
fn wait_fds_eq(pid: u32, budget: Duration, pred: impl Fn(usize) -> bool) -> usize {
    let deadline = Instant::now() + budget;
    let mut last = server_fds(pid);
    while Instant::now() < deadline {
        last = server_fds(pid);
        if pred(last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

fn run_pane(scratch: &Scratch) -> u64 {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fno"))
        .args([
            "mux",
            "pane",
            "run",
            "--server",
            "main",
            "/bin/zsh",
            "-c",
            "sleep 600",
        ])
        .env("FNO_MUX_DIR", &scratch.0)
        .env("FNO_AGENTS_HOME", &scratch.0.join("iso-agents"))
        .output()
        .expect("pane run runs");
    assert!(
        out.status.success(),
        "pane run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .expect("pane run prints the pane id")
}

fn pane_rows(scratch: &Scratch) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(["mux", "pane", "ls", "--server", "main"])
        .env("FNO_MUX_DIR", &scratch.0)
        .env("FNO_AGENTS_HOME", &scratch.0.join("iso-agents"))
        .output()
        .expect("pane ls runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn pane_pid(row: &str) -> Option<u32> {
    row.split_whitespace()
        .find_map(|tok| tok.strip_prefix("pid="))
        .and_then(|v| v.parse().ok())
}

#[test]
fn fd_table_stays_flat_under_churn_and_a_wedged_keeper_close() {
    let scratch = Scratch::new("fd-churn");
    let sock = scratch.main_sock();
    let server = spawn_server(&sock, &[]);
    let pid = server.0.id();
    connect_with_retry(&sock);

    // One attached viewer for the whole test: a clientless E2E server idles
    // out after its grace, and a viewer keeps output flowing as in real use.
    let viewer = FakeClient::attach(&sock, 30, 100, &scratch.home_cwd());

    let baseline = server_fds(pid);

    // Leg 1 - connection churn: abrupt deaths and clean detaches must
    // release every per-connection descriptor.
    for _ in 0..12 {
        let mut c = FakeClient::attach(&sock, 30, 100, &scratch.home_cwd());
        c.detach();
        drop(c);
    }
    let settled = wait_fds_eq(pid, Duration::from_secs(3), |n| n <= baseline + 2);
    assert!(
        settled <= baseline + 2,
        "connection churn leaked fds: baseline {baseline}, settled {settled}"
    );

    // Leg 2 - pane churn: keeper-hosted spawn and deliberate close; both
    // descriptors must come back.
    for _ in 0..3 {
        let pane = run_pane(&scratch);
        let rows = pane_rows(&scratch);
        assert!(
            rows.iter().any(|r| r.starts_with(&pane.to_string())),
            "pane {pane} never registered: {rows:?}"
        );
        let kill = std::process::Command::new(env!("CARGO_BIN_EXE_fno"))
            .args(["mux", "pane", "kill", "--server", "main", &pane.to_string()])
            .env("FNO_MUX_DIR", &scratch.0)
            .env("FNO_AGENTS_HOME", &scratch.0.join("iso-agents"))
            .output()
            .expect("pane kill runs");
        assert!(
            kill.status.success(),
            "pane kill failed: {}",
            String::from_utf8_lossy(&kill.stderr)
        );
        let gone_by = Instant::now() + Duration::from_secs(5);
        while Instant::now() < gone_by
            && pane_rows(&scratch)
                .iter()
                .any(|r| r.starts_with(&pane.to_string()))
        {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            !pane_rows(&scratch)
                .iter()
                .any(|r| r.starts_with(&pane.to_string())),
            "pane {pane} never closed"
        );
    }
    let settled = wait_fds_eq(pid, Duration::from_secs(3), |n| n <= baseline + 2);
    assert!(
        settled <= baseline + 2,
        "pane churn leaked fds: baseline {baseline}, settled {settled}"
    );
    // Leg 3 - a frozen (SIGSTOP) keeper cannot process the Kill frame. The
    // close must still free the pane's descriptors: after the 2s grace the
    // Drop shutdown frees the reader without needing the keeper's exit.
    let pane = run_pane(&scratch);
    let row = pane_rows(&scratch)
        .iter()
        .find(|r| r.starts_with(&pane.to_string()))
        .expect("wedged leg pane registered")
        .clone();
    let child = pane_pid(&row).expect("pane row carries the child pid");
    let keeper_pid_out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &child.to_string()])
        .output()
        .expect("ps runs");
    let keeper_pid: u32 = String::from_utf8_lossy(&keeper_pid_out.stdout)
        .trim()
        .parse()
        .expect("the keeper is the child's parent");

    std::process::Command::new("kill")
        .args(["-STOP", &keeper_pid.to_string()])
        .output()
        .expect("SIGSTOP runs");
    let stop_time = Instant::now();
    let kill = std::process::Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(["mux", "pane", "kill", "--server", "main", &pane.to_string()])
        .env("FNO_MUX_DIR", &scratch.0)
        .env(
            "FNO_MUX_ADMISSION_NAMESPACE",
            scratch.0.file_name().unwrap_or_default(),
        )
        .env("FNO_AGENTS_HOME", &scratch.0.join("iso-agents"))
        .output()
        .expect("pane kill runs");
    assert!(
        kill.status.success(),
        "a wedged keeper must not wedge the close: {}",
        String::from_utf8_lossy(&kill.stderr)
    );

    // Release the keeper so it processes the buffered Kill frame on resume,
    // closes out the seat and its child; the server-side reader is already
    // free via the Drop shutdown.
    std::process::Command::new("kill")
        .args(["-CONT", &keeper_pid.to_string()])
        .output()
        .expect("SIGCONT runs");

    let settled = wait_fds_eq(pid, Duration::from_secs(5), |n| n <= baseline + 2);
    assert!(
        settled <= baseline + 2,
        "wedged-keeper close leaked fds: baseline {baseline}, settled {settled}"
    );
    let elapsed = stop_time.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1900),
        "the Kill grace should have run: {elapsed:?}"
    );

    drop(viewer);
    server.terminate_and_wait();
}

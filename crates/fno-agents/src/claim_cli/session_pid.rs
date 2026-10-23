//! The `claim session-pid` leaf, ported verbatim from
//! `claim_commands.rs::run_claim_session_pid` (wave 7 of the leaf port): the
//! one resolver of the durable pid arm. The Python side (`session_pid.py`,
//! the init hook) reads THIS command; `durable_session_pid` calls the same
//! resolution in-process, so there is one producer and no subprocess behind
//! the mutex. Contract matches the retired Python command byte for byte: the
//! pid on stdout, nothing at all when uncapturable, always exit 0 - so
//! `$(fno agents claim session-pid)` stays the empty string. `--json`
//! answers both halves in one read.

pub fn run(args: &[String]) -> i32 {
    let mut from_pid: Option<u32> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--from-pid" => match it.next().and_then(|v| v.parse::<u32>().ok()) {
                Some(pid) => from_pid = Some(pid),
                None => return 2,
            },
            "--json" | "-J" => json = true,
            other => {
                eprintln!("fno-agents: claim session-pid: unknown flag {other}");
                return 2;
            }
        }
    }
    let start = from_pid.unwrap_or_else(|| unsafe { libc::getppid() } as u32);
    let (pid, harness) = crate::spawn_context::session_identity_ambient(start);
    if json {
        println!(
            "{}",
            serde_json::json!({"session_pid": pid, "harness": harness})
        );
    } else if let Some(pid) = pid {
        println!("{pid}");
    }
    // Uncapturable prints nothing: the caller degrades to TTL-only liveness.
    0
}

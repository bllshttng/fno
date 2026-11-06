//! `fno doctor -h` answers the family usage natively and exits 0.
//!
//! Before the Help claim, the door forwarded help to the Python CLI, whose
//! signpost handed the same argv straight back; the pair spawned one waiting
//! Python per cycle and chained until someone killed it. This pin fails both
//! ways a regression can: a forwarded argv cannot exit 0 with the usage on
//! stdout, and a reintroduced loop hangs instead of answering.

use std::process::{Command, Stdio};

fn doctor_help_exit_zero(spelling: &str) {
    let exe = env!("CARGO_BIN_EXE_fno");
    let out = Command::new(exe)
        .args(["doctor", spelling])
        .stdin(Stdio::null())
        .output()
        .expect("fno doctor help runs");
    assert_eq!(out.status.code(), Some(0), "{spelling} must exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("fno doctor [leaf] [flags]"),
        "{spelling} must print the family usage, got: {text}"
    );
}

#[test]
fn short_help_answers_native() {
    doctor_help_exit_zero("-h");
}

#[test]
fn long_help_answers_native() {
    doctor_help_exit_zero("--help");
}

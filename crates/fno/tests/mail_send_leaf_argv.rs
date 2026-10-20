//! `fno agents mail send <id> <body>` execs the sibling leaf with the send
//! word intact. The classifier hands `Role::MailSend` the tail WITHOUT the
//! verb, so the exec prefix must re-add it: dropping it made an 8-hex or
//! UUID target read as the leaf's verb ("unknown verb <id>", exit 2) while
//! plain names, which route through the Python leg, kept working. The
//! machine-mail door (`fno backlog note` notify, events push) rides the
//! same front leg with a flag-led send. The pin reads the argv a stub leaf
//! actually receives, so it fails if the claim OR the reconstruction
//! drifts.
//!
//! The front is driven as a SUBPROCESS with `env_clear`, mirroring
//! squad_prune.rs: no cross-test env races, and `FNO_AGENTS_BIN` points the
//! paired-binary lookup at the stub.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

/// A stub fno-agents that appends its argv, one word per line, to
/// `$LEAF_ARGV_RECORD` and exits 0, so the test reads the leaf-side argv
/// the front built.
struct LeafStub {
    dir: PathBuf,
}

impl LeafStub {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fno-mail-send-leaf-argv-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("fno-agents-stub");
        std::fs::write(
            &stub,
            r#"#!/bin/sh
: > "$LEAF_ARGV_RECORD"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$LEAF_ARGV_RECORD"
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        LeafStub { dir }
    }

    /// Run the real front binary with the stub as the sibling leaf. Returns
    /// the argv the leaf received plus the front's stderr for failure text.
    fn leaf_argv(&self, args: &[&str]) -> (Vec<String>, String) {
        let record = self.dir.join("argv.txt");
        let _ = std::fs::remove_file(&record);
        let out = Command::new(env!("CARGO_BIN_EXE_fno"))
            .args(args)
            .env_clear()
            .env("HOME", &self.dir)
            .env("FNO_AGENTS_BIN", self.dir.join("fno-agents-stub"))
            .env("LEAF_ARGV_RECORD", &record)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let recorded = std::fs::read_to_string(&record)
            .unwrap_or_else(|_| format!("<stub never ran> front stderr: {stderr}"));
        (recorded.lines().map(String::from).collect(), stderr)
    }
}

impl Drop for LeafStub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An 8-hex short id reaches the leaf as the target of `mail send`, not as
/// the verb: the recorded argv opens with the `mail send` pair.
#[test]
fn hex_target_reaches_leaf_with_send_intact() {
    let s = LeafStub::new("hex");
    let (argv, stderr) = s.leaf_argv(&["agents", "mail", "send", "abcdef12", "probe"]);
    assert_eq!(
        argv,
        vec!["mail", "send", "abcdef12", "probe"],
        "leaf argv must open with the send pair: {stderr}"
    );
}

/// A dashed full UUID rides the same native leg with a byte-verbatim tail:
/// flags after the body pass through untouched.
#[test]
fn uuid_target_keeps_the_tail_verbatim() {
    let s = LeafStub::new("uuid");
    let (argv, stderr) = s.leaf_argv(&[
        "mail",
        "send",
        "0000dead-0000-4000-8000-000000000000",
        "probe",
        "--from-self",
    ]);
    assert_eq!(
        argv,
        vec![
            "mail",
            "send",
            "0000dead-0000-4000-8000-000000000000",
            "probe",
            "--from-self"
        ],
        "leaf argv must carry the tail untouched: {stderr}"
    );
}

/// A plain-name target never takes the native leg: it stays the Python
/// leg's, so the stub must not run. This keeps the two legs from merging -
/// the fix that re-added `send` must not swallow the relay's claim.
#[test]
fn plain_name_target_stays_off_the_native_leg() {
    let s = LeafStub::new("name");
    let record = s.dir.join("argv.txt");
    let _ = Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(["agents", "mail", "send", "zz-no-such-agent", "probe"])
        .env_clear()
        .env("HOME", &s.dir)
        .env("FNO_AGENTS_BIN", s.dir.join("fno-agents-stub"))
        .env("LEAF_ARGV_RECORD", &record)
        .output()
        .unwrap();
    assert!(
        !record.exists(),
        "a plain-name target must not reach the native leaf"
    );
}

/// The machine-mail door (`fno backlog note` notify, events push) execs the
/// front with a flag-led send: `agents mail send --from-name <sender> --
/// <recipient> <body>`. The flag-led target claims the native leg, so the
/// reconstruction must still open with the send pair; dropping it made the
/// leaf read `--from-name` as the verb.
#[test]
fn flag_led_machine_mail_send_reconstructs_the_send_pair() {
    let s = LeafStub::new("flagled");
    let (argv, stderr) = s.leaf_argv(&[
        "agents",
        "mail",
        "send",
        "--from-name",
        "note-pointer",
        "--",
        "abcdef12",
        "probe",
    ]);
    assert_eq!(
        argv,
        vec![
            "mail",
            "send",
            "--from-name",
            "note-pointer",
            "--",
            "abcdef12",
            "probe"
        ],
        "flag-led send argv must open with the send pair: {stderr}"
    );
}

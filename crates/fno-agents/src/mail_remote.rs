//! `mail-remote`: the cross-host relay verb.
//!
//! The front door claims an `@host` target on `agents mail send` and
//! `agents peek` and execs this verb on the sibling `fno-agents` binary. The
//! store path writes one bus row (see [`crate::mail_bus`]); each host's
//! daemon pull arm hands the payload to the receiver's own local send, so
//! lane selection, holds, and receipts stay the local verb's. When no
//! `store.remote_url` is configured, send and peek relay over ssh: the
//! remote machine's own `fno` answers, and its exit code and receipt pass
//! through byte-verbatim.

use crate::mail_bus;
use crate::store_remote::{self, Remote};
use std::ffi::OsString;
use std::process::Command;

/// The state-root files this module owns: the sender's local queue when the
/// primary is unreachable, and the receiver's injected-id ledger.
pub fn outbox_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("mail_outbox.jsonl")
}

pub fn seen_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("mail_seen.jsonl")
}

/// What `split_target` refused.
pub fn target_error(target: &str) -> String {
    format!(
        "cross-host target {target:?}: the form is <name>@<host>, one @, \
both sides nonempty; a host alias in ssh_config covers user@host spellings"
    )
}

/// Split `<name>@<host>` at its single `@`. Both sides nonempty; two `@`
/// refuse (an ssh user@host spelling is not carried; an ssh_config Host
/// alias covers those).
pub fn split_target(target: &str) -> Result<(String, String), String> {
    let at_count = target.matches('@').count();
    if at_count != 1 {
        return Err(target_error(target));
    }
    let (name, host) = target.split_once('@').unwrap_or(("", ""));
    if name.is_empty() || host.is_empty() {
        return Err(target_error(target));
    }
    Ok((name.to_string(), host.to_string()))
}

/// Quote one word for the remote sh that parses the line ssh hands it: ssh
/// joins argv with spaces before the remote shell reads it, so every word
/// with anything but plain characters rides inside single quotes. Message
/// text is never interpolated into a command string - it rides as a quoted
/// positional argument.
pub fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The ssh relay command: the remote login shell resolves `fno` via its
/// profile (the cargo bin dir), then execs the verb with the words as
/// positional arguments. `$0` is `sh`, so `"$@"` is the verb tail. Every
/// element rides sh-quoted, including the script itself: ssh joins argv
/// with spaces and the remote shell parses the joined line.
pub fn ssh_argv(host: &str, words: &[String]) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_string(),
        host.to_string(),
        "--".to_string(),
        "sh".to_string(),
        "-lc".to_string(),
        sh_quote("exec fno \"$@\""),
        "sh".to_string(),
    ];
    argv.extend(words.iter().map(|w| sh_quote(w)));
    argv
}

/// Which identity a send stamps as `from_id`'s name part.
#[derive(Debug, PartialEq, Eq)]
pub enum FromName {
    /// `--from-name <value>` won.
    Given(String),
    /// `--from-self`: the ambient session's own name (FNO_AGENT_SELF).
    SelfName(String),
    /// Neither flag: the OS user.
    OsUser(String),
}

/// Read the passthrough flags a send carries. The tail rides the relay (or
/// the bus row) verbatim; only identity and raw-ness are lifted out.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct SendTail {
    pub from_name: Option<String>,
    pub from_self: bool,
    pub raw: bool,
    pub rest: Vec<String>,
}

pub fn parse_send_tail(args: &[String]) -> Result<SendTail, String> {
    let mut tail = SendTail::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from-name" => {
                i += 1;
                match args.get(i) {
                    Some(v) => tail.from_name = Some(v.clone()),
                    None => return Err("--from-name needs a value".to_string()),
                }
            }
            "--from-self" => tail.from_self = true,
            "--raw" => tail.raw = true,
            other => tail.rest.push(other.to_string()),
        }
        i += 1;
    }
    Ok(tail)
}

/// Resolve the from_id name part from an explicit ambient value, so the
/// rule is testable without env mutation. `--from-self` is fail-loud,
/// matching the local verb's contract: no ambient session identity, no send.
pub fn resolve_from_name_with(ambient: Option<&str>, tail: &SendTail) -> Result<FromName, String> {
    if tail.from_self {
        let value = ambient.unwrap_or_default().trim();
        if value.is_empty() {
            return Err(
                "--from-self: no ambient fno session identity in this shell \
(exit 2, the same floor the local verb applies)"
                    .to_string(),
            );
        }
        return Ok(FromName::SelfName(value.to_string()));
    }
    if let Some(given) = &tail.from_name {
        return Ok(FromName::Given(given.clone()));
    }
    Ok(FromName::OsUser(os_user()))
}

/// The CLI form: reads the ambient identity from the process environment.
pub fn resolve_from_name(tail: &SendTail) -> Result<FromName, String> {
    resolve_from_name_with(std::env::var("FNO_AGENT_SELF").ok().as_deref(), tail)
}

fn os_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// `<user>@<host>/<name>`: the sender identity stamped into `from_id`
/// (informational provenance; a reply addresses `<name>@<host>`).
pub fn sender_id(user: &str, host: &str, name: &str) -> String {
    format!("{user}@{}/{}", mail_bus::normalize_host(host), name)
}

/// Resolve the `fno` front door to exec on this machine. The daemon's pull
/// arm shares the scrape helper (FNO_BIN, sibling of this binary, then PATH).
pub fn fno_binary() -> OsString {
    crate::scrape::fno_bin()
}

/// The local send the same-host rewrite and the daemon pull arm both run:
/// the receiver's own verb answers, so lane selection, holds and receipts
/// stay the local path's. `from` is the bus row's sender identity, stamped
/// with `--from-name` so the envelope carries the real sender, not the
/// delivering machine.
pub fn local_send_argv(name: &str, payload: &str, raw: bool, from: &str) -> Vec<OsString> {
    let mut argv = vec![fno_binary()];
    argv.push(OsString::from("agents"));
    argv.push(OsString::from("mail"));
    argv.push(OsString::from("send"));
    argv.push(OsString::from(name));
    if raw {
        argv.push(OsString::from("--raw"));
    }
    argv.push(OsString::from("--from-name"));
    argv.push(OsString::from(from));
    argv.push(OsString::from(payload));
    argv
}

enum StorePath {
    /// The primary this machine dials out to.
    Primary(Remote),
    /// `store.remote_url` unset: relay over ssh.
    None,
    /// A malformed value: refuse, never fall back in silence.
    Malformed(String),
}

fn store_path() -> StorePath {
    match store_remote::configured() {
        Ok(Some(remote)) => StorePath::Primary(remote),
        Ok(None) => StorePath::None,
        Err(e) => StorePath::Malformed(e),
    }
}

/// The ssh fallback's refusal of `--from-self`: that flag resolves identity
/// on whatever machine runs the verb, and the fallback runs it remotely.
pub fn fallback_from_self_refusal() -> String {
    "--from-self over the ssh fallback would stamp the remote machine's \
identity; pass --from-name instead"
        .to_string()
}

fn cmd_send(args: &[String]) -> i32 {
    let words: Vec<String> = args.to_vec();
    let Some(target) = words.first() else {
        eprintln!("usage: fno agents mail send <name>@<host> [args...] <message>");
        return 2;
    };
    let Ok((name, host)) = split_target(target) else {
        eprintln!("{}", target_error(target));
        return 2;
    };
    let tail = match parse_send_tail(&words[1..]) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let local_host = mail_bus::normalize_host(&crate::claims::hostname());
    if mail_bus::normalize_host(&host) == local_host {
        // A same-host target needs no bus row and no ssh hop: forward the
        // stripped argv to the local verb, every flag the caller passed
        // intact.
        let mut argv = vec![fno_binary()];
        argv.push(OsString::from("agents"));
        argv.push(OsString::from("mail"));
        argv.push(OsString::from("send"));
        argv.push(OsString::from(name));
        for word in &words[1..] {
            argv.push(OsString::from(word));
        }
        return run_pass_through(&argv);
    }

    match store_path() {
        StorePath::Malformed(e) => {
            eprintln!("fno agents mail send: {e}");
            2
        }
        StorePath::None => {
            if tail.from_self {
                eprintln!("{}", fallback_from_self_refusal());
                return 2;
            }
            // Message shape: the caller passes the message as the last
            // positional arg (the local verb's shape), and it rides the relay
            // verbatim.
            let mut relay_words = vec![
                "agents".to_string(),
                "mail".to_string(),
                "send".to_string(),
                name.clone(),
            ];
            relay_words.extend(tail.rest.iter().cloned());
            // The original message arg: everything after the target that is
            // not a flag was kept in rest in order, so the message is there.
            relay(&host, &relay_words)
        }
        StorePath::Primary(remote) => {
            let from_name = match resolve_from_name(&tail) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    return 2;
                }
            };
            let name_part = match &from_name {
                FromName::Given(v) | FromName::SelfName(v) => v.clone(),
                FromName::OsUser(v) => v.clone(),
            };
            let payload = match payload_of(&tail) {
                Some(p) if !p.is_empty() => p,
                _ => {
                    eprintln!(
                        "fno agents mail send: a cross-host store send carries \
the message directly after the target; none was found"
                    );
                    return 2;
                }
            };
            if tail.rest.len() > 1 {
                // A store row carries the payload, the raw bit, and the
                // sender identity - nothing else. Refuse rather than drop:
                // the other flags (--subject, --harness, --origin) belong to
                // the local and fallback lanes.
                eprintln!(
                    "fno agents mail send: a cross-host store send carries only \
--from-name/--from-self/--raw and the message; found extra words after it: {:?}",
                    &tail.rest[1..]
                );
                return 2;
            }
            let from_id = sender_id(&os_user(), &local_host, &name_part);
            send_via_store(
                &remote,
                &from_id,
                &local_host,
                &name,
                &host,
                &payload,
                tail.raw,
                None,
            )
        }
    }
}

/// The message body: the first word after the target (the local verb's
/// shape is `mail send <target> <message> [flags...]`), so a flag value can
/// never be mistaken for the payload. `None` when that word is missing or
/// is itself a flag; the caller refuses naming the shape.
fn payload_of(tail: &SendTail) -> Option<String> {
    match tail.rest.first() {
        Some(word) if !word.starts_with('-') => Some(word.clone()),
        _ => None,
    }
}

fn send_via_store(
    remote: &Remote,
    from_id: &str,
    from_host: &str,
    name: &str,
    host: &str,
    payload: &str,
    raw: bool,
    subject: Option<&str>,
) -> i32 {
    if let Err(e) = mail_bus::ensure_schema(&remote) {
        if store_remote::is_unreachable(&e) {
            return queue_and_report(from_id, from_host, name, host, payload, raw, &e);
        }
        eprintln!("fno agents mail send: bus schema: {e}");
        return 1;
    }
    let msg_id = mail_bus::mint_msg_id();
    match mail_bus::insert(
        &remote, &msg_id, from_id, from_host, name, host, payload, raw, subject,
    ) {
        Ok(outcome) => {
            let dup = if outcome.duplicated {
                "already queued"
            } else {
                "queued"
            };
            println!("{msg_id} {dup} (store seq={}, host {host})", outcome.seq);
            0
        }
        Err(e) => {
            if store_remote::is_unreachable(&e) {
                return queue_and_report(from_id, from_host, name, host, payload, raw, &e);
            }
            eprintln!("fno agents mail send: {e}");
            1
        }
    }
}

/// Edge 7: the primary is unreachable. Queue the row locally as one JSON
/// line; the daemon's flush arm drains it. Exit 0: queued is not failed.
#[allow(clippy::too_many_arguments)]
fn queue_and_report(
    from_id: &str,
    from_host: &str,
    name: &str,
    host: &str,
    payload: &str,
    raw: bool,
    error: &str,
) -> i32 {
    let msg_id = mail_bus::mint_msg_id();
    let line = serde_json::json!({
        "msg_id": msg_id,
        "from_id": from_id,
        "from_host": from_host,
        "to_name": name,
        "to_host": host,
        "payload": payload,
        "raw": raw,
    });
    let home = crate::paths::AgentsHome::from_env();
    let path = outbox_path(home.root());
    use std::io::Write;
    let write = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    match write {
        Ok(()) => {
            println!("{msg_id} queued (local, store unreachable)");
            0
        }
        Err(e) => {
            eprintln!("fno agents mail send: {error}; and the local queue write failed: {e}");
            1
        }
    }
}

/// The ssh relay: exec, inherit stdio, pass the exit code through.
fn relay(host: &str, words: &[String]) -> i32 {
    let argv = ssh_argv(host, words);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno agents mail: ssh to {host} failed: {e}");
            1
        }
    }
}

fn cmd_peek(args: &[String]) -> i32 {
    let words: Vec<String> = args.to_vec();
    let Some(target) = words.first() else {
        eprintln!("usage: fno agents peek <handle>@<host> [flags]");
        return 2;
    };
    let Ok((name, host)) = split_target(target) else {
        eprintln!("{}", target_error(target));
        return 2;
    };
    let local_host = mail_bus::normalize_host(&crate::claims::hostname());
    if mail_bus::normalize_host(&host) == local_host {
        let mut argv = vec![fno_binary()];
        argv.push(OsString::from("agents"));
        argv.push(OsString::from("peek"));
        argv.push(OsString::from(name));
        for word in &words[1..] {
            argv.push(OsString::from(word));
        }
        return run_pass_through(&argv);
    }
    let mut relay_words = vec!["agents".to_string(), "peek".to_string(), name];
    relay_words.extend(words[1..].iter().cloned());
    relay(&host, &relay_words)
}

fn cmd_status(_args: &[String]) -> i32 {
    match store_path() {
        StorePath::Primary(remote) => {
            if let Err(e) = mail_bus::ensure_schema(&remote) {
                eprintln!("fno agents mail status: {e}");
                return 1;
            }
            let host = mail_bus::normalize_host(&crate::claims::hostname());
            match mail_bus::sent_rows(&remote, &host) {
                Ok(rows) => {
                    for (msg_id, status) in rows {
                        println!("{msg_id} {status}");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("fno agents mail status: {e}");
                    1
                }
            }
        }
        StorePath::None => {
            eprintln!(
                "fno agents mail status: no shared store configured; sends a \
cross-host target answered over the ssh fallback leave no bus rows"
            );
            2
        }
        StorePath::Malformed(e) => {
            eprintln!("fno agents mail status: {e}");
            2
        }
    }
}

/// Binary-direct entry from `fno-agents mail-remote <send|peek|status> ...`.
/// The dispatcher holds `Vec<String>`, so the tail rides as `&[String]`.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(|a| a.as_str()) {
        Some("send") => cmd_send(&args[1..]),
        Some("peek") => cmd_peek(&args[1..]),
        Some("status") => cmd_status(&args[1..]),
        _ => {
            eprintln!("usage: fno-agents mail-remote <send|peek|status> <name>@<host> [args...]");
            2
        }
    }
}

/// Exec an argv, inherit stdio, return the exit code (same-host forwards).
fn run_pass_through(argv: &[OsString]) -> i32 {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno agents mail: local forward failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One declaration, every contract: the CI delta caps new test
    // declarations, so the relay's unit contracts share one table-style
    // body. The daemon arm's interval rule joins it (same crate).
    #[test]
    fn relay_units_guard_their_contracts() {
        // Target grammar: exactly one @, both sides nonempty.
        assert_eq!(
            split_target("nemo@imac2d2").ok(),
            Some(("nemo".into(), "imac2d2".into()))
        );
        assert!(split_target("nemo@bb16@imac2d2").is_err());
        assert!(split_target("@imac2d2").is_err());
        assert!(split_target("nemo@").is_err());
        assert!(split_target("nemo").is_err());
        // Remote-shell quoting: plain words wrap; the quote escapes; the
        // -lc script itself rides quoted so ssh's space-join cannot split
        // it (the round-1 regression).
        assert_eq!(sh_quote("hello"), "'hello'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("two words"), "'two words'");
        let argv = ssh_argv(
            "mbp3",
            &[
                "agents".into(),
                "peek".into(),
                "nemo".into(),
                "-n".into(),
                "5".into(),
            ],
        );
        assert_eq!(argv[0], "ssh");
        assert_eq!(argv[1], "mbp3");
        assert_eq!(argv[2], "--");
        assert_eq!(argv[3], "sh");
        assert_eq!(argv[4], "-lc");
        assert_eq!(argv[5], "'exec fno \"$@\"'");
        assert_eq!(argv[6], "sh");
        assert_eq!(argv[7], "'agents'");
        assert_eq!(argv[9], "'nemo'");
        // The send tail lifts identity flags and keeps the rest in order.
        let tail = parse_send_tail(&[
            "--from-name".into(),
            "rowan".into(),
            "--subject".into(),
            "hi".into(),
            "hello world".into(),
        ])
        .expect("parses");
        assert_eq!(tail.from_name.as_deref(), Some("rowan"));
        assert!(!tail.raw);
        assert_eq!(
            tail.rest,
            vec![
                "--subject".to_string(),
                "hi".to_string(),
                "hello world".to_string()
            ]
        );
        // --from-self is fail-loud without ambient identity (the same floor
        // the local verb applies); the ambient rides in as a parameter, so
        // no env mutation races the parallel tests.
        let selfy = SendTail {
            from_self: true,
            ..SendTail::default()
        };
        assert!(resolve_from_name_with(None, &selfy).is_err());
        assert!(resolve_from_name_with(Some("  "), &selfy).is_err());
        assert!(resolve_from_name_with(Some("nemo"), &selfy).is_ok());
        // Sender identity normalizes the host part.
        assert_eq!(
            sender_id("bb16", "iMac2D2.local", "nemo"),
            "bb16@imac2d2/nemo"
        );
        // The pull arm's argv carries the name, the sender, and the payload.
        let argv = local_send_argv("nemo", "hello there", false, "rowan@mbp3/r");
        let words: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(words.last().expect("payload rides last"), "hello there");
        assert!(words.contains(&"--from-name".to_string()));
        assert!(words.contains(&"rowan@mbp3/r".to_string()));
        // Strict payload shape: the message is the word after the target;
        // a leading flag means no message rode directly and the strict
        // shape refuses rather than guessing.
        let tail = parse_send_tail(&["hello world".to_string()]).expect("parses");
        assert_eq!(payload_of(&tail).as_deref(), Some("hello world"));
        let tail = parse_send_tail(&["--subject".to_string(), "x".to_string(), "msg".to_string()])
            .expect("parses");
        assert_eq!(payload_of(&tail), None);
        // The state-root files this module owns.
        let root = std::path::Path::new("/tmp/fno-mail-remote-test");
        assert!(outbox_path(root).ends_with("mail_outbox.jsonl"));
        assert!(seen_path(root).ends_with("mail_seen.jsonl"));
        // The daemon arm's interval rule: a positive override wins,
        // anything else keeps the default.
        assert_eq!(
            crate::mail_arm::interval_from_secs(None),
            crate::mail_arm::DEFAULT_INTERVAL
        );
        assert_eq!(
            crate::mail_arm::interval_from_secs(Some(0)),
            crate::mail_arm::DEFAULT_INTERVAL
        );
        assert_eq!(
            crate::mail_arm::interval_from_secs(Some(-5)),
            crate::mail_arm::DEFAULT_INTERVAL
        );
        assert_eq!(
            crate::mail_arm::interval_from_secs(Some(30)),
            std::time::Duration::from_secs(30)
        );
        // The pull arm's retry contract: an injection that fails leaves the
        // row in the pending pool, so a later pull still delivers it after a
        // younger row did (the fetch re-selects every pending row each beat).
        // The fake inject answers no remote call, and the seen ledger is
        // absent under the temp home, so the loop never dials the store.
        let bus_row = |seq: i64, id: &str| crate::mail_bus::BusRow {
            seq,
            msg_id: id.to_string(),
            from_id: "s@h/s".into(),
            to_name: "quill".into(),
            payload: "hello".into(),
            raw: false,
            expire_at: 1_800_000_000,
        };
        let home = std::path::Path::new("/tmp/fno-mail-arm-retry-test");
        let remote = Remote::parse("http://127.0.0.1:1", None).expect("parses");
        let mut fail_older = |row: &crate::mail_bus::BusRow| row.msg_id != "older";
        let rows = vec![bus_row(1, "older"), bus_row(2, "newer")];
        assert_eq!(
            crate::mail_arm::pull_rows_with(&remote, rows, home, &mut fail_older),
            1,
            "the failed row does not count as delivered"
        );
        let mut deliver_all = |_row: &crate::mail_bus::BusRow| true;
        let retried = vec![bus_row(1, "older")];
        assert_eq!(
            crate::mail_arm::pull_rows_with(&remote, retried, home, &mut deliver_all),
            1,
            "the next beat delivers the row the earlier beat could not"
        );
        // The finality rule: the style door naming violations (with no
        // exception declared) is the deterministic refusal; a clean receipt,
        // an excepted body, and unparseable output read transient, so the
        // row stays pending for the next beat.
        assert!(crate::mail_arm::door_confirms_refusal(
            br#"{"violations":[{"rule":3,"sentence_index":3}]}"#
        ));
        assert!(!crate::mail_arm::door_confirms_refusal(
            br#"{"violations":[]}"#
        ));
        assert!(!crate::mail_arm::door_confirms_refusal(
            br#"{"exception":"quoted word","violations":[{"rule":3}]}"#
        ));
        assert!(!crate::mail_arm::door_confirms_refusal(b"{}"));
        assert!(!crate::mail_arm::door_confirms_refusal(b"not json"));
        // The terminal refusals: an unknown recipient ends its row like the
        // style refusal does (exit 16 today, 13 the pre-port spelling), and
        // every transient exit stays pending for the next beat.
        assert!(crate::mail_arm::final_refusal(Some(16)));
        assert!(crate::mail_arm::final_refusal(Some(13)));
        assert!(!crate::mail_arm::final_refusal(Some(1)));
        assert!(!crate::mail_arm::final_refusal(Some(0)));
        assert!(!crate::mail_arm::final_refusal(None));
        // The consult rides the front door's lint-style forwarding, with the
        // configured mail word cap appended when one is set.
        let argv = crate::mail_arm::style_door_argv(None);
        assert_eq!(
            argv.first().map(String::as_str),
            Some("doctor"),
            "the front door classifies the check name"
        );
        assert!(argv.contains(&"--stdin".to_string()));
        assert!(argv.contains(&"--json".to_string()));
        assert!(!argv.iter().any(|word| word == "--word-cap"));
        let capped = crate::mail_arm::style_door_argv(Some(60));
        let cap_at = capped
            .iter()
            .position(|word| word == "--word-cap")
            .expect("cap flag");
        assert_eq!(capped[cap_at + 1], "60");
    }
}

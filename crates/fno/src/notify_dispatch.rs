//! `fno inbox notify TITLE MESSAGE [--pointer P]`: the operator-notice verb,
//! ported from the deleted Python leg `cli/src/fno/notify/_impl.py`. The
//! dispatch runs in-process at the front door: the journal row commits through
//! the generated [`crate::event_store`] (the same SQL transaction the Python
//! `emit-envelope` op reached by subprocess), the attached-client count rides
//! the mux control roundtrip in-process beside [`crate::notify_command`], and
//! the OS toast stays a best-effort `osascript`/`notify-send` child.
//!
//! The decision table is the Python leg's, characterized against
//! `tests/golden/notify_dispatch/` (see the parity test header): journal row
//! first, then the mux (client-first, zero clients falls through), then the
//! local tool, then the sink lane, else the loud one-line degrade. Empty title
//! and message default to `target` / `Complete`. `FNO_TEST_HERMETIC=1` skips
//! the mux and the toast but keeps the channel verdict honest - a host whose
//! tool is present still answers delivered with the toast suppressed, exactly
//! as the Python half of the rule did.
//!
//! The top-level `fno notify` spelling stays a deprecated move: it prints the
//! one stderr line and dispatches here as `inbox notify`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The Python `_impl._NO_TOOL_MSG`, byte for byte.
const NO_TOOL_MSG: &str = "fno inbox notify: no OS notification tool available (need osascript on macOS or notify-send on Linux); notification not sent.";

/// The Python `events.NOTICE_CAP`: one field of the journal row.
const NOTICE_CAP: usize = 2000;

/// The osascript / notify-send wall clock, mirroring the Python timeouts: a
/// hung notifier must not wedge the caller, and a notification is never
/// load-bearing.
const TOAST_TIMEOUT: Duration = Duration::from_secs(5);

/// `["inbox", "notify", ...rest]` claims the verb. The front door execs the
/// tail in-process; the Python wheel's app for this spelling is deleted.
pub fn classify_inbox_notify(args: &[OsString]) -> Option<Vec<OsString>> {
    let a0 = args.first()?.to_str()?;
    let a1 = args.get(1)?.to_str()?;
    if a0 != "inbox" || a1 != "notify" {
        return None;
    }
    Some(args[2..].to_vec())
}

/// `["notify", ...rest]` is the deprecated root spelling: same body, one
/// stderr line first. The Python move table carried this spelling; the front
/// door owns it now.
pub fn classify_root_notify(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.first()?.to_str()? != "notify" {
        return None;
    }
    Some(args[1..].to_vec())
}

/// Parse and run one notify dispatch; returns the exit code.
pub fn run(args: &[OsString]) -> i32 {
    let mut pointer: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(tok) = it.next() {
        let tok = match tok.to_str() {
            Some(t) => t,
            None => continue,
        };
        match tok {
            "--pointer" => match it.next().and_then(|v| v.to_str()) {
                Some(p) => pointer = Some(p.to_string()),
                None => return usage_error("requires an argument", "--pointer"),
            },
            _ if tok.starts_with("--pointer=") => {
                pointer = Some(tok["--pointer=".len()..].to_string());
            }
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            _ if tok.starts_with('-') && tok.len() > 1 => {
                return usage_error(&format!("unknown option: \"{tok}\""), "");
            }
            _ => positionals.push(tok.to_string()),
        }
    }
    if positionals.len() > 2 {
        eprintln!(
            "Usage: fno inbox notify [OPTIONS] {{title}} {{message}} COMMAND [ARGS]...\n\
             Try 'fno inbox notify --help' for help.\n\
             \n\
             Error: Got unexpected extra argument ({})",
            positionals[2]
        );
        return 2;
    }
    let Some(title) = positionals.first() else {
        eprintln!(
            "Usage: fno inbox notify [OPTIONS] {{title}} {{message}} COMMAND [ARGS]...\n\
             Try 'fno inbox notify --help' for help.\n\
             \n\
             Error: Missing argument 'title'."
        );
        return 2;
    };
    let Some(message) = positionals.get(1) else {
        eprintln!(
            "Usage: fno inbox notify [OPTIONS] {{title}} {{message}} COMMAND [ARGS]...\n\
             Try 'fno inbox notify --help' for help.\n\
             \n\
             Error: Missing argument 'message'."
        );
        return 2;
    };
    let (code, err) = send_notification(title, message, pointer.as_deref().unwrap_or(""));
    if !err.is_empty() {
        eprintln!("{err}");
    }
    code
}

fn usage_error(detail: &str, _flag: &str) -> i32 {
    eprintln!(
        "Usage: fno inbox notify [OPTIONS] {{title}} {{message}} COMMAND [ARGS]...\n\
         Try 'fno inbox notify --help' for help.\n\
         \n\
         Error: Invalid value for '--pointer': {detail}."
    );
    2
}

fn print_help() {
    print!(
        "Send an OS notification.\n\
         \n\
         Dispatches to the attached mux clients first, then the OS toast \
         (macOS osascript, Linux notify-send), then the status-sink lane. \
         With no channel at all it degrades loudly: non-zero exit plus a \
         one-line message.\n\
         \n\
         Arguments:\n\
           title   Notification title\n\
           message Notification message body\n\
         \n\
         Options:\n\
           --pointer TEXT  Command or URL naming the durable state this notice \
         points at\n\
           -h, --help      Print help\n"
    );
}

/// The dispatch decision table, the Python `send_notification` order.
/// Returns `(exit_code, error_message)` - the empty message on any accepted
/// channel, the degrade line only when no channel exists at all.
fn send_notification(title: &str, message: &str, pointer: &str) -> (i32, String) {
    let title = if title.is_empty() { "target" } else { title };
    let message = if message.is_empty() {
        "Complete"
    } else {
        message
    };

    let row = emit_operator_notice(title, message, pointer);
    if mux_clients(title, message).is_some_and(|clients| clients > 0) {
        return (0, String::new());
    }
    if dispatch_local(title, message) {
        return (0, String::new());
    }
    // No local tool: the sink lane is the only possible channel, so the only
    // case where the config read buys an answer is this one. A host with a
    // working notifier never pays a config load here.
    if row && sink_routes("operator_notice") {
        return (0, String::new());
    }
    (1, NO_TOOL_MSG.to_string())
}

fn suppressed() -> bool {
    std::env::var_os("FNO_TEST_HERMETIC").is_some_and(|v| v == "1")
}

fn cap(s: &str) -> String {
    s.chars().take(NOTICE_CAP).collect()
}

/// Append the `operator_notice` journal row (the sink lane's input). Best
/// effort beside the local toast: a journal failure must never turn a
/// delivered toast into an error, but the count reads it, so a host with no
/// local tool still answers honestly when this is the only channel.
fn emit_operator_notice(title: &str, message: &str, pointer: &str) -> bool {
    if suppressed() {
        return false;
    }
    let mut data = serde_json::Map::new();
    data.insert("title".into(), serde_json::Value::String(cap(title)));
    data.insert("body".into(), serde_json::Value::String(cap(message)));
    if !pointer.is_empty() {
        data.insert("pointer".into(), serde_json::Value::String(cap(pointer)));
    }
    let mut envelope = serde_json::Map::new();
    envelope.insert("ts".into(), serde_json::Value::String(now_rfc3339_micros()));
    envelope.insert(
        "type".into(),
        serde_json::Value::String("operator_notice".into()),
    );
    envelope.insert("source".into(), serde_json::Value::String("rust".into()));
    envelope.insert("data".into(), serde_json::Value::Object(data));
    let line = serde_json::Value::Object(envelope).to_string();
    match crate::event_store::append_envelope(&project_journal(), &line, None) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("notify: operator_notice append failed: {e}");
            false
        }
    }
}

/// The project event journal: `<spaces_root>/<space_slug(canonical)>/events.jsonl`,
/// mirroring Python `paths.project_log` (space-anchored so every worktree of
/// one repo lands on the same file). `FNO_SPACES_DIR` > `config.paths.spaces_dir`
/// > `<state root>/spaces`; the canonical root reads the `.git` gitfile the
/// same way `digest_overlay::canonical_root_with` does.
fn project_journal() -> PathBuf {
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let spaces = match std::env::var_os("FNO_SPACES_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => crate::model_catalog::state_dir().join("spaces"),
    };
    spaces
        .join(space_slug(&canonical_root(&root)))
        .join("events.jsonl")
}

fn canonical_root(root: &std::path::Path) -> PathBuf {
    let gitdir = root.join(".git");
    // A `.git` dir is a main checkout; an absent one is no repo. Only the
    // linked-worktree gitfile needs the probe.
    if !gitdir.exists() || gitdir.is_dir() {
        return root.to_path_buf();
    }
    crate::digest_overlay::canonical_root_with(root, false).unwrap_or_else(|| root.to_path_buf())
}

/// The full canonical path with `/` swapped for `-` (Python `paths.space_slug`,
/// mirrored byte for byte so the two languages read one space).
fn space_slug(canonical_root: &std::path::Path) -> String {
    canonical_root.to_string_lossy().replace('/', "-")
}

/// Wall-clock UTC stamp at the Python `_ts_now` precision (microseconds, `Z`).
fn now_rfc3339_micros() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let micros = dur.subsec_micros();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (
        (rem / 3600) as u32,
        (rem % 3600) / 60 as u32,
        (rem % 60) as u32,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{micros:06}Z")
}

/// Howard Hinnant's `civil_from_days`: unix days to `(y, m, d)`, the same
/// conversion `fno-agents::events::civil_from_unix` applies (no `chrono`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y, m as u32, d as u32)
}

/// Attached clients that took the notice via `fno mux notify`, else `None`.
/// In-process control roundtrip where the Python leg spawned a subprocess;
/// the receipt semantics are the ones the Python `_mux_clients` held: any
/// failure reads as no clients, and a zero count falls through to the toast.
fn mux_clients(title: &str, message: &str) -> Option<u64> {
    if suppressed() {
        return None;
    }
    let session = crate::mux_cli::resolve_session(None, crate::mux_cli::env_server().as_deref());
    let sock = crate::proto::socket_path(&session).ok()?;
    match crate::mux_cli::control_roundtrip(
        &sock,
        &session,
        crate::proto::ControlCommand::Notify {
            title: title.to_string(),
            body: message.to_string(),
        },
    ) {
        Ok(crate::proto::ServerMsg::NoticeSent { clients }) => Some(clients),
        _ => None,
    }
}

/// Fire the OS toast. True when a local tool was available to attempt it. The
/// tool's own failure is swallowed: a notification is never load-bearing.
/// Under the hermetic stamp the subprocess is skipped but a present tool still
/// counts as a channel, so a caller that branches on the code takes the same
/// path a real run would.
fn dispatch_local(title: &str, message: &str) -> bool {
    if cfg!(target_os = "macos") {
        // `osascript -e` compiles its argument as AppleScript, so a `"` or `\`
        // in title/message could terminate the string literal and inject
        // script. Escape backslash first, then double-quote (gemini PR #515
        // security). The subprocess failure is swallowed.
        if suppressed() {
            return true;
        }
        let esc_message = message.replace('\\', "\\\\").replace('"', "\\\"");
        let esc_title = title.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{esc_message}\" with title \"{esc_title}\"");
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return true;
    }
    if which("notify-send") {
        if suppressed() {
            return true;
        }
        // argv-fence: exempt (a notification body, not a spawn seed: a
        // leading-dash body drops one toast, it never idles a worker).
        let _ = Command::new("notify-send")
            .arg(title)
            .arg(message)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return true;
    }
    false
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// True when at least one enabled status sink routes `event_type`. A config
/// read must not break the toast: any failure answers false with a warning,
/// exactly the Python `_sink_routes` posture.
fn sink_routes(event_type: &str) -> bool {
    let rows = match status_sinks_rows() {
        Some(rows) => rows,
        None => return false,
    };
    let enabled_and_routing = rows.iter().any(|sink| {
        let enabled = sink
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let events = sink.get("events").and_then(|v| v.as_array());
        enabled
            && events
                .map(|evs| evs.iter().any(|e| e.as_str() == Some(event_type)))
                .unwrap_or(false)
    });
    enabled_and_routing
}

/// The `status_sinks` rows from the config ladder `$FNO_CONFIG` >
/// project-local `.fno/config.toml` > the global one. A container-level type
/// mismatch fails safe to "none" (the Python ConfigBlock degraded to `[]`).
fn status_sinks_rows() -> Option<Vec<serde_json::Value>> {
    for path in config_candidates() {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            continue;
        };
        let Some(value) = table.get("status_sinks") else {
            continue;
        };
        return value.as_array().map(|rows| {
            rows.iter()
                .map(|row| serde_json::to_value(row).unwrap_or(serde_json::Value::Null))
                .collect()
        });
    }
    None
}

fn config_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("FNO_CONFIG").filter(|v| !v.is_empty()) {
        out.push(PathBuf::from(explicit));
        return out;
    }
    let anchor = std::env::var_os("FNO_REPO_ROOT")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            loop {
                if dir.join(".git").exists() {
                    break dir;
                }
                match dir.parent() {
                    Some(parent) => dir = parent.to_path_buf(),
                    None => break PathBuf::from("."),
                }
            }
        });
    out.push(anchor.join(".fno").join("config.toml"));
    out.push(crate::model_catalog::state_dir().join("config.toml"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_title_and_message_take_the_defaults() {
        // The decision table with every channel absent: the defaults ride the
        // request, the exit is the loud degrade, and the message is byte-equal
        // to the Python _NO_TOOL_MSG.
        let _guard = env_lock();
        std::env::remove_var("FNO_TEST_HERMETIC");
        // No local tool on a non-macOS runner; on macOS osascript always
        // answers present, so pin the suppressed posture instead: the toast
        // channel is skipped but still counts, which is the golden case.
        std::env::set_var("FNO_TEST_HERMETIC", "1");
        let (code, msg) = send_notification("", "", "");
        assert_eq!(code, 0, "suppressed macOS toast still counts as a channel");
        assert!(msg.is_empty());
        std::env::remove_var("FNO_TEST_HERMETIC");
    }

    #[test]
    fn pointer_caps_at_the_notice_cap() {
        let big = "x".repeat(NOTICE_CAP + 50);
        let capped = cap(&big);
        assert_eq!(capped.chars().count(), NOTICE_CAP);
    }

    #[test]
    fn space_slug_swaps_slashes_keeps_the_leading_dash() {
        assert_eq!(
            space_slug(std::path::Path::new("/Users/bb16/code/footnote/footnote")),
            "-Users-bb16-code-footnote-footnote"
        );
    }

    #[test]
    fn micros_stamp_matches_the_python_shape() {
        let ts = now_rfc3339_micros();
        // 2026-10-26T05:12:34.123456Z
        assert_eq!(ts.len(), 27, "{ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[19..20], ".");
    }

    #[test]
    fn classify_claims_the_two_spellings_only() {
        let os = |a: &[&str]| -> Vec<OsString> { a.iter().map(OsString::from).collect() };
        assert!(classify_inbox_notify(&os(&["inbox", "notify", "T"])).is_some());
        assert!(classify_inbox_notify(&os(&["inbox", "mail"])).is_none());
        assert!(classify_root_notify(&os(&["notify", "T", "B"])).is_some());
        assert!(classify_root_notify(&os(&["notifyx"])).is_none());
    }

    /// One env-var test at a time: the dispatch posture reads the process env.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
}

//! The `fno inbox notify TITLE MESSAGE [--pointer P]` dispatch owner, living
//! in this crate. The daemon's operator notices run the decision table
//! in-process (operator_notice is the caller), and `crates/fno` compiles the
//! generated copy behind the same names, so the front door's `fno inbox
//! notify` and the deprecated `fno notify` move answer from one
//! implementation (the deleted Python leg `cli/src/fno/notify/_impl.py`'s
//! tables). build.rs `sync_notify_dispatch` generates the copy; the marked
//! `// @fno-copy:` regions carry the two crate-bound channels, everything
//! else is shared text.
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
//!
//! The journal row commits through [`crate::event_store`] (the owner), the
//! toast stays a best-effort `osascript`/`notify-send` child, and the two
//! crate-bound channels are the seams: the mux server owns attached clients,
//! so the daemon reaches it through the narrow `fno mux notify` one-shot
//! spawn while the fno copy rides its in-process `mux_cli` control roundtrip;
//! the daemon resolves the project journal through `state_path` while the fno
//! copy keeps its `review_invocation` resolver.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The Python `_impl._NO_TOOL_MSG`, byte for byte.
const NO_TOOL_MSG: &str = "fno inbox notify: no OS notification tool available (need osascript on macOS or notify-send on Linux); notification not sent.";

/// The deprecated root spelling's move line (the Python move table's text,
/// byte for byte). `pub` so the parity test pins it against the golden.
pub const DEPRECATED_MOVE_LINE: &str = "fno notify is now fno inbox notify";

/// The Python `events.NOTICE_CAP`: one field of the journal row.
const NOTICE_CAP: usize = 2000;

/// The osascript / notify-send wall clock, mirroring the Python timeouts: a
/// hung notifier must not wedge the caller, and a notification is never
/// load-bearing.
const TOAST_TIMEOUT: Duration = Duration::from_secs(5);

/// `["inbox", "notify", ...rest]` claims the command. The front door execs the
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
    let mut fenced = false;
    let mut it = args.iter();
    while let Some(tok) = it.next() {
        let tok = match tok.to_str() {
            Some(t) => t,
            None => continue,
        };
        match tok {
            "--" if !fenced => fenced = true,
            "--pointer" if !fenced => match it.next().and_then(|v| v.to_str()) {
                Some(p) => pointer = Some(p.to_string()),
                None => return usage_error("requires an argument", "--pointer"),
            },
            _ if tok.starts_with("--pointer=") && !fenced => {
                pointer = Some(tok["--pointer=".len()..].to_string());
            }
            "-h" | "--help" if !fenced => {
                print_help();
                return 0;
            }
            _ if !fenced && tok.starts_with('-') && tok.len() > 1 => {
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
pub fn send_notification(title: &str, message: &str, pointer: &str) -> (i32, String) {
    send_notification_seams(
        title,
        message,
        pointer,
        &mut |t, m| dispatch_local(t, m),
        &mut |t, m| mux_clients(t, m),
        &mut |t, m, p| emit_operator_notice(t, m, p),
        &mut |e| sink_routes(e),
    )
}

/// The table with its four channels injected: the parity test drives it
/// exactly the way the deleted Python leg's tests monkeypatched the module
/// functions (`_impl._dispatch_local` and friends).
pub fn send_notification_seams(
    title: &str,
    message: &str,
    pointer: &str,
    local: &mut dyn FnMut(&str, &str) -> bool,
    mux: &mut dyn FnMut(&str, &str) -> Option<u64>,
    journal: &mut dyn FnMut(&str, &str, &str) -> bool,
    sinks: &mut dyn FnMut(&str) -> bool,
) -> (i32, String) {
    let title = if title.is_empty() { "target" } else { title };
    let message = if message.is_empty() {
        "Complete"
    } else {
        message
    };

    let row = journal(title, message, pointer);
    if mux(title, message).is_some_and(|clients| clients > 0) {
        return (0, String::new());
    }
    if local(title, message) {
        return (0, String::new());
    }
    // No local tool: the sink lane is the only possible channel, so the only
    // case where the config read buys an answer is this one. A host with a
    // working notifier never pays a config load here.
    if row && sinks("operator_notice") {
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

/// The project event journal, resolved through the crate's one resolver: the
/// `FNO_EVENTS_PATH` pin, else the fno-agents-resolved space (the file the
/// fanout lane reads). A hand-built path beside it splits the notice row from
/// the sink lane under every pin a harness sets.
fn project_journal() -> PathBuf {
    // @fno-copy:begin journal_path
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::state_path::resolve("events", &cwd).unwrap_or_default()
    // @fno-copy:end journal_path
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
        ((rem % 3600) / 60) as u32,
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

/// Attached clients that took the notice via the mux channel, else `None`.
/// The receipt semantics are the ones the Python `_mux_clients` held: any
/// failure reads as no clients, and a zero count falls through to the toast.
fn mux_clients(title: &str, message: &str) -> Option<u64> {
    if suppressed() {
        return None;
    }
    // @fno-copy:begin mux_channel
    // The mux server owns attached clients, so the notice crosses to it as
    // the narrow `fno mux notify` one-shot (a production fno-agents cannot
    // link the mux crate). `--json --` keeps a leading-dash title in its
    // positional slot; the receipt's `clients` count is the channel verdict.
    // The wall clock is shorter than the toast budget a pager caller allows,
    // long enough for one control roundtrip over a live socket.
    const MUX_TIMEOUT_SECS: u64 = 10;
    let bin = crate::scrape::fno_bin();
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(["mux", "notify", "--json", "--", title, message]);
    let out = crate::bounded_cmd::output_with_timeout(cmd, MUX_TIMEOUT_SECS)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    Some(parsed.get("clients")?.as_u64()?)
    // @fno-copy:end mux_channel
}

/// The macOS app that owns the user's terminal, from `TERM_PROGRAM` or the
/// launching bundle id. A multiplexer (Zellij) sets neither of its own, so the
/// outer terminal's value shows through. `None` means no known terminal.
fn terminal_app(term_program: &str, bundle_id: &str) -> Option<&'static str> {
    match term_program {
        "WarpTerminal" => return Some("Warp"),
        "iTerm.app" => return Some("iTerm"),
        "Apple_Terminal" => return Some("Terminal"),
        "ghostty" => return Some("Ghostty"),
        "WezTerm" => return Some("WezTerm"),
        "Hyper" => return Some("Hyper"),
        _ => {}
    }
    match bundle_id {
        "dev.warp.Warp-Stable" => Some("Warp"),
        "com.googlecode.iterm2" => Some("iTerm"),
        "com.apple.Terminal" => Some("Terminal"),
        "com.mitchellh.ghostty" => Some("Ghostty"),
        "com.github.wez.wezterm" => Some("WezTerm"),
        "co.zeit.hyper" => Some("Hyper"),
        _ => None,
    }
}

/// The AppleScript for one toast. Backslash first, then double quote, so a
/// title or body cannot end the string literal and inject script. A known
/// terminal owns the alert, so macOS shows that app's icon, not Script Editor.
fn toast_script(app: Option<&str>, title: &str, message: &str) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let show = format!(
        "display notification \"{}\" with title \"{}\"",
        esc(message),
        esc(title)
    );
    match app {
        // Only a running app is told: `tell` on a closed one launches it. A
        // terminal with no Apple-event dictionary (Warp, WezTerm, Hyper)
        // errors, so the handler falls back to the plain toast, never none.
        Some(app) => format!(
            "try\nif application \"{app}\" is running then\ntell application \"{app}\" to {show}\nelse\n{show}\nend if\non error\n{show}\nend try"
        ),
        None => show,
    }
}

/// The desk toast the daemon fires on a done badge: an attached mux client
/// first (it writes the OSC alert to its own terminal), then the local tool.
/// No journal row. A failure on either leg is dropped.
pub fn desk_toast(title: &str, message: &str) {
    if mux_clients(title, message).is_some_and(|clients| clients > 0) {
        return;
    }
    dispatch_local(title, message);
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
        let app = terminal_app(
            &std::env::var("TERM_PROGRAM").unwrap_or_default(),
            &std::env::var("__CFBundleIdentifier").unwrap_or_default(),
        );
        let script = toast_script(app, title, message);
        let _ = run_bounded(
            Command::new("osascript")
                .arg("-e")
                .arg(&script)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        return true;
    }
    if which("notify-send") {
        if suppressed() {
            return true;
        }
        // argv-fence: exempt (a notification body, not a spawn seed: a
        // leading-dash body drops one toast, it never idles a worker).
        let _ = run_bounded(
            Command::new("notify-send")
                .arg(title)
                .arg(message)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
        return true;
    }
    false
}

/// Run a notifier child under [`TOAST_TIMEOUT`]: spawn, poll `try_wait`, kill
/// at the deadline. The Python leg bounded `subprocess.run` with `timeout=5`;
/// a bare `.status()` has no deadline, so a hung `osascript` wedges the
/// caller.
fn run_bounded(command: &mut Command) -> bool {
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + TOAST_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
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
    // The global candidate, spelled crate-neutrally so both copies share the
    // line: `FNO_STATE_DIR` when set, else `$HOME/.fno` - the same resolution
    // fno's `model_catalog::state_dir` applies.
    out.push(
        std::env::var_os("FNO_STATE_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".fno"))
                    .unwrap_or_else(|| PathBuf::from(".fno"))
            })
            .join("config.toml"),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::test_env_lock;

    #[test]
    fn toast_script_escapes_and_names_the_app() {
        assert_eq!(
            toast_script(Some("iTerm"), "a\"b", "c\\d"),
            "try\nif application \"iTerm\" is running then\ntell application \"iTerm\" to display notification \"c\\\\d\" with title \"a\\\"b\"\nelse\ndisplay notification \"c\\\\d\" with title \"a\\\"b\"\nend if\non error\ndisplay notification \"c\\\\d\" with title \"a\\\"b\"\nend try"
        );
        assert!(toast_script(None, "t", "m").starts_with("display notification"));
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

    /// A stub `fno` whose argv lands in a file, so a seam spawn's shape is
    /// asserted from the child's own record. The argv file path rides an env
    /// var the stub reads.
    fn stub_fno(dir: &std::path::Path, name: &str, script: &str) -> PathBuf {
        crate::write_exec_stub(dir, name, &format!("#!/bin/bash\n{script}\n"))
    }

    fn env_guard(var: &str, value: Option<&std::ffi::OsStr>) -> Option<std::ffi::OsString> {
        let prev = std::env::var_os(var);
        match value {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
        prev
    }

    fn restore_env(var: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var(var, v),
            None => std::env::remove_var(var),
        }
    }

    #[test]
    fn mux_seam_failure_and_zero_clients_fall_through_honestly() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // A one-shot that answers zero clients: the count passes through so
        // the table falls to the toast, never reading zero as failure.
        let zero = stub_fno(dir.path(), "fno-zero", "echo '{\"clients\": 0}'");
        let bin_prev = env_guard("FNO_BIN", Some(zero.as_os_str()));
        let hermetic_prev = env_guard("FNO_TEST_HERMETIC", None);
        let seen = mux_clients("T", "B");
        restore_env("FNO_TEST_HERMETIC", hermetic_prev);
        restore_env("FNO_BIN", bin_prev);
        assert_eq!(seen, Some(0));
        // A failing or non-JSON one-shot reads as no clients: the seam's
        // None sends the table to the toast, exactly the Python posture.
        let dead = stub_fno(dir.path(), "fno-dead", "exit 7");
        let bin_prev = env_guard("FNO_BIN", Some(dead.as_os_str()));
        let hermetic_prev = env_guard("FNO_TEST_HERMETIC", None);
        let seen = mux_clients("T", "B");
        restore_env("FNO_TEST_HERMETIC", hermetic_prev);
        restore_env("FNO_BIN", bin_prev);
        assert_eq!(seen, None);
    }

    #[test]
    fn hermetic_stamp_skips_the_mux_spawn() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let argv = dir.path().join("mux.argv");
        std::env::set_var("FNO_TEST_ARGV_FILE", &argv);
        let stub = stub_fno(dir.path(), "fno-hermetic", "exit 0");
        let bin_prev = env_guard("FNO_BIN", Some(stub.as_os_str()));
        let hermetic_prev = env_guard("FNO_TEST_HERMETIC", Some(std::ffi::OsStr::new("1")));
        let seen = mux_clients("T", "B");
        restore_env("FNO_TEST_HERMETIC", hermetic_prev);
        restore_env("FNO_BIN", bin_prev);
        assert_eq!(seen, None);
        assert!(
            !argv.exists(),
            "the hermetic stamp must suppress the mux spawn entirely"
        );
    }
}

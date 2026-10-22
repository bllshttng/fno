//! The pane-prepare crossing: gate and envelope a payload through the native
//! `fno-agents mail pane-prepare` renderer before it is typed into a pane.

use std::path::PathBuf;
use std::time::Duration;

/// How long the envelope renderer gets. It reads the pane frame and runs the
/// manifest engine over it, so it is a subprocess hop, not a network call; a
/// renderer that has not answered by now is wedged, and a wedged renderer must
/// fail the send rather than release a bare paste.
///
/// Strictly GREATER than the child's own worst case, which is what makes the
/// diagnostic honest. The nested pane read is bounded at 30s on the renderer
/// leg, and the manifest eval runs in-process there - the port removed the
/// interpreter startup that used to add its own seconds. At exactly 30s this
/// side killed the child first and reported a wedged renderer for a slow pane,
/// sending the reader to the wrong component. The child hits its own bound
/// first now and says which hop was slow.
const PANE_PREPARE_TIMEOUT: Duration = Duration::from_secs(45);

/// Gate and envelope `bytes` for `session:pane` by shelling to the pane-send
/// renderer, the native `fno-agents mail pane-prepare` command.
///
/// Fails closed on every arm: a missing renderer, a non-zero exit (the pane is
/// showing an option prompt, hosts no registered agent, or the body cannot be
/// attributed) or a timeout. There is deliberately no
/// bare-paste fallback -- an unattributed send is the defect this exists to
/// close, so falling back to one when the renderer is unavailable would restore
/// it exactly when something is already wrong.
///
/// The renderer program resolves through the digest overlay's paired
/// `fno-agents` binary. The front binary is still resolved and handed to the
/// child as `FNO_BIN`: the renderer's nested `mux pane read` hop resolves that
/// variable or a bare `fno` on PATH, and this side can be running precisely
/// because `fno` is not on PATH.
pub(super) fn prepare_pane_bytes(
    session: &str,
    pane: u64,
    bytes: &[u8],
    style_exception: Option<&str>,
) -> Result<Vec<u8>, String> {
    use std::io::Write;

    let front = match std::env::var_os("FNO_BIN") {
        Some(v) => PathBuf::from(v),
        None => std::env::current_exe()
            .map_err(|e| format!("cannot locate the fno binary to render the envelope: {e}"))?,
    };
    let program = crate::digest_overlay::fno_agents_bin();
    let mut command = crate::process_admission::std_command(&program);
    command.args([
        "mail",
        "pane-prepare",
        "--session-id",
        session,
        "--pane",
        &pane.to_string(),
    ]);
    if let Some(reason) = style_exception.filter(|r| !r.trim().is_empty()) {
        // The mail commands' reasoned one-send exception, same flag on this lane.
        command.arg("--style-exception").arg(reason);
    }
    let child = command
        // Hand the child the front path we resolved. It runs a nested
        // `mux pane read` for the prompt gate, and that hop resolves
        // `FNO_BIN` or a bare `fno` on PATH. The front can be off PATH (the
        // reason this crossing runs at all), so leaving this unset made the
        // gate unable to read the frame on exactly those installs, and every
        // non-raw send refused as "frame unreadable".
        .env("FNO_BIN", &front)
        .env("FNO_MAIL_FENCE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = crate::process_admission::std_spawn(child).map_err(|e| {
        format!(
            "envelope renderer ({}) could not start: {e}. Refusing to type an \
                 unattributed payload; pass --raw for a genuine keystroke.",
            program.display()
        )
    })?;
    // The write runs on its own thread, so it cannot deadlock against a child
    // that starts answering before it has read everything. Dropping the handle
    // at the end of the closure closes the pipe, which is the renderer's EOF.
    // A renderer that refuses reads nothing and exits, closing the pipe first;
    // that EPIPE is not the failure, the exit code below is, so it is swallowed
    // here and left to the exit arm to explain.
    let payload = bytes.to_vec();
    let writer = child.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        })
    });
    let waited = wait_with_deadline(child, PANE_PREPARE_TIMEOUT);
    if let Some(handle) = writer {
        let _ = handle.join();
    }
    let (exit, rendered, diagnostics) = waited?;
    if !exit.success() {
        let detail = String::from_utf8_lossy(&diagnostics).trim().to_string();
        let detail = if detail.is_empty() {
            format!("renderer exited {exit}")
        } else {
            detail
        };
        return Err(format!("{detail} (pass --raw to type keystrokes instead)"));
    }
    Ok(rendered)
}

/// Wait for `child`, killing it once `budget` elapses. `Child::wait_with_output`
/// has no timeout of its own, and a renderer that never exits hangs the send
/// forever -- a hang is a worse failure than a refusal, because nothing surfaces
/// it.
///
/// Both pipes are drained on their own threads FOR THE WHOLE WAIT, not after it.
/// The renderer's stdout is the wrapped body, so it is as large as the payload,
/// and `pane send --stdin` has no body cap of its own. A poll loop that only
/// calls `try_wait` leaves stdout unread; once it fills the pipe buffer the child
/// blocks on write, never exits, and every large payload fails at the deadline
/// with a message blaming a renderer that was working fine.
///
/// Returns the exit result plus both drained streams, rather than
/// `process::Output`: naming that struct's exit field would put a bare
/// plan-frontmatter identifier in this file, which a repo ratchet counts to keep
/// readiness classification out of Rust. The tuple is what the one caller wants
/// anyway.
fn wait_with_deadline(
    mut child: std::process::Child,
    budget: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> {
    use std::io::Read;

    let mut out_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "envelope renderer stdout unavailable".to_string())?;
    let mut err_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "envelope renderer stderr unavailable".to_string())?;
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = std::time::Instant::now() + budget;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(exit)) => break exit,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // The readers end when the killed child's pipes close, so
                    // joining here cannot outlive the kill.
                    let _ = out_reader.join();
                    let _ = err_reader.join();
                    return Err(format!(
                        "envelope renderer did not answer within {}s; refusing to type \
                         an unattributed payload (pass --raw for a genuine keystroke)",
                        budget.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("envelope renderer wait failed: {e}")),
        }
    };
    let rendered = out_reader
        .join()
        .map_err(|_| "envelope renderer stdout reader panicked".to_string())?;
    let diagnostics = err_reader
        .join()
        .map_err(|_| "envelope renderer stderr reader panicked".to_string())?;
    Ok((exit, rendered, diagnostics))
}

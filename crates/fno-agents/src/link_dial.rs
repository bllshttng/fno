//! `fno-agents attach --dial <host> <key>`: the local half of a remote session.
//! A mux pane runs it (through `fno mux serve --link <key> --host <host>`), so
//! the keeper holds it across a client quit and a server restart.
//!
//! It runs ssh on the inherited tty and copies no bytes. A lost connection keeps
//! the last screen, marks the top row, and redials. Keys typed during the gap
//! are flushed, so nothing reaches an agent late.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::mail_remote::ssh_tty_argv;

/// The far door's exits (`fno::mux_link`). Ssh answers 255 for a lost connection.
const FAR_LEFT: i32 = 0;
const FAR_USAGE: i32 = 2;
const FAR_GONE: i32 = 3;
const FAR_RETRY: i32 = 4;
const SSH_LOST: i32 = 255;

const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(15);
/// Past this with no session, the pane stops and says why. It covers ssh's own
/// 45-second keepalive window several times over.
const BUDGET: Duration = Duration::from_secs(180);
/// A session that ran this long proves the path works, so the next drop starts
/// a fresh budget.
const PROVEN_SESSION: Duration = Duration::from_secs(30);

/// What the supervisor does after one ssh run.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Close the pane at once: the user left.
    Done,
    /// Stop and hold the reason on screen until a key.
    Stop(String),
    Redial(Duration),
}

/// `code` is the ssh exit, `None` when a signal ended it. `waited` is the time
/// since the last proven session, and `delay` the wait before this redial.
fn decide(host: &str, code: Option<i32>, waited: Duration, delay: Duration) -> Next {
    match code {
        Some(FAR_LEFT) => Next::Done,
        Some(FAR_GONE) => Next::Stop(format!("the session on {host} is gone")),
        Some(FAR_USAGE) => Next::Stop(format!(
            "{host} cannot open this link: its build may predate it. On {host}, run `fno doctor update`"
        )),
        None | Some(SSH_LOST) | Some(FAR_RETRY) if waited >= BUDGET => Next::Stop(format!(
            "gave up on {host} after {} seconds; reopen the row to try again",
            BUDGET.as_secs()
        )),
        None | Some(SSH_LOST) | Some(FAR_RETRY) => Next::Redial(delay),
        Some(other) => Next::Stop(format!("the session on {host} ended with exit {other}")),
    }
}

fn next_delay(delay: Duration) -> Duration {
    (delay * 2).min(MAX_DELAY)
}

/// The top-row mark over the kept screen, cursor saved and restored around it.
fn reconnect_mark(host: &str, delay: Duration) -> String {
    format!(
        "\x1b7\x1b[1;1H\x1b[0;7m[reconnecting to {host}: retry in {}s, typed keys are dropped]\x1b[0m\x1b[K\x1b8",
        delay.as_secs()
    )
}

/// No echo and no line editing while no ssh owns this tty. Ssh saves and
/// restores the mode itself, so a session starts and ends in this mode.
fn raw_tty() -> Option<libc::termios> {
    let fd = std::io::stdin().as_raw_fd();
    // SAFETY: tcgetattr and tcsetattr only read and write the termios struct.
    unsafe {
        let mut saved: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut saved) != 0 {
            return None;
        }
        let mut raw = saved;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(fd, libc::TCSANOW, &raw);
        Some(saved)
    }
}

fn restore_tty(saved: &libc::termios) {
    // SAFETY: as above; `saved` came from tcgetattr.
    unsafe {
        libc::tcsetattr(std::io::stdin().as_raw_fd(), libc::TCSANOW, saved);
    }
}

fn flush_typed_keys() {
    // SAFETY: tcflush discards queued tty input and touches no memory.
    unsafe {
        libc::tcflush(std::io::stdin().as_raw_fd(), libc::TCIFLUSH);
    }
}

fn write_tty(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// Run the link until the user leaves or a stop holds its reason on screen.
pub fn run(host: &str, key: &str) -> i32 {
    let words: Vec<String> = ["mux", "serve", "--link", key]
        .into_iter()
        .map(String::from)
        .collect();
    let argv = ssh_tty_argv(host, &words);
    let saved = raw_tty();
    let mut delay = FIRST_DELAY;
    let mut proven = Instant::now();
    let code = loop {
        flush_typed_keys();
        let began = Instant::now();
        let status = Command::new(&argv[0]).args(&argv[1..]).status();
        let code = match status {
            Ok(status) => status.code(),
            Err(e) => {
                write_tty(&format!("\r\ncould not run ssh: {e}\r\n"));
                break 1;
            }
        };
        if began.elapsed() >= PROVEN_SESSION {
            proven = Instant::now();
            delay = FIRST_DELAY;
        }
        match decide(host, code, proven.elapsed(), delay) {
            Next::Done => break 0,
            Next::Stop(reason) => {
                write_tty(&format!(
                    "\r\n{reason}\r\npress a key to close this pane\r\n"
                ));
                flush_typed_keys();
                let _ = std::io::stdin().read(&mut [0u8; 1]);
                break 0;
            }
            Next::Redial(wait) => {
                write_tty(&reconnect_mark(host, wait));
                std::thread::sleep(wait);
                delay = next_delay(wait);
            }
        }
    };
    if let Some(saved) = &saved {
        restore_tty(saved);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOON: Duration = Duration::from_secs(1);

    fn at(code: Option<i32>, waited: Duration) -> Next {
        decide("bb16", code, waited, SOON)
    }

    #[test]
    fn link_dial_redials_only_a_lost_connection_inside_the_budget() {
        for lost in [Some(SSH_LOST), Some(FAR_RETRY), None] {
            assert_eq!(at(lost, SOON), Next::Redial(SOON));
            assert!(
                matches!(at(lost, BUDGET), Next::Stop(reason) if reason.contains("gave up on bb16"))
            );
        }
    }

    #[test]
    fn link_dial_stops_on_every_other_exit() {
        let Next::Stop(reason) = at(Some(FAR_USAGE), SOON) else {
            panic!("exit 2 must not redial");
        };
        assert!(reason.contains("fno doctor update"), "{reason}");
        assert_eq!(at(Some(FAR_LEFT), SOON), Next::Done);
        assert!(matches!(at(Some(FAR_GONE), SOON), Next::Stop(reason) if reason.contains("gone")));
        assert!(matches!(at(Some(13), SOON), Next::Stop(reason) if reason.contains("exit 13")));
    }
}

//! The org board's actions on a seat or machine this mux does not host: the
//! front-door words each key runs, the refusals that name why nothing ran,
//! and the one off-loop runner. Every action execs the public `fno` command,
//! so the ssh relay stays the only path to another machine.

use super::super::org_seats::{Machine, Seat};
use std::time::Duration;

/// How many transcript records a peek page shows.
const PEEK_LINES: &str = "40";
/// The bound on one action. A spawn over ssh answers in seconds.
const BOUND: Duration = Duration::from_secs(60);
/// The one spawn refusal a prompt line fixes: every other refusal stops the
/// `s` key before the prompt opens.
pub(crate) const NO_PROMPT: &str = "type a prompt first";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// Space: the seat's recent transcript on a page.
    Peek,
    /// `P`: a local pane that follows the seat's transcript, read-only.
    View,
    /// `s`: a new seat on the machine.
    Spawn,
}

/// One action ready to run: its words after `fno`, the label its notice
/// names, and the board key its page belongs to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Queued {
    pub(crate) act: Act,
    pub(crate) label: String,
    pub(crate) key: String,
    pub(crate) argv: Vec<String>,
}

pub(crate) struct Done {
    pub(crate) act: Act,
    pub(crate) label: String,
    pub(crate) key: String,
    /// The command's stdout lines, control bytes stripped, or why it failed.
    pub(crate) result: Result<Vec<String>, String>,
}

/// The directory a new seat starts in: the selected seat's, else the first
/// seat on the machine that reports one. The path is on that machine.
pub(crate) fn spawn_cwd(m: &Machine, seat: Option<&Seat>) -> Option<String> {
    seat.and_then(|s| s.cwd.clone()).or_else(|| {
        m.leads
            .iter()
            .flat_map(|l| &l.seats)
            .find_map(|s| s.cwd.clone())
    })
}

/// The front-door words for `act` on machine `m`, or the refusal. Pure.
/// `local_owner` is the owner this machine's beat states; `input` is the
/// typed prompt for a spawn.
pub(crate) fn action_argv(
    act: Act,
    m: &Machine,
    seat: Option<&Seat>,
    local_owner: Option<&str>,
    input: &str,
) -> Result<Vec<String>, String> {
    if !m.fresh {
        return Err(format!(
            "{} last beat {} ago, so its fno may be down: nothing ran",
            m.host,
            super::super::org_tabs::dur(m.age_s)
        ));
    }
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if act == Act::Spawn {
        if let (Some(owner), Some(local)) = (m.owner.as_deref(), local_owner) {
            if owner != local {
                return Err(format!(
                    "{} belongs to {owner}: work on another person's machine needs team mode, \
                     which fno does not have yet. The owner is a stated name, not proof, so \
                     this refusal protects nothing",
                    m.host
                ));
            }
        }
        let cwd = spawn_cwd(m, seat).ok_or_else(|| {
            format!(
                "no seat on {} reports a cwd, so the board cannot pick a directory: \
                 run fno agents spawn --name @{} --cwd <dir> \"<prompt>\"",
                m.host, m.host
            )
        })?;
        let prompt = input.trim();
        if prompt.is_empty() {
            return Err(NO_PROMPT.into());
        }
        // The spawn seam reads any word that starts with a dash as a flag.
        if prompt.starts_with('-') {
            return Err("a prompt that starts with - reads as a flag: start it with a word".into());
        }
        let name = format!("@{}", m.host);
        return Ok(words(&[
            "agents",
            "spawn",
            "--name",
            name.as_str(),
            "--cwd",
            cwd.as_str(),
            prompt,
        ]));
    }
    let s = seat.ok_or("select a seat first")?;
    let at = format!("{}@{}", s.short, m.host);
    Ok(match act {
        Act::Peek => words(&["agents", "session", "peek", at.as_str(), "-n", PEEK_LINES]),
        _ => words(&[
            "mux",
            "pane",
            "run",
            "fno",
            "agents",
            "session",
            "peek",
            at.as_str(),
            "--follow",
        ]),
    })
}

/// Run one action off the UI loop under [`BOUND`]. A failure names the
/// command's last stderr line, which for the relay is ssh's own message.
pub(crate) async fn run(q: Queued) -> Done {
    let mut cmd = crate::process_admission::tokio_command(crate::server::fno_bin());
    cmd.args(&q.argv)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output =
        tokio::time::timeout(BOUND, crate::process_admission::tokio_output(&mut cmd)).await;
    let clean = |bytes: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(super::super::overlay_lines::sanitize_peek_line)
            .collect()
    };
    let result = match output {
        Err(_) => Err(format!(
            "{}: no answer within {}s",
            q.label,
            BOUND.as_secs()
        )),
        Ok(Err(e)) => Err(format!("{}: could not run: {e}", q.label)),
        Ok(Ok(o)) if o.status.success() => Ok(clean(&o.stdout)),
        Ok(Ok(o)) => {
            let why = clean(&o.stderr)
                .into_iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_else(|| format!("exit {}", o.status.code().unwrap_or(-1)));
            Err(format!("{} failed: {}", q.label, why.trim()))
        }
    };
    Done {
        act: q.act,
        label: q.label,
        key: q.key,
        result,
    }
}

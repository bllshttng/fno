//! One write path for every backlog board (the mux TUI and the web bridge):
//! the argv builders and the one bounded shell-out. The commands themselves are
//! Python-served `fno backlog` commands; a board builds argv here, runs it
//! through [`run_command`], and shows the command's own last line back as the
//! notice. No board builds a write argv by hand.

use std::time::Duration;

/// One `fno backlog ...` write command's budget (the board read's).
const COMMAND_BUDGET: Duration = Duration::from_secs(10);

/// Where a rank move puts the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Top,
    Bottom,
    Before,
    After,
}

/// The argv for one card move: `fno backlog rank <id> <place> ...`, always
/// ending in `--operator`. Before and After need their anchor.
pub fn rank_argv(id: &str, place: Place, anchor: Option<&str>) -> Result<Vec<String>, String> {
    let (word, flag) = match place {
        Place::Top => ("top", None),
        Place::Bottom => ("bottom", None),
        Place::Before => ("before", Some("--before")),
        Place::After => ("after", Some("--after")),
    };
    if flag.is_some() && anchor.is_none() {
        return Err(format!("rank {word}: no anchor card named"));
    }
    let mut args: Vec<String> = vec!["backlog".into(), "rank".into(), id.into(), word.into()];
    if let (Some(flag), Some(anchor)) = (flag, anchor) {
        args.push(flag.into());
        args.push(anchor.into());
    }
    args.push("--operator".into());
    Ok(args)
}

/// One line comment in a review: where it sits and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    pub path: String,
    pub line: u32,
    /// The anchor is an old-side line of a removed row.
    pub removed: bool,
    pub source: String,
    pub text: String,
}

/// A finding's prose limit in the note engine.
const REVIEW_BODY_CAP: usize = 5000;
/// How much of a quoted source line a review keeps.
const QUOTE_CHARS: usize = 160;

/// The body of one blocking finding: a head line, then each comment numbered.
/// Over the note engine's limit it refuses with the count and never truncates.
pub fn review_body(
    pr: &crate::pr_diff::PrRef,
    head: &str,
    comments: &[ReviewComment],
) -> Result<String, String> {
    let head8: String = head.chars().take(8).collect();
    let mut body = format!(
        "review of PR #{} ({}/{}) at {head8}: {} comment{}. Fix each, then resolve this finding.\n",
        pr.number,
        pr.owner,
        pr.repo,
        comments.len(),
        if comments.len() == 1 { "" } else { "s" }
    );
    for (i, c) in comments.iter().enumerate() {
        let mark = if c.removed { " (removed)" } else { "" };
        let quote: String = c.source.trim().chars().take(QUOTE_CHARS).collect();
        body.push_str(&format!(
            "\n{}. {}:{}{mark}\n> {quote}\n{}\n",
            i + 1,
            c.path,
            c.line,
            c.text
        ));
    }
    let count = body.chars().count();
    if count > REVIEW_BODY_CAP {
        return Err(format!(
            "review is {count} chars (cap {REVIEW_BODY_CAP}): send part of it"
        ));
    }
    Ok(body)
}

/// The note engine argv for one review send, run with the fno-agents binary.
pub fn review_argv(node: &str, graph: &str, pr: &crate::pr_diff::PrRef) -> Vec<String> {
    vec![
        "backlog".into(),
        "note".into(),
        "--graph".into(),
        graph.into(),
        node.into(),
        "--blocking".into(),
        "--stdin".into(),
        "--block-cmd".into(),
        format!("gh api {} (diff)", crate::pr_diff::api_path(pr)),
    ]
}

/// [`run_built`] with the `fno` binary, the path of every board write but the
/// review send.
pub(crate) async fn run_command(args: &[String], stdin: Option<String>) -> (bool, String) {
    let mut command = crate::process_admission::tokio_command(crate::server::fno_bin());
    run_built(&mut command, args, stdin).await
}

/// [`run_built`] with another binary: the review send runs the note engine.
pub(crate) async fn run_command_in(
    bin: std::path::PathBuf,
    args: &[String],
    stdin: Option<String>,
) -> (bool, String) {
    let mut command = crate::process_admission::tokio_command(bin);
    run_built(&mut command, args, stdin).await
}

/// The one bounded shell-out for every board write: argv as an array, off
/// the UI loop, `kill_on_drop`, a 10 s budget. The notice is the last
/// stderr line on a non-zero exit (the command's refusal, verbatim), else the
/// last stdout line, else the updated fallback; the bool is the exit fact
/// so the web bridge can answer `{"ok": false, ...}` without re-reading the
/// text. Sibling of the foreground restart runner
/// ([`crate::client::update_menu::run_restart_foreground`]), which instead
/// inherits stderr so the operator watches the receipts live.
async fn run_built(
    command: &mut tokio::process::Command,
    args: &[String],
    stdin: Option<String>,
) -> (bool, String) {
    use std::process::Stdio;
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .kill_on_drop(true);
    let fut = async {
        let mut child = command.spawn().ok()?;
        if let Some(text) = stdin {
            let mut w = child.stdin.take()?;
            use tokio::io::AsyncWriteExt;
            w.write_all(text.as_bytes()).await.ok()?;
            w.shutdown().await.ok()?;
            drop(w);
        }
        child.wait_with_output().await.ok()
    };
    let output = match tokio::time::timeout(COMMAND_BUDGET, fut).await {
        Ok(Some(o)) => o,
        Ok(None) => return (false, "the command could not start".into()),
        Err(_) => {
            return (
                false,
                format!(
                    "fno {} timed out after 10s; re-read to see if it landed",
                    args.first().map(String::as_str).unwrap_or("verb")
                ),
            )
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return (
            false,
            last_line(&stderr).unwrap_or_else(|| "the command refused".into()),
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let id = serde_json::from_str::<serde_json::Value>(&stdout)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
        });
    if matches!(args, [command, action, ..] if command == "backlog" && action == "idea")
        && id.is_none()
    {
        return (
            false,
            "The command returned no node ID. Check the backlog before retrying.".into(),
        );
    }
    (
        true,
        id.or_else(|| last_line(&stdout))
            .unwrap_or_else(|| "updated".into()),
    )
}

/// The last non-blank line of a command's output.
fn last_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn rank_argv_always_ends_operator_and_needs_an_anchor() {
        let args = rank_argv("x-1", Place::Top, None).expect("top needs no anchor");
        assert_eq!(args.last().map(String::as_str), Some("--operator"));
        assert!(rank_argv("x-1", Place::Before, None).is_err());
        let args = rank_argv("x-1", Place::After, Some("x-2")).expect("anchored after");
        assert_eq!(
            args,
            vec![
                "backlog",
                "rank",
                "x-1",
                "after",
                "--after",
                "x-2",
                "--operator"
            ]
        );
    }

    // AC4/AC8 halves: the shell-out reports the exit fact and the command's own
    // last line. FNO_BIN is process-global, so the existing guard serializes
    // the tests that repoint it.
    #[tokio::test]
    async fn run_command_reports_the_last_stdout_line_on_success() {
        let _guard = crate::pane_send_audit::FNO_BIN_GUARD
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("fno-backlog-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("argv");
        let stub = dir.join("stub.sh");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {}\necho board wrote the card\n",
                record.display()
            ),
        )
        .unwrap();
        let _ = std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755));
        let previous = std::env::var_os("FNO_BIN");
        std::env::set_var("FNO_BIN", &stub);
        let (ok, notice) = run_command(
            &rank_argv("x-1", Place::Top, None).expect("top needs no anchor"),
            None,
        )
        .await;
        std::env::remove_var("FNO_BIN");
        if let Some(prev) = previous {
            std::env::set_var("FNO_BIN", prev);
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok);
        assert_eq!(notice, "board wrote the card");
    }
}

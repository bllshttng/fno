//! Wave notes after their host closes. A wave note (`fno backlog idea
//! --wave-of`) is a finding folded onto a live node. Nothing acts on it, so it
//! stays open here until someone records where it went: `absorbed` by a PR or
//! `refiled` to a node. The record lives in the note's own `disposition` key,
//! so the note itself never moves and `fno backlog get` keeps printing it.

use serde_json::{json, Value};
use std::cell::Cell;
use std::path::Path;

/// The working list starts here. The notes on hosts that closed earlier stay
/// out of it: nobody triaged them, so they read as a report behind `--all`.
const WATCH_FROM: &str = "2026-11-01";

const LIST_COMMAND: &str = "fno backlog list wave-notes";
const DISPOSE_USAGE: &str =
    "fno backlog note dispose <host> <note-ts> (--absorbed <pr> | --refiled <node>)";

const LIST_HELP: &str = "Usage: fno backlog list wave-notes [--all] [--json]

  List wave notes on closed hosts that no PR or node has taken.

Options:
  --all       Also list the notes on hosts that closed before the list started.
  -J, --json  Emit {\"total\": N, \"older\": M, \"rows\": [...]}.
  -h, --help  Show this message and exit.
";

const DISPOSE_HELP: &str =
    "Usage: fno backlog note dispose HOST NOTE_TS (--absorbed PR | --refiled NODE)

  Record where one wave note went. The note stays on its host.

Arguments:
  HOST     The exact id of the node that holds the note.
  NOTE_TS  The note's ts, as `fno backlog list wave-notes` prints it.

Options:
  --absorbed PR    A merged PR handled the finding. PR is its number.
  --refiled NODE   An existing node now holds the finding.
  -h, --help       Show this message and exit.
";

/// The wave rows of one node's thread.
pub(crate) fn wave_rows(row: &Value) -> impl Iterator<Item = &Value> {
    row.get("progress_notes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|note| note.get("kind").and_then(Value::as_str) == Some("wave"))
}

fn undisposed(row: &Value) -> impl Iterator<Item = &Value> {
    wave_rows(row).filter(|note| note.get("disposition").is_none_or(Value::is_null))
}

fn is_terminal(row: &Value) -> bool {
    matches!(
        row.get("status").and_then(Value::as_str),
        Some("done") | Some("superseded")
    ) || row.get("completed_at").is_some_and(|v| !v.is_null())
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The note's title, or its text for a note filed with none.
fn note_title(note: &Value) -> &str {
    match text(note, "title") {
        "" => text(note, "text"),
        title => title,
    }
}

/// A note filed earlier on a host that closes after the start date still
/// counts: its host's close is the moment it can be lost.
fn in_working_list(host: &Value, note: &Value) -> bool {
    // A superseded host often has no completed_at; touched_at is its nearest close time.
    let closed = match text(host, "completed_at") {
        "" => text(host, "touched_at"),
        at => at,
    };
    text(note, "ts") >= WATCH_FROM || closed >= WATCH_FROM
}

/// The undisposed notes on closed hosts: the rows to show, and how many older
/// notes the working list left out.
fn read_set(rows: &[Value], all: bool) -> (Vec<Value>, usize) {
    let mut shown = Vec::new();
    let mut older = 0;
    for host in rows.iter().filter(|row| is_terminal(row)) {
        for note in undisposed(host) {
            if !all && !in_working_list(host, note) {
                older += 1;
                continue;
            }
            shown.push(json!({
                "host": text(host, "id"),
                "project": host.get("project").cloned().unwrap_or(Value::Null),
                "ts": text(note, "ts"),
                "title": note_title(note),
            }));
        }
    }
    (shown, older)
}

/// One stderr line per named host that closed with undisposed notes. A host
/// with none adds nothing, so a close prints the same bytes as before.
pub(crate) fn close_lines(rows: &[Value], closed: &[&str]) -> Vec<String> {
    closed
        .iter()
        .filter_map(|id| rows.iter().find(|row| text(row, "id") == *id))
        .filter_map(|host| {
            let titles: Vec<&str> = undisposed(host).map(note_title).collect();
            if titles.is_empty() {
                return None;
            }
            Some(format!(
                "{} closed with {} undisposed wave note(s): {}. List them: {LIST_COMMAND}. \
                 Dispose each: {DISPOSE_USAGE}",
                text(host, "id"),
                titles.len(),
                titles.join("; "),
            ))
        })
        .collect()
}

/// The close lines for hosts a write just closed. A failed read costs the
/// receipt and never the close: the set still lists the notes.
pub(crate) fn close_lines_at(graph: &Path, closed: &[&str]) -> Vec<String> {
    if closed.is_empty() {
        return Vec::new();
    }
    crate::graph_store::read_rows(graph)
        .map(|rows| close_lines(&rows, closed))
        .unwrap_or_default()
}

#[derive(Debug, PartialEq)]
enum Taken {
    Absorbed(u64),
    Refiled(String),
}

impl Taken {
    fn stamp(&self, at: &str) -> Value {
        match self {
            Taken::Absorbed(pr) => json!({"kind": "absorbed", "pr": pr, "at": at}),
            Taken::Refiled(node) => json!({"kind": "refiled", "node": node, "at": at}),
        }
    }
}

/// Stamp one note. An `Err` names what is missing, and the caller's write then
/// drops, so a refusal writes nothing.
fn dispose(rows: &mut [Value], host: &str, ts: &str, taken: &Taken) -> Result<(), String> {
    if let Taken::Refiled(node) = taken {
        if node == host {
            return Err(format!("{host} cannot take its own note"));
        }
        if !rows.iter().any(|row| text(row, "id") == node) {
            return Err(format!("no node resolves to '{node}'"));
        }
    }
    let row = rows
        .iter_mut()
        .find(|row| text(row, "id") == host)
        .ok_or_else(|| format!("no node resolves to '{host}'"))?;
    let note = row
        .get_mut("progress_notes")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
        .filter(|note| text(note, "kind") == "wave" && text(note, "ts") == ts)
        // Older notes stamp whole seconds, so two can share a ts. An undisposed
        // one goes first, or the second could never be reached.
        .min_by_key(|note| note.get("disposition").is_some_and(|d| !d.is_null()))
        .ok_or_else(|| format!("{host} holds no wave note with ts '{ts}'"))?;
    note.as_object_mut()
        .ok_or_else(|| format!("{host} holds a malformed wave note at '{ts}'"))?
        .insert(
            "disposition".to_string(),
            taken.stamp(&crate::graph_store::now_isoformat()),
        );
    Ok(())
}

/// `fno backlog list wave-notes`. Exits: 0 answered, 1 unreadable graph.
pub fn run_list(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{LIST_HELP}");
        return 0;
    }
    if let Some(other) = tail
        .iter()
        .find(|a| !matches!(a.as_str(), "--all" | "--json" | "-J"))
    {
        eprintln!("Error: unexpected argument '{other}'\n\n{LIST_HELP}");
        return 2;
    }
    let all = tail.iter().any(|a| a == "--all");
    let json_out = tail.iter().any(|a| a == "--json" || a == "-J");
    let rows = match crate::backlog::read_entries(&super::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };
    let (shown, older) = read_set(&rows, all);
    if json_out {
        println!(
            "{}",
            json!({"total": shown.len(), "older": older, "rows": shown})
        );
        return 0;
    }
    if shown.is_empty() {
        println!("no undisposed wave notes");
    }
    for row in &shown {
        println!(
            "{}\t{}\t{}",
            text(row, "host"),
            text(row, "ts"),
            text(row, "title")
        );
    }
    if older > 0 {
        println!("{older} older note(s) left out; --all lists them");
    }
    if !shown.is_empty() {
        println!("Dispose each: {DISPOSE_USAGE}");
    }
    0
}

fn parse_dispose(tail: &[String]) -> Result<(String, String, Taken), String> {
    let mut positional = Vec::new();
    let mut taken = None;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--absorbed" | "--refiled" if taken.is_some() => {
                return Err("--absorbed and --refiled are one choice".to_string());
            }
            "--absorbed" => {
                let value = it.next().ok_or("--absorbed requires a PR number")?;
                let pr = value
                    .trim_start_matches('#')
                    .parse::<u64>()
                    .ok()
                    .filter(|pr| *pr > 0)
                    .ok_or_else(|| format!("--absorbed takes a PR number, not '{value}'"))?;
                taken = Some(Taken::Absorbed(pr));
            }
            "--refiled" => {
                let value = it.next().ok_or("--refiled requires a node id")?;
                taken = Some(Taken::Refiled(value.clone()));
            }
            other if other.starts_with('-') => return Err(format!("No such option: {other}")),
            _ => positional.push(arg.clone()),
        }
    }
    let taken = taken.ok_or("one of --absorbed or --refiled is required")?;
    let [host, ts] = <[String; 2]>::try_from(positional)
        .map_err(|_| "HOST and NOTE_TS are both required".to_string())?;
    Ok((host, ts, taken))
}

/// `fno backlog note dispose`. Exits: 0 recorded, 1 refused or store error,
/// 2 usage.
pub fn run_dispose(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{DISPOSE_HELP}");
        return 0;
    }
    let (host, ts, taken) = match parse_dispose(tail) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("Error: {message}\n\n{DISPOSE_HELP}");
            return 2;
        }
    };
    let graph = super::settings::graph_path();
    match dispose_at(&graph, &host, &ts, &taken) {
        Ok(()) => {
            println!("wave note {ts} on {host} disposed");
            0
        }
        Err(message) => {
            eprintln!("Error: {message}");
            1
        }
    }
}

/// The locked write. A domain refusal returns `Ok(false)` from the closure,
/// which drops the transaction.
fn dispose_at(graph: &Path, host: &str, ts: &str, taken: &Taken) -> Result<(), String> {
    let refused: Cell<Option<String>> = Cell::new(None);
    crate::backlog::mutate_single_row(graph, "wave_note_dispose", |rows| {
        match dispose(rows, host, ts, taken) {
            Ok(()) => Ok(true),
            Err(message) => {
                refused.set(Some(message));
                Ok(false)
            }
        }
    })?;
    match refused.into_inner() {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_host(ts: &str) -> Value {
        json!({"id": "x-host", "status": "done", "completed_at": ts, "progress_notes": [
            {"kind": "progress", "ts": ts, "text": "not a finding"},
            {"kind": "wave", "ts": ts, "title": "alpha beta", "text": "alpha beta"},
        ]})
    }

    const NEW: &str = "2026-11-02T00:00:00+00:00";
    const OLD: &str = "2026-09-17T20:56:08Z";

    #[test]
    fn the_set_lists_a_closed_hosts_note_and_holds_older_ones_for_all() {
        let (shown, older) = read_set(&[closed_host(NEW)], false);
        assert_eq!((shown.len(), older), (1, 0));
        assert_eq!(shown[0]["host"], "x-host");
        assert_eq!(shown[0]["title"], "alpha beta");

        let (shown, older) = read_set(&[closed_host(OLD)], false);
        assert_eq!((shown.len(), older), (0, 1));
        assert_eq!(read_set(&[closed_host(OLD)], true).0.len(), 1);

        // An old note whose host closes inside the window is still at risk.
        let mut late = closed_host(OLD);
        late["completed_at"] = json!(NEW);
        assert_eq!(read_set(&[late], false).0.len(), 1);
        let mut superseded = closed_host(OLD);
        superseded["status"] = json!("superseded");
        superseded["completed_at"] = Value::Null;
        superseded["touched_at"] = json!(NEW);
        assert_eq!(read_set(&[superseded], false).0.len(), 1);

        // A live host's note is not in the set: nothing lost it yet.
        let live = json!({"id": "x-live", "status": "ready", "progress_notes": [
            {"kind": "wave", "ts": NEW, "title": "t"}]});
        assert!(read_set(&[live], true).0.is_empty());
    }

    #[test]
    fn a_close_line_names_the_notes_and_a_clean_host_adds_nothing() {
        let clean = json!({"id": "x-clean", "status": "done"});
        let rows = vec![closed_host(NEW), clean];
        let lines = close_lines(&rows, &["x-host", "x-clean"]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("x-host closed with 1 undisposed wave note(s): alpha beta"));
        assert!(lines[0].contains("fno backlog note dispose"));
        assert!(close_lines(&rows, &["x-clean"]).is_empty());
    }

    #[test]
    fn a_disposal_refuses_a_missing_target_and_writes_nothing() {
        let before = vec![closed_host(NEW)];
        let mut rows = before.clone();
        let error = dispose(&mut rows, "x-host", NEW, &Taken::Refiled("x-gone".into()));
        assert!(error.unwrap_err().contains("x-gone"));
        let error = dispose(&mut rows, "x-host", OLD, &Taken::Absorbed(7));
        assert!(error.unwrap_err().contains(OLD));
        let error = dispose(&mut rows, "x-host", NEW, &Taken::Refiled("x-host".into()));
        assert!(error.unwrap_err().contains("own note"));
        assert_eq!(rows, before);
    }

    /// The real store: extras must survive `comments::save`, and a closed or
    /// migrated row must accept a stamp, since it adds no note.
    #[test]
    fn a_disposal_round_trips_through_the_store_and_leaves_the_note_readable() {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        let other = json!({"id": "x-other", "status": "ready", "title": "other"});
        let mut host = closed_host(NEW);
        host["title"] = json!("host");
        crate::graph_store::seed_rows(&graph, &[host, other]).unwrap();

        dispose_at(&graph, "x-host", NEW, &Taken::Refiled("x-other".into())).unwrap();

        let rows = crate::graph_store::read_rows(&graph).unwrap();
        assert!(read_set(&rows, true).0.is_empty());
        let host = rows.iter().find(|r| text(r, "id") == "x-host").unwrap();
        let note = wave_rows(host).next().unwrap();
        assert_eq!(note["title"], "alpha beta");
        assert_eq!(note["disposition"]["kind"], "refiled");
        assert_eq!(note["disposition"]["node"], "x-other");

        let refused = dispose_at(&graph, "x-host", NEW, &Taken::Refiled("x-gone".into()));
        assert!(refused.unwrap_err().contains("x-gone"));
        let again = crate::graph_store::read_rows(&graph).unwrap();
        let host = again.iter().find(|r| text(r, "id") == "x-host").unwrap();
        assert_eq!(
            wave_rows(host).next().unwrap()["disposition"]["node"],
            "x-other"
        );
    }
}

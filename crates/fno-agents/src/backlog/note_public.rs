//! The public `fno backlog note` face: evidence gate, archived refusal, the
//! reader walk, the thread append, and the pointer delivery. Readers resolve
//! BEFORE the append: nobody bound refuses and writes nothing. The contract
//! lives in docs/architecture/backlog-graph-command-contracts.md.

use serde_json::Value;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use super::node_ref::resolve_tiers;
use super::settings;
use super::workflows::{archived_entry, text_at};
use crate::graph_store;
use crate::spawn_context::session_identity_key;
use crate::worktree::OWNERSHIP_LIVE_STATUSES;

const POINTER_WORDS: usize = 20;
const SEND_TIMEOUT_SECONDS: u64 = 30;
const GRAPH_FIELDS: [&str; 3] = ["locked_by_harness_session", "session_id", "locked_by"];
const QUIET_HINT: &str =
    "Write it anyway with --quiet, or find a reader with fno agents team and mail them by name.";
const LONG_NOTE_FACTOR: usize = 4;

pub(super) const NOTE_HELP: &str = "Usage: fno backlog note <id> [TEXT] [OPTIONS]

Append a note to the node's thread (one feed, oldest first). A note is a
comment row stamped with this session's identity. It never replaces
current_state. Read the feed: fno backlog note comment <id> --list

Options:
      --body-file PATH  Read the text from a file ('-' = stdin).
      --kind KIND       progress, finding, ruling, or collision. Default progress.
  -q, --quiet           Write it and mail nobody.
  -J, --json            Emit the receipt as JSON.
      --read COMMAND    A command that produced a code fact in this note. It runs
                        at record time and its output is stored. Repeatable.
      --blocking        Record a blocking finding.
      --resolve ID      Resolve a blocking finding.
  -h, --help            Print help.

Exit codes: 1 refused, 2 usage, 3 nobody bound (nothing written), 4 no send confirmed.
";

struct PublicArgs {
    node: Option<String>,
    text: Option<String>,
    body_file: Option<String>,
    quiet: bool,
    json_out: bool,
    reads: Vec<String>,
    kind: Option<String>,
    replace: bool,
}

/// A dash-led token in the body slot stays a body: `note <id> --list` must
/// reach the flag-shaped refusal, which names the read it meant.
fn parse_public(tail: &[String]) -> Result<PublicArgs, String> {
    let mut out = PublicArgs {
        node: None,
        text: None,
        body_file: None,
        quiet: false,
        json_out: false,
        reads: Vec::new(),
        kind: None,
        replace: false,
    };
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--body-file" => out.body_file = Some(value("--body-file")?),
            "--kind" => out.kind = Some(value("--kind")?),
            "--read" => out.reads.push(value("--read")?),
            "--quiet" | "-q" => out.quiet = true,
            "--json" | "-J" => out.json_out = true,
            "--replace" => out.replace = true,
            other if out.node.is_none() => {
                if other.starts_with('-') {
                    return Err(format!("unknown flag {other}"));
                }
                out.node = Some(other.to_string());
            }
            other if out.text.is_none() => out.text = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(out)
}

fn read_text(args: &PublicArgs) -> Result<String, String> {
    let raw = match (&args.body_file, &args.text) {
        (Some(_), Some(_)) => return Err("pass the note text or --body-file, not both".into()),
        (Some(path), None) if path == "-" => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map_err(|e| format!("stdin read failed: {e}"))?;
            s
        }
        (Some(path), None) => {
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?
        }
        (None, Some(text)) => text.clone(),
        (None, None) => String::new(),
    };
    Ok(raw.trim().to_string())
}

#[derive(Debug, Default)]
struct NoteReaders {
    node_id: String,
    recipients: Vec<(String, String)>,
    author_bound: Option<String>,
    readings: Vec<String>,
}

fn claim_holder(node_id: &str) -> Option<String> {
    let (state, record) = crate::claims::status(&format!("node:{node_id}"), None);
    if !matches!(
        state,
        crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
    ) {
        return None;
    }
    record.map(|r| r.holder).filter(|h| !h.is_empty())
}

fn row_name(row: &Value) -> &str {
    row.get("name").and_then(Value::as_str).unwrap_or("")
}

fn row_live(row: &Value) -> bool {
    row.get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| OWNERSHIP_LIVE_STATUSES.contains(&s))
}

/// The agent behind a claim holder, or None: a role holder is a marker, not an
/// address, and no row behind it means nobody is there.
fn holder_agent_name(holder: &str, rows: &[Value]) -> Option<String> {
    if holder.is_empty() {
        return None;
    }
    if let Some(name) = holder.strip_prefix("spawn-handover:") {
        return rows
            .iter()
            .any(|r| row_name(r) == name)
            .then(|| name.to_string());
    }
    if holder.starts_with("target-session:") || holder.starts_with("blueprint-session:") {
        let sid = holder.split_once(':')?.1;
        return rows
            .iter()
            .find(|r| {
                ["harness_session_id", "session_id", "cc_session_id"]
                    .iter()
                    .any(|k| r.get(*k).and_then(Value::as_str) == Some(sid))
            })
            .map(|r| row_name(r).to_string());
    }
    Some(holder.to_string())
}

struct Walk<'a> {
    rows: &'a [Value],
    self_session: Option<&'a str>,
    out: NoteReaders,
}

impl Walk<'_> {
    /// True only when a live RECIPIENT joined; an author hit is never mailed.
    fn add(&mut self, address: &str, why: &str) -> bool {
        let seen = |out: &NoteReaders, a: &str| out.recipients.iter().any(|(n, _)| n == a);
        if address.is_empty() || seen(&self.out, address) {
            return false;
        }
        let Some(resolved) = holder_agent_name(address, self.rows) else {
            return false;
        };
        if seen(&self.out, &resolved) {
            return false;
        }
        if let Some(me) = self.self_session {
            let sid = self
                .rows
                .iter()
                .find(|r| row_name(r) == resolved)
                .and_then(|r| r.get("harness_session_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            // A row-backed address decides by identity key; a name suffix
            // stands only when no row is behind the name.
            let author = match sid {
                Some(sid) => session_identity_key(sid) == session_identity_key(me),
                None => resolved.ends_with(me) || address.ends_with(me),
            };
            if author {
                self.out.author_bound.get_or_insert_with(|| why.to_string());
                return false;
            }
        }
        self.out.recipients.push((resolved, why.to_string()));
        true
    }

    /// The ownership-live row behind a graph binding, by name or identity.
    fn bound_row(&self, value: &str) -> Option<String> {
        let name = holder_agent_name(value, self.rows);
        let by_name = self
            .rows
            .iter()
            .find(|r| row_live(r) && Some(row_name(r)) == name.as_deref());
        let needle = session_identity_key(value);
        by_name
            .or_else(|| {
                self.rows.iter().find(|r| {
                    row_live(r)
                        && r.get("harness_session_id")
                            .and_then(Value::as_str)
                            .is_some_and(|c| session_identity_key(c) == needle)
                })
            })
            .map(|r| row_name(r).to_string())
    }

    fn worker_readers(
        &mut self,
        subject_id: &str,
        subject: &str,
        source: Option<&Value>,
        holder_of: &dyn Fn(&str) -> Option<String>,
    ) {
        let holder = holder_of(subject_id);
        self.out.readings.push(format!(
            "claim node:{subject_id}: {}",
            holder.as_deref().unwrap_or("free")
        ));
        if let Some(holder) = &holder {
            if self.add(holder, &format!("holder of {subject}")) {
                return;
            }
        }
        for field in GRAPH_FIELDS {
            let value = source
                .and_then(|s| s.get(field))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty());
            let Some(value) = value else {
                self.out.readings.push(format!("graph {field}: none"));
                continue;
            };
            let row = self.bound_row(value);
            let suffix = row
                .as_ref()
                .map_or("names no live row".to_string(), |n| format!("-> {n}"));
            self.out
                .readings
                .push(format!("graph {field}: {value} {suffix}"));
            if let Some(name) = row {
                if self.add(
                    &name,
                    &format!("session bound to {subject} (graph {field})"),
                ) {
                    return;
                }
            }
        }
        let mut named: Vec<String> = self
            .rows
            .iter()
            .filter(|r| row_live(r) && r.get("node").and_then(Value::as_str) == Some(subject_id))
            .map(|r| row_name(r).to_string())
            .collect();
        named.sort();
        self.out.readings.push(if named.is_empty() {
            format!("registry: no live row names {subject_id}")
        } else {
            format!("registry: {}", named.join(", "))
        });
        for name in named {
            self.add(&name, &format!("worker on {subject} (registry node)"));
        }
    }
}

/// Every bound reader for one note; the author is named, never mailed.
fn note_readers(
    entry: &Value,
    owner: Option<&Value>,
    rows: &[Value],
    holder_of: &dyn Fn(&str) -> Option<String>,
    leads_of: &dyn Fn(&str) -> Result<Vec<String>, String>,
    self_session: Option<&str>,
) -> NoteReaders {
    let node_id = text_at(entry, "id").unwrap_or("").to_string();
    let mut walk = Walk {
        rows,
        self_session,
        out: NoteReaders {
            node_id: node_id.clone(),
            ..Default::default()
        },
    };
    walk.worker_readers(&node_id, &node_id, Some(entry), holder_of);
    let parent = text_at(entry, "parent").filter(|p| !p.is_empty());
    let owner_id = text_at(entry, "contained_in")
        .filter(|c| !c.is_empty())
        .or(parent);
    if let Some(owner_id) = owner_id {
        let subject = format!("owner {owner_id}");
        walk.worker_readers(owner_id, &subject, owner, holder_of);
    }
    // Role walk, nearest first; it stops at the first scope with a live role.
    let epic = owner
        .and_then(|o| text_at(o, "parent"))
        .filter(|p| !p.is_empty())
        .or(parent);
    let mut scopes: Vec<(String, String, String)> = Vec::new();
    if text_at(entry, "type") == Some("epic") {
        scopes.push((
            node_id.clone(),
            format!("lead of {node_id}"),
            format!("role {node_id}"),
        ));
    }
    if let Some(epic) = epic {
        scopes.push((
            epic.to_string(),
            format!("lead of {epic}"),
            format!("role {epic}"),
        ));
    }
    if let Some(project) = text_at(entry, "project").filter(|p| !p.is_empty()) {
        scopes.push((
            project.to_string(),
            format!("lead of {project} (project)"),
            format!("role {project} (project)"),
        ));
    }
    for (scope, why, label) in scopes {
        let leads = match leads_of(&scope) {
            Ok(leads) => leads,
            Err(e) => {
                walk.out.readings.push(format!("{label}: unreadable ({e})"));
                continue;
            }
        };
        if leads.is_empty() {
            walk.out.readings.push(format!("{label}: vacant"));
            continue;
        }
        walk.out
            .readings
            .push(format!("{label}: {}", leads.join(", ")));
        for lead in &leads {
            walk.add(lead, &why);
        }
        break;
    }
    walk.out
}

fn refused(head: &str, readings: &[String]) -> String {
    let trail: String = readings.iter().map(|r| format!("\n  {r}")).collect();
    format!("{head}so nothing was written.{trail}\n{QUIET_HINT}")
}

/// One line naming the node and the note's opening, never the note itself.
fn pointer(node_id: &str, text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let words: Vec<&str> = first.split_whitespace().collect();
    let mut head = words
        .iter()
        .take(POINTER_WORDS)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if words.len() > POINTER_WORDS {
        head.push_str("...");
    }
    format!("note on {node_id}: {head} Read: fno backlog get {node_id}")
}

/// One wall-clock-bounded send. A live inject waits on the recipient's flock,
/// so a slow one answers UNCONFIRMED: it may still land.
fn send_pointer(address: &str, why: &str, body: &str) -> String {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return format!("notify FAILED {address} ({why}): {e}"),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["machine-mail-send", "--arm", "note-pointer"]);
    cmd.args(["--timeout-secs", &SEND_TIMEOUT_SECONDS.to_string()]);
    cmd.args(["--to", address, "--", body]);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cmd.output());
    });
    match rx.recv_timeout(Duration::from_secs(SEND_TIMEOUT_SECONDS)) {
        Ok(Ok(out)) if out.status.success() => {
            let said = String::from_utf8_lossy(&out.stdout);
            format!("notified {address} ({why}): {}", said.trim())
        }
        Ok(Ok(out)) => {
            let said = String::from_utf8_lossy(&out.stderr);
            let detail = match said.trim() {
                "" => format!("machine-mail-send exited {:?}", out.status.code()),
                text => text.to_string(),
            };
            format!("notify FAILED {address} ({why}): {detail}")
        }
        Ok(Err(e)) => format!("notify FAILED {address} ({why}): {e}"),
        Err(_) => {
            format!("notify UNCONFIRMED {address} ({why}): no answer in {SEND_TIMEOUT_SECONDS}s")
        }
    }
}

fn deliver(readers: &NoteReaders, text: &str, json_out: bool) -> i32 {
    let emit = |line: &str, to_err: bool| {
        if to_err {
            eprintln!("{line}");
        } else {
            super::receipt::emit_line(line);
        }
    };
    if readers.recipients.is_empty() {
        emit(
            &format!(
                "notify: you are the only reader bound to {} ({}); nobody else to tell",
                readers.node_id,
                readers.author_bound.as_deref().unwrap_or("")
            ),
            json_out,
        );
        return 0;
    }
    let body = pointer(&readers.node_id, text);
    let (mut notified, mut unconfirmed, mut failed) = (0, 0, 0);
    for (address, why) in &readers.recipients {
        let line = send_pointer(address, why, &body);
        let undelivered = !line.starts_with("notified ");
        if line.starts_with("notify UNCONFIRMED") {
            unconfirmed += 1;
        } else if undelivered {
            failed += 1;
        } else {
            notified += 1;
        }
        emit(&line, undelivered || json_out);
    }
    if notified > 0 {
        return 0;
    }
    eprintln!(
        "notify: {} is noted, but no reader confirmed delivery ({unconfirmed} UNCONFIRMED, \
         {failed} FAILED). An UNCONFIRMED send may still land, so check before you re-send.",
        readers.node_id
    );
    4
}

/// The live row, the alias and slug tiers included. The by-id read answers
/// the hot path; the whole read answers what it misses. `Err` is an
/// unreadable store: the reader walk then refuses, never guesses.
fn live_entry(graph: &Path, query: &str) -> Result<Option<Value>, String> {
    let is_live = |row: &Value| row.get("archived_at").is_none_or(Value::is_null);
    if let Ok(Some(row)) = graph_store::read_one(graph, query) {
        if is_live(&row) {
            return Ok(Some(row));
        }
    }
    let rows: Vec<Value> = graph_store::read_rows(graph)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|r| is_live(r))
        .collect();
    Ok(resolve_tiers(&rows, query).cloned())
}

fn own_session() -> Option<String> {
    crate::spawn_context::resolve_self_identity(
        &|k| std::env::var(k).ok(),
        None,
        None,
        &crate::paths::AgentsHome::from_env(),
    )
    .session_id
    .filter(|s| !s.is_empty())
}

fn readers_before_append(entry: &Value, graph: &Path) -> Result<NoteReaders, String> {
    let task = text_at(entry, "id").unwrap_or("");
    let cannot = |e: String| {
        refused(
            &format!("note refused: could not read who is bound to {task} ({e}), "),
            &[],
        )
    };
    let registry = crate::paths::AgentsHome::shared_registry_json();
    let rows = crate::client_commands::load_registry_entries(&registry)
        .map_err(|e| cannot(e.to_string()))?;
    let owner_id = text_at(entry, "contained_in")
        .filter(|c| !c.is_empty())
        .or_else(|| text_at(entry, "parent").filter(|p| !p.is_empty()));
    let owner = match owner_id {
        Some(id) => graph_store::read_one(graph, id).map_err(|e| cannot(e.to_string()))?,
        None => None,
    };
    let session = own_session();
    let leads = |scope: &str| {
        crate::mail_cli::send::lanes::resolve_to_lead(scope).map_err(|(message, _)| message)
    };
    let readers = note_readers(
        entry,
        owner.as_ref(),
        &rows,
        &claim_holder,
        &leads,
        session.as_deref(),
    );
    if readers.recipients.is_empty() && readers.author_bound.is_none() {
        return Err(refused(
            &format!(
                "note refused: nobody bound to {} would be told, ",
                readers.node_id
            ),
            &readers.readings,
        ));
    }
    Ok(readers)
}

/// `fno backlog note`: the engine door keeps the finding and import routes.
pub fn run(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{NOTE_HELP}");
        return 0;
    }
    let graph = settings::graph_path();
    let engine = tail
        .iter()
        .any(|a| matches!(a.as_str(), "--blocking" | "--resolve" | "--import-record"));
    if engine {
        let mut argv = vec!["--graph".to_string(), graph.display().to_string()];
        argv.extend_from_slice(tail);
        return super::note_cli::run_note(&argv);
    }
    let args = match parse_public(tail) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("Error: {e}\n\n{NOTE_HELP}");
            return 2;
        }
    };
    let Some(task_id) = args.node.clone() else {
        eprint!("{NOTE_HELP}");
        return 2;
    };
    if args.replace {
        eprintln!(
            "--replace is retired: a note appends to the thread and cannot clobber anything. \
             Read the feed: fno backlog note comment {task_id} --list"
        );
        return 3;
    }
    let text = match read_text(&args) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    // A body that looks like a flag is a mistyped flag, not a note: a bare
    // `note <id> --list` once wrote the literal text over state. File bodies
    // are deliberate and exempt.
    if args.body_file.is_none() && text.starts_with("--") {
        eprintln!(
            "Error: note refused: the body starts with '--' ({text}), so it is a mistyped \
             flag, not a note. Nothing was written. Read the feed: `fno backlog notes history \
             {task_id}` or `fno backlog get {task_id}`. To write flag-shaped text, pass --body-file."
        );
        return 2;
    }
    let root = crate::law_match::evidence_repo_root();
    let mut runner = |cmd: &str, root: &Path| crate::evidence::shell_run(cmd, root, 20);
    let (read_rows, claims) =
        match crate::evidence::note_evidence(&text, &args.reads, &root, &mut runner) {
            Ok(answer) => answer,
            Err(gate) => {
                eprintln!("Error: note refused: {}", gate.message);
                return 1;
            }
        };
    let entry = match live_entry(&graph, &task_id) {
        Ok(found) => found,
        Err(e) if args.quiet => {
            eprintln!("Error: the backlog graph could not be read: {e}");
            return 1;
        }
        Err(e) => {
            eprintln!(
                "{}",
                refused(
                    &format!("note refused: could not read who is bound to {task_id} ({e}), "),
                    &[]
                )
            );
            return 3;
        }
    };
    let Some(entry) = entry else {
        if let Some(archived) = archived_entry(&task_id) {
            let aid = text_at(&archived, "id").unwrap_or(&task_id);
            eprintln!(
                "Error: node {aid} is archived; run `fno backlog unarchive {aid}` to restore it before updating."
            );
        } else {
            eprintln!("Error: no node resolves to '{task_id}'");
        }
        return 1;
    };
    let readers = if args.quiet {
        None
    } else {
        match readers_before_append(&entry, &graph) {
            Ok(readers) => Some(readers),
            Err(message) => {
                eprintln!("{message}");
                return 3;
            }
        }
    };
    let code = super::note_cli::write_public(
        &graph,
        &entry,
        text.clone(),
        args.kind.clone(),
        read_rows.map(|rows| Value::Array(rows).to_string()),
        args.json_out,
    );
    if code != 0 {
        return code;
    }
    if let Some(claim) = claims.as_ref().and_then(|c| c.first()) {
        eprintln!(
            "note appended with an unmeasured code fact ('{claim}'): a reader cannot tell \
             measured from assumed. Attach --read <command> - it runs at record time and its \
             output is stored; pair a zero with a control."
        );
    }
    let count = super::style_check::word_count(&text);
    if count > super::style_check::configured_word_cap() * LONG_NOTE_FACTOR {
        eprintln!(
            "note appended ({count} words). Long evidence belongs in a plan doc; a note \
             carrying a path is cheaper for every later reader."
        );
    }
    match readers {
        Some(readers) => deliver(&readers, &text, args.json_out),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn free(_: &str) -> Option<String> {
        None
    }

    fn no_lead(_: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    #[test]
    fn the_author_is_named_and_never_mailed() {
        let rows = vec![
            json!({"name": "me", "status": "busy", "harness_session_id": "sess-1"}),
            json!({"name": "peer", "status": "busy", "node": "x-1", "harness_session_id": "sess-2"}),
        ];
        let entry = json!({"id": "x-1", "session_id": "sess-1"});
        let found = note_readers(&entry, None, &rows, &free, &no_lead, Some("sess-1"));
        assert_eq!(
            found.author_bound.as_deref(),
            Some("session bound to x-1 (graph session_id)")
        );
        assert_eq!(found.recipients.len(), 1);
        assert_eq!(found.recipients[0].0, "peer");
    }

    #[test]
    fn the_role_walk_stops_at_the_first_scope_with_a_live_role() {
        let entry = json!({"id": "x-1", "parent": "x-e", "project": "fno"});
        let leads = |scope: &str| match scope {
            "x-e" => Ok(vec!["epic-lead".to_string()]),
            _ => Ok(vec!["project-lead".to_string()]),
        };
        let found = note_readers(&entry, None, &[], &free, &leads, None);
        let names: Vec<&str> = found.recipients.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["epic-lead"]);
    }
}

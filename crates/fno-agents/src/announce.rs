//! Fleet announcements: one announcement is one bus line, and every session
//! reads it by its own cursor.
//!
//! `mail team` used to fan out one `dispatch_send` per recipient (37 sends in
//! 19 minutes at fleet size, no proof anyone read a copy). One announcement is
//! instead ONE `kind: "announce"` line on the shared bus, appended under the
//! same sidecar flock Python uses. Every session reads it through its own
//! per-session seen-id cursor at its next hook boundary; the sender reads
//! receipts (`audience / landed / pending / woken / unverified / late`).
//!
//! Subcommands (all direct dispatch, no daemon RPC):
//!   - `announce send`   - write the one line (authority + rate limit + supersede)
//!   - `announce read`   - render unseen standing announcements for one session
//!   - `announce status` - the sender's receipt view for one announcement
//!
//! Envelope key order mirrors `bus/log.py::to_json_line` so the Python reader
//! parses Rust-written lines unchanged (`from_json_line` round-trip is tested).
//! `to: "fleet:<scope>"` never equals a Python address, so `scan_unread` and
//! every addressed-mail reader cannot deliver it; `kind: "announce"` is not a
//! control kind, it is simply never addressed to a session.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::mail::envelope::ENVELOPE_VERSION;
use crate::mail::log::read_bus_segments;
use crate::store_remote::{Remote, SqlValue};

/// Terminal registry statuses (registry.py::TERMINAL_STATUSES). `pub(crate)`
/// so other JSON-row readers in this crate (`team_settle`) share the one
/// string-matched copy instead of re-declaring it.
pub(crate) const TERMINAL_STATUSES: &[&str] = &["exited", "orphaned", "failed", "permanent_dead"];

const ANNOUNCE_KIND: &str = "announce";
const LANDED_KIND: &str = "landed";
const WAKE_KIND: &str = "announce-wake";

/// Rate limit: 6 announcements per sender per rolling hour, counted from the
/// bus itself (no second state file to drift).
const HOURLY_LIMIT: usize = 6;
const DEFAULT_EXPIRES: &str = "24h";
const MAX_EXPIRES: Duration = Duration::from_secs(7 * 24 * 3600);

/// Paths this command reads/writes, resolved once so tests can pin every root
/// without env mutation.
pub(crate) struct AnnouncePaths {
    bus_live: PathBuf,
    registry: PathBuf,
    /// The chats store the mirror seam records into. Env constructors take
    /// the config ladder (`chats_dir()`); the test fixture pins its own root,
    /// so a fixture send can never mirror into the live store.
    chats_dir: PathBuf,
    /// `~/.fno` (the Python `paths.state_dir()`); holds `announce-cursors/`.
    state_root: PathBuf,
}

impl AnnouncePaths {
    pub(crate) fn from_env() -> Self {
        let home = crate::paths::AgentsHome::from_env();
        let dot_fno = home
            .root()
            .parent()
            .unwrap_or_else(|| home.root())
            .to_path_buf();
        Self {
            bus_live: crate::mail_paths::bus_log_path(&dot_fno),
            registry: home.registry_json(),
            chats_dir: crate::chats::chats_dir(),
            state_root: dot_fno,
        }
    }

    /// `None` under a test process with no declared home - in-process callers
    /// (nudge.rs) skip the announce read instead of panicking on the fence.
    pub(crate) fn from_env_opt() -> Option<Self> {
        let home = crate::paths::AgentsHome::from_env_opt()?;
        let dot_fno = home
            .root()
            .parent()
            .unwrap_or_else(|| home.root())
            .to_path_buf();
        Some(Self {
            bus_live: crate::mail_paths::bus_log_path(&dot_fno),
            registry: home.registry_json(),
            chats_dir: crate::chats::chats_dir(),
            state_root: dot_fno,
        })
    }
}

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

pub(crate) fn row_str<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// The primary's announce table: one row per announcement, the same JSON the
/// local bus line holds. With `store.remote_url` set, a send writes here and
/// every machine's read merges these rows with its own bus, so an announcement
/// made on one machine reaches sessions on another.
const ANNOUNCE_TABLE: &str = "fleet_announce";

/// How many rows the primary keeps: expiry governs standing-ness, this cap
/// only bounds the table. Local retention is size-based (5 MB, 5 segments);
/// a row count well past any expiry window is the same answer over SQL.
const ANNOUNCE_PRIMARY_CAP: i64 = 200;

/// The announce row store: the local bus is always the writer's first copy;
/// the primary carries the row to every other machine. Absent
/// `store.remote_url` both arms are local-only and nothing changes.
struct AnnounceStore {
    primary: Option<Remote>,
}

impl AnnounceStore {
    fn from_env() -> Result<Self, String> {
        Ok(Self {
            primary: crate::store_remote::configured()?,
        })
    }

    /// The primary's rows oldest -> newest. A primary that never saw a send
    /// has no table: no rows. Every other failure is the caller's to surface.
    fn rows(&self) -> Result<Vec<Value>, String> {
        let Some(remote) = &self.primary else {
            return Ok(Vec::new());
        };
        let label = format!("the {ANNOUNCE_TABLE} rows on {}", remote.url());
        let reply = match remote.execute(
            &format!("SELECT record FROM {ANNOUNCE_TABLE} ORDER BY id"),
            &[],
        ) {
            Ok(reply) => reply,
            Err(e) if e.contains("no such table") => return Ok(Vec::new()),
            Err(e) => return Err(format!("read {label}: {e}")),
        };
        Ok(reply
            .rows
            .iter()
            .filter_map(|row| row.first())
            .filter_map(|cell| cell.text())
            .map(|text| serde_json::from_str::<Value>(text))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{label}: unparsable row: {e}"))?)
    }

    /// Insert one announcement row and bound the table. A failure fails the
    /// send: the row would be machine-local, which is the bug this store
    /// exists to close.
    fn insert(&self, row: &Value) -> Result<(), String> {
        let Some(remote) = &self.primary else {
            return Ok(());
        };
        remote.script(&format!(
            "CREATE TABLE IF NOT EXISTS {ANNOUNCE_TABLE} (\
                 id INTEGER PRIMARY KEY AUTOINCREMENT, \
                 record TEXT NOT NULL)"
        ))?;
        let text = serde_json::to_string(row).map_err(|e| format!("serialize: {e}"))?;
        remote.execute(
            &format!("INSERT INTO {ANNOUNCE_TABLE} (record) VALUES (?)"),
            &[SqlValue::Text(text)],
        )?;
        // Best-effort bound: a retention failure must not fail a sent row.
        let _ = remote.execute(
            &format!(
                "DELETE FROM {ANNOUNCE_TABLE} WHERE id NOT IN (\
                     SELECT id FROM {ANNOUNCE_TABLE} ORDER BY id DESC LIMIT {ANNOUNCE_PRIMARY_CAP})"
            ),
            &[],
        );
        Ok(())
    }
}

/// The rows both announce surfaces scan: the local bus segments merged with
/// the primary's rows, deduped by id (the sender's own machine holds both
/// copies) and ordered oldest -> newest by ts, so the supersede and
/// standing-announcements folds see one timeline.
fn read_announce_rows(live: &Path, store: &AnnounceStore) -> Result<Vec<Value>, String> {
    let mut out = read_bus_segments(live);
    out.extend(store.rows()?);
    let mut seen: HashSet<String> = HashSet::new();
    out.retain(|row| match row_str(row, "id") {
        Some(id) => seen.insert(id.to_string()),
        None => true,
    });
    out.sort_by_key(|row| {
        row_str(row, "ts")
            .and_then(parse_iso)
            .map(|t| t.timestamp_millis())
            .unwrap_or(i64::MAX)
    });
    Ok(out)
}

fn row_session_id(row: &Value) -> Option<String> {
    let sid = row_str(row, "harness_session_id")
        .filter(|sid| !sid.is_empty())
        .unwrap_or_else(|| {
            let harness = row_str(row, "harness").unwrap_or("");
            crate::client_commands::resume_session_id(row, harness)
        });
    (!sid.is_empty()).then(|| sid.to_string())
}

pub(crate) fn row_terminal(row: &Value) -> bool {
    let status = row_str(row, "status").unwrap_or("live");
    TERMINAL_STATUSES.contains(&status)
}

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn parse_iso(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Parse a `--expires` duration: `45m` / `24h` / `7d`.
fn parse_expires(raw: &str) -> Result<Duration, String> {
    let (num, unit) = raw.split_at(raw.len().saturating_sub(1));
    let mult = match unit {
        "m" => 60u64,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(format!("bad --expires {raw:?}: use m/h/d (e.g. 24h)")),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("bad --expires {raw:?}: use m/h/d (e.g. 24h)"))?;
    Ok(Duration::from_secs(n * mult))
}

/// Append one line under the sidecar flock, with the Python appender's
/// rotation and owner-only mode (the bus-append port, d-697ea9c4). A bare
/// `fno` sender refuses before the write, so a refused send leaves no bus
/// row.
pub(crate) fn append_line(live: &Path, obj: &Value, chats_dir: &Path) -> Result<(), String> {
    if let Some(from) = obj.get("from").and_then(Value::as_str) {
        crate::system_sender::guard_sender(from)?;
    }
    append_line_open(live, obj, chats_dir)
}

/// A test build must never touch the operator's mail store. Same rule as
/// [`fence_test_bus`], on both files the append writes: the bus line and the
/// chats mirror. A fixture that passes explicit paths but leaves the env
/// unpinned used to leak the mirror into `~/.fno/chats` silently.
fn fence_test_store(path: &Path) -> Result<(), String> {
    if !cfg!(test) {
        return Ok(());
    }
    if crate::paths::under_temp_dir(path) {
        return Ok(());
    }
    Err(format!(
        "append fenced under test: {} is not under the temp dir; pin FNO_AGENTS_HOME to a temp home in the test",
        path.display()
    ))
}

/// The append without the announce sender guard: the Python appender's
/// parity surface, where the caller owns sender validation and a tested
/// send from the bare fleet name lands as-is. `chats_dir` is the store the
/// chat mirror records into - always pass the dir that belongs to the same
/// root as `live`, so a fixture bus gets a fixture chats store.
pub(crate) fn append_line_open(live: &Path, obj: &Value, chats_dir: &Path) -> Result<(), String> {
    fence_test_store(live)?;
    fence_test_store(chats_dir)?;
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
    let has_delivery = obj
        .get("delivery")
        .and_then(Value::as_str)
        .is_some_and(|d| !d.is_empty());
    // The record seam (plan R2): message kinds record BEFORE the bus write
    // and fail closed (what was never recorded is never sent); receipts
    // (`landed`, or an envelope carrying a delivery mark) record AFTER the
    // bus write and warn only - a receipt must never break an ack flow.
    // The fold reads the live bus's own dir, so an overridden bus dir
    // (FNO_BUS_DIR, paths.bus_dir) gives up its stray chats too.
    if let Some(stray_root) = live.parent() {
        crate::chats::fold_stray_chats(chats_dir, stray_root);
    }
    if crate::chats::is_message_kind(kind) && !has_delivery {
        crate::chats::record(chats_dir, obj)?;
    }
    crate::mail::log::append_raw(live, obj)?;
    if kind == "landed" || kind == crate::mail::state::STATE_KIND || has_delivery {
        if let Err(e) = crate::chats::record(chats_dir, obj) {
            eprintln!("chats record (receipt) failed: {e}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scope matching: the fleet recipient rule
// ---------------------------------------------------------------------------

/// Which team answers to `scope`: one rule shared with the walk, blank
/// answers false. `project:<p>` rides the same rule. An unreadable project
/// map answers equality only: without it a portfolio reads as an epic set
/// and would answer for each of its projects.
pub(crate) fn team_answers(
    held: Option<&str>,
    requested: &str,
    projects: Option<&HashMap<String, String>>,
) -> bool {
    held.is_some_and(|h| {
        !h.is_empty()
            && match projects {
                Some(map) => crate::loop_lead::team_answers_to(h, requested, map),
                None => crate::loop_lead::same_territory(h, requested, &HashMap::new()),
            }
    })
}

/// Is this session inside the announcement's audience? The SNAPSHOT question
/// (membership list written at send time).
fn in_audience(audience: &[Value], session_id: &str) -> bool {
    audience.iter().filter_map(Value::as_str).any(|a| {
        matches!(
            crate::identity::session_handle_tier(a, session_id),
            Some(0 | 1)
        )
    })
}

/// Does this session match the scope at READ time (the late-arrival rule)?
fn matches_scope_now(
    scope: &str,
    session_id: &str,
    registry: &[Value],
    projects: Option<&HashMap<String, String>>,
) -> bool {
    if scope == "all" {
        return true;
    }
    registry
        .iter()
        .filter(|row| !row_terminal(row))
        .filter(|row| {
            row_session_id(row)
                .map(|sid| {
                    matches!(
                        crate::identity::session_handle_tier(session_id, &sid),
                        Some(0 | 1)
                    )
                })
                .unwrap_or(false)
        })
        .any(|row| match scope {
            "leads" => row.get("role_level").map(|c| !c.is_null()).unwrap_or(false),
            _ => team_answers(row_str(row, "role_scope"), scope, projects),
        })
}

fn resolve_audience(
    scope: &str,
    registry: &[Value],
    projects: Option<&HashMap<String, String>>,
) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = Vec::new(); // (key, name)
    for row in registry {
        if row_terminal(row) {
            continue;
        }
        let Some(sid) = row_session_id(row) else {
            continue;
        };
        if scope == "leads" && row.get("role_level").map(|c| c.is_null()).unwrap_or(true) {
            continue;
        }
        if scope != "all" && scope != "leads" {
            let held = row_str(row, "role_scope");
            let team_ok = team_answers(held, scope, projects);
            // project:<p> also matches rows WORKING in that project (their cwd
            // names the repo), so an announcement reaches the team, not only a
            // team that may not exist.
            let project_ok = scope.strip_prefix("project:").is_some_and(|p| {
                !p.is_empty() && row_str(row, "cwd").is_some_and(|cwd| cwd_contains_project(cwd, p))
            });
            if !team_ok && !project_ok {
                continue;
            }
        }
        let name = row_str(row, "name").unwrap_or("").to_string();
        pairs.push((crate::spawn_context::session_identity_key(&sid), name));
    }
    pairs.sort();
    pairs.dedup();
    pairs.into_iter().map(|(k, _)| k).collect()
}

/// Does a registry cwd sit inside project `p`? Cheap path-prefix answer: the
/// project's slug appears as a path segment of the checkout (e.g.
/// `.../footnote`, `.../worktrees/footnote/<branch>`). Best-effort by design; the
/// audience is a snapshot, `announce status` reports what actually landed.
fn cwd_contains_project(cwd: &str, project: &str) -> bool {
    Path::new(cwd)
        .components()
        .any(|c| c.as_os_str().to_string_lossy() == project)
}

// ---------------------------------------------------------------------------
// `announce send`
// ---------------------------------------------------------------------------

struct SendArgs {
    scope: String,
    subject: String,
    expires_raw: String,
    urgent: bool,
    from: String,
    sender_kind: String,
    from_session: Option<String>,
    json_out: bool,
}

fn parse_send_args(args: &[String]) -> Result<SendArgs, String> {
    let mut out = SendArgs {
        scope: String::new(),
        subject: String::new(),
        expires_raw: DEFAULT_EXPIRES.to_string(),
        urgent: false,
        from: String::new(),
        sender_kind: String::new(),
        from_session: None,
        json_out: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--scope" => out.scope = it.next().ok_or("--scope needs a value")?.clone(),
            "--subject" => out.subject = it.next().ok_or("--subject needs a value")?.clone(),
            "--expires" => out.expires_raw = it.next().ok_or("--expires needs a value")?.clone(),
            "--urgent" => out.urgent = true,
            "--from" => out.from = it.next().ok_or("--from needs a value")?.clone(),
            "--sender-kind" => {
                out.sender_kind = it.next().ok_or("--sender-kind needs a value")?.clone()
            }
            "--from-session" => {
                out.from_session = Some(it.next().ok_or("--from-session needs a value")?.clone())
            }
            "--json" | "-J" => out.json_out = true,
            other => return Err(format!("unknown announce send flag {other:?}")),
        }
    }
    if out.scope.is_empty() {
        return Err("--scope is required (all | leads | <team> | project:<p>)".to_string());
    }
    if out.from.is_empty() {
        return Err("--from is required".to_string());
    }
    if out.sender_kind != "operator" && out.sender_kind != "agent" {
        return Err("--sender-kind must be operator or agent".to_string());
    }
    Ok(out)
}

/// Usage line printed on arg or authority refusal.
fn send_usage() -> &'static str {
    "usage: fno-agents announce send --scope <all|leads|<team>|project:<p>> \
     [--subject S] [--expires 24h] [--urgent] --from <sender> \
     --sender-kind <operator|agent> [--from-session <id>] [--json|-J]  (body on stdin)"
}

fn team_row<'a>(rows: &'a [Value], sender: &str) -> Option<&'a Value> {
    rows.iter().filter(|row| !row_terminal(row)).find(|row| {
        row.get("role_level").map(|c| !c.is_null()).unwrap_or(false)
            && (row_str(row, "name") == Some(sender)
                || row_session_id(row)
                    .map(|sid| {
                        matches!(
                            crate::identity::session_handle_tier(sender, &sid),
                            Some(0 | 1)
                        )
                    })
                    .unwrap_or(false))
    })
}

#[derive(Debug)]
struct SendReceipt {
    id: String,
    scope: String,
    audience: usize,
    superseded: Vec<String>,
}

fn send_announcement(
    parsed: &SendArgs,
    body: &str,
    paths: &AnnouncePaths,
    projects: Option<&HashMap<String, String>>,
) -> Result<SendReceipt, (i32, String)> {
    let store = AnnounceStore::from_env().map_err(|e| (12, format!("announce send: {e}")))?;
    send_announcement_in(parsed, body, paths, projects, &store)
}

/// [`send_announcement`] with the row store injected, so a test can seat two
/// machines on one in-process primary.
fn send_announcement_in(
    parsed: &SendArgs,
    body: &str,
    paths: &AnnouncePaths,
    projects: Option<&HashMap<String, String>>,
    store: &AnnounceStore,
) -> Result<SendReceipt, (i32, String)> {
    let registry = crate::client_commands::load_registry_entries(&paths.registry)
        .map_err(|e| (12, format!("announce send: {e}")))?;
    let sender_row = if parsed.sender_kind == "operator" {
        None
    } else {
        team_row(&registry, &parsed.from)
    };
    if parsed.sender_kind != "operator" && sender_row.is_none() {
        return Err((
            2,
            format!(
                "announce: refused: sender {:?} holds no team; fleet announcements are operator- or teamed-lead-only",
                parsed.from
            ),
        ));
    }

    let mut audience = resolve_audience(&parsed.scope, &registry, projects);
    let from_session = parsed
        .from_session
        .clone()
        .or_else(|| sender_row.and_then(row_session_id));
    if let Some(session_id) = from_session.as_deref() {
        audience.retain(|key| {
            !matches!(
                crate::identity::session_handle_tier(key, session_id),
                Some(0 | 1)
            )
        });
    }

    // One scan serves both the rate limit (decision 8) and the supersede
    // list (decision 7): a newer announcement with the same subject+scope
    // replaces the older standing one.
    let now = chrono::Utc::now();
    let fleet_scope = format!("fleet:{}", parsed.scope);
    let mut recent = 0usize;
    let mut supersedes: Vec<String> = Vec::new();
    let bus_rows = read_announce_rows(&paths.bus_live, store)
        .map_err(|e| (12, format!("announce send: {e}")))?;
    for m in bus_rows.iter() {
        if row_str(&m, "kind") != Some(ANNOUNCE_KIND) {
            continue;
        }
        if row_str(&m, "from") == Some(parsed.from.as_str())
            && row_str(&m, "ts")
                .and_then(parse_iso)
                .is_some_and(|t| now.signed_duration_since(t).num_seconds() < 3600)
        {
            recent += 1;
        }
        if row_str(&m, "to") == Some(fleet_scope.as_str())
            && !expired(&m, now)
            && m.get("meta")
                .and_then(|meta| row_str(meta, "subject"))
                .unwrap_or("")
                == parsed.subject
        {
            if let Some(id) = row_str(&m, "id") {
                supersedes.push(id.to_string());
            }
        }
    }
    if recent >= HOURLY_LIMIT {
        return Err((
            2,
            format!(
                "announce: refused: rate limit is {HOURLY_LIMIT} announcements per rolling hour and {:?} already sent {recent}",
                parsed.from
            ),
        ));
    }

    let ttl = match parse_expires(&parsed.expires_raw) {
        Ok(t) if t <= MAX_EXPIRES => t,
        Ok(_) => return Err((2, "announce send: --expires beyond the 7d maximum".into())),
        Err(why) => return Err((2, format!("announce send: {why}"))),
    };
    let expires_at = (now + chrono::Duration::seconds(ttl.as_secs() as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    if audience.is_empty() {
        return Err((
            1,
            format!(
                "announce send: no live recipients in scope {:?}",
                parsed.scope
            ),
        ));
    }

    let id = new_msg_id();
    let word_count = body.split_whitespace().count() as i64;
    let mut obj = Map::new();
    obj.insert("v".into(), json!(ENVELOPE_VERSION));
    obj.insert("id".into(), json!(&id));
    obj.insert("ts".into(), json!(now_iso()));
    obj.insert("thread".into(), json!(&id));
    obj.insert("from".into(), json!(&parsed.from));
    obj.insert("to".into(), json!(format!("fleet:{}", parsed.scope)));
    obj.insert("kind".into(), json!(ANNOUNCE_KIND));
    if let Some(fs) = &from_session {
        obj.insert("from_session".into(), json!(fs));
    }
    obj.insert("to_kind".into(), json!("fleet"));
    obj.insert("word_count".into(), json!(word_count));
    obj.insert(
        "meta".into(),
        json!({
            "scope": parsed.scope,
            "audience": audience,
            "subject": parsed.subject,
            "expires_at": expires_at,
            "urgent": parsed.urgent,
            "supersedes": supersedes,
        }),
    );
    obj.insert("body".into(), json!(body));

    let row = Value::Object(obj);
    append_line(&paths.bus_live, &row, &paths.chats_dir)
        .map_err(|e| (1, format!("announce send: {e}")))?;
    // The primary copy is what carries the row to other machines; a failed
    // insert fails the send, because a machine-local announcement during an
    // incident is exactly the bug this closes. The local copy stays: the
    // sender's retry supersedes it by subject+scope.
    store.insert(&row).map_err(|e| {
        (
            1,
            format!(
                "announce send: the primary copy failed ({e}); the announcement is machine-local"
            ),
        )
    })?;
    Ok(SendReceipt {
        id,
        scope: parsed.scope.clone(),
        audience: audience.len(),
        superseded: supersedes,
    })
}

/// One urgent `all`-scope announcement from an in-crate operator door (the
/// test hold). Same line, rate limit and supersede as `announce send`.
pub(crate) fn announce_all(from: &str, subject: &str, body: &str) -> Result<String, String> {
    let paths = AnnouncePaths::from_env_opt().ok_or("no agents home")?;
    fence_test_bus(&paths.bus_live)?;
    let args = SendArgs {
        scope: "all".into(),
        subject: subject.into(),
        expires_raw: DEFAULT_EXPIRES.into(),
        urgent: true,
        from: from.into(),
        sender_kind: "operator".into(),
        from_session: None,
        json_out: false,
    };
    send_announcement(&args, body, &paths, None)
        .map(|receipt| receipt.id)
        .map_err(|(_, message)| message)
}

/// A test build must never mail the live fleet: the bus has to sit under the
/// temp dir, which every pinned agents home in a test resolves to. An
/// unpinned test that reaches this door hears the fence instead of writing
/// the operator's `~/.fno/bus`. Production (the daemon, a lead session) is
/// never a test build, so the fence is dead weight there.
fn fence_test_bus(bus_live: &Path) -> Result<(), String> {
    if !cfg!(test) {
        return Ok(());
    }
    let temp = std::env::temp_dir();
    if bus_live.ancestors().any(|a| a == temp) {
        return Ok(());
    }
    Err(format!(
        "announce fenced under test: {} is not under the temp dir; pin FNO_AGENTS_HOME to a temp home in the test",
        bus_live.display()
    ))
}

pub(crate) fn run_announce_send(args: &[String], paths: &AnnouncePaths) -> i32 {
    let parsed = match parse_send_args(args) {
        Ok(p) => p,
        Err(why) => {
            eprintln!("announce send: {why}\n{}", send_usage());
            return 2;
        }
    };
    let mut body = String::new();
    if std::io::stdin().read_to_string(&mut body).is_err() {
        eprintln!("announce send: could not read the body from stdin");
        return 2;
    }
    let body = body.trim().to_string();
    if body.is_empty() {
        eprintln!("announce send: empty body");
        return 2;
    }
    let projects =
        crate::org_board::scope::project_map(&std::env::current_dir().unwrap_or_default()).ok();
    match send_announcement(&parsed, &body, paths, projects.as_ref()) {
        Ok(receipt) if parsed.json_out => {
            println!(
                "{}",
                json!({"id": receipt.id, "scope": receipt.scope, "audience": receipt.audience, "superseded": receipt.superseded})
            );
            0
        }
        Ok(receipt) => {
            println!(
                "announce {} scope={} audience={}",
                receipt.id, receipt.scope, receipt.audience
            );
            0
        }
        Err((code, message)) => {
            eprintln!("{message}");
            code
        }
    }
}

pub(crate) fn new_msg_id() -> String {
    // 'fmail-XXXXXXXXXXXX' (12 hex), matching bus/log.py::new_msg_id.
    // Pre-widening 'msg-XXXXXX' ids stay legal and resolve through the
    // migrated chats rows.
    let mut buf = [0u8; 6];
    if getrandom::fill(&mut buf).is_err() {
        // Fallback entropy: pid + clock. A collision costs one duplicate id.
        let seed = (std::process::id() as u64) << 32
            | std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0);
        buf.copy_from_slice(&seed.to_le_bytes()[..6]);
    }
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("fmail-{hex}")
}

fn expired(m: &Value, now: chrono::DateTime<chrono::Utc>) -> bool {
    // Missing or unparseable expires_at is EXPIRED: stale news reads worse
    // than missed news (decision 7).
    match m
        .get("meta")
        .and_then(|meta| meta.get("expires_at"))
        .and_then(Value::as_str)
        .and_then(parse_iso)
    {
        Some(exp) => exp <= now,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Cursor: per-session seen-id set (decision 4)
// ---------------------------------------------------------------------------

fn cursor_path(state_root: &Path, dir: &str, session_id: &str) -> PathBuf {
    let safe = format!("{}.json", session_id.replace('/', "_"));
    crate::state_layout::place_in(state_root, dir, &safe)
}

fn load_cursor(state_root: &Path, dir: &str, session_id: &str) -> HashSet<String> {
    let text = match std::fs::read_to_string(cursor_path(state_root, dir, session_id)) {
        Ok(t) => t,
        Err(_) => return HashSet::new(),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => v
            .get("seen")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => HashSet::new(),
    }
}

fn save_cursor(state_root: &Path, dir: &str, session_id: &str, seen: &HashSet<String>) {
    let path = cursor_path(state_root, dir, session_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut ids: Vec<String> = seen.iter().cloned().collect();
    ids.sort();
    let payload = json!({"seen": ids}).to_string();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, payload).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// How long a cursor writer waits for another writer on the same file before
/// proceeding unlocked (the bound merge_hold's PlanLock uses).
const CURSOR_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Serialized read-modify-write of one cursor file. The flock on
/// `<cursor>.lock` spans load through save: without it two overlapping
/// reads of one session (async PostToolUse runs collide) both load the same
/// set and one save drops the other's insert, so a deduped rule, door span
/// or announcement fires again later. The closure mutates the loaded set
/// and returns true when anything changed; true saves. A wait past the
/// timeout proceeds unlocked: the file stays consistent and the worst case
/// is the duplicate reminder this lock exists to prevent, never a hung hook.
pub(crate) fn update_cursor(
    state_root: &Path,
    dir: &str,
    session_id: &str,
    f: impl FnOnce(&mut HashSet<String>) -> bool,
) {
    let path = cursor_path(state_root, dir, session_id);
    let _lock = cursor_lock(&path);
    let mut seen = load_cursor(state_root, dir, session_id);
    if f(&mut seen) {
        save_cursor(state_root, dir, session_id, &seen);
    }
}

/// The held flock lives as long as the returned file; `None` proceeds
/// unlocked (open failure, lock error, or the timeout above).
fn cursor_lock(path: &Path) -> Option<std::fs::File> {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    if let Some(parent) = lock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .ok()?;
    let deadline = std::time::Instant::now() + CURSOR_LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// `announce read`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Boundary {
    Start,
    Prompt,
    Loop,
    Compact,
}

impl Boundary {
    fn parse(s: &str) -> Option<Boundary> {
        match s {
            "start" => Some(Boundary::Start),
            "prompt" => Some(Boundary::Prompt),
            "loop" => Some(Boundary::Loop),
            "compact" => Some(Boundary::Compact),
            _ => None,
        }
    }
}

fn standing_announcements(
    rows: &[Value],
    now: chrono::DateTime<chrono::Utc>,
) -> (Vec<&Value>, HashSet<String>) {
    let mut superseded: HashSet<String> = HashSet::new();
    for m in rows {
        if row_str(m, "kind") != Some(ANNOUNCE_KIND) {
            continue;
        }
        if let Some(list) = m
            .get("meta")
            .and_then(|meta| meta.get("supersedes"))
            .and_then(Value::as_array)
        {
            for id in list {
                if let Some(s) = id.as_str() {
                    superseded.insert(s.to_string());
                }
            }
        }
    }
    let standing: Vec<&Value> = rows
        .iter()
        .filter(|m| row_str(m, "kind") == Some(ANNOUNCE_KIND))
        .filter(|m| !expired(m, now))
        .filter(|m| row_str(m, "id").is_none_or(|id| !superseded.contains(id)))
        .collect();
    (standing, superseded)
}

/// One announcement block: the delivered-mail header line, then the body.
/// The sender reads through the system-sender table (`fleet-incident` ->
/// `fno/fleet-incident`). User law d-b720d23c: a bodied announcement's
/// header carries no subject; a header-only one keeps the row's subject
/// when it is header-safe (no backtick or separator could forge a header
/// field), else the body summary.
fn render_block(m: &Value) -> String {
    let id = row_str(m, "id").unwrap_or("");
    let from = row_str(m, "from").unwrap_or("unknown");
    let meta = m.get("meta").cloned().unwrap_or(Value::Null);
    let subject = meta.get("subject").and_then(Value::as_str).unwrap_or("");
    let body = row_str(m, "body").unwrap_or("");
    let sender = crate::system_sender::canonical(from);
    let safe = !subject.is_empty() && !subject.contains('`') && !subject.contains(" · ");
    let third = crate::mail_header::header_third_field(safe.then_some(subject), body);
    let header = crate::mail_header::render_header(
        crate::mail_header::HeaderForm::Mention,
        sender,
        id,
        third.as_deref(),
    );
    format!("{header}\n{body}")
}

fn render_compact_line(m: &Value) -> String {
    let from = row_str(m, "from").unwrap_or("unknown");
    let meta = m.get("meta").cloned().unwrap_or(Value::Null);
    let subject = meta.get("subject").and_then(Value::as_str).unwrap_or("");
    let expires = meta.get("expires_at").and_then(Value::as_str).unwrap_or("");
    format!("announce (still standing): \"{subject}\" from {from}, expires {expires}")
}

/// Core reader, shared by the CLI command and `nudge.rs` (the loop boundary reads
/// natively). `Ok(None)` = nothing to inject; the CLI prints that as silence.
pub(crate) fn read_render(
    paths: &AnnouncePaths,
    session_id: &str,
    boundary: Boundary,
) -> Result<Option<String>, String> {
    let store = AnnounceStore::from_env()?;
    read_render_in(paths, session_id, boundary, &store)
}

/// [`read_render`] with the row store injected, so a test can seat two
/// machines on one in-process primary.
fn read_render_in(
    paths: &AnnouncePaths,
    session_id: &str,
    boundary: Boundary,
    store: &AnnounceStore,
) -> Result<Option<String>, String> {
    let rows = read_announce_rows(&paths.bus_live, store)?;
    let now = chrono::Utc::now();
    let (standing, _superseded) = standing_announcements(&rows, now);
    if standing.is_empty() {
        return Ok(None);
    }
    let registry = crate::client_commands::load_registry_entries(&paths.registry)?;
    let projects =
        crate::org_board::scope::project_map(&std::env::current_dir().unwrap_or_default()).ok();
    let mut result: Option<String> = None;
    update_cursor(&paths.state_root, "announce-cursors", session_id, |seen| {
        let mut fresh: Vec<&Value> = Vec::new();
        let mut standing_seen: Vec<&Value> = Vec::new();
        for m in &standing {
            let Some(id) = row_str(m, "id") else { continue };
            // The sender never reads its own announcement back.
            if m.get("from_session")
                .and_then(Value::as_str)
                .is_some_and(|fs| {
                    crate::spawn_context::session_identity_key(fs)
                        == crate::spawn_context::session_identity_key(session_id)
                })
            {
                continue;
            }
            let audience: Vec<Value> = m
                .get("meta")
                .and_then(|meta| meta.get("audience"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let scope = m
                .get("meta")
                .and_then(|meta| row_str(meta, "scope"))
                .unwrap_or("all");
            let mine = in_audience(&audience, session_id)
                || matches_scope_now(scope, session_id, &registry, projects.as_ref());
            if !mine {
                continue;
            }
            if seen.contains(id) {
                standing_seen.push(*m);
            } else {
                fresh.push(*m);
            }
        }

        if boundary == Boundary::Compact {
            let mut out: Vec<String> = fresh.iter().map(|m| render_block(m)).collect();
            out.extend(standing_seen.iter().map(|m| render_compact_line(m)));
            for m in fresh.iter().chain(standing_seen.iter()) {
                if let Some(id) = row_str(m, "id") {
                    seen.insert(id.to_string());
                }
            }
            prune_seen(seen, &rows);
            result = (!out.is_empty()).then(|| out.join("\n"));
            // The compact read always saves: the prune keeps the cursor
            // bounded even when nothing new fired.
            true
        } else if fresh.is_empty() {
            false
        } else {
            let out = fresh
                .iter()
                .map(|m| render_block(m))
                .collect::<Vec<_>>()
                .join("\n");
            for m in &fresh {
                if let Some(id) = row_str(m, "id") {
                    seen.insert(id.to_string());
                }
            }
            prune_seen(seen, &rows);
            result = Some(out);
            true
        }
    });
    Ok(result)
}

/// Keep the cursor bounded: ids no longer on any retained segment leave the
/// set (nudge.py's `nudged &= unread_ids` pruning). The save belongs to the
/// `update_cursor` caller.
fn prune_seen(seen: &mut HashSet<String>, rows: &[Value]) {
    let live_ids: HashSet<String> = rows
        .iter()
        .filter_map(|m| row_str(m, "id"))
        .map(str::to_string)
        .collect();
    seen.retain(|id| live_ids.contains(id));
}

pub(crate) fn run_announce_read(args: &[String], paths: &AnnouncePaths) -> i32 {
    let mut session_id = String::new();
    let mut boundary = Boundary::Prompt;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--session-id" => session_id = it.next().map(String::clone).unwrap_or_default(),
            "--boundary" => {
                boundary = it
                    .next()
                    .and_then(|b| Boundary::parse(b))
                    .unwrap_or(Boundary::Prompt)
            }
            "--harness" => {
                it.next(); // accepted for the documented surface; the reader is session-keyed
            }
            other => {
                eprintln!("announce read: unknown flag {other:?}");
                return 2;
            }
        }
    }
    if session_id.is_empty() {
        eprintln!("announce read: --session-id is required");
        return 2;
    }
    match read_render(paths, &session_id, boundary) {
        Ok(Some(out)) => println!("{out}"),
        Ok(None) => {}
        Err(e) => {
            // A hook-boundary reader fails open: never block a session on
            // announce state (AC3-ERR).
            eprintln!("announce read: {e}");
        }
    }
    0
}

// ---------------------------------------------------------------------------
// `announce status`: the sender view (decision 6)
// ---------------------------------------------------------------------------

/// reply_resolve.py::_transcript_path, mirrored: claude
/// `<projects>/*/<id>.jsonl`, codex a rollout embedding the id. A key neither
/// store resolves is `unverified`, never `pending`.
pub(crate) fn transcript_path(
    harness_home: &Path,
    session_key: &str,
    codex: bool,
) -> Option<PathBuf> {
    if codex {
        let root = std::env::var_os("FNO_CODEX_SESSIONS_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("CODEX_HOME").map(|h| PathBuf::from(h).join("sessions")))
            .unwrap_or_else(|| harness_home.join(".codex").join("sessions"));
        return walk_session_file(&root, session_key, 6);
    }
    let root = std::env::var_os("FNO_CLAUDE_PROJECTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::claude_ask::ClaudeHome::at(harness_home).projects_dir());
    // One level: projects keys a transcript dir by the session cwd slug.
    let dir = std::fs::read_dir(&root).ok()?;
    for entry in dir.flatten() {
        let candidate = entry.path().join(format!("{session_key}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Depth-bounded recursive walk for a file whose name embeds the session key.
fn walk_session_file(root: &Path, key: &str, depth: u8) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let rd = std::fs::read_dir(root).ok()?;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(hit) = walk_session_file(&path, key, depth - 1) {
                return Some(hit);
            }
        } else if path
            .file_name()
            .map(|n| n.to_string_lossy().contains(key))
            .unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

/// Does the transcript carry the id? JSONL quote-escapes are normalized
/// first, then the id must sit where a delivery puts it: a header line's
/// middle token, or inside an `<fno_mail` open tag on an old transcript
/// (reply_resolve.py::_ID_RE), so an unrelated `id="..."` attribute cannot
/// false-positive.
fn transcript_has_id(path: &Path, id: &str) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let normalized = text.replace("\\\"", "\"");
    if normalized
        .lines()
        .any(|line| crate::mail_header::delivered_msg_id(line).as_deref() == Some(id))
    {
        return Ok(true);
    }
    let needle = regex::Regex::new(&format!(r#"<fno_mail\b[^>]*\bid="{id}""#))
        .map_err(|e| format!("id regex: {e}"))?;
    Ok(needle.is_match(&normalized))
}

fn record_landed_row(live: &Path, chats_dir: &Path, from: &str, session_key: &str, id: &str) {
    let mut obj = Map::new();
    obj.insert("v".into(), json!(ENVELOPE_VERSION));
    obj.insert("id".into(), json!(new_msg_id()));
    obj.insert("ts".into(), json!(now_iso()));
    obj.insert("thread".into(), json!(id));
    obj.insert("from".into(), json!(from));
    obj.insert("to".into(), json!(session_key));
    obj.insert("kind".into(), json!(LANDED_KIND));
    obj.insert("word_count".into(), json!(0));
    obj.insert("meta".into(), json!({"landed": id, "session": session_key}));
    obj.insert("body".into(), json!(""));
    if let Err(e) = append_line(live, &Value::Object(obj), chats_dir) {
        eprintln!("announce status: landed row write failed: {e}");
    }
}

fn announce_receipt(
    rows: &[Value],
    announcement: &Value,
    id: &str,
    paths: &AnnouncePaths,
    home: &Path,
) -> Value {
    let from = row_str(announcement, "from").unwrap_or("unknown");
    let meta = announcement.get("meta").cloned().unwrap_or(Value::Null);
    let audience: Vec<String> = meta
        .get("audience")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut landed_keys: HashSet<String> = HashSet::new();
    for m in rows {
        if row_str(m, "kind") != Some(LANDED_KIND) {
            continue;
        }
        let landed = m.get("meta").and_then(|meta| row_str(meta, "landed"));
        if landed != Some(id) {
            continue;
        }
        if let Some(session) = m.get("meta").and_then(|meta| row_str(meta, "session")) {
            landed_keys.insert(session.to_string());
        } else if let Some(to) = row_str(m, "to") {
            landed_keys.insert(to.to_string());
        }
    }
    let woken = rows
        .iter()
        .filter(|m| row_str(m, "kind") == Some(WAKE_KIND))
        .filter(|m| {
            m.get("meta").and_then(|meta| row_str(meta, "announce")) == Some(id)
                && m.get("meta").and_then(|meta| row_str(meta, "result")) == Some("woken")
        })
        .count();

    let audience_set: HashSet<&str> = audience.iter().map(String::as_str).collect();
    let mut per_session: Vec<Value> = Vec::new();
    let mut landed = 0usize;
    let mut pending = 0usize;
    let mut unverified = 0usize;
    for key in &audience {
        if landed_keys.contains(key) {
            landed += 1;
            per_session.push(json!({"session": key, "state": "landed"}));
            continue;
        }
        let claude_store = transcript_path(home, key, false);
        let codex_store = if claude_store.is_none() {
            transcript_path(home, key, true)
        } else {
            None
        };
        match claude_store.or(codex_store) {
            None => {
                unverified += 1;
                per_session.push(json!({"session": key, "state": "unverified"}));
            }
            Some(path) => match transcript_has_id(&path, id) {
                Ok(true) => {
                    landed += 1;
                    record_landed_row(&paths.bus_live, &paths.chats_dir, from, key, id);
                    per_session.push(json!({"session": key, "state": "landed"}));
                }
                Ok(false) => {
                    pending += 1;
                    per_session.push(json!({"session": key, "state": "pending"}));
                }
                Err(_) => {
                    unverified += 1;
                    per_session.push(json!({"session": key, "state": "unverified"}));
                }
            },
        }
    }
    let late = landed_keys
        .iter()
        .filter(|key| !audience_set.contains(key.as_str()))
        .count();
    json!({
        "id": id,
        "audience": audience.len(),
        "landed": landed,
        "pending": pending,
        "woken": woken,
        "unverified": unverified,
        "late": late,
        "sessions": per_session,
    })
}

pub(crate) fn run_announce_status(args: &[String], paths: &AnnouncePaths) -> i32 {
    let mut id = String::new();
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" | "-J" => json_out = true,
            other if id.is_empty() && !other.starts_with('-') => id = other.to_string(),
            other => {
                eprintln!("announce status: unknown flag {other:?}");
                return 2;
            }
        }
    }
    if id.is_empty() {
        eprintln!("usage: fno-agents announce status <id> [--json|-J]");
        return 2;
    }

    let rows = read_bus_segments(&paths.bus_live);
    let announcement = rows.iter().find(|m| {
        row_str(m, "kind") == Some(ANNOUNCE_KIND) && row_str(m, "id") == Some(id.as_str())
    });
    let Some(announcement) = announcement else {
        eprintln!("announce status: no announcement {id:?} on the retained bus");
        return 1;
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let receipt = announce_receipt(&rows, announcement, &id, paths, &home);

    if json_out {
        println!("{receipt}");
    } else {
        println!(
            "audience {}, landed {}, pending {}, woken {}, unverified {}, late {}",
            receipt["audience"],
            receipt["landed"],
            receipt["pending"],
            receipt["woken"],
            receipt["unverified"],
            receipt["late"]
        );
    }
    0
}

// ---------------------------------------------------------------------------
// Command entry
// ---------------------------------------------------------------------------

/// Command entrypoint reached from `bin/client.rs`'s direct dispatch. `pub`, not
/// `pub(crate)`: a package's bin target sees its own lib as an external crate,
/// so `pub(crate)` here fails the publish tarball build.
pub fn run_announce(args: &[String]) -> i32 {
    let Some(sub) = args.first() else {
        eprintln!(
            "usage: fno-agents announce <send|read|status> ...  (one announcement, one bus line)"
        );
        return 2;
    };
    let paths = AnnouncePaths::from_env();
    match sub.as_str() {
        "send" => run_announce_send(&args[1..], &paths),
        "read" => run_announce_read(&args[1..], &paths),
        "status" => run_announce_status(&args[1..], &paths),
        other => {
            eprintln!("announce: unknown subcommand {other:?} (send | read | status)");
            2
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier, Mutex};

    /// One temp root per test; env-mutating tests share the process, so the
    /// mutex keeps FNO_* pins from racing.
    static ENV_LOCK: std::sync::LazyLock<&'static Mutex<()>> =
        std::sync::LazyLock::new(crate::claims::test_env_lock);

    struct Fixture {
        root: PathBuf,
        paths: AnnouncePaths,
    }

    fn fixture(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "fno-announce-{tag}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(root.join("bus")).unwrap();
        std::fs::create_dir_all(root.join("agents")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        Fixture {
            paths: AnnouncePaths {
                bus_live: root.join("bus").join("messages.jsonl"),
                registry: root.join("agents").join("registry.json"),
                chats_dir: root.join("chats"),
                state_root: root.clone(),
            },
            root,
        }
    }

    fn write_registry(paths: &AnnouncePaths, rows: &[Value]) {
        crate::registry_store::seed_raw(
            &paths.registry,
            serde_json::to_string(&json!({ "schema_version": 1, "agents": rows })).unwrap(),
        );
    }

    #[test]
    fn a_test_build_is_fenced_from_the_live_mail_store() {
        // The live-store fence: a test whose paths resolve outside the temp dir
        // refuses loudly instead of leaking fixture rows into the
        // operator's Messages tab. A repo-relative path is never under
        // $TMPDIR, so it stands in for the real ~/.fno store without
        // reading the operator's home.
        let outside = std::env::current_dir()
            .expect("cargo test runs with a cwd")
            .join(".fno")
            .join("bus")
            .join("messages.jsonl");
        let err =
            fence_test_store(&outside).expect_err("the live store must refuse under a test build");
        assert!(err.contains("fenced under test"), "{err}");
        let fixture = fixture("fence-ok");
        fence_test_store(&fixture.paths.chats_dir).expect("a temp fixture passes the fence");
        std::fs::remove_dir_all(&fixture.root).ok();
    }

    fn agent_row(name: &str, session: &str, extra: Value) -> Value {
        let mut row = json!({
            "name": name, "harness": "claude", "status": "live",
            "harness_session_id": session,
            "short_id": session.chars().take(8).collect::<String>(),
            "cwd": format!("/tmp/{name}"),
            "log_path": format!("/tmp/{name}.log"),
        });
        let obj = row.as_object_mut().unwrap();
        for (k, v) in extra.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        row
    }

    fn send_for_test(
        paths: &AnnouncePaths,
        args: &[String],
        body: &str,
        registry: &[Value],
    ) -> (i32, String) {
        write_registry(paths, registry);
        let parsed = match parse_send_args(args) {
            Ok(p) => p,
            Err(why) => return (2, why),
        };
        match send_announcement(&parsed, body.trim(), paths, Some(&HashMap::new())) {
            Ok(receipt) => (0, receipt.id),
            Err((code, message)) => (code, message),
        }
    }

    fn send_via(
        paths: &AnnouncePaths,
        flags: &[&str],
        body: &str,
        registry: &[Value],
    ) -> (i32, String) {
        let args: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
        send_for_test(paths, &args, body, registry)
    }

    fn send_flags(scope: &'static str) -> Vec<&'static str> {
        vec![
            "--scope",
            scope,
            "--from",
            "op",
            "--sender-kind",
            "operator",
        ]
    }

    #[test]
    fn unpromoted_agent_sender_is_refused_and_the_bus_is_unchanged() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture("authority");
        let rows = vec![agent_row(
            "worker",
            "dddd4444-4444-4444-4444-444444444444",
            json!({}),
        )];
        let mut flags = send_flags("all");
        flags[3] = "worker";
        flags[5] = "agent";
        let (code, why) = send_via(&f.paths, &flags, "mutiny", &rows);
        assert_eq!(code, 2, "{why}");
        assert!(why.contains("worker"));
        assert!(read_bus_segments(&f.paths.bus_live).is_empty());
        std::fs::remove_dir_all(&f.root).ok();
    }

    #[test]
    fn rate_limit_refuses_the_seventh_announcement_in_the_rolling_hour() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let f = fixture("rate");
        let rows = vec![agent_row(
            "red",
            "ffff6666-6666-6666-6666-666666666666",
            json!({}),
        )];
        for i in 0..HOURLY_LIMIT {
            let (code, _) = send_via(&f.paths, &send_flags("all"), &format!("notice {i}"), &rows);
            assert_eq!(code, 0, "send {i} refused early");
        }
        let (code, why) = send_via(&f.paths, &send_flags("all"), "one too many", &rows);
        assert_eq!(code, 2, "{why}");
        assert!(why.contains("rate limit"));
        std::fs::remove_dir_all(&f.root).ok();
    }

    #[test]
    fn python_parses_the_rust_line_key_for_key() {
        // The fixture (crates/fno-agents/tests/fixtures/announce_line.jsonl)
        // is generated by hand in the same key order the writer emits; this
        // test regenerates the shape and pins the order the Python
        // from_json_line round-trip depends on.
        let f = fixture("keyorder");
        let rows = vec![agent_row(
            "red",
            "bbbb8888-8888-8888-8888-888888888888",
            json!({}),
        )];
        send_via(&f.paths, &send_flags("all"), "order check", &rows);
        let line = std::fs::read_to_string(&f.paths.bus_live).unwrap();
        let obj: Value = serde_json::from_str(line.trim()).unwrap();
        let keys: Vec<&str> = obj
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: &[&str] = &[
            "v",
            "id",
            "ts",
            "thread",
            "from",
            "to",
            "kind",
            "to_kind",
            "word_count",
            "meta",
            "body",
        ];
        assert_eq!(keys, expected, "key order must match to_json_line");
        std::fs::remove_dir_all(&f.root).ok();
    }

    /// The lock spans load through save, so threads that race on one cursor
    /// keep every insert. Without the lock, the barrier plus the in-closure
    /// sleep put every thread's load before any thread's save and the last
    /// save wins, which is the lost update this exists to prevent.
    #[test]
    fn concurrent_cursor_writers_keep_every_insert() {
        let root = std::env::temp_dir().join(format!(
            "fno-announce-cursor-race-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ids: Vec<String> = (0..4)
            .flat_map(|w| (0..3).map(move |i| format!("w{w}-id{i}")))
            .collect();
        let barrier = Arc::new(Barrier::new(ids.len()));
        let handles: Vec<_> = ids
            .iter()
            .cloned()
            .map(|id| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    update_cursor(&root, "rules-seen", "sess", |seen| {
                        std::thread::sleep(Duration::from_millis(2));
                        seen.insert(id);
                        true
                    });
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let seen = load_cursor(&root, "rules-seen", "sess");
        assert_eq!(
            seen.len(),
            ids.len(),
            "a concurrent save dropped an id: {seen:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

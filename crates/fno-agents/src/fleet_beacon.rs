//! The fleet map: every machine and its sessions, read from any machine.
//!
//! Each daemon publishes one `fleet_machine` row and its own `fleet_session`
//! rows to the shared primary (`store.remote_url`) on its tick. Each row has
//! one writer, the machine it names, so two machines never race on a fact.
//! The reader is `fno agents fleet top --machines`; the mux Fleet tab reads
//! its JSON off the UI loop.
//!
//! The numbers come from what the daemon already measures: the newest
//! `machine_sample` journal row (CPU, verdict, memory pressure) and the
//! capacity state file (memory used, effective cap). This module samples
//! nothing itself. Watts and GPU stay null until a sampler that reports
//! them runs on the daemon side: the mux meter's `macmon` child is the only
//! reader today, and it is not installed on every machine.
//!
//! With `store.remote_url` unset nothing publishes, and the reader shows
//! this machine alone with a notice naming the key.
//!
//! The seat facts (model, start, directory, project, role, context) ride a
//! side table, `fleet_seat`, so an old daemon's nine-column session insert
//! keeps working and an old reader never selects them. Its seats read null.
//! A seat row carries the beat that wrote it, and a read keeps it only when
//! that beat is still the machine's newest, so a seat left behind by a newer
//! daemon never attaches to an old daemon's session of the same name.
//! The text of a session's last turn stays on its own machine: the seat row
//! carries only the stamp of that turn.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::paths::AgentsHome;
use crate::store_remote::{Remote, SqlValue};

/// The publish cadence. The reader's default freshness window spans several
/// beats, so one missed beat never reads as offline.
pub const INTERVAL: Duration = Duration::from_secs(60);

/// A machine whose newest beat is older than this reads `fresh: false`.
pub const DEFAULT_FRESH_S: i64 = 300;

pub const NO_REMOTE_NOTICE: &str = "fleet map needs store.remote_url: this machine shows alone";

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS fleet_machine (
    machine_id TEXT PRIMARY KEY,
    host TEXT NOT NULL,
    owner TEXT NOT NULL,
    role TEXT NOT NULL,
    beat_at INTEGER NOT NULL,
    cpu REAL,
    mem_used REAL,
    mem_pressure INTEGER,
    watts REAL,
    gpu REAL,
    verdict TEXT,
    cap INTEGER,
    ceiling INTEGER,
    takes_work INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS fleet_session (
    machine_id TEXT NOT NULL,
    name TEXT NOT NULL,
    session TEXT,
    harness TEXT,
    node TEXT,
    status TEXT,
    age_s INTEGER,
    rss_mb REAL,
    parent TEXT,
    PRIMARY KEY (machine_id, name)
);
CREATE TABLE IF NOT EXISTS fleet_seat (
    machine_id TEXT NOT NULL,
    name TEXT NOT NULL,
    session_id TEXT,
    model TEXT,
    model_basis TEXT,
    started_at TEXT,
    last_message_at TEXT,
    cwd TEXT,
    project TEXT,
    role TEXT,
    context_pct INTEGER,
    beat_at INTEGER NOT NULL,
    PRIMARY KEY (machine_id, name)
);";

const MACHINE_COLUMNS: &str = "machine_id, host, owner, role, beat_at, cpu, mem_used, \
     mem_pressure, watts, gpu, verdict, cap, ceiling, takes_work";
const SESSION_COLUMNS: &str =
    "machine_id, name, session, harness, node, status, age_s, rss_mb, parent";
const SEAT_COLUMNS: &str = "machine_id, name, session_id, model, model_basis, started_at, \
     last_message_at, cwd, project, role, context_pct, beat_at";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub machine_id: String,
    pub host: String,
    pub owner: String,
    pub role: String,
    pub beat_at: i64,
    pub cpu: Option<f64>,
    pub mem_used: Option<f64>,
    pub mem_pressure: Option<i64>,
    pub watts: Option<f64>,
    pub gpu: Option<f64>,
    pub verdict: Option<String>,
    pub cap: Option<i64>,
    pub ceiling: Option<i64>,
    pub takes_work: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(skip)]
    pub machine_id: String,
    pub name: String,
    pub session: Option<String>,
    pub harness: Option<String>,
    pub node: Option<String>,
    pub status: Option<String>,
    pub age_s: Option<i64>,
    pub rss_mb: Option<f64>,
    /// The spawner's session id prefix, when the row recorded one.
    pub parent: Option<String>,
    /// The seat facts, from `fleet_seat`. Null for a machine whose daemon
    /// predates the table.
    pub session_id: Option<String>,
    pub model: Option<String>,
    /// `registry` or `observed`: where the model was read.
    pub model_basis: Option<String>,
    pub started_at: Option<String>,
    pub last_message_at: Option<String>,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub role: Option<String>,
    pub context_pct: Option<i64>,
}

impl Session {
    fn has_seat(&self) -> bool {
        self.session_id.is_some()
            || self.model.is_some()
            || self.started_at.is_some()
            || self.last_message_at.is_some()
            || self.cwd.is_some()
            || self.project.is_some()
            || self.role.is_some()
            || self.context_pct.is_some()
    }
}

/// The local flag the spawn gate reads: present means this machine takes no
/// new agent work. One file, so a toggle needs no daemon round trip.
pub fn takes_work_path(home: &AgentsHome) -> PathBuf {
    home.root().join("fleet").join("takes-work-off")
}

pub fn takes_work(home: &AgentsHome) -> bool {
    !takes_work_path(home).exists()
}

pub fn set_takes_work(home: &AgentsHome, on: bool) -> std::io::Result<()> {
    let path = takes_work_path(home);
    if on {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, b"")
    }
}

fn config_text(key: &str) -> Option<String> {
    crate::agents_config::config_lookup_global(&["fleet", key])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|v| !v.trim().is_empty())
}

/// The person who owns this machine: `fleet.owner`, else the login name.
fn owner() -> String {
    config_text("owner")
        .or_else(|| {
            // A launchd-started daemon can carry LOGNAME without USER.
            ["USER", "LOGNAME"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|u| !u.is_empty()))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// The short host name: the part before the first dot.
fn short_host() -> String {
    let host = crate::claims::hostname();
    host.split('.').next().unwrap_or_default().to_string()
}

/// This machine's row, folded from the newest sample and the capacity state.
pub fn local_machine(home: &AgentsHome, now: i64) -> Machine {
    let sample = crate::machine_sample::newest(&home.events_jsonl()).map(|(_, data)| data);
    let state = crate::capacity::read_state(home);
    let num = |key: &str| {
        sample
            .as_ref()
            .and_then(|s| s.get(key))
            .and_then(Value::as_f64)
    };
    let machine_id = crate::claims::machine_id();
    let host = short_host();
    Machine {
        machine_id: if machine_id.is_empty() {
            host.clone()
        } else {
            machine_id
        },
        host,
        owner: owner(),
        role: config_text("role").unwrap_or_else(|| "worker".into()),
        beat_at: now,
        cpu: num("busy_fraction"),
        mem_used: state
            .as_ref()
            .and_then(|s| s.memory.as_ref())
            .map(|m| m.used_fraction),
        mem_pressure: sample
            .as_ref()
            .and_then(|s| s.get("memory_pressure_level"))
            .and_then(Value::as_i64),
        watts: None,
        gpu: None,
        verdict: sample
            .as_ref()
            .and_then(|s| s.get("verdict"))
            .and_then(Value::as_str)
            .map(str::to_string),
        cap: state.as_ref().map(|s| i64::from(s.effective)),
        ceiling: state.as_ref().map(|s| i64::from(s.ceiling)),
        takes_work: takes_work(home),
    }
}

/// Session rows from the `top` payload's worker rows.
pub fn sessions_from_top(machine_id: &str, top: &Value) -> Vec<Session> {
    let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(str::to_string);
    top.get("workers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some(Session {
                machine_id: machine_id.to_string(),
                name: text(row, "name")?,
                session: text(row, "handle"),
                harness: text(row, "harness"),
                node: text(row, "node"),
                status: text(row, "status"),
                // The top row carries the age as a float (53.0).
                age_s: row
                    .get("status_age_s")
                    .and_then(Value::as_f64)
                    .map(|a| a.round() as i64),
                rss_mb: row.get("rss_mb").and_then(Value::as_f64),
                parent: text(row, "lead"),
                session_id: text(row, "session_id"),
                model: text(row, "model"),
                model_basis: text(row, "model_basis"),
                started_at: text(row, "started_at"),
                last_message_at: text(row, "last_message_at"),
                cwd: text(row, "cwd"),
                project: text(row, "project"),
                role: text(row, "role"),
                context_pct: row.get("context_pct").and_then(Value::as_i64),
            })
        })
        .collect()
}

fn opt_real(v: Option<f64>) -> SqlValue {
    v.map_or(SqlValue::Null, SqlValue::Real)
}

fn opt_int(v: Option<i64>) -> SqlValue {
    v.map_or(SqlValue::Null, SqlValue::Integer)
}

fn opt_text(v: &Option<String>) -> SqlValue {
    v.clone().map_or(SqlValue::Null, SqlValue::Text)
}

/// Write this machine's row and replace its session rows, all or nothing.
/// Only rows keyed on `machine.machine_id` are touched.
pub fn publish(remote: &Remote, machine: &Machine, sessions: &[Session]) -> Result<(), String> {
    remote.script(SCHEMA)?;
    let id = SqlValue::Text(machine.machine_id.clone());
    let mut steps = vec![
        (
            format!(
                "INSERT OR REPLACE INTO fleet_machine ({MACHINE_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
            ),
            vec![
                id.clone(),
                SqlValue::Text(machine.host.clone()),
                SqlValue::Text(machine.owner.clone()),
                SqlValue::Text(machine.role.clone()),
                SqlValue::Integer(machine.beat_at),
                opt_real(machine.cpu),
                opt_real(machine.mem_used),
                opt_int(machine.mem_pressure),
                opt_real(machine.watts),
                opt_real(machine.gpu),
                opt_text(&machine.verdict),
                opt_int(machine.cap),
                opt_int(machine.ceiling),
                SqlValue::Integer(i64::from(machine.takes_work)),
            ],
        ),
        (
            "DELETE FROM fleet_session WHERE machine_id = ?1".to_string(),
            vec![id.clone()],
        ),
        (
            "DELETE FROM fleet_seat WHERE machine_id = ?1".to_string(),
            vec![id.clone()],
        ),
    ];
    for s in sessions {
        steps.push((
            format!(
                "INSERT OR REPLACE INTO fleet_session ({SESSION_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
            ),
            vec![
                id.clone(),
                SqlValue::Text(s.name.clone()),
                opt_text(&s.session),
                opt_text(&s.harness),
                opt_text(&s.node),
                opt_text(&s.status),
                opt_int(s.age_s),
                opt_real(s.rss_mb),
                opt_text(&s.parent),
            ],
        ));
        if !s.has_seat() {
            continue;
        }
        steps.push((
            format!(
                "INSERT OR REPLACE INTO fleet_seat ({SEAT_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ),
            vec![
                id.clone(),
                SqlValue::Text(s.name.clone()),
                opt_text(&s.session_id),
                opt_text(&s.model),
                opt_text(&s.model_basis),
                opt_text(&s.started_at),
                opt_text(&s.last_message_at),
                opt_text(&s.cwd),
                opt_text(&s.project),
                opt_text(&s.role),
                opt_int(s.context_pct),
                SqlValue::Integer(machine.beat_at),
            ],
        ));
    }
    remote.transaction(&steps).map(drop).map_err(|(_, e)| e)
}

fn real(v: &SqlValue) -> Option<f64> {
    match v {
        SqlValue::Real(r) => Some(*r),
        SqlValue::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn text(v: &SqlValue) -> Option<String> {
    v.text().map(str::to_string)
}

/// What [`read`] answers: every machine, every session with its seat facts
/// joined on, and why the seat read failed when it did.
pub struct Fleet {
    pub machines: Vec<Machine>,
    pub sessions: Vec<Session>,
    pub seat_error: Option<String>,
}

/// Every machine row and every session row on the primary, each session
/// with its `fleet_seat` row when one exists. A seat whose session row is
/// gone, or whose beat is not its machine's newest, is dropped. A seat read
/// that fails leaves the sessions as read and names the failure in
/// `seat_error`.
pub fn read(remote: &Remote) -> Result<Fleet, String> {
    remote.script(SCHEMA)?;
    let machines = remote
        .execute(
            &format!("SELECT {MACHINE_COLUMNS} FROM fleet_machine ORDER BY owner, host"),
            &[],
        )?
        .rows
        .iter()
        .map(|r| Machine {
            machine_id: text(&r[0]).unwrap_or_default(),
            host: text(&r[1]).unwrap_or_default(),
            owner: text(&r[2]).unwrap_or_default(),
            role: text(&r[3]).unwrap_or_default(),
            beat_at: r[4].integer().unwrap_or(0),
            cpu: real(&r[5]),
            mem_used: real(&r[6]),
            mem_pressure: r[7].integer(),
            watts: real(&r[8]),
            gpu: real(&r[9]),
            verdict: text(&r[10]),
            cap: r[11].integer(),
            ceiling: r[12].integer(),
            takes_work: r[13].integer().unwrap_or(1) != 0,
        })
        .collect();
    let sessions = remote
        .execute(
            &format!("SELECT {SESSION_COLUMNS} FROM fleet_session ORDER BY machine_id, name"),
            &[],
        )?
        .rows
        .iter()
        .map(|r| Session {
            machine_id: text(&r[0]).unwrap_or_default(),
            name: text(&r[1]).unwrap_or_default(),
            session: text(&r[2]),
            harness: text(&r[3]),
            node: text(&r[4]),
            status: text(&r[5]),
            age_s: r[6].integer(),
            rss_mb: real(&r[7]),
            parent: text(&r[8]),
            ..Session::default()
        })
        .collect();
    let mut fleet = Fleet {
        machines,
        sessions,
        seat_error: None,
    };
    match remote.execute(&format!("SELECT {SEAT_COLUMNS} FROM fleet_seat"), &[]) {
        Ok(reply) => {
            for r in &reply.rows {
                let (machine_id, name) = (text(&r[0]), text(&r[1]));
                let current = fleet.machines.iter().any(|m| {
                    Some(&m.machine_id) == machine_id.as_ref() && Some(m.beat_at) == r[11].integer()
                });
                if !current {
                    continue;
                }
                let Some(s) = fleet.sessions.iter_mut().find(|s| {
                    Some(&s.machine_id) == machine_id.as_ref() && Some(&s.name) == name.as_ref()
                }) else {
                    continue;
                };
                s.session_id = text(&r[2]);
                s.model = text(&r[3]);
                s.model_basis = text(&r[4]);
                s.started_at = text(&r[5]);
                s.last_message_at = text(&r[6]);
                s.cwd = text(&r[7]);
                s.project = text(&r[8]);
                s.role = text(&r[9]);
                s.context_pct = r[10].integer();
            }
        }
        Err(error) => fleet.seat_error = Some(error),
    }
    Ok(fleet)
}

/// The map as JSON: one entry per machine with `fresh`, `age_s`, `local`
/// and its sessions. A child whose parent prefix matches a session on any
/// machine gets `parent_at` naming that session and host.
pub fn map_json(
    machines: &[Machine],
    sessions: &[Session],
    local_id: &str,
    now: i64,
    fresh_s: i64,
) -> Value {
    let parent_at = |prefix: &str| {
        sessions.iter().find_map(|p| {
            let id = p.session.as_deref()?;
            (!prefix.is_empty() && id.starts_with(prefix)).then(|| {
                let host = machines
                    .iter()
                    .find(|m| m.machine_id == p.machine_id)
                    .map_or("?", |m| m.host.as_str());
                format!("{}@{host}", p.name)
            })
        })
    };
    let rows: Vec<Value> = machines
        .iter()
        .map(|m| {
            let age = (now - m.beat_at).max(0);
            let mut row = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
            row["fresh"] = json!(age <= fresh_s);
            row["age_s"] = json!(age);
            row["local"] = json!(m.machine_id == local_id);
            row["sessions"] = sessions
                .iter()
                .filter(|s| s.machine_id == m.machine_id)
                .map(|s| {
                    let mut v = serde_json::to_value(s).unwrap_or_else(|_| json!({}));
                    v["parent_at"] = json!(s.parent.as_deref().and_then(|p| parent_at(p)));
                    v
                })
                .collect();
            row
        })
        .collect();
    json!({"machines": rows, "fresh_s": fresh_s})
}

/// The text rendering: one block per machine, sessions indented under it.
pub fn render(map: &Value) -> String {
    let mut out = Vec::new();
    if let Some(notice) = map.get("notice").and_then(Value::as_str) {
        out.push(notice.to_string());
    }
    if let Some(error) = map.get("error").and_then(Value::as_str) {
        out.push(format!("primary unreadable: {error}"));
    }
    for m in map["machines"].as_array().into_iter().flatten() {
        out.push(machine_line(m));
        for s in m["sessions"].as_array().into_iter().flatten() {
            out.push(format!("    {}", session_line(s)));
        }
    }
    out.join("\n")
}

fn pct(v: &Value) -> String {
    v.as_f64()
        .map_or("-".into(), |f| format!("{:.0}%", f * 100.0))
}

pub fn machine_line(m: &Value) -> String {
    let s = |k: &str| m[k].as_str().unwrap_or("-").to_string();
    let online = if m["fresh"].as_bool() == Some(true) {
        "online".to_string()
    } else {
        format!("offline {}s", m["age_s"].as_i64().unwrap_or(0))
    };
    let cap = match (m["cap"].as_i64(), m["ceiling"].as_i64()) {
        (Some(c), Some(ceil)) => format!("cap {c}/{ceil}"),
        _ => "cap -".into(),
    };
    let watts = m["watts"]
        .as_f64()
        .map_or("-".into(), |w| format!("{w:.0}W"));
    format!(
        "{host} ({owner}, {role}) {online} · cpu {cpu} · mem {mem} · {watts} · {verdict} · {cap} · {takes}",
        host = s("host"),
        owner = s("owner"),
        role = s("role"),
        cpu = pct(&m["cpu"]),
        mem = pct(&m["mem_used"]),
        verdict = s("verdict"),
        takes = if m["takes_work"].as_bool() == Some(false) {
            "takes no work"
        } else {
            "takes work"
        },
    )
}

pub fn session_line(s: &Value) -> String {
    let t = |k: &str| s[k].as_str().unwrap_or("-");
    let age = s["age_s"]
        .as_i64()
        .map_or("-".into(), |a| format!("{}m", a / 60));
    let mut line = format!(
        "{} {} {} {} {}",
        t("name"),
        t("harness"),
        t("node"),
        t("status"),
        age
    );
    if let Some(parent) = s["parent_at"].as_str() {
        line.push_str(&format!(" <- {parent}"));
    }
    line
}

/// The resident publisher: one beat every [`INTERVAL`], one in flight, off
/// the daemon's own loop.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    let remote = match crate::store_remote::configured() {
        Ok(Some(remote)) => remote,
        Ok(None) => return,
        Err(error) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("fleet-beacon: {error}"));
            return;
        }
    };
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < INTERVAL)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    std::thread::spawn(move || {
        let machine = local_machine(&home, chrono::Utc::now().timestamp());
        let sessions = sessions_from_top(
            &machine.machine_id,
            &crate::agents_top::payload(false, false),
        );
        if let Err(error) = publish(&remote, &machine, &sessions) {
            // An unreachable primary repeats every beat; say it once.
            static UNREACHABLE: std::sync::Once = std::sync::Once::new();
            if crate::store_remote::is_unreachable(&error) {
                UNREACHABLE.call_once(|| eprintln!("fleet-beacon: {error}"));
            } else {
                eprintln!("fleet-beacon: {error}");
            }
        }
        flag.store(false, Ordering::SeqCst);
    });
}

const USAGE: &str =
    "fno agents fleet top --machines [--json] [--since <dur>] [--takes-work on|off]\n\n\
Every machine on the shared primary (store.remote_url) and its sessions. A machine \
whose newest beat is older than --since (default 5m) shows offline. --takes-work \
off makes the spawn gate on THIS machine refuse new agent work; the next beat \
publishes the flag. With no store.remote_url this machine shows alone.";

/// `fno-agents census --workers --machines ...`, which the public
/// `fno agents fleet top --machines` execs.
pub fn run(args: &[String]) -> i32 {
    let mut as_json = false;
    let mut fresh_s = DEFAULT_FRESH_S;
    let mut toggle: Option<bool> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--machines" => {}
            "--json" | "-J" => as_json = true,
            "--since" => match it
                .next()
                .and_then(|v| crate::duration::parse_duration_secs(v))
            {
                Some(secs) => fresh_s = secs as i64,
                None => {
                    eprintln!("fno agents fleet top --machines: --since needs a duration like 5m");
                    return 2;
                }
            },
            "--takes-work" => match it.next().map(String::as_str) {
                Some("on") => toggle = Some(true),
                Some("off") => toggle = Some(false),
                _ => {
                    eprintln!("fno agents fleet top --machines: --takes-work needs on or off");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            other => {
                eprintln!("fno agents fleet top --machines: unknown flag {other}");
                return 2;
            }
        }
    }
    let home = AgentsHome::from_env();
    if let Some(on) = toggle {
        if let Err(e) = set_takes_work(&home, on) {
            eprintln!(
                "fno agents fleet top --machines: cannot write {}: {e}",
                takes_work_path(&home).display()
            );
            return 1;
        }
    }
    let now = chrono::Utc::now().timestamp();
    let local = local_machine(&home, now);
    let map = match crate::store_remote::configured() {
        Ok(Some(remote)) => match read(&remote) {
            Ok(mut fleet) => {
                // A toggle shows at once on this machine, before the next beat.
                if let Some(row) = fleet
                    .machines
                    .iter_mut()
                    .find(|m| m.machine_id == local.machine_id)
                {
                    row.takes_work = local.takes_work;
                }
                let mut map = map_json(
                    &fleet.machines,
                    &fleet.sessions,
                    &local.machine_id,
                    now,
                    fresh_s,
                );
                map["remote"] = json!(remote.url());
                if let Some(error) = fleet.seat_error {
                    map["seat_error"] = json!(error);
                }
                map
            }
            Err(error) => {
                let mut map = alone(&local, now, fresh_s);
                map["error"] = json!(error);
                map
            }
        },
        Ok(None) => {
            let mut map = alone(&local, now, fresh_s);
            map["notice"] = json!(NO_REMOTE_NOTICE);
            map
        }
        Err(error) => {
            let mut map = alone(&local, now, fresh_s);
            map["error"] = json!(error);
            map
        }
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
    } else {
        println!("{}", render(&map));
    }
    0
}

/// This machine alone, with its local sessions.
fn alone(local: &Machine, now: i64, fresh_s: i64) -> Value {
    let sessions = sessions_from_top(&local.machine_id, &crate::agents_top::payload(false, false));
    map_json(
        std::slice::from_ref(local),
        &sessions,
        &local.machine_id,
        now,
        fresh_s,
    )
}

#[cfg(test)]
#[path = "fleet_beacon_tests.rs"]
mod tests;

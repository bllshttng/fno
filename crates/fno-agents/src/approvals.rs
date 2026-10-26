//! `fno inbox approvals ls|show|decide`: inspect and decide pending approvals.
//!
//! Ported from `cli/src/fno/approvals` (cli.py, policy.py, and the read and
//! decide halves of store.py), the Python leg deleted in the same change. The
//! commands never execute an effect and never report an acknowledgment: a
//! destination acknowledges, and this process never talks to one. The submit
//! half of the gate lives in `effect_gate`; both share its schema and db.

use crate::effect_gate::{self, EffectRequest};
use crate::operator_turns::python_json;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const FAIL: i32 = 2;
const AUTHORITY_SOURCE: &str = "config.approvals.authorized_principals";
const USAGE: &str = "Usage: fno inbox approvals <ls|show|decide> ...\n\n  ls [--pending] [--json]\n  show <request-digest> [--json]\n  decide <request-digest> --as <principal> (--approve | --decline) [--transport <t>] [--json]\n";

/// A refusal with the stable shape callers parse, whatever the reason.
struct Refusal {
    reason: &'static str,
    detail: String,
    fields: Vec<&'static str>,
    authority_source: Option<&'static str>,
    recovery: Option<&'static str>,
}

enum Fail {
    Refused(Refusal),
    Other(String),
}

impl From<rusqlite::Error> for Fail {
    fn from(e: rusqlite::Error) -> Self {
        Fail::Other(e.to_string())
    }
}

struct Request {
    request_id: String,
    principal_id: String,
    work_order_id: String,
    attempt_id: String,
    effect_id: String,
    effect_class: String,
    destination: String,
    action_digest: String,
    created_at: String,
    expires_at: String,
    expires: chrono::DateTime<chrono::Utc>,
}

impl Request {
    /// The digest the CLI shows and joins decisions by: computed from the
    /// bound fields, never the stored column.
    fn digest(&self) -> String {
        EffectRequest {
            request_id: self.request_id.clone(),
            principal_id: self.principal_id.clone(),
            work_order_id: self.work_order_id.clone(),
            attempt_id: self.attempt_id.clone(),
            effect_id: self.effect_id.clone(),
            effect_class: self.effect_class.clone(),
            destination: self.destination.clone(),
            action_digest: self.action_digest.clone(),
            created_at: self.created_at.clone(),
            expires_at: self.expires_at.clone(),
        }
        .request_digest()
    }
}

struct Decision {
    decision: String,
    principal: String,
    decided_at: String,
    transport: Option<String>,
}

/// A stored timestamp as an aware UTC instant; a naive one reads as UTC.
fn parse_ts(value: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(t.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|n| n.and_utc())
        .map_err(|e| format!("unreadable timestamp {value}: {e}"))
}

fn request_from_row(row: &rusqlite::Row) -> Result<Request, Fail> {
    let created: String = row.get("created_at")?;
    let expires: String = row.get("expires_at")?;
    let created = parse_ts(&created).map_err(Fail::Other)?;
    let expires = parse_ts(&expires).map_err(Fail::Other)?;
    Ok(Request {
        request_id: row.get("request_id")?,
        principal_id: row.get("principal_id")?,
        work_order_id: row.get("work_order_id")?,
        attempt_id: row.get("attempt_id")?,
        effect_id: row.get("effect_id")?,
        effect_class: row.get("effect_class")?,
        destination: row.get("destination")?,
        action_digest: row.get("action_digest")?,
        created_at: effect_gate::isoformat(created),
        expires_at: effect_gate::isoformat(expires),
        expires,
    })
}

fn list_requests(conn: &Connection, pending_only: bool) -> Result<Vec<Request>, Fail> {
    let mut sql = String::from("SELECT r.* FROM requests r");
    if pending_only {
        sql.push_str(" LEFT JOIN decisions d ON d.request_digest = r.request_digest");
        sql.push_str(" WHERE d.request_digest IS NULL");
    }
    sql.push_str(" ORDER BY r.created_at");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(request_from_row(row)?);
    }
    Ok(out)
}

fn get_request(conn: &Connection, digest: &str) -> Result<Option<Request>, Fail> {
    let mut stmt = conn.prepare("SELECT * FROM requests WHERE request_digest = ?")?;
    let mut rows = stmt.query([digest])?;
    match rows.next()? {
        Some(row) => Ok(Some(request_from_row(row)?)),
        None => Ok(None),
    }
}

fn decision_of(conn: &Connection, digest: &str) -> Result<Option<Decision>, Fail> {
    let row = conn
        .query_row(
            "SELECT decision, deciding_principal_id, decided_at, transport FROM decisions \
             WHERE request_digest = ?",
            [digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((decision, principal, decided_at, transport)) = row else {
        return Ok(None);
    };
    let decided_at = effect_gate::isoformat(parse_ts(&decided_at).map_err(Fail::Other)?);
    Ok(Some(Decision {
        decision,
        principal,
        decided_at,
        transport,
    }))
}

fn attempts_json(conn: &Connection, digest: &str) -> Result<Vec<Value>, Fail> {
    let mut stmt = conn.prepare(
        "SELECT effect_id, work_order_id, attempt_id, request_digest, idempotency_key, \
         action_digest, destination, effect_class, adapter_id, adapter_version, state, \
         external_ref, reconciliation_ref FROM attempts WHERE request_digest = ? \
         ORDER BY idempotency_key",
    )?;
    let mut rows = stmt.query([digest])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let text = |i: usize| -> Result<Value, Fail> { Ok(Value::from(row.get::<_, String>(i)?)) };
        let opt = |i: usize| -> Result<Value, Fail> {
            Ok(row
                .get::<_, Option<String>>(i)?
                .map_or(Value::Null, Value::from))
        };
        let mut m = Map::new();
        for (i, key) in [
            "effect_id",
            "work_order_id",
            "attempt_id",
            "request_digest",
            "idempotency_key",
            "action_digest",
            "destination",
            "effect_class",
            "adapter_id",
            "adapter_version",
            "state",
        ]
        .iter()
        .enumerate()
        {
            m.insert((*key).to_string(), text(i)?);
        }
        m.insert("external_ref".into(), opt(11)?);
        m.insert("reconciliation_ref".into(), opt(12)?);
        out.push(Value::Object(m));
    }
    Ok(out)
}

/// Every bound field, the decision, and the current effect state.
fn describe(conn: &Connection, request: &Request) -> Result<Value, Fail> {
    let digest = request.digest();
    let decision = decision_of(conn, &digest)?;
    let attempts = attempts_json(conn, &digest)?;
    let opt = |f: fn(&Decision) -> Option<String>| decision.as_ref().and_then(f);
    Ok(json!({
        "request_digest": digest,
        "request_id": request.request_id,
        "principal_id": request.principal_id,
        "work_order_id": request.work_order_id,
        "work_order_attempt": request.attempt_id,
        "effect_id": request.effect_id,
        "effect_class": request.effect_class,
        "destination": request.destination,
        "action_digest": request.action_digest,
        "created_at": request.created_at,
        "expires_at": request.expires_at,
        "expired": request.expires <= chrono::Utc::now(),
        "decision": opt(|d| Some(d.decision.clone())),
        "deciding_principal_id": opt(|d| Some(d.principal.clone())),
        "decided_at": opt(|d| Some(d.decided_at.clone())),
        "decision_transport": opt(|d| d.transport.clone()),
        "effect_attempts": attempts,
    }))
}

fn s<'a>(record: &'a Value, key: &str) -> &'a str {
    record[key].as_str().unwrap_or("")
}

fn print_human(record: &Value) {
    println!("request     {}", s(record, "request_digest"));
    for (label, key) in [
        ("principal", "principal_id"),
        ("work order", "work_order_id"),
        ("attempt", "work_order_attempt"),
        ("effect", "effect_id"),
        ("class", "effect_class"),
        ("destination", "destination"),
        ("action", "action_digest"),
        ("expires", "expires_at"),
    ] {
        println!("  {:<12}{}", label, s(record, key));
    }
    if record["decision"].is_null() {
        println!("  {:<12}pending", "decision");
    } else {
        println!(
            "  {:<12}{} by {} at {}",
            "decision",
            s(record, "decision"),
            s(record, "deciding_principal_id"),
            s(record, "decided_at")
        );
        if let Some(t) = record["decision_transport"]
            .as_str()
            .filter(|t| !t.is_empty())
        {
            println!("  {:<12}{t} (informational, not authority)", "transport");
        }
    }
    if record["expired"].as_bool().unwrap_or(false) {
        println!("  {:<12}yes - a new request is required", "expired");
    }
    let attempts = record["effect_attempts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if attempts.is_empty() {
        println!("  {:<12}not prepared", "effect");
        return;
    }
    for attempt in &attempts {
        let state = s(attempt, "state");
        println!(
            "  {:<12}{}: {state}",
            "effect",
            s(attempt, "idempotency_key")
        );
        if state == "unknown" {
            println!(
                "               outcome ambiguous: retry only through an adapter that enforces \
                 this idempotency key remotely, or read the destination and supply that \
                 reconciliation result"
            );
        }
        if let Some(r) = attempt["external_ref"].as_str().filter(|r| !r.is_empty()) {
            println!("               destination ref: {r}");
        }
    }
}

// -- Authority ---------------------------------------------------------------

/// Whether any effect class names at least one principal: policy.py's
/// `is_configured`. Unconfigured means nobody may decide.
fn authority_configured(cwd: &Path) -> bool {
    crate::agents_config::config_table_merged(cwd, &["approvals", "authorized_principals"])
        .map(|t| {
            t.values()
                .any(|v| v.as_array().is_some_and(|list| !list.is_empty()))
        })
        .unwrap_or(false)
}

// -- Decide ------------------------------------------------------------------

fn refused(
    reason: &'static str,
    detail: String,
    fields: &[&'static str],
    authority_source: Option<&'static str>,
    recovery: Option<&'static str>,
) -> Fail {
    Fail::Refused(Refusal {
        reason,
        detail,
        fields: fields.to_vec(),
        authority_source,
        recovery,
    })
}

struct DecideArgs<'a> {
    digest: &'a str,
    principal: &'a str,
    decision: &'static str,
    transport: Option<&'a str>,
}

/// Decide one exact request digest inside one write transaction. The clock is
/// read after the lock is held: a timestamp taken before a long busy wait
/// could clear an expiration that passed while the call queued.
fn decide_tx(conn: &Connection, cwd: &Path, args: &DecideArgs) -> Result<Decision, Fail> {
    let now = chrono::Utc::now();
    let row = conn
        .query_row(
            "SELECT effect_class, destination, expires_at, work_order_id, attempt_id, effect_id \
             FROM requests WHERE request_digest = ?",
            [args.digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((effect_class, destination, expires_at, work_order, attempt, effect)) = row else {
        return Err(refused(
            "unknown_request",
            format!("no request matches digest {}", args.digest),
            &["request_digest"],
            None,
            Some("Re-submit the request; an edited action produces a new digest."),
        ));
    };

    if let Some(prior) = decision_of(conn, args.digest)? {
        // Immutable: the same decision by the same principal reads back as is.
        if prior.principal == args.principal && prior.decision == args.decision {
            return Ok(prior);
        }
        return Err(refused(
            "replay",
            format!(
                "request {} was already decided {} by {}",
                args.digest, prior.decision, prior.principal
            ),
            &["request_digest"],
            None,
            Some("A decided request is immutable. Submit a new request instead."),
        ));
    }

    if parse_ts(&expires_at).map_err(Fail::Other)? <= now {
        return Err(refused(
            "expired",
            format!("request {} expired at {expires_at}", args.digest),
            &["expires_at"],
            None,
            Some("Submit a new request with a fresh expiration."),
        ));
    }

    if !effect_gate::authorized(cwd, args.principal, &effect_class) {
        return Err(refused(
            "unauthorized_principal",
            format!(
                "principal {} may not decide {effect_class} to {destination}",
                args.principal
            ),
            &["deciding_principal_id"],
            Some(AUTHORITY_SOURCE),
            Some("Have an authorized principal decide this request."),
        ));
    }

    let decided_at = effect_gate::isoformat(now);
    conn.execute(
        "INSERT INTO decisions (request_digest, deciding_principal_id, decision, decided_at, \
         transport) VALUES (?,?,?,?,?)",
        params![
            args.digest,
            args.principal,
            args.decision,
            decided_at,
            args.transport
        ],
    )?;
    let event_id = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}{}{}",
                args.digest,
                now.timestamp_micros(),
                std::process::id()
            )
            .as_bytes()
        )
    )[..32]
        .to_string();
    let payload = json!({
        "ts": now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "type": "approval_decided",
        "source": "approvals",
        "data": {
            "request_digest": args.digest,
            "decision": args.decision,
            "deciding_principal_id": args.principal,
            "work_order_id": work_order,
            "attempt_id": attempt,
            "effect_id": effect,
            "authority_source": AUTHORITY_SOURCE,
            "transport": args.transport,
            "event_id": event_id,
        },
    });
    conn.execute(
        "INSERT INTO outbox (event_id, payload) VALUES (?,?)",
        params![event_id, payload.to_string()],
    )?;
    Ok(Decision {
        decision: args.decision.to_string(),
        principal: args.principal.to_string(),
        decided_at,
        transport: args.transport.map(str::to_string),
    })
}

fn decide(conn: &Connection, cwd: &Path, args: &DecideArgs) -> Result<Decision, Fail> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match decide_tx(conn, cwd, args) {
        Ok(decision) => {
            conn.execute_batch("COMMIT")?;
            effect_gate::drain(conn, Some(&effect_gate::events_for(cwd)));
            Ok(decision)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// -- Commands ----------------------------------------------------------------

struct Flags {
    json: bool,
    pending: bool,
    approve: bool,
    decline: bool,
    db: Option<PathBuf>,
    principal: Option<String>,
    transport: Option<String>,
    positional: Vec<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut f = Flags {
        json: false,
        pending: false,
        approve: false,
        decline: false,
        db: None,
        principal: None,
        transport: None,
        positional: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("Option '{name}' requires an argument."))
        };
        match arg {
            "--json" | "-J" => f.json = true,
            "--pending" => f.pending = true,
            "--approve" => f.approve = true,
            "--decline" => f.decline = true,
            "--db" => f.db = Some(PathBuf::from(value(arg)?)),
            "--as" => f.principal = Some(value(arg)?),
            "--transport" => f.transport = Some(value(arg)?),
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(format!("No such option: {flag}"))
            }
            word => f.positional.push(word.to_string()),
        }
        i += 1;
    }
    Ok(f)
}

fn open(db: Option<PathBuf>, cwd: &Path) -> Result<Connection, String> {
    let path = match db {
        Some(path) => path,
        None => effect_gate::default_db_path(cwd)?,
    };
    effect_gate::open_db(&path)
}

fn fail(message: &str) -> i32 {
    eprintln!("{message}");
    FAIL
}

fn print_refusal(refusal: &Refusal, as_json: bool) -> i32 {
    if as_json {
        let doc = json!({
            "result": "refused",
            "reason": refusal.reason,
            "detail": refusal.detail,
            "fields": refusal.fields,
            "authority_source": refusal.authority_source,
            "recovery": refusal.recovery,
        });
        println!("{}", python_json(&doc));
    } else {
        eprintln!("REFUSED [{}] {}", refusal.reason, refusal.detail);
        if !refusal.fields.is_empty() {
            eprintln!("  conflicting fields: {}", refusal.fields.join(", "));
        }
        if let Some(source) = refusal.authority_source {
            eprintln!("  authority consulted: {source}");
        }
        if let Some(recovery) = refusal.recovery {
            eprintln!("  recovery: {recovery}");
        }
    }
    FAIL
}

fn finish<T>(result: Result<T, Fail>, then: impl FnOnce(T) -> i32) -> i32 {
    match result {
        Ok(value) => then(value),
        Err(Fail::Refused(r)) => print_refusal(&r, false),
        Err(Fail::Other(e)) => fail(&e),
    }
}

fn cmd_ls(f: Flags, cwd: &Path) -> i32 {
    let conn = match open(f.db.clone(), cwd) {
        Ok(conn) => conn,
        Err(e) => return fail(&e),
    };
    let records = list_requests(&conn, f.pending).and_then(|requests| {
        requests
            .iter()
            .map(|r| describe(&conn, r))
            .collect::<Result<Vec<_>, _>>()
    });
    finish(records, |records| {
        if f.json {
            println!("{}", python_json(&Value::Array(records)));
        } else if records.is_empty() {
            println!(
                "{}",
                if f.pending {
                    "no pending approval requests"
                } else {
                    "no approval requests"
                }
            );
        } else {
            for record in &records {
                let state = record["decision"].as_str().unwrap_or("pending");
                let digest = s(record, "request_digest");
                println!(
                    "{}  {:<9} {:<26} {}",
                    &digest[..12.min(digest.len())],
                    state,
                    s(record, "effect_class"),
                    s(record, "destination")
                );
            }
        }
        0
    })
}

fn cmd_show(f: Flags, cwd: &Path) -> i32 {
    let [digest] = f.positional.as_slice() else {
        return fail("Usage: fno inbox approvals show <request-digest> [--json]");
    };
    let conn = match open(f.db.clone(), cwd) {
        Ok(conn) => conn,
        Err(e) => return fail(&e),
    };
    let record = get_request(&conn, digest).and_then(|request| match request {
        Some(request) => describe(&conn, &request).map(Some),
        None => Ok(None),
    });
    finish(record, |record| match record {
        None => fail(&format!("no request matches digest {digest}")),
        Some(record) => {
            if f.json {
                println!("{}", python_json(&record));
            } else {
                print_human(&record);
            }
            0
        }
    })
}

fn cmd_decide(f: Flags, cwd: &Path) -> i32 {
    let ([digest], Some(principal)) = (f.positional.as_slice(), f.principal.as_deref()) else {
        return fail(
            "Usage: fno inbox approvals decide <request-digest> --as <principal> \
             (--approve | --decline)",
        );
    };
    if f.approve == f.decline {
        return fail("choose exactly one of --approve or --decline");
    }
    if !authority_configured(cwd) {
        return fail(
            "no approval policy is configured, so nobody may decide. Set \
             config.approvals.authorized_principals before deciding.",
        );
    }
    let conn = match open(f.db.clone(), cwd) {
        Ok(conn) => conn,
        Err(e) => return fail(&e),
    };
    let args = DecideArgs {
        digest,
        principal,
        decision: if f.approve { "approved" } else { "declined" },
        transport: f.transport.as_deref(),
    };
    let outcome = decide(&conn, cwd, &args).and_then(|recorded| {
        let request = get_request(&conn, digest)?
            .ok_or_else(|| Fail::Other(format!("no request matches digest {digest}")))?;
        Ok((recorded, describe(&conn, &request)?))
    });
    match outcome {
        Err(Fail::Refused(r)) => print_refusal(&r, f.json),
        Err(Fail::Other(e)) => fail(&e),
        Ok((recorded, record)) => {
            if f.json {
                let doc = json!({
                    "result": recorded.decision,
                    "request_digest": digest,
                    "deciding_principal_id": recorded.principal,
                    "authority_source": AUTHORITY_SOURCE,
                    "executed": false,
                    "acknowledged": false,
                    "request": record,
                });
                println!("{}", python_json(&doc));
                return 0;
            }
            println!("{} by {}", recorded.decision, recorded.principal);
            println!("  authority   {AUTHORITY_SOURCE}");
            if f.approve {
                println!("  effect      not executed - approval is not execution");
                println!("  delivery    not acknowledged - only the destination can acknowledge");
            } else {
                println!("  effect      terminal - a declined request never executes");
            }
            0
        }
    }
}

/// The `fno-agents compaction approvals <ls|show|decide> ...` entry.
pub fn run(args: &[String]) -> i32 {
    let Some((command, rest)) = args.split_first() else {
        print!("{USAGE}");
        return FAIL;
    };
    if matches!(command.as_str(), "--help" | "-h" | "help") {
        print!("{USAGE}");
        return 0;
    }
    let flags = match parse_flags(rest) {
        Ok(flags) => flags,
        Err(e) => return fail(&e),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match command.as_str() {
        "ls" => cmd_ls(flags, &cwd),
        "show" => cmd_show(flags, &cwd),
        "decide" => cmd_decide(flags, &cwd),
        other => fail(&format!("No such command '{other}'.\n{USAGE}")),
    }
}

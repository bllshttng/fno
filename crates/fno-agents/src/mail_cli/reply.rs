//! The `mail reply` front leaf (Python `mail.cli.cmd_reply`, now a forward
//! leaf): reply to a message id, routed by the answered message's lane.
//!
//! Routing contract (docs/architecture/mail-reply-routing.md): the id resolves
//! against the durable bus FIRST and answers name/session/node mail back at
//! its original sender; an id absent from the bus is recovered off THIS
//! session's transcript (the live-injected envelope), with the chats index as
//! the first receipt read; only an id absent from both is a hard error. A bus
//! row addressed to a project falls to the thread-store append / orphan
//! self-note. The cross-host branch runs before any of that:
//! `reply_cross_host` owns foreign-host senders, exactly as it did when the
//! front claimed the verb for the remote lane.
//!
//! The delivery rungs are the send leaf's shared choke points
//! (`name_lane_send`, `write_durable_thread`, `append_to_kind_thread`); this
//! module owns reply's routing, refusals and transcript reads only. Goldens
//! captured from the Python verb through tests/capture/mail_reply_capture.py
//! freeze the bytes.

use serde_json::{json, Value};
use std::path::PathBuf;

use crate::mail_cli::ack::{bus_dir, load_messages};
use crate::mail_cli::send;
use crate::mail_cli::send::lanes::{name_lane_send, NameLane, NameLaneArgs};
use crate::mail_cli::send::resolve::{is_full_session_id, resolve_reachable, ReachResult};

/// Valid `--kind` values (Python `VALID_KINDS` = `AUTHORED_MAIL_KINDS`).
const VALID_KINDS: [&str; 4] = ["fyi", "heads-up", "question", "send"];

/// Python `DEPRECATED_KINDS`: removed kinds name their replacement.
fn deprecated_kind_replacement(kind: &str) -> Option<&'static str> {
    match kind {
        "notification" => Some("fyi"),
        "lesson" => Some("fyi --persist memory"),
        "answer" => Some("fyi --reply-to <msg-id>  (or any kind with --reply-to)"),
        "complete" => Some("fyi --reply-to <msg-id>"),
        _ => None,
    }
}

/// Python `LEGACY_HANDLE_RE`: the retired `<harness>-<hex6+>` address form.
/// Kept only so a pre-flip record's sender can be migrated and answered;
/// never an accepted address, never generated.
fn is_legacy_handle(token: &str) -> bool {
    let Some((harness, rest)) = token.split_once('-') else {
        return false;
    };
    crate::provider::KNOWN_HARNESSES.contains(&harness)
        && (6..=64).contains(&rest.len())
        && rest.chars().all(|c| c.is_ascii_hexdigit())
}

/// typer's usage block for this verb; every parameter refusal rides it.
fn usage_error(detail: &str) -> i32 {
    eprintln!("Usage: fno agents mail reply [OPTIONS] [BODY_ARG]");
    eprintln!("Try 'fno agents mail reply --help' for help.");
    eprintln!();
    eprintln!("Error: {detail}");
    2
}

struct ReplyArgs {
    to: Option<String>,
    kind: String,
    kind_seen: bool,
    body: Option<String>,
    body_file: Option<PathBuf>,
    positional: Option<String>,
    ref_mission: Option<String>,
    source_mission: Option<String>,
    cascade_of: Option<String>,
    from_project: Option<String>,
    json_out: bool,
    sender_session: Option<String>,
    style_exception: Option<String>,
}

fn parse_args(args: &[String]) -> Result<ReplyArgs, i32> {
    let mut out = ReplyArgs {
        to: None,
        kind: "fyi".to_string(),
        kind_seen: false,
        body: None,
        body_file: None,
        positional: None,
        ref_mission: None,
        source_mission: None,
        cascade_of: None,
        from_project: None,
        json_out: false,
        sender_session: None,
        style_exception: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut flag_value = |name: &str| match it.next() {
            Some(v) => Ok(v.clone()),
            None => Err(usage_error(&format!(
                "Option '{name}' requires an argument."
            ))),
        };
        match a.as_str() {
            "--to" => out.to = Some(flag_value("--to")?),
            "--kind" => {
                out.kind = flag_value("--kind")?;
                out.kind_seen = true;
            }
            "--body" => out.body = Some(flag_value("--body")?),
            "--body-file" => out.body_file = Some(PathBuf::from(flag_value("--body-file")?)),
            "--ref-mission" => out.ref_mission = Some(flag_value("--ref-mission")?),
            "--source-mission" => out.source_mission = Some(flag_value("--source-mission")?),
            "--cascade-of" => out.cascade_of = Some(flag_value("--cascade-of")?),
            "--from" => out.from_project = Some(flag_value("--from")?),
            "--json" | "-J" => out.json_out = true,
            "--sender-session" => out.sender_session = Some(flag_value("--sender-session")?),
            "--style-exception" => out.style_exception = Some(flag_value("--style-exception")?),
            other => {
                if other.starts_with('-') && other.len() > 1 {
                    return Err(usage_error(&format!("Unknown argument: '{other}'.")));
                }
                if out.positional.is_some() {
                    return Err(usage_error(&format!(
                        "Got more than one positional value: '{other}'."
                    )));
                }
                out.positional = Some(other.to_string());
            }
        }
    }
    Ok(out)
}

/// Python `_read_body`: the body from whichever one of the three forms was
/// used; two at once is an explicit error, never a precedence rule. File
/// reads (and `-` for stdin) mirror `read_text_arg`.
fn resolve_body(args: &ReplyArgs) -> Result<String, i32> {
    let supplied = [
        args.positional.is_some(),
        args.body.is_some(),
        args.body_file.is_some(),
    ]
    .iter()
    .filter(|used| **used)
    .count();
    if supplied > 1 {
        eprintln!(
            "error: provide the body once - as a positional argument, --body, or --body-file"
        );
        return Err(1);
    }
    if let Some(path) = &args.body_file {
        if path.as_os_str() == "-" {
            use std::io::Read;
            let mut buf = String::new();
            match std::io::stdin().read_to_string(&mut buf) {
                Ok(_) => return Ok(buf),
                Err(_) => {
                    eprintln!("error: could not read the body from stdin");
                    return Err(1);
                }
            }
        }
        return match std::fs::read_to_string(path) {
            Ok(text) => Ok(text),
            Err(e) => {
                eprintln!("error: could not read {}: {e}", path.display());
                Err(1)
            }
        };
    }
    if let Some(body) = &args.body {
        return Ok(body.clone());
    }
    if let Some(pos) = &args.positional {
        return Ok(pos.clone());
    }
    eprintln!("error: provide a body - as a positional argument, --body, or --body-file");
    Err(1)
}

/// Python `_validate_kind`: valid kinds pass; removed kinds hint at their
/// replacement; anything else refuses naming the valid set.
fn validate_kind(kind: &str) -> Result<String, i32> {
    if VALID_KINDS.contains(&kind) {
        return Ok(kind.to_string());
    }
    if let Some(replacement) = deprecated_kind_replacement(kind) {
        eprintln!(
            "error: kind '{kind}' was removed in the 2026-05 inbox redesign. \
             Use --kind {replacement} instead. Valid kinds: fyi, heads-up, question, send"
        );
        return Err(1);
    }
    eprintln!("error: unknown kind '{kind}'. Valid kinds: fyi, heads-up, question, send");
    Err(1)
}

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let Some(to_msg) = parsed.to.clone() else {
        return usage_error("Missing option '--to'.");
    };
    let body_text = match resolve_body(&parsed) {
        Ok(text) => text,
        Err(code) => return code,
    };

    // The cross-host branch first: a foreign-host sender is a cross-host
    // send, and the flags the store lane cannot carry are refused there
    // naming each. Everything else falls through to the local verb.
    let mut extra: Vec<String> = Vec::new();
    if parsed.json_out {
        extra.push("--json".to_string());
    }
    for (seen, name) in [
        (&parsed.from_project, "--from"),
        (&parsed.sender_session, "--sender-session"),
        (&parsed.style_exception, "--style-exception"),
        (&parsed.ref_mission, "--ref-mission"),
        (&parsed.source_mission, "--source-mission"),
        (&parsed.cascade_of, "--cascade-of"),
    ] {
        if seen.is_some() {
            extra.push(name.to_string());
        }
    }
    // The RemoteMail tail parser counted ANY --kind spelling, so the default
    // value rides the cross-host drop-note exactly as it did there.
    let kind_given = parsed.kind_seen;
    if let Some(code) =
        crate::mail_remote::reply_cross_host(&to_msg, &body_text, kind_given, &extra)
    {
        return code;
    }

    let kind = match validate_kind(&parsed.kind) {
        Ok(kind) => kind,
        Err(code) => return code,
    };
    if let Err(code) = send::vet_body(&body_text, parsed.style_exception.as_deref()) {
        return code;
    }
    let classified_origin = match send::classify_origin(None) {
        Ok(origin) => origin,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 2;
        }
    };
    let mail_origin = if classified_origin == "unknown" {
        None
    } else {
        Some(classified_origin.as_str())
    };
    send::record_mail_origin(
        &classified_origin,
        "reply",
        &send::stamp_from(parsed.from_project.as_deref()),
        None,
        Some(&body_text),
        Some(&to_msg),
    );

    // Directed-lane routing: look the --to msg-id up on the durable bus and
    // answer name/session/node mail back to its original sender. Anything
    // else falls through to the thread-store reply below.
    let orig = bus_dir()
        .map(|bus| load_messages(&bus))
        .unwrap_or_default()
        .into_iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(to_msg.as_str()));

    if let Some(orig) = &orig {
        let to_kind = orig
            .get("to_kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if to_kind == "name" || to_kind == "session" || to_kind == "node" {
            return directed_lane_reply(&orig, to_kind, &parsed, &body_text, &to_msg, mail_origin);
        }
        return thread_store_reply(&parsed, kind.as_str(), &body_text, &to_msg, mail_origin);
    }

    // US3: a live-confirmed delivery writes no durable thread, so the id is
    // not on the bus. Before erroring, recover the sender off THIS session's
    // transcript, where the injected <fno_mail id=...> envelope already
    // carries `from`, and reply to that handle by identity.
    match resolve_live_sender(&to_msg) {
        Some(live_sender) => reply_ladder(
            &live_sender,
            false,
            parsed.sender_session.as_deref(),
            &body_text,
            parsed.from_project.as_deref(),
            &to_msg,
            mail_origin,
        ),
        // AC1-ERR / LD4: the name lane cannot invent a target from nothing.
        // An id absent from BOTH the bus and the transcript is genuinely
        // unknown: hard error, never a silent self-note.
        None => {
            eprintln!("msg-id '{to_msg}' not in the bus log or this session's transcript");
            1
        }
    }
}

/// The directed lanes (to_kind name/session/node). A stored sender predating
/// the address flip carries the retired `<harness>-<short8>` form: migrate it
/// and deliver. Refusing here would invent a wall at a knowledge boundary.
fn directed_lane_reply(
    orig: &Value,
    to_kind: &str,
    parsed: &ReplyArgs,
    body_text: &str,
    to_msg: &str,
    origin: Option<&str>,
) -> i32 {
    let str_field = |key: &str| -> Option<String> {
        orig.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut target = str_field("from").unwrap_or_default();
    let mut require_resolution = false;
    // Full sender provenance wins on EVERY lane, not only the
    // session-addressed one: the name lane stamps `from_session` on all three
    // of its records. The head-8 the row also carries is a display handle
    // whose first eight hex collide within a UUIDv7 clock bucket; the record
    // already held the address that works.
    if let Some(from_session) = str_field("from_session") {
        if is_full_session_id(&from_session) {
            target = from_session;
        } else if to_kind == "session" {
            require_resolution = true;
        }
    } else if to_kind == "session" {
        // A session-addressed record without full sender provenance may use
        // its stored sender only when current discovery proves that token
        // uniquely. Never demote an unverified mutable alias.
        require_resolution = true;
    } else if is_legacy_handle(&target) {
        let migrated = crate::identity::canonical_handle(target.split('-').nth(1).unwrap_or(""));
        eprintln!(
            "note: stored sender '{target}' is a retired address form \
             (pre-flip record); resolving legacy token '{migrated}'."
        );
        target = migrated;
        require_resolution = true;
    }
    reply_ladder(
        &target,
        require_resolution,
        parsed.sender_session.as_deref(),
        body_text,
        parsed.from_project.as_deref(),
        to_msg,
        origin,
    )
}

/// Python `_reply_to_name_handle`: the reply ladder over the resolved sender
/// handle. The uniqueness check exists to disambiguate a TYPED name; this
/// handle came off the answered message and was never typed, so an unreadable
/// store is not evidence against it: the reply fails open to the durable
/// floor, unless a candidate was visible whose uniqueness went unproven (the
/// wake-a-stranger guess) or the caller marked the handle a guess
/// (`require_resolution`).
fn reply_ladder(
    target: &str,
    require_resolution: bool,
    sender_session: Option<&str>,
    body_text: &str,
    from_project: Option<&str>,
    to_msg: &str,
    origin: Option<&str>,
) -> i32 {
    let reach = resolve_reachable(target);
    route_ladder(
        &reach,
        target,
        require_resolution,
        sender_session,
        body_text,
        from_project,
        to_msg,
        origin,
    )
}

/// The ladder's routing table, input-complete: every branch reads only the
/// `ReachResult` and the caller's arguments, so the refusal contracts test
/// against crafted results instead of monkeypatched stores.
fn route_ladder(
    reach: &ReachResult,
    target: &str,
    require_resolution: bool,
    sender_session: Option<&str>,
    body_text: &str,
    from_project: Option<&str>,
    to_msg: &str,
    origin: Option<&str>,
) -> i32 {
    if let Some(resolved) = &reach.hit {
        // An unreadable store hides sessions a clean read would have seen, so
        // a hit beside a degraded read is unproven uniqueness: refuse before
        // any delivery side effect. Do not unify with the fail-open below -
        // they differ in where the handle came from and what a wrong choice
        // costs (a woken stranger).
        if !reach.degraded.is_empty() {
            let failed = reach.degraded.join(", ");
            return usage_error(&format!(
                "Invalid value: sender handle '{target}' cannot be checked uniquely; \
                 unreadable stores: {failed}"
            ));
        }
        return run_name_lane(NameLaneArgs {
            message: body_text,
            from_name: from_project,
            resolved: Some(resolved.clone()),
            recipient: None,
            provider: None,
            reply_to: Some(to_msg),
            token: None,
            force: false,
            origin,
            subject: None,
        });
    }
    if !reach.degraded.is_empty() {
        // No candidate anywhere is the fail-open case: nothing to choose
        // between, so the reply falls to the durable floor addressed to the
        // handle the answered message recorded. That wakes nobody and invents
        // no address.
        let failed = reach.degraded.join(", ");
        eprintln!(
            "note: reachability stores unreadable ({failed}); replying to '{target}' \
             as recorded on the answered message."
        );
        return floor_to(target, body_text, from_project, to_msg, origin);
    }
    if !reach.ambiguous.is_empty() {
        // The collision escape hatch, and the ONLY place `--sender-session`
        // is read: `ambiguous` IS the candidate set. A threaded reply that
        // cannot be sent is worse than an unthreaded one.
        if let Some(sender_session) = sender_session {
            let wanted = crate::spawn_context::session_identity_key(sender_session);
            let chosen = reach
                .ambiguous
                .iter()
                .find(|candidate| crate::spawn_context::session_identity_key(candidate) == wanted);
            return match chosen {
                Some(chosen) => run_name_lane(NameLaneArgs {
                    message: body_text,
                    from_name: from_project,
                    resolved: None,
                    recipient: None,
                    provider: None,
                    reply_to: Some(to_msg),
                    token: Some(chosen),
                    force: false,
                    origin,
                    subject: None,
                }),
                None => usage_error(&format!(
                    "Invalid value: --sender-session '{sender_session}' is not one of the \
                     sessions '{target}' could name: {}. Nothing was sent.",
                    reach.ambiguous.join(", ")
                )),
            };
        }
        return usage_error(&format!(
            "Invalid value: sender handle '{target}' is ambiguous across sessions: {}. \
             Re-run with --sender-session <full-session-id> to answer one and keep the thread.",
            reach.ambiguous.join(", ")
        ));
    }
    if require_resolution {
        return usage_error(&format!(
            "Invalid value: retired sender handle '{target}' cannot be resolved uniquely"
        ));
    }
    // AC1-FR: the original sender is no longer live -> durable floor
    // addressed to their canonical handle, still drainable.
    floor_to(target, body_text, from_project, to_msg, origin)
}

/// The durable-floor rung: `_name_lane_send(recipient=target)`.
fn floor_to(
    target: &str,
    body_text: &str,
    from_project: Option<&str>,
    to_msg: &str,
    origin: Option<&str>,
) -> i32 {
    run_name_lane(NameLaneArgs {
        message: body_text,
        from_name: from_project,
        resolved: None,
        recipient: Some(target),
        provider: None,
        reply_to: Some(to_msg),
        token: None,
        force: false,
        origin,
        subject: None,
    })
}

/// One `name_lane_send` call. The ladder above resolves before calling, so
/// the token/refusal outcomes here are defensive mirrors of the send leaf's.
fn run_name_lane(args: NameLaneArgs) -> i32 {
    match name_lane_send(args) {
        Ok(NameLane::Done) => 0,
        Ok(NameLane::UnreachableToken) => {
            crate::mail_cli::send::resolve::unreachable_token_exit("the sender handle", &[], 16)
        }
        Ok(NameLane::AmbiguousToken(candidates)) => {
            crate::mail_cli::send::resolve::ambiguous_token_exit("the sender handle", &candidates)
        }
        Ok(NameLane::UnavailableToken { failed, candidates }) => {
            crate::mail_cli::send::resolve::unavailable_token_exit(
                "the sender handle",
                &failed,
                &candidates,
            )
        }
        Err(code) => code,
    }
}

/// Python `reply_resolve.resolve_live_sender`: the chats index first (the
/// receipt read the Python leg made through the `chats resolve` child), then
/// this session's own transcript stores.
fn resolve_live_sender(msg_id: &str) -> Option<String> {
    if let Ok(hit) = crate::chats::resolve_prefix(msg_id) {
        if !hit.from_key.is_empty() {
            return Some(hit.from_key);
        }
    }
    for harness in candidate_harnesses() {
        let Some(session_id) = harness.session_id else {
            continue;
        };
        let Some(path) = transcript_path(harness.name, &session_id) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        // The envelope lives inside JSONL transcript records, so its quotes
        // arrive escaped; normalize before matching so a raw or a JSON-
        // escaped transcript both resolve.
        let normalized = text.replace("\\\"", "\"");
        if let Some(sender) = sender_from_transcript_text(&normalized, msg_id, Some(&session_id)) {
            return Some(sender);
        }
    }
    None
}

struct CandidateHarness {
    name: &'static str,
    session_id: Option<String>,
}

/// Python `_candidate_stores`: every store this process could own, deduped.
/// A process can carry markers for more than one harness family; enumerate
/// instead of picking, and let the receipt decide.
fn candidate_harnesses() -> Vec<CandidateHarness> {
    let mut out: Vec<CandidateHarness> = Vec::new();
    let mut seen: Vec<(&str, String)> = Vec::new();
    for (_marker, harness, value) in
        crate::spawn_context::present_markers(&|name| std::env::var(name).ok())
    {
        if value.is_empty() {
            continue;
        }
        let lane = match harness.as_str() {
            "claude" => "claude",
            "codex" => "codex",
            _ => continue,
        };
        // Python dedups (harness, value): the same session id under two
        // marker families is legal (a claude session launched under codex),
        // and each family reads its own store.
        let key = (lane, value.clone());
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key);
        out.push(CandidateHarness {
            name: lane,
            session_id: Some(value),
        });
    }
    out
}

/// Python `_transcript_path`: locate the invoking session's transcript
/// (claude: `<projects>/*/<id>.jsonl`; codex: rollout embedding the id).
fn transcript_path(harness: &str, session_id: &str) -> Option<PathBuf> {
    if harness == "claude" {
        let projects = crate::mail_cli::send::resolve::projects_dir();
        let entries = std::fs::read_dir(projects).ok()?;
        for entry in entries.flatten() {
            let candidate = entry.path().join(format!("{session_id}.jsonl"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        return None;
    }
    if harness == "codex" {
        let root = std::env::var("CODEX_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                    .join(".codex")
            });
        return find_codex_rollout(&root, session_id);
    }
    None
}

fn find_codex_rollout(dir: &std::path::Path, session_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_codex_rollout(&path, session_id) {
                return Some(found);
            }
        } else if path
            .file_name()
            .map(|n| n.to_string_lossy().contains(session_id))
            .unwrap_or(false)
            && path.extension().map(|e| e == "jsonl").unwrap_or(false)
        {
            return Some(path);
        }
    }
    None
}

/// Python `reply_resolve.sender_from_transcript_text`: the sender address of
/// the `<fno_mail ... id="...">` envelope in `text`, tier-checked against the
/// owning session. The FULL `from_session` wins (it survives a head-8
/// collision); a forwarded copy carrying a different `to=` no longer counts.
fn sender_from_transcript_text(
    text: &str,
    msg_id: &str,
    session_id: Option<&str>,
) -> Option<String> {
    for turn in crate::mail_header::header_turns(text) {
        if turn.get("id").and_then(Value::as_str) == Some(msg_id) {
            return turn
                .get("sender")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    for tag in crate::mail_header::legacy_tags(text) {
        if tag.get("id").and_then(Value::as_str) != Some(msg_id) {
            continue;
        }
        if let Some(session_id) = session_id {
            if !addressed_here(&tag, session_id) {
                continue;
            }
        }
        // Full provenance first, display handle second.
        return tag
            .get("from_session")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| tag.get("from").and_then(Value::as_str))
            .map(str::to_string);
    }
    None
}

/// Python `reply_resolve._addressed_here`: a present session address is
/// matched by TIER, not by string equality. No usable address at all counts
/// (`to` is optional in the renderer, and a `node:` job address names work
/// rather than a session).
fn addressed_here(tag: &Value, session_id: &str) -> bool {
    let Some(token) = tag
        .get("to")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return true;
    };
    if token.contains(':') {
        return true;
    }
    crate::identity::session_handle_tier(token, session_id).is_some()
}

/// The thread-store reply path (non-name-lane): resolve the sender project
/// here so the name-lane path above is never forced through project
/// identification, then append to the thread that carries the id, or write
/// the orphan self-note.
fn thread_store_reply(
    parsed: &ReplyArgs,
    kind: &str,
    body_text: &str,
    to_msg: &str,
    origin: Option<&str>,
) -> i32 {
    let sender = match crate::mail_cli::send::lanes::resolve_kind_sender(
        parsed.from_project.as_deref(),
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    ) {
        Ok(project) => project,
        Err((msg, code)) => {
            eprintln!("{msg}");
            return code;
        }
    };
    let own_handle = send::find_thread_by_msg_id(&sender, to_msg);
    let recipient = match &own_handle {
        Some(path) => std::fs::read_to_string(path)
            .ok()
            .and_then(|text| send::thread_frontmatter_field(&text, "from"))
            .unwrap_or_default(),
        None => {
            eprintln!(
                "warning: msg-id '{to_msg}' not found in own inbox; \
                 writing a self-note thread (likely orphan reply)"
            );
            sender.clone()
        }
    };
    let mut refs = serde_json::Map::new();
    for (key, value) in [
        ("mission_id", &parsed.ref_mission),
        ("source_mission", &parsed.source_mission),
        ("cascade_of", &parsed.cascade_of),
    ] {
        if let Some(value) = value {
            refs.insert(key.to_string(), json!(value));
        }
    }
    let reply_id = send::generate_msg_id();
    let reservation = send::reserve_send_budget(&sender, &recipient, body_text, &reply_id);
    if let Err((msg, code)) = &reservation {
        eprintln!("{msg}");
        return *code;
    }
    let reservation = reservation.expect("checked above");
    let authored_words = crate::backlog::style_check::word_count(body_text) as i64;

    // The reply's recipient is read off the parent thread's `from:`; re-find
    // under THAT inbox, exactly as the oracle did.
    let existing = send::find_thread_by_msg_id(&recipient, to_msg);
    if let Some(thread_path) = existing {
        match send::append_to_kind_thread(
            &thread_path,
            &sender,
            body_text,
            &reply_id,
            Some(authored_words),
            origin,
        ) {
            Ok(path) => {
                let payload = json!({
                    "msg_id": reply_id,
                    "thread_path": path.display().to_string(),
                    "appended": true,
                });
                if parsed.json_out {
                    println!("{payload}");
                } else {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    println!("appended reply {reply_id} to {name} (in {recipient})");
                }
                return 0;
            }
            Err(exc) => {
                send::release_budget(&reservation);
                eprintln!("error: {exc}");
                return 1;
            }
        }
    }
    match send::write_durable_thread(
        &recipient,
        &sender,
        kind,
        body_text,
        &reply_id,
        None,
        None,
        None,
        None,
        None,
        "",
        Some(authored_words),
        origin,
        None,
        Some(to_msg),
        false,
        Some(Value::Object(refs)),
    ) {
        Ok(path) => {
            let payload = json!({
                "msg_id": reply_id,
                "thread_path": path.display().to_string(),
                "appended": false,
                "orphan": true,
            });
            if parsed.json_out {
                println!("{payload}");
            } else {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                println!(
                    "sent orphan reply {reply_id} to {recipient} ({name}; replies_to:{to_msg})"
                );
            }
            0
        }
        Err(exc) => {
            send::release_budget(&reservation);
            eprintln!("error: {exc}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail_cli::send::resolve::ReachableSession;

    fn vec_args(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// The two-body conflict is an explicit error, not a precedence rule: a
    /// silent winner would relocate the trap rather than remove it.
    #[test]
    fn two_bodies_at_once_is_an_explicit_error() {
        let parsed = parse_args(&vec_args(&[
            "--to", "m-1", "body-one", "--body", "body-two",
        ]))
        .expect("parse");
        assert_eq!(resolve_body(&parsed).unwrap_err(), 1);
    }

    /// A body is required; none of the three forms is also a refusal.
    #[test]
    fn missing_body_is_refused() {
        let parsed = parse_args(&vec_args(&["--to", "m-1"])).expect("parse");
        assert_eq!(resolve_body(&parsed).unwrap_err(), 1);
    }

    /// An unknown kind refuses naming the valid set; a removed kind hints at
    /// its replacement.
    #[test]
    fn kind_validation_refusals() {
        assert_eq!(validate_kind("bogus").unwrap_err(), 1);
        assert_eq!(validate_kind("notification").unwrap_err(), 1);
        assert_eq!(validate_kind("fyi").unwrap(), "fyi");
    }

    /// `--to` is required: the usage block, exit 2.
    #[test]
    fn missing_to_is_a_usage_error() {
        assert_eq!(run(&vec_args(&["hello there"])), 2);
        assert_eq!(run(&[]), 2);
    }

    /// The retired `<harness>-<hex6+>` address form is recognized for the
    /// migration arm; every other shape passes through untouched.
    #[test]
    fn legacy_handle_shape_matches_only_known_harnesses() {
        assert!(is_legacy_handle("claude-deadbeef"));
        assert!(is_legacy_handle("codex-04b5621d"));
        assert!(!is_legacy_handle("worker-1"), "a fleet name is not legacy");
        assert!(!is_legacy_handle("unknownharness-deadbeef"));
        assert!(!is_legacy_handle("claude-abc"), "under six hex digits");
    }

    /// The transcript receipt: an envelope whose `to=` names a DIFFERENT
    /// session no longer counts (a forward quotes the envelope verbatim), an
    /// envelope with no usable address still counts, and the full
    /// `from_session` beats the display handle.
    #[test]
    fn transcript_receipt_tier_checks_the_envelope() {
        let tag = serde_json::json!({
            "id": "m-1", "from": "9a063cd3",
            "from_session": "9a063cd3-69d4-415a-ada5-649b0164189c",
            "to": "b2c3d4e5-1111-2222-3333-444455556666"
        });
        assert!(!addressed_here(
            &tag,
            "a1b2c3d4-1111-2222-3333-444455556666"
        ));
        let unaddressed = serde_json::json!({"id": "m-1", "from": "9a063cd3"});
        assert!(addressed_here(
            &unaddressed,
            "a1b2c3d4-1111-2222-3333-444455556666"
        ));
        let job = serde_json::json!({"id": "m-1", "from": "9a063cd3", "to": "node:fno-cafe"});
        assert!(addressed_here(&job, "a1b2c3d4-1111-2222-3333-444455556666"));
        let text = r#"<fno_mail from="9a063cd3" from_session="9a063cd3-69d4-415a-ada5-649b0164189c" id="m-1">hi</fno_mail>"#;
        assert_eq!(
            sender_from_transcript_text(text, "m-1", None).as_deref(),
            Some("9a063cd3-69d4-415a-ada5-649b0164189c"),
            "full provenance wins"
        );
        let bare = r#"<fno_mail from="9a063cd3" id="m-2">hi</fno_mail>"#;
        assert_eq!(
            sender_from_transcript_text(bare, "m-2", None).as_deref(),
            Some("9a063cd3")
        );
    }

    /// The bus-first lookup: an id absent from the bus and every transcript
    /// store is a hard error that queues nothing. Fixture: a fresh temp home.
    #[test]
    fn unknown_msg_id_is_a_hard_error_sending_nothing() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _state = crate::claims::EnvVarGuard::set("FNO_STATE_DIR", dir.path().to_str().unwrap());
        let _home = crate::claims::EnvVarGuard::set(
            "FNO_AGENTS_HOME",
            dir.path().join("agents-home").to_str().unwrap(),
        );
        let _bus = crate::claims::EnvVarGuard::set(
            "FNO_BUS_DIR",
            dir.path().join("bus").to_str().unwrap(),
        );
        let _claude = crate::claims::EnvVarGuard::set("CLAUDE_CODE_SESSION_ID", "");
        let _codex = crate::claims::EnvVarGuard::set("CODEX_SESSION_ID", "");
        let _thread = crate::claims::EnvVarGuard::set("CODEX_THREAD_ID", "");
        assert_eq!(run(&vec_args(&["--to", "msg-absent", "ack"])), 1);
        assert!(
            !dir.path().join("bus").join("messages.jsonl").exists(),
            "a hard error queues nothing"
        );
    }

    fn reach(
        hit: Option<&str>,
        ambiguous: Vec<String>,
        degraded: Vec<&'static str>,
    ) -> ReachResult {
        ReachResult {
            hit: hit.map(|session_id| ReachableSession {
                session_id: session_id.to_string(),
                source: "registry",
                agent: "claude".to_string(),
                cwd: None,
            }),
            ambiguous,
            degraded,
        }
    }

    /// A discovery miss must not turn a known store ambiguity into durable
    /// mail: nothing is queued, and the refusal names the candidate set.
    #[test]
    fn ambiguous_offline_sender_handle_refuses_without_queuing() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _bus = crate::claims::EnvVarGuard::set(
            "FNO_BUS_DIR",
            dir.path().join("bus").to_str().unwrap(),
        );
        let code = route_ladder(
            &reach(
                None,
                vec![
                    "aaaaaaaa-1111-2222-3333-4444deadbeef".to_string(),
                    "bbbbbbbb-1111-2222-3333-5555deadbeef".to_string(),
                ],
                vec![],
            ),
            "deadbeef",
            false,
            None,
            "ack",
            None,
            "m-1",
            None,
        );
        assert_ne!(code, 0, "an ambiguous handle refuses");
        assert!(
            !dir.path().join("bus").join("messages.jsonl").exists(),
            "an ambiguous handle queues nothing"
        );
    }

    /// A visible candidate whose uniqueness went unproven is exactly the
    /// wake-a-stranger risk: choosing the one row we can see is the guess.
    /// Failing open is safe only when there is no candidate to choose between.
    #[test]
    fn lone_visible_candidate_still_refuses() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _bus = crate::claims::EnvVarGuard::set(
            "FNO_BUS_DIR",
            dir.path().join("bus").to_str().unwrap(),
        );
        let code = route_ladder(
            &reach(
                Some("9a063cd3-69d4-415a-ada5-649b0164189c"),
                vec![],
                vec!["transcript"],
            ),
            "9a063cd3",
            false,
            None,
            "ack",
            None,
            "m-1",
            None,
        );
        assert_ne!(code, 0, "unproven uniqueness refuses");
        assert!(
            !dir.path().join("bus").join("messages.jsonl").exists(),
            "the wake-a-stranger refusal queues nothing"
        );
    }

    /// `--sender-session` outside the candidate set sends nothing: membership
    /// is checked against the real candidate set or not at all.
    #[test]
    fn sender_session_noncandidate_sends_nothing() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _bus = crate::claims::EnvVarGuard::set(
            "FNO_BUS_DIR",
            dir.path().join("bus").to_str().unwrap(),
        );
        let code = route_ladder(
            &reach(
                None,
                vec![
                    "aaaaaaaa-1111-2222-3333-4444deadbeef".to_string(),
                    "bbbbbbbb-1111-2222-3333-5555deadbeef".to_string(),
                ],
                vec![],
            ),
            "deadbeef",
            false,
            Some("cccccccc-1111-2222-3333-6666deadbeef"),
            "ack",
            None,
            "m-1",
            None,
        );
        assert_ne!(code, 0, "a non-candidate --sender-session refuses");
        assert!(
            !dir.path().join("bus").join("messages.jsonl").exists(),
            "nothing was sent"
        );
    }

    /// The retired handle kept refusing after its discovery ladders died:
    /// there proven uniqueness is the only thing vouching for the address.
    #[test]
    fn retired_sender_handle_refuses_offline() {
        let code = route_ladder(
            &reach(None, vec![], vec![]),
            "deadbeef",
            true,
            None,
            "ack",
            None,
            "m-1",
            None,
        );
        assert_ne!(code, 0);
    }

    /// An unreadable candidate store is skipped, never fatal: the next
    /// candidate holding the receipt resolves. A directory where the
    /// transcript file belongs makes the read fail deterministically.
    #[test]
    fn unreadable_store_is_skipped_for_the_next_candidate() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // The claude candidate's transcript path is a directory: the read
        // fails, the scan moves on.
        let projects = dir.path().join(".claude").join("projects").join("-enc");
        std::fs::create_dir_all(projects.join("11111111-2222-3333-4444-555566667777.jsonl"))
            .unwrap();
        let _home = crate::claims::EnvVarGuard::set("HOME", dir.path().to_str().unwrap());
        let _config_dir = crate::claims::EnvVarGuard::set("CLAUDE_CONFIG_DIR", "");
        let _claude = crate::claims::EnvVarGuard::set(
            "CLAUDE_CODE_SESSION_ID",
            "11111111-2222-3333-4444-555566667777",
        );
        let _codex_home = crate::claims::EnvVarGuard::set(
            "CODEX_HOME",
            dir.path().join("codex").to_str().unwrap(),
        );
        let sid = "11111111-2222-3333-4444-555566667777";
        let rollout = dir
            .path()
            .join("codex")
            .join("sessions")
            .join("2026")
            .join("10")
            .join(format!("rollout-{sid}.jsonl"));
        std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        std::fs::write(
            &rollout,
            r#"{"type":"user","message":{"content":"<fno_mail from=\"9a063cd3\" id=\"m-live1\">ping</fno_mail>"}}"#,
        )
        .unwrap();
        // Only the claude candidate exists and its read fails: the resolver
        // misses and the caller falls to its hard error.
        let _codex = crate::claims::EnvVarGuard::set("CODEX_SESSION_ID", "");
        assert_eq!(resolve_live_sender("m-live1"), None);
        // With the codex marker set, the skip moves past the torn claude
        // store and the codex rollout resolves the sender.
        let _codex2 = crate::claims::EnvVarGuard::set(
            "CODEX_SESSION_ID",
            "11111111-2222-3333-4444-555566667777",
        );
        assert_eq!(resolve_live_sender("m-live1").as_deref(), Some("9a063cd3"));
    }
}

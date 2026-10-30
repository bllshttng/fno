//! The `rename` command's client half: argv-to-request shaping and the success
//! receipt. The daemon owns the transaction, except `--from-journal`, the
//! daemon-free label rebuild that reads the registry and journal directly.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::event_store::EventRow;
use crate::state::RegistryEntry;

/// One row's planned label rebuild.
pub struct Rebuild {
    /// The label the registry holds today.
    pub name: String,
    /// The full harness session id the rename is keyed by (d-e952ed19).
    pub session: String,
    /// The newest label the journal names for that session.
    pub journal_name: String,
    /// Set when the rebuild is refused, so `--apply` renames the rest.
    pub reason: Option<String>,
}

/// Pure: the label rebuild each row needs. Events are keyed by
/// `data.harness_session_id`, newest by `ts_ms` then `seq`; `agent_spawned`
/// contributes `data.name` and `agent_renamed` contributes `data.to`. A row
/// already named as its journal plans nothing; a journal name another row
/// holds plans a skip with the reason.
pub fn plan_from_journal(rows: &[RegistryEntry], events: &[EventRow]) -> Vec<Rebuild> {
    let mut newest: BTreeMap<String, (String, i64, i64)> = BTreeMap::new();
    for row in events {
        let Ok(value) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let Some(data) = value.get("data") else {
            continue;
        };
        let Some(session) = data
            .get("harness_session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let label = match row.r#type.as_str() {
            "agent_spawned" => data.get("name").and_then(Value::as_str),
            "agent_renamed" => data.get("to").and_then(Value::as_str),
            _ => continue,
        };
        let Some(label) = label.filter(|l| !l.is_empty()) else {
            continue;
        };
        let newer = newest
            .get(session)
            .map_or(true, |(_, ts, seq)| (*ts, *seq) < (row.ts_ms, row.seq));
        if newer {
            newest.insert(session.to_string(), (label.to_string(), row.ts_ms, row.seq));
        }
    }
    let mut plans = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let Some(session) = row.harness_session_id.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some((label, _, _)) = newest.get(session) else {
            continue;
        };
        let label = label.as_str();
        if label == row.name {
            continue;
        }
        let taken = rows
            .iter()
            .enumerate()
            .any(|(j, other)| j != i && other.name == label);
        plans.push(Rebuild {
            name: row.name.clone(),
            session: session.to_string(),
            journal_name: label.to_string(),
            reason: taken.then(|| format!("registry label {label:?} already names another row")),
        });
    }
    plans
}

/// `rename --from-journal`: print one `<current> -> <journal name>` line per
/// row (a refused row prints its skip reason) and write nothing without
/// `--apply`. With `--apply` each planned row renames through
/// `state::rename_agent`, keyed by the full session id. Exit 0, or 1 when
/// any row was refused.
pub fn run_from_journal(args: &[String], home: &crate::paths::AgentsHome) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let registry_path = home.registry_json();
    let registry = match crate::state::load_registry(&registry_path) {
        Ok(registry) => registry,
        Err(e) => {
            eprintln!(
                "rename --from-journal: cannot read {}: {e}",
                registry_path.display()
            );
            return 2;
        }
    };
    let _ = crate::event_store::import_all(&home.events_jsonl());
    let query = crate::event_store::EventQuery {
        types: vec!["agent_spawned".into(), "agent_renamed".into()],
        ..Default::default()
    };
    let events = match crate::event_store::query_events(&home.events_jsonl(), &query) {
        Ok(events) => events,
        Err(e) => {
            eprintln!("rename --from-journal: cannot read the agents journal: {e}");
            return 2;
        }
    };
    let plans = plan_from_journal(&registry.entries, &events);
    let mut refused = 0;
    for plan in &plans {
        match &plan.reason {
            Some(reason) => {
                println!("skip {} -> {}: {reason}", plan.name, plan.journal_name);
                refused += 1;
            }
            None => println!("{} -> {}", plan.name, plan.journal_name),
        }
    }
    if plans.is_empty() {
        println!("rename --from-journal: every label already matches the journal");
    }
    if !apply {
        return i32::from(refused > 0);
    }
    for plan in &plans {
        if plan.reason.is_some() {
            continue;
        }
        if let Err(e) =
            crate::state::rename_agent(&registry_path, &plan.session, &plan.journal_name, None)
        {
            eprintln!("rename {} -> {} failed: {e}", plan.name, plan.journal_name);
            refused += 1;
        }
    }
    i32::from(refused > 0)
}

/// Shape `rename <token> --name <new-label>`: swap the `--name` value (which
/// lands in `params.name`) with the positional token, carrying the label as
/// `new_name`.
pub fn request(params: &mut Map<String, Value>, positional: &[String]) -> Result<(), String> {
    let new_name = params
        .remove("name")
        .ok_or("rename needs --name <new-label>")?;
    let token = positional
        .first()
        .ok_or("rename needs a <name> to rename")?;
    params.insert("new_name".into(), new_name);
    params.insert("name".into(), Value::String(token.clone()));
    Ok(())
}

/// Rename a registry label and, when the session holds a named live team,
/// its team record in the same RPC.
pub(crate) fn respond(
    home: &crate::paths::AgentsHome,
    req: &crate::protocol::Request,
) -> crate::protocol::Response {
    use crate::protocol::{ErrorCode, Response};
    let token = match req.params.get("name").and_then(Value::as_str) {
        Some(token) if !token.is_empty() => token,
        _ => {
            return Response::err(
                req.id,
                ErrorCode::InvalidParams,
                "rename needs a <name> (current label, short id, or full session id)",
            )
        }
    };
    let Some(new_name) = req.params.get("new_name").and_then(Value::as_str) else {
        return Response::err(
            req.id,
            ErrorCode::InvalidParams,
            "rename needs --name <new-label>",
        );
    };
    if !crate::state::is_valid_registry_label(new_name) {
        return Response::err(
            req.id,
            ErrorCode::InvalidParams,
            "registry name must be 1-64 letters, numbers, underscores, hyphens, or apostrophes",
        );
    }
    let registry_path = home.registry_json();
    let registry = match crate::state::load_registry(&registry_path) {
        Ok(registry) => registry,
        Err(error) => return Response::err(req.id, ErrorCode::Internal, error.to_string()),
    };
    let source = match crate::state::resolve_rename_source(&registry.entries, token) {
        Ok(source) => source,
        Err(error) => return Response::err(req.id, ErrorCode::Internal, error),
    };
    let old_label = source.name.clone();
    let session = source.harness_session_id.clone();
    let team = match session.as_deref() {
        Some(session) => match crate::team_names::rename_team(
            &home.team_names_json(),
            &registry_path,
            session,
            new_name,
            false,
        ) {
            Ok(team) => team,
            Err(error) => {
                return Response::err(req.id, ErrorCode::InvalidParams, error);
            }
        },
        None => None,
    };
    let label = if team.is_some() {
        new_name.to_ascii_lowercase()
    } else {
        new_name.to_string()
    };
    let (old, new) = match crate::state::rename_agent(&registry_path, token, &label, None) {
        Ok(result) => result,
        Err(error) => return Response::err(req.id, ErrorCode::Internal, error),
    };
    let team = match (team, session.as_deref()) {
        (Some(_), Some(session)) => {
            let result = crate::team_names::rename_team(
                &home.team_names_json(),
                &registry_path,
                session,
                new_name,
                true,
            )
            .and_then(|team| {
                team.ok_or_else(|| "the named live team changed before rename".to_string())
            });
            match result {
                Ok(team) => Some(team),
                Err(team_error) => {
                    let rollback =
                        crate::state::rename_agent(&registry_path, session, &old_label, None);
                    let rollback_result = match rollback {
                        Ok(_) => "registry label rolled back".to_string(),
                        Err(error) => format!("registry rollback failed: {error}"),
                    };
                    return Response::err(
                        req.id,
                        ErrorCode::Internal,
                        format!("team rename failed: {team_error}; {rollback_result}"),
                    );
                }
            }
        }
        _ => None,
    };
    Response::ok(
        req.id,
        serde_json::json!({
            "renamed": true,
            "old_name": old,
            "new_name": new,
            "role": team.map(|(from, to)| serde_json::json!({"from": from, "to": to})),
        }),
    )
}

/// The receipt names BOTH labels: the new one is the live address, the old one
/// the alias a later `peek`/`mail send` may still use.
pub fn receipt(name: &str, result: &Value) -> Option<String> {
    let old = result
        .get("old_name")
        .and_then(Value::as_str)
        .unwrap_or(name);
    let new = result
        .get("new_name")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    let mut receipt = format!("renamed {old} -> {new}");
    if let Some(team) = result.get("role") {
        if let (Some(from), Some(to)) = (
            team.get("from").and_then(Value::as_str),
            team.get("to").and_then(Value::as_str),
        ) {
            receipt.push_str(&format!("; team {from} -> {to}"));
        }
    }
    if let Some(push) = result.get("title_push").and_then(Value::as_str) {
        receipt.push_str("; ");
        receipt.push_str(push);
    }
    Some(receipt)
}

/// The thread-session harnesses `mail-inject` delivers to. A title push
/// attempts only these: another harness has no delivery lane, and the
/// attempt would spend a subprocess to answer not-injectable. The keeper
/// and pane lanes host panes, not thread addresses, so they are not here.
const TITLE_PUSH_HARNESSES: &[&str] = &["claude", "codex", "opencode"];

/// The statuses a quiet-capable row holds. A title push types at the
/// prompt line, so a mid-turn row (Busy) is excluded: its quiet gate can
/// hold the delivery for its full wait, a budget the mux rename gesture
/// does not have. Mail keeps the wider drive-eligible set; a title is not
/// urgent enough to wait out a turn.
const TITLE_PUSH_STATUSES: &[crate::AgentStatus] = &[
    crate::AgentStatus::Ready,
    crate::AgentStatus::Idle,
    crate::AgentStatus::Live,
];

/// The harness-title push a completed rename owes the session: the native
/// rename command when the row's harness declares one, the session is
/// quiet-capable, and its reported title still differs. `None` names no
/// push: no harness session, no delivery lane, no native rename command, the title
/// already matches, or the row is not quiet-capable.
pub(crate) fn title_push(row: &RegistryEntry, new_name: &str) -> Option<String> {
    row.harness_session_id
        .as_deref()
        .filter(|session| !session.is_empty())?;
    let harness = row
        .harness
        .as_deref()
        .filter(|harness| !harness.is_empty())?;
    if !TITLE_PUSH_HARNESSES.contains(&harness) || !TITLE_PUSH_STATUSES.contains(&row.status) {
        return None;
    }
    crate::lead_checkin::title_rename_command(harness, new_name, row.harness_title.as_deref())
}

/// Deliver a command to one live session: type it at the session's prompt
/// through the one-shot live-delivery command, the same vehicle the mail
/// raw lane drives. `self_send` marks the audit event when the recipient
/// is the calling session's own row. The caller's transaction is already
/// durable when this runs, so a failed delivery is a reported note, never
/// a rollback.
pub(crate) fn deliver_command_to_session(
    session: &str,
    harness: &str,
    command: &str,
    self_send: bool,
) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut args = vec![
        "mail-inject".to_string(),
        "--session".to_string(),
        session.to_string(),
        "--harness".to_string(),
        harness.to_string(),
    ];
    if self_send {
        args.push("--self-send".to_string());
    }
    let mut child = Command::new(executable)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("mail-inject start failed: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("mail-inject stdin unavailable")?;
    stdin
        .write_all(command.as_bytes())
        .map_err(|error| format!("mail-inject write failed: {error}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("mail-inject wait failed: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("mail-inject exited {}", output.status)
    } else {
        detail
    })
}

/// After a successful `agent.rename` RPC: when the renamed row's harness
/// session can hear a native rename, deliver it and stamp the receipt
/// field that says what happened. Best-effort by contract: the registry
/// rename is already durable, and a silent skip (no eligible session) is
/// the absent field, not a note.
pub fn stamp_title_push(home: &crate::paths::AgentsHome, result: &mut Value) {
    let Some(new_name) = result.get("new_name").and_then(Value::as_str) else {
        return;
    };
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    let Some(row) = registry.entries.iter().find(|entry| entry.name == new_name) else {
        return;
    };
    let Some(command) = title_push(row, new_name) else {
        return;
    };
    let session = row.harness_session_id.as_deref().unwrap_or_default();
    let harness = row.harness.as_deref().unwrap_or_default();
    // One quiet read, no wait: the delivery's own gate would hold up to its
    // full budget on a held composer (a draft, a question picker, Stop
    // hooks), a wait the mux rename gesture bounds at 20s. Not quiet now
    // means skip; the title is cosmetic and the rename is already durable.
    if crate::mail_inject::quiet_blocker(
        &home.events_jsonl(),
        &home.registry_json(),
        &crate::claude_drive::claude_projects_dir(),
        session,
        crate::mail_inject::now_ms(),
    )
    .is_some()
    {
        result["title_push"] = Value::String("title push skipped: session not quiet".to_string());
        return;
    }
    // title_push proved both fields present and non-empty, so the defaults
    // below are unreachable spellings of a proved fact, not guesses.
    let outcome = deliver_command_to_session(session, harness, &command, false);
    result["title_push"] = Value::String(match outcome {
        Ok(()) => "title pushed".to_string(),
        Err(error) => format!("title push failed: {error}"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::AgentsHome;
    use std::path::PathBuf;

    fn row(name: &str, session: &str) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            harness: Some("claude".into()),
            harness_session_id: Some(session.into()),
            ..Default::default()
        }
    }

    fn renamed(session: &str, to: &str, ts_ms: i64, seq: i64) -> EventRow {
        EventRow {
            seq,
            history_only: false,
            recovery_batch: None,
            event_id: format!("e{seq}"),
            ts_ms,
            r#type: "agent_renamed".to_string(),
            source: "daemon".into(),
            scope: None,
            retention_class: "durable".into(),
            reject_reason: None,
            line: serde_json::json!({
                "ts": "2026-09-27T00:00:00Z",
                "type": "agent_renamed",
                "source": "daemon",
                "data": {"to": to, "harness_session_id": session},
            })
            .to_string(),
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-rename-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn title_push_plans_the_native_rename_for_a_reachable_row() {
        let mut r = row("wren", "sess-1");
        r.status = crate::AgentStatus::Live;
        r.harness_title = Some("quill".into());
        assert_eq!(title_push(&r, "wren").as_deref(), Some("/rename wren"));
    }

    #[test]
    fn title_push_skips_when_the_harness_title_already_matches() {
        let mut r = row("wren", "sess-1");
        r.status = crate::AgentStatus::Live;
        r.harness_title = Some("wren".into());
        assert_eq!(title_push(&r, "wren"), None);
    }

    #[test]
    fn title_push_skips_a_harness_without_the_native_rename_command() {
        let mut r = row("kestrel", "sess-1");
        r.harness = Some("codex".into());
        r.status = crate::AgentStatus::Live;
        r.harness_title = Some("kestrel".into());
        assert_eq!(title_push(&r, "hawk"), None);
    }

    #[test]
    fn title_push_skips_a_harness_with_no_delivery_lane() {
        // agy declares /rename but mail-inject delivers no agy thread
        // session, so the push would spend a subprocess to answer
        // not-injectable. Skipped at the plan, not at delivery.
        let mut r = row("wren", "sess-1");
        r.harness = Some("agy".into());
        r.status = crate::AgentStatus::Live;
        r.harness_title = Some("quill".into());
        assert_eq!(title_push(&r, "wren"), None);
    }

    #[test]
    fn title_push_skips_a_row_that_cannot_receive_a_turn() {
        let mut r = row("wren", "sess-1");
        r.harness_title = Some("quill".into());
        r.status = crate::AgentStatus::Exited;
        assert_eq!(title_push(&r, "wren"), None);
    }

    #[test]
    fn title_push_skips_a_mid_turn_row_whose_quiet_gate_would_hold_the_delivery() {
        let mut r = row("wren", "sess-1");
        r.harness_title = Some("quill".into());
        r.status = crate::AgentStatus::Busy;
        assert_eq!(title_push(&r, "wren"), None);
    }

    #[test]
    fn title_push_skips_a_row_without_a_harness_session() {
        let mut r = row("wren", "");
        r.status = crate::AgentStatus::Live;
        assert_eq!(title_push(&r, "wren"), None);
    }

    #[test]
    fn receipt_names_the_title_push() {
        let result = serde_json::json!({
            "renamed": true, "old_name": "quill", "new_name": "wren",
            "title_push": "title pushed",
        });
        let line = receipt("quill", &result).unwrap();
        assert!(line.contains("renamed quill -> wren"), "{line}");
        assert!(line.contains("title pushed"), "{line}");
    }

    #[test]
    fn quiet_blocker_holds_on_a_fresh_draft_and_reads_none_when_clear() {
        // The rename door's skip gate is the delivery's own single read:
        // a fresh typing row (a draft) holds, and a journal with no fresh
        // typing row reads clear. Row shape mirrors the daemon's
        // operator_typing emit.
        let dir = std::env::temp_dir().join(format!(
            "fno-rename-quiet-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let journal = dir.join("events.jsonl");
        let base = crate::mail_inject::now_ms();
        let row = serde_json::json!({
            "ts": "2026-09-27T18:00:00Z",
            "type": "operator_typing",
            "source": "daemon",
            "data": {
                "mux_session": "main", "pane": 7, "via": "pane",
                "typed_ms": base - 1_000,
                "resolution": "ok",
                "harness_session": "sess-1",
            }
        });
        std::fs::write(&journal, format!("{row}\n")).unwrap();
        // A later submit row ages the typing row out of draft: the composer
        // was submitted, so the same journal reads clear once the typing row
        // leaves the recent window.
        let submit = serde_json::json!({
            "ts": "2026-09-27T18:00:00Z",
            "type": "operator_submit",
            "source": "daemon",
            "data": {
                "mux_session": "main", "pane": 7, "via": "pane",
                "submit_ms": base,
                "resolution": "ok",
                "harness_session": "sess-1",
            }
        });
        std::fs::write(&journal, format!("{row}\n{submit}\n")).unwrap();
        let registry = std::env::temp_dir().join("fno-rename-quiet-no-registry");
        let held =
            crate::mail_inject::quiet_blocker(&journal, &registry, &registry, "sess-1", base);
        assert_eq!(held.as_deref(), Some("user-typing"));
        let clear = crate::mail_inject::quiet_blocker(
            &journal,
            &registry,
            &registry,
            "sess-1",
            base + 120_000,
        );
        assert_eq!(clear, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_takes_the_newest_journal_label() {
        let rows = vec![row("05dddd48", "sess-1")];
        let events = vec![
            renamed("sess-1", "first", 100, 2),
            renamed("sess-1", "vellum", 100, 3),
            renamed("sess-1", "older", 50, 9),
        ];
        let plans = plan_from_journal(&rows, &events);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].journal_name, "vellum", "newest ts then seq wins");
    }

    #[test]
    fn run_from_journal_apply_skips_held_name_and_exits_1() {
        let dir = tmpdir("apply-held");
        let home = AgentsHome::at(&dir);
        let path = home.registry_json();
        crate::state::update_registry(&path, |registry| {
            registry.entries = vec![
                row("05dddd48", "sess-1"),
                row("99887766", "aaaaaaaa-0000-0000-0000-222222222222"),
                row("vellum", "sess-3"),
            ];
        })
        .unwrap();
        let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
        emitter
            .emit(
                "agent_spawned",
                &serde_json::json!({"name": "vellum", "harness_session_id": "sess-1"}),
            )
            .unwrap();
        emitter
            .emit(
                "agent_spawned",
                &serde_json::json!({
                    "name": "ghost",
                    "harness_session_id": "aaaaaaaa-0000-0000-0000-222222222222"
                }),
            )
            .unwrap();

        let code = run_from_journal(
            &[
                "rename".to_string(),
                "--from-journal".into(),
                "--apply".into(),
            ],
            &home,
        );

        // sess-1's journal name is held by the sess-3 row: skipped with a
        // named reason, the sess-2 row still renames, exit 1.
        assert_eq!(code, 1);
        let registry = crate::state::load_registry(&path).unwrap();
        let by_name = |n: &str| registry.entries.iter().find(|e| e.name == n);
        assert!(
            by_name("05dddd48").is_some(),
            "the skipped row is untouched"
        );
        assert!(by_name("ghost").is_some(), "the other row still renames");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_teamed_codex_rename_shows_one_name_everywhere() {
        use crate::protocol::{Request, ResponsePayload};

        let dir = tmpdir("teamed-rename");
        let home = AgentsHome::at(&dir);
        let session = "01a0ee3f-235d-7671-8fbb-e09af1d5fb52";
        let other_session = "01a0ee3f-235d-7671-8fbb-e09af1d5fb53";
        crate::state::update_registry(&home.registry_json(), |registry| {
            let mut teamed = row("kestrel", session);
            teamed.harness = Some("codex".into());
            teamed.status = crate::AgentStatus::Ready;
            teamed.role_scope = Some("x-test".into());
            teamed.role_level = Some(2);
            let mut other = row("raven", other_session);
            other.harness = Some("codex".into());
            other.status = crate::AgentStatus::Ready;
            other.role_scope = Some("y-test".into());
            other.role_level = Some(2);
            registry.entries = vec![teamed, other];
        })
        .unwrap();
        std::fs::write(
            home.team_names_json(),
            serde_json::json!({
                "version": 1,
                "teams": {
                    "x-test": {
                        "name": "Kestrel",
                        "generation": 2,
                        "holder_session": session,
                        "nodes": [],
                        "updated_at": "2026-09-29T00:00:00Z"
                    },
                    "y-test": {
                        "name": "Bob",
                        "generation": 1,
                        "holder_session": other_session,
                        "nodes": [],
                        "updated_at": "2026-09-29T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let before_registry = crate::registry_store::read(&home.registry_json()).unwrap();
        let before_teams = std::fs::read(home.team_names_json()).unwrap();
        let duplicate = respond(
            &home,
            &Request::new(
                1,
                "agent.rename",
                serde_json::json!({"name": "kestrel", "new_name": "bob"}),
            ),
        );
        let ResponsePayload::Err(error) = duplicate.payload else {
            panic!("duplicate team name was accepted");
        };
        assert!(error
            .message
            .contains("the name Bob is held by raven over y-test"));
        assert_eq!(
            crate::registry_store::read(&home.registry_json()).unwrap(),
            before_registry
        );
        assert_eq!(std::fs::read(home.team_names_json()).unwrap(), before_teams);
        crate::state::update_registry(&home.registry_json(), |registry| {
            registry.entries.retain(|entry| entry.name != "raven");
        })
        .unwrap();
        std::fs::write(
            home.team_names_json(),
            serde_json::json!({
                "version": 1,
                "teams": {
                    "x-test": {
                        "name": "Kestrel",
                        "generation": 2,
                        "holder_session": session,
                        "nodes": [],
                        "updated_at": "2026-09-29T00:00:00Z"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let response = respond(
            &home,
            &Request::new(
                1,
                "agent.rename",
                serde_json::json!({"name": "kestrel", "new_name": "bob"}),
            ),
        );
        let ResponsePayload::Ok(result) = response.payload else {
            panic!("rename failed: {:?}", response.payload);
        };
        assert_eq!(
            receipt("kestrel", &result).as_deref(),
            Some("renamed kestrel -> bob; team Kestrel -> Bob")
        );

        let registry = crate::state::load_registry(&home.registry_json()).unwrap();
        let renamed = registry
            .entries
            .iter()
            .find(|entry| entry.name == "bob")
            .unwrap();
        assert!(renamed.aliases.iter().any(|alias| alias == "kestrel"));
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.team_names_json()).unwrap()).unwrap();
        assert_eq!(record["teams"]["x-test"]["name"], "bob");
        assert_eq!(record["teams"]["x-test"]["generation"], 1);
        assert_eq!(record["teams"]["x-test"]["holder_session"], session);
        assert!(crate::team_names::ensure_named_team(
            &home.team_names_json(),
            &home.registry_json(),
            "x-test"
        )
        .unwrap());
        let envelope = crate::mail_envelope::render_at(
            &serde_json::json!({
                "mode": "tag",
                "from_session": session,
                "id": "fmail-0badc0de1234"
            }),
            &home.registry_json(),
        )
        .unwrap();
        assert!(
            envelope.starts_with("`@bob · fmail-0badc0de1234 ·"),
            "{envelope}"
        );
        assert_eq!(
            crate::lead_checkin::title_rename_command("codex", "bob", None),
            None
        );
        assert_eq!(
            crate::lead_checkin::title_rename_command("claude", "bob", None).as_deref(),
            Some("/rename bob")
        );

        let before_registry = crate::registry_store::read(&home.registry_json()).unwrap();
        let before_teams = std::fs::read(home.team_names_json()).unwrap();
        let refused = respond(
            &home,
            &Request::new(
                2,
                "agent.rename",
                serde_json::json!({"name": "bob", "new_name": "bob_2"}),
            ),
        );
        assert!(matches!(refused.payload, ResponsePayload::Err(_)));
        assert_eq!(
            crate::registry_store::read(&home.registry_json()).unwrap(),
            before_registry
        );
        assert_eq!(std::fs::read(home.team_names_json()).unwrap(), before_teams);
        std::fs::remove_dir_all(&dir).ok();
    }
}

//! The roster adopt: registry rows for `claude --bg` sessions the user
//! launched outside fno. The sweep already reads the harness listing every
//! beat, so a listing row no registry row holds is born there - within one
//! beat of `claude --bg`. Report minting (`native_adopt`) never sees these
//! sessions: a hand launch produces no launch record and no `--agent-self`.
//! Transcript IO runs outside the write lock; the write closure re-decides
//! absence via `session_report::find_row`, so a racing report and this
//! adopt mint one row, never two.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::claude_roster::ClaudeAgentsSnapshot;
use crate::events::EventEmitter;
use crate::state::{Lineage, RegistryEntry};
use crate::AgentStatus;

/// Per-beat cap on mint candidates and backfill candidates: a pathological
/// first beat (a claude restart surfacing every historical worker) must not
/// hold the registry write lock minting them all. The rest wait for later
/// beats.
const BUDGET_PER_BEAT: usize = 8;

/// The substrate word for a session claude's own daemon hosts: never
/// `thread`/`pane` (fno-managed lanes), so the convert/portal machinery does
/// not adopt these rows. Documented on `RegistryEntry::substrate`.
const SUBSTRATE: &str = "native-bg";

/// The `start_source` basis for a roster-minted row.
const START_SOURCE: &str = "claude-agents-listing";

/// The lineage reason every roster birth carries: the site names why it
/// cannot name a parent.
const LINEAGE_REASON: &str =
    "adopted from the claude agents listing: no launch record named this session";

pub(crate) fn mint_missing_native_bg(
    home: &crate::paths::AgentsHome,
    listing: &ClaudeAgentsSnapshot,
    emitter: &EventEmitter,
) -> usize {
    let rows = match listing {
        ClaudeAgentsSnapshot::Known { rows, .. } => rows,
        ClaudeAgentsSnapshot::Unknown { .. } => return 0,
    };
    if rows.is_empty() {
        return 0;
    }
    // A broken registry is the sweep's own failure (it already reports it);
    // this pass degrades to a no-op rather than piling a second error on.
    let Ok(registry) = crate::daemon::load_registry_asserted(&home.registry_json()) else {
        return 0;
    };
    let entries = registry.entries;

    // Pre-lock work: transcript facts for absent listing rows, and node fills
    // for rows born before their transcript existed. The write closure
    // re-decides both under the lock.
    let mut prepared = Vec::new();
    let mut deferred = false;
    for row in rows.iter().filter(|r| {
        !r.state
            .as_deref()
            .is_some_and(crate::claude_roster::is_terminal_roster_state)
    }) {
        if prepared.len() >= BUDGET_PER_BEAT {
            deferred = true;
            break;
        }
        let session: &str = row
            .session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&row.short_id);
        if crate::session_report::find_row(&entries, "claude", session, None).is_some() {
            continue;
        }
        let transcript = crate::claude_drive::find_transcript(session);
        let (node, model) = transcript_facts(transcript.as_deref());
        prepared.push(prepare_row(row, session, transcript, node, model));
    }

    let mut backfills: Vec<(String, String)> = Vec::new();
    for e in entries.iter().filter(|e| {
        e.harness_name() == "claude"
            && e.substrate.as_deref() == Some(SUBSTRATE)
            && e.node.is_none()
    }) {
        if backfills.len() >= BUDGET_PER_BEAT {
            deferred = true;
            break;
        }
        let session = e
            .harness_session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&e.short_id);
        if let Some(node) =
            transcript_facts(crate::claude_drive::find_transcript(session).as_deref()).0
        {
            backfills.push((session.to_string(), node));
        }
    }

    let mut born: Vec<Value> = Vec::new();
    let mut filled: Vec<Value> = Vec::new();
    let write = state::update_registry(&home.registry_json(), |r| {
        for cand in prepared {
            let session = cand
                .harness_session_id
                .clone()
                .or_else(|| cand.claude_session_uuid.clone())
                .unwrap_or_else(|| cand.short_id.clone());
            if crate::session_report::find_row(&r.entries, "claude", &session, None).is_some() {
                continue;
            }
            let mut entry = cand;
            entry.name = crate::native_adopt::unique_name(&r.entries, &entry.name);
            born.push(json!({
                "name": entry.name,
                "harness_session_id": entry.harness_session_id,
                "cwd": entry.cwd,
                "node": entry.node,
                "pid": entry.pid,
                "model": entry.model,
            }));
            r.entries.push(entry);
        }
        for (session, node) in backfills {
            let Some(e) = r.entries.iter_mut().find(|e| {
                e.harness_name() == "claude"
                    && (e.harness_session_id.as_deref() == Some(session.as_str())
                        || e.claude_session_uuid.as_deref() == Some(session.as_str())
                        || session == e.short_id)
            }) else {
                continue;
            };
            if e.node.is_none() {
                e.node = Some(node.clone());
                filled.push(json!({"name": e.name, "node": node}));
            }
        }
    });
    if write.is_err() {
        return 0;
    }
    for b in &born {
        // The birth key set mirrors the report mint's native birth (AC9): a
        // spawned birth's keys are all present, with the listing's values
        // where it observed them and null where it did not.
        let extras = json!({
            "spawn_id": Value::Null,
            "harness_session_id": b["harness_session_id"],
            "cwd": b["cwd"],
            "substrate": SUBSTRATE,
            "pid": b["pid"],
            "launch": "adopted",
        });
        let _ = emitter.emit(
            "agent_spawned",
            &crate::spawn_edge::birth_event(
                b["name"].as_str().unwrap_or_default(),
                &Lineage::unproven(LINEAGE_REASON),
                extras,
            ),
        );
    }
    for f in &filled {
        let _ = emitter.emit("native_bg_node_backfilled", f);
    }
    if deferred {
        let _ = emitter.emit(
            "native_bg_mint_deferred",
            &json!({"budget": BUDGET_PER_BEAT}),
        );
    }
    born.len()
}

/// One transcript walk, two birth facts: the first user entry's node id and
/// the running model (the last real `message.model`). Both are
/// absence-honest: a missing or unreadable transcript yields `(None, None)`
/// and the row still mints.
fn transcript_facts(path: Option<&Path>) -> (Option<String>, Option<String>) {
    let Some(path) = path else {
        return (None, None);
    };
    let Ok(file) = std::fs::File::open(path) else {
        return (None, None);
    };
    let mut node: Option<String> = None;
    let mut model: Option<String> = None;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if node.is_none() && v.get("type").and_then(Value::as_str) == Some("user") {
            node = user_text(&v).and_then(|text| {
                text.split_whitespace()
                    .find(|t| crate::backlog::node_ref::is_wellformed_node_id(t))
                    .map(str::to_string)
            });
        }
        if let Some(m) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
        {
            // A synthetic tail turn is not the running model; the last real
            // one is (the transcript_model precedence).
            if !m.is_empty() && m != crate::resume_pin::SYNTHETIC_MODEL {
                model = Some(m.to_string());
            }
        }
    }
    (node, model)
}

/// The user entry's prompt text: a string content, or the first text block
/// of a block-array content.
fn user_text(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => blocks.iter().find_map(|b| {
            match (
                b.get("type").and_then(Value::as_str),
                b.get("text").and_then(Value::as_str),
            ) {
                (Some("text"), Some(text)) => Some(text.to_string()),
                _ => None,
            }
        }),
        _ => None,
    }
}

/// The row a listing observation mints. The listing is the evidence: name
/// from `--name`, pid and cwd from the listing, node and model from the
/// transcript. Every field carries its basis; nothing is guessed.
fn prepare_row(
    row: &crate::claude_roster::ClaudeAgentRow,
    session: &str,
    transcript: Option<PathBuf>,
    node: Option<String>,
    model: Option<String>,
) -> RegistryEntry {
    let model_basis = model.as_ref().map(|_| "verified".to_string());
    let cwd = row.cwd.clone().unwrap_or_default();
    RegistryEntry {
        name: row
            .name
            .clone()
            .unwrap_or_else(|| crate::claude_adopt::synthesized_entry_name(session, &row.short_id)),
        origin: Some("adopted".into()),
        status: AgentStatus::Live,
        short_id: row.short_id.clone(),
        harness: Some("claude".into()),
        substrate: Some(SUBSTRATE.into()),
        node,
        transcript_path: transcript,
        start_source: Some(START_SOURCE.into()),
        cwd: cwd.clone(),
        project_root: cwd,
        claude_session_uuid: row.session_id.clone().filter(|s| !s.is_empty()),
        model,
        model_basis,
        ..RegistryEntry::new(Some(session.to_string()), Lineage::unproven(LINEAGE_REASON))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_roster::ClaudeAgentRow;

    fn sandbox<R>(f: impl FnOnce(&crate::paths::AgentsHome) -> R) -> R {
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let saved_home = std::env::var_os("HOME");
        let saved_agents = std::env::var_os("FNO_AGENTS_HOME");
        std::env::set_var("HOME", tmp.path());
        let agents = tmp.path().join("agents-home");
        std::fs::create_dir_all(&agents).unwrap();
        std::env::set_var("FNO_AGENTS_HOME", &agents);
        let home = crate::paths::AgentsHome::from_env();
        let out = f(&home);
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_agents {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
        out
    }

    fn listing_row(name: &str, state: &str) -> ClaudeAgentRow {
        let mut row = ClaudeAgentRow::new("492ccda4", Some(state));
        row.session_id = Some("492ccda4-63ff-4736-a02a-8818f63c8b46".into());
        row.name = Some(name.into());
        row.cwd = Some("/tmp/proj".into());
        row.pid = Some(1895);
        row
    }

    fn emitter(home: &crate::paths::AgentsHome) -> EventEmitter {
        EventEmitter::new(home.events_jsonl(), "daemon")
    }

    #[test]
    fn mint_creates_row_for_absent_listing_row() {
        sandbox(|home| {
            let snapshot = ClaudeAgentsSnapshot::known(vec![listing_row("tui-build", "working")]);
            let em = emitter(home);
            let minted = mint_missing_native_bg(home, &snapshot, &em);
            assert_eq!(minted, 1);
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert_eq!(registry.entries.len(), 1);
            let e = &registry.entries[0];
            assert_eq!(e.name, "tui-build");
            assert_eq!(e.origin.as_deref(), Some("adopted"));
            assert_eq!(e.substrate.as_deref(), Some("native-bg"));
            assert_eq!(e.harness_name(), "claude");
            assert_eq!(e.status, AgentStatus::Live);
            assert_eq!(e.pid, Some(1895));
            assert_eq!(e.cwd, "/tmp/proj");
            assert_eq!(
                e.claude_session_uuid.as_deref(),
                Some("492ccda4-63ff-4736-a02a-8818f63c8b46")
            );
            assert_eq!(e.start_source.as_deref(), Some(START_SOURCE));
        });
    }

    #[test]
    fn mint_is_idempotent_across_beats() {
        sandbox(|home| {
            let snapshot = ClaudeAgentsSnapshot::known(vec![listing_row("tui-build", "working")]);
            let em = emitter(home);
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 1);
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 0);
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert_eq!(registry.entries.len(), 1);
        });
    }

    #[test]
    fn mint_skips_terminal_and_unknown() {
        sandbox(|home| {
            let em = emitter(home);
            let done = ClaudeAgentsSnapshot::known(vec![listing_row("tui-build", "done")]);
            assert_eq!(mint_missing_native_bg(home, &done, &em), 0);
            let unknown = ClaudeAgentsSnapshot::unknown("unreadable");
            assert_eq!(mint_missing_native_bg(home, &unknown, &em), 0);
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert!(registry.entries.is_empty());
        });
    }

    fn write_transcript(dir: &Path, uuid: &str, lines: &[String]) -> PathBuf {
        let slug = "-tmp-proj";
        let project = dir.join(slug);
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{uuid}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    #[test]
    fn first_prompt_node_id_reads_the_launch_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            "492ccda4-63ff-4736-a02a-8818f63c8b46",
            &[
                json!({"type":"user","message":{"content":"/fno:target x-4854"}}).to_string(),
                json!({"type":"assistant","message":{"model":"claude-opus-4-6"}}).to_string(),
            ],
        );
        let (node, model) = transcript_facts_from_path(&path);
        assert_eq!(node.as_deref(), Some("x-4854"));
        assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
        // No user entry: both stay absent, never guessed.
        let bare = write_transcript(
            tmp.path(),
            "ffffffff-63ff-4736-a02a-8818f63c8b46",
            &[json!({"type":"assistant","message":{"model":"m"}}).to_string()],
        );
        let (node, _) = transcript_facts_from_path(&bare);
        assert_eq!(node, None);
    }

    #[test]
    fn backfill_fills_node_on_a_later_beat() {
        sandbox(|home| {
            let em = emitter(home);
            let snapshot = ClaudeAgentsSnapshot::known(vec![listing_row("tui-build", "working")]);
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 1);
            // The beat raced the transcript: the row was born nodeless. A
            // later transcript now names the node.
            let tmp = tempfile::TempDir::new().unwrap();
            write_transcript(
                tmp.path(),
                "492ccda4-63ff-4736-a02a-8818f63c8b46",
                &[json!({"type":"user","message":{"content":"/fno:target x-53a4"}}).to_string()],
            );
            let saved = std::env::var_os("CLAUDE_PROJECTS_DIR");
            std::env::set_var("CLAUDE_PROJECTS_DIR", tmp.path());
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 0);
            std::env::remove_var("CLAUDE_PROJECTS_DIR");
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert_eq!(registry.entries.len(), 1);
            assert_eq!(registry.entries[0].node.as_deref(), Some("x-53a4"));
            match saved {
                Some(v) => std::env::set_var("CLAUDE_PROJECTS_DIR", v),
                None => {}
            }
        });
    }
}

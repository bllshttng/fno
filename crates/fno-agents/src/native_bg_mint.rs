//! The roster adopt: registry rows for `claude --bg` sessions the user
//! launched outside fno and that joined fno (`join_marker`). The sweep
//! already reads the harness listing every beat, so a listing row no registry row holds is born there - within one
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
    // An Unknown union still carries every root that parsed (a partial
    // read). Minting from present rows is a presence read, which stays
    // sound there; only absence checks need a full Known.
    let rows = match listing {
        ClaudeAgentsSnapshot::Known { rows, .. } | ClaudeAgentsSnapshot::Unknown { rows, .. } => {
            rows
        }
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
    // State alone lies: a died session keeps its last `working`/`blocked`
    // row with no pid. A listing that carries pids makes the missing pid a
    // death witness, so liveness runs through `has_live_process`.
    let carries_pids = listing.carries_pids();
    for row in rows.iter().filter(|r| r.has_live_process(carries_pids)) {
        if prepared.len() >= BUDGET_PER_BEAT {
            deferred = true;
            break;
        }
        let session: &str = row
            .session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&row.short_id);
        // Two-key dedup, the same shape liveness_sweep's find() runs: a row
        // keyed only by its short id still holds this session.
        let held_by_short = entries.iter().any(|e| e.short_id == session);
        if held_by_short
            || crate::session_report::find_row(&entries, "claude", session, None).is_some()
        {
            continue;
        }
        // Only a session that joined fno, or one a launch record names, is
        // adopted; the listing then only rescues a row that went missing.
        let record = crate::launch_record::claimed_edge(home.root(), session);
        if record.is_none() && !crate::join_marker::is_joined(home.root(), "claude", session) {
            continue;
        }
        let transcript = find_transcript_for(row, session);
        let (node, model) = transcript_facts(transcript.as_deref());
        prepared.push(prepare_row(
            row,
            session,
            transcript,
            node,
            model,
            record.as_ref(),
        ));
    }

    // A row an earlier beat birthed with no parent: the launch record its
    // child claimed still names the spawner.
    let mut parented: Vec<(String, crate::launch_record::Record)> = Vec::new();
    for e in entries.iter().filter(|e| {
        e.harness_name() == "claude"
            && e.substrate.as_deref() == Some(SUBSTRATE)
            && e.origin.as_deref() == Some("adopted")
            && e.spawned_by_session
                .as_deref()
                .unwrap_or_default()
                .is_empty()
    }) {
        if parented.len() >= BUDGET_PER_BEAT {
            deferred = true;
            break;
        }
        let Some(session) = e.harness_session_id.as_deref() else {
            continue;
        };
        if let Some(record) = crate::launch_record::claimed_edge(home.root(), session) {
            parented.push((session.to_string(), record));
        }
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
        if let Some(node) = (match e.transcript_path.as_deref() {
            // A row minted with a transcript path re-reads that file
            // directly; the ambient re-scan is for rows without one (it
            // also misses account roots).
            Some(path) => transcript_facts(Some(Path::new(path))),
            None => transcript_facts(crate::claude_drive::find_transcript(session).as_deref()),
        })
        .0
        {
            backfills.push((session.to_string(), node));
        }
    }

    let mut born: Vec<Value> = Vec::new();
    let mut filled: Vec<Value> = Vec::new();
    let mut parent_filled: Vec<Value> = Vec::new();
    let write = crate::state::update_registry(&home.registry_json(), |r| {
        for cand in prepared {
            let session = cand
                .harness_session_id
                .clone()
                .or_else(|| cand.claude_session_uuid.clone())
                .unwrap_or_else(|| cand.short_id.clone());
            let held_by_short = r.entries.iter().any(|e| e.short_id == session);
            if held_by_short
                || crate::session_report::find_row(&r.entries, "claude", &session, None).is_some()
            {
                continue;
            }
            let mut entry = cand;
            entry.name = crate::native_adopt::unique_name(&r.entries, &entry.name);
            let launch = match entry.origin.as_deref() {
                Some("spawn") => "native",
                _ => "adopted",
            };
            born.push(json!({
                "name": entry.name.clone(),
                "launch": launch,
                "spawned_by_session": entry.spawned_by_session.clone(),
                "spawned_by_harness": entry.spawned_by_harness.clone(),
                "spawned_by_cwd": entry.spawned_by_cwd.clone(),
                "harness_session_id": entry.harness_session_id.clone(),
                "cwd": entry.cwd.clone(),
                "node": entry.node.clone(),
                "pid": entry.pid,
                "model": entry.model.clone(),
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
                filled.push(json!({"name": e.name.clone(), "node": node}));
            }
        }
        for (session, record) in parented {
            let Some(e) = r.entries.iter_mut().find(|e| {
                e.harness_session_id.as_deref() == Some(session.as_str())
                    && e.spawned_by_session
                        .as_deref()
                        .unwrap_or_default()
                        .is_empty()
            }) else {
                continue;
            };
            let lineage = crate::native_adopt::launcher_lineage(&record);
            e.origin = Some("spawn".into());
            e.spawned_by_session = lineage.session;
            e.spawned_by_harness = lineage.harness;
            e.spawned_by_cwd = lineage.cwd;
            e.lineage_reason = None;
            if e.node.is_none() {
                e.node = record.launch.node.clone();
            }
            parent_filled.push(json!({
                "name": e.name.clone(),
                "spawned_by_session": e.spawned_by_session.clone(),
            }));
        }
    });
    if let Err(e) = write {
        // A failed birth write surfaces as an event, never a silent zero:
        // the sweep's own report does not name this pass.
        let _ = emitter.emit("native_bg_mint_failed", &json!({"error": e.to_string()}));
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
            "launch": b["launch"],
        });
        let lineage = match b["spawned_by_session"].as_str() {
            Some(session) => Lineage::captured((
                Some(session.to_string()),
                b["spawned_by_harness"].as_str().map(str::to_string),
                b["spawned_by_cwd"].as_str().map(str::to_string),
            )),
            None => Lineage::unproven(LINEAGE_REASON),
        };
        let _ = emitter.emit(
            "agent_spawned",
            &crate::spawn_edge::birth_event(
                b["name"].as_str().unwrap_or_default(),
                &lineage,
                extras,
            ),
        );
    }
    for f in &filled {
        let _ = emitter.emit("native_bg_node_backfilled", f);
    }
    for f in &parent_filled {
        let _ = emitter.emit("native_bg_parent_backfilled", f);
    }
    if deferred {
        let _ = emitter.emit(
            "native_bg_mint_deferred",
            &json!({"budget": BUDGET_PER_BEAT}),
        );
    }
    born.len()
}

/// Transcript discovery that honors the listing row's account: a row read
/// from an isolated account's config dir keeps its transcript under that
/// root, so the ambient scan alone would miss it.
fn find_transcript_for(
    row: &crate::claude_roster::ClaudeAgentRow,
    session: &str,
) -> Option<PathBuf> {
    crate::claude_drive::find_transcript_for_account(session, row.account.as_deref())
        .or_else(|| crate::claude_drive::find_transcript(session))
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
    let mut seen_user = false;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // The launch prompt is the FIRST user entry only: later user turns
        // mention node ids in conversation, and attributing one of those
        // would pin the wrong node on the row.
        if !seen_user && v.get("type").and_then(Value::as_str) == Some("user") {
            seen_user = true;
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
/// An unnamed `claude --bg` job is listed under its prompt, so the listing
/// name is slugged and capped before it becomes a registry name.
fn listing_name_slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn prepare_row(
    row: &crate::claude_roster::ClaudeAgentRow,
    session: &str,
    transcript: Option<PathBuf>,
    node: Option<String>,
    model: Option<String>,
    record: Option<&crate::launch_record::Record>,
) -> RegistryEntry {
    let model_basis = model.as_ref().map(|_| "verified".to_string());
    let cwd = row.cwd.clone().unwrap_or_default();
    // The session report that carries the claimed record can die after the
    // claim, so the record is read here too: it names the spawner.
    let (origin, lineage) = match record {
        Some(r) => ("spawn", crate::native_adopt::launcher_lineage(r)),
        None => ("adopted", Lineage::unproven(LINEAGE_REASON)),
    };
    let node = record.and_then(|r| r.launch.node.clone()).or(node);
    RegistryEntry {
        name: record
            .and_then(|r| r.launch.name.clone())
            .or_else(|| row.name.as_deref().map(listing_name_slug))
            // A listing that shows only the short id names nothing.
            .filter(|s| !s.is_empty() && *s != row.short_id)
            .unwrap_or_else(|| {
                let node_name = node
                    .as_deref()
                    .and_then(|n| crate::naming::node_worker_name(n, Path::new(&cwd)));
                crate::claude_adopt::synthesized_entry_name(session, &row.short_id, node_name)
            }),
        origin: Some(origin.into()),
        status: AgentStatus::Live,
        short_id: row.short_id.clone(),
        harness: Some("claude".into()),
        substrate: Some(SUBSTRATE.into()),
        node,
        created_at: crate::daemon::now_rfc3339_like(),
        // The listing pid can be a pre-warmed spare in claude's bg daemon,
        // not this session; the row stays pidless and routes by short id.
        pid: None,
        transcript_path: transcript.map(|p| p.display().to_string()),
        start_source: Some(START_SOURCE.into()),
        cwd: cwd.clone(),
        project_root: cwd,
        claude_session_uuid: row.session_id.clone().filter(|s| !s.is_empty()),
        model,
        model_basis,
        ..RegistryEntry::new(Some(session.to_string()), lineage)
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
        // The fixture sessions joined fno; `unjoined_listing_row_mints_nothing`
        // covers the other side.
        for sid in [
            "492ccda4-63ff-4736-a02a-8818f63c8b46",
            "aaaaaaaa-0000-0000-0000-000000000001",
        ] {
            crate::join_marker::write_marker(home.root(), "claude", sid).unwrap();
        }
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
            assert_eq!(e.pid, None, "a listing pid never seeds an adopted row");
            assert_eq!(e.cwd, "/tmp/proj");
            assert_eq!(
                e.claude_session_uuid.as_deref(),
                Some("492ccda4-63ff-4736-a02a-8818f63c8b46")
            );
            assert_eq!(e.start_source.as_deref(), Some(START_SOURCE));
            assert!(!e.created_at.is_empty(), "a birth dates itself");
            let prompt = "Fix the login bug: see /tmp/x.log, then run ALL the tests please now ok";
            let slug = listing_name_slug(prompt);
            assert_eq!(slug, "fix-the-login-bug-see-tmp-x.log-then-run-all-the");
            assert!(slug.len() <= 48);
        });
    }

    fn claim_record(home: &crate::paths::AgentsHome, sid: &str) {
        let dir = home.root().join("launches").join("claimed");
        std::fs::create_dir_all(&dir).unwrap();
        let record = json!({
            "harness": "claude", "name": "t-xaaaa-glm", "node": "x-aaaa", "cwd": "/tmp/proj",
            "launcher_session": "d7031e2c", "launcher_harness": "claude",
            "launched_at_ms": 0, "child_session": sid,
        });
        std::fs::write(dir.join(format!("{sid}.json")), record.to_string()).unwrap();
    }

    /// A claimed launch record names the spawner at birth, even for a session
    /// that never joined, and a row born before its record was read takes the
    /// parent on the next beat.
    #[test]
    fn a_claimed_launch_record_parents_the_row() {
        sandbox(|home| {
            let sid = "bbbbbbbb-0000-0000-0000-000000000002";
            let mut child = listing_row("prompt words", "working");
            child.session_id = Some(sid.into());
            child.short_id = "bbbbbbbb".into();
            claim_record(home, sid);
            let em = emitter(home);
            let snapshot =
                ClaudeAgentsSnapshot::known(vec![child, listing_row("tui-build", "working")]);
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 2);
            claim_record(home, "492ccda4-63ff-4736-a02a-8818f63c8b46");
            mint_missing_native_bg(home, &snapshot, &em);
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            for e in &registry.entries {
                assert_eq!(e.origin.as_deref(), Some("spawn"), "{}", e.name);
                assert_eq!(
                    e.spawned_by_session.as_deref(),
                    Some("d7031e2c"),
                    "{}",
                    e.name
                );
                assert_eq!(e.node.as_deref(), Some("x-aaaa"), "{}", e.name);
            }
            assert!(registry.entries.iter().any(|e| e.name == "t-xaaaa-glm"));
        });
    }

    #[test]
    fn unjoined_listing_row_mints_nothing() {
        sandbox(|home| {
            let mut row = listing_row("hand-bg", "working");
            row.session_id = Some("bbbbbbbb-0000-0000-0000-000000000002".into());
            row.short_id = "bbbbbbbb".into();
            let snapshot = ClaudeAgentsSnapshot::known(vec![row]);
            assert_eq!(mint_missing_native_bg(home, &snapshot, &emitter(home)), 0);
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
    fn degraded_listings_mint_only_sound_rows() {
        sandbox(|home| {
            let em = emitter(home);
            let done = ClaudeAgentsSnapshot::known(vec![listing_row("tui-build", "done")]);
            assert_eq!(mint_missing_native_bg(home, &done, &em), 0);
            let unknown = ClaudeAgentsSnapshot::unknown("unreadable");
            assert_eq!(mint_missing_native_bg(home, &unknown, &em), 0);
            // A died session keeps its last `working` row with no pid; in a
            // listing that carries pids that row is a death witness, and the
            // live row beside it still mints.
            let mut dead = listing_row("tui-dead", "working");
            dead.pid = None;
            let mixed = ClaudeAgentsSnapshot::known(vec![dead, listing_row("tui-live", "working")]);
            assert_eq!(mint_missing_native_bg(home, &mixed, &em), 1);
            // A failed root leaves the union Unknown but keeps every root
            // that parsed; presence reads on those rows stay sound. Its row
            // carries a distinct session: the dedup is per session, and the
            // fixture uuid above is already held by tui-live. The short id
            // follows the session, as a real listing derives it from the
            // uuid's own leading eight; a second row wearing tui-live's
            // short id is an identity collision the registry write refuses.
            let mut partial_row = listing_row("tui-partial", "working");
            partial_row.session_id = Some("aaaaaaaa-0000-0000-0000-000000000001".into());
            partial_row.short_id = "aaaaaaaa".into();
            let partial = ClaudeAgentsSnapshot::Unknown {
                rows: vec![partial_row],
                warnings: vec!["a root failed".into()],
            };
            assert_eq!(mint_missing_native_bg(home, &partial, &em), 1);
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert_eq!(registry.entries.len(), 2);
            let names: Vec<&str> = registry.entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["tui-live", "tui-partial"]);
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

    fn transcript_facts_from_path(path: &Path) -> (Option<String>, Option<String>) {
        transcript_facts(Some(path))
    }

    #[test]
    fn first_prompt_node_id_reads_the_launch_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_transcript(
            tmp.path(),
            "492ccda4-63ff-4736-a02a-8818f63c8b46",
            &[
                json!({"type":"user","message":{"content":"/fno:target x-deadbeef"}}).to_string(),
                json!({"type":"assistant","message":{"model":"claude-opus-4-6"}}).to_string(),
            ],
        );
        let (node, model) = transcript_facts_from_path(&path);
        assert_eq!(node.as_deref(), Some("x-deadbeef"));
        assert_eq!(model, Some("claude-opus-4-6".to_string()));
        // A LATER user turn naming another node is conversation, not the
        // launch prompt: the first entry owns the attribution.
        let two = write_transcript(
            tmp.path(),
            "eeeeeeee-63ff-4736-a02a-8818f63c8b46",
            &[
                json!({"type":"user","message":{"content":"look at x-feedface later"}}).to_string(),
                json!({"type":"assistant","message":{"model":"m"}}).to_string(),
                json!({"type":"user","message":{"content":"now do x-deadbeef"}}).to_string(),
            ],
        );
        let (node, _) = transcript_facts_from_path(&two);
        assert_eq!(node, Some("x-feedface".to_string()));
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
    fn account_row_transcript_reads_the_account_root() {
        sandbox(|home| {
            let em = emitter(home);
            // The account root, wired through the fno accounts config, holds
            // the transcript; the ambient HOME has none. A config dir
            // mirrors ~/.claude, so the transcript sits under projects/.
            let root = tempfile::TempDir::new().unwrap();
            let uuid = "492ccda4-63ff-4736-a02a-8818f63c8b46";
            write_transcript(
                &root.path().join("projects"),
                uuid,
                &[
                    json!({"type":"user","message":{"content":"/fno:target x-deadbeef"}})
                        .to_string(),
                ],
            );
            let accounts_dir = home
                .registry_json()
                .parent()
                .and_then(|p| p.parent())
                .unwrap()
                .join(".fno");
            std::fs::create_dir_all(&accounts_dir).unwrap();
            std::fs::write(
                accounts_dir.join("config.toml"),
                format!(
                    "[[accounts.records]]\nid = \"test-acct-x53a4\"\nconfig_dir = \"{}\"\n",
                    root.path().display()
                ),
            )
            .unwrap();
            let mut row = listing_row("acct-build", "working");
            row.account = Some("test-acct-x53a4".into());
            let snapshot = ClaudeAgentsSnapshot::known(vec![row]);
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 1);
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert_eq!(registry.entries[0].node.as_deref(), Some("x-deadbeef"));
        });
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
                &[
                    json!({"type":"user","message":{"content":"/fno:target x-feedface"}})
                        .to_string(),
                ],
            );
            let saved = std::env::var_os("FNO_CLAUDE_PROJECTS_DIR");
            std::env::set_var("FNO_CLAUDE_PROJECTS_DIR", tmp.path());
            assert_eq!(mint_missing_native_bg(home, &snapshot, &em), 0);
            std::env::remove_var("FNO_CLAUDE_PROJECTS_DIR");
            let registry = crate::daemon::load_registry_asserted(&home.registry_json()).unwrap();
            assert_eq!(registry.entries.len(), 1);
            assert_eq!(registry.entries[0].node.as_deref(), Some("x-feedface"));
            match saved {
                Some(v) => std::env::set_var("FNO_CLAUDE_PROJECTS_DIR", v),
                None => {}
            }
        });
    }
}

//! The first-turn bind door: a claude `UserPromptSubmit` payload names one
//! backlog node and this door stamps the session's roster row (minting one
//! adopt-style when no row exists) with `node` + `node_basis: "first-turn"`.
//! The bind holds NO claim - `node:<id>` stays free and no manifest is
//! written; `fno do target start <id> --steps` remains the only claim door.
//! One payload in on stdin, at most one line out on stdout, and every
//! failure is silent exit 0: nothing optional sits on the foreground path
//! (law d-bad5a42f). Found and written strictly by harness session id,
//! never by name (law d-e952ed19); the basis rides the row beside the node
//! (law d-d6cb1827).

use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Read;

/// Node statuses a first-turn mention never binds: terminal or withdrawn
/// work. Everything else (intake, ready, in_progress, blocked, deferred)
/// binds.
const UNBINDABLE: [&str; 3] = ["done", "superseded", "retracted"];

/// What the door decided for one payload. `Silent` writes and prints
/// nothing; the other two carry the one line the shim relays.
#[derive(Debug)]
pub(crate) enum BindOutcome {
    Silent,
    Bound(String),
    Ambiguous(String),
}

pub fn run(_rest: &[String]) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    match bind_first_turn(&input) {
        BindOutcome::Silent => {}
        BindOutcome::Bound(line) | BindOutcome::Ambiguous(line) => println!("{line}"),
    }
    0
}

/// The door's one decision, testable without stdin. Reads the graph and the
/// agent registry through their environment seams, so a test redirects both
/// with `FNO_STATE_DIR` and `FNO_AGENTS_HOME`.
pub(crate) fn bind_first_turn(payload: &str) -> BindOutcome {
    let Ok(payload) = serde_json::from_str::<Value>(payload) else {
        return BindOutcome::Silent;
    };
    let sid = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(sid) = sid else {
        return BindOutcome::Silent;
    };
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let resolved = match open_nodes_named(prompt) {
        Ok(nodes) => nodes,
        Err(e) => {
            eprintln!("node-bind: graph unreadable, no bind: {e}");
            return BindOutcome::Silent;
        }
    };
    match resolved.len() {
        0 => BindOutcome::Silent,
        1 => bind_session(
            sid,
            cwd,
            resolved.into_iter().next().expect("one open node"),
        ),
        _ => BindOutcome::Ambiguous(format!(
            "first turn names several open nodes: {}; bound nothing. To claim \
one: fno do target start <node-id> --steps",
            resolved.into_iter().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The DISTINCT open node ids the prompt names, resolved by exact graph
/// lookup through the shared id-shape gate. A token the graph does not hold
/// is not a candidate; a done, superseded or retracted node is not open.
fn open_nodes_named(prompt: &str) -> Result<BTreeSet<String>, String> {
    let graph = crate::backlog::settings::graph_path();
    let rows = crate::graph_store::read_rows(&graph).map_err(|e| e.to_string())?;
    let mut open = BTreeSet::new();
    for token in prompt
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
    {
        if !crate::backlog::node_ref::is_wellformed_node_id(token) {
            continue;
        }
        let Some(row) = crate::backlog::node_ref::resolve_tiers(&rows, token) else {
            continue;
        };
        let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() || UNBINDABLE.contains(&status) {
            continue;
        }
        open.insert(id.to_string());
    }
    Ok(open)
}

/// Stamp `node` + `node_basis: "first-turn"` on the row keyed by `sid`,
/// minting one adopt-style when the session has no row. A row that already
/// names a node is left untouched (spawn and target work reads as claimed by
/// absence of the basis).
fn bind_session(sid: &str, cwd: &str, node: String) -> BindOutcome {
    let home = crate::paths::AgentsHome::from_env();
    let registry_path = home.registry_json();
    let entries = match crate::state::load_registry(&registry_path) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("node-bind: registry unreadable, no bind: {e}");
            return BindOutcome::Silent;
        }
    };
    let existing = entries
        .entries
        .iter()
        .any(|e| e.harness_session_id.as_deref() == Some(sid));
    let wrote = if existing {
        // Re-locate by sid INSIDE the locked write: the load-time snapshot
        // can go stale on a concurrent writer. A row another writer stamped
        // between load and lock keeps its node and stays silent; a row the
        // reaper removed between load and lock falls through to the mint
        // below, so the confirm line never describes a write that did not
        // happen.
        let found = std::cell::Cell::new(false);
        let held = std::cell::Cell::new(false);
        let wrote = crate::state::update_registry(&registry_path, |reg| {
            if let Some(row) = reg
                .entries
                .iter_mut()
                .find(|e| e.harness_session_id.as_deref() == Some(sid))
            {
                found.set(true);
                if row.node.as_deref().is_none_or(|n| n.is_empty()) {
                    row.node = Some(node.clone());
                    row.node_basis = Some("first-turn".to_string());
                } else {
                    held.set(true);
                }
            }
        });
        match wrote {
            Ok(()) if held.get() => return BindOutcome::Silent,
            Ok(()) if found.get() => None,
            Ok(()) => Some(()),
            Err(e) => {
                eprintln!("node-bind: registry write failed, no bind: {e}");
                return BindOutcome::Silent;
            }
        }
    } else {
        Some(())
    };
    if let Some(()) = wrote {
        if let Err(e) = mint_and_upsert(&registry_path, sid, cwd, &node) {
            eprintln!("node-bind: registry write failed, no bind: {e}");
            return BindOutcome::Silent;
        }
    }
    BindOutcome::Bound(format!(
        "bound this session to {node} (named in your first turn). It holds no \
claim. To claim it without the loop: fno do target start {node} --steps"
    ))
}

/// The adopt-shaped mint for a session no registry row holds: identity only,
/// origin `adopted` (or `operator` when the operator checks say so), no
/// liveness or runtime state - reconcile owns those. Every ManifestIdentity
/// field is written out: the mint guard's text scan reads any
/// `..Default::default()` spread in production code as a default-based mint.
fn mint_for_session(sid: &str, cwd: &str) -> crate::state::RegistryEntry {
    crate::client_verbs::mint_synthesized_entry(
        &crate::manifest_lookup::ManifestIdentity {
            harness: "claude".to_string(),
            harness_session_id: sid.to_string(),
            claude_session_id: sid.to_string(),
            codex_thread_id: String::new(),
            owner_cwd: cwd.to_string(),
            fno_id: String::new(),
            manifest_path: std::path::PathBuf::new(),
        },
        &crate::daemon::now_rfc3339_like(),
    )
}

/// Mint the row and stamp the bind on it in one upsert. Also the fallback
/// for a row that vanished between the load and the locked write.
fn mint_and_upsert(
    registry_path: &std::path::Path,
    sid: &str,
    cwd: &str,
    node: &str,
) -> Result<(), crate::state::StateError> {
    let mut minted = mint_for_session(sid, cwd);
    minted.node = Some(node.to_string());
    minted.node_basis = Some("first-turn".to_string());
    crate::client_verbs::upsert_synthesized_row(registry_path, minted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn seed(tmp: &std::path::Path, rows: &[Value]) {
        crate::graph_store::seed_rows(&tmp.join("graph.json"), rows).expect("seed graph");
    }

    fn payload(sid: &str, prompt: &str) -> String {
        json!({"session_id": sid, "prompt": prompt, "cwd": "/work/proj"}).to_string()
    }

    #[test]
    fn one_open_node_binds_once_and_respects_a_held_node() {
        let _g = env_guard();
        let tmp = tempfile::TempDir::new().expect("tmp");
        std::env::set_var("FNO_STATE_DIR", tmp.path());
        std::env::set_var(crate::paths::HOME_ENV, tmp.path().join("home"));
        seed(
            tmp.path(),
            &[
                json!({"id": "x-aaaa", "title": "a", "status": "ready"}),
                json!({"id": "x-bbbb", "title": "b", "status": "in_progress"}),
            ],
        );

        // AC12: prose naming exactly one open node mints the row keyed by
        // session id, stamps node + node_basis, and prints the confirm line.
        let out = bind_first_turn(&payload(
            "11111111-2222-4333-8444-555566667777",
            "start on x-aaaa please",
        ));
        let BindOutcome::Bound(line) = &out else {
            panic!("expected a bind, got {out:?}");
        };
        assert!(
            line.contains("x-aaaa") && line.contains("--steps"),
            "{line}"
        );
        let reg =
            crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
                .unwrap();
        assert_eq!(reg.entries.len(), 1);
        assert_eq!(reg.entries[0].node.as_deref(), Some("x-aaaa"));
        assert_eq!(reg.entries[0].node_basis.as_deref(), Some("first-turn"));

        // A later door run on the now-bound row changes nothing and prints
        // nothing (the shim's marker is the per-prompt guard; this is the
        // door's own).
        let again = bind_first_turn(&payload(
            "11111111-2222-4333-8444-555566667777",
            "x-aaaa again",
        ));
        assert!(matches!(again, BindOutcome::Silent));

        // AC14: a row that already holds a spawn node is never rebound.
        let sid = "99998888-7777-4666-8555-444433332222";
        let held = crate::client_verbs::mint_synthesized_entry(
            &crate::manifest_lookup::ManifestIdentity {
                harness: "claude".into(),
                harness_session_id: sid.into(),
                claude_session_id: sid.into(),
                owner_cwd: "/work/proj".into(),
                ..Default::default()
            },
            "t0",
        );
        let mut held = held;
        held.node = Some("x-bbbb".into());
        crate::client_verbs::upsert_synthesized_row(
            &crate::paths::AgentsHome::from_env().registry_json(),
            held,
        )
        .unwrap();
        let out = bind_first_turn(&payload(sid, "actually x-aaaa"));
        assert!(matches!(out, BindOutcome::Silent));
        let reg =
            crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
                .unwrap();
        let row = reg
            .entries
            .iter()
            .find(|e| e.harness_session_id == Some(sid.to_string()))
            .unwrap();
        assert_eq!(row.node.as_deref(), Some("x-bbbb"));
        assert_eq!(row.node_basis, None);
        std::env::remove_var("FNO_STATE_DIR");
        std::env::remove_var(crate::paths::HOME_ENV);
    }

    #[test]
    fn two_open_nodes_list_done_and_absent_stay_silent() {
        let _g = env_guard();
        let tmp = tempfile::TempDir::new().expect("tmp");
        std::env::set_var("FNO_STATE_DIR", tmp.path());
        std::env::set_var(crate::paths::HOME_ENV, tmp.path().join("home"));
        seed(
            tmp.path(),
            &[
                json!({"id": "x-aaaa", "title": "a", "status": "ready"}),
                json!({"id": "x-bbbb", "title": "b", "status": "ready"}),
                json!({"id": "x-cccc", "title": "c", "status": "done"}),
            ],
        );

        // AC13, two open nodes: no write, one line listing both and the door.
        let out = bind_first_turn(&payload(
            "11111111-2222-4333-8444-555566667777",
            "compare x-aaaa with x-bbbb",
        ));
        let BindOutcome::Ambiguous(line) = &out else {
            panic!("expected the listing line, got {out:?}");
        };
        assert!(line.contains("x-aaaa") && line.contains("x-bbbb") && line.contains("--steps"));
        assert!(
            crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
                .unwrap()
                .entries
                .is_empty()
        );

        // One done node: nothing changes, nothing prints.
        let out = bind_first_turn(&payload(
            "11111111-2222-4333-8444-555566667777",
            "see x-cccc",
        ));
        assert!(matches!(out, BindOutcome::Silent));
        // A well-formed token the graph does not hold: silent.
        let out = bind_first_turn(&payload(
            "11111111-2222-4333-8444-555566667777",
            "read x-dddd",
        ));
        assert!(matches!(out, BindOutcome::Silent));
        // No node id at all: silent.
        let out = bind_first_turn(&payload(
            "11111111-2222-4333-8444-555566667777",
            "hello there",
        ));
        assert!(matches!(out, BindOutcome::Silent));
        assert!(
            crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
                .unwrap()
                .entries
                .is_empty()
        );
        std::env::remove_var("FNO_STATE_DIR");
        std::env::remove_var(crate::paths::HOME_ENV);
    }
}

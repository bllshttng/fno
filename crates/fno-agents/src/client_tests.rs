//! The fno-agents client binary's test module. Lives outside client.rs
//! because the file-budget gate makes that file shrink-only.

use super::*;
use fno_agents::restart_run::rm_after_drift_repair;
use fno_agents::{emit_schema_json, state::AgentState, AgentStatus, KNOWN_EVENT_KINDS};

// -----------------------------------------------------------------------
// ab-351427cb: --help parity (top-level command list + per-command usage)
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// x-9112: bg/headless provider inference parity with Python
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// W7: --emit-schema unit tests (struct-drift guard + JSON parse check)
// -----------------------------------------------------------------------

/// AC2-HP: emit_schema_json() must produce valid JSON containing the
/// required top-level keys (envelope, status, event_kinds).
#[test]
fn schema_rows() {
    let schema = emit_schema_json();
    assert!(schema.get("envelope").is_some(), "missing 'envelope' key");
    assert!(schema.get("status").is_some(), "missing 'status' key");
    assert!(
        schema.get("event_kinds").is_some(),
        "missing 'event_kinds' key"
    );
    assert!(
        schema["envelope"]["properties"]["source"]["anyOf"][0]["enum"]
            .as_array()
            .is_some_and(|sources| sources.iter().any(|source| source.as_str() == Some("rust"))),
        "Rust registry event source is absent from the emitted envelope"
    );

    let schema = emit_schema_json();
    let s = serde_json::to_string(&schema).expect("schema must serialize");
    let back: serde_json::Value = serde_json::from_str(&s).expect("re-parse must succeed");
    assert_eq!(schema, back);

    use fno_agents::state::PtyState;

    // --- AgentState (pty: None) ---
    let sample = AgentState {
        schema_version: 1,
        short_id: "wkA".into(),
        status: AgentStatus::Ready,
        ready: true,
        last_message_at: Some("2026-01-01T00:00:00Z".into()),
        last_reply: Some("hello".into()),
        restart_count: 2,
        last_restart_at: Some("2026-01-01T00:00:01Z".into()),
        pty: None,
    };
    let serialized = serde_json::to_value(&sample).expect("AgentState must serialize");
    let struct_keys: std::collections::HashSet<String> = serialized
        .as_object()
        .expect("must be object")
        .keys()
        .cloned()
        .collect();

    let schema = emit_schema_json();
    let schema_props = schema["status"]["properties"]
        .as_object()
        .expect("status.properties must be object");
    let schema_keys: std::collections::HashSet<String> = schema_props.keys().cloned().collect();

    // Direction 1: schema_props ⊆ struct_keys
    for key in &schema_keys {
        assert!(
                struct_keys.contains(key.as_str()),
                "emitted status schema has property {key:?} not in serialized AgentState: {struct_keys:?}"
            );
    }

    // Direction 2: struct_keys ⊆ schema_props
    for key in &struct_keys {
        assert!(
            schema_keys.contains(key.as_str()),
            "AgentState field {key:?} not in emitted status schema properties: {schema_keys:?}"
        );
    }

    // --- PtyState / PtyStateWire bidirectional check ---
    // Serialize a PtyState WITH drive active so all optional wire fields
    // (drive_session_id, drive_mode, last_heartbeat_at_monotonic_ns) are
    // present in the output. Using default() (no drive) omits them via
    // skip_serializing_if, which would make Direction 1 trivially pass
    // while hiding that the schema has properties absent from the struct.
    use fno_agents::state::DriveWindow;
    let pty_sample = PtyState {
        active: true,
        drive: Some(DriveWindow {
            session_id: Some("sess-1".into()),
            mode: Some("interactive".into()),
            last_heartbeat_at_monotonic_ns: Some(123_456_789),
        }),
    };
    let pty_json = serde_json::to_value(&pty_sample).expect("PtyState must serialize");
    let pty_struct_keys: std::collections::HashSet<String> = pty_json
        .as_object()
        .expect("pty must be object")
        .keys()
        .cloned()
        .collect();

    // The emitted pty schema is the second branch of the oneOf (type: object).
    let pty_schema_obj = schema["status"]["properties"]["pty"]["oneOf"]
        .as_array()
        .expect("pty oneOf must be array")
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("object"))
        .expect("pty oneOf must have an object branch");
    let pty_schema_props = pty_schema_obj["properties"]
        .as_object()
        .expect("pty object branch must have properties");
    let pty_schema_keys: std::collections::HashSet<String> =
        pty_schema_props.keys().cloned().collect();

    // Direction 1: pty schema_props ⊆ pty wire keys
    for key in &pty_schema_keys {
        assert!(
                pty_struct_keys.contains(key.as_str()),
                "emitted pty schema has property {key:?} not in serialized PtyState wire: {pty_struct_keys:?}"
            );
    }

    // Direction 2: pty wire keys ⊆ pty schema_props
    for key in &pty_struct_keys {
        assert!(
            pty_schema_keys.contains(key.as_str()),
            "PtyState wire field {key:?} not in emitted pty schema properties: {pty_schema_keys:?}"
        );
    }

    assert!(!KNOWN_EVENT_KINDS.is_empty());
    assert!(KNOWN_EVENT_KINDS.contains(&"agent_spawned"));
    assert!(KNOWN_EVENT_KINDS.contains(&"daemon_started"));
    assert!(KNOWN_EVENT_KINDS.contains(&"event_payload_too_large"));
}

/// AC2-HP: The emitted schema must serialize to valid JSON (round-trip check).

/// Bidirectional struct-drift guard for AgentState + PtyStateWire.
///
/// Direction 1 (schema ⊆ struct): every property key in the emitted
/// status schema must exist as a serialized AgentState field. A property
/// added to emit_schema_json() without a corresponding struct field is
/// caught here.
///
/// Direction 2 (struct ⊆ schema): every serialized AgentState field must
/// appear in the emitted status schema properties. A new AgentState field
/// forgotten in emit_schema_json() is caught here.
///
/// The same bidirectional check is applied to the pty sub-object vs the
/// on-disk PtyStateWire flat fields (active, drive_active, drive_session_id,
/// drive_mode, last_heartbeat_at_monotonic_ns).

/// AC2-HP: KNOWN_EVENT_KINDS must be non-empty and contain the canonical kinds.

// -----------------------------------------------------------------------
// Task 2.1: format_success per-command output (stop/rm stdout parity)
// -----------------------------------------------------------------------

/// A codex thread stop names the interrupt outcome; `no-turn` stays silent.
#[test]
fn stop_rows() {
    let result = json!({"stopped": true, "backend": "codex-thread", "interrupt": "interrupted"});
    let out = format_success("stop", "t", &result, false, true, false).expect("stop line");
    assert_eq!(out, "stopped: t (turn interrupted)");
    let no_turn = json!({"stopped": true, "backend": "codex-thread", "interrupt": "no-turn"});
    let out = format_success("stop", "t", &no_turn, false, true, false).expect("stop line");
    assert_eq!(out, "stopped: t");

    let result = json!({
        "stopped": false,
        "backend": "codex-thread",
        "interrupt": "timeout-turn-still-running",
    });
    let out = format_success("stop", "t", &result, false, true, false).expect("stop line");
    assert!(
        !out.contains("stopped"),
        "a refused stop must not report one: {out}"
    );
    assert_eq!(
        out,
        "stop refused: t is still running (timeout-turn-still-running)"
    );

    let result = json!({"stopped": true, "short_id": "fo-1a2b"});
    let out = format_success("stop", "foo", &result, false, true, false);
    assert_eq!(out, Some("stopped: foo (fo-1a2b)".to_string()));

    let result = json!({"stopped": true});
    let out = format_success("stop", "foo", &result, false, true, false);
    assert_eq!(out, Some("stopped: foo".to_string()));
}

/// (x-6678) A refused stop never prints the word "stopped". The daemon
/// answers `stopped: false` over a turn its interrupt never settled, and
/// the old formatter read only `interrupt`, so it printed
/// "stopped: t (turn timeout-turn-still-running)" over a live worker.

/// AC1-HP: stop with short_id in result -> "stopped: <name> (<short_id>)"

/// AC1-HP: stop fallback when short_id absent -> "stopped: <name>"

/// A verified Claude cascade names both surfaces in the receipt.
#[tokio::test]
async fn rm_flow_rows() {
    // Authoring gate: (1) protects the observable ordering that rm repairs a
    // proven drift before issuing the RPC; (2) a regression that skips repair,
    // repairs on Unknown, or calls rm after a failed repair breaks the counts;
    // (3) existing rm formatting tests do not reach this lifecycle boundary;
    // (4) the helper uses restart and RPC closures that production also needs
    // to order the real operations, so this is not a test-only seam.
    use fno_agents::client::RestartError;
    use std::cell::Cell;

    let drifted = DriftState::Drifted {
        running: fno_agents::drift::ExeFingerprint {
            path: "/old/fno-agents-daemon".into(),
            mtime_nanos: 1,
            size: 1,
        },
        on_disk: fno_agents::drift::ExeFingerprint {
            path: "/new/fno-agents-daemon".into(),
            mtime_nanos: 2,
            size: 2,
        },
    };
    for (state, should_restart) in [
        (drifted.clone(), true),
        (DriftState::Fresh, false),
        (DriftState::DaemonDown, false),
        (DriftState::Unknown, false),
    ] {
        let restart_calls = Cell::new(0);
        let rm_calls = Cell::new(0);
        let result = rm_after_drift_repair(
            &state,
            async {
                restart_calls.set(restart_calls.get() + 1);
                Ok::<(), RestartError>(())
            },
            async {
                rm_calls.set(rm_calls.get() + 1);
                "removed"
            },
        )
        .await;
        assert_eq!(result, Ok("removed"));
        assert_eq!(restart_calls.get(), usize::from(should_restart));
        assert_eq!(rm_calls.get(), 1);
    }

    let rm_calls = Cell::new(0);
    let result = rm_after_drift_repair(
        &drifted,
        async { Err::<(), RestartError>(RestartError::StatusMissingPid) },
        async {
            rm_calls.set(rm_calls.get() + 1);
            "removed"
        },
    )
    .await;
    assert_eq!(result, Err(()));
    assert_eq!(rm_calls.get(), 0, "failed repair must not issue rm");

    let result = json!({
        "removed": true,
        "registry_removed": true,
        "harness": "claude",
        "harness_removed": true,
        "was_orphaned": false
    });
    let out = format_success("rm", "bar-agent", &result, false, true, false);
    assert_eq!(out, Some("removed: bar-agent (fno + claude)".to_string()));

    assert_eq!(
        rm_failure_line("bar-agent", "protocol: connection closed", Some(true)),
        "fno-agents: rm bar-agent: protocol: connection closed; \
         nothing was removed - the row is still registered"
    );
    assert_eq!(
        rm_failure_line("bar-agent", "protocol: connection closed", Some(false)),
        "fno-agents: rm bar-agent: protocol: connection closed; \
         the row is no longer registered"
    );
    assert_eq!(
        rm_failure_line("bar-agent", "protocol: connection closed", None),
        "fno-agents: rm bar-agent: protocol: connection closed; \
         the registry could not be read to confirm whether anything was removed"
    );

    let result = json!({
        "removed": true,
        "registry_removed": true,
        "harness": "claude",
        "harness_removed": true,
        "worktree_touched": true,
        "worktree_outcome": "removed",
        "reclaimed_bytes": 4096
    });
    let out = format_success("rm", "bar-agent", &result, false, true, false)
        .expect("rm renders a receipt");
    assert!(
        out.contains("WARNING"),
        "worktree deletion must be visible: {out}"
    );
    assert!(out.to_ascii_lowercase().contains("worktree"), "{out}");

    let result = json!({
        "removed": true,
        "registry_removed": true,
        "harness": "codex",
        "harness_row_id": "01a02125",
        "harness_session_id": "01a02125-4eb4-7bf1-b74e-d238887eb092",
        "harness_removed": true
    });
    let out = format_success("rm", "bar-agent", &result, false, true, false)
        .expect("rm renders a receipt");
    assert!(
        out.contains("fno agents adopt 01a02125-4eb4-7bf1-b74e-d238887eb092"),
        "{out}"
    );
    assert!(!out.contains("adopt 01a02125\n"), "{out}");

    let result = json!({
        "removed": true,
        "registry_removed": true,
        "harness": "claude",
        "harness_row_id": "0a6e775f",
        "harness_removed": true
    });
    let out = format_success("rm", "bar-agent", &result, false, true, false)
        .expect("rm renders a receipt");
    assert!(
        out.starts_with("removed: bar-agent (fno + claude)"),
        "{out}"
    );
    assert!(out.contains("fno agents adopt 0a6e775f"), "{out}");
    assert!(out.contains("resume handle"), "{out}");

    let resume = command_usage("resume").expect("resume usage");
    assert!(resume.contains("--cwd <existing-checkout>"), "{resume}");
    // The seam appends --account on a wake; the usage line naming it is
    // the parse contract that keeps the wake ladder honest.
    assert!(resume.contains("--account <id>"), "{resume}");

    let adopt = command_usage("adopt").expect("adopt usage");
    assert!(!adopt.contains("--cross-project"), "{adopt}");

    let result = json!({
        "removed": true,
        "registry_removed": true,
        "harness": "claude",
        "harness_removed": true
    });
    let out = format_success("rm", "bar-agent", &result, false, true, false);
    assert_eq!(out, Some("removed: bar-agent (fno + claude)".to_string()));

    let result = json!({
        "removed": true,
        "registry_removed": true,
        "harness": "claude",
        "harness_removed": null,
        "harness_reason": "claude list unreadable"
    });
    let out = format_success("rm", "bar-agent", &result, false, true, false);
    assert_eq!(
        out,
        Some(
            "removed: bar-agent (fno only; claude list unreadable, harness side unverified)"
                .to_string()
        )
    );
}

/// The transport-failure line names the row and answers from the registry
/// verdict the caller passed, never from the past-tense pre-exec banner. An
/// unread store (`None`) says so instead of wearing an absent verdict.

/// A reap that really removed a harness row names the command that puts it
/// back. Asserted on the literal `fno agents adopt` string, not merely on
/// the receipt being longer: an absence has two explanations.
/// A codex row carries no short_id, so `harness_row_id` degrades to the
/// first eight chars of a time-prefixed id -- which collides across
/// same-window sessions. The hint must name the full id instead, or it
/// points an operator at a sibling session.

/// No row id means no handle to name, so the receipt stays exactly as it
/// was rather than printing `fno agents adopt unknown`.

// x-9485: a confirmed pane stop prints its measurement - the pid that died -
// not a bare surface name.

/// AC2-HP: unknown command returns None (falls back to pretty-print).
/// `spawn` is NOT unknown post-x-3ab8 (it renders a receipt, covered by
/// `format_success_spawn_emits_compact_receipt`); use a truly unhandled command.

// ab-1891cdff: `restart` outcome rendering (AC2-HP / AC2-EDGE / AC2-FR)
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Task 4.1: ask create-on-first-contact + follow-up output parity
// -----------------------------------------------------------------------

/// AC1-HP (create): ask with created=true in result prints "<short_id>\n" only.
#[test]
fn ask_rows() {
    let result = json!({"created": true, "short_id": "cx-1a2b3c"});
    let out = format_success("ask", "myagent", &result, false, true, false);
    assert_eq!(out, Some("cx-1a2b3c".to_string()));

    let reply = "Here is my answer to your question.";
    let result = json!({"reply": reply, "backend": "pty"});
    let out = format_success("ask", "myagent", &result, false, true, false);
    assert_eq!(out, Some(reply.to_string()));

    let result = json!({"reply": "", "backend": "pty"});
    let out = format_success("ask", "myagent", &result, false, true, false);
    assert_eq!(out, Some(String::new()));

    let result = json!({
        "reply": null,
        "backend": "codex-thread",
        "turn_id": "turn-9",
        "status": "in_flight",
    });
    let out = format_success("ask", "myagent", &result, false, true, false)
        .expect("in_flight formats a line");
    assert!(out.contains("turn-9"), "names the turn: {out}");
    assert!(out.contains("in flight"), "names the state: {out}");

    let args = vec![
        "myagent".to_string(),
        "hello there".to_string(),
        "--from-name".to_string(),
        "fno".to_string(),
        "--timeout".to_string(),
        "30".to_string(),
    ];
    let result = build_request("ask", &args);
    assert!(
        result.is_ok(),
        "build_request must accept --from-name/--timeout: {result:?}"
    );
    let (method, params) = result.unwrap();
    assert_eq!(method, "agent.ask");
    assert_eq!(params["name"], "myagent");
    assert_eq!(params["from_name"], "fno");
    assert_eq!(params["timeout"], 30u64);
    // --yolo retired with the spawn axes, on every command and in both spellings.
    for flag in ["--yolo", "-Y"] {
        let err = build_request("ask", &["a".to_string(), "m".to_string(), flag.to_string()])
            .unwrap_err();
        assert!(err.contains("--yolo is retired"), "{flag}: {err}");
    }

    let args = vec![
        "myagent".to_string(),
        "hello there".to_string(),
        "--harness=gemini".to_string(),
        "--cwd=/repo".to_string(),
        "--timeout=30".to_string(),
        "--from-name=bot".to_string(),
    ];
    let result = build_request("ask", &args);
    assert!(
        result.is_ok(),
        "equals-form ask flags must parse: {result:?}"
    );
    let (method, params) = result.unwrap();
    assert_eq!(method, "agent.ask");
    assert_eq!(params["name"], "myagent");
    assert_eq!(params["provider"], "gemini");
    assert_eq!(params["cwd"], "/repo");
    assert_eq!(params["timeout"], 30u64);
    assert_eq!(params["from_name"], "bot");
    // A value containing '=' (e.g. a path) splits only on the first '='.
    let (_m, p2) = build_request(
        "ask",
        &["a".to_string(), "m".to_string(), "--cwd=/a=b".to_string()],
    )
    .unwrap();
    assert_eq!(p2["cwd"], "/a=b");
    // The anycast ask names a project, so every positional is the message and
    // the peer name resolves later in main. --any alone breaks no tie.
    let (method, p3) = build_request(
        "ask",
        &[
            "--to-project=fno".to_string(),
            "hi".to_string(),
            "there".to_string(),
            "--any".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(method, "agent.ask");
    assert_eq!(p3["to_project"], "fno");
    assert_eq!(p3["message"], "hi there");
    assert_eq!(p3["any"], Value::Bool(true));
    assert!(p3.get("name").is_none());
    let err = build_request(
        "ask",
        &["a".to_string(), "m".to_string(), "--any".to_string()],
    )
    .unwrap_err();
    assert!(err.contains("--to-project"), "{err}");
    let err = build_request("ask", &["--to-project=".to_string(), "hi".to_string()]).unwrap_err();
    assert!(err.contains("needs a project"), "{err}");
    // AC3 (Rust path, equals form): the retired --provider spelling is a
    // tombstone in both syntaxes - it must NOT silently route.
    let err = build_request(
        "ask",
        &[
            "a".to_string(),
            "m".to_string(),
            "--provider=gemini".to_string(),
        ],
    )
    .unwrap_err();
    assert!(err.contains("split at the axis rename"), "got: {err}");
    assert!(err.contains("--harness/-H"), "got: {err}");

    // -H/-c/-t build the byte-identical request the long flags would.
    let short = build_request(
        "ask",
        &[
            "myagent".to_string(),
            "hi".to_string(),
            "-H".to_string(),
            "claude".to_string(),
            "-c".to_string(),
            "/repo".to_string(),
            "-t".to_string(),
            "30".to_string(),
        ],
    )
    .expect("short flags must parse on the Rust ask path");
    let long = build_request(
        "ask",
        &[
            "myagent".to_string(),
            "hi".to_string(),
            "--harness".to_string(),
            "claude".to_string(),
            "--cwd".to_string(),
            "/repo".to_string(),
            "--timeout".to_string(),
            "30".to_string(),
        ],
    )
    .expect("long flags must parse");
    assert_eq!(
        short, long,
        "short flags must build the same request as long flags"
    );
    let (method, params) = short;
    assert_eq!(method, "agent.ask");
    assert_eq!(params["name"], "myagent");
    assert_eq!(params["provider"], "claude");
    assert_eq!(params["cwd"], "/repo");
    assert_eq!(params["timeout"], 30u64);
    // AC6 (Rust path): -p is no longer the provider short; it is a loud
    // tombstone (the one-shot short is spawn-only), never a silent harness.
    let err = build_request(
        "ask",
        &[
            "myagent".to_string(),
            "hi".to_string(),
            "-p".to_string(),
            "claude".to_string(),
        ],
    )
    .unwrap_err();
    assert!(err.contains("-p is not valid here"), "got: {err}");
    assert!(err.contains("--harness/-H"), "got: {err}");
}

/// AC1-HP (follow-up): ask without created prints the reply verbatim (no added newline).

/// AC2-ERR: ask follow-up with empty reply prints empty string (not None).

/// The codex-thread bounded-ask receipt: `reply: null` + in_flight prints
/// the in-flight line, never an empty line that reads as an empty answer.

/// AC3-HP: build_request accepts --from-name, --yolo, --timeout without error.
/// These flags are forwarded to the daemon so `ask` can be called with full
/// Python-parity flag surface without exit 2 (unknown flag).

/// Codex P2 (PR #379): with `ask` unconditionally auto-routed, the binary
/// must accept the Click/Typer `--flag=value` equals form for EVERY
/// value-carrying option. Without the normalization, `--cwd=/repo` /
/// `--timeout=30` / `--from-name=bot` would regress to "unknown flag"
/// instead of reaching the dispatch. The harness axis is `--harness`
/// (wire param `provider`); `--provider=...` is a tombstone (AC3).

/// The retired spawn axes refuse by name in both space- and equals-form: the
/// lane template owns the value, and no spelling may reach the request.
#[test]
fn spawn_flag_rows() {
    let refused = |args: &[&str], flag: &str| {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let err = build_request("spawn", &args).unwrap_err();
        assert!(
            err.contains(&format!("{flag} is retired")),
            "{args:?}: {err}"
        );
    };
    // The harness's own flags typed before the fence are shorthand for the
    // same flag behind it: they join the fenced tail in harness_args.
    let harness_args = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (_m, params) = build_request("spawn", &args).expect("harness shorthand parses");
        params["harness_args"].clone()
    };
    assert_eq!(
        harness_args(&["wk", "--model", "Gemini 3.5 Flash (High)"]),
        serde_json::json!(["--model", "Gemini 3.5 Flash (High)"])
    );
    assert_eq!(
        harness_args(&["wk", "-m", "pro"]),
        serde_json::json!(["--model", "pro"])
    );
    assert_eq!(
        harness_args(&["wk", "--model=pro", "--effort", "high"]),
        serde_json::json!(["--model", "pro", "--effort", "high"])
    );
    assert_eq!(
        harness_args(&["wk", "--permission-mode=bypassPermissions"]),
        serde_json::json!(["--permission-mode", "bypassPermissions"])
    );

    let (_method, short) = build_request(
        "spawn",
        &[
            "reviewer".to_string(),
            "-s".to_string(),
            "reviews".to_string(),
            "-x".to_string(),
            "right".to_string(),
        ],
    )
    .expect("mobile placement aliases must parse");
    assert_eq!(short["squad"], "reviews");
    assert_eq!(short["split"], "right");

    let (_method, long) = build_request(
        "spawn",
        &[
            "reviewer".to_string(),
            "--squad=reviews".to_string(),
            "--split=right".to_string(),
        ],
    )
    .expect("long placement options must parse");
    assert_eq!(short, long);

    // The thread lane (bg on the wire) carries the placement flags only
    // when --portal names the pane the thread hosts: without it the
    // placement has nothing to place, and headless never hosts a session.
    let params = serde_json::json!({"squad": "reviews", "split": "Right"});
    assert_eq!(
        validate_spawn_placement(&params, "bg"),
        Err("--workspace/-s, --split/-x, and --tab on the thread \
             lane need --portal N: a thread hosts no pane until a portal \
             opens one, so the placement has nothing to place"
            .to_string())
    );
    assert_eq!(
        validate_spawn_placement(&serde_json::json!({"portal": 1u8}), "pane"),
        Err(
            "--portal applies only to the thread lane; a pane hosts its \
             own geometry and headless hosts no session at all"
                .to_string()
        )
    );
    assert!(
        validate_spawn_placement(
            &serde_json::json!({"portal": 1u8, "split": "right", "tab": "3"}),
            "bg"
        )
        .is_ok(),
        "portal + placement on the thread lane is the legal combination"
    );

    refused(
        &["wk", "--harness", "claude", "--substrate", "thread"],
        "--substrate",
    );

    let (_m, space) = build_request(
        "spawn",
        &[
            "wk".to_string(),
            "--account".to_string(),
            "readyrule".to_string(),
        ],
    )
    .expect("--account must parse");
    assert_eq!(space["account"], "readyrule");
    let (_m2, eq) = build_request("spawn", &["wk".to_string(), "--account=makers".to_string()])
        .expect("--account= must parse");
    assert_eq!(eq["account"], "makers");

    // --deny-tools is fno's own spelling, not a harness flag, so it still
    // refuses by name; the real claude flags forward.
    refused(&["wk", "--deny-tools", "x"], "--deny-tools");
    assert_eq!(
        harness_args(&[
            "wk",
            "--add-dir=/extra",
            "--agent",
            "reviewer",
            "--tools",
            "Read"
        ]),
        serde_json::json!([
            "--add-dir",
            "/extra",
            "--agent",
            "reviewer",
            "--tools",
            "Read"
        ])
    );
}

// x-d012: --account parses into params (space + equals form) so the spawn
// arm can route an account spawn to the Python resolver instead of erroring
// as an unknown flag.

/// ab-3ff64151 AC1 (Rust-path parity) + x-bab1 AC6: `agents ask` accepts the
/// surviving phone shorts `-c`/`-t` and the global `-Y`, with the harness axis
/// short `-H` (renamed from `-p`). `-p` was the provider short; off spawn it
/// is now a loud tombstone, never silently bound to a harness.

/// ab-3ff64151 AC2 (Rust-path parity): the global-register boolean shorts
/// the client recognizes (`-A` --all, `-F` --force) parse identically to the
/// long forms on the commands that use them. `-J` --json is client-side (not a
/// build_request param) and is covered by the json-detection path.
#[test]
fn parse_rows() {
    let (_m, all_params) = build_request("list", &["-A".to_string()]).expect("-A must parse");
    assert_eq!(all_params["all"], true);
    let (_m, force_params) =
        build_request("rm", &["myagent".to_string(), "-F".to_string()]).expect("-F must parse");
    assert_eq!(force_params["force"], true);

    let (method, _params) = build_request(
        "rm",
        &["myagent".to_string(), "--cross-project".to_string()],
    )
    .expect("the retired --cross-project no-op still parses on rm");
    assert_eq!(method, "agent.rm");
    let (_, plain) = build_request("rm", &["myagent".to_string()]).expect("plain rm must parse");
    assert!(plain.get("cross_project").is_none());
    let (stop_method, _stop_params) = build_request(
        "stop",
        &["myagent".to_string(), "--cross-project".to_string()],
    )
    .expect("the retired --cross-project no-op still parses on stop");
    assert_eq!(stop_method, "agent.stop");
    let err = build_request("list", &["--cross-project".to_string()]).unwrap_err();
    assert!(err.contains("unknown flag: --cross-project"), "got: {err}");

    let args = vec![
        "w1".to_string(),
        "--harness".to_string(),
        "claude".to_string(),
        "--force".to_string(),
        "--no-wait".to_string(),
    ];
    let (_m, params) = build_request("spawn", &args).expect("gate flags must parse");
    assert_eq!(params["force"], true);
    assert_eq!(params["no_wait"], true);

    let forced = gate_flags_from_params(&serde_json::json!({"force": true, "no_wait": true}));
    assert!(forced.force);
    assert!(forced.no_wait);
    let defaults = gate_flags_from_params(&serde_json::json!({}));
    assert!(!defaults.force);
    assert!(!defaults.no_wait);
}

/// The lifecycle commands accept the retired `--cross-project` flag as a
/// silent no-op (d-460ea8c7: the store heal is machine-wide now), so
/// `fno agents rm|stop <id> --cross-project` still PARSES (the old surface
/// died with "unknown flag: --cross-project" before any resolution ran).
/// Commands that never took the flag keep refusing it.

/// x-c5cc: the spawn-gate flags parse on the spawn command (--force already
/// shared with stop/rm; --no-wait is gate-only).

/// Both gate constructions (the daemon-bound codex-thread gate and the
/// shared one) read their flags through `gate_flags_from_params`: a
/// hardcoded `GateFlags { force: false, .. }` refused a `--force` spawn at
/// capacity and made `--no-wait` queue for a slot.

/// AC4-HP: spawn with provider and no --argv succeeds (uses provider-derived argv).

/// Sigma-review (PR #379): the equals-form normalization must NOT touch the
/// `--argv` payload. A downstream tool's `--timeout=5` in the provider
/// command line must survive verbatim, not get split into `--timeout 5`.

// -----------------------------------------------------------------------
// an unflagged spawn takes the harness's default lane
// -----------------------------------------------------------------------

#[test]
fn substrate_rows() {
    // A pane comes only from an explicit lane and gets the interactive
    // host_mode; codex/gemini/agy never mint a session id (claude-only).
    for provider in ["gemini", "agy"] {
        let args = vec![
            "wk".to_string(),
            "--harness".to_string(),
            provider.to_string(),
            "--as".to_string(),
            "pane".to_string(),
        ];
        let (method, params) = build_request("spawn", &args).unwrap();
        assert_eq!(method, "agent.spawn");
        assert_eq!(params["substrate"], "pane", "{provider} keeps the pane");
        assert_eq!(
            params["host_mode"], "interactive",
            "{provider} default-interactive"
        );
        assert!(params.get("session_id").is_none(), "{provider} never mints");
    }

    // An unflagged spawn takes a thread for every harness, as `cmd_spawn`
    // does; admission refuses a thread the harness cannot seat. The resolved
    // lane rides params, so dispatch and the thread-only grant read one value,
    // and a thread gets no host_mode and no mint.
    for provider in ["claude", "codex", "gemini", "agy"] {
        let args = vec![
            "wk".to_string(),
            "--harness".to_string(),
            provider.to_string(),
        ];
        let (_m, params) = build_request("spawn", &args).unwrap();
        assert_eq!(
            params["substrate"], "thread",
            "{provider} defaults to a thread"
        );
        assert!(
            params.get("host_mode").is_none(),
            "{provider}: no host_mode"
        );
        assert!(params.get("session_id").is_none(), "{provider}: no mint");
    }

    // --once is the headless spelling -> no host_mode, no mint, for EVERY
    // provider; substrate=headless.
    for provider in ["claude", "codex", "gemini", "agy"] {
        let args = vec![
            "wk".to_string(),
            "--harness".to_string(),
            provider.to_string(),
            "--once".to_string(),
        ];
        let (_m, params) = build_request("spawn", &args).unwrap();
        assert_eq!(
            params.get("substrate").and_then(|v| v.as_str()),
            Some("headless"),
            "{provider} --once aliases to substrate=headless"
        );
        assert!(
            params.get("host_mode").is_none(),
            "{provider} --once: no host_mode"
        );
        assert!(
            params.get("session_id").is_none(),
            "{provider} --once: no mint"
        );
    }

    let args = vec![
        "--name".to_string(),
        "parity-agent".to_string(),
        "hi".to_string(),
        "--harness".to_string(),
        "codex".to_string(),
        "--once".to_string(),
    ];

    let (method, params) = build_request("spawn", &args).unwrap();

    assert_eq!(method, "agent.spawn");
    assert_eq!(params["name"], "parity-agent");
    assert_eq!(params["message"], "hi");
    assert_eq!(params["provider"], "codex");
    assert_eq!(params["substrate"], "headless");
    assert!(params.get("host_mode").is_none());

    // Every --substrate spelling, the retired bg included, refuses at scan.
    for sub in ["pane", "thread", "headless", "bg"] {
        let err = build_request(
            "spawn",
            &[
                "wk".to_string(),
                "--harness".to_string(),
                "claude".to_string(),
                "--substrate".to_string(),
                sub.to_string(),
            ],
        )
        .unwrap_err();
        assert!(err.contains("--substrate is retired"), "{sub}: {err}");
    }
}

#[test]
fn harness_flag_rows() {
    // x-6de8: --harness/-H is the CLI-binary axis. -H takes a VALUE (harness
    // name) rather than meaning headless.
    for flag in ["--harness", "-H"] {
        let args = vec!["wk".to_string(), flag.to_string(), "codex".to_string()];
        let (_m, params) = build_request("spawn", &args).unwrap();
        assert_eq!(
            params.get("provider").and_then(|v| v.as_str()),
            Some("codex"),
            "{flag} sets provider"
        );
        // -H carries a value now, so it must NOT default the substrate to headless.
        assert_eq!(
            params["substrate"], "thread",
            "{flag}: no headless substrate side effect"
        );
    }

    // On spawn the vendor axis retired with the rest: a harness name or a
    // vendor typed after --provider/-P refuses by name before any routing.
    for v in ["claude", "codex", "zai"] {
        for flag in ["--provider", "-P"] {
            let args = vec!["wk".to_string(), flag.to_string(), v.to_string()];
            let err = build_request("spawn", &args).unwrap_err();
            assert!(err.contains("--provider is retired"), "{flag}: {err}");
        }
    }

    // Off `spawn`, --provider is the retired harness-axis spelling: a
    // tombstone (the CLI binary is --harness/-H), never silently routed.
    let ask = vec![
        "wk".to_string(),
        "hi".to_string(),
        "--provider".to_string(),
        "codex".to_string(),
    ];
    let err = build_request("ask", &ask).unwrap_err();
    assert!(err.contains("split at the axis rename"), "got: {err}");
    assert!(err.contains("--harness/-H"), "got: {err}");

    // x-c5db: the role halves the Python seam passes for a promoted codex
    // thread spawn parse into params in both spellings, and a level outside
    // the ladder refuses before any request is built.
    let (_m, space) = build_request(
        "spawn",
        &[
            "wk".to_string(),
            "--role-level".to_string(),
            "2".to_string(),
            "--role-scope".to_string(),
            "x-aaaa".to_string(),
        ],
    )
    .expect("--role-level must parse");
    assert_eq!(space["role_level"], 2);
    assert_eq!(space["role_scope"], "x-aaaa");
    let (_m2, equals) = build_request(
        "spawn",
        &[
            "wk".to_string(),
            "--role-level=1".to_string(),
            "--role-scope=x-bbbb".to_string(),
        ],
    )
    .expect("the role equals forms must parse");
    assert_eq!(equals["role_level"], 1);
    assert_eq!(equals["role_scope"], "x-bbbb");
    let err = build_request("spawn", &["wk".to_string(), "--role-level=7".to_string()])
        .expect_err("a level outside the ladder refuses");
    assert!(err.contains("level 0-2"), "got: {err}");
}

// -----------------------------------------------------------------------
// cwd forwarding (fix/agents-host-cwd): the daemon is a shared long-lived
// process whose own current_dir is frozen to wherever it was first started,
// so the client must stamp the caller's cwd into daemon-bound spawn/ask
// requests. Without this, a spawn from project A opens the provider in the
// daemon's home project B.
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// x-85fe: canonical-by-default cwd precedence (inverts ab-77b691dc)
//
// effective_worker_cwd encodes: --cwd > --here (caller) > default canonical
// (unresolved canonical -> caller, the safe side). --fresh is an accepted
// no-op alias. It is pure so the precedence is provable without git.
// -----------------------------------------------------------------------

fn pb(s: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(s)
}

#[test]
fn effective_cwd_rows() {
    // No flags: the inverted default lands on canonical (AC1-HP).
    let got = fno_agents::spawn_cwd::effective_worker_cwd(
        None,
        false,
        false,
        Some(pb("/canon")),
        pb("/wt"),
    );
    assert_eq!(got, pb("/canon"));

    // --fresh is an accepted no-op alias: identical to passing nothing, the
    // default already being canonical (AC2-EDGE).
    let with_fresh = fno_agents::spawn_cwd::effective_worker_cwd(
        None,
        true,
        false,
        Some(pb("/canon")),
        pb("/wt"),
    );
    let without = fno_agents::spawn_cwd::effective_worker_cwd(
        None,
        false,
        false,
        Some(pb("/canon")),
        pb("/wt"),
    );
    assert_eq!(with_fresh, without);
    assert_eq!(with_fresh, pb("/canon"));

    // --here is the explicit opt-in to stay in the caller's worktree (AC2-HP).
    let got = fno_agents::spawn_cwd::effective_worker_cwd(
        None,
        false,
        true,
        Some(pb("/canon")),
        pb("/wt"),
    );
    assert_eq!(got, pb("/wt"));

    // Ambiguous / git-missing canonical resolution -> caller cwd, the safe
    // side (AC1-ERR; Failure Modes > Boundaries: never guess canonical).
    let got = fno_agents::spawn_cwd::effective_worker_cwd(None, false, false, None, pb("/wt"));
    assert_eq!(got, pb("/wt"));

    // --cwd is the highest-priority cwd source and wins over --here/--fresh
    // (AC2-ERR; Failure Modes > Invariants).
    let got = fno_agents::spawn_cwd::effective_worker_cwd(
        Some(pb("/explicit")),
        true,
        true,
        Some(pb("/canon")),
        pb("/wt"),
    );
    assert_eq!(got, pb("/explicit"));

    // --fresh / --here / --in-place are plumbed into params for spawn/ask.
    let (_m, p) = build_request(
        "spawn",
        &[
            "w".into(),
            "--harness".into(),
            "claude".into(),
            "--fresh".into(),
        ],
    )
    .unwrap();
    assert_eq!(p["fresh"], Value::Bool(true));
    assert!(p.get("here").is_none());

    let (_m, p) = build_request("ask", &["w".into(), "hi".into(), "--here".into()]).unwrap();
    assert_eq!(p["here"], Value::Bool(true));

    let (_m, p) = build_request("ask", &["w".into(), "hi".into(), "--in-place".into()]).unwrap();
    assert_eq!(p["here"], Value::Bool(true));
}

// -----------------------------------------------------------------------
// (x-9b60) The portal placement trio on the Rust spawn path
// -----------------------------------------------------------------------

/// AC6-HP/AC7-ERR: the runtime that runs accepts the flags the help
/// advertises, and an out-of-range --portal refuses with the range, never
/// the catch-all "unknown flag".
#[test]
fn placement_rows() {
    let (method, p) = build_request(
        "spawn",
        &[
            "w".into(),
            "--portal".into(),
            "1".into(),
            "--tab".into(),
            "3".into(),
            "--at".into(),
            "7".into(),
            "--split".into(),
            "Right".into(),
        ],
    )
    .unwrap();
    assert_eq!(method, "agent.spawn");
    assert_eq!(p["portal"], Value::from(1u8));
    assert_eq!(p["tab"], Value::String("3".into()));
    assert_eq!(p["at"], Value::String("7".into()));
    assert_eq!(p["split"], Value::String("Right".into()));

    for bad in ["999", "abc", "-1", ""] {
        let err = build_request(
            "spawn",
            &["w".into(), "--portal".into(), bad.to_string().into()],
        )
        .unwrap_err();
        assert!(
            err.contains("0-255"),
            "the refusal names the range, never the catch-all: {err}"
        );
        assert!(!err.contains("unknown flag"), "{err}");
    }

    let (_m, p) = build_request(
        "spawn",
        &["w".into(), "--portal=1".into(), "--tab=2".into()],
    )
    .unwrap();
    assert_eq!(p["portal"], Value::from(1u8));
    assert_eq!(p["tab"], Value::String("2".into()));
}

/// The equals form parses the same way: a routed `--portal=1` must not
/// regress to "unknown flag" (the PR 379/371 regression class).

// (canonical_repo_root unit tests live in src/paths.rs, where the shared
// resolver now lives -- ab-77b691dc.)

// -----------------------------------------------------------------------
// Task 3.1: list/reconcile JSON parity + flag parsing
// -----------------------------------------------------------------------

/// AC1-HP: list --cwd and --provider are forwarded to daemon params

/// AC1-HP: --json is NOT forwarded to daemon params (it is a client-side rendering flag)

/// AC2-HP: render_list_json produces the Python-matching shape with correct keys
#[test]
fn render_list_rows() {
    // Simulate the full daemon RPC result so envelope metadata cannot be
    // reconstructed independently by the outward client renderer.
    let result = json!({
        "agents": [{
            "name": "worker-a",
            "harness": "claude",
            "observed_model": {"kind": "observed", "model": "glm-5.2", "samples": 300},
            "short_id": "cl-abc123",
            "session_id": "cl-abc123",
            "cwd": "/home/user/project",
            "created_at": "2026-05-25T00:00:00Z",
            "last_message_at": "2026-05-25T01:00:00Z",
            "status": "live",
            "live_status": null,
            "pid": 4242,
            "last_reconciled_at": "2026-05-25T00:30:00Z",
            "log_path": null,
        }],
        "fields_omitted": ["model", "model_basis"],
        "filters_applied": {"cwd": null, "provider": null, "status": null},
    });
    let output = format_success("list", "", &result, true, false, false)
        .expect("list has an outward renderer");

    let parsed: Value = serde_json::from_str(&output).expect("must be valid JSON");
    // Top-level keys must match Python's render_json shape
    assert!(parsed.get("agents").is_some(), "missing 'agents' key");
    assert!(parsed.get("count").is_some(), "missing 'count' key");
    assert!(
        parsed.get("filters_applied").is_some(),
        "missing 'filters_applied' key"
    );
    assert!(
        parsed.get("schema_version").is_some(),
        "missing 'schema_version' key"
    );
    // ab-098967b4: discovered lane keys are additive; schema bumped to 2.
    assert!(
        parsed.get("discovered_sessions").is_some(),
        "missing 'discovered_sessions' key"
    );
    assert_eq!(parsed["discovered_count"], 0);
    assert_eq!(parsed["schema_version"], 7);
    assert_eq!(parsed["count"], 1);
    assert_eq!(
        parsed["fields_omitted"], result["fields_omitted"],
        "outward envelope must preserve the daemon's exact sorted omission contract"
    );

    // NOT the key-set guard, despite appearances: this row is hand-built in
    // this test, so the list below only asserts against its own input. The
    // real projection lives in the daemon and is pinned to
    // schemas/agents-list-row.json by daemon.rs's
    // `list_row_key_set_matches_shared_contract`. Adding a key here proves
    // nothing about what `fno agents list` emits.
    //
    // The client passes rows through verbatim: the 10 Python parity keys
    // (incl. live_status, retained for back-compat -- AC4-FR) plus the
    // additive Architecture C keys pid + last_reconciled_at (AC4-HP) survive.
    let row = &parsed["agents"][0];
    for key in &[
        "name",
        "harness",
        "observed_model",
        "short_id",
        "session_id",
        "cwd",
        "created_at",
        "last_message_at",
        "status",
        "live_status",
        "log_path",
        "pid",
        "last_reconciled_at",
    ] {
        assert!(row.get(*key).is_some(), "row missing key: {key}");
    }
    assert_eq!(row["pid"], 4242, "pid passes through");
    assert!(row["live_status"].is_null(), "live_status retained as null");

    let agents = json!([]);
    let filters = json!({"cwd": null, "provider": null, "status": null});
    let discovered = vec![json!({
        "handle": "fno-aaaa1111",
        "address": "aaaa1111",
        "short_id": "aaaa1111",
        "session_id": "uuid-1",
        "pid": 4242,
        "cwd": "/Users/x/code/proj",
        "project": "fno",
        "status": "busy",
        "agent": "claude",
    })];
    let out = render_list_json(
        &agents,
        &filters,
        &json!(["model"]),
        &discovered,
        Some(3),
        Some(3),
        None,
        &Value::Null,
    );
    let parsed: Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(parsed["discovered_count"], 1);
    assert_eq!(parsed["discovered_sessions"][0]["handle"], "fno-aaaa1111");
    assert_eq!(parsed["schema_version"], 7);
    // Without a codex probe the key is absent, not null.
    assert!(parsed.get("codex_loaded").is_none());

    let table = fno_agents::list_render::render_list_table(
        &agents,
        &discovered,
        Some(3),
        Some(3),
        &Value::Null,
    );
    assert!(table.contains("DISCOVERED LIVE SESSIONS (1, host-local)"));
    // ADDRESS leads and the alias is demoted to LABEL. The alias led this
    // table for its whole life, so it was the leftmost thing a reader
    // copied, and `<project>-<short8>` is not a mailbox.
    // Anchored on the banner: the registry header above now carries
    // ADDRESS too, so a bare `contains` search finds the wrong line.
    let lines: Vec<&str> = table.lines().collect();
    let banner = lines
        .iter()
        .position(|l| l.contains("DISCOVERED LIVE"))
        .expect("banner present");
    let header = lines[banner + 1];
    assert!(
        header.find("ADDRESS") < header.find("LABEL"),
        "address must lead the discovered lane, got: {header:?}"
    );
    assert!(!table.contains("HANDLE"), "HANDLE was renamed to LABEL");
    assert!(table.contains("aaaa1111"));
    assert!(table.contains("fno-aaaa1111"));
    assert!(table.contains("busy"));

    let agents = json!([]);
    let filters = json!({"cwd": null, "provider": "codex", "status": null});
    let block = fno_agents::codex_inject::loaded_threads_block(Ok(vec![
        fno_agents::codex_inject::LoadedThread {
            session_id: "019f4d0c-full".into(),
            cwd: "/repo".into(),
        },
    ]));
    let out = render_list_json(
        &agents,
        &filters,
        &json!([]),
        &[],
        None,
        None,
        Some(&block),
        &Value::Null,
    );
    let parsed: Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(parsed["codex_loaded"]["available"], true);
    assert_eq!(
        parsed["codex_loaded"]["threads"][0]["session_id"],
        "019f4d0c-full"
    );
    assert_eq!(parsed["codex_loaded"]["threads"][0]["cwd"], "/repo");

    let agents = json!([
        {
            "name": "pane-worker",
            "harness": "claude",
            "harness_session_id": "e6f78b98-e594-47ed-ad81-84f8a78b8bb7",
            "effort": "xhigh",
            "status": "writing",
            "requested_model": "glm-5.3-flash[1m]",
            "model": "glm-5.3-flash[1m]",
            "model_basis": "requested",
            "observed_model": {"kind": "no-transcript"},
            "pr": 2136,
            "pr_basis": "node",
        },
        {
            "name": "bare-worker",
            "harness": "codex",
            "session_id": "019f4d0c-full",
            "effort": null,
            "status": "quiet",
            "observed_model": {"kind": "no-model-yet"},
            "pr": null,
            "pr_basis": "no-node",
        },
        {
            "name": "legacy-claude",
            "harness": "claude",
            "session_id": "abc12345",
            "effort": "high",
            "status": "quiet",
            "observed_model": {"kind": "no-transcript"},
            "pr": null,
            "pr_basis": "no-node",
        }
    ]);
    let table =
        fno_agents::list_render::render_list_table(&agents, &[], Some(3), Some(3), &Value::Null);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>().join(" "),
        "ROW NAME SESSION HARNESS MODEL EFFORT PR AGE LAST MESSAGE STATUS"
    );
    let first = lines[1];
    assert!(first.starts_with("1 "), "ROW is 1-based: {first}");
    assert!(
        first.contains("e6f78b98-e594-47ed-ad81-84f8a78b8bb7"),
        "full session id: {first}"
    );
    assert!(first.contains("glm-5.3-flash[1m] (requested)"), "{first}");
    assert!(
        first.contains("xhigh") && first.contains("#2136"),
        "{first}"
    );
    let second = lines[2];
    assert!(second.starts_with("2 "), "{second}");
    assert!(second.contains("019f4d0c-full"), "{second}");
    assert!(second.contains("- (unrequested)"), "{second}");
    assert!(second.contains("- (no-node)"), "{second}");
    let legacy = lines[3];
    assert_eq!(
        legacy.split_whitespace().nth(2),
        Some("unknown"),
        "SESSION must not show Claude's short transport id: {legacy}"
    );
    assert!(
        !legacy.contains("abc12345"),
        "short Claude transport id must not be shown as full session: {legacy}"
    );

    let agents = json!([
        {
            "name": "sub-worker",
            "requested_model": "glm-5.3-flash[1m]",
            "model": "glm-5.3-flash[1m]",
            "model_basis": "requested",
            "model_substituted": {"requested": "glm-5.3-flash", "observed": "claude-opus-5"},
            "observed_model": {"kind": "observed", "model": "claude-opus-5"},
            "pr": null,
            "pr_basis": "no-pr",
        },
        {
            "name": "graph-worker",
            "requested_model": "gpt-5.6-luna",
            "observed_model": {"kind": "observed", "model": "gpt-5.6-luna"},
            "pr": null,
            "pr_basis": "graph-unreadable",
        }
    ]);
    let table =
        fno_agents::list_render::render_list_table(&agents, &[], Some(2), Some(2), &Value::Null);
    let sub = table.lines().find(|l| l.contains("sub-worker")).unwrap();
    assert!(
        sub.contains("claude-opus-5 (observed; requested glm-5.3-flash[1m])"),
        "{sub}"
    );
    assert!(sub.contains(" none "), "{sub}");
    let graph = table.lines().find(|l| l.contains("graph-worker")).unwrap();
    assert!(graph.contains("gpt-5.6-luna (observed)"), "{graph}");
    assert!(graph.contains("? (graph-unreadable)"), "{graph}");

    let fresh = (chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
    let agents = json!([
        {
            "name": "live-worker",
            "address": "aaaa1111",
            "harness": "claude",
            "short_id": null,
            "session_id": null,
            "cwd": "/home/user/project",
            "created_at": "2026-05-25T00:00:00Z",
            "last_message_at": null,
            "live_status": null,
            "pid": 99,
            "last_reconciled_at": "2026-05-25T00:00:00Z",
            "last_event_at": fresh,
            "last_message": "[tool_use: Bash] on the pytest run",
            "log_path": null
        },
        {
            "name": "gone-worker",
            "address": "bbbb2222",
            "harness": "claude",
            "short_id": null,
            "session_id": null,
            "cwd": "/home/user/other",
            "created_at": "2026-05-25T00:00:00Z",
            "last_message_at": null,
            "live_status": null,
            "pid": null,
            "last_reconciled_at": null,
            "log_path": null
        }
    ]);
    let table =
        fno_agents::list_render::render_list_table(&agents, &[], Some(3), Some(3), &Value::Null);
    let lines: Vec<&str> = table.lines().collect();

    assert!(
        lines[0].contains(" AGE ") && lines[0].contains("LAST MESSAGE"),
        "header must carry both new columns, got: {:?}",
        lines[0]
    );
    let live = lines.iter().find(|l| l.contains("live-worker")).unwrap();
    assert!(live.contains("5s"), "fresh transcript age renders: {live}");
    assert!(
        live.contains("[tool_use: Bash] on the pytest run"),
        "last-turn text renders: {live}"
    );
    // No probe answer: an absent reading, rendered as unread - never `0s`,
    // which would claim the transcript moved seconds ago.
    let gone = lines.iter().find(|l| l.contains("gone-worker")).unwrap();
    assert!(
        gone.contains("unknown"),
        "absent stamp reads unknown: {gone}"
    );
    assert!(
        !gone.contains("0s"),
        "absent stamp never reads fresh: {gone}"
    );

    let agents = json!([
        {
            "name": "any-worker",
            "harness": "claude",
            "status": "unknown",
            "address": null,
            "short_id": null,
            "session_id": null,
            "cwd": "/home/user/proj",
            "created_at": "2026-05-25T00:00:00Z",
            "last_message_at": null,
            "live_status": null,
            "pid": null,
            "last_reconciled_at": null,
            "log_path": null
        }
    ]);
    let table =
        fno_agents::list_render::render_list_table(&agents, &[], Some(43), Some(0), &Value::Null);
    assert!(
        table.contains("truth probe failed: 0 of 43 rows answered"),
        "outage must be named, got: {table}"
    );
    assert!(
        table.contains("unmeasured, not healthy"),
        "the statuses must be disclaimed: {table}"
    );

    let healthy =
        fno_agents::list_render::render_list_table(&agents, &[], Some(43), Some(43), &Value::Null);
    assert!(
        !healthy.contains("truth probe failed"),
        "a healthy page carries no outage line: {healthy}"
    );
}

/// ab-098967b4: render_list_json folds in the discovered lane (additive
/// keys, schema 2); render_list_table appends a distinct DISCOVERED section.

/// A codex-filtered list folds the loaded-thread block into the JSON
/// envelope; the key carries the retired command's exact payload shape.

/// AC1/AC7: the operator's ten columns in order, ROW is the served ordinal,
/// and SESSION is the full id, never truncated. Positive strings, never a
/// shorter output.

/// AC3/AC6: an observation outranks the stored request, and every PR
/// absence names its reason.

/// AGE renders the transcript's newest-activity age and LAST
/// MESSAGE the flattened last-turn text. The registry timestamp this column
/// was wired to for its whole life was null on many rows while the worker
/// was mid-sentence, so a "last message" column that never showed a
/// message. Absent readings render `unknown` / `-`, never a fresh age: an
/// unread transcript is not health.
///
/// The state fixture key is omitted on purpose (see the address test
/// above): the identifier ratchet in check-plan-rung-authority counts over
/// inline tests too, and the assertions below never read it.

/// x-e3cc: a total probe outage is named in the table, not left to read as a
/// wall of `unknown` statuses. The daemon's stderr WARN is write-only; this
/// line is the receipt the operator actually sees.

/// AC4-HP: render_reconcile_json produces the Python-matching key set

// -----------------------------------------------------------------------
// x-f1ab / x-90a9 task 0.1: the spawn seam gate (spawn_needs_python_seam)
// -----------------------------------------------------------------------

#[test]
fn seam_rows() {
    // AC22 (positive gate marker): an argv that never crossed the Python seam
    // parses with no defaults_applied param, and the gate is true.
    let (_m, params) = build_request(
        "spawn",
        &[
            "--name".into(),
            "w".into(),
            "--harness".into(),
            "claude".into(),
            "/target x-90a9".into(),
        ],
    )
    .expect("plain spawn parses");
    assert!(params.get("defaults_applied").is_none());
    assert!(spawn_needs_python_seam(&params));

    // AC23/AC27 (positive dispatch markers): both spellings parse, the gate
    // is false, and the marker token is consumed - it never reaches the seed.
    for token in ["--defaults-applied", "--defaults-applied=enforced"] {
        let (_m, params) = build_request(
            "spawn",
            &[
                token.into(),
                "--name".into(),
                "w".into(),
                "--harness".into(),
                "codex".into(),
                "--".into(),
                "/target x-90a9".into(),
            ],
        )
        .unwrap_or_else(|e| panic!("{token} parses: {e}"));
        assert!(!spawn_needs_python_seam(&params), "{token} gates");
        let message = params["message"].as_str().unwrap_or("");
        assert_eq!(message, "/target x-90a9", "{token} must not touch the seed");
        assert!(!message.contains("defaults-applied"), "{token} leaks");
    }
    let (_m, params) = build_request(
        "spawn",
        &[
            "--defaults-applied=unenforced".into(),
            "--name".into(),
            "w".into(),
            "--once".into(),
            "seed".into(),
        ],
    )
    .expect("unenforced marker parses");
    assert_eq!(params["defaults_applied"], "unenforced");
    assert!(!spawn_needs_python_seam(&params));

    // AC24 (bridge case): the exact argv shape rust_spawn.py builds - marker
    // straight after the command, seed behind the fence. One parse, gate false,
    // message intact: no re-entry recursion and no prompt contamination.
    let (_m, params) = build_request(
        "spawn",
        &[
            "--defaults-applied=unenforced".into(),
            "--name".into(),
            "probe-w".into(),
            "--harness".into(),
            "codex".into(),
            "--cwd".into(),
            "/tmp".into(),
            "--node".into(),
            "x-90a9".into(),
            "--slug".into(),
            "probe-slug".into(),
            "--plan=/plans/p.md".into(),
            "--".into(),
            "/target x-90a9".into(),
        ],
    )
    .expect("bridge argv parses");
    assert!(!spawn_needs_python_seam(&params));
    assert_eq!(params["message"], "/target x-90a9");
    assert_eq!(params["node"], "x-90a9");
    assert_eq!(params["slug"], "probe-slug");
    assert_eq!(params["plan"], "/plans/p.md");

    // A typo'd marker must refuse loudly, not degrade to unenforced: the
    // value is the binary's only record of the seam's policy decision.
    let err = build_request(
        "spawn",
        &[
            "--defaults-applied=bogus".into(),
            "--name".into(),
            "w".into(),
        ],
    )
    .expect_err("bogus verdict refuses");
    assert!(err.contains("--defaults-applied"), "{err}");
}

// -----------------------------------------------------------------------
// x-6484: the status payload's canonical arms_attention selection
// -----------------------------------------------------------------------

fn attention_row(
    arm: &str,
    evidence: fno_agents::tick_ledger::ProducerEvidence,
) -> fno_agents::tick_ledger::ArmStatus {
    fno_agents::tick_ledger::ArmStatus {
        arm: arm.to_string(),
        scheduler: None,
        last_ts: None,
        age_s: None,
        acted: None,
        skip_reason: None,
        detail: None,
        interval_s: 300,
        producer_evidence: evidence,
        stale: false,
        failing: false,
        failing_for_s: None,
        cause: None,
        line: String::new(),
        repair: None,
        heal: None,
        upstream: None,
        arm_key: None,
        arm_value: None,
        reader: None,
        starved: false,
        retries: Vec::new(),
    }
}

/// AC1/AC2/AC4/AC5 at the payload seam: `arms_attention` is selected by the
/// one Rust predicate - an unobserved arm is in it, an observed fresh no-op
/// is not, an observed stale or failed arm is.
#[test]
fn status_rows() {
    let unobserved = attention_row(
        "a_unobserved",
        fno_agents::tick_ledger::ProducerEvidence::Unobserved,
    );
    let mut fresh_no_op = attention_row(
        "a_fresh_no_op",
        fno_agents::tick_ledger::ProducerEvidence::Observed,
    );
    fresh_no_op.acted = Some(0);
    fresh_no_op.skip_reason = Some("no_work".to_string());
    let mut stale = attention_row(
        "a_stale",
        fno_agents::tick_ledger::ProducerEvidence::Observed,
    );
    stale.stale = true;
    let mut failing = attention_row(
        "a_failing",
        fno_agents::tick_ledger::ProducerEvidence::Observed,
    );
    failing.failing = true;
    let rows = vec![unobserved, fresh_no_op, stale, failing];

    let payload = degraded_status_payload(&rows);
    assert_eq!(payload["schema_version"], 1);
    assert!(payload["daemon"].is_null());
    assert_eq!(payload["arms"].as_array().map(Vec::len), Some(4));
    let attention: Vec<&str> = payload["arms_attention"]
        .as_array()
        .expect("arms_attention array")
        .iter()
        .filter_map(|r| r["arm"].as_str())
        .collect();
    assert!(attention.contains(&"a_unobserved"), "{attention:?}");
    assert!(!attention.contains(&"a_fresh_no_op"), "{attention:?}");
    assert!(attention.contains(&"a_stale"), "{attention:?}");
    assert!(attention.contains(&"a_failing"), "{attention:?}");

    let payload = degraded_status_payload(&[]);
    let stuck = &payload["stuck_work"];
    assert!(stuck.is_object(), "{stuck}");
    if let Some(err) = stuck.get("error").and_then(|e| e.as_str()) {
        let lines = fno_agents::stuck_work::render_lines(stuck);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("stuck work: unreadable"), "{err}");
    } else {
        assert!(stuck["hung_commands"].is_array(), "{stuck}");
        assert!(stuck["dead_holders"].is_array(), "{stuck}");
    }

    let value = serde_json::json!({
        "hung_commands": ["hung command pid 7 1h fno backlog advance (over 1800s)"],
        "dead_holders": ["dead holder flight:x holder h pid 9 absent held 10m"],
    });
    let lines = fno_agents::stuck_work::render_lines(&value);
    assert_eq!(lines[0], "stuck work:");
    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[1],
        "  hung command pid 7 1h fno backlog advance (over 1800s)"
    );
    let clean = serde_json::json!({"hung_commands": [], "dead_holders": []});
    assert!(fno_agents::stuck_work::render_lines(&clean).is_empty());
    let err = serde_json::json!({"error": "ps exited 1"});
    let lines = fno_agents::stuck_work::render_lines(&err);
    assert_eq!(lines, vec!["stuck work: unreadable (ps exited 1)"]);
}

/// AC4: the degraded payload carries the stuck_work read beside
/// arms_attention, whatever the machine answers - the two finding lists on a
/// readable host, the error shape on a blind one.

/// AC4-HP/EDGE at the render seam: findings print as a stuck work block,
/// a clean read prints nothing, an error read prints its reason.

// -----------------------------------------------------------------------
// x-1961: `resume` inside the client runtime must not panic
// -----------------------------------------------------------------------

/// Saves three process-global env vars and restores them on Drop, so a
/// panic inside the test body (the pre-fix red this test guards) cannot
/// leak the mutated environment to tests running after it.
struct ResumeEnvGuard {
    home: Option<std::ffi::OsString>,
    codex_home: Option<std::ffi::OsString>,
    path: Option<std::ffi::OsString>,
}

impl Drop for ResumeEnvGuard {
    fn drop(&mut self) {
        match &self.home {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }
        match &self.codex_home {
            Some(v) => std::env::set_var("CODEX_HOME", v),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match &self.path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// Serializes the env-mutating resume tests inside THIS test binary. The
/// lib-side helper (`claims::test_env_lock`) is cfg(test) of the lib crate,
/// so the bin's test build cannot see it.
fn resume_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// main block_ons `run`, and a codex thread row's wake builds its own
/// runtime and block_ons again (`resume_wake.rs`). Drive the command through
/// the same entry main uses, inside a tokio runtime: it must refuse 16
/// (`no-daemon`), never panic with "Cannot start a runtime from within a
/// runtime".
#[test]
fn resume_through_run_inside_a_runtime_refuses_16_without_panicking() {
    // Serializes against the other env-mutating resume tests (shared
    // FNO_AGENTS_HOME / CODEX_HOME / PATH mutation, parallel threads).
    let _env = resume_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let home_dir = temp.path().join("agents-home");
    std::fs::create_dir_all(&home_dir).unwrap();
    // The recorded cwd must exist: run_resume refuses exit 13 on a gone
    // cwd before the wake route ever runs.
    let cwd_dir = temp.path().join("cwd");
    std::fs::create_dir_all(&cwd_dir).unwrap();
    std::fs::write(
        home_dir.join("registry.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "agents": [
                {
                    "name": "w1",
                    "harness": "codex",
                    "substrate": "thread",
                    "cwd": cwd_dir.display().to_string(),
                    "log_path": "w1.log",
                    "harness_session_id": "01a09790-5bd4-7f70-af15-f4884e8dd554"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    // The resume argv resolves before the wake route, so `codex` must be
    // on PATH (the stub never executes on the thread-delivery arm).
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("codex");
    std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _restore = ResumeEnvGuard {
        home: std::env::var_os("FNO_AGENTS_HOME"),
        codex_home: std::env::var_os("CODEX_HOME"),
        path: std::env::var_os("PATH"),
    };
    let mut stub_path = std::ffi::OsString::from(&bin);
    if let Some(previous) = std::env::var_os("PATH") {
        stub_path.push(":");
        stub_path.push(previous);
    }
    std::env::set_var("FNO_AGENTS_HOME", &home_dir);
    // An empty CODEX_HOME has no daemon socket, so the delivery refuses.
    std::env::set_var("CODEX_HOME", temp.path());
    std::env::set_var("PATH", &stub_path);
    let code = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(vec!["resume".into(), "w1".into()]));
    assert_eq!(code, 16);
}

/// `resume <predecessor uuid>` on a live claude row refuses with
/// the spawn --resume remedy and launches nothing - it never re-enters the
/// row's current session. The refusal precedes the route and PATH work, so
/// no stub is needed; the codex runtime test above covers the dispatch
/// path this rides. The detector's scoping contract is tested beside it,
/// in resolve_tier's exact_id_tests.
#[test]
fn resume_by_predecessor_full_id_refuses_13() {
    // Serialize against the sibling resume tests that mutate the same env
    // vars: parallel threads swapping FNO_AGENTS_HOME read each other's
    // registries mid-test.
    let _env = resume_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let home_dir = temp.path().join("agents-home");
    std::fs::create_dir_all(&home_dir).unwrap();
    let cwd_dir = temp.path().join("cwd");
    std::fs::create_dir_all(&cwd_dir).unwrap();
    std::fs::write(
        home_dir.join("registry.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "agents": [
                {
                    "name": "w1",
                    "harness": "claude",
                    "substrate": "pane",
                    "cwd": cwd_dir.display().to_string(),
                    "log_path": "w1.log",
                    "harness_session_id": "77770001-2222-3333-4444-555555555555",
                    "predecessor_session_ids": ["77770003-2222-3333-4444-555555555555"]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let _restore = ResumeEnvGuard {
        home: std::env::var_os("FNO_AGENTS_HOME"),
        codex_home: std::env::var_os("CODEX_HOME"),
        path: std::env::var_os("PATH"),
    };
    std::env::set_var("FNO_AGENTS_HOME", &home_dir);
    let code = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(vec![
            "resume".into(),
            "77770003-2222-3333-4444-555555555555".into(),
        ]));
    assert_eq!(code, 13);
}

// -----------------------------------------------------------------------
// the client-side gate receives the route provider
// -----------------------------------------------------------------------

//! `-H footnote --substrate thread` against real processes: the worker
//! binary hosts the session, a stand-in `footnote acp` speaks ACP stdio, and
//! every steer is a typed message, never a keystroke.

use fno_agents::footnote_harness::thread::{deliver_rpc, dispatch_thread, maybe_ask_host};
use fno_agents::paths::AgentsHome;
use fno_agents::protocol::Request;
use fno_agents::state::{load_registry, update_registry};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

/// The stand-in reads the spec's id, opens that session, takes one prompt,
/// asks once, and holds the turn until a cancel arrives. Each frame it reads
/// lands in a capture file.
const FAKE_ACP: &str = r#"#!/bin/sh
sid=$(sed -n 's/.*"fno_id":"\([^"]*\)".*/\1/p' "$2")
read -r init; printf '%s\n' "$init" > "$CAP/init.json"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"footnote"}}}'
read -r new
printf '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"%s"}}\n' "$sid"
read -r prompt; printf '%s\n' "$prompt" > "$CAP/prompt.json"
printf '%s\n' '{"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{"toolCall":{"title":"Write: a.txt"},"options":[{"optionId":"allow_once","kind":"allow_once"},{"optionId":"reject_once","kind":"reject_once"}]}}'
read -r answer; printf '%s\n' "$answer" > "$CAP/answer.json"
read -r cancel; printf '%s\n' "$cancel" > "$CAP/cancel.json"
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}'
read -r eof
"#;

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn captured(dir: &Path, name: &str) -> Value {
    let path = dir.join(name);
    wait_for(name, || {
        std::fs::read_to_string(&path).is_ok_and(|t| t.ends_with('\n'))
    });
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// A spawn with no footnote binary refuses with the install command and
/// leaves no row. A thread spawn registers the fnh session id and a live
/// host pid. Mail reaches fnh as `session/prompt` whose origin names the
/// sender's full session id and the message id. `--answer deny` rejects the
/// pending ask. A stop SIGINT to the host's group sends `session/cancel`,
/// closes the socket and marks the row Exited.
#[test]
fn footnote_thread_takes_mail_an_answer_and_a_stop_over_acp_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let cap = tmp.path().join("cap");
    std::fs::create_dir_all(&cap).unwrap();
    let fake = tmp.path().join("footnote");
    std::fs::write(&fake, FAKE_ACP).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (k, v) in [
        (
            "FNO_AGENTS_HOME",
            tmp.path().join("agents").to_str().unwrap(),
        ),
        (
            "FNO_AGENTS_WORKER_BIN",
            env!("CARGO_BIN_EXE_fno-agents-worker"),
        ),
        ("FNO_FOOTNOTE_BIN", "/nonexistent/footnote"),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
        ("CAP", cap.to_str().unwrap()),
    ] {
        std::env::set_var(k, v);
    }
    let home = AgentsHome::from_env();
    let cwd = tmp.path();
    let missing = dispatch_thread(
        &home,
        "fx-gone",
        "",
        "tester",
        cwd,
        Some("glm-test"),
        &json!({}),
    );
    assert_eq!(missing.exit_code, 2, "{}", missing.stderr);
    assert!(
        missing.stderr.contains("cargo install --locked"),
        "{}",
        missing.stderr
    );
    assert!(load_registry(&home.registry_json())
        .unwrap()
        .find("fx-gone")
        .is_none());

    std::env::set_var("FNO_FOOTNOTE_BIN", &fake);
    let spawned = dispatch_thread(
        &home,
        "fx-thread",
        "",
        "tester",
        cwd,
        Some("glm-test"),
        &json!({}),
    );
    assert_eq!(spawned.exit_code, 0, "{}", spawned.stderr);
    let receipt: Value = serde_json::from_str(spawned.stdout.trim()).unwrap();
    let sid = receipt["session_id"].as_str().unwrap().to_string();
    let row = load_registry(&home.registry_json())
        .unwrap()
        .find("fx-thread")
        .cloned()
        .unwrap();
    let pid = row.pid.unwrap();
    assert_eq!(receipt["host_pid"], pid);
    assert_eq!(row.substrate.as_deref(), Some("thread"));
    assert_eq!(row.harness_session_id.as_deref(), Some(sid.as_str()));
    // SAFETY: signal 0 only probes that the pid exists.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
    let init = captured(&cap, "init.json");
    assert_eq!(
        init["params"]["clientCapabilities"]["_meta"]["footnote/records"],
        true
    );

    // The sender's row holds its full id; the RPC carries only its name.
    let lead_id = "0b73a1c0-6bd0-4bf7-a30f-9b1fddac1e58";
    // A clone keeps none of the thread row's handles, or the registry refuses
    // the collision.
    let mut lead = row.clone();
    lead.name = "lead".into();
    lead.short_id = String::new();
    lead.session_id = Some(lead_id.into());
    lead.harness_session_id = None;
    lead.fno_id = None;
    update_registry(&home.registry_json(), |reg| {
        reg.entries.push(lead.clone());
        true
    })
    .unwrap();
    let body = "`@lead · fmail-0123456789ab`\nhello";
    let params = json!({"name": "fx-thread", "body": body, "from_name": "lead"});
    let delivered = deliver_rpc(&home, &Request::new(7, "agent.deliver", params));
    let result = delivered.result().cloned().unwrap_or_default();
    assert_eq!(result["delivered"], true, "{result}");
    let prompt = captured(&cap, "prompt.json");
    let origin = &prompt["params"]["_meta"]["footnote/origin"];
    assert_eq!(origin["kind"], "mail");
    assert_eq!(origin["sender_session_id"], lead_id);
    assert_eq!(origin["message_id"], "fmail-0123456789ab");
    assert_eq!(prompt["params"]["sessionId"], sid.as_str());
    assert!(prompt["params"]["prompt"][0]["text"]
        .as_str()
        .unwrap()
        .ends_with("hello"));

    let socket = row.messaging_socket_path.clone().unwrap();
    let status = || {
        fno_agents::footnote_harness::thread::host_call(
            Path::new(&socket),
            &json!({"op": "status"}),
        )
    };
    wait_for("the ask", || status().is_ok_and(|s| !s["ask"].is_null()));
    assert_eq!(maybe_ask_host(&row, &json!({"answer": "deny"})), Some(0));
    let answer = captured(&cap, "answer.json");
    assert_eq!(answer["id"], 90);
    assert_eq!(answer["result"]["outcome"]["optionId"], "reject_once");

    // What `fno agents stop` sends a footnote row's pid group.
    // SAFETY: the host called setsid, so its pid is its group id.
    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGINT) };
    let cancel = captured(&cap, "cancel.json");
    assert_eq!(cancel["method"], "session/cancel");
    assert_eq!(cancel["params"]["sessionId"], sid.as_str());
    wait_for("the row to exit", || {
        load_registry(&home.registry_json())
            .ok()
            .and_then(|r| {
                r.find("fx-thread")
                    .map(|e| e.status == fno_agents::AgentStatus::Exited)
            })
            .unwrap_or(false)
    });
    assert!(!Path::new(&socket).exists());
    let mut code = 0;
    // SAFETY: reaps the host this process started.
    unsafe { libc::waitpid(pid as libc::pid_t, &mut code, 0) };
}

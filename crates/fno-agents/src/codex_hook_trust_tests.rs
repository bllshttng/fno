// Tests for codex_hook_trust: a fake app-server daemon over a unix socket.
//
// `check()`/`trust()`/`run()` are never called here: they would ensure the
// LIVE daemon and write trust rows on this machine. Every test drives
// `trust_flow` (which takes the socket) against a fake server, plus the pure
// helpers.

use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio_tungstenite::{accept_async, WebSocketStream};

use super::*;

/// The row shape fno reads: camelCase wire keys on `hooks/list`.
fn hook_row(key: &str, plugin: &str, status: &str, enabled: bool) -> Value {
    json!({
        "key": key,
        "eventName": "session_start",
        "matcher": null,
        "timeoutSec": 3,
        "pluginId": plugin,
        "enabled": enabled,
        "currentHash": "sha256:decl/sha256:content",
        "trustStatus": status,
        "sourcePath": "/x/hooks/codex-hooks.json",
        "source": "plugin",
        "isManaged": false,
        "displayOrder": 0,
    })
}

/// Complete the app-server handshake on the server side, read one request,
/// and hand back the socket so the test scripts the reply.
async fn accept_initialized(listener: UnixListener) -> WebSocketStream<UnixStream> {
    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = accept_async(stream).await.unwrap();
    let init = ws.next().await.unwrap().unwrap().into_text().unwrap();
    let init: Value = serde_json::from_str(&init).unwrap();
    assert_eq!(init["method"], "initialize");
    ws.send(Message::Text(r#"{"id":"init","result":{}}"#.into()))
        .await
        .unwrap();
    let initialized = ws.next().await.unwrap().unwrap().into_text().unwrap();
    let initialized: Value = serde_json::from_str(&initialized).unwrap();
    assert_eq!(initialized["method"], "initialized");
    ws
}

/// One request frame from the fake server side, parsed.
async fn next_request(ws: &mut WebSocketStream<UnixStream>) -> Value {
    let raw = ws.next().await.unwrap().unwrap().into_text().unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[tokio::test]
async fn trust_check_counts_every_state_over_a_fake_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("codex.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_initialized(listener).await;
        let list = next_request(&mut ws).await;
        assert_eq!(list["method"], "hooks/list");
        // Two roots: the walk flattens hooks across every data entry.
        ws.send(Message::Text(
            json!({"id":"fno-hook-trust-list","result":{"data":[
                {"hooks":[
                    hook_row("fno@fno:session_start:0", "fno@fno", "trusted", true),
                    hook_row("fno@fno:user_prompt_submit:0", "fno@fno", "trusted", true)
                ]},
                {"hooks":[
                    hook_row("fno@fno:stop:0", "fno@fno", "modified", true),
                    hook_row("fno@fno:pre_tool_use:0", "fno@fno", "untrusted", true),
                    hook_row("fno@fno:session_end:0", "fno@fno", "modified", false),
                    hook_row("x", "other@x", "untrusted", true)
                ]}
            ]}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    });
    let report = super::trust_flow(&socket, Path::new("/tmp"), false)
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(report.status, "untrusted");
    assert_eq!(report.checked, 4);
    assert_eq!(report.trusted, 2);
    assert_eq!(report.modified, 1);
    assert_eq!(report.untrusted, 1);
    assert_eq!(report.not_trusted, 2);
    assert_eq!(report.disabled, 1);
    assert!(report.reason.is_none());
}

#[tokio::test]
async fn trust_writes_only_untrusted_enabled_fno_hooks_then_rereads() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("codex.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_initialized(listener).await;
        let list = next_request(&mut ws).await;
        assert_eq!(list["method"], "hooks/list");
        ws.send(Message::Text(
            json!({"id":"fno-hook-trust-list","result":{"data":[{"hooks":[
                hook_row("fno@fno:session_start:0", "fno@fno", "trusted", true),
                hook_row("fno@fno:stop:0", "fno@fno", "modified", true),
                hook_row("fno@fno:pre_tool_use:0", "fno@fno", "untrusted", true),
                hook_row("fno@fno:session_end:0", "fno@fno", "modified", false),
                hook_row("x", "other@x", "untrusted", true)
            ]}]}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let write = next_request(&mut ws).await;
        assert_eq!(write["id"], "fno-hook-trust-write");
        assert_eq!(write["method"], "config/batchWrite");
        let edit = &write["params"]["edits"][0];
        assert_eq!(edit["keyPath"], "hooks.state");
        assert_eq!(edit["mergeStrategy"], "upsert");
        // Only the enabled, not-yet-trusted fno rows: never the disabled row,
        // the already-trusted one, or another plugin's row.
        assert_eq!(
            edit["value"],
            json!({
                "fno@fno:stop:0": {"trusted_hash": "sha256:decl/sha256:content"},
                "fno@fno:pre_tool_use:0": {"trusted_hash": "sha256:decl/sha256:content"}
            })
        );
        assert_eq!(write["params"]["reloadUserConfig"], true);
        ws.send(Message::Text(
            r#"{"id":"fno-hook-trust-write","result":{}}"#.into(),
        ))
        .await
        .unwrap();
        let relist = next_request(&mut ws).await;
        assert_eq!(relist["method"], "hooks/list");
        ws.send(Message::Text(
            json!({"id":"fno-hook-trust-list","result":{"data":[{"hooks":[
                hook_row("fno@fno:session_start:0", "fno@fno", "trusted", true),
                hook_row("fno@fno:stop:0", "fno@fno", "trusted", true),
                hook_row("fno@fno:pre_tool_use:0", "fno@fno", "trusted", true)
            ]}]}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    });
    let report = super::trust_flow(&socket, temp.path(), true).await.unwrap();
    server.await.unwrap();
    assert_eq!(report.status, "ok");
    assert_eq!(report.checked, 3);
    assert_eq!(report.trusted, 3);
    assert_eq!(report.not_trusted, 0);
    assert!(report.reason.is_none());
}

#[tokio::test]
async fn refused_hooks_list_fails_the_flow_naming_the_method() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("codex.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_initialized(listener).await;
        let list = next_request(&mut ws).await;
        assert_eq!(list["method"], "hooks/list");
        ws.send(Message::Text(
            json!({"id":"fno-hook-trust-list","error":{"code":-32601,"message":"method not found"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    });
    let error = super::trust_flow(&socket, Path::new("/tmp"), false)
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(
        error.contains("hooks/list refused"),
        "refusal must name the method: {error}"
    );
}

#[tokio::test]
async fn an_empty_hooks_list_reports_ok_with_zero_counts() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("codex.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_initialized(listener).await;
        let list = next_request(&mut ws).await;
        assert_eq!(list["method"], "hooks/list");
        ws.send(Message::Text(
            json!({"id":"fno-hook-trust-list","result":{"data":[]}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    });
    let report = super::trust_flow(&socket, Path::new("/tmp"), false)
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(report.status, "ok");
    assert_eq!(report.checked, 0);
    assert_eq!(report.trusted, 0);
    assert_eq!(report.not_trusted, 0);
    assert_eq!(report.disabled, 0);
    assert!(report.reason.is_none());
}

#[test]
fn fno_rows_filter_to_fno_plugin_rows_with_wire_defaults() {
    let parsed = super::fno_rows(&[
        hook_row("k1", "fno@fno", "trusted", true),
        hook_row("k2", "other@x", "untrusted", true),
        // No key/currentHash: the row is unreadable and dropped.
        json!({"pluginId": "fno@fno"}),
        // Missing trustStatus/enabled read as the wire defaults.
        json!({"pluginId": "fno@fno", "key": "k4", "currentHash": "h4"}),
    ]);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].key, "k1");
    assert_eq!(parsed[0].trust_status, "trusted");
    assert!(parsed[0].enabled);
    assert_eq!(parsed[1].key, "k4");
    assert_eq!(parsed[1].trust_status, "unknown");
    assert!(parsed[1].enabled);
}

#[test]
fn counts_bucket_every_trust_state() {
    let empty = HookTrustReport::counts(&[]);
    assert_eq!(empty.status, "ok");
    assert_eq!(empty.checked, 0);

    let report = HookTrustReport::counts(&super::fno_rows(&[
        hook_row("a", "fno@fno", "trusted", true),
        hook_row("b", "fno@fno", "modified", true),
        hook_row("c", "fno@fno", "untrusted", true),
        hook_row("d", "fno@fno", "unknown", true),
        hook_row("e", "fno@fno", "modified", false),
    ]));
    assert_eq!(report.status, "untrusted");
    assert_eq!(report.trusted, 1);
    assert_eq!(report.modified, 1);
    assert_eq!(report.untrusted, 2);
    assert_eq!(report.not_trusted, 3);
    assert_eq!(report.checked, 4);
    assert_eq!(report.disabled, 1);
}

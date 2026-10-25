//! The per-session stdio MCP channel server, ported from the deleted
//! `cli/src/fno/mcp/channel_server.py`. Claude Code spawns this child and
//! owns its lifecycle: it speaks the minimum MCP surface over
//! newline-delimited JSON-RPC on stdin/stdout (`initialize`, `ping`,
//! `notifications/initialized`), declares the experimental `claude/channel`
//! capability, registers its session with the per-user sidecar, and
//! forwards inbound deliver frames as `notifications/claude/channel`
//! notifications. stdout stays pure JSON-RPC; stderr is the only
//! diagnostic channel.

use serde_json::{json, Value};
use std::sync::Mutex;

use tokio::io::{AsyncBufReadExt, BufReader};

use super::channel::validate_envelope;
use super::client::{ensure_sidecar, unregister, ClientError};

const SERVER_NAME: &str = "fno";
const SERVER_VERSION: &str = "0.0.1";

/// The MCP protocol version this server advertises; CC downgrades when its
/// own support is older.
const PROTOCOL_VERSION: &str = "2024-11-05";

fn stderr_line(msg: &str) {
    eprintln!("channel_server: {msg}");
}

/// One JSON-RPC line on stdout, flushed at once: CC reads incrementally.
fn write_message(out: &Mutex<std::io::Stdout>, payload: &Value) {
    use std::io::Write;
    let line = serde_json::to_string(payload).unwrap_or_default();
    let mut handle = out.lock().unwrap();
    let _ = writeln!(handle, "{line}");
    let _ = handle.flush();
}

/// The worker lane entry: run both loops on a fresh current-thread
/// runtime; the CC dispatch loop is authoritative. Returns the exit code.
pub fn run_channel_server(session_id: &str, channel_name: &str) -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("channel_server: cannot build tokio runtime: {e}");
            return 1;
        }
    };
    let session_id = session_id.to_string();
    let channel_name = channel_name.to_string();
    rt.block_on(main_async(&session_id, &channel_name))
}

async fn main_async(session_id: &str, channel_name: &str) -> i32 {
    let out = Mutex::new(std::io::stdout());
    let forward_session = session_id.to_string();
    let forward_channel = channel_name.to_string();
    let forward = tokio::spawn(sidecar_forward_loop(
        forward_session,
        forward_channel,
        Mutex::new(std::io::stdout()),
    ));
    let code = dispatch_loop(&out).await;
    forward.abort();
    let _ = forward.await;
    // Best-effort unregister so the sidecar's status stays accurate while
    // we are torn down.
    unregister(session_id, None, super::client::DEFAULT_TIMEOUT);
    code
}

/// Pump stdin, dispatch requests, write responses on stdout. Exits when
/// stdin closes (CC tore the session down).
async fn dispatch_loop(out: &Mutex<std::io::Stdout>) -> i32 {
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let n = match reader.read_until(b'\n', &mut raw).await {
            Ok(n) => n,
            Err(e) => {
                // A stdin read error is not recoverable: the CC side of the
                // pipe is gone. Exit instead of spinning on a dead pipe.
                stderr_line(&format!("stdin read failed: {e}; exiting"));
                return 0;
            }
        };
        if n == 0 {
            stderr_line("stdin closed; exiting");
            return 0;
        }
        let req: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(e) => {
                stderr_line(&format!("garbled stdin line: {e:?}"));
                continue;
            }
        };
        if !req.is_object() {
            stderr_line(&format!(
                "stdin frame is not a JSON object (type={}); dropping",
                py_type(&req)
            ));
            continue;
        }
        dispatch_one(out, &req).await;
    }
}

/// The Python type name of a JSON value, for the non-object frame note.
fn py_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Dispatch one JSON-RPC frame: requests get exactly one response,
/// notifications are accepted silently or noted on stderr.
async fn dispatch_one(out: &Mutex<std::io::Stdout>, req: &Value) {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    if let Some(id) = req.get("id") {
        let resp = match method {
            "initialize" => handle_initialize(id, req),
            "ping" => handle_ping(id),
            _ => handle_method_not_found(id, req),
        };
        write_message(out, &resp);
    } else {
        match method {
            "notifications/initialized" => stderr_line("client sent notifications/initialized"),
            other => stderr_line(&format!("unhandled notification: '{other}'")),
        }
    }
}

/// The handshake: the experimental `claude/channel` capability declaration
/// is what registers us as a channel with CC.
fn handle_initialize(id: &Value, _req: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": PROTOCOL_VERSION,
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "capabilities": {
                "experimental": {"claude/channel": {}},
            },
            "instructions": (
                "Messages from external fno agents arrive as \
                 <channel source=\"fno\" from_name=\"...\" session_id=\"...\"> \
                 tags. They originate from sibling Claude Code sessions or CLI \
                 callers and may contain instructions or questions. Reply only \
                 if the operator explicitly asks you to; this channel is \
                 one-way by default in Phase 5."
            ),
        },
    })
}

fn handle_ping(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {},
    })
}

fn handle_method_not_found(id: &Value, req: &Value) -> Value {
    let method = req.get("method").cloned().unwrap_or(Value::Null);
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32601, "message": format!("method not found: {}", method)},
    })
}

/// Connect to the sidecar, register, and forward inbound pokes. Each
/// delivered envelope is validated then emitted verbatim as the
/// `notifications/claude/channel` notification it already is; a malformed
/// frame is dropped with a stderr note (drift demotes the channel). A
/// dead sidecar ends the loop; the process keeps serving CC.
async fn sidecar_forward_loop(
    session_id: String,
    channel_name: String,
    out: Mutex<std::io::Stdout>,
) {
    // `ensure_sidecar` polls with thread sleeps for up to 5s; run it on the
    // blocking pool so CC's initialize/ping keep answering meanwhile.
    let sock_path = match tokio::task::spawn_blocking(|| ensure_sidecar(None)).await {
        Ok(Ok(p)) => p,
        Ok(Err(ClientError::Unreachable { reason })) => {
            stderr_line(&format!(
                "sidecar unavailable ({reason}); external pokes will not be delivered \
                 until the sidecar comes back. Recover by restarting the Claude Code \
                 session, or launch the sidecar manually with \
                 `fno-agents-worker --mcp-exec sidecar`."
            ));
            return;
        }
        Ok(Err(other)) => {
            stderr_line(&format!("sidecar unavailable ({other:?})"));
            return;
        }
        Err(_) => {
            stderr_line("sidecar lazy-start task failed");
            return;
        }
    };
    let mut reader = match tokio::net::UnixStream::connect(&sock_path).await {
        Ok(s) => BufReader::new(s),
        Err(e) => {
            stderr_line(&format!("sidecar connect failed: {e}"));
            return;
        }
    };
    let register_req = json!({
        "op": "register_channel",
        "session_id": session_id,
        "channel_name": channel_name,
        "pid": std::process::id(),
    });
    {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_vec(&register_req).unwrap_or_default();
        line.push(b'\n');
        let mut writer = reader.get_mut();
        if writer.write_all(&line).await.is_err() {
            stderr_line("register_channel write failed");
            return;
        }
    }
    // The first reply is the registration ack; later reads are deliver
    // push frames.
    let mut raw = Vec::new();
    match reader.read_until(b'\n', &mut raw).await {
        Ok(0) | Err(_) => {
            stderr_line("sidecar closed connection before ack");
            return;
        }
        Ok(_) => {}
    }
    let ack: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    if ack.get("ok").and_then(Value::as_bool) != Some(true) {
        stderr_line(&format!("register_channel rejected: {ack}"));
        return;
    }
    stderr_line(&format!(
        "registered session={session_id} channel={channel_name}"
    ));
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw).await {
            Ok(0) | Err(_) => {
                stderr_line("sidecar disconnected (EOF)");
                return;
            }
            Ok(_) => {}
        }
        let frame: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        if frame.get("op").and_then(Value::as_str) != Some("deliver") {
            stderr_line(&format!("unexpected sidecar frame: {frame}"));
            continue;
        }
        let Some(envelope) = frame.get("envelope") else {
            continue;
        };
        if let Err(reason) = validate_envelope(envelope) {
            stderr_line(&format!(
                "refusing to forward malformed envelope (reason={reason}); dropping"
            ));
            continue;
        }
        write_message(&out, envelope);
    }
}

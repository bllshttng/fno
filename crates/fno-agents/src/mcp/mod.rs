//! The MCP channel family: the wire format ([`channel`]), the sidecar
//! client ([`client`]), the per-user sidecar daemon ([`sidecar`]), and the
//! per-session stdio channel server ([`server`]). Ported whole from
//! `cli/src/fno/mcp/*.py` and answered natively through the worker's
//! `--mcp-exec` lane; the daemon's delivery leg answers in-process.

pub mod channel;
pub mod client;
pub mod server;
pub mod sidecar;

const USAGE: &str = "usage: fno-agents-worker --mcp-exec <sub-op> [flags]

sub-ops:
  send --session-id <id>      route one stdin envelope to a registered channel
  status                      print the sidecar status JSON
  serve --session-id <id> --channel-name <name>
                              run the per-session stdio channel server
  sidecar                     run the per-user sidecar daemon

Callers build this argv; the human spelling is `fno agents mcp send`.";

/// The `--mcp-exec` door: dispatch one sub-op. Returns the process exit
/// code. `args` is the full worker argv (the `--mcp-exec` flag included).
pub fn run_mcp_exec(args: &[String]) -> i32 {
    let idx = match args.iter().position(|a| a == "--mcp-exec") {
        Some(idx) => idx,
        None => return usage_refusal(),
    };
    match args.get(idx + 1).map(String::as_str) {
        Some("send") => run_send(args.get(idx + 2..).unwrap_or(&[])),
        Some("status") => run_status(),
        Some("serve") => run_serve(args.get(idx + 2..).unwrap_or(&[])),
        Some("sidecar") => sidecar::run_sidecar(sidecar::DEFAULT_IDLE_EXIT_SECONDS),
        _ => usage_refusal(),
    }
}

fn usage_refusal() -> i32 {
    eprintln!("{USAGE}");
    2
}

/// `send --session-id <id>` with the envelope on stdin (never argv: it can
/// be arbitrarily large).
fn run_send(rest: &[String]) -> i32 {
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        print!("{SEND_HELP}");
        return 0;
    }
    let session_id = match flag_value(rest, "--session-id") {
        Some(v) => v,
        None => {
            eprintln!("{SEND_USAGE}");
            eprintln!("Error: --session-id is required");
            return 2;
        }
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    if std::io::stdin().read_to_end(&mut bytes).is_err() {
        eprintln!("invalid envelope JSON: stdin unreadable");
        return 2;
    }
    client::send_from_bytes(&session_id, &bytes)
}

const SEND_HELP: &str = "Usage: fno agents mcp send [OPTIONS]

  Route a JSON envelope (read from stdin) to the channel under SESSION.

  Exit 0 on delivery; 1 on a sidecar/delivery failure; 2 on a malformed
  envelope. The delivery leg lazy-starts the sidecar (client default).

Options:
  --session-id <str>  Registered channel session id (the mcp_channel_id).
                      [required]
  --help              Show this message and exit.";

const SEND_USAGE: &str = "Usage: fno agents mcp send [OPTIONS]";

/// `status`: the sidecar status JSON on stdout, exit 0; an unreachable
/// sidecar notes the reason on stderr, exit 1. The reachability probe's
/// native door.
fn run_status() -> i32 {
    match client::status(None, client::DEFAULT_TIMEOUT) {
        Ok(reply) => {
            println!("{reply}");
            0
        }
        Err(client::ClientError::Unreachable { reason }) => {
            eprintln!("sidecar unreachable: {reason}");
            1
        }
        Err(other) => {
            eprintln!("status failed: {other:?}");
            1
        }
    }
}

/// `serve --session-id <id> --channel-name <name>`, both required.
fn run_serve(rest: &[String]) -> i32 {
    let (Some(session_id), Some(channel_name)) = (
        flag_value(rest, "--session-id"),
        flag_value(rest, "--channel-name"),
    ) else {
        eprintln!("mcp serve: --session-id and --channel-name are required");
        return 2;
    };
    server::run_channel_server(&session_id, &channel_name)
}

/// The last value of `flag` in `rest`.
fn flag_value(rest: &[String], flag: &str) -> Option<String> {
    rest.iter()
        .rev()
        .position(|a| a == flag)
        .and_then(|p| rest.get(rest.len() - p).cloned())
}

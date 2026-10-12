//! `fno mux notify <title> <message> [--json]`: one desktop notification
//! through every ATTACHED client's tty, over one control roundtrip. The
//! receipt's `clients` count is the delivery verdict - 0 means no attached
//! client took the notice, which is `fno notify`'s signal to fall back to
//! osascript. Notification codes only: the wire shape is `(title, body)`
//! strings, and the client-side builder strips control characters, so no raw
//! OSC payload can ride this verb to the outer terminal.

use crate::mux_cli::{control_roundtrip, resolve_session, EXIT_ERROR, EXIT_OK};
use crate::proto::{self, ControlVerb, ServerMsg};

/// Run one notify delivery and print the receipt. Registered in `lib.rs`
/// beside the other mux one-shots (`server_stats`), not inside `mux_cli.rs`:
/// that file is over the line budget and may only shrink.
pub fn cli(title: &str, message: &str, json: bool) -> i32 {
    let env_session = std::env::var("FNO_MUX_SESSION").ok();
    let session = resolve_session(None, env_session.as_deref());
    let sock = match proto::socket_path(&session) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fno mux notify: {e}");
            return EXIT_ERROR;
        }
    };
    match control_roundtrip(
        &sock,
        &session,
        ControlVerb::Notify {
            title: title.to_string(),
            body: message.to_string(),
        },
    ) {
        Ok(ServerMsg::NoticeSent { clients }) => {
            if json {
                let payload = serde_json::json!({ "clients": clients });
                println!("{payload}");
            } else if clients == 0 {
                println!("fno mux notify: no attached client took the notice");
            } else {
                println!("notice: {clients} attached client(s)");
            }
            EXIT_OK
        }
        Ok(other) => {
            eprintln!("fno mux notify: unexpected reply {other:?}");
            EXIT_ERROR
        }
        Err(e) => {
            eprintln!("fno mux notify: {e}");
            EXIT_ERROR
        }
    }
}

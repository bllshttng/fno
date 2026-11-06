//! Connection admission and request framing for the daemon's local socket.

use super::*;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Ordinary work is bounded so registry-lock waits cannot grow the blocking
/// pool without limit. Stop requests use their own small lane so ordinary
/// work cannot occupy every thread needed to stop a worker.
pub(super) const ORDINARY_REQUEST_SLOTS: usize = 32;
pub(super) const STOP_REQUEST_SLOTS: usize = 4;

pub(super) struct RequestAdmission {
    ordinary: Arc<Semaphore>,
    stop: Arc<Semaphore>,
}

impl Default for RequestAdmission {
    fn default() -> Self {
        Self {
            ordinary: Arc::new(Semaphore::new(ORDINARY_REQUEST_SLOTS)),
            stop: Arc::new(Semaphore::new(STOP_REQUEST_SLOTS)),
        }
    }
}

impl RequestAdmission {
    fn try_acquire(&self, method: &str) -> Result<OwnedSemaphorePermit, ()> {
        let lane = if method == "agent.stop" {
            Arc::clone(&self.stop)
        } else {
            Arc::clone(&self.ordinary)
        };
        lane.try_acquire_owned().map_err(|_| ())
    }
}

/// Idle cap for the first read on a connection: a client that connects but
/// never sends a frame self-terminates rather than holding the task forever.
const CONN_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) async fn serve_connection(
    ctx: Arc<Ctx>,
    admission: Arc<RequestAdmission>,
    mut stream: UnixStream,
) {
    // One request per accepted connection (clients open per RPC). A read fault
    // is mapped to a structured error response so callers get a deterministic
    // error code rather than a transport EOF (Codex P2): only a clean hangup
    // (UnexpectedEof) is silent. A silent client is bounded by the timeout.
    let req = match tokio::time::timeout(CONN_READ_TIMEOUT, read_request(&mut stream)).await {
        Err(_elapsed) => return, // client sent nothing within the window; drop
        Ok(Ok(r)) => r,
        Ok(Err(crate::protocol::ProtocolError::UnexpectedEof)) => return, // clean hangup
        Ok(Err(e)) => {
            // Malformed / oversized frame: we could not parse a request id, so
            // reply against id 0 with a structured MalformedFrame error.
            let resp = Response::err(0, ErrorCode::MalformedFrame, format!("{e}"));
            let _ = write_response(&mut stream, &resp).await;
            return;
        }
    };

    let _permit = match admission.try_acquire(&req.method) {
        Ok(permit) => permit,
        Err(()) => {
            let lane = if req.method == "agent.stop" {
                "stop"
            } else {
                "ordinary"
            };
            let response = Response::err(
                req.id,
                ErrorCode::Busy,
                format!("daemon {lane} request capacity is full; retry"),
            );
            let _ = write_response(&mut stream, &response).await;
            return;
        }
    };

    // `agent.logs` (with --follow) upgrades the same stream to a WebSocket and
    // streams appended log lines until the client detaches; it does not fit the
    // one-request/one-response shape.
    if req.method == "agent.logs" {
        crate::logs::handle_logs(&ctx.home, &req, stream).await;
        return;
    }
    let response = super::dispatch(&ctx, &req).await;
    let _ = write_response(&mut stream, &response).await;
}

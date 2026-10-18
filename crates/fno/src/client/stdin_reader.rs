//! The client's one stdin reader: raw chunks to the channel the prefix layer
//! scans.

use std::io::Read;

use tokio::sync::mpsc;

/// Pick the terminal's image protocol, then start the reader thread. The
/// order is the point: the protocol query reads the terminal's reply from
/// stdin, and once this thread owns stdin that reply would reach a pane.
pub(super) fn spawn(tx: mpsc::Sender<Vec<u8>>) -> Result<(), String> {
    super::image_layer::detect();
    std::thread::Builder::new()
        .name("fno-mux-stdin".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map(|_| ())
        .map_err(|e| format!("stdin thread: {e}"))
}

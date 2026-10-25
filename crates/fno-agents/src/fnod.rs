//! fnod: the local decision model, served by `kev.serve` on loopback. Its
//! answers are advice only. A caller journals the score beside its own rule
//! verdict and keeps the rule's answer, so a missing server, a timeout or a
//! malformed reply costs the score and nothing else.

use serde_json::{json, Map, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

/// `python -m kev.serve` listens here unless told otherwise.
const DEFAULT_URL: &str = "http://127.0.0.1:8008";
const DEFAULT_TIMEOUT_MS: u64 = 500;
/// Asks sit at the end of a message, so a long one keeps its tail.
const MAX_MESSAGE_CHARS: usize = 16_000;

/// The question the is_ask head was trained on, word for word
/// (`intel_corpus.rs::question`): a reworded question is a different input.
const IS_ASK: &str = "Does this message ask the user for a decision or an answer?";

#[derive(Debug, PartialEq)]
pub(crate) enum Answer {
    Score { p: f64, ms: u64 },
    Unavailable(String),
}

impl Answer {
    /// The journal attrs for this answer: `fnod_is_ask` and `fnod_ms`, or
    /// `fnod_error` with the reason the score is missing.
    pub(crate) fn attrs(&self) -> Map<String, Value> {
        let mut out = Map::new();
        match self {
            Answer::Score { p, ms } => {
                out.insert("fnod_is_ask".to_string(), json!(p));
                out.insert("fnod_ms".to_string(), json!(ms));
            }
            Answer::Unavailable(why) => {
                out.insert("fnod_error".to_string(), json!(why));
            }
        }
        out
    }
}

/// fnod's probability that `message` asks the user for a decision or an
/// answer. `fnod.url` (empty turns it off) and `fnod.timeout_ms` come from
/// fno config.
pub(crate) fn is_ask(cwd: &Path, message: &str) -> Answer {
    let url = crate::agents_config::config_value_deep(cwd, &["fnod", "url"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    if url.trim().is_empty() {
        return Answer::Unavailable("off".to_string());
    }
    let timeout = crate::agents_config::config_value_deep(cwd, &["fnod", "timeout_ms"])
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    let body = json!({
        "state": {"message": tail(message, MAX_MESSAGE_CHARS)},
        "questions": {"is_ask": {"type": "noul", "instructions": IS_ASK}},
    });
    let start = Instant::now();
    match post(&url, "/v1/systemone", &body, Duration::from_millis(timeout)) {
        Ok(reply) => match reply
            .pointer("/answers/is_ask/noul")
            .and_then(Value::as_f64)
        {
            Some(p) => Answer::Score {
                p,
                ms: start.elapsed().as_millis() as u64,
            },
            None => Answer::Unavailable("reply carries no answers.is_ask.noul".to_string()),
        },
        Err(e) => Answer::Unavailable(e),
    }
}

fn tail(text: &str, max: usize) -> &str {
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let skip = text.char_indices().nth(count - max).map_or(0, |(i, _)| i);
    &text[skip..]
}

/// One JSON POST to a loopback `http://host:port` base. The whole exchange,
/// connect to last byte, shares one deadline.
fn post(base: &str, route: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let authority = base
        .strip_prefix("http://")
        .ok_or_else(|| format!("fnod.url {base:?} is not http://"))?
        .trim_end_matches('/');
    let deadline = Instant::now() + timeout;
    let addr = authority
        .to_socket_addrs()
        .map_err(|e| format!("resolve {authority}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {authority}: no address"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("connect {authority}: {e}"))?;
    let left = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| format!("no reply within {}ms", timeout.as_millis()))
    };
    stream
        .set_write_timeout(Some(left()?))
        .map_err(|e| e.to_string())?;
    let payload = body.to_string();
    let head = format!(
        "POST {route} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(payload.as_bytes()))
        .map_err(|e| format!("write {authority}: {e}"))?;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        stream
            .set_read_timeout(Some(left()?))
            .map_err(|e| e.to_string())?;
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(format!("no reply within {}ms", timeout.as_millis()))
            }
            Err(e) => return Err(format!("read {authority}: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("malformed reply from {authority}"))?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!("HTTP {status}"));
    }
    serde_json::from_str(body).map_err(|e| format!("reply is not JSON: {e}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-shot kev stand-in on an ephemeral port: it reads one request,
    /// hands its body back through the join handle, and answers `reply`
    /// after `delay`.
    pub(crate) fn serve_once(
        reply: &'static str,
        delay: Duration,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= len {
                        std::thread::sleep(delay);
                        let out = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                            reply.len()
                        );
                        stream.write_all(out.as_bytes()).ok();
                        return body.to_string();
                    }
                }
                if n == 0 {
                    return String::new();
                }
            }
        });
        (url, handle)
    }

    fn cwd_with(url: &str, timeout_ms: u64) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        std::fs::write(
            dir.path().join(".fno/config.toml"),
            format!("[fnod]\nurl = \"{url}\"\ntimeout_ms = {timeout_ms}\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_kev_reply_scores_and_a_slow_or_absent_server_fails_open() {
        let (url, server) = serve_once(
            r#"{"model":"kev-latest","answers":{"is_ask":{"type":"noul","noul":0.91}}}"#,
            Duration::ZERO,
        );
        let cwd = cwd_with(&url, 2000);
        let answer = is_ask(cwd.path(), "Ship it now or wait for review?");
        assert!(matches!(answer, Answer::Score { p, .. } if (p - 0.91).abs() < 1e-9));
        let sent: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(sent["questions"]["is_ask"]["instructions"], IS_ASK);
        assert_eq!(sent["state"]["message"], "Ship it now or wait for review?");

        let (url, server) = serve_once("{}", Duration::from_millis(600));
        let cwd = cwd_with(&url, 100);
        let started = Instant::now();
        let answer = is_ask(cwd.path(), "Which lane?");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(matches!(&answer, Answer::Unavailable(e) if e.contains("no reply")));
        assert!(answer.attrs().contains_key("fnod_error"));
        server.join().ok();

        let cwd = cwd_with("", 100);
        assert_eq!(
            is_ask(cwd.path(), "Which lane?"),
            Answer::Unavailable("off".to_string())
        );

        // A long message keeps its tail, cut on a char boundary.
        assert_eq!(tail(&format!("{}é?", "x".repeat(20)), 3), "xé?");
    }
}

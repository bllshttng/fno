//! fnod: the local decision model, served by `kev.serve` on loopback. Its
//! answers are advice only. A caller journals the score beside its own rule
//! verdict and keeps the rule's answer, so a missing server, a timeout or a
//! malformed reply costs the score and nothing else.

use serde_json::{json, Map, Value};
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

/// fnod's answer to "does `message` ask the user for a decision or an
/// answer?" as journal attrs: `fnod_is_ask` (the probability) and `fnod_ms`,
/// or `fnod_error` with the reason the score is missing. `fnod.url` (empty
/// turns it off) and `fnod.timeout_ms` come from fno config.
pub(crate) fn is_ask(cwd: &Path, message: &str) -> Map<String, Value> {
    let url = crate::agents_config::config_value_deep(cwd, &["fnod", "url"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let timeout = crate::agents_config::config_value_deep(cwd, &["fnod", "timeout_ms"])
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    let body = json!({
        "state": {"message": tail(message, MAX_MESSAGE_CHARS)},
        "questions": {"is_ask": {"type": "noul", "instructions": IS_ASK}},
    });
    let start = Instant::now();
    let score = if url.trim().is_empty() {
        Err("off".to_string())
    } else if !loopback(&url) {
        // The message is chat text: a project config must not route it off
        // the machine.
        Err(format!("fnod.url {url} is not loopback; refused"))
    } else {
        let wait = Duration::from_millis(timeout);
        crate::opencode_serve::http_json(&url, "POST", "/v1/systemone", Some(&body), None, wait)
            .and_then(|(status, text)| match status {
                200 => serde_json::from_str::<Value>(&text).map_err(|e| format!("reply: {e}")),
                _ => Err(format!("HTTP {status}")),
            })
            .and_then(|reply| {
                reply
                    .pointer("/answers/is_ask/noul")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "reply carries no answers.is_ask.noul".to_string())
            })
    };
    let mut out = Map::new();
    match score {
        Ok(p) => {
            out.insert("fnod_is_ask".to_string(), json!(p));
            out.insert(
                "fnod_ms".to_string(),
                json!(start.elapsed().as_millis() as u64),
            );
        }
        Err(why) => {
            out.insert("fnod_error".to_string(), json!(why));
        }
    }
    out
}

/// Every address `url` resolves to is a loopback address.
fn loopback(url: &str) -> bool {
    let authority = url
        .strip_prefix("http://")
        .unwrap_or_default()
        .trim_end_matches('/');
    std::net::ToSocketAddrs::to_socket_addrs(authority).is_ok_and(|addrs| {
        let addrs: Vec<_> = addrs.collect();
        !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
    })
}

fn tail(text: &str, max: usize) -> &str {
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    let skip = text.char_indices().nth(count - max).map_or(0, |(i, _)| i);
    &text[skip..]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};
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
        assert_eq!(answer["fnod_is_ask"], 0.91);
        let sent: Value = serde_json::from_str(&server.join().unwrap()).unwrap();
        assert_eq!(sent["questions"]["is_ask"]["instructions"], IS_ASK);
        assert_eq!(sent["state"]["message"], "Ship it now or wait for review?");

        let (url, server) = serve_once("{}", Duration::from_millis(600));
        let cwd = cwd_with(&url, 100);
        let started = Instant::now();
        let answer = is_ask(cwd.path(), "Which lane?");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(answer["fnod_error"]
            .as_str()
            .unwrap()
            .starts_with("read from"));
        server.join().ok();

        let cwd = cwd_with("", 100);
        assert_eq!(is_ask(cwd.path(), "Which lane?")["fnod_error"], "off");

        // A non-loopback host never receives the message.
        let cwd = cwd_with("http://10.0.0.1:8008", 100);
        let refused = is_ask(cwd.path(), "Which lane?");
        assert!(refused["fnod_error"]
            .as_str()
            .unwrap()
            .contains("not loopback"));

        // A long message keeps its tail, cut on a char boundary.
        assert_eq!(tail(&format!("{}é?", "x".repeat(20)), 3), "xé?");
    }
}

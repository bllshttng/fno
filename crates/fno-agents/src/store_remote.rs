//! The opt-in remote store: one libSQL (sqld) primary that every machine
//! dials out to.
//!
//! `store.remote_url` in the per-machine global config turns it on. Unset,
//! nothing here runs and no process opens a socket. Set, the claim keys that
//! decide dispatch (see `claim_store::SHARED_PREFIXES`) run their statements
//! on the primary, so two machines never hold one node. Every other store
//! stays local. The client speaks sqld's Hrana-over-HTTP pipeline over HTTP
//! or HTTPS. The primary sits on a private network (the tailnet), which owns
//! remote reach, as `fno web` leaves it. An `https://` URL wraps the same
//! hand-written request in TLS, trusting the system roots, so the token and
//! every row stop crossing the wire in clear.
//!
//! A primary that cannot be reached fails closed: the call returns an error
//! that names the URL and the key, and nothing is written anywhere.

use base64::Engine;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

const CONNECT_WAIT: Duration = Duration::from_secs(3);
const REPLY_WAIT: Duration = Duration::from_secs(15);
const KEY: &str = "store.remote_url";
const ADDRS_KEY: &str = "store.remote_addrs";
const UNREACHABLE: &str = "is unreachable";
/// The primary took the request and sent nothing back in time. Callers
/// still fail closed on it, but the words must not say "unreachable": the
/// host answers, and the cure is on the primary, not the network.
const QUIET: &str = "did not answer";
const NO_REPLY: &str = "no reply within";

#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl SqlValue {
    pub fn text(&self) -> Option<&str> {
        match self {
            SqlValue::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn integer(&self) -> Option<i64> {
        match self {
            SqlValue::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn real(&self) -> Option<f64> {
        match self {
            SqlValue::Real(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn to_hrana(&self) -> Value {
        match self {
            SqlValue::Null => json!({"type": "null"}),
            SqlValue::Integer(v) => json!({"type": "integer", "value": v.to_string()}),
            SqlValue::Real(v) => json!({"type": "float", "value": v}),
            SqlValue::Text(v) => json!({"type": "text", "value": v}),
            SqlValue::Blob(v) => json!({
                "type": "blob",
                "base64": base64::engine::general_purpose::STANDARD_NO_PAD.encode(v)
            }),
        }
    }

    pub(crate) fn from_hrana(value: &Value) -> Result<Self, String> {
        let field = |name: &str| value.get(name);
        Ok(match field("type").and_then(Value::as_str) {
            Some("null") => SqlValue::Null,
            Some("integer") => SqlValue::Integer(
                field("value")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse().ok())
                    .ok_or("integer value without digits")?,
            ),
            Some("float") => {
                SqlValue::Real(field("value").and_then(Value::as_f64).ok_or("bad float")?)
            }
            Some("text") => SqlValue::Text(
                field("value")
                    .and_then(Value::as_str)
                    .ok_or("text value without text")?
                    .to_string(),
            ),
            Some("blob") => SqlValue::Blob(
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(
                        field("base64")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim_end_matches('='),
                    )
                    .map_err(|e| e.to_string())?,
            ),
            other => return Err(format!("unknown value type {other:?}")),
        })
    }
}

impl rusqlite::ToSql for SqlValue {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            SqlValue::Null => ValueRef::Null,
            SqlValue::Integer(v) => ValueRef::Integer(*v),
            SqlValue::Real(v) => ValueRef::Real(*v),
            SqlValue::Text(v) => ValueRef::Text(v.as_bytes()),
            SqlValue::Blob(v) => ValueRef::Blob(v),
        }))
    }
}

impl From<rusqlite::types::ValueRef<'_>> for SqlValue {
    fn from(value: rusqlite::types::ValueRef<'_>) -> Self {
        use rusqlite::types::ValueRef;
        match value {
            ValueRef::Null => SqlValue::Null,
            ValueRef::Integer(v) => SqlValue::Integer(v),
            ValueRef::Real(v) => SqlValue::Real(v),
            ValueRef::Text(v) => SqlValue::Text(String::from_utf8_lossy(v).into_owned()),
            ValueRef::Blob(v) => SqlValue::Blob(v.to_vec()),
        }
    }
}

/// One statement's reply: the column names, the rows, and the change count.
#[derive(Debug, Default)]
pub struct Reply {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    pub affected: u64,
}

/// The TLS client config an `https://` remote speaks with. `None` is the
/// system roots; a test hands in its own CA. Two remotes compare equal on
/// the same config object, which is all the derived `PartialEq` needs.
#[derive(Debug, Clone)]
struct Tls(Option<Arc<rustls::ClientConfig>>);

impl PartialEq for Tls {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    url: String,
    host: String,
    port: u16,
    path: String,
    token: Option<String>,
    fallbacks: Vec<SocketAddr>,
    /// `Some` for an `https://` URL.
    tls: Option<Tls>,
}

/// The words a TLS failure starts with. It is the primary's certificate,
/// not the network, so callers must not hold it as an outage.
const TLS_FAULT: &str = "TLS: ";

impl Remote {
    /// Parse `http://host[:port][/prefix]` or `https://host[:port][/prefix]`.
    /// Any other scheme refuses. https trusts the system roots, so the
    /// primary serves a real certificate (`tailscale cert`).
    pub fn parse(url: &str, token: Option<String>) -> Result<Self, String> {
        let url = url.trim().trim_end_matches('/');
        let (rest, tls, default_port) = match url.strip_prefix("https://") {
            Some(rest) => (rest, Some(Tls(None)), 443),
            None => (
                url.strip_prefix("http://").ok_or_else(|| {
                    format!("{KEY} = {url:?}: only http:// and https:// URLs are supported")
                })?,
                None,
                80,
            ),
        };
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, String::new()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse()
                    .map_err(|_| format!("{KEY} = {url:?}: bad port {port:?}"))?,
            ),
            None => (authority, default_port),
        };
        if host.is_empty() {
            return Err(format!("{KEY} = {url:?}: no host"));
        }
        Ok(Self {
            url: url.to_string(),
            host: host.to_string(),
            port,
            path,
            token: token.filter(|t| !t.is_empty()),
            fallbacks: Vec::new(),
            tls,
        })
    }

    /// Trust `config` instead of the system roots. Only tests need it: a
    /// real primary serves a certificate the system already trusts.
    #[cfg(test)]
    pub(crate) fn with_tls_config(mut self, config: Arc<rustls::ClientConfig>) -> Self {
        self.tls = Some(Tls(Some(config)));
        self
    }

    /// Addresses to dial, in order, after the host name's own: a Tailscale
    /// IP still works when MagicDNS does not. An entry is an IP or an
    /// IP:port; a bare IP takes the URL's port.
    pub fn with_fallbacks(mut self, addrs: &[String]) -> Result<Self, String> {
        let port = self.port;
        self.fallbacks = addrs
            .iter()
            .map(|addr| {
                let addr = addr.trim();
                addr.parse::<SocketAddr>()
                    .or_else(|_| addr.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, port)))
                    .map_err(|_| format!("{ADDRS_KEY} entry {addr:?}: not an IP or IP:port"))
            })
            .collect::<Result<_, _>>()?;
        Ok(self)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The store host from `store.remote_url`: the ssh rescue's last-resort
    /// peer key.
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn execute(&self, sql: &str, args: &[SqlValue]) -> Result<Reply, String> {
        let mut replies = self.pipeline(vec![json!({
            "type": "execute",
            "stmt": {"sql": sql, "args": args.iter().map(SqlValue::to_hrana).collect::<Vec<_>>()},
        })])?;
        Ok(replies.pop().unwrap_or_default())
    }

    /// Run a multi-statement script (DDL) in one request.
    pub fn script(&self, sql: &str) -> Result<(), String> {
        self.pipeline(vec![json!({"type": "sequence", "sql": sql})])
            .map(drop)
    }

    /// Run `steps` as one transaction in one request. Each step runs only
    /// when the step before it succeeded, and a failed step rolls the batch
    /// back, so the primary keeps all of the steps or none. A refusal
    /// carries the index of the step that failed, or `None` when the
    /// request itself failed.
    pub fn transaction(
        &self,
        steps: &[(String, Vec<SqlValue>)],
    ) -> Result<Vec<Reply>, (Option<usize>, String)> {
        let stmt = |sql: &str, args: &[SqlValue]| json!({"sql": sql, "args": args.iter().map(SqlValue::to_hrana).collect::<Vec<_>>()});
        let commit = steps.len() + 1;
        let mut batch = vec![json!({"stmt": stmt("BEGIN IMMEDIATE", &[])})];
        for (i, (sql, args)) in steps.iter().enumerate() {
            batch.push(json!({"stmt": stmt(sql, args), "condition": {"type": "ok", "step": i}}));
        }
        batch.push(
            json!({"stmt": stmt("COMMIT", &[]), "condition": {"type": "ok", "step": commit - 1}}),
        );
        batch.push(json!({
            "stmt": stmt("ROLLBACK", &[]),
            "condition": {"type": "not", "cond": {"type": "ok", "step": commit}},
        }));
        let response = self
            .responses(vec![json!({"type": "batch", "batch": {"steps": batch}})])
            .map_err(|error| (None, error))?
            // The batch answers first; the pipeline's close answers last.
            .into_iter()
            .next()
            .unwrap_or_default();
        let result = response.get("result").cloned().unwrap_or_default();
        let errors = result.get("step_errors").and_then(Value::as_array);
        for index in 0..=commit {
            let Some(error) = errors.and_then(|e| e.get(index)).filter(|e| !e.is_null()) else {
                continue;
            };
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("statement failed");
            let step = (1..commit).contains(&index).then(|| index - 1);
            return Err((step, format!("remote store {}: {message}", self.url)));
        }
        let results = result.get("step_results").and_then(Value::as_array);
        if results
            .and_then(|r| r.get(commit))
            .is_none_or(Value::is_null)
        {
            return Err((
                None,
                format!("remote store {}: the batch did not commit", self.url),
            ));
        }
        (1..commit)
            .map(|index| {
                let step = results
                    .and_then(|r| r.get(index))
                    .cloned()
                    .unwrap_or_default();
                self.reply(&step).map_err(|error| (Some(index - 1), error))
            })
            .collect()
    }

    fn pipeline(&self, requests: Vec<Value>) -> Result<Vec<Reply>, String> {
        self.responses(requests)?
            .iter()
            .filter_map(|response| response.get("result"))
            .map(|rows| self.reply(rows))
            .collect()
    }

    /// Send one pipeline and return each request's `response`. The first
    /// request that failed fails the call.
    fn responses(&self, mut requests: Vec<Value>) -> Result<Vec<Value>, String> {
        requests.push(json!({"type": "close"}));
        let body = json!({"baton": null, "requests": requests}).to_string();
        let reply = self.post("/v2/pipeline", &body).map_err(|error| {
            // A 4xx is the primary answering, a bad token say: a fault to
            // name, not an outage to hold through.
            if error.starts_with("HTTP 4") {
                format!(
                    "remote store {} ({KEY}) refused the request: {error}",
                    self.url
                )
            } else if let Some(tls) = error.strip_prefix(TLS_FAULT) {
                // A certificate the system roots reject is a fault on the
                // primary, never an outage to queue through.
                format!(
                    "remote store {} ({KEY}) failed TLS: {tls}. Nothing was written. \
                     The primary must serve a certificate the system trusts, such as \
                     one from `tailscale cert`.",
                    self.url
                )
            } else {
                self.unreachable(&error)
            }
        })?;
        let reply: Value = serde_json::from_str(&reply)
            .map_err(|error| format!("remote store {}: bad reply: {error}", self.url))?;
        let results = reply
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("remote store {}: reply has no results", self.url))?;
        let mut out = Vec::new();
        for result in results {
            if result.get("type").and_then(Value::as_str) != Some("ok") {
                let message = result
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("statement failed");
                return Err(format!("remote store {}: {message}", self.url));
            }
            if let Some(response) = result.get("response") {
                out.push(response.clone());
            }
        }
        Ok(out)
    }

    /// One statement result: its columns, rows and change count.
    fn reply(&self, rows: &Value) -> Result<Reply, String> {
        let columns = rows
            .get("cols")
            .and_then(Value::as_array)
            .map(|cols| {
                cols.iter()
                    .map(|c| c.get("name").and_then(Value::as_str).unwrap_or("").into())
                    .collect()
            })
            .unwrap_or_default();
        let rows_out = rows
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        row.as_array()
                            .into_iter()
                            .flatten()
                            .map(SqlValue::from_hrana)
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(|error| format!("remote store {}: {error}", self.url))?
            .unwrap_or_default();
        Ok(Reply {
            columns,
            rows: rows_out,
            affected: rows
                .get("affected_row_count")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        })
    }

    fn unreachable(&self, error: &str) -> String {
        let verdict = if error.starts_with(NO_REPLY) {
            QUIET
        } else {
            UNREACHABLE
        };
        format!(
            "remote store {} ({KEY}) {verdict}: {error}. Nothing was written locally. \
             A request that timed out can still have reached the primary.",
            self.url
        )
    }

    /// Connect to the first address that answers: the host name's, then each
    /// fallback. Only the connect moves on. A request already written can
    /// have reached the primary, so it is never sent to a second address.
    fn connect(&self) -> Result<TcpStream, String> {
        let mut failures = Vec::new();
        let resolved = self.resolve().unwrap_or_else(|error| {
            failures.push(format!("resolve {}: {error}", self.host));
            Vec::new()
        });
        let fallbacks = self.fallbacks.iter().filter(|a| !resolved.contains(a));
        for address in resolved.iter().chain(fallbacks) {
            match TcpStream::connect_timeout(address, CONNECT_WAIT) {
                Ok(stream) => return Ok(stream),
                Err(e) => failures.push(format!("connect {address}: {e}")),
            }
        }
        Err(failures.join("; "))
    }

    /// The host name's addresses. The system resolver takes no timeout, and
    /// a dead MagicDNS server can hold it for its whole retry schedule, so
    /// the lookup runs on its own thread and waits at most CONNECT_WAIT.
    fn resolve(&self) -> Result<Vec<SocketAddr>, String> {
        let (host, port) = (self.host.clone(), self.port);
        let (send, receive) = std::sync::mpsc::channel::<std::io::Result<Vec<SocketAddr>>>();
        std::thread::spawn(move || {
            let _ = send.send(
                (host.as_str(), port)
                    .to_socket_addrs()
                    .map(Iterator::collect),
            );
        });
        match receive.recv_timeout(CONNECT_WAIT) {
            Ok(Ok(addrs)) if addrs.is_empty() => Err("no address".into()),
            Ok(Ok(addrs)) => Ok(addrs),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("no answer in {}s", CONNECT_WAIT.as_secs())),
        }
    }

    fn post(&self, route: &str, body: &str) -> Result<String, String> {
        let stream = self.connect()?;
        stream
            .set_read_timeout(Some(REPLY_WAIT))
            .and_then(|()| stream.set_write_timeout(Some(REPLY_WAIT)))
            .map_err(|e| e.to_string())?;
        let auth = self
            .token
            .as_ref()
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let head = format!(
            "POST {}{route} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n{auth}Connection: close\r\n\r\n",
            self.path,
            self.host,
            self.port,
            body.len()
        );
        let Some(Tls(config)) = &self.tls else {
            return exchange(stream, &head, body);
        };
        let config = match config {
            Some(config) => Arc::clone(config),
            None => system_tls()?,
        };
        // The certificate name is the URL's host even when the dial went to
        // a fallback IP: the IP is only a route to the same primary.
        let name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|e| format!("{TLS_FAULT}bad server name {:?}: {e}", self.host))?;
        let session =
            rustls::ClientConnection::new(config, name).map_err(|e| format!("{TLS_FAULT}{e}"))?;
        exchange(rustls::StreamOwned::new(session, stream), &head, body)
    }
}

/// Write one request and read its reply. A TLS failure (a rejected
/// certificate, say) comes back as an io error that wraps a rustls error;
/// it is named [`TLS_FAULT`] so the caller does not read it as an outage.
fn exchange(mut stream: impl Read + Write, head: &str, body: &str) -> Result<String, String> {
    let tls_fault = |e: std::io::Error| match e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        Some(tls) => format!("{TLS_FAULT}{tls}"),
        None => e.to_string(),
    };
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body.as_bytes()))
        .and_then(|()| stream.flush())
        .map_err(tls_fault)?;
    read_response(BufReader::new(stream))
}

/// The client config for `https://`: the system roots on the ring provider,
/// built once per process. No root loads is a refusal, not an empty store
/// that would reject every certificate with a misleading message.
fn system_tls() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: std::sync::OnceLock<Result<Arc<rustls::ClientConfig>, String>> =
        std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            let found = rustls_native_certs::load_native_certs();
            let (added, _) = roots.add_parsable_certificates(found.certs);
            if added == 0 {
                return Err(format!(
                    "{TLS_FAULT}no system root certificate loaded ({} errors)",
                    found.errors.len()
                ));
            }
            tls_config(roots)
        })
        .clone()
}

pub(crate) fn tls_config(
    roots: rustls::RootCertStore,
) -> Result<Arc<rustls::ClientConfig>, String> {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map(|builder| Arc::new(builder.with_root_certificates(roots).with_no_client_auth()))
        .map_err(|e| format!("{TLS_FAULT}{e}"))
}

/// Is this error the fail-closed refusal of a primary that could not be
/// reached or did not answer in time?
pub fn is_unreachable(error: &str) -> bool {
    error.starts_with("remote store ") && (error.contains(UNREACHABLE) || error.contains(QUIET))
}

/// A read error in words. A read that hit [`REPLY_WAIT`] says so: macOS
/// reports it as os error 35, "Resource temporarily unavailable", which
/// reads like a refused connection when the primary took the request and
/// went quiet.
fn io_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => format!(
            "{NO_REPLY} {}s (the primary took the request and went quiet; \
             `fno agents claim backlog probe` times one read and one write)",
            REPLY_WAIT.as_secs()
        ),
        _ => error.to_string(),
    }
}

fn read_response(mut reader: impl BufRead) -> Result<String, String> {
    let mut status = String::new();
    reader.read_line(&mut status).map_err(io_error)?;
    let code = status.split_whitespace().nth(1).unwrap_or("");
    let (mut length, mut chunked) = (None, false);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(io_error)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse::<usize>().ok(),
                "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
                _ => {}
            }
        }
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).map_err(io_error)?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| "bad chunk size in reply".to_string())?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).map_err(io_error)?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else if let Some(length) = length {
        body.resize(length, 0);
        reader.read_exact(&mut body).map_err(io_error)?;
    } else {
        reader.read_to_end(&mut body).map_err(io_error)?;
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    if code != "200" {
        return Err(format!("HTTP {code}: {}", body.trim()));
    }
    Ok(body)
}

/// The primary this machine is configured to use. `Ok(None)` is the stock
/// install: the key is unset and the local file is the store. A malformed
/// value is an error, so a typo never falls back to local in silence.
pub fn configured() -> Result<Option<Remote>, String> {
    #[cfg(test)]
    {
        Ok(None)
    }
    #[cfg(not(test))]
    {
        static CONFIGURED: std::sync::OnceLock<Result<Option<Remote>, String>> =
            std::sync::OnceLock::new();
        CONFIGURED
            .get_or_init(|| {
                let Some(url) = store_string("remote_url") else {
                    return Ok(None);
                };
                Remote::parse(&url, configured_token())?
                    .with_fallbacks(&configured_addrs()?)
                    .map(Some)
            })
            .clone()
    }
}

/// `store.remote_token`: this machine's own credential on the primary.
pub fn configured_token() -> Option<String> {
    store_string("remote_token")
}

/// The token for an explicit `url`: the configured token only when `url`
/// names the configured primary. Any other server never sees it.
pub fn token_for_url(url: &str) -> Option<String> {
    let same = |a: &str, b: &str| a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/');
    store_string("remote_url")
        .filter(|configured| same(configured, url))
        .and_then(|_| configured_token())
}

fn store_string(key: &str) -> Option<String> {
    crate::agents_config::config_lookup_global(&["store", key])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|v| !v.trim().is_empty())
}

/// `store.remote_addrs`: unset is no fallback, and anything but a list of
/// strings refuses, so a typo never drops the fallback in silence.
#[cfg(not(test))]
fn configured_addrs() -> Result<Vec<String>, String> {
    let Some(value) = crate::agents_config::config_lookup_global(&["store", "remote_addrs"]) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
        .ok_or_else(|| format!("{ADDRS_KEY} = {value}: expected a list of IPs"))
}

/// The primary that holds the shared backlog. Both `store.remote_url` and
/// `store.share_backlog = true` must be set; either one unset is the stock
/// install, where every backlog write stays in the local file and no
/// process opens a socket for it.
pub fn share_backlog() -> Result<Option<Remote>, String> {
    #[cfg(test)]
    {
        Ok(None)
    }
    #[cfg(not(test))]
    {
        // Read once per process: every backlog open asks, key on or off.
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let on = *ON.get_or_init(|| {
            crate::agents_config::config_lookup_global(&["store", "share_backlog"])
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
        if on {
            configured()
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
pub(crate) mod test_primary {
    //! A sqld stand-in for tests: the Hrana pipeline over HTTP, answered by
    //! one SQLite file. Same SQL engine, a real socket.

    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    pub(crate) struct Primary {
        pub remote: Remote,
        pub db: Arc<Mutex<rusqlite::Connection>>,
        /// A statement reply over this many bytes is refused, the way sqld
        /// refuses one over its response size limit. Unlimited by default.
        pub cap: Arc<AtomicUsize>,
        _dir: tempfile::TempDir,
    }

    impl Primary {
        /// The bytes of the reply `sql` would get, measured the way the cap is.
        pub(crate) fn reply_len(&self, sql: &str, args: &[SqlValue]) -> usize {
            let args: Vec<Value> = args.iter().map(SqlValue::to_hrana).collect();
            statement(&self.db.lock().unwrap(), &json!({"sql": sql, "args": args}))
                .unwrap()
                .to_string()
                .len()
        }

        pub(crate) fn set_cap(&self, cap: usize) {
            self.cap.store(cap, Ordering::SeqCst);
        }
    }

    pub(crate) fn start() -> Primary {
        let (primary, port) = start_on(None);
        Primary {
            remote: Remote::parse(&format!("http://127.0.0.1:{port}"), None).unwrap(),
            ..primary
        }
    }

    /// The same stand-in behind TLS, serving a `localhost` certificate from
    /// a test CA. The remote it returns trusts that CA, and nothing else.
    pub(crate) fn start_tls() -> Primary {
        let pem = |name: &str| {
            let dir =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store_tls");
            std::fs::read(dir.join(name)).unwrap()
        };
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let chain = vec![CertificateDer::from_pem_slice(&pem("leaf.pem")).unwrap()];
        let key = PrivateKeyDer::from_pem_slice(&pem("leaf.pk8.pem")).unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
        let (primary, port) = start_on(Some(Arc::new(server)));
        Primary {
            remote: Remote::parse(&format!("https://localhost:{port}"), None)
                .unwrap()
                .with_tls_config(test_ca()),
            ..primary
        }
    }

    /// A client config that trusts only the test CA.
    pub(crate) fn test_ca() -> Arc<rustls::ClientConfig> {
        use rustls::pki_types::pem::PemObject;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/store_tls/ca.pem");
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from_pem_file(path).unwrap())
            .unwrap();
        tls_config(roots).unwrap()
    }

    fn start_on(tls: Option<Arc<rustls::ServerConfig>>) -> (Primary, u16) {
        let dir = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(dir.path().join("primary.db")).unwrap();
        // The real primary enforces foreign keys.
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        let db = Arc::new(Mutex::new(connection));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::clone(&db);
        let cap = Arc::new(AtomicUsize::new(usize::MAX));
        let limit = Arc::clone(&cap);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let cap = limit.load(Ordering::SeqCst);
                match &tls {
                    None => serve(stream, &served, cap),
                    // One thread per TLS connection: a client that rejects
                    // the certificate aborts its handshake, and that panic
                    // must not take the listener down with it.
                    Some(config) => {
                        let (config, served) = (Arc::clone(config), Arc::clone(&served));
                        std::thread::spawn(move || {
                            let session = rustls::ServerConnection::new(config).unwrap();
                            serve(rustls::StreamOwned::new(session, stream), &served, cap);
                        });
                    }
                }
            }
        });
        let primary = Primary {
            remote: dead(),
            db,
            cap,
            _dir: dir,
        };
        (primary, port)
    }

    /// A URL with nothing listening behind it.
    pub(crate) fn dead() -> Remote {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Remote::parse(&format!("http://127.0.0.1:{port}"), None).unwrap()
    }

    fn serve(stream: impl Read + Write, db: &Mutex<rusqlite::Connection>, cap: usize) {
        let mut reader = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let request: Value = serde_json::from_slice(&body).unwrap();
        let connection = db.lock().unwrap();
        let results: Vec<Value> = request["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| answer(&connection, r, cap))
            .collect();
        let reply = json!({"baton": null, "results": results}).to_string();
        let stream = reader.get_mut();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
            reply.len()
        )
        .and_then(|()| stream.flush())
        .unwrap();
    }

    /// A Hrana batch: steps run in order, each only when its condition
    /// holds, and every step reports a result or an error.
    fn batch(connection: &rusqlite::Connection, steps: &[Value]) -> Value {
        let mut outcomes: Vec<Option<bool>> = Vec::new();
        let (mut results, mut errors) = (Vec::new(), Vec::new());
        for step in steps {
            if !step["condition"].is_null() && !holds(&step["condition"], &outcomes) {
                outcomes.push(None);
                results.push(Value::Null);
                errors.push(Value::Null);
                continue;
            }
            match statement(connection, &step["stmt"]) {
                Ok(result) => {
                    outcomes.push(Some(true));
                    results.push(result);
                    errors.push(Value::Null);
                }
                Err(e) => {
                    outcomes.push(Some(false));
                    results.push(Value::Null);
                    errors.push(json!({"message": e.to_string()}));
                }
            }
        }
        json!({"type": "ok", "response": {"type": "batch", "result": {
            "step_results": results, "step_errors": errors,
        }}})
    }

    fn holds(condition: &Value, outcomes: &[Option<bool>]) -> bool {
        let step = |c: &Value| {
            outcomes
                .get(c["step"].as_u64().unwrap() as usize)
                .copied()
                .flatten()
        };
        match condition["type"].as_str() {
            Some("ok") => step(condition) == Some(true),
            Some("error") => step(condition) == Some(false),
            Some("not") => !holds(&condition["cond"], outcomes),
            Some("and") => condition["conds"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| holds(c, outcomes)),
            Some("or") => condition["conds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| holds(c, outcomes)),
            other => panic!("unknown condition {other:?}"),
        }
    }

    fn statement(connection: &rusqlite::Connection, stmt: &Value) -> rusqlite::Result<Value> {
        let args: Vec<SqlValue> = stmt["args"]
            .as_array()
            .map(|args| {
                args.iter()
                    .map(|v| SqlValue::from_hrana(v).unwrap())
                    .collect()
            })
            .unwrap_or_default();
        let mut statement = connection.prepare(stmt["sql"].as_str().unwrap())?;
        let cols: Vec<Value> = statement
            .column_names()
            .iter()
            .map(|n| json!({"name": n}))
            .collect();
        let width = cols.len();
        let mut rows = statement.query(rusqlite::params_from_iter(args.iter()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(
                (0..width)
                    .map(|i| Ok(SqlValue::from(row.get_ref(i)?).to_hrana()))
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        drop(rows);
        Ok(json!({"cols": cols, "rows": out, "affected_row_count": connection.changes()}))
    }

    fn answer(connection: &rusqlite::Connection, request: &Value, cap: usize) -> Value {
        let failed =
            |e: rusqlite::Error| json!({"type": "error", "error": {"message": e.to_string()}});
        match request["type"].as_str() {
            Some("batch") => batch(connection, request["batch"]["steps"].as_array().unwrap()),
            Some("sequence") => match connection.execute_batch(request["sql"].as_str().unwrap()) {
                Ok(()) => json!({"type": "ok", "response": {"type": "sequence"}}),
                Err(e) => failed(e),
            },
            Some("execute") => match statement(connection, &request["stmt"]) {
                Ok(result) if result.to_string().len() > cap => {
                    json!({"type": "error", "error": {"message": "Response is too large"}})
                }
                Ok(result) => {
                    json!({"type": "ok", "response": {"type": "execute", "result": result}})
                }
                Err(e) => failed(e),
            },
            _ => json!({"type": "ok", "response": {"type": "close"}}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_does_not_resolve_falls_back_to_the_listed_addresses() {
        let primary = test_primary::start();
        let lost = Remote::parse(
            &format!("http://r2d2.invalid:{}", primary.remote.port),
            None,
        )
        .unwrap();
        let error = lost.execute("SELECT 1", &[]).unwrap_err();
        assert!(
            is_unreachable(&error) && error.contains("resolve r2d2.invalid"),
            "{error}"
        );

        // A dead fallback is skipped; the bare IP takes the URL's port.
        let dead = format!("127.0.0.1:{}", test_primary::dead().port);
        let found = lost.with_fallbacks(&[dead, "127.0.0.1".into()]).unwrap();
        let reply = found.execute("SELECT 1", &[]).unwrap();
        assert_eq!(reply.rows, vec![vec![SqlValue::Integer(1)]]);
    }

    #[test]
    fn https_defaults_to_port_443_and_other_schemes_still_refuse() {
        let remote = Remote::parse("https://r2d2.tailnet.ts.net/", None).unwrap();
        assert_eq!((remote.host(), remote.port), ("r2d2.tailnet.ts.net", 443));
        assert!(remote.tls.is_some());
        let plain = Remote::parse("http://r2d2", None).unwrap();
        assert_eq!((plain.port, plain.tls.is_none()), (80, true));
        let error = Remote::parse("ftp://r2d2", None).unwrap_err();
        assert!(error.contains(KEY) && error.contains("https://"), "{error}");
    }

    #[test]
    fn a_tls_primary_answers_and_an_untrusted_certificate_fails_closed() {
        let primary = test_primary::start_tls();
        let reply = primary.remote.execute("SELECT 1", &[]).unwrap();
        assert_eq!(reply.rows, vec![vec![SqlValue::Integer(1)]]);

        // The same primary, trusted by an empty root store: the handshake
        // fails, the error names the key, and it never reads as an outage
        // (an outage queues mail; a bad certificate must not).
        let untrusted = primary
            .remote
            .clone()
            .with_tls_config(tls_config(rustls::RootCertStore::empty()).unwrap());
        let error = untrusted
            .execute("CREATE TABLE t (x INTEGER)", &[])
            .unwrap_err();
        assert!(
            error.contains(KEY) && error.contains("failed TLS") && !is_unreachable(&error),
            "{error}"
        );
        let tables = primary
            .remote
            .execute("SELECT count(*) FROM sqlite_master WHERE name = 't'", &[])
            .unwrap();
        assert_eq!(tables.rows, vec![vec![SqlValue::Integer(0)]]);
    }

    #[test]
    fn a_fallback_that_is_not_an_ip_refuses() {
        let remote = Remote::parse("http://r2d2:18080", None).unwrap();
        let error = remote.with_fallbacks(&["r2d2.lan".into()]).unwrap_err();
        assert!(error.contains(ADDRS_KEY), "{error}");
    }

    #[test]
    fn a_reply_timeout_says_the_primary_did_not_answer_and_still_fails_closed() {
        let remote = Remote::parse("http://r2d2:18080", None).unwrap();
        let quiet = remote.unreachable(&io_error(std::io::ErrorKind::WouldBlock.into()));
        assert!(
            quiet.contains("did not answer: no reply within 15s"),
            "{quiet}"
        );
        assert!(
            !quiet.contains(UNREACHABLE) && is_unreachable(&quiet),
            "{quiet}"
        );
        // The local-store advice predates the share: a local write today
        // splits the claim store, so the text must never send a reader there.
        assert!(!quiet.contains("local store"), "{quiet}");
        let refused = remote.unreachable("connect 1.2.3.4:18080: refused");
        assert!(
            refused.contains(UNREACHABLE) && is_unreachable(&refused),
            "{refused}"
        );
        assert!(!refused.contains("local store"), "{refused}");
    }
}

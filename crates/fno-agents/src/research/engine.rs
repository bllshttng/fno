//! Retrieve + store for `fno do research`: the ddgs backbone, a self-fetch
//! of every result behind the SSRF guard, and one `sources.jsonl` row per
//! source (`url, fetched_at, hash, extract, verified, reason`).
//!
//! A row is `verified=true` only after a successful text fetch produced a
//! content hash. Fetched page text is data, never instructions: nothing here
//! puts an extract into a prompt.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::pyfmt;

pub use super::html::{strip_html, unescape};

pub const MIN_QUERY_WORDS: usize = 2;
pub const MAX_EXTRACT_CHARS: usize = 4000;
pub const DEFAULT_MAX_RESULTS: i64 = 10;

const FETCH_TIMEOUT_S: u64 = 15;
const DDGS_TIMEOUT_S: u64 = 30;
const BODY_CAP_BYTES: u64 = 2_000_000;
const UA: &str = "fno-research/0.1 (+https://github.com/jasonnoahchoi/footnote)";
// urllib's HTTPRedirectHandler limits.
const MAX_REDIRECTIONS: usize = 10;
const MAX_REPEATS: usize = 4;
const CLAIM_TTL_MS: i64 = 2 * 3_600_000;

/// Why a round stopped before it stored anything.
#[derive(Debug)]
pub enum RoundError {
    /// The topic was empty or one word (exit 2).
    EmptyQuery(String),
    /// The ddgs backbone is missing, failed or timed out (exit 3).
    DdgsUnavailable(String),
    /// A local filesystem failure.
    Io(String),
}

// ---------------------------------------------------------------------------
// Query / slug
// ---------------------------------------------------------------------------

/// Collapse inline whitespace; refuse an empty or one-word topic.
pub fn normalize_query(topic: &str) -> Result<String, String> {
    let q = super::html::collapse_inline_ws(pyfmt::strip(topic));
    if q.is_empty() || pyfmt::split_ws(&q).len() < MIN_QUERY_WORDS {
        return Err(format!(
            "topic too thin: {}. Give at least {MIN_QUERY_WORDS} words, \
             e.g. fno do research \"CA CCLD financials\".",
            pyfmt::repr(topic)
        ));
    }
    Ok(q)
}

/// Filesystem-safe, stable per-topic slug (the sources.jsonl basename).
pub fn slugify(topic: &str) -> String {
    let lower = topic.to_lowercase();
    let mut s = String::with_capacity(lower.len());
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
            in_run = false;
        } else if !in_run {
            s.push('-');
            in_run = true;
        }
    }
    let trimmed = s.trim_matches('-');
    let capped: String = trimmed.chars().take(60).collect();
    let out = capped.trim_end_matches('-');
    if out.is_empty() {
        "topic".to_string()
    } else {
        out.to_string()
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// `<state_dir>/notes/research/`, created on first use.
pub fn research_dir() -> std::io::Result<PathBuf> {
    let state = crate::backlog::settings::state_dir().unwrap_or_else(|| PathBuf::from(".fno"));
    let d = crate::paths::resolve_loose(&state)
        .join("notes")
        .join("research");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn sources_path(topic: &str) -> std::io::Result<PathBuf> {
    Ok(research_dir()?.join(format!("{}.sources.jsonl", slugify(topic))))
}

// ---------------------------------------------------------------------------
// Backbone: ddgs search
// ---------------------------------------------------------------------------

/// The `subprocess.TimeoutExpired` text the Python leg surfaced.
pub fn ddgs_timeout_message(query: &str, max_results: i64) -> String {
    let argv = [
        "ddgs".to_string(),
        "text".to_string(),
        "-q".to_string(),
        query.to_string(),
        "-m".to_string(),
        max_results.to_string(),
        "-o".to_string(),
        "json".to_string(),
    ];
    let list: Vec<String> = argv.iter().map(|a| pyfmt::repr(a)).collect();
    format!(
        "ddgs search timed out: Command '[{}]' timed out after {DDGS_TIMEOUT_S} seconds",
        list.join(", ")
    )
}

fn first_json_output(dir: &Path) -> std::io::Result<Option<String>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && !name.starts_with('.') {
            let bytes = std::fs::read(entry.path())?;
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    Ok(None)
}

/// Run the ddgs CLI in a throwaway cwd; JSON from stdout, else from the
/// file ddgs wrote there.
fn run_ddgs(query: &str, max_results: i64) -> Result<String, RoundError> {
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::Builder::new()
        .prefix("fno-ddgs-")
        .tempdir()
        .map_err(|e| RoundError::Io(e.to_string()))?;
    let mut cmd = Command::new("ddgs");
    cmd.args([
        "text",
        "-q",
        query,
        "-m",
        &max_results.to_string(),
        "-o",
        "json",
    ])
    .current_dir(dir.path());
    let started = Instant::now();
    let out = match crate::bounded_cmd::output_with_timeout_result(cmd, DDGS_TIMEOUT_S) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RoundError::DdgsUnavailable(
                "ddgs not found. Install the backbone: `pip install ddgs` \
                 (or `pipx install ddgs`)."
                    .to_string(),
            ));
        }
        Err(e) => return Err(RoundError::DdgsUnavailable(format!("ddgs failed: {e}"))),
    };
    let rc = match (out.status.code(), out.status.signal()) {
        (Some(code), _) => code,
        (None, Some(sig)) => {
            if started.elapsed() >= Duration::from_secs(DDGS_TIMEOUT_S) {
                return Err(RoundError::DdgsUnavailable(ddgs_timeout_message(
                    query,
                    max_results,
                )));
            }
            -sig
        }
        (None, None) => -1,
    };
    let mut raw = String::from_utf8_lossy(&out.stdout).into_owned();
    if rc == 0 && pyfmt::strip(&raw).is_empty() {
        if let Some(text) =
            first_json_output(dir.path()).map_err(|e| RoundError::Io(e.to_string()))?
        {
            raw = text;
        }
    }
    if rc != 0 {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let err = pyfmt::strip(&stderr);
        let err = if err.is_empty() {
            format!("exit {rc}")
        } else {
            err.to_string()
        };
        return Err(RoundError::DdgsUnavailable(format!(
            "ddgs failed (rate-limited?): {err}"
        )));
    }
    Ok(raw)
}

/// Pull URLs out of ddgs JSON output, tolerant of key naming across versions.
pub fn parse_ddgs(raw: &str) -> Vec<String> {
    let raw = pyfmt::strip(raw);
    if raw.is_empty() {
        return Vec::new();
    }
    let Ok(data) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let data = match data {
        Value::Object(map) => [map.get("results"), map.get("data")]
            .into_iter()
            .flatten()
            .find(|v| pyfmt::truthy(v))
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        other => other,
    };
    let mut urls: Vec<String> = Vec::new();
    let Value::Array(rows) = &data else {
        return urls;
    };
    for row in rows {
        let Value::Object(row) = row else {
            continue;
        };
        let u = [row.get("href"), row.get("url"), row.get("link")]
            .into_iter()
            .flatten()
            .find(|v| pyfmt::truthy(v));
        if let Some(Value::String(u)) = u {
            let public = u.starts_with("http://") || u.starts_with("https://");
            if public && !urls.contains(u) {
                urls.push(u.clone());
            }
        }
    }
    urls
}

/// De-duped result URLs for `query`; empty when the backbone finds none.
pub fn search(query: &str, max_results: i64) -> Result<Vec<String>, RoundError> {
    Ok(parse_ddgs(&run_ddgs(query, max_results)?))
}

// ---------------------------------------------------------------------------
// SSRF guard
// ---------------------------------------------------------------------------

/// The parts of `urllib.parse.urlsplit` the guard reads.
struct UrlParts {
    scheme: String,
    netloc: String,
}

fn split_url(url: &str) -> UrlParts {
    let cleaned: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(*c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest: &str = &cleaned;
    let mut scheme = String::new();
    if let Some(i) = cleaned.find(':') {
        let head = &cleaned[..i];
        let valid = head.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && head
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if valid {
            scheme = head.to_ascii_lowercase();
            rest = &cleaned[i + 1..];
        }
    }
    let netloc = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            after[..end].to_string()
        }
        None => String::new(),
    };
    UrlParts { scheme, netloc }
}

/// `(hostname, port text)` from a netloc, as `SplitResult._hostinfo` reads it.
fn host_info(netloc: &str) -> (String, String) {
    let hostinfo = match netloc.rsplit_once('@') {
        Some((_, h)) => h,
        None => netloc,
    };
    if let Some((_, bracketed)) = hostinfo.split_once('[') {
        let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
        let port = after.split_once(':').map(|(_, p)| p).unwrap_or("");
        return (host.to_string(), port.to_string());
    }
    match hostinfo.split_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (hostinfo.to_string(), String::new()),
    }
}

fn v4_in(ip: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (u32::from(ip) & mask) == (u32::from(Ipv4Addr::from(net)) & mask)
}

fn v6_in(ip: Ipv6Addr, net: [u16; 8], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    (u128::from(ip) & mask) == (u128::from(Ipv6Addr::from(net)) & mask)
}

fn v4_blocked(ip: Ipv4Addr) -> bool {
    const PRIVATE: &[([u8; 4], u32)] = &[
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 0, 170], 31),
        ([192, 0, 2, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([240, 0, 0, 0], 4),
        ([255, 255, 255, 255], 32),
    ];
    const PRIVATE_EXCEPT: &[([u8; 4], u32)] = &[([192, 0, 0, 9], 32), ([192, 0, 0, 10], 32)];
    let private = PRIVATE.iter().any(|(n, p)| v4_in(ip, *n, *p))
        && !PRIVATE_EXCEPT.iter().any(|(n, p)| v4_in(ip, *n, *p));
    private
        || v4_in(ip, [127, 0, 0, 0], 8)
        || v4_in(ip, [169, 254, 0, 0], 16)
        || v4_in(ip, [240, 0, 0, 0], 4)
        || v4_in(ip, [224, 0, 0, 0], 4)
        || u32::from(ip) == 0
}

fn v6_blocked(ip: Ipv6Addr) -> bool {
    const PRIVATE: &[([u16; 8], u32)] = &[
        ([0, 0, 0, 0, 0, 0, 0, 1], 128),
        ([0, 0, 0, 0, 0, 0, 0, 0], 128),
        ([0, 0, 0, 0, 0, 0xffff, 0, 0], 96),
        ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48),
        ([0x100, 0, 0, 0, 0, 0, 0, 0], 64),
        ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23),
        ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
        ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
        ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
        ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
        ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
    ];
    const PRIVATE_EXCEPT: &[([u16; 8], u32)] = &[
        ([0x2001, 1, 0, 0, 0, 0, 0, 1], 128),
        ([0x2001, 1, 0, 0, 0, 0, 0, 2], 128),
        ([0x2001, 3, 0, 0, 0, 0, 0, 0], 32),
        ([0x2001, 4, 0x112, 0, 0, 0, 0, 0], 48),
        ([0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28),
        ([0x2001, 0x30, 0, 0, 0, 0, 0, 0], 28),
    ];
    const RESERVED: &[([u16; 8], u32)] = &[
        ([0, 0, 0, 0, 0, 0, 0, 0], 8),
        ([0x100, 0, 0, 0, 0, 0, 0, 0], 8),
        ([0x200, 0, 0, 0, 0, 0, 0, 0], 7),
        ([0x400, 0, 0, 0, 0, 0, 0, 0], 6),
        ([0x800, 0, 0, 0, 0, 0, 0, 0], 5),
        ([0x1000, 0, 0, 0, 0, 0, 0, 0], 4),
        ([0x4000, 0, 0, 0, 0, 0, 0, 0], 3),
        ([0x6000, 0, 0, 0, 0, 0, 0, 0], 3),
        ([0x8000, 0, 0, 0, 0, 0, 0, 0], 3),
        ([0xa000, 0, 0, 0, 0, 0, 0, 0], 3),
        ([0xc000, 0, 0, 0, 0, 0, 0, 0], 3),
        ([0xe000, 0, 0, 0, 0, 0, 0, 0], 4),
        ([0xf000, 0, 0, 0, 0, 0, 0, 0], 5),
        ([0xf800, 0, 0, 0, 0, 0, 0, 0], 6),
        ([0xfe00, 0, 0, 0, 0, 0, 0, 0], 9),
    ];
    let private = PRIVATE.iter().any(|(n, p)| v6_in(ip, *n, *p))
        && !PRIVATE_EXCEPT.iter().any(|(n, p)| v6_in(ip, *n, *p));
    let bits = u128::from(ip);
    private
        || bits == 1
        || v6_in(ip, [0xfe80, 0, 0, 0, 0, 0, 0, 0], 10)
        || RESERVED.iter().any(|(n, p)| v6_in(ip, *n, *p))
        || v6_in(ip, [0xff00, 0, 0, 0, 0, 0, 0, 0], 8)
        || bits == 0
}

/// Private, loopback, link-local, reserved, multicast or unspecified, with
/// an IPv4-mapped IPv6 address judged as its IPv4 address. Returns the
/// address the verdict was taken on.
pub fn blocked_ip(ip: IpAddr) -> Option<IpAddr> {
    let ip = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    };
    let blocked = match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => v6_blocked(v6),
    };
    blocked.then_some(ip)
}

/// The verdict over every address `host` resolved to.
pub fn check_resolved(host: &str, ips: &[IpAddr]) -> Result<(), String> {
    for ip in ips {
        if let Some(judged) = blocked_ip(*ip) {
            return Err(format!("blocked non-public host: {host} -> {judged}"));
        }
    }
    Ok(())
}

/// Refuse a non-http(s) scheme or a host resolving to a non-public address.
/// The error is the row's `reason`. The host is resolved here and again at
/// connect time, so a DNS rebind between the two is not closed.
pub fn guard_url(url: &str) -> Result<(), String> {
    let parts = split_url(url);
    let netloc = &parts.netloc;
    // urlsplit refuses an unbalanced bracket before any scheme check.
    if netloc.contains('[') != netloc.contains(']') {
        return Err("fetch error: Invalid IPv6 URL".to_string());
    }
    if parts.scheme != "http" && parts.scheme != "https" {
        let shown = if parts.scheme.is_empty() {
            "none"
        } else {
            parts.scheme.as_str()
        };
        return Err(format!("blocked scheme: {shown}"));
    }
    let (raw_host, port_text) = host_info(netloc);
    if raw_host.is_empty() {
        return Err("blocked: no host".to_string());
    }
    let host = match raw_host.split_once('%') {
        Some((h, zone)) => format!("{}%{zone}", h.to_lowercase()),
        None => raw_host.to_lowercase(),
    };
    let port: u16 = if port_text.is_empty() {
        0
    } else if port_text.chars().all(|c| c.is_ascii_digit()) {
        match port_text.parse::<u32>() {
            Ok(p) if p <= 65535 => p as u16,
            _ => return Err("fetch error: Port out of range 0-65535".to_string()),
        }
    } else {
        return Err(format!(
            "fetch error: Port could not be cast to integer value as {}",
            pyfmt::repr(&port_text)
        ));
    };
    let addrs = match (host.as_str(), port).to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(e) => return Err(format!("unresolvable host: {host} ({e})")),
    };
    let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
    check_resolved(&host, &ips)
}

// ---------------------------------------------------------------------------
// Self-fetch + extract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct FetchResult {
    pub ok: bool,
    pub text: String,
    pub content_type: String,
    pub status: Option<i64>,
    pub reason: String,
}

impl FetchResult {
    fn failed(status: Option<i64>, reason: String) -> Self {
        FetchResult {
            ok: false,
            text: String::new(),
            content_type: String::new(),
            status,
            reason,
        }
    }
}

/// `\b(text/|application/(json|xml|xhtml))`, case-insensitive.
pub fn text_content_type(ct: &str) -> bool {
    static RE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\b(text/|application/(json|xml|xhtml))").ok())
        .as_ref()
        .is_some_and(|re| re.is_match(ct))
}

/// `Message.get_content_charset()`: the lowercased `charset` parameter.
pub fn content_charset(ct: &str) -> Option<String> {
    for param in ct.split(';').skip(1) {
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        if key.trim().to_ascii_lowercase() != "charset" {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        return Some(value.to_lowercase());
    }
    None
}

const CP1252_HIGH: [Option<char>; 32] = [
    Some('\u{20ac}'),
    None,
    Some('\u{201a}'),
    Some('\u{0192}'),
    Some('\u{201e}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02c6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017d}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201c}'),
    Some('\u{201d}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02dc}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203a}'),
    Some('\u{0153}'),
    None,
    Some('\u{017e}'),
    Some('\u{0178}'),
];

/// `bytes.decode(charset, errors="replace")` for the charsets pages declare
/// most; any other name decodes as UTF-8.
pub fn decode_body(raw: &[u8], charset: &str) -> String {
    let name = charset.trim().to_ascii_lowercase().replace('_', "-");
    match name.as_str() {
        "iso-8859-1" | "iso8859-1" | "latin-1" | "latin1" | "l1" | "8859" | "cp819" => {
            raw.iter().map(|&b| char::from(b)).collect()
        }
        "us-ascii" | "ascii" | "646" => raw
            .iter()
            .map(|&b| if b < 0x80 { char::from(b) } else { '\u{fffd}' })
            .collect(),
        "windows-1252" | "cp1252" => raw
            .iter()
            .map(|&b| {
                if (0x80..0xa0).contains(&b) {
                    CP1252_HIGH[(b - 0x80) as usize].unwrap_or('\u{fffd}')
                } else {
                    char::from(b)
                }
            })
            .collect(),
        _ => String::from_utf8_lossy(raw).into_owned(),
    }
}

/// One HTTP exchange, redirects not followed.
struct Hop {
    status: i64,
    content_type: String,
    redirect_url: String,
    body_path: PathBuf,
}

fn curl_once(url: &str, dir: &Path, n: usize) -> Result<Hop, String> {
    let body_path = dir.join(format!("body-{n}"));
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-g", "--proto", "=http,https"])
        .args(["--max-time", &FETCH_TIMEOUT_S.to_string()])
        .args(["-A", UA, "-o"])
        .arg(&body_path)
        .args(["-w", "%{http_code}\\n%{content_type}\\n%{redirect_url}"])
        .arg(url);
    let out = match crate::bounded_cmd::output_with_timeout_result(cmd, FETCH_TIMEOUT_S + 5) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("fetch error: curl not found on PATH".to_string());
        }
        Err(e) => return Err(format!("fetch error: {e}")),
    };
    match out.status.code() {
        Some(0) => {}
        Some(_) => {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(format!("fetch error: {}", pyfmt::strip(&stderr)));
        }
        None => return Err("fetch error: timed out".to_string()),
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = stdout.splitn(3, '\n');
    let status = lines
        .next()
        .unwrap_or("")
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    let content_type = lines.next().unwrap_or("").trim().to_string();
    let redirect_url = lines.next().unwrap_or("").trim().to_string();
    Ok(Hop {
        status,
        content_type,
        redirect_url,
        body_path,
    })
}

fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    // curl writes no file for an empty body.
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut buf = Vec::new();
    file.take(BODY_CAP_BYTES).read_to_end(&mut buf)?;
    Ok(buf)
}

/// GET `url`, re-vetting every redirect hop. A non-text or failed response
/// returns `ok=false` with a reason, never an error, so one bad source never
/// aborts a round.
pub fn fetch_url(url: &str) -> FetchResult {
    if let Err(reason) = guard_url(url) {
        return FetchResult::failed(None, reason);
    }
    let dir = match tempfile::Builder::new().prefix("fno-research-").tempdir() {
        Ok(d) => d,
        Err(e) => return FetchResult::failed(None, format!("fetch error: {e}")),
    };
    let mut current = url.to_string();
    let mut visited: HashMap<String, usize> = HashMap::new();
    let mut hop_n = 0usize;
    loop {
        let hop = match curl_once(&current, dir.path(), hop_n) {
            Ok(h) => h,
            Err(reason) => return FetchResult::failed(None, reason),
        };
        hop_n += 1;
        let code = hop.status;
        if matches!(code, 301 | 302 | 303 | 307 | 308) {
            let next = hop.redirect_url;
            if next.is_empty() {
                return FetchResult::failed(Some(code), format!("http {code}"));
            }
            let scheme = split_url(&next).scheme;
            if !matches!(scheme.as_str(), "http" | "https" | "ftp" | "") {
                return FetchResult::failed(Some(code), format!("http {code}"));
            }
            if let Err(reason) = guard_url(&next) {
                return FetchResult::failed(None, reason);
            }
            let seen = visited.get(&next).copied().unwrap_or(0);
            if visited.len() >= MAX_REDIRECTIONS || seen >= MAX_REPEATS {
                return FetchResult::failed(Some(code), format!("http {code}"));
            }
            visited.insert(next.clone(), seen + 1);
            current = next;
            continue;
        }
        if !(200..300).contains(&code) {
            return FetchResult::failed(Some(code), format!("http {code}"));
        }
        let ct = hop.content_type;
        if !text_content_type(&ct) {
            let shown = if ct.is_empty() {
                "unknown"
            } else {
                ct.as_str()
            };
            let reason = format!("non-text content-type: {shown}");
            return FetchResult {
                ok: false,
                text: String::new(),
                content_type: ct,
                status: Some(code),
                reason,
            };
        }
        let raw = match read_capped(&hop.body_path) {
            Ok(b) => b,
            Err(e) => return FetchResult::failed(None, format!("fetch error: {e}")),
        };
        let charset = content_charset(&ct)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "utf-8".to_string());
        let text = decode_body(&raw, &charset);
        return FetchResult {
            ok: true,
            text: strip_html(&text),
            content_type: ct,
            status: Some(code),
            reason: String::new(),
        };
    }
}

pub fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// Evidence rows
// ---------------------------------------------------------------------------

/// One evidence row. Fields read back from the store keep whatever JSON
/// type the row carried; the key order is the eval's contract.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub url: Value,
    pub fetched_at: Value,
    pub hash: Value,
    pub extract: Value,
    pub verified: bool,
    pub reason: Value,
}

impl Source {
    pub fn to_json_line(&self) -> String {
        format!(
            "{{\"url\": {}, \"fetched_at\": {}, \"hash\": {}, \"extract\": {}, \
             \"verified\": {}, \"reason\": {}}}",
            pyfmt::json_dumps(&self.url),
            pyfmt::json_dumps(&self.fetched_at),
            pyfmt::json_dumps(&self.hash),
            pyfmt::json_dumps(&self.extract),
            if self.verified { "true" } else { "false" },
            pyfmt::json_dumps(&self.reason),
        )
    }
}

/// Turn a fetch into an evidence row: verified only on a hashed text fetch.
pub fn build_source(url: &str, fetch: &FetchResult, fetched_at: &str) -> Source {
    if !fetch.ok {
        return Source {
            url: Value::String(url.to_string()),
            fetched_at: Value::String(fetched_at.to_string()),
            hash: Value::String(String::new()),
            extract: Value::String(String::new()),
            verified: false,
            reason: Value::String(fetch.reason.clone()),
        };
    }
    let extract: String = fetch.text.chars().take(MAX_EXTRACT_CHARS).collect();
    Source {
        url: Value::String(url.to_string()),
        fetched_at: Value::String(fetched_at.to_string()),
        hash: Value::String(content_hash(&fetch.text)),
        extract: Value::String(extract),
        verified: true,
        reason: Value::String(String::new()),
    }
}

pub fn append_source(path: &Path, source: &Source) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    fh.write_all(format!("{}\n", source.to_json_line()).as_bytes())
}

/// Parse sources.jsonl text, skipping malformed rows.
pub fn parse_sources(text: &str) -> Vec<Source> {
    let empty = || Value::String(String::new());
    let mut out = Vec::new();
    // Rows end at `\n` only: an older writer left U+2028 raw inside a row.
    for line in text.split('\n') {
        let line = pyfmt::strip(line);
        if line.is_empty() {
            continue;
        }
        let Ok(Value::Object(d)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(url) = d.get("url") else {
            continue;
        };
        out.push(Source {
            url: url.clone(),
            fetched_at: d.get("fetched_at").cloned().unwrap_or_else(empty),
            hash: d.get("hash").cloned().unwrap_or_else(empty),
            extract: d.get("extract").cloned().unwrap_or_else(empty),
            verified: d.get("verified").is_some_and(pyfmt::truthy),
            reason: d.get("reason").cloned().unwrap_or_else(empty),
        });
    }
    out
}

/// The rows of a sources.jsonl; a missing file reads as no rows.
pub fn read_sources(path: &Path) -> std::io::Result<Vec<Source>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path)?;
    Ok(parse_sources(&String::from_utf8_lossy(&bytes)))
}

// ---------------------------------------------------------------------------
// Topic single-writer claim
// ---------------------------------------------------------------------------

fn topic_key(slug: &str) -> String {
    format!("node:research:{slug}")
}

/// Hold `node:research:<slug>` so the cache has one writer. `false` only
/// when the claim is refused (held elsewhere, or the store would not take
/// it); the claim is a guard, never a hard dependency on anything else.
pub fn acquire_topic(slug: &str, holder: &str) -> bool {
    use crate::claims::{self, AcquireOpts, AcquireOutcome, ClaimRecord};
    let key = topic_key(slug);
    let ttl_ms = Some(CLAIM_TTL_MS);
    let pid = claims::open_session_pid().map(|p| p as u32);
    let pid_unavailable = pid.is_none();
    if claims::validate_inputs(&key, holder, ttl_ms, pid, pid_unavailable).is_err() {
        return false;
    }
    let (_, ambient_harness) = claims::resolve_identity();
    let provenance = if pid.is_none() || !claims::pid_dies_with_session(ambient_harness.as_deref())
    {
        "ambient"
    } else {
        match crate::spawn_context::session_identity_ambient(std::process::id()).0 {
            Some(p) if Some(p) == pid => "session-prover",
            _ => "ambient",
        }
    };
    let opts = AcquireOpts {
        pid,
        pid_unavailable,
        ttl_ms,
        reason: None,
        metadata: Some(serde_json::Map::new()),
        pid_provenance: Some(provenance.to_string()),
        root: crate::claim_cli::node_aware_root(&key),
        host: None,
        events_dir: None,
        identity: None,
    };
    let witness = |record: &ClaimRecord| {
        let (witness, _drain) = crate::claim_commands::default_session_witness();
        witness(record)
    };
    let witness: claims::SessionWitness<'_> = &witness;
    matches!(
        claims::acquire_with_session_witness(&key, holder, opts, Some(witness)),
        AcquireOutcome::Acquired(_)
    )
}

pub fn release_topic(slug: &str, holder: &str) {
    let key = topic_key(slug);
    let root = crate::claim_cli::node_aware_root(&key);
    let _ = crate::claims::release(&key, holder, root.as_deref(), None);
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

pub const NOTE_CLAIMED: &str = "topic claimed by another writer";
pub const NOTE_NO_SOURCES: &str = "no sources found";

#[derive(Debug, Clone, PartialEq)]
pub struct RoundResult {
    pub topic: String,
    pub slug: String,
    pub sources_path: PathBuf,
    pub found: usize,
    pub verified: usize,
    pub failed: usize,
    pub note: String,
}

fn now_iso() -> String {
    pyfmt::isoformat(&chrono::Utc::now())
}

/// One retrieve+store round: validate, claim, search, self-fetch, append.
/// Always leaves a (possibly empty) sources file so downstream has a path.
pub fn run_round(topic: &str, max_results: i64, claim: bool) -> Result<RoundResult, RoundError> {
    let query = normalize_query(topic).map_err(RoundError::EmptyQuery)?;
    let slug = slugify(&query);
    let holder = format!("research:{slug}");
    let out = sources_path(&query).map_err(|e| RoundError::Io(e.to_string()))?;
    if claim && !acquire_topic(&slug, &holder) {
        return Ok(RoundResult {
            topic: query,
            slug,
            sources_path: out,
            found: 0,
            verified: 0,
            failed: 0,
            note: NOTE_CLAIMED.to_string(),
        });
    }
    let counts = fill_round(&query, max_results, &out);
    if claim {
        release_topic(&slug, &holder);
    }
    let (found, verified, failed, note) = counts?;
    Ok(RoundResult {
        topic: query,
        slug,
        sources_path: out,
        found,
        verified,
        failed,
        note: note.to_string(),
    })
}

/// Search, self-fetch and append: `(found, verified, failed, note)`.
fn fill_round(
    query: &str,
    max_results: i64,
    out: &Path,
) -> Result<(usize, usize, usize, &'static str), RoundError> {
    let urls = search(query, max_results)?;
    if urls.is_empty() {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(out)
            .map_err(|e| RoundError::Io(e.to_string()))?;
        return Ok((0, 0, 0, NOTE_NO_SOURCES));
    }
    let (mut verified, mut failed) = (0usize, 0usize);
    for url in &urls {
        let src = build_source(url, &fetch_url(url), &now_iso());
        append_source(out, &src).map_err(|e| RoundError::Io(e.to_string()))?;
        if src.verified {
            verified += 1;
        } else {
            failed += 1;
        }
    }
    Ok((urls.len(), verified, failed, ""))
}

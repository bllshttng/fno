//! The provider error taxonomy: one call's outcome (HTTP status and body, or
//! a CLI exit code and stderr) classed into a closed [`ErrorClass`] set, the
//! cooldown rule it earns ([`classify_error`]) and the reset time its body
//! names ([`reset_epoch_from`]). Ported from
//! `fno.adapters.providers.error_taxonomy`; the parity cases live in
//! `tests/accounts_error_class_parity.rs`.
//!
//! Every function takes the observation time as `now` instead of reading the
//! clock, so a replay over recorded refusals bounds its stamps by the event's
//! own time.

use chrono::{NaiveDateTime, TimeZone as _};
use serde_json::{json, Value};

/// Body markers that make a successful exit a quota exhaustion. "usage limit"
/// is the claude CLI's own phrasing ("Claude usage limit reached").
pub const QUOTA_BODY_MARKERS: [&str; 3] = ["rate limit", "quota exceeded", "usage limit"];

const BODY_EXCERPT_LEN: usize = 256;
const MODEL_ID_MAX_LEN: usize = 256;

/// The furthest ahead a rate-limit window may reopen. The lock it feeds has
/// no TTL, so a misread stamp (a millisecond epoch read as seconds, a token
/// expiry in the body) would otherwise take a provider out for a year.
pub const MAX_RESET_HORIZON_S: f64 = 14.0 * 24.0 * 3600.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Provider5xx,
    Provider4xxAuth,
    Provider4xxQuota,
    ParserError,
    Unknown,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider5xx => "provider_5xx",
            Self::Provider4xxAuth => "provider_4xx_auth",
            Self::Provider4xxQuota => "provider_4xx_quota",
            Self::ParserError => "parser_error",
            Self::Unknown => "unknown",
        }
    }

    /// A swap gives the next account a turn: a 5xx, an auth refusal (the
    /// credential is bound to the record) or a quota refusal.
    pub fn triggers_swap(self) -> bool {
        matches!(
            self,
            Self::Provider5xx | Self::Provider4xxAuth | Self::Provider4xxQuota
        )
    }
}

/// One classified outcome. `resets_at` is the epoch the body says the window
/// reopens; `reset_stamp_unparsed` is a stamp that was present but refused (a
/// naive stamp with no zone); `reset_is_derived` marks an epoch built from the
/// window the body NAMES plus `now`, a bound never shorter than the real reset.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedError {
    pub error_class: ErrorClass,
    pub raw_status: Option<i64>,
    pub raw_exit_code: Option<i64>,
    pub body_excerpt: String,
    pub model: Option<String>,
    pub resets_at: Option<f64>,
    pub reset_stamp_unparsed: Option<String>,
    pub reset_is_derived: bool,
}

impl NormalizedError {
    pub fn to_json(&self) -> Value {
        json!({
            "error_class": self.error_class.as_str(),
            "raw_status": self.raw_status,
            "raw_exit_code": self.raw_exit_code,
            "body_excerpt": self.body_excerpt,
            "triggers_swap": self.error_class.triggers_swap(),
            "model": self.model,
            "resets_at": self.resets_at,
            "reset_stamp_unparsed": self.reset_stamp_unparsed,
            "reset_is_derived": self.reset_is_derived,
        })
    }
}

/// One provider call's outcome, as [`normalize`] reads it. No `Default`: a
/// defaulted `now` of 0 would refuse every reset stamp as past the horizon.
#[derive(Clone, Copy, Debug)]
pub struct Outcome<'a> {
    pub http_status: Option<i64>,
    pub exit_code: Option<i64>,
    pub body: &'a str,
    /// The caller already tried the provider's schema and it failed: the
    /// authoritative parser-error signal.
    pub parser_failed: bool,
    pub model: Option<&'a str>,
    /// IANA name from `accounts.<id>.reset_timezone`; read only for a naive
    /// reset stamp.
    pub reset_timezone: Option<&'a str>,
    pub now: f64,
}

pub fn matches_quota_body(body: &str) -> bool {
    let lowered = body.to_lowercase();
    QUOTA_BODY_MARKERS.iter().any(|m| lowered.contains(m))
}

fn classify(http_status: Option<i64>, body: &str, parser_failed: bool) -> ErrorClass {
    match http_status {
        Some(500 | 502 | 503 | 504 | 529) => return ErrorClass::Provider5xx,
        Some(401 | 403) => return ErrorClass::Provider4xxAuth,
        Some(402 | 429) => return ErrorClass::Provider4xxQuota,
        _ => {}
    }
    if matches_quota_body(body) {
        return ErrorClass::Provider4xxQuota;
    }
    // A 200 whose body is HTML where JSON was expected is an upstream error
    // page; callers that really parsed pass `parser_failed` instead.
    if parser_failed || (http_status == Some(200) && body.trim_start().starts_with('<')) {
        return ErrorClass::ParserError;
    }
    ErrorClass::Unknown
}

/// Classify a provider call. Refuses an empty model id, which would later
/// be an unusable lock key.
pub fn normalize(o: &Outcome) -> Result<NormalizedError, String> {
    if o.model == Some("") {
        return Err("NormalizedError.model must be a non-empty string when set".into());
    }
    let error_class = classify(o.http_status, o.body, o.parser_failed);
    let (mut resets_at, reset_stamp_unparsed) = parse_reset_stamp(o.body, o.reset_timezone, o.now);
    let mut reset_is_derived = false;
    // Only a quota refusal claims a window: "valid for 30 minutes" in an auth
    // body must not lock the provider for thirty minutes.
    if resets_at.is_none() && error_class == ErrorClass::Provider4xxQuota {
        if let Some(span) = reset_window_seconds_from(o.body) {
            resets_at = Some(o.now + span);
            reset_is_derived = true;
        }
    }
    Ok(NormalizedError {
        error_class,
        raw_status: o.http_status,
        raw_exit_code: o.exit_code,
        body_excerpt: o.body.chars().take(BODY_EXCERPT_LEN).collect(),
        model: o.model.map(|m| m.chars().take(MODEL_ID_MAX_LEN).collect()),
        resets_at,
        reset_stamp_unparsed,
        reset_is_derived,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleMatch {
    /// Case-insensitive substring of the body.
    Text(&'static str),
    Status(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cooldown {
    FixedMs(u64),
    Backoff,
}

/// One row of [`ERROR_RULES`]: what it matches and the cooldown it earns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorRule {
    pub on: RuleMatch,
    pub cooldown: Cooldown,
}

impl ErrorRule {
    pub fn to_json(&self) -> Value {
        let (text, status) = match self.on {
            RuleMatch::Text(t) => (Some(t), None),
            RuleMatch::Status(s) => (None, Some(s)),
        };
        let (cooldown_ms, backoff) = match self.cooldown {
            Cooldown::FixedMs(ms) => (Some(ms), false),
            Cooldown::Backoff => (None, true),
        };
        json!({ "text": text, "status": status, "cooldown_ms": cooldown_ms, "backoff": backoff })
    }
}

/// Auth and credential errors: the operator must step in.
pub const COOLDOWN_LONG_MS: u64 = 2 * 60 * 1000;
/// Transient request-shape problems: another provider may succeed at once.
pub const COOLDOWN_SHORT_MS: u64 = 5 * 1000;

const fn text(t: &'static str, cooldown: Cooldown) -> ErrorRule {
    ErrorRule {
        on: RuleMatch::Text(t),
        cooldown,
    }
}

const fn status(s: i64, cooldown: Cooldown) -> ErrorRule {
    ErrorRule {
        on: RuleMatch::Status(s),
        cooldown,
    }
}

/// The cooldown rules, first match wins: every text rule before every status
/// rule. The quota markers are the same list [`normalize`] reads, so a
/// marker that classes a quota refusal also earns its cooldown.
pub static ERROR_RULES: [ErrorRule; 14] = [
    text("no credentials", Cooldown::FixedMs(COOLDOWN_LONG_MS)),
    text("request not allowed", Cooldown::FixedMs(COOLDOWN_SHORT_MS)),
    text(
        "improperly formed request",
        Cooldown::FixedMs(COOLDOWN_LONG_MS),
    ),
    text(QUOTA_BODY_MARKERS[0], Cooldown::Backoff),
    text(QUOTA_BODY_MARKERS[1], Cooldown::Backoff),
    text(QUOTA_BODY_MARKERS[2], Cooldown::Backoff),
    text("too many requests", Cooldown::Backoff),
    text("capacity", Cooldown::Backoff),
    text("overloaded", Cooldown::Backoff),
    status(401, Cooldown::FixedMs(COOLDOWN_LONG_MS)),
    status(402, Cooldown::FixedMs(COOLDOWN_LONG_MS)),
    status(403, Cooldown::FixedMs(COOLDOWN_LONG_MS)),
    status(404, Cooldown::FixedMs(COOLDOWN_LONG_MS)),
    status(429, Cooldown::Backoff),
];

/// The first rule `status` and `body` match. A `None` body skips the text
/// rules. `None` means no rule: the caller keeps the [`normalize`] class.
pub fn classify_error(status: Option<i64>, body: Option<&str>) -> Option<&'static ErrorRule> {
    let lowered = body.map(str::to_lowercase);
    ERROR_RULES.iter().find(|rule| match rule.on {
        RuleMatch::Text(t) => lowered.as_deref().is_some_and(|b| b.contains(t)),
        RuleMatch::Status(s) => status == Some(s),
    })
}

fn regex(cell: &'static std::sync::OnceLock<regex::Regex>, pattern: &str) -> &'static regex::Regex {
    cell.get_or_init(|| regex::Regex::new(pattern).unwrap())
}

macro_rules! stamp {
    () => {
        r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?"
    };
}

/// An ISO-8601 stamp carrying its own offset, to epoch seconds.
fn offset_stamp_epoch(stamp: &str) -> Option<f64> {
    let (body, offset) = match stamp.strip_suffix('Z') {
        Some(body) => (body, "+00:00".to_string()),
        None => {
            let at = stamp.rfind(['+', '-'])?;
            let off = &stamp[at..];
            let off = if off.len() == 5 {
                format!("{}:{}", &off[..3], &off[3..])
            } else {
                off.to_string()
            };
            (&stamp[..at], off)
        }
    };
    let dt = chrono::DateTime::parse_from_rfc3339(&format!("{}{offset}", full_time(body)?)).ok()?;
    Some(micros_epoch(dt.timestamp(), dt.timestamp_subsec_micros()))
}

/// `date[T ]HH:MM[:SS][.frac]` with the `T` and the seconds always present.
fn full_time(stamp: &str) -> Option<String> {
    let (date, time) = (stamp.get(..10)?, stamp.get(11..)?);
    let time = if time.len() == 5 {
        format!("{time}:00")
    } else {
        time.to_string()
    };
    Some(format!("{date}T{time}"))
}

/// Epoch seconds the way Python's `timedelta.total_seconds` sums them, so a
/// fractional stamp agrees to the last bit.
fn micros_epoch(secs: i64, micros: u32) -> f64 {
    (secs * 1_000_000 + i64::from(micros)) as f64 / 1e6
}

/// A naive local time resolved in the IANA zone `tz`. An ambiguous time takes
/// the earlier instant and a skipped one the offset before the gap, as
/// Python's `fold=0` does. An unknown zone name is `None`.
pub fn naive_epoch_in_zone(naive: NaiveDateTime, tz: &str) -> Option<f64> {
    let zone = tz.parse::<chrono_tz::Tz>().ok()?;
    let micros = naive.and_utc().timestamp_subsec_micros();
    if let Some(dt) = zone.from_local_datetime(&naive).earliest() {
        return Some(micros_epoch(dt.timestamp(), micros));
    }
    use chrono::Offset as _;
    let before = zone.offset_from_utc_datetime(&(naive - chrono::Duration::hours(24)));
    let secs = naive.and_utc().timestamp() - i64::from(before.fix().local_minus_utc());
    Some(micros_epoch(secs, micros))
}

/// `(epoch, unparsed_stamp)` for the reset time `body` names; at most one is
/// set. An offset-bearing stamp and a reset-keyed epoch resolve on their own.
/// A naive stamp resolves only in an explicit `tz`, because a wrong-by-eight-
/// hours reset unlocks a capped provider early or locks a healthy one out;
/// without one it comes back as the unparsed stamp. Anything past
/// [`MAX_RESET_HORIZON_S`] from `now` is a misread and reads `None`.
pub fn parse_reset_stamp(body: &str, tz: Option<&str>, now: f64) -> (Option<f64>, Option<String>) {
    static OFFSET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static EPOCH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static NAIVE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let sane = |epoch: Option<f64>| epoch.filter(|e| *e <= now + MAX_RESET_HORIZON_S);
    if body.is_empty() {
        return (None, None);
    }
    if let Some(m) = regex(&OFFSET, concat!(stamp!(), r"(?:Z|[+-]\d{2}:?\d{2})")).find(body) {
        return (sane(offset_stamp_epoch(m.as_str())), None);
    }
    // An epoch is read only under a reset-shaped key: a bare ten-digit number
    // is as likely a request id. A run of 13 digits is milliseconds, and a
    // run outside 9..=13 digits is no epoch at all.
    let epoch_re = regex(&EPOCH, r#"reset[a-zA-Z_]*["'\s:=]+(\d+)"#);
    if let Some(digits) = epoch_re
        .captures_iter(body)
        .map(|c| c.get(1).unwrap().as_str())
        .find(|d| (9..=13).contains(&d.len()))
    {
        let raw: f64 = digits.parse().ok().unwrap_or(f64::INFINITY);
        return (
            sane(Some(if digits.len() > 11 { raw / 1000.0 } else { raw })),
            None,
        );
    }
    let Some(m) = regex(&NAIVE, stamp!()).find(body) else {
        return (None, None);
    };
    let stamp = m.as_str().to_string();
    let resolved = tz.filter(|tz| !tz.is_empty()).and_then(|tz| {
        let naive =
            NaiveDateTime::parse_from_str(&full_time(&stamp)?, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
        naive_epoch_in_zone(naive, tz)
    });
    match sane(resolved) {
        Some(epoch) => (Some(epoch), None),
        None => (None, Some(stamp)),
    }
}

/// The reset epoch `body` names, or `None`: the one-call form for the paths
/// that write the provider lock from refusal text.
pub fn reset_epoch_from(body: &str, tz: Option<&str>, now: f64) -> Option<f64> {
    parse_reset_stamp(body, tz, now).0
}

/// The span of the window `body` NAMES (a "for 5 hour" clause),
/// in seconds. The name is the offset-free half of a vendor message, so
/// `now + span` is a bound that needs no timezone guess. `None` when no
/// window is named or it exceeds the horizon.
pub fn reset_window_seconds_from(body: &str) -> Option<f64> {
    static WINDOW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let c = regex(
        &WINDOW,
        r"(?i)\bfor\s+(?:(\d+)\s+)?(minute|hour|day|week|month)s?\b",
    )
    .captures(body)?;
    let count: f64 = match c.get(1) {
        Some(n) => n.as_str().parse().ok()?,
        None => 1.0,
    };
    let unit = match c[2].to_lowercase().as_str() {
        "minute" => 60.0,
        "hour" => 3600.0,
        "day" => 86400.0,
        "week" => 604800.0,
        _ => 2592000.0,
    };
    let span = count * unit;
    (count > 0.0 && span <= MAX_RESET_HORIZON_S).then_some(span)
}

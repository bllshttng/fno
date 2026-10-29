//! The per-account usage probe: read remaining quota and reset times before a
//! dispatch decision instead of burning a failed call to learn them. Ported
//! from `fno.adapters.providers.usage`; the parity cases live in
//! `tests/accounts_usage_probe_parity.rs`.
//!
//! A probe never raises and never writes the runtime-state file. Every
//! failure comes back as `(None, reason)`, a stable non-secret slug, so the
//! caller degrades to reactive behavior. A snapshot carries only labels,
//! percentages and epoch seconds; no bearer is logged or returned.

use crate::accounts::binding::{self, BindingRequest, SlotProbe, MATCHED, MISMATCH};
use crate::accounts::reconcile;
use crate::accounts::records::Record;
use crate::accounts::state::{py_float, Snapshot, Window};
use crate::accounts::store::{self, Host};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ANTHROPIC_API: &str = "https://api.anthropic.com";
const CLAUDE_USER_AGENT: &str = "claude-code/2.1.0";
const ZAI_LIMIT_TYPES: [&str; 3] = ["TOKENS_LIMIT", "TIME_LIMIT", "CREDIT_LIMIT"];
const ZAI_HOSTS: [&str; 2] = ["api.z.ai", "open.bigmodel.cn"];
/// A z.ai reset is the window's own only within this many spans ahead; the
/// TIME_LIMIT row carries the plan-period reset, not its window's.
const ZAI_RESET_TOLERANCE: f64 = 2.0;
/// A refused auto-reconcile waits this long before the next probe retries.
const RECONCILE_BACKOFF_S: f64 = 300.0;

/// z.ai unit codes in minutes. Unit 5 is a month: the TIME_LIMIT row counts
/// monthly tool calls (4000 on Max) and resets at the plan period, so the
/// one-minute reading Python gave it put a multi-day window at 1m.
fn zai_unit_minutes(unit: i64) -> Option<i64> {
    match unit {
        1 => Some(1440),
        3 => Some(60),
        5 => Some(43_200),
        6 => Some(10_080),
        _ => None,
    }
}

/// The reads a probe makes outside the process.
pub struct Ctx<'a> {
    pub cwd: &'a Path,
    pub repo_root: Option<&'a Path>,
    /// The managed store root; `None` when no state dir resolves.
    pub store: Option<PathBuf>,
    /// Every configured record, for naming a proven slot.
    pub records: &'a [Record],
    pub slot: &'a dyn SlotProbe,
    pub host: &'a dyn Host,
    /// One HTTP GET from a curl `-K` config: the status and the raw body.
    pub http: &'a dyn Fn(&str) -> Result<(u16, String), ()>,
    /// `dispatch_env` for one record id; `None` when it refuses.
    pub route_env: &'a dyn Fn(&str) -> Option<BTreeMap<String, String>>,
    pub codex_home: PathBuf,
}

fn system_http(config: &str) -> Result<(u16, String), ()> {
    crate::claude_vault::curl_raw(config).map_err(|_| ())
}

/// Run `f` with the live probe context: the Keychain, the profile and usage
/// endpoints over curl, and the launch env.
pub fn with_system<T>(
    cwd: &Path,
    repo_root: Option<&Path>,
    records: &[Record],
    f: impl FnOnce(&Ctx<'_>) -> T,
) -> T {
    let route_env = |id: &str| {
        crate::accounts::env::launch_env(
            id,
            repo_root,
            None,
            cwd,
            &crate::accounts::env::Sources::system(),
        )
        .ok()
    };
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".codex")
        });
    let ctx = Ctx {
        cwd,
        repo_root,
        store: crate::accounts::slot::store_root(cwd),
        records,
        slot: &binding::SystemProbe,
        host: &store::SystemHost,
        http: &system_http,
        route_env: &route_env,
        codex_home,
    };
    f(&ctx)
}

/// `_iso_to_epoch`: a number passes through; an ISO-8601 string parses with
/// a trailing `Z` read as UTC.
pub fn iso_to_epoch(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) if !s.is_empty() => crate::client_commands::parse_iso8601(s)
            .map(|dt| dt.timestamp_micros() as f64 / 1_000_000.0),
        _ => None,
    }
}

/// `_label_for_minutes`: one label vocabulary for every lane, derived from a
/// span the payload states.
pub fn label_for_minutes(minutes: Option<i64>) -> String {
    match minutes {
        None => "unknown".to_string(),
        Some(300) => "5h".to_string(),
        Some(10_080) => "weekly".to_string(),
        Some(m) if m % 1440 == 0 => format!("{}d", m / 1440),
        Some(m) if m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
    }
}

fn window(label: String, used_pct: f64, resets_at: Option<f64>) -> Window {
    Window {
        label,
        used_pct: used_pct.clamp(0.0, 100.0),
        resets_at,
    }
}

fn claude_label(key: &str) -> String {
    match key {
        "five_hour" => "5h".to_string(),
        "seven_day" => "weekly".to_string(),
        _ => match key.strip_prefix("seven_day_") {
            Some(model) => format!("weekly-{model}"),
            None => key.to_string(),
        },
    }
}

/// `_parse_claude_windows`: every `five_hour` / `seven_day*` object with a
/// utilization and a parseable reset, so a maxed model weekly binds.
pub fn parse_claude_windows(payload: &Value) -> Vec<Window> {
    let Some(obj) = payload.as_object() else {
        return Vec::new();
    };
    obj.iter()
        .filter(|(key, _)| *key == "five_hour" || key.starts_with("seven_day"))
        .filter_map(|(key, w)| {
            let w = w.as_object()?;
            let util = w.get("utilization").filter(|v| !v.is_null())?;
            let epoch = iso_to_epoch(w.get("resets_at")?)?;
            Some(window(claude_label(key), py_float(util)?, Some(epoch)))
        })
        .collect()
}

fn find_rate_limits(v: &Value) -> Option<&serde_json::Map<String, Value>> {
    match v {
        Value::Object(m) => match m.get("rate_limits") {
            Some(Value::Object(rl)) => Some(rl),
            _ => m.values().find_map(find_rate_limits),
        },
        Value::Array(a) => a.iter().find_map(find_rate_limits),
        _ => None,
    }
}

/// A JSON integer, never a bool or a float.
fn json_int(v: Option<&Value>) -> Option<i64> {
    v.filter(|v| v.is_i64() || v.is_u64())
        .and_then(Value::as_i64)
}

/// `_parse_codex_rate_limits`: `primary` then `secondary`, labelled from
/// `window_minutes` because the position does not say the span.
pub fn parse_codex_windows(payload: &Value) -> Vec<Window> {
    let Some(rl) = find_rate_limits(payload) else {
        return Vec::new();
    };
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| {
            let sub = rl.get(*key)?.as_object()?;
            let pct = sub.get("used_percent").filter(|v| !v.is_null())?;
            let resets = sub.get("resets_at").filter(|v| !v.is_null())?;
            let minutes = json_int(sub.get("window_minutes")).filter(|m| *m > 0);
            Some(window(
                label_for_minutes(minutes),
                py_float(pct)?,
                Some(py_float(resets)?),
            ))
        })
        .collect()
}

fn zai_limits(payload: &Value) -> Option<&Vec<Value>> {
    payload.get("data")?.get("limits")?.as_array()
}

fn zai_row_type(raw: &Value) -> Option<&str> {
    raw.get("type")
        .and_then(Value::as_str)
        .filter(|t| ZAI_LIMIT_TYPES.contains(t))
}

/// `_zai_confidence`: `exact` only when every recognized row reports counts.
pub fn zai_confidence(payload: &Value) -> &'static str {
    let Some(limits) = zai_limits(payload).filter(|l| !l.is_empty()) else {
        return "unknown";
    };
    let coarse = limits
        .iter()
        .filter(|raw| zai_row_type(raw).is_some())
        .any(|raw| json_int(raw.get("usage")).is_none());
    if coarse {
        "percent_only"
    } else {
        "exact"
    }
}

/// `_parse_zai_windows`: the windows and whether a recognized row was
/// rejected (`partial`, which floors headroom at LOW). A row with no reset
/// is kept; a reset farther out than its span allows is not published.
pub fn parse_zai_windows(payload: &Value, now: f64) -> (Vec<Window>, bool) {
    let ok = payload.get("success") == Some(&Value::Bool(true))
        && payload
            .get("code")
            .filter(|c| c.is_number())
            .and_then(Value::as_f64)
            == Some(200.0);
    let Some(limits) = zai_limits(payload).filter(|_| ok) else {
        return (Vec::new(), false);
    };
    let mut windows = Vec::new();
    let mut partial = false;
    for raw in limits {
        if zai_row_type(raw).is_none() {
            continue;
        }
        let span_minutes = json_int(raw.get("unit")).and_then(zai_unit_minutes);
        let number = json_int(raw.get("number")).filter(|n| *n > 0);
        let percentage = json_int(raw.get("percentage"));
        let (Some(unit_minutes), Some(number), Some(percentage)) =
            (span_minutes, number, percentage)
        else {
            partial = true;
            continue;
        };
        let field = |name: &str| {
            let present = raw.get(name).filter(|v| !v.is_null());
            (present.is_some(), json_int(present))
        };
        let (usage_set, usage) = field("usage");
        let (current_set, current) = field("currentValue");
        let (remaining_set, remaining) = field("remaining");
        if (usage_set && usage.is_none())
            || (current_set && current.is_none())
            || (remaining_set && remaining.is_none())
        {
            partial = true;
            continue;
        }
        let window_minutes = number * unit_minutes;
        let mut used_pct = percentage as f64;
        if let Some(usage) = usage.filter(|u| *u > 0) {
            if let Some(remaining) = remaining {
                let used = (usage - remaining).max(current.unwrap_or(usage - remaining));
                used_pct = 100.0 * used as f64 / usage as f64;
            } else if let Some(current) = current {
                used_pct = 100.0 * current as f64 / usage as f64;
            }
        }
        let resets_at = json_int(raw.get("nextResetTime"))
            .filter(|ms| *ms > 0)
            .map(|ms| ms as f64 / 1000.0)
            .filter(|at| at - now <= window_minutes as f64 * 60.0 * ZAI_RESET_TOLERANCE);
        windows.push(window(
            label_for_minutes(Some(window_minutes)),
            used_pct,
            resets_at,
        ));
    }
    (windows, partial)
}

fn snapshot(windows: Vec<Window>, now: f64, source: &str, confidence: &str) -> Snapshot {
    Snapshot {
        windows,
        probed_at: now,
        source: source.to_string(),
        partial: false,
        confidence: confidence.to_string(),
    }
}

type Probed = (Option<Snapshot>, Option<&'static str>);

fn shares_the_slot(record: &Record) -> bool {
    record.auth == "managed" && binding::credential_root(record).is_none()
}

fn is_active_slot_occupant(ctx: &Ctx<'_>, record: &Record) -> bool {
    let Some(root) = ctx.store.as_deref() else {
        return false;
    };
    matches!(
        crate::accounts::slot::active_slot_id(&record.harness, Some(root), ctx.cwd),
        Ok(Some(id)) if id == record.id
    ) && !store::slot_tainted(root, &record.harness)
}

/// `_attributed_credential_dir`: `(probeable, dir)`. Its own dir qualifies,
/// and so does the shared slot when this record provably occupies it.
fn attributed_dir(ctx: &Ctx<'_>, record: &Record) -> (bool, Option<PathBuf>) {
    if let Some(own) = binding::credential_root(record) {
        return (true, Some(own));
    }
    (
        record.auth == "managed" && is_active_slot_occupant(ctx, record),
        None,
    )
}

fn backoff_path(root: &Path, cli: &str) -> PathBuf {
    root.join(format!(".active-{cli}.reconcile-attempt"))
}

/// `_reconcile_slot_once`: one repair attempt, noted before it runs so a
/// crash still backs off.
fn reconcile_once(ctx: &Ctx<'_>, record: &Record, now: f64) -> bool {
    let Some(root) = ctx.store.as_deref().filter(|_| shares_the_slot(record)) else {
        return false;
    };
    let path = backoff_path(root, &record.harness);
    let backing_off = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .is_some_and(|at| now - at < RECONCILE_BACKOFF_S);
    if backing_off {
        return false;
    }
    let _ = crate::claude_vault::atomic_write(&path, &now.to_string());
    reconcile::reconcile_slot(
        &record.harness,
        ctx.records,
        root,
        ctx.slot,
        ctx.host,
        Duration::from_secs(10),
        now,
    )
    .ok()
}

/// `_reconcile_tainted_slot`: a taint may be false; a fresh probe asks once.
fn reconcile_tainted(ctx: &Ctx<'_>, record: &Record, now: f64) -> bool {
    let tainted = ctx
        .store
        .as_deref()
        .is_some_and(|root| store::slot_tainted(root, &record.harness));
    shares_the_slot(record) && tainted && reconcile_once(ctx, record, now)
}

/// `_bearer_verdict`: may this bearer's usage be filed under `record`?
fn bearer_verdict(ctx: &Ctx<'_>, record: &Record, bearer: &str, now: f64) -> &'static str {
    if !shares_the_slot(record) {
        return "unsupported";
    }
    let Some(root) = ctx.store.as_deref() else {
        return "unprovable";
    };
    if !attributed_dir(ctx, record).0 {
        let want = binding::record_principal(&record.id, root).map(|p| binding::identity_key(&p));
        let got = ctx
            .slot
            .profile(bearer)
            .ok()
            .map(|p| binding::identity_key(&p));
        return match (want, got) {
            (Some(want), Some(got)) if want == got => "match",
            (Some(_), Some(_)) => "mismatch",
            _ => "unprovable",
        };
    }
    let got = binding::resolve(
        &BindingRequest {
            record: Some(record),
            harness: &record.harness,
            bearer: Some(bearer),
            root,
            records: ctx.records,
            now,
            ttl: binding::PRINCIPAL_TTL_S,
        },
        ctx.slot,
    );
    match got.status {
        MATCHED => "match",
        MISMATCH => "mismatch",
        _ => "unprovable",
    }
}

/// `_stored_bearers`: an off-slot managed claude record's stored login.
fn stored_bearers(ctx: &Ctx<'_>, record: &Record) -> Vec<String> {
    if record.harness != "claude" || !shares_the_slot(record) {
        return Vec::new();
    }
    ctx.store
        .as_deref()
        .and_then(|root| store::read_blob(root, &record.id))
        .and_then(|blob| binding::bearer_of(&blob))
        .into_iter()
        .collect()
}

/// `_claude_bearer_candidates`: every bearer the attributed root serves, in
/// preference order, read fresh.
fn claude_candidates(ctx: &Ctx<'_>, record: &Record) -> Vec<String> {
    let (probeable, dir) = attributed_dir(ctx, record);
    if !probeable {
        return stored_bearers(ctx, record);
    }
    let mut tokens: Vec<String> = Vec::new();
    for blob in binding::slot_blobs(dir.as_deref(), ctx.slot).unwrap_or_default() {
        if let Some(token) = binding::bearer_of(&blob) {
            if !tokens.contains(&token) {
                tokens.push(token);
            }
        }
    }
    tokens
}

fn get(ctx: &Ctx<'_>, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), ()> {
    // A line break in a value would start a new curl config directive.
    if headers.iter().any(|(_, v)| v.contains(['\n', '\r'])) {
        return Err(());
    }
    let esc = crate::claude_vault::curl_escape;
    let mut config = format!("url = \"{}\"\n", esc(url));
    for (name, value) in headers {
        config.push_str(&format!("header = \"{}: {}\"\n", name, esc(value)));
    }
    (ctx.http)(&config)
}

/// `_probe_claude`: try each attributed bearer until one answers 200.
fn probe_claude(ctx: &Ctx<'_>, record: &Record, now: f64) -> Probed {
    let mut unattributable = false;
    let mut rejected = false;
    for bearer in claude_candidates(ctx, record) {
        // Prove before the request, so another account's usage is never fetched.
        if !matches!(
            bearer_verdict(ctx, record, &bearer, now),
            "match" | "unsupported"
        ) {
            unattributable = true;
            continue;
        }
        let auth = format!("Bearer {bearer}");
        let headers = [
            ("Authorization", auth.as_str()),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("User-Agent", CLAUDE_USER_AGENT),
        ];
        let Ok((status, body)) = get(ctx, &format!("{ANTHROPIC_API}/api/oauth/usage"), &headers)
        else {
            return (None, Some("probe-failed"));
        };
        if status == 401 || status == 403 {
            rejected = true;
            continue;
        }
        if !(200..300).contains(&status) {
            return (None, Some("probe-failed"));
        }
        let Ok(payload) = serde_json::from_str::<Value>(&body) else {
            return (None, Some("probe-failed"));
        };
        if !claude_candidates(ctx, record).contains(&bearer) {
            return (None, Some("identity_changed"));
        }
        let windows = parse_claude_windows(&payload);
        return (
            Some(snapshot(windows, now, "oauth-endpoint", "percent_only")),
            None,
        );
    }
    if unattributable {
        if attributed_dir(ctx, record).0 {
            reconcile_once(ctx, record, now);
        }
        return (None, Some("unattributed"));
    }
    (
        None,
        Some(if rejected {
            "credential-rejected"
        } else {
            "probe-failed"
        }),
    )
}

/// `_latest_codex_session`: the newest session JSONL under the record's home.
fn latest_codex_session(ctx: &Ctx<'_>, record: &Record) -> Option<PathBuf> {
    let base = record
        .credentials_source
        .clone()
        .unwrap_or_else(|| ctx.codex_home.clone());
    let sessions = base.join("sessions");
    let search = if sessions.is_dir() { sessions } else { base };
    if !search.is_dir() {
        return None;
    }
    let mut stack = vec![search];
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            // Like `rglob`, never descend a symlinked dir: a link cycle would never end.
            if entry.file_type().ok()?.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") && path.is_file() {
                let mtime = entry.metadata().ok()?.modified().ok()?;
                if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                    best = Some((mtime, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// `_probe_codex`: the newest event in the newest session that carries
/// `rate_limits`.
fn probe_codex(ctx: &Ctx<'_>, record: &Record, now: f64) -> Probed {
    let Some(text) =
        latest_codex_session(ctx, record).and_then(|p| std::fs::read_to_string(p).ok())
    else {
        return (None, Some("probe-failed"));
    };
    for line in text.lines().rev().map(str::trim) {
        if line.is_empty() || !line.contains("rate_limits") {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let windows = parse_codex_windows(&payload);
        if !windows.is_empty() {
            return (
                Some(snapshot(windows, now, "session-events", "exact")),
                None,
            );
        }
    }
    (None, Some("probe-failed"))
}

/// `urlparse(base_url).hostname` for an `https` URL, lowercased.
fn https_host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// `_probe_zai`: the configured z.ai route's quota endpoint.
fn probe_zai(ctx: &Ctx<'_>, record: &Record, now: f64) -> Probed {
    let failed = (None, Some("probe-failed"));
    let Some(env) = (ctx.route_env)(&record.id) else {
        return failed;
    };
    let nonempty = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    let bearer = nonempty("ANTHROPIC_AUTH_TOKEN").or_else(|| nonempty("ANTHROPIC_API_KEY"));
    let host = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(|u| https_host(u))
        .filter(|h| ZAI_HOSTS.contains(&h.as_str()));
    let (Some(bearer), Some(host)) = (bearer, host) else {
        return failed;
    };
    let auth = format!("Bearer {bearer}");
    let url = format!("https://{host}/api/monitor/usage/quota/limit");
    let headers = [
        ("Authorization", auth.as_str()),
        ("User-Agent", "fno-provider-usage/1"),
    ];
    let Ok((200..=299, body)) = get(ctx, &url, &headers) else {
        return failed;
    };
    let Ok(payload) = serde_json::from_str::<Value>(&body) else {
        return failed;
    };
    let (windows, partial) = parse_zai_windows(&payload, now);
    if windows.is_empty() {
        return failed;
    }
    let mut snap = snapshot(windows, now, "quota-endpoint", zai_confidence(&payload));
    snap.partial = partial;
    (Some(snap), None)
}

/// `probe_usage_detail`: the snapshot, or exactly one reason it is unknown.
/// Capability is classified before attribution: a record with no probe or no
/// bearer has no binding to repair.
pub fn probe_detail(ctx: &Ctx<'_>, record: &Record, now: f64) -> Probed {
    let route_provider = record
        .route
        .as_deref()
        .and_then(crate::spawn_route::parse_target)
        .map(|(provider, _)| provider);
    let key = route_provider.as_deref().unwrap_or(&record.harness);
    let probe: fn(&Ctx<'_>, &Record, f64) -> Probed = match key {
        "claude" => probe_claude,
        "codex" => probe_codex,
        "zai" => probe_zai,
        _ => return (None, Some("harness-unsupported")),
    };
    if route_provider.is_none() {
        if record.auth == "api_key" {
            return (None, Some("auth-unsupported"));
        }
        if !attributed_dir(ctx, record).0 {
            // A taint may be false; ask once, then re-read attribution.
            reconcile_tainted(ctx, record, now);
            if !attributed_dir(ctx, record).0 && stored_bearers(ctx, record).is_empty() {
                return (None, Some("unattributed"));
            }
        }
    }
    match probe(ctx, record, now) {
        (Some(snap), _) => (Some(snap), None),
        (None, reason) => (None, Some(reason.unwrap_or("probe-failed"))),
    }
}

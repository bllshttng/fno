//! `fno config accounts usage --logins`: the usage of every login already on
//! the machine, managed by fno or not. It reads the plain claude Keychain
//! item (the System default row), each claude config dir's scoped item, the
//! codex `auth.json`, and provider API keys from the env and `~/.fno/.env`.
//!
//! The reader is read-only. It sends the live access token as it is stored.
//! It never refreshes a token, never copies one, and never writes a slot or a
//! cache. An expired token is reported as `token-expired`, never repaired.

use crate::accounts::binding::{bearer_of, keychain_service, SlotProbe};
use crate::accounts::records::Record;
use crate::accounts::usage_probe::{
    label_for_minutes, parse_claude_windows, parse_zai_windows, ANTHROPIC_API,
};
use base64::Engine as _;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const UNSCOPED_SERVICE: &str = "Claude Code-credentials";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const USER_AGENT: &str = "fno-provider-usage/1";

/// Env keys read as API-key logins: name, provider, and the z.ai quota host
/// when the provider has a usage endpoint.
const API_KEYS: [(&str, &str, Option<&str>); 8] = [
    ("ZAI_API_KEY", "zai", Some("api.z.ai")),
    ("Z_AI_API_KEY", "zai", Some("api.z.ai")),
    ("BIGMODEL_API_KEY", "zai", Some("open.bigmodel.cn")),
    ("KIMI_API_KEY", "kimi", None),
    ("MOONSHOT_API_KEY", "kimi", None),
    ("ANTHROPIC_API_KEY", "anthropic", None),
    ("OPENAI_API_KEY", "openai", None),
    ("DEEPSEEK_API_KEY", "deepseek", None),
];

/// The reads the reader makes outside the process.
pub struct Ctx<'a> {
    /// The Keychain item and the profile endpoint (for the email).
    pub slot: &'a dyn SlotProbe,
    /// One HTTP GET from a curl `-K` config: the status and the raw body.
    pub http: &'a dyn Fn(&str) -> Result<(u16, String), ()>,
    /// An env value: the process env first, then `~/.fno/.env`.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Claude config dirs, each with its scoped Keychain item and its
    /// `.credentials.json`, with the account id that owns the dir if any.
    pub claude_dirs: Vec<(PathBuf, Option<String>)>,
    pub codex_home: PathBuf,
}

/// One login and what its provider said about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Login {
    pub provider: String,
    /// `subscription` or `api_key`.
    pub kind: &'static str,
    /// The row name: `System default`, the owning account id, or the dir.
    pub label: String,
    /// Where the credential lives, never the credential.
    pub sources: Vec<String>,
    pub plan: Option<String>,
    /// A masked email, or the last 4 characters of an API key.
    pub identity: Option<String>,
    pub windows: Vec<LoginWindow>,
    /// The reason no usage is shown; `None` when the read worked.
    pub reason: Option<&'static str>,
    pub read_at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoginWindow {
    pub label: String,
    pub used_pct: f64,
    pub resets_at: Option<f64>,
    pub span_s: Option<f64>,
}

impl LoginWindow {
    /// Used percent minus elapsed percent. Above zero burns faster than time.
    pub fn pace(&self, now: f64) -> Option<f64> {
        let (span, resets) = (self.span_s?, self.resets_at?);
        let elapsed = (100.0 * (span - (resets - now)) / span).clamp(0.0, 100.0);
        Some(((self.used_pct - elapsed) * 10.0).round() / 10.0)
    }

    fn to_json(&self, now: f64) -> Value {
        json!({
            "label": self.label,
            "used_pct": self.used_pct,
            "resets_at": self.resets_at,
            "span_s": self.span_s,
            "pace": self.pace(now),
        })
    }
}

impl Login {
    pub fn to_json(&self, now: f64) -> Value {
        json!({
            "provider": self.provider,
            "kind": self.kind,
            "label": self.label,
            "sources": self.sources,
            "plan": self.plan,
            "identity": self.identity,
            "state": if self.reason.is_none() { "ok" } else { "unknown" },
            "reason": self.reason,
            "windows": self.windows.iter().map(|w| w.to_json(now)).collect::<Vec<_>>(),
            "read_at": self.read_at,
            "age_s": (now - self.read_at).max(0.0).round(),
        })
    }
}

/// `j***@example.com`: the first character and the domain.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((user, domain)) if !user.is_empty() => {
            let first: String = user.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        _ => "***".to_string(),
    }
}

fn mask_key(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
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

fn json_get(ctx: &Ctx<'_>, url: &str, headers: &[(&str, &str)]) -> Result<Value, &'static str> {
    match get(ctx, url, headers) {
        Ok((401 | 403, _)) => Err("credential-rejected"),
        Ok((200..=299, body)) => serde_json::from_str(&body).map_err(|_| "probe-failed"),
        _ => Err("probe-failed"),
    }
}

fn login(provider: &str, kind: &'static str, label: String, source: String, now: f64) -> Login {
    Login {
        provider: provider.to_string(),
        kind,
        label,
        sources: vec![source],
        plan: None,
        identity: None,
        windows: Vec::new(),
        reason: None,
        read_at: now,
    }
}

/// `max` with tier `default_claude_max_20x` reads `max 20x`.
fn claude_plan(oauth: &Value) -> Option<String> {
    let kind = oauth.get("subscriptionType")?.as_str()?;
    let multiplier = oauth
        .get("rateLimitTier")
        .and_then(Value::as_str)
        .and_then(|t| t.rsplit('_').next())
        .filter(|m| m.ends_with('x') && m[..m.len() - 1].chars().all(|c| c.is_ascii_digit()));
    Some(match multiplier {
        Some(m) => format!("{kind} {m}"),
        None => kind.to_string(),
    })
}

fn read_claude(ctx: &Ctx<'_>, blob: &str, row: &mut Login, now: f64) {
    let oauth = serde_json::from_str::<Value>(blob)
        .ok()
        .and_then(|v| v.get("claudeAiOauth").cloned())
        .unwrap_or(Value::Null);
    row.plan = claude_plan(&oauth);
    let Some(bearer) = bearer_of(blob) else {
        row.reason = Some("no-access-token");
        return;
    };
    let expires_ms = oauth.get("expiresAt").and_then(Value::as_f64);
    if expires_ms.is_some_and(|ms| ms / 1000.0 <= now) {
        row.reason = Some("token-expired");
        return;
    }
    let auth = format!("Bearer {bearer}");
    let headers = [
        ("Authorization", auth.as_str()),
        ("anthropic-beta", "oauth-2025-04-20"),
        ("User-Agent", USER_AGENT),
    ];
    match json_get(ctx, &format!("{ANTHROPIC_API}/api/oauth/usage"), &headers) {
        Ok(payload) => {
            row.windows = parse_claude_windows(&payload)
                .into_iter()
                .map(|w| LoginWindow {
                    span_s: Some(if w.label == "5h" { 18_000.0 } else { 604_800.0 }),
                    label: w.label,
                    used_pct: w.used_pct,
                    resets_at: w.resets_at,
                })
                .collect();
        }
        Err(reason) => row.reason = Some(reason),
    }
    if row.reason != Some("credential-rejected") {
        row.identity = ctx
            .slot
            .profile(&bearer)
            .ok()
            .and_then(|p| p.email)
            .map(|e| mask_email(&e));
    }
}

/// Every claude login: the plain Keychain item first, then each dir's
/// scoped item and file. A token served by two items is one row.
fn claude_logins(ctx: &Ctx<'_>, now: f64) -> Vec<Login> {
    let mut found: Vec<(String, String, String)> = Vec::new();
    let mut push = |label: String, source: String, blob: Option<String>| {
        let Some(blob) = blob.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) else {
            return;
        };
        found.push((label, source, blob));
    };
    let home = |p: &Path| {
        let home = std::env::var("HOME").unwrap_or_default();
        match p.strip_prefix(&home) {
            Ok(rest) if !home.is_empty() => format!("~/{}", rest.display()),
            _ => p.display().to_string(),
        }
    };
    push(
        "System default".to_string(),
        format!("keychain:{UNSCOPED_SERVICE}"),
        ctx.slot.keychain(UNSCOPED_SERVICE).ok().flatten(),
    );
    for (dir, owner) in &ctx.claude_dirs {
        let label = owner.clone().unwrap_or_else(|| home(dir));
        if let Some(service) = keychain_service(dir) {
            let blob = ctx.slot.keychain(&service).ok().flatten();
            push(label.clone(), format!("keychain:{service}"), blob);
        }
        let file = dir.join(".credentials.json");
        push(
            label,
            format!("file:{}", home(&file)),
            std::fs::read_to_string(&file).ok(),
        );
    }
    let mut rows: Vec<(Option<String>, Login)> = Vec::new();
    for (label, source, blob) in found {
        let token = bearer_of(&blob);
        if let Some(row) = rows
            .iter_mut()
            .find(|(t, _)| token.is_some() && *t == token)
        {
            row.1.sources.push(source);
            continue;
        }
        let mut row = login("anthropic", "subscription", label, source, now);
        read_claude(ctx, &blob, &mut row, now);
        rows.push((token, row));
    }
    rows.into_iter().map(|(_, row)| row).collect()
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn codex_windows(payload: &Value) -> Vec<LoginWindow> {
    let Some(rl) = payload.get("rate_limit") else {
        return Vec::new();
    };
    ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|key| {
            let w = rl.get(*key)?.as_object()?;
            let used = w.get("used_percent")?.as_f64()?;
            let span = w
                .get("limit_window_seconds")
                .and_then(Value::as_f64)
                .filter(|s| *s > 0.0);
            Some(LoginWindow {
                label: label_for_minutes(span.map(|s| (s / 60.0) as i64)),
                used_pct: used.clamp(0.0, 100.0),
                resets_at: w.get("reset_at").and_then(Value::as_f64),
                span_s: span,
            })
        })
        .collect()
}

fn codex_login(ctx: &Ctx<'_>, now: f64) -> Option<Login> {
    let path = ctx.codex_home.join("auth.json");
    let auth: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let tokens = auth.get("tokens")?;
    let access = tokens
        .get("access_token")?
        .as_str()
        .filter(|t| !t.is_empty())?;
    let mut row = login(
        "openai",
        "subscription",
        "System default".to_string(),
        format!("file:{}", path.display()),
        now,
    );
    let id = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(jwt_claims);
    let claim = |k: &str| {
        id.as_ref()?
            .get("https://api.openai.com/auth")?
            .get(k)?
            .as_str()
            .map(str::to_string)
    };
    row.plan = claim("chatgpt_plan_type");
    row.identity = id
        .as_ref()
        .and_then(|c| c.get("email")?.as_str().map(mask_email));
    let expired = jwt_claims(access)
        .and_then(|c| c.get("exp")?.as_f64())
        .is_some_and(|exp| exp <= now);
    if expired {
        row.reason = Some("token-expired");
        return Some(row);
    }
    let bearer = format!("Bearer {access}");
    let account = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let headers = [
        ("Authorization", bearer.as_str()),
        ("ChatGPT-Account-Id", account),
        ("User-Agent", USER_AGENT),
    ];
    match json_get(ctx, CODEX_USAGE_URL, &headers) {
        Ok(payload) => {
            if let Some(plan) = payload.get("plan_type").and_then(Value::as_str) {
                row.plan = Some(plan.to_string());
            }
            row.windows = codex_windows(&payload);
        }
        Err(reason) => row.reason = Some(reason),
    }
    Some(row)
}

fn zai_windows(
    ctx: &Ctx<'_>,
    host: &str,
    key: &str,
    now: f64,
) -> Result<Vec<LoginWindow>, &'static str> {
    let auth = format!("Bearer {key}");
    let url = format!("https://{host}/api/monitor/usage/quota/limit");
    let payload = json_get(
        ctx,
        &url,
        &[("Authorization", auth.as_str()), ("User-Agent", USER_AGENT)],
    )?;
    let (windows, _partial) = parse_zai_windows(&payload, now);
    if windows.is_empty() {
        return Err("probe-failed");
    }
    Ok(windows
        .into_iter()
        .map(|w| LoginWindow {
            span_s: label_span(&w.label),
            label: w.label,
            used_pct: w.used_pct,
            resets_at: w.resets_at,
        })
        .collect())
}

/// The inverse of `label_for_minutes`.
fn label_span(label: &str) -> Option<f64> {
    if label == "weekly" {
        return Some(604_800.0);
    }
    let (n, unit) = label.split_at(label.len().checked_sub(1)?);
    let n: f64 = n.parse().ok()?;
    Some(
        n * match unit {
            "d" => 86_400.0,
            "h" => 3_600.0,
            "m" => 60.0,
            _ => return None,
        },
    )
}

fn api_key_logins(ctx: &Ctx<'_>, now: f64) -> Vec<Login> {
    let mut seen: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for (name, provider, host) in API_KEYS {
        let Some(key) = (ctx.env)(name).filter(|k| !k.trim().is_empty()) else {
            continue;
        };
        if seen.contains(&key) {
            continue;
        }
        seen.push(key.clone());
        let mut row = login(
            provider,
            "api_key",
            name.to_string(),
            format!("env:{name}"),
            now,
        );
        row.identity = Some(mask_key(&key));
        match host {
            Some(host) => match zai_windows(ctx, host, &key, now) {
                Ok(windows) => row.windows = windows,
                Err(reason) => row.reason = Some(reason),
            },
            None => row.reason = Some("usage-unsupported"),
        }
        rows.push(row);
    }
    rows
}

/// Every login on the machine and its usage, read once.
pub fn read(ctx: &Ctx<'_>, now: f64) -> Vec<Login> {
    let mut rows = claude_logins(ctx, now);
    rows.extend(codex_login(ctx, now));
    rows.extend(api_key_logins(ctx, now));
    rows
}

/// The claude config dirs to read: `CLAUDE_CONFIG_DIR` or `~/.claude`, then
/// each configured claude account's own dir.
fn claude_dirs(records: &[Record]) -> Vec<(PathBuf, Option<String>)> {
    let default = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(crate::accounts::binding::home_slot);
    let mut dirs = vec![(default, None)];
    for record in records.iter().filter(|r| r.harness == "claude") {
        let Some(dir) = record.config_dir.clone() else {
            continue;
        };
        match dirs.iter_mut().find(|(d, _)| *d == dir) {
            Some(slot) => slot.1 = Some(record.id.clone()),
            None => dirs.push((dir, Some(record.id.clone()))),
        }
    }
    dirs
}

fn render_text(rows: &[Login], now: f64) -> String {
    if rows.is_empty() {
        return "no logins found on this machine".to_string();
    }
    let mut out = Vec::new();
    for row in rows {
        let plan = row
            .plan
            .as_deref()
            .map(|p| format!(" [{p}]"))
            .unwrap_or_default();
        let who = row
            .identity
            .as_deref()
            .map(|i| format!(" {i}"))
            .unwrap_or_default();
        out.push(format!(
            "{} / {}{plan}{who} ({})",
            row.provider, row.label, row.kind
        ));
        if let Some(reason) = row.reason {
            out.push(format!("  unknown: {reason}"));
        }
        for w in &row.windows {
            let reset = crate::accounts::usage::resets_in(w.resets_at, now);
            let pace = w
                .pace(now)
                .map(|p| format!(", pace {p:+}"))
                .unwrap_or_default();
            out.push(format!(
                "  {}: {:.0}% used, {reset}{pace}",
                w.label, w.used_pct
            ));
        }
    }
    out.join("\n")
}

/// `usage --logins [--json]`: one live read, printed. Always exits 0: the
/// read is advisory, and every unknown names its reason.
pub fn run(cwd: &Path, as_json: bool) -> i32 {
    let now = crate::accounts::state::wall_now();
    let records = crate::accounts::records::load(Some(cwd), cwd)
        .map(|a| a.records)
        .unwrap_or_default();
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| crate::provider_key::read_var_from_env_file("~/.fno/.env", name))
    };
    let http = |config: &str| crate::claude_vault::curl_raw(config).map_err(|_| ());
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
        slot: &crate::accounts::binding::SystemProbe,
        http: &http,
        env: &env,
        claude_dirs: claude_dirs(&records),
        codex_home,
    };
    let rows = read(&ctx, now);
    if as_json {
        let list: Vec<Value> = rows.iter().map(|r| r.to_json(now)).collect();
        println!("{}", json!({ "logins": list }));
    } else {
        println!("{}", render_text(&rows, now));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_vault::{ExternalFailure, Principal};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const NOW: f64 = 1_789_400_000.0;

    struct Fake(BTreeMap<String, String>);

    impl SlotProbe for Fake {
        fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
            Ok(self.0.get(service).cloned())
        }
        fn profile(&self, _bearer: &str) -> Result<Principal, ExternalFailure> {
            Ok(Principal {
                account_uuid: "a".into(),
                organization_uuid: "o".into(),
                email: Some("jn@makersof.xyz".into()),
            })
        }
    }

    fn claude_blob(token: &str, expires_ms: f64) -> String {
        json!({"claudeAiOauth": {"accessToken": token, "refreshToken": "r", "expiresAt": expires_ms,
            "subscriptionType": "max", "rateLimitTier": "default_claude_max_20x"}})
        .to_string()
    }

    fn jwt(claims: Value) -> String {
        let enc =
            |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
        format!("{}.{}.sig", enc(&json!({"alg": "none"})), enc(&claims))
    }

    fn run_read(
        slot: &Fake,
        codex_home: PathBuf,
        env: BTreeMap<&str, &str>,
        answer: &dyn Fn(&str) -> (u16, String),
    ) -> (Vec<Login>, Vec<String>) {
        let calls = RefCell::new(Vec::new());
        let http = |config: &str| {
            calls.borrow_mut().push(config.to_string());
            Ok(answer(config))
        };
        let env_fn = |name: &str| env.get(name).map(|v| v.to_string());
        let dir = PathBuf::from("/nonexistent-fno-test/cfg");
        let ctx = Ctx {
            slot,
            http: &http,
            env: &env_fn,
            claude_dirs: vec![(dir, None)],
            codex_home,
        };
        let rows = read(&ctx, NOW);
        (rows, calls.into_inner())
    }

    #[test]
    fn claude_rows_read_live_token_and_dedupe_items() {
        let scoped = keychain_service(Path::new("/nonexistent-fno-test/cfg")).unwrap();
        let live = claude_blob("tok-live", (NOW + 3600.0) * 1000.0);
        let mut items = BTreeMap::new();
        items.insert(UNSCOPED_SERVICE.to_string(), live.clone());
        items.insert(scoped.clone(), live);
        let usage = json!({"five_hour": {"utilization": 50.0, "resets_at": NOW + 9000.0},
            "seven_day": {"utilization": 10.0, "resets_at": NOW + 302_400.0}});
        let (rows, calls) = run_read(
            &Fake(items),
            PathBuf::from("/nonexistent"),
            BTreeMap::new(),
            &|_| (200, usage.to_string()),
        );
        assert_eq!(rows.len(), 1, "one token served by two items is one row");
        let row = &rows[0];
        assert_eq!(row.label, "System default");
        assert_eq!(row.sources.len(), 2);
        assert_eq!(row.plan.as_deref(), Some("max 20x"));
        assert_eq!(row.identity.as_deref(), Some("j***@makersof.xyz"));
        assert_eq!(row.reason, None);
        let five = row.windows.iter().find(|w| w.label == "5h").unwrap();
        assert_eq!(
            five.pace(NOW),
            Some(0.0),
            "half used at half elapsed is even pace"
        );
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("Bearer tok-live") && calls[0].contains("oauth/usage"));
    }

    #[test]
    fn expired_tokens_are_reported_never_refreshed() {
        let mut items = BTreeMap::new();
        items.insert(
            UNSCOPED_SERVICE.to_string(),
            claude_blob("tok-old", (NOW - 60.0) * 1000.0),
        );
        let codex = tempfile::tempdir().unwrap();
        let access = jwt(json!({"exp": NOW - 1.0}));
        std::fs::write(
            codex.path().join("auth.json"),
            json!({"tokens": {"access_token": access, "refresh_token": "r", "account_id": "acc",
                "id_token": jwt(json!({"email": "x@y.z", "https://api.openai.com/auth": {"chatgpt_plan_type": "pro"}}))}})
            .to_string(),
        )
        .unwrap();
        let (rows, calls) = run_read(
            &Fake(items),
            codex.path().to_path_buf(),
            BTreeMap::new(),
            &|_| (500, String::new()),
        );
        assert!(
            calls.is_empty(),
            "an expired token makes no request: {calls:?}"
        );
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.reason == Some("token-expired")));
        let codex_row = rows.iter().find(|r| r.provider == "openai").unwrap();
        assert_eq!(codex_row.plan.as_deref(), Some("pro"));
        assert_eq!(codex_row.identity.as_deref(), Some("x***@y.z"));
    }

    #[test]
    fn codex_and_api_keys_read_their_endpoints() {
        let codex = tempfile::tempdir().unwrap();
        std::fs::write(
            codex.path().join("auth.json"),
            json!({"tokens": {"access_token": jwt(json!({"exp": NOW + 600.0})), "account_id": "acc"}}).to_string(),
        )
        .unwrap();
        let codex_body = json!({"plan_type": "pro", "rate_limit": {"primary_window":
            {"used_percent": 9, "limit_window_seconds": 604_800, "reset_at": NOW + 117_774.0}, "secondary_window": null}});
        let zai_body = json!({"success": true, "code": 200, "data": {"limits": [
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 40, "nextResetTime": ((NOW + 3600.0) * 1000.0) as i64}]}});
        let env = BTreeMap::from([
            ("ZAI_API_KEY", "zk-1234"),
            ("Z_AI_API_KEY", "zk-1234"),
            ("KIMI_API_KEY", "km-9999"),
        ]);
        let answer = |config: &str| {
            if config.contains("wham/usage") {
                assert!(config.contains("ChatGPT-Account-Id: acc"));
                (200, codex_body.to_string())
            } else {
                assert!(config.contains("api.z.ai") && config.contains("Bearer zk-1234"));
                (200, zai_body.to_string())
            }
        };
        let (rows, calls) = run_read(
            &Fake(BTreeMap::new()),
            codex.path().to_path_buf(),
            env,
            &answer,
        );
        assert_eq!(
            calls.len(),
            2,
            "one codex read, one z.ai read for two names of one key"
        );
        let codex_row = rows.iter().find(|r| r.provider == "openai").unwrap();
        assert_eq!(
            (
                codex_row.windows[0].label.as_str(),
                codex_row.windows[0].used_pct
            ),
            ("weekly", 9.0)
        );
        let zai = rows.iter().find(|r| r.provider == "zai").unwrap();
        assert_eq!(
            (zai.windows[0].label.as_str(), zai.windows[0].span_s),
            ("5h", Some(18_000.0))
        );
        assert_eq!(zai.identity.as_deref(), Some("...1234"));
        let kimi = rows.iter().find(|r| r.provider == "kimi").unwrap();
        assert_eq!(kimi.reason, Some("usage-unsupported"));
    }
}

//! `fno config accounts list [--json]`: every configured account, the active
//! one marked `*`. The `.active` stamp says who PUT a credential in the slot,
//! not who it serves, so the human listing carries an identity column: the
//! binding verdict plus doctor's problems for the row. `-J` is the Connections
//! modal's feed and does no binding work.

use crate::accounts::binding::{
    self, AccountBinding, BindingRequest, SlotProbe, AMBIGUOUS, MATCHED, MISMATCH,
};
use crate::accounts::records::{self, Accounts, Record};
use crate::accounts::show::pwd;
use crate::accounts::{doctor, slot, state, store};
use crate::backlog::quota_route::load_quota_config;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// `runtime_state.headroom`'s defaults: the list column reads the verdict at
/// the stock TTL, not the configured probe TTL.
const HEADROOM_TTL_S: f64 = 300.0;

/// `_headroom_label`: the verdict, then its source and the evidence age, so
/// the column never reads as the credential snapshot's provenance.
fn headroom_label(state_path: &Path, id: &str, now: f64) -> String {
    let h = state::headroom(
        state_path,
        id,
        now,
        HEADROOM_TTL_S,
        state::DEFAULT_THRESHOLD_PCT,
    );
    let mut detail = h.source.to_string();
    if !detail.is_empty() {
        if let Some(at) = h.observed_at {
            let age = (now - at).max(0.0);
            let (n, unit) = match age {
                a if a < 60.0 => (a, "s"),
                a if a < 3600.0 => (a / 60.0, "m"),
                a if a < 86400.0 => (a / 3600.0, "h"),
                a => (a / 86400.0, "d"),
            };
            detail.push_str(&format!(" {}{unit}", n.floor() as i64));
        }
        detail = format!("({detail})");
    }
    format!("{}{detail}", h.verdict.as_str())
}

/// `_UsageAge`: the usage PROBE age, distinct from the credential snapshot age.
struct UsageAge {
    age: Option<f64>,
    ttl: f64,
    stale: bool,
}

fn usage_age(state_path: &Path, id: &str, ttl: f64, now: f64) -> UsageAge {
    // A stale snapshot still reports its age, so the read takes no TTL.
    let age = state::read_usage(state_path, id, f64::INFINITY, now).map(|s| now - s.probed_at);
    UsageAge {
        stale: age.is_some_and(|a| a > ttl),
        age,
        ttl,
    }
}

fn usage_col(u: &UsageAge) -> String {
    match u.age {
        None => "usage=never".to_string(),
        Some(age) if u.stale => format!(
            "usage={} (STALE, ttl={})",
            store::age_label_from_seconds(age),
            store::age_label_from_seconds(u.ttl)
        ),
        Some(age) => format!("usage={}", store::age_label_from_seconds(age)),
    }
}

/// A whole number of seconds reads as an int, the way the Python config holds it.
fn seconds_json(s: f64) -> Value {
    if s.fract() == 0.0 && s.abs() < 1e15 {
        json!(s as i64)
    } else {
        json!(s)
    }
}

/// `_identity_cell`: matched names the row's own record, mismatch names who
/// the credential really serves, and anything unproven reads `?<reason>`.
fn identity_cell(
    record: &Record,
    got: Option<&AccountBinding>,
    problems: &[String],
    off_slot: bool,
) -> String {
    let nonempty = |s: &Option<String>| s.clone().filter(|v| !v.is_empty());
    let cell = match got {
        _ if record.harness != "claude" || record.auth == "api_key" => "-".to_string(),
        None => "?no-observation".to_string(),
        Some(b) if b.status == MATCHED => nonempty(&b.matched_record)
            .or_else(|| nonempty(&b.requested_record))
            .unwrap_or_else(|| "?".to_string()),
        // A managed record not in the slot: no slot login is its own.
        Some(b) if b.status == MISMATCH && off_slot => "off-slot".to_string(),
        Some(b) if b.status == MISMATCH => format!(
            "!serves {}",
            nonempty(&b.matched_record)
                .or_else(|| nonempty(&b.observed_label))
                .or_else(|| nonempty(&b.observed_principal))
                .unwrap_or_else(|| "another-account".to_string())
        ),
        Some(b) if b.status == AMBIGUOUS => "?ambiguous".to_string(),
        Some(b) => format!(
            "?{}",
            nonempty(&b.reason).unwrap_or_else(|| "unknown".to_string())
        ),
    };
    cell + &problems
        .iter()
        .map(|p| format!(" !{p}"))
        .collect::<String>()
}

/// `_shared_identity_ids`: records whose observed principal is also observed
/// through a different credential root. Managed records share the slot root,
/// so a managed pair never trips this; two config dirs on one principal do.
fn shared_identity_ids(identities: &BTreeMap<String, AccountBinding>) -> BTreeSet<String> {
    let mut roots: BTreeMap<&str, BTreeSet<Option<&PathBuf>>> = BTreeMap::new();
    for got in identities.values() {
        if let Some(p) = got.observed_principal.as_deref() {
            roots
                .entry(p)
                .or_default()
                .insert(got.credential_root.as_ref());
        }
    }
    identities
        .iter()
        .filter(|(_, got)| {
            got.observed_principal
                .as_deref()
                .is_some_and(|p| roots[p].len() > 1)
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// The vendor a record bills when it is not its harness's own: a route's
/// first segment, a Bedrock switch in the env, or the vendor that owns the
/// env's base URL. None means the harness default, so a plain record's row
/// stays as it was.
fn record_provider(
    route: Option<&str>,
    env: Option<&BTreeMap<String, String>>,
    vendor: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let head = route
        .map(|r| r.replace(',', "/"))
        .and_then(|r| r.split('/').next().map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty());
    if head.is_some() {
        return head;
    }
    let env = env?;
    if ["CLAUDE_CODE_USE_BEDROCK", "AWS_BEARER_TOKEN_BEDROCK"]
        .iter()
        .any(|k| env.contains_key(*k))
    {
        return Some("bedrock".to_string());
    }
    ["ANTHROPIC_BASE_URL", "OPENAI_BASE_URL"]
        .iter()
        .find_map(|k| env.get(*k).filter(|u| !u.is_empty()))
        .and_then(|url| vendor(url))
}

/// A claude login's plan, read from its saved blob: `subscriptionType`, then
/// the multiplier `rateLimitTier` ends on (`max` + `default_claude_max_20x`
/// reads `Max 20x`).
fn claude_plan(blob: &Value) -> Option<String> {
    let oauth = blob.get("claudeAiOauth")?;
    let sub = oauth.get("subscriptionType")?.as_str()?.trim();
    let mut chars = sub.chars();
    let mut plan: String = chars.next()?.to_uppercase().chain(chars).collect();
    let mult = oauth
        .get("rateLimitTier")
        .and_then(Value::as_str)
        .and_then(|tier| tier.rsplit('_').next())
        .filter(|m| {
            m.len() > 1 && m.ends_with('x') && m[..m.len() - 1].bytes().all(|b| b.is_ascii_digit())
        });
    if let Some(mult) = mult {
        plan.push(' ');
        plan.push_str(mult);
    }
    Some(plan)
}

/// The plan a row names: the record's own `plan` key, else the tier its saved
/// claude login carries.
fn record_plan(record: &Record, root: &Path) -> Option<String> {
    if let Some(plan) = record.extra.get("plan").and_then(Value::as_str) {
        return Some(plan.trim().to_string()).filter(|p| !p.is_empty());
    }
    if record.auth != "managed" || record.harness != "claude" {
        return None;
    }
    let text = std::fs::read_to_string(store::blob_path(root, &record.id)).ok()?;
    claude_plan(&serde_json::from_str(&text).ok()?)
}

fn rows(
    accounts: &Accounts,
    cwd: &Path,
    state_path: &Path,
    root: &Path,
    ttl: f64,
    now: f64,
) -> Value {
    let rows: Vec<Value> = accounts
        .records
        .iter()
        .map(|record| {
            let u = usage_age(state_path, &record.id, ttl, now);
            let mut row = json!({
                "id": record.id,
                "name": record.name,
                "harness": record.harness,
                "auth": record.auth,
                "priority": record.priority,
                "active": records::is_effective_active(record, accounts, cwd),
                "headroom": headroom_label(state_path, &record.id, now),
                "cred-snapshot": (record.auth == "managed")
                    .then(|| store::snapshot_age_label(root, &record.id, now)),
                "usage_age_s": u.age,
                "usage_ttl_seconds": seconds_json(u.ttl),
                "usage_stale": u.stale,
            });
            if record.config_dir.is_some() {
                row["spawn_only"] = json!(true);
            }
            let vendor = |url: &str| crate::accounts::credentials::vendor_for_endpoint(cwd, url);
            if let Some(provider) =
                record_provider(record.route.as_deref(), record.env.as_ref(), &vendor)
            {
                row["provider"] = json!(provider);
            }
            // The email the saved login proved at sign-in, never a config label.
            if let Some(email) = (record.auth == "managed")
                .then(|| binding::record_principal(&record.id, root))
                .flatten()
                .and_then(|p| p.email)
            {
                row["email"] = json!(email);
            }
            if let Some(plan) = record_plan(record, root) {
                row["plan"] = json!(plan);
            }
            row
        })
        .collect();
    Value::Array(rows)
}

/// The listing for `cwd`: the `-J` array or the human report.
pub fn report(
    cwd: &Path,
    as_json: bool,
    probe: &dyn SlotProbe,
    now: f64,
) -> Result<String, String> {
    let accounts = records::load(Some(cwd), cwd).map_err(|e| e.to_string())?;
    let quota = load_quota_config(cwd.to_str());
    let state_path = crate::provider_cap::runtime_state_path(cwd);
    let root = slot::store_root(cwd).unwrap_or_default();
    if as_json {
        let rows = rows(
            &accounts,
            cwd,
            &state_path,
            &root,
            quota.probe_ttl_seconds,
            now,
        );
        return Ok(crate::backlog::render::py_json_compact(&rows));
    }
    if accounts.records.is_empty() {
        return Ok("No accounts configured. Run `fno config accounts add` to add one.".to_string());
    }
    let findings = doctor::findings(&accounts.records, &root, probe, now);
    let identities: BTreeMap<String, AccountBinding> = accounts
        .records
        .iter()
        .filter(|r| r.harness == "claude")
        .map(|r| {
            let req = BindingRequest {
                record: Some(r),
                harness: &r.harness,
                bearer: None,
                root: &root,
                records: &accounts.records,
                now,
                ttl: binding::PRINCIPAL_TTL_S,
            };
            (r.id.clone(), binding::resolve(&req, probe))
        })
        .collect();
    let shared = shared_identity_ids(&identities);
    let mut lines = Vec::new();
    for record in &accounts.records {
        let mut problems: Vec<String> = findings
            .iter()
            .filter(|f| f.record == record.id)
            .map(|f| f.problem.clone())
            .collect();
        // shared-identity rides the problems list: one wire field, both surfaces.
        if shared.contains(&record.id) {
            problems.push("shared-identity".to_string());
        }
        let active = records::is_effective_active(record, &accounts, cwd);
        let mut line = format!(
            "{} {}  [{}] {}  priority={}  headroom={}",
            if active { "*" } else { " " },
            record.id,
            record.harness,
            record.auth,
            record.priority,
            headroom_label(&state_path, &record.id, now)
        );
        if record.auth == "managed" {
            line.push_str(&format!(
                "  cred-snapshot={}",
                store::snapshot_age_label(&root, &record.id, now)
            ));
        }
        line.push_str("  ");
        line.push_str(&usage_col(&usage_age(
            &state_path,
            &record.id,
            quota.probe_ttl_seconds,
            now,
        )));
        let off_slot = record.auth == "managed" && !active;
        let cell = identity_cell(record, identities.get(&record.id), &problems, off_slot);
        line.push_str(&format!("  identity={cell}"));
        lines.push(line);
    }
    // Two flags, two sentences: `observe` first, the reversible half.
    if !(quota.observe || quota.defer_dispatch) {
        lines.push(
            "\nquota observation is OFF: accounts.quota.observe = false and \
             accounts.quota.defer_dispatch = false, so dispatch never probes \
             (the daemon still does). Set accounts.quota.observe = true in \
             ~/.fno/config.toml under [accounts.quota] for a meter that \
             changes no dispatch decision; add defer_dispatch = true to also \
             hold a dispatch on a walled account."
                .to_string(),
        );
    } else if !quota.defer_dispatch {
        lines.push(
            "\nobservation is ON and rotation is DISARMED: \
             accounts.quota.defer_dispatch = false, so headroom is measured \
             and recorded but never holds a dispatch."
                .to_string(),
        );
    }
    Ok(lines.join("\n"))
}

const HELP: &str = "Usage: fno config accounts list [OPTIONS]

  List all configured accounts, marking the active one with *.

  The human listing always carries an identity column: the .active stamp says
  who PUT a credential in the slot, not who it serves, so the binding verdict
  and doctor's per-record problems ride the row.

Options:
  -J, --json  Emit a JSON array of record rows (Connections UI).";

/// `fno-agents spawn-overlay accounts list [--json]`.
pub fn run(args: &[String]) -> i32 {
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "-J" | "--json" => as_json = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return 0;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: no such option: {flag}");
                return 2;
            }
            word => {
                eprintln!("error: unexpected extra argument ({word})");
                return 2;
            }
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    match report(&pwd(), as_json, &binding::SystemProbe, now) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_or_a_base_url_names_the_provider_and_a_plain_key_names_none() {
        let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let vendor = |url: &str| url.contains("z.ai").then(|| "zai".to_string());
        let got = |route, env| record_provider(route, env, &vendor);
        assert_eq!(got(Some("deepseek/v3"), None).as_deref(), Some("deepseek"));
        let zai = env(&[("ANTHROPIC_BASE_URL", "https://z.ai.example/anthropic")]);
        assert_eq!(got(None, Some(&zai)).as_deref(), Some("zai"));
        let bedrock = env(&[("CLAUDE_CODE_USE_BEDROCK", "1")]);
        assert_eq!(got(None, Some(&bedrock)).as_deref(), Some("bedrock"));
        let plain = env(&[("ANTHROPIC_API_KEY", "k")]);
        assert_eq!(got(None, Some(&plain)), None);
        assert_eq!(got(Some(" "), None), None);
    }

    #[test]
    fn a_saved_claude_login_names_its_plan_and_tier() {
        let blob = |sub: &str, tier: &str| json!({"claudeAiOauth": {"subscriptionType": sub, "rateLimitTier": tier}});
        let max = blob("max", "default_claude_max_20x");
        assert_eq!(claude_plan(&max).as_deref(), Some("Max 20x"));
        let pro = blob("pro", "default_claude_ai");
        assert_eq!(claude_plan(&pro).as_deref(), Some("Pro"));
        assert_eq!(
            claude_plan(&json!({"claudeAiOauth": {"accessToken": "t"}})),
            None
        );
    }
}

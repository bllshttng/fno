//! The supervisor half of the footnote endpoint: resolve the borrowed model
//! endpoint from the route env and `config.model_routing`, then hand it to the
//! `footnote` binary inside the launch spec.

use crate::footnote_transcript::EndpointSpec;
use std::path::Path;

/// The host part of a base URL, for the refusals below.
fn host(base_url: &str) -> &str {
    let rest = base_url.split("://").nth(1).unwrap_or(base_url);
    rest.split('/').next().unwrap_or("")
}

/// The `config.model_routing.providers.<provider>` record the endpoint reads,
/// merged with the built-in preset by the one owner (`provider_route`): the
/// zero-config zai lane fills every field the config leaves absent or
/// empty. `None` when neither names the provider.
pub fn provider_record(cwd: &Path, provider: &str) -> Option<toml::Table> {
    crate::provider_route::record_table(cwd, provider)
}

/// `-m <provider>/<model>`: the provider a footnote model id names, and the
/// model without it. A prefix equal to the route stamp is dropped, because the
/// route already is that provider. Under an env endpoint any other id goes
/// whole: a proxy's own ids carry slashes, and the env route must not move to
/// another vendor. A prefix that names no provider record is part of the id.
pub fn split_model<'a>(
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    model: &'a str,
) -> (Option<String>, &'a str) {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let Some((prefix, rest)) = model.split_once('/').filter(|(_, rest)| !rest.is_empty()) else {
        return (None, model);
    };
    if get(crate::codex_route::ROUTE_PROVIDER_ENV).as_deref() == Some(prefix) {
        (None, rest)
    } else if get("ANTHROPIC_BASE_URL").is_none() && provider_record(cwd, prefix).is_some() {
        (Some(prefix.to_string()), rest)
    } else {
        (None, model)
    }
}

/// The endpoint of one `config.model_routing.providers` record.
fn record_endpoint(cwd: &Path, provider: String) -> Result<EndpointSpec, String> {
    let Some(table) = provider_record(cwd, &provider) else {
        return Err(format!(
            "provider {provider:?} is not configured: add config.model_routing.providers.{provider}"
        ));
    };
    let field = |n: &str| {
        table
            .get(n)
            .and_then(toml::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let openai = field("protocol")
        .unwrap_or_else(|| "anthropic".into())
        .eq_ignore_ascii_case("openai");
    let base_url = field("base_url")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("provider {provider:?} has no base_url"))?;
    let key_env = field("api_key_env").unwrap_or_else(|| {
        if openai {
            "OPENAI_API_KEY"
        } else {
            "ANTHROPIC_API_KEY"
        }
        .into()
    });
    let key = crate::provider_key::resolve_key(&key_env, field("api_key_file").as_deref())
        .ok_or_else(|| format!("provider {provider:?} has no API key under {key_env:?}"))?;
    Ok(EndpointSpec {
        base_url,
        key,
        bearer: key_env != "ANTHROPIC_API_KEY",
        wire: if openai { "openai" } else { "anthropic" }.into(),
        provider_id: Some(provider),
        route: "config".into(),
    })
}

/// Resolve the endpoint. `provider` is the one the model id or the resumed
/// row named, and its own record comes first: a resumed session returns to
/// the provider it was spawned on, whatever endpoint the asking session
/// exports. Then the spawn front's route env, then the
/// `config.model_routing.providers.<FNO_ROUTE_PROVIDER>` record. Never a
/// Claude.ai login: an Anthropic host with no API key refuses.
pub fn resolve_endpoint(
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    provider: Option<&str>,
) -> Result<EndpointSpec, String> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let named = provider.filter(|p| !p.is_empty()).map(str::to_string);
    let stamp = get(crate::codex_route::ROUTE_PROVIDER_ENV);
    let recorded = match named.as_deref() {
        Some(p) if provider_record(cwd, p).is_some() => match record_endpoint(cwd, p.to_string()) {
            Ok(ep) => Some(ep),
            // An env endpoint stamped as this provider is the same vendor, so
            // a row spawned on it still resumes there. Any other env endpoint
            // is another vendor: refuse with the record's own reason.
            Err(_) if stamp.as_deref() == Some(p) && get("ANTHROPIC_BASE_URL").is_some() => None,
            Err(e) => return Err(e),
        },
        _ => None,
    };
    let ep = if let Some(ep) = recorded {
        ep
    } else if let Some(base_url) = get("ANTHROPIC_BASE_URL") {
        let (key, bearer) = match (get("ANTHROPIC_AUTH_TOKEN"), get("ANTHROPIC_API_KEY")) {
            (Some(t), _) => (t, true),
            (None, Some(k)) => (k, false),
            (None, None) => (String::new(), false),
        };
        EndpointSpec {
            base_url,
            key,
            bearer,
            wire: "anthropic".into(),
            provider_id: named.or(stamp),
            route: "env".into(),
        }
    } else if let Some(provider) = named.or(stamp) {
        record_endpoint(cwd, provider)?
    } else {
        return Err(
            "no model endpoint: pass -m <provider>/<model> (a config.model_routing.providers name, e.g. zai/glm-5.3-flash), or set ANTHROPIC_BASE_URL and a key"
                .into(),
        );
    };
    if host(&ep.base_url).ends_with("api.anthropic.com") && (ep.bearer || ep.key.is_empty()) {
        return Err("api.anthropic.com needs an API key (ANTHROPIC_API_KEY); a Claude.ai login runs only in the Claude Code lane (-H claude)".into());
    }
    if ep.key.is_empty() {
        return Err(format!("no API key for {}", host(&ep.base_url)));
    }
    Ok(ep)
}

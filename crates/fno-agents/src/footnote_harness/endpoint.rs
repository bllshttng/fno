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
/// with the zero-config zai lane filling every field the config leaves absent
/// or empty: the same built-in record Python's model_routing
/// `_DEFAULT_PROVIDERS` carries. `None` when neither names the provider.
pub fn provider_record(cwd: &Path, provider: &str) -> Option<toml::Table> {
    let mut record =
        crate::agents_config::config_table_merged(cwd, &["model_routing", "providers", provider]);
    if provider == "zai" {
        let table = record.get_or_insert_with(toml::Table::new);
        for (field, value) in [
            ("base_url", "https://api.z.ai/api/anthropic"),
            ("api_key_env", "ZAI_API_KEY"),
            ("api_key_file", "~/.fno/.env"),
        ] {
            let set = table
                .get(field)
                .and_then(toml::Value::as_str)
                .is_some_and(|v| !v.is_empty());
            if !set {
                table.insert(field.into(), toml::Value::String(value.into()));
            }
        }
    }
    record
}

/// `-m <provider>/<model>`: the provider a footnote model id names, and the
/// model without it. A prefix that names no provider record is part of the id.
pub fn split_model<'a>(cwd: &Path, model: &'a str) -> (Option<String>, &'a str) {
    match model.split_once('/') {
        Some((p, rest)) if !rest.is_empty() && provider_record(cwd, p).is_some() => {
            (Some(p.to_string()), rest)
        }
        _ => (None, model),
    }
}

/// Resolve the endpoint: the provider the model id named first, then the
/// spawn front's route env, then the
/// `config.model_routing.providers.<FNO_ROUTE_PROVIDER>` record. A named
/// provider outranks the env, so a spawn from a routed session never sends
/// its model to that session's endpoint. Never a Claude.ai login: an
/// Anthropic host with no API key refuses.
pub fn resolve_endpoint(
    cwd: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    provider: Option<&str>,
) -> Result<EndpointSpec, String> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let named = provider.filter(|p| !p.is_empty()).map(str::to_string);
    let route_provider = named
        .clone()
        .or_else(|| get(crate::codex_route::ROUTE_PROVIDER_ENV));
    let env_url = get("ANTHROPIC_BASE_URL").filter(|_| named.is_none());
    let ep = if let Some(base_url) = env_url {
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
            provider_id: route_provider,
            route: "env".into(),
        }
    } else if let Some(provider) = route_provider {
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
        EndpointSpec {
            base_url,
            key,
            bearer: key_env != "ANTHROPIC_API_KEY",
            wire: if openai { "openai" } else { "anthropic" }.into(),
            provider_id: Some(provider),
            route: "config".into(),
        }
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

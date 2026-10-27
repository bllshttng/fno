//! The `--route` and `--role` arms of a Rust-run spawn: build a routed claude
//! spawn's route env and `--settings` file from the one Rust owner of each
//! fact (`provider_route`, `route_gather`, `route_settings`, `spawn_role`).
//! The refusal lines copy `cmd_spawn`'s byte for byte. The pane spine
//! resolves its own route (`pane_spawn::claude`).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::claude_ask::py_repr;
use crate::provider_route::{claude_route, ClaudeRoute, RouteCtx};

/// The env key that binds a route env to the vendor that produced it.
pub const ROUTE_PROVIDER_ENV: &str = crate::codex_route::ROUTE_PROVIDER_ENV;

/// A resolved route, ready to hand to the claude thread lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub env: BTreeMap<String, String>,
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub settings: String,
}

/// How a spawn's `--route` proceeds on the Rust arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteSeam {
    /// No route on the spawn: the primary model.
    None,
    /// A refusal line; the spawn exits 2 before the gate.
    Refuse(String),
    Routed(Route),
}

/// Port of `model_routing._parse_target`: `provider/model` or legacy
/// `provider,model`. A comma wins as the separator; otherwise the first slash
/// splits, keeping a namespaced model id whole. Provider lowercases.
pub fn parse_target(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim();
    let (provider, model) = if raw.contains(',') {
        let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
        if parts.len() != 2 {
            return None;
        }
        (parts[0], parts[1])
    } else {
        let (p, m) = raw.split_once('/')?;
        (p.trim(), m.trim())
    };
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    if provider.chars().any(char::is_whitespace) || model.chars().any(char::is_whitespace) {
        return None;
    }
    Some((provider.to_lowercase(), model.to_string()))
}

/// The shared reason a target resolves to nothing: the pane arm and the
/// thread seam name the same gap in the same words (cli.py:1702 is the
/// Python door's twin).
pub(crate) const ROUTE_KEYLESS: &str = "provider unknown, non-anthropic, or keyless";

pub(crate) fn refused(route: &str, reason: &str) -> String {
    format!(
        "--route {} refused ({reason}); no worker launched, node stays dispatchable.",
        py_repr(route)
    )
}

/// The verdict for one spawn. `substrate` is the public name (`thread`,
/// `headless`, `cloud`); `env_get` reads the key env the route builder needs.
/// It must run before the account seam scrubs the credential vars, because
/// a vendor's key can live in one of them (`ANTHROPIC_AUTH_TOKEN` as an
/// `api_key_env`).
pub fn route_seam(
    route: Option<&str>,
    harness: &str,
    substrate: &str,
    cwd: &Path,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> RouteSeam {
    let Some(route) = route else {
        return RouteSeam::None;
    };
    if harness != "claude" {
        return RouteSeam::Refuse(format!(
            "--route {route} requires the claude harness; got harness {} substrate {}. \
             A non-claude harness resolves its provider from its own config: pass that \
             harness's own model id via --model instead (opencode spells it \
             provider/model, e.g. zai-coding-plan/glm-5.3-flash).",
            py_repr(harness),
            py_repr(substrate)
        ));
    }
    // The cloud lane launches remotely and carries no route env; the Python
    // lane dropped the route there without a word, which mis-bills.
    if substrate == "cloud" {
        return RouteSeam::Refuse(refused(route, "the cloud lane carries no route"));
    }
    let (provider, model, effort) = match parse_target(route) {
        Some((p, m)) => (p, m, None),
        None => {
            let row = crate::route_gather::run_mode(
                &json!({"mode": "lane", "match": "name", "name": route}),
                cwd,
            );
            if row.get("found").and_then(Value::as_bool) != Some(true) {
                return RouteSeam::Refuse(format!(
                    "--route takes 'provider,model' or a declared lane name; got {}",
                    py_repr(route)
                ));
            }
            let field = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            if field("vendor").is_empty() {
                return RouteSeam::Refuse(format!(
                    "--route {} names a lane that declares no route; no worker launched, \
                     node stays dispatchable.",
                    py_repr(route)
                ));
            }
            let effort = Some(field("effort")).filter(|e| !e.is_empty());
            (field("vendor"), field("model"), effort)
        }
    };
    // resolve_explicit_route: a blank provider or model resolves nothing.
    let pname = provider.trim().to_lowercase();
    if pname.is_empty() || model.trim().is_empty() {
        return RouteSeam::Refuse(refused(route, crate::spawn_route::ROUTE_KEYLESS));
    }
    let ctx = RouteCtx::Peer(pname.clone());
    let mut env = match claude_route(cwd, &pname, model.trim(), &ctx, env_get) {
        ClaudeRoute::Routed { env, .. } => env,
        ClaudeRoute::Skipped { notices } => {
            let reason = if notices.is_empty() {
                crate::spawn_route::ROUTE_KEYLESS.to_string()
            } else {
                notices.join("; ")
            };
            return RouteSeam::Refuse(refused(route, &reason));
        }
    };
    // bind_route_provider: the stamp rides the route env into the file.
    env.insert(ROUTE_PROVIDER_ENV.to_string(), provider.clone());
    settled(
        &|reason| refused(route, reason),
        env,
        provider,
        model,
        effort,
        cwd,
    )
}

/// Write the route's `--settings` file and hand the lane its route. A write
/// failure refuses in the caller's own line, `refuse(reason)`.
fn settled(
    refuse: &dyn Fn(&str) -> String,
    env: BTreeMap<String, String>,
    provider: String,
    model: String,
    effort: Option<String>,
    cwd: &Path,
) -> RouteSeam {
    let route_env: Map<String, Value> = env
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let settings =
        match crate::route_settings::write_composed(&route_env, &Map::new(), None, None, cwd) {
            Ok(Some(path)) => path,
            Ok(None) => return RouteSeam::Refuse(refuse("the route wrote no settings file")),
            Err(e) => return RouteSeam::Refuse(refuse(&e)),
        };
    RouteSeam::Routed(Route {
        env,
        provider,
        model,
        effort,
        settings,
    })
}

/// The `--role` verdict for a spawn with no `--route` (an explicit route wins
/// over a role). Only claude resolves a role route: another harness takes its
/// model from its own config, and its role routes nothing, as before. A role
/// that resolves no route answers `None`, the primary model. Its notices
/// print here, the lines the Python lane's notice sink wrote.
pub fn role_seam(
    role: Option<&str>,
    harness: &str,
    cwd: &Path,
    env_get: &dyn Fn(&str) -> Option<String>,
) -> RouteSeam {
    let Some(role) = role.filter(|_| harness == "claude") else {
        return RouteSeam::None;
    };
    let block = crate::spawn_role::RoutingBlock::read(cwd);
    let mut notices = Vec::new();
    let resolved = crate::spawn_role::resolve(role, &block, cwd, env_get, &mut notices);
    for note in &notices {
        eprintln!("{note}");
    }
    match resolved {
        Err(line) => RouteSeam::Refuse(line),
        Ok(None) => RouteSeam::None,
        Ok(Some(r)) => settled(
            &|reason| {
                format!(
                    "--role {} refused ({reason}); no worker launched, node stays dispatchable.",
                    py_repr(role)
                )
            },
            r.env,
            r.provider,
            r.model,
            None,
            cwd,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_ONLY: &str = "--route zai/glm-5.3 requires the claude harness; got harness 'codex' substrate 'thread'. A non-claude harness resolves its provider from its own config: pass that harness's own model id via --model instead (opencode spells it provider/model, e.g. zai-coding-plan/glm-5.3-flash).";
    const MALFORMED: &str =
        "--route takes 'provider,model' or a declared lane name; got 'no-such-lane'";

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn parse_target_matches_the_python_grammar() {
        assert_eq!(
            parse_target("ZAI/glm-5.3[1m]"),
            Some(("zai".into(), "glm-5.3[1m]".into()))
        );
        assert_eq!(
            parse_target("zai/z-ai/glm-5.3"),
            Some(("zai".into(), "z-ai/glm-5.3".into()))
        );
        assert_eq!(
            parse_target(" zai , glm-5.3 "),
            Some(("zai".into(), "glm-5.3".into()))
        );
        for bad in ["zai", "zai/", "/glm", "a,b,c", "zai/glm 5", "z ai,glm"] {
            assert_eq!(parse_target(bad), None, "{bad}");
        }
    }

    #[test]
    fn refusals_copy_the_python_lane() {
        let cwd = std::env::temp_dir();
        let seam = |route, harness, substrate| route_seam(route, harness, substrate, &cwd, &no_env);
        assert_eq!(seam(None, "claude", "thread"), RouteSeam::None);
        assert_eq!(
            seam(Some("zai/glm-5.3"), "codex", "thread"),
            RouteSeam::Refuse(CLAUDE_ONLY.into())
        );
        assert_eq!(
            seam(Some("zai/glm-5.3"), "claude", "cloud"),
            RouteSeam::Refuse(refused("zai/glm-5.3", "the cloud lane carries no route"))
        );
        assert_eq!(
            seam(Some("no-such-lane"), "claude", "headless"),
            RouteSeam::Refuse(MALFORMED.into())
        );
        assert!(
            matches!(seam(Some("zai/"), "claude", "thread"), RouteSeam::Refuse(l) if l == MALFORMED.replace("'no-such-lane'", "'zai/'"))
        );
    }

    #[test]
    fn a_role_on_another_harness_or_an_unrouted_role_takes_the_primary_model() {
        let cwd = std::env::temp_dir();
        assert_eq!(role_seam(None, "claude", &cwd, &no_env), RouteSeam::None);
        assert_eq!(
            role_seam(Some("tidy"), "codex", &cwd, &no_env),
            RouteSeam::None
        );
        assert_eq!(
            role_seam(Some("implement"), "claude", &cwd, &no_env),
            RouteSeam::None
        );
    }

    #[test]
    fn an_unknown_vendor_refuses_with_the_builder_notice() {
        let cwd = std::env::temp_dir();
        match route_seam(
            Some("no-such-vendor/m-1"),
            "claude",
            "thread",
            &cwd,
            &no_env,
        ) {
            RouteSeam::Refuse(line) => {
                assert!(
                    line.starts_with("--route 'no-such-vendor/m-1' refused ("),
                    "{line}"
                );
                assert!(
                    line.ends_with("); no worker launched, node stays dispatchable."),
                    "{line}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

//! The `--role` arm of a Rust-run spawn: port of `model_routing.resolve_route`
//! (the role table, the protected guard, the business-role projection) over
//! the one route builder, `provider_route::claude_route`. A role that routes
//! nowhere launches on the primary model, as the Python lane did; only a
//! blocked or incomplete business role refuses.
//!
//! `resolve_spawn_route`'s endpoint/credential/model checks have no twin
//! here: they fire only on a hand-built partial env, and `claude_route`
//! always answers the complete set.

use std::collections::BTreeMap;
use std::path::Path;

use crate::claude_ask::py_repr;
use crate::provider_route::{claude_route, ClaudeRoute, RouteCtx};

/// `DEFAULT_SECONDARY_MODEL` and `DEFAULT_ROUTED_ROLES`: the roles routed to
/// zai by default. The config `model_routing.roles` map overrides per role.
pub const DEFAULT_SECONDARY_MODEL: &str = "glm-5.3";
pub const DEFAULT_ROUTED_ROLES: &[&str] =
    &["coordinate", "tidy", "orient", "consolidate", "post-merge"];
/// Roles the secondary provider never touches, enforced before config.
pub const PROTECTED_ROLES: &[&str] = &["implement", "review-verdict"];
pub const PROTECTED_ROLE_FLOOR: &str = "high";

/// A role resolved to a route env, stamped with its provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRoute {
    pub provider: String,
    pub model: String,
    pub env: BTreeMap<String, String>,
}

/// The `model_routing` block, read once: `enabled` (default true) and the
/// effective role table (built-ins overlaid by config, keys lowercased).
pub struct RoutingBlock {
    pub enabled: bool,
    pub roles: BTreeMap<String, String>,
}

impl RoutingBlock {
    pub fn read(cwd: &Path) -> Self {
        let block =
            crate::agents_config::config_table_merged(cwd, &["model_routing"]).unwrap_or_default();
        let enabled = block
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        let mut roles: BTreeMap<String, String> = DEFAULT_ROUTED_ROLES
            .iter()
            .map(|r| (r.to_string(), format!("zai,{DEFAULT_SECONDARY_MODEL}")))
            .collect();
        if let Some(table) = block.get("roles").and_then(toml::Value::as_table) {
            for (k, v) in table {
                let v = match v {
                    toml::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                roles.insert(k.trim().to_lowercase(), v);
            }
        }
        RoutingBlock { enabled, roles }
    }
}

/// The business-role answer short of a refusal: no role by that name, a role
/// that declares no routing, or its routing hint.
pub enum Business {
    NotFound,
    Unrouted,
    Hint {
        provider: Option<String>,
        model: Option<String>,
    },
}

fn business(role: &str) -> Result<Business, String> {
    match crate::roles::routing::business_role(role) {
        Ok(routing) => Ok(match routing.routing_projection {
            None => Business::Unrouted,
            Some(hint) => Business::Hint {
                provider: hint.provider,
                model: hint.model,
            },
        }),
        Err(blocked) if blocked.reason == "not_found" => Ok(Business::NotFound),
        Err(blocked) => {
            let detail = blocked
                .detail
                .as_deref()
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            Err(format!(
                "business role {} is blocked ({}){detail}; no worker launched",
                py_repr(&blocked.role.id),
                blocked.reason
            ))
        }
    }
}

/// `_business_role_target`: the legacy table target, completed or replaced
/// by a business role's projection.
fn target(
    name: &str,
    block: &RoutingBlock,
    business: Business,
) -> Result<Option<(String, String)>, String> {
    let legacy = block
        .roles
        .get(name)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| crate::spawn_route::parse_target(raw));
    let (provider, model) = match business {
        Business::NotFound => return Ok(legacy),
        Business::Unrouted => return Ok(None),
        Business::Hint { provider, model } => (provider, model),
    };
    let provider = provider.or_else(|| legacy.as_ref().map(|l| l.0.clone()));
    let model = model.or_else(|| legacy.as_ref().map(|l| l.1.clone()));
    match (provider, model) {
        (Some(p), Some(m)) => Ok(Some((p, m))),
        (p, m) => {
            let missing: Vec<&str> = [("provider", p.is_none()), ("model", m.is_none())]
                .into_iter()
                .filter(|(_, gone)| *gone)
                .map(|(f, _)| f)
                .collect();
            Err(format!(
                "resolved business role {} has an incomplete routing projection: \
                 missing {}; no configured legacy target can complete it; no worker launched",
                py_repr(name),
                missing.join(" and ")
            ))
        }
    }
}

/// Resolve one spawn's `--role`. `Ok(None)` launches on the primary model;
/// each notice is printed by the caller, the same lines Python's `notice`
/// sink wrote to stderr.
pub fn resolve(
    role: &str,
    block: &RoutingBlock,
    cwd: &Path,
    env_get: &dyn Fn(&str) -> Option<String>,
    notices: &mut Vec<String>,
) -> Result<Option<RoleRoute>, String> {
    resolve_with(role, block, cwd, env_get, notices, &business)
}

fn resolve_with(
    role: &str,
    block: &RoutingBlock,
    cwd: &Path,
    env_get: &dyn Fn(&str) -> Option<String>,
    notices: &mut Vec<String>,
    business: &dyn Fn(&str) -> Result<Business, String>,
) -> Result<Option<RoleRoute>, String> {
    let requested = role.trim();
    let name = requested.to_lowercase();
    if name.is_empty() {
        return Ok(None);
    }
    // Discovery runs before the guard, so a malformed manifest cannot hide
    // behind the protected-role fallback.
    let found = business(requested)?;
    if PROTECTED_ROLES.contains(&name.as_str()) {
        notices.push(format!(
            "protected-role({name}) floor={PROTECTED_ROLE_FLOOR}; never routed to a secondary provider"
        ));
        return Ok(None);
    }
    if !block.enabled {
        return Ok(None);
    }
    let Some((provider, model)) = target(&name, block, found)? else {
        return Ok(None);
    };
    match claude_route(cwd, &provider, &model, &RouteCtx::Role(name), env_get) {
        ClaudeRoute::Routed {
            mut env,
            provider: pname,
            notices: n,
        } => {
            notices.extend(n);
            env.insert(
                crate::spawn_route::ROUTE_PROVIDER_ENV.to_string(),
                pname.clone(),
            );
            Ok(Some(RoleRoute {
                provider: pname,
                model,
                env,
            }))
        }
        ClaudeRoute::Skipped { notices: n } => {
            notices.extend(n);
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(enabled: bool, extra: &[(&str, &str)]) -> RoutingBlock {
        let mut roles: BTreeMap<String, String> = DEFAULT_ROUTED_ROLES
            .iter()
            .map(|r| (r.to_string(), format!("zai,{DEFAULT_SECONDARY_MODEL}")))
            .collect();
        for (k, v) in extra {
            roles.insert(k.to_string(), v.to_string());
        }
        RoutingBlock { enabled, roles }
    }

    fn none(_: &str) -> Result<Business, String> {
        Ok(Business::NotFound)
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_protected_role_never_routes_and_says_so() {
        let mut notes = Vec::new();
        let got = resolve_with(
            " Implement ",
            &block(true, &[("implement", "zai/glm-5.3")]),
            &std::env::temp_dir(),
            &no_env,
            &mut notes,
            &none,
        );
        assert_eq!(got, Ok(None));
        assert_eq!(
            notes,
            vec!["protected-role(implement) floor=high; never routed to a secondary provider"]
        );
    }

    #[test]
    fn an_unrouted_or_disabled_role_launches_on_the_primary_model() {
        let cwd = std::env::temp_dir();
        let mut notes = Vec::new();
        assert_eq!(
            resolve_with("lead", &block(true, &[]), &cwd, &no_env, &mut notes, &none),
            Ok(None)
        );
        assert_eq!(
            resolve_with("tidy", &block(false, &[]), &cwd, &no_env, &mut notes, &none),
            Ok(None)
        );
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_blocked_business_role_refuses_before_the_guard() {
        let blocked = |_: &str| -> Result<Business, String> { Err("blocked line".into()) };
        let mut notes = Vec::new();
        let got = resolve_with(
            "implement",
            &block(true, &[]),
            &std::env::temp_dir(),
            &no_env,
            &mut notes,
            &blocked,
        );
        assert_eq!(got, Err("blocked line".into()));
    }

    #[test]
    fn a_business_projection_completes_from_the_legacy_table_or_refuses() {
        let b = block(true, &[]);
        let hint = |p: Option<&str>, m: Option<&str>| Business::Hint {
            provider: p.map(str::to_string),
            model: m.map(str::to_string),
        };
        assert_eq!(
            target("tidy", &b, hint(None, Some("glm-5.2"))),
            Ok(Some(("zai".into(), "glm-5.2".into())))
        );
        assert_eq!(
            target("tidy", &b, Business::Unrouted),
            Ok(None),
            "a role with no projection routes nowhere"
        );
        assert_eq!(
            target("writer", &b, hint(Some("zai"), None)),
            Err(
                "resolved business role 'writer' has an incomplete routing projection: \
                 missing model; no configured legacy target can complete it; no worker launched"
                    .into()
            )
        );
    }
}

//! The fleet-side block validators of `fno/config/__init__.py` (HandoffBlock
//! through WorkBlock), ported one function per pydantic decorator.

use super::{py_repr, Ctx, Fail, Registry};
use serde_json::{Map, Number, Value};

pub(super) fn register(reg: &mut Registry) {
    reg.after(
        "HandoffBlock",
        &["used_pct_trigger"],
        used_pct_trigger_range,
    );
    reg.after(
        "HandoffBlock",
        &["lead_used_pct_trigger"],
        lead_used_pct_trigger_range,
    );
    reg.model_after("HandoffBlock", lead_trigger_below_teammate_trigger);

    reg.before(
        "TargetDefaultsBlock",
        &["max_iterations"],
        max_iterations_positive,
    );
    reg.before("TargetConfig", &["blast"], dict_or_empty);
    reg.after(
        "InboxBlock",
        &["unclaimed_ttl", "landed_abandon_ttl"],
        ttl_is_non_negative,
    );
    reg.after("A2aBlock", &["turn_ceiling"], ceiling_is_positive);
    reg.before("SidelineBlock", &["layout"], coerce_layout);
    reg.before("ProviderBudget", &["lanes"], coerce_lane_dimension);
    reg.before("ProviderBudget", &["subagents"], coerce_budget_dimension);
    reg.before(
        "ProcessAdmissionBlock",
        &["max_processes"],
        coerce_max_processes,
    );

    reg.before("AgentsBlock", &["profiles"], coerce_profiles);
    reg.before("AgentsBlock", &["fallback"], dict_or_empty);
    reg.after("AgentsBlock", &["confirm"], confirm_is_known);
    reg.before("AgentsBlock", &["max_live"], coerce_max_live);
    reg.before(
        "AgentsBlock",
        &["max_live_per_territory"],
        coerce_max_live_per_territory,
    );
    reg.before("AgentsBlock", &["pane_group_max"], coerce_pane_group_max);
    reg.model_before("AgentsBlock", accept_legacy_max_lanes);
    reg.model_before("AgentsBlock", accept_legacy_spawn_permission_mode);
    reg.before("AgentsBlock", &["provider_limits"], coerce_provider_limits);
    reg.model_after("AgentsBlock", warn_max_load_per_cpu_deprecated);
    reg.before(
        "AgentsBlock",
        &["min_free_gb", "max_swap_pct"],
        coerce_ram_floor_terms,
    );
    reg.before(
        "AgentsBlock",
        &["max_load_per_cpu"],
        coerce_max_load_per_cpu,
    );
    reg.before(
        "AgentsBlock",
        &["max_fleet_cpu_share"],
        coerce_max_fleet_cpu_share,
    );
    reg.before("AgentsBlock", &["worker_qos"], coerce_worker_qos);
    reg.before("AgentsBlock", &["claude", "codex", "gemini"], dict_or_empty);
    reg.before("AgentsBlock", &["defaults"], dict_or_empty);

    reg.before("AutonomyBlock", &["enabled"], coerce_bool_default_true);
    reg.before("AutoContinueBlock", &["enabled"], coerce_affirmative);
    reg.before("KeepGoingBlock", &["enabled"], coerce_affirmative);

    reg.before(
        "ThinkSpawnBlock",
        &["enabled", "on_work_start", "on_decompose_wave0"],
        coerce_affirmative,
    );
    reg.before("ThinkSpawnBlock", &["max_per_run"], coerce_max_per_run);
    reg.before(
        "ThinkSpawnBlock",
        &["idle_threshold_s"],
        coerce_idle_threshold,
    );
    reg.before("ThinkSpawnBlock", &["daily_cap"], coerce_daily_cap);
    reg.before("ThinkSpawnBlock", &["attended"], coerce_attended);
    reg.before("ThinkSpawnBlock", &["substrate"], coerce_legacy_substrate);

    reg.before("JoinBlock", &["sandbox"], coerce_sandbox);

    reg.before("AutoMergeBlock", &["enabled"], coerce_affirmative);
    reg.before("AutoMergeBlock", &["grant"], coerce_grant);
    reg.before(
        "AutoMergeBlock",
        &[
            "delete_branch_on_merge",
            "require_checks_pass",
            "require_fresh_ci",
        ],
        coerce_affirmative,
    );
    reg.before("AutoMergeBlock", &["merge_strategy"], coerce_strategy);
    reg.before(
        "AutoMergeBlock",
        &["conflict_resolution"],
        coerce_conflict_resolution,
    );
    reg.before("AutoMergeBlock", &["remediation"], coerce_remediation);

    reg.before("GroomBlock", &["enabled"], coerce_bool_default_true);
    reg.model_before("RecoveryBlock", coerce_legacy_watchdog);
    reg.model_before("HealthThresholdsBlock", sanitize_health_thresholds);
    reg.before(
        "HealthMonitorBlock",
        &["thresholds", "notifications", "history"],
        dict_or_empty,
    );
    reg.before(
        "ResourceMeterBlock",
        &["thresholds", "notifications", "history"],
        dict_or_empty,
    );
    reg.model_before("CollisionThresholdsBlock", sanitize_collision_thresholds);
    reg.before("CollisionBlock", &["severity_thresholds"], dict_or_empty);
    reg.before("WorkBlock", &["workspaces"], dict_or_empty);
}

// ---------------------------------------------------------------------------
// Python value helpers
// ---------------------------------------------------------------------------

/// `isinstance(v, int) and not isinstance(v, bool)`: the integer value.
fn py_int_value(v: &Value) -> Option<i128> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

/// `int(s)` on a str: surrounding whitespace, one sign, digits with single
/// underscores between them.
fn py_int_str(s: &str) -> Option<i64> {
    let t = s.trim();
    let body = match t.strip_prefix('-') {
        Some(rest) => rest,
        None => t.strip_prefix('+').unwrap_or(t),
    };
    if !underscores_between_digits(body) || !body.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let digits: String = t.chars().filter(|c| *c != '_' && *c != '+').collect();
    digits.parse::<i64>().ok()
}

/// `float(s)` on a str.
fn py_float_str(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.contains('_') {
        if !underscores_between_digits(t) {
            return None;
        }
        return t.replace('_', "").parse::<f64>().ok();
    }
    t.parse::<f64>().ok()
}

/// A non-empty run where every `_` sits between two ASCII digits.
fn underscores_between_digits(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return false;
    }
    chars.iter().enumerate().all(|(i, c)| {
        *c != '_'
            || (i > 0
                && i + 1 < chars.len()
                && chars[i - 1].is_ascii_digit()
                && chars[i + 1].is_ascii_digit())
    })
}

fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn float_value(f: f64) -> Value {
    Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// `{k: v for k, v in data.items() if k != key}`.
fn without(map: &Map<String, Value>, key: &str) -> Map<String, Value> {
    map.iter()
        .filter(|(k, _)| k.as_str() != key)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// A mapping passes; anything else degrades to the empty table.
fn dict_or_empty(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_object() {
        v
    } else {
        Value::Object(Map::new())
    })
}

// ---------------------------------------------------------------------------
// HandoffBlock
// ---------------------------------------------------------------------------

fn pct_range(name: &str, v: &Value) -> Result<(), Fail> {
    let n = py_int_value(v).unwrap_or(0);
    if !(1..=100).contains(&n) {
        return Err(Fail::Value(format!(
            "config.target.handoff.{name} must be in range 1-100; got {}",
            py_repr(v)
        )));
    }
    Ok(())
}

fn used_pct_trigger_range(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    pct_range("used_pct_trigger", &v)?;
    Ok(v)
}

fn lead_used_pct_trigger_range(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    pct_range("lead_used_pct_trigger", &v)?;
    Ok(v)
}

fn lead_trigger_below_teammate_trigger(
    ctx: &mut Ctx,
    m: &mut Map<String, Value>,
) -> Result<(), Fail> {
    if !ctx.fields_set.iter().any(|f| f == "lead_used_pct_trigger") {
        return Ok(());
    }
    let lead = m
        .get("lead_used_pct_trigger")
        .cloned()
        .unwrap_or(Value::Null);
    let used = m.get("used_pct_trigger").cloned().unwrap_or(Value::Null);
    if py_int_value(&lead).unwrap_or(0) >= py_int_value(&used).unwrap_or(0) {
        return Err(Fail::Value(format!(
            "config.target.handoff.lead_used_pct_trigger must be BELOW used_pct_trigger \
             (got {}, teammate trigger {}). A lead compacts earlier than a teammate on \
             purpose: a worker's degradation costs one node, a lead's propagates into every \
             ruling it issues and every worker it routes, and the handoff itself costs context.",
            py_repr(&lead),
            py_repr(&used)
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Target, inbox, a2a, sideline
// ---------------------------------------------------------------------------

fn max_iterations_positive(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let ok = match &v {
        Value::Number(_) => py_int_value(&v).is_some_and(|n| n >= 1),
        Value::String(s) => py_int_str(s).is_some_and(|n| n >= 1),
        _ => false,
    };
    if !ok {
        ctx.note(format!(
            "config.target.defaults.max_iterations={} invalid; using default 40",
            py_repr(&v)
        ));
        return Ok(Value::from(40));
    }
    Ok(v)
}

fn ttl_is_non_negative(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if py_int_value(&v).unwrap_or(0) < 0 {
        return Err(Fail::Value(format!(
            "config.inbox.{} must be >= 0; got {}",
            ctx.field_name,
            py_repr(&v)
        )));
    }
    Ok(v)
}

fn ceiling_is_positive(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if py_int_value(&v).unwrap_or(0) < 1 {
        return Err(Fail::Value(format!(
            "config.agents.a2a.turn_ceiling must be >= 1; got {}",
            py_repr(&v)
        )));
    }
    Ok(v)
}

fn coerce_layout(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let list = matches!(&v, Value::String(s) if s.trim().to_lowercase() == "list");
    Ok(Value::from(if list { "list" } else { "card" }))
}

// ---------------------------------------------------------------------------
// Budgets and agents
// ---------------------------------------------------------------------------

/// `_coerce_positive_int`: a positive int, a str holding one, else `default`.
fn positive_int_or(v: &Value, default: Value) -> Value {
    match v {
        Value::Number(_) => match py_int_value(v) {
            Some(n) if n >= 1 => v.clone(),
            _ => default,
        },
        Value::String(s) => match py_int_str(s) {
            Some(n) if n >= 1 => Value::from(n),
            _ => default,
        },
        _ => default,
    }
}

/// `_positive_int`: an int (never a bool) of at least 1.
fn is_positive_int(v: &Value) -> bool {
    py_int_value(v).is_some_and(|n| n >= 1)
}

/// `_lane_count`: an int (never a bool) of at least 0. Zero closes the lane.
fn is_lane_count(v: &Value) -> bool {
    py_int_value(v).is_some_and(|n| n >= 0)
}

fn coerce_lane_dimension(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(match &v {
        Value::Number(_) if is_lane_count(&v) => v.clone(),
        Value::String(s) => py_int_str(s)
            .filter(|n| *n >= 0)
            .map_or(Value::Null, Value::from),
        _ => Value::Null,
    })
}

fn coerce_budget_dimension(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(positive_int_or(&v, Value::Null))
}

fn coerce_max_processes(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(positive_int_or(&v, Value::from(400)))
}

fn coerce_max_live(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(positive_int_or(&v, Value::from(3)))
}

fn coerce_max_live_per_territory(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(positive_int_or(&v, Value::from(4)))
}

fn coerce_pane_group_max(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if is_positive_int(&v) {
        v
    } else {
        Value::from(4)
    })
}

fn coerce_profiles(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::Object(map) = v else {
        return Ok(Value::Object(Map::new()));
    };
    Ok(Value::Object(
        map.into_iter().filter(|(_, val)| val.is_object()).collect(),
    ))
}

fn confirm_is_known(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if !matches!(v.as_str(), Some("always" | "auto" | "never")) {
        return Err(Fail::Value(format!(
            "config.agents.confirm must be one of always|auto|never; got {}",
            py_repr(&v)
        )));
    }
    Ok(v)
}

fn accept_legacy_max_lanes(ctx: &mut Ctx, data: Value) -> Result<Value, Fail> {
    let Value::Object(map) = &data else {
        return Ok(data);
    };
    if !map.contains_key("max_lanes") {
        return Ok(data);
    }
    if map.contains_key("provider_limits") {
        ctx.note(
            "fno config: [agents] carries both provider_limits and the legacy max_lanes; \
             using provider_limits and ignoring max_lanes",
        );
        return Ok(Value::Object(without(map, "max_lanes")));
    }
    ctx.note("fno config: [agents] max_lanes is renamed provider_limits; the legacy spelling still parses ");
    let mut out = map.clone();
    out.insert("provider_limits".to_string(), map["max_lanes"].clone());
    Ok(Value::Object(without(&out, "max_lanes")))
}

/// `_legacy_permission_mode.accept_legacy_spawn_permission_mode`.
fn accept_legacy_spawn_permission_mode(ctx: &mut Ctx, data: Value) -> Result<Value, Fail> {
    let Value::Object(map) = &data else {
        return Ok(data);
    };
    let Some(legacy_val) = map.get("spawn_permission_mode").cloned() else {
        return Ok(data);
    };
    let mut defaults = match map.get("defaults") {
        Some(Value::Object(d)) => d.clone(),
        _ => Map::new(),
    };
    let modern_val = defaults
        .get("permission_mode")
        .cloned()
        .unwrap_or(Value::Null);
    if py_truthy(&modern_val) {
        ctx.note(format!(
            "fno config: agents.spawn_permission_mode is retired; agents.defaults.permission_mode \
             is already set to {} and wins, the legacy value {} is dropped",
            py_repr(&modern_val),
            py_repr(&legacy_val)
        ));
    } else if legacy_val.as_str() == Some("") {
        defaults.insert("permission_mode".to_string(), legacy_val);
        ctx.note(
            "fno config: agents.spawn_permission_mode was explicitly set to \"\" (the old opt-out \
             from auto-approval on autonomous dispatch); agents.defaults.permission_mode has no \
             equivalent opt-out - an unset value there resolves the built-in bypassPermissions \
             for any command-seeded spawn. Set agents.defaults.permission_mode to \"default\" to \
             keep normal prompting.",
        );
    } else {
        defaults.insert("permission_mode".to_string(), legacy_val);
        ctx.note(
            "fno config: agents.spawn_permission_mode is renamed agents.defaults.permission_mode; \
             the legacy spelling still parses ",
        );
    }
    let mut out = without(map, "spawn_permission_mode");
    out.insert("defaults".to_string(), Value::Object(defaults));
    Ok(Value::Object(out))
}

fn builtin_provider_budgets() -> Value {
    let mut zai = Map::new();
    zai.insert("lanes".to_string(), Value::from(5));
    zai.insert("subagents".to_string(), Value::from(1));
    let mut out = Map::new();
    out.insert("zai".to_string(), Value::Object(zai));
    Value::Object(out)
}

fn is_provider_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn coerce_provider_limits(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::Object(table) = &v else {
        return Ok(builtin_provider_budgets());
    };
    let builtins = builtin_provider_budgets();
    let mut out = Map::new();
    for (provider, budget) in table {
        if !is_provider_name(provider) {
            return Ok(builtin_provider_budgets());
        }
        let fields: Map<String, Value> = if is_lane_count(budget) {
            let mut one = Map::new();
            one.insert("lanes".to_string(), budget.clone());
            one
        } else if let Value::Object(dims) = budget {
            if dims.keys().any(|k| k != "lanes" && k != "subagents") {
                return Ok(builtin_provider_budgets());
            }
            let kept: Map<String, Value> = dims
                .iter()
                .filter(|(k, d)| {
                    if k.as_str() == "lanes" {
                        is_lane_count(d)
                    } else {
                        is_positive_int(d)
                    }
                })
                .map(|(k, d)| (k.clone(), d.clone()))
                .collect();
            if kept.len() != dims.len() {
                return Ok(builtin_provider_budgets());
            }
            kept
        } else {
            return Ok(builtin_provider_budgets());
        };
        let mut merged = match builtins.get(provider.as_str()) {
            Some(Value::Object(b)) => b.clone(),
            _ => Map::new(),
        };
        for (k, d) in fields {
            merged.insert(k, d);
        }
        out.insert(provider.clone(), Value::Object(merged));
    }
    Ok(Value::Object(out))
}

fn warn_max_load_per_cpu_deprecated(ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    if m.get("max_load_per_cpu").and_then(Value::as_f64) != Some(8.0) {
        ctx.note(
            "fno config: agents.max_load_per_cpu is deprecated and ignored: admission decides on \
             the fleet's CPU share (agents.max_fleet_cpu_share); delete the key",
        );
    }
    Ok(())
}

/// `_finite_or`: a finite float from a number or a numeric str, else `default`.
fn finite_or(v: &Value, default: f64) -> Value {
    let parsed = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => py_float_str(s),
        _ => None,
    };
    float_value(parsed.filter(|f| f.is_finite()).unwrap_or(default))
}

fn coerce_ram_floor_terms(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let default = if ctx.field_name == "min_free_gb" {
        4.0
    } else {
        90.0
    };
    Ok(finite_or(&v, default))
}

fn coerce_max_load_per_cpu(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(finite_or(&v, 8.0))
}

fn coerce_max_fleet_cpu_share(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(finite_or(&v, 0.5))
}

fn coerce_worker_qos(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        let word = s.trim().to_lowercase();
        if word == "utility" || word == "off" {
            return Ok(Value::from(word));
        }
    }
    Ok(Value::from("utility"))
}

// ---------------------------------------------------------------------------
// Opt-in switches
// ---------------------------------------------------------------------------

/// The strict affirmative: a bool, int 1, or a yes-word; anything else is
/// `fallback`.
fn affirmative_or(v: &Value, fallback: bool) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(_) => match py_int_value(v) {
            Some(n) => n == 1,
            None => fallback,
        },
        Value::String(s) => matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => fallback,
    }
}

fn coerce_affirmative(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(Value::Bool(affirmative_or(&v, false)))
}

fn coerce_bool_default_true(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(Value::Bool(affirmative_or(&v, true)))
}

/// `ThinkSpawnBlock._nonneg_int_or`.
fn nonneg_int_or(v: &Value, fallback: i64) -> Value {
    match v {
        Value::Number(_) => match py_int_value(v) {
            Some(n) if n >= 0 => v.clone(),
            _ => Value::from(fallback),
        },
        Value::String(s) => {
            let t = s.trim();
            if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
                t.parse::<i64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| Value::from(fallback))
            } else {
                Value::from(fallback)
            }
        }
        _ => Value::from(fallback),
    }
}

fn coerce_max_per_run(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(nonneg_int_or(&v, 5))
}

fn coerce_idle_threshold(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(nonneg_int_or(&v, 0))
}

fn coerce_daily_cap(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(nonneg_int_or(&v, 20))
}

fn coerce_attended(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let spawn = matches!(&v, Value::String(s) if s.trim().to_lowercase() == "spawn");
    Ok(Value::from(if spawn { "spawn" } else { "offer" }))
}

fn coerce_legacy_substrate(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match &v {
        Value::Null => Ok(Value::Null),
        Value::String(s) if s.trim().is_empty() => Ok(Value::Null),
        Value::String(s) => {
            let word = s.trim().to_lowercase();
            if matches!(word.as_str(), "pane" | "thread" | "bg" | "headless") {
                Ok(Value::from(word))
            } else {
                Ok(Value::from("bg"))
            }
        }
        _ => Ok(Value::from("bg")),
    }
}

fn coerce_sandbox(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(if v.is_boolean() {
        v
    } else {
        Value::Bool(false)
    })
}

/// A trimmed str inside `allowed`, else `fallback`.
fn trimmed_choice(v: &Value, allowed: &[&str], fallback: &str) -> Value {
    match v {
        Value::String(s) if allowed.contains(&s.trim()) => Value::from(s.trim()),
        _ => Value::from(fallback),
    }
}

fn coerce_grant(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(trimmed_choice(&v, &["none", "dispatch"], "none"))
}

fn coerce_strategy(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(trimmed_choice(&v, &["merge", "squash", "rebase"], "merge"))
}

fn coerce_conflict_resolution(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(trimmed_choice(&v, &["opus", "fail"], "opus"))
}

fn coerce_remediation(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    Ok(trimmed_choice(&v, &["attempt", "verify_only"], "attempt"))
}

// ---------------------------------------------------------------------------
// Recovery, health, collision
// ---------------------------------------------------------------------------

/// `_watchdog.coerce_legacy`: a one-word `watchdog` becomes a block.
fn coerce_legacy_watchdog(_ctx: &mut Ctx, data: Value) -> Result<Value, Fail> {
    let Value::Object(map) = &data else {
        return Ok(data);
    };
    let flat = map.get("watchdog");
    let mut block = match flat {
        Some(Value::Object(b)) => b.clone(),
        _ => Map::new(),
    };
    let is_word = matches!(flat, Some(Value::String(_)));
    if let Some(Value::String(s)) = flat {
        let word = s.trim().to_lowercase();
        let enabled = !(word.is_empty() || word == "off");
        block.insert("enabled".to_string(), Value::Bool(enabled));
        if enabled {
            block.insert("mode".to_string(), Value::from(word));
        }
    }
    if !block.is_empty() || is_word {
        let mut out = map.clone();
        out.insert("watchdog".to_string(), Value::Object(block));
        return Ok(Value::Object(out));
    }
    Ok(data)
}

fn sanitize_health_thresholds(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::Object(map) = &v else {
        return Ok(v);
    };
    let mut out = map.clone();
    for key in [
        "idea_pile_depth",
        "stale_ready_days",
        "failure_prone_attempts",
        "collision_count",
        "project_cwd_mismatch",
    ] {
        let Some(raw) = out.get(key).cloned() else {
            continue;
        };
        // `int(x)` truncates a float toward zero, so -0.5 passes as 0.
        let ok = match &raw {
            Value::Number(n) => match py_int_value(&raw) {
                Some(i) => i >= 0,
                None => n
                    .as_f64()
                    .is_some_and(|f| f.is_finite() && f.trunc() >= 0.0),
            },
            Value::String(s) => py_int_str(s).is_some_and(|i| i >= 0),
            _ => false,
        };
        if !ok {
            ctx.note(format!(
                "config.health_monitor.thresholds.{key}={} invalid; using default",
                py_repr(&raw)
            ));
            out = without(&out, key);
        }
    }
    Ok(Value::Object(out))
}

fn sanitize_collision_thresholds(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::Object(map) = &v else {
        return Ok(v);
    };
    let mut out = map.clone();
    for key in ["high_count", "high_ratio", "medium_count", "medium_ratio"] {
        let Some(raw) = out.get(key).cloned() else {
            continue;
        };
        let num = match &raw {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => py_float_str(s),
            _ => None,
        };
        match num {
            Some(f) if f >= 0.0 => {
                out.insert(key.to_string(), float_value(f));
            }
            _ => {
                ctx.note(format!(
                    "config.collision.severity_thresholds.{key}={} is not numeric or is negative; using default",
                    py_repr(&raw)
                ));
                out = without(&out, key);
            }
        }
    }
    Ok(Value::Object(out))
}

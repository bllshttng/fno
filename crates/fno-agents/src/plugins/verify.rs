//! Pure two-axis verification of a pack before activation.
//!
//! Every condition reports twice: `checked` (was it evaluated at all) and a
//! four-state `result` (passed, failed, blocked, unknown). Collapsing the two
//! into one boolean is how a verifier starts reporting green for a condition
//! it never evaluated, so only an all-passed report exits zero.
//!
//! Verification reads the manifest and an installed-pack index and writes
//! nothing. It never spawns a command: runnability is a static check. A
//! malformed, unreadable or schema-invalid manifest reports `blocked`, naming
//! the file and the error, and never reads as an absent pack.

use super::manifest::PackManifest;
use super::{is_safe_component, resolve_path, version};
use crate::client_commands::py_repr_str;
use crate::roles::models::{
    CapabilityFact, ContextBundleBounds, ContextReference, DefinitionSource, Manifest, RoleLayer,
    WorkOrderRef,
};
use crate::roles::resolver::{self, Request, Resolution};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const IDENTITY: &str = "identity";
pub const SCHEMA: &str = "schema";
pub const DEPENDENCY: &str = "dependency";
pub const CAPABILITY: &str = "capability";
pub const COMPATIBILITY: &str = "compatibility";
pub const DECLARED_TEST: &str = "declared-test";

const TOPOLOGIES: [&str; 4] = ["direct", "loop", "squad", "pipeline"];

/// A packaged agent's tool list is bounded: no ceiling grants a network,
/// shell or delegation tool. The set is uniform today; the bound role's
/// ceiling is where a tighter per-ceiling policy would slot in.
const AGENT_TOOL_ALLOWLIST: [&str; 5] = ["Read", "Write", "Edit", "Glob", "Grep"];
const INTERPRETERS: [&str; 8] = [
    "bash", "sh", "zsh", "python", "python3", "ruby", "node", "perl",
];
const SCRIPT_SUFFIXES: [&str; 5] = [".sh", ".py", ".rb", ".js", ".pl"];

#[derive(Clone, Debug)]
pub struct Condition {
    pub family: &'static str,
    pub name: String,
    pub checked: bool,
    pub result: String,
    pub detail: Option<String>,
}

impl Condition {
    pub fn ok(&self) -> bool {
        self.checked && self.result == "passed"
    }
}

fn cond(
    family: &'static str,
    name: impl Into<String>,
    result: &str,
    detail: Option<String>,
) -> Condition {
    Condition {
        family,
        name: name.into(),
        checked: true,
        result: result.to_string(),
        detail,
    }
}

fn verdict(ok: bool, fail: &'static str) -> &'static str {
    if ok {
        "passed"
    } else {
        fail
    }
}

pub struct Report {
    pub pack_path: String,
    pub conditions: Vec<Condition>,
}

impl Report {
    pub fn ok(&self) -> bool {
        !self.conditions.is_empty() && self.conditions.iter().all(Condition::ok)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "pack_path": self.pack_path,
            "ok": self.ok(),
            "conditions": self.conditions.iter().map(|c| json!({
                "family": c.family,
                "name": c.name,
                "checked": c.checked,
                "result": c.result,
                "detail": c.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

/// A directory names its `plugin.yaml`; a file names itself.
pub fn manifest_file(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("plugin.yaml")
    } else {
        path.to_path_buf()
    }
}

/// Load and check a manifest, or answer the one blocked condition.
pub fn load_manifest(path: &Path) -> Result<PackManifest, Condition> {
    let file = manifest_file(path);
    let label = file.to_string_lossy().into_owned();
    let blocked = |name: &str, detail: String| cond(SCHEMA, name, "blocked", Some(detail));
    let bytes = std::fs::read(&file).map_err(|e| {
        blocked(
            "manifest-load",
            format!(
                "unreadable manifest {label}: {}",
                crate::roles::registry::os_error_text(&e, &file)
            ),
        )
    })?;
    let text = String::from_utf8(bytes).map_err(|e| {
        blocked(
            "manifest-load",
            format!("invalid UTF-8 in manifest {label}: {e}"),
        )
    })?;
    let raw: Value = serde_yaml_ng::from_str(&text).map_err(|e| {
        blocked(
            "manifest-load",
            format!("malformed YAML in manifest {label}: {e}"),
        )
    })?;
    if !raw.is_object() {
        return Err(blocked(
            "manifest-load",
            format!("manifest {label} is not a mapping"),
        ));
    }
    PackManifest::parse(&raw).map_err(|e| {
        blocked(
            "manifest-schema",
            format!("manifest {label} failed schema validation: {e}"),
        )
    })
}

fn identity_conditions(manifest: &PackManifest) -> Vec<Condition> {
    let pack_id_safe = is_safe_component(&manifest.id);
    let mut out = vec![cond(
        IDENTITY,
        "pack-id",
        verdict(pack_id_safe, "failed"),
        Some(if pack_id_safe {
            format!("id={} version={}", manifest.id, manifest.version)
        } else {
            format!(
                "unsafe pack id {}: must be a single path component",
                py_repr_str(&manifest.id)
            )
        }),
    )];
    let first = manifest.digest();
    let second = manifest.digest();
    out.push(cond(
        IDENTITY,
        "digest-stability",
        verdict(first == second, "failed"),
        Some(first),
    ));
    let role_ids: Vec<&str> = manifest.roles.iter().map(|r| r.role.id.as_str()).collect();
    let unsafe_id = role_ids.iter().find(|id| !is_safe_component(id));
    out.push(cond(
        IDENTITY,
        "role-ids-safe",
        verdict(unsafe_id.is_none(), "failed"),
        unsafe_id.map(|id| format!("unsafe role id {}", py_repr_str(id))),
    ));
    let lower: Vec<String> = role_ids.iter().map(|id| id.to_lowercase()).collect();
    let case_duplicate = role_ids
        .iter()
        .find(|id| lower.iter().filter(|l| **l == id.to_lowercase()).count() > 1);
    out.push(cond(
        IDENTITY,
        "role-ids-case-unique",
        verdict(case_duplicate.is_none(), "failed"),
        case_duplicate.map(|id| format!("role id {} collides case-insensitively", py_repr_str(id))),
    ));
    let duplicate = role_ids
        .iter()
        .find(|id| role_ids.iter().filter(|other| other == id).count() > 1);
    out.push(cond(
        IDENTITY,
        "role-and-function-ids-unique",
        verdict(duplicate.is_none(), "failed"),
        duplicate.map(|id| format!("duplicate role id {}", py_repr_str(id))),
    ));
    let functions: BTreeSet<&str> = manifest
        .roles
        .iter()
        .map(|r| r.function_id.as_str())
        .collect();
    let joined = functions.into_iter().collect::<Vec<_>>().join(",");
    out.push(cond(
        IDENTITY,
        "function-set",
        "passed",
        Some(if joined.is_empty() {
            "no roles".into()
        } else {
            joined
        }),
    ));
    out
}

fn schema_conditions(manifest: &PackManifest) -> Vec<Condition> {
    // Every declaration already parsed against a model refusing unknown
    // fields; reaching here means the schema held.
    let counts = [
        ("roles", manifest.roles.len()),
        ("skills", manifest.skills.len()),
        ("workflows", manifest.workflows.len()),
        ("adapters", manifest.adapters.len()),
        ("evaluators", manifest.evaluators.len()),
        ("assets", manifest.assets.len()),
        ("permissions", manifest.permissions.len()),
        ("scenarios", manifest.scenarios.len()),
    ];
    let detail = counts
        .iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<_>>()
        .join("; ");
    vec![cond(SCHEMA, "declarations-parse", "passed", Some(detail))]
}

fn declared_source_conditions(manifest: &PackManifest, base: &Path) -> Vec<Condition> {
    // Each declared workflow, skill, agent and asset source must resolve to an
    // existing entry beneath the pack directory.
    let mut declared: Vec<(&str, &str, &str)> = Vec::new();
    declared.extend(
        manifest
            .workflows
            .iter()
            .map(|w| ("workflow", w.id.as_str(), w.source.as_str())),
    );
    declared.extend(
        manifest
            .skills
            .iter()
            .map(|s| ("skill", s.id.as_str(), s.source.as_str())),
    );
    declared.extend(
        manifest
            .agents
            .iter()
            .map(|a| ("agent", a.id.as_str(), a.source.as_str())),
    );
    declared.extend(
        manifest
            .assets
            .iter()
            .map(|a| ("asset", a.id.as_str(), a.source.as_str())),
    );
    let base_resolved = resolve_path(base);
    declared
        .into_iter()
        .map(|(kind, id, source)| {
            let resolved = resolve_path(&base.join(source));
            let exists = resolved != base_resolved
                && resolved.starts_with(&base_resolved)
                && resolved.exists();
            let name = format!("source:{kind}:{id}");
            if exists {
                cond(SCHEMA, name, "passed", Some(format!("{source} present")))
            } else {
                cond(
                    SCHEMA,
                    name,
                    "failed",
                    Some(format!("{source} missing or escapes the pack directory")),
                )
            }
        })
        .collect()
}

/// An agent source's YAML frontmatter: `None` when it cannot be read or
/// parsed, an empty map when it carries none.
fn read_frontmatter(source: &Path) -> Option<serde_json::Map<String, Value>> {
    let text = std::fs::read_to_string(source).ok()?;
    if !text.starts_with("---") {
        return Some(Default::default());
    }
    let lines: Vec<&str> = text.lines().collect();
    let Some(end) = (1..lines.len()).find(|&i| lines[i].trim() == "---") else {
        return Some(Default::default());
    };
    let loaded: Value = serde_yaml_ng::from_str(&lines[1..end].join("\n")).ok()?;
    Some(match loaded {
        Value::Object(map) => map,
        _ => Default::default(),
    })
}

/// Python's `str()` of a loaded YAML value.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

/// Python's `repr()` of a loaded YAML value.
fn py_repr(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => py_repr_str(text),
        other => py_str(other),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn agent_binding_conditions(manifest: &PackManifest, base: &Path) -> Vec<Condition> {
    // Frontmatter is copied verbatim at bundle time, so this check is the only
    // thing binding an agent's real tool list to its role.
    let mut out = Vec::new();
    for agent in &manifest.agents {
        let binding = format!("agent-role-binding:{}", agent.id);
        out.push(match &agent.role {
            None => cond(
                SCHEMA,
                binding,
                "passed",
                Some("no role binding declared".into()),
            ),
            Some(role) if manifest.roles.iter().any(|r| r.role == *role) => cond(
                SCHEMA,
                binding,
                "passed",
                Some(format!("role {} declared by this pack", role.id)),
            ),
            Some(role) => cond(
                SCHEMA,
                binding,
                "failed",
                Some(format!("role {} is not declared by this pack", role.id)),
            ),
        });
        let tools_name = format!("agent-tools-bounded:{}", agent.id);
        let identity_name = format!("agent-frontmatter-identity:{}", agent.id);
        // Containment before read: an escaping source is already failed by
        // source:agent, and verify must not read an arbitrary external path.
        let agent_resolved = resolve_path(&base.join(&agent.source));
        if !agent_resolved.starts_with(resolve_path(base)) {
            for name in [&tools_name, &identity_name] {
                out.push(cond(
                    SCHEMA,
                    name.clone(),
                    "blocked",
                    Some(format!(
                        "{} escapes the pack directory; not read",
                        agent.source
                    )),
                ));
            }
            continue;
        }
        let Some(frontmatter) = read_frontmatter(&agent_resolved) else {
            out.push(cond(
                SCHEMA,
                tools_name,
                "blocked",
                Some(format!("cannot read {} to check tools", agent.source)),
            ));
            continue;
        };
        // The copied identity fields must match the declaration and the pack,
        // so another bounded agent cannot ride this agent's id.
        let fm_name = py_str(frontmatter.get("name"));
        let fm_pack = py_str(frontmatter.get("pack"));
        let fm_role = frontmatter.get("role");
        let role_ok = agent
            .role
            .as_ref()
            .is_none_or(|role| py_str(fm_role) == role.id);
        if fm_name == agent.id && fm_pack == manifest.id && role_ok {
            out.push(cond(
                SCHEMA,
                identity_name,
                "passed",
                Some("frontmatter name/pack/role match the declaration".into()),
            ));
        } else {
            out.push(cond(
                SCHEMA,
                identity_name,
                "failed",
                Some(format!(
                    "frontmatter identity mismatch: name={} pack={} role={} vs declaration id={} pack={} role={}",
                    py_repr_str(&fm_name),
                    py_repr_str(&fm_pack),
                    py_repr(fm_role),
                    py_repr_str(&agent.id),
                    py_repr_str(&manifest.id),
                    agent.role.as_ref().map_or("None".into(), |r| py_repr_str(&r.id)),
                )),
            ));
        }
        let Some(raw_tools) = frontmatter.get("tools") else {
            // An omitted tools field inherits the harness default tool set.
            out.push(cond(
                SCHEMA,
                tools_name,
                "failed",
                Some(
                    "no tools frontmatter; the agent would inherit the harness default tool set"
                        .into(),
                ),
            ));
            continue;
        };
        if !truthy(raw_tools) {
            out.push(cond(
                SCHEMA,
                tools_name,
                "failed",
                Some(
                    "empty tools list; the agent would inherit the harness default tool set".into(),
                ),
            ));
            continue;
        }
        let tools: Vec<String> = match raw_tools {
            Value::Array(list) => list.iter().map(|t| py_str(Some(t))).collect(),
            other => vec![py_str(Some(other))],
        };
        let offenders: BTreeSet<&str> = tools
            .iter()
            .map(String::as_str)
            .filter(|t| !AGENT_TOOL_ALLOWLIST.contains(t))
            .collect();
        if offenders.is_empty() {
            out.push(cond(
                SCHEMA,
                tools_name,
                "passed",
                Some(format!(
                    "all {} tool(s) within the bounded allowlist",
                    tools.len()
                )),
            ));
        } else {
            out.push(cond(
                SCHEMA,
                tools_name,
                "failed",
                Some(format!(
                    "tools outside the bounded allowlist: {}",
                    offenders.into_iter().collect::<Vec<_>>().join(", ")
                )),
            ));
        }
    }
    out
}

fn range_text(minimum: &str, maximum: Option<&str>) -> String {
    format!("{minimum}..{}", maximum.unwrap_or("open"))
}

fn dependency_conditions(
    manifest: &PackManifest,
    installed: &BTreeMap<String, String>,
) -> Vec<Condition> {
    let mut out = Vec::new();
    for dependency in &manifest.depends_on {
        let name = format!("dependency:{}", dependency.pack_id);
        let range = &dependency.range;
        let span = range_text(&range.minimum, range.maximum.as_deref());
        let Some(resolved) = installed.get(&dependency.pack_id) else {
            out.push(cond(
                DEPENDENCY,
                name,
                "blocked",
                Some(format!(
                    "declared dependency {} ({span}) is not installed",
                    dependency.pack_id
                )),
            ));
            continue;
        };
        out.push(
            match version::in_range(resolved, &range.minimum, range.maximum.as_deref()) {
                Err(e) => cond(
                    DEPENDENCY,
                    name,
                    "failed",
                    Some(format!(
                        "{} version {} is not parseable: {e}",
                        dependency.pack_id,
                        py_repr_str(resolved)
                    )),
                ),
                Ok(false) => cond(
                    DEPENDENCY,
                    name,
                    "failed",
                    Some(format!(
                        "{} resolved to {resolved}, outside {span}",
                        dependency.pack_id
                    )),
                ),
                Ok(true) => cond(
                    DEPENDENCY,
                    name,
                    "passed",
                    Some(format!("{} resolved to {resolved}", dependency.pack_id)),
                ),
            },
        );
    }
    if manifest.depends_on.is_empty() {
        out.push(cond(DEPENDENCY, "no-declared-dependencies", "passed", None));
    }
    out
}

fn topology_conditions(manifest: &PackManifest) -> Vec<Condition> {
    manifest
        .roles
        .iter()
        .map(|role| {
            let name = format!("topology:{}", role.role.id);
            let value = &role.default_topology;
            if TOPOLOGIES.contains(&value.as_str()) {
                cond(
                    COMPATIBILITY,
                    name,
                    "passed",
                    Some(format!("{} default_topology={value}", role.role.id)),
                )
            } else {
                cond(
                    COMPATIBILITY,
                    name,
                    "failed",
                    Some(format!(
                        "{} default_topology {}: set default_topology to one of: direct, loop, squad, pipeline",
                        role.role.id,
                        py_repr_str(value)
                    )),
                )
            }
        })
        .collect()
}

/// One synthetic resolution per packaged role: the definition as it would
/// land in the plugin layer, a fact for every required capability and an
/// eligible reference for every selector. A structural refusal fails; any
/// other refusal is unknown, since the real facts arrive per work order.
fn resolution_conditions(manifest: &PackManifest) -> Vec<Condition> {
    let revision = "verify-snapshot";
    let clock = chrono::Utc::now().fixed_offset();
    manifest
        .roles
        .iter()
        .map(|role| {
            let request = synthetic_request(manifest, role, revision, clock);
            let name = format!("resolve:{}", role.role.id);
            match resolver::resolve(&request) {
                Resolution::Blocked(blocked) => {
                    let structural = matches!(
                        blocked.reason,
                        "invalid_manifest" | "invalid_overlay" | "not_found"
                    );
                    cond(
                        COMPATIBILITY,
                        name,
                        if structural { "failed" } else { "unknown" },
                        Some(format!(
                            "{} blocked at {}: {}",
                            role.role.id,
                            blocked.reason,
                            blocked.detail.as_deref().unwrap_or("None")
                        )),
                    )
                }
                Resolution::Resolved(value) => {
                    let digest = value
                        .get("manifest_digest")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    cond(
                        COMPATIBILITY,
                        name,
                        "passed",
                        Some(format!(
                            "{} manifest={}",
                            role.role.id,
                            &digest[..digest.len().min(12)]
                        )),
                    )
                }
            }
        })
        .collect()
}

fn synthetic_request(
    manifest: &PackManifest,
    role: &Manifest,
    revision: &str,
    clock: crate::roles::models::Instant,
) -> Request {
    let definition = DefinitionSource {
        layer: RoleLayer::Plugin,
        source_id: format!("plugin/{}/{}.json", manifest.id, role.role.id),
        snapshot_revision: revision.into(),
        role: role.role.clone(),
        manifest: Some(role.clone()),
        status: "valid",
        error: None,
    };
    let capability_facts = role
        .required_capabilities
        .iter()
        .map(|capability| CapabilityFact {
            capability: capability.clone(),
            available: true,
            source_id: "verify-synthetic".into(),
            snapshot_revision: revision.into(),
            work_order_scope: None,
        })
        .collect();
    let context_catalog = role
        .context_selectors
        .iter()
        .map(|selector| {
            let seed = format!(
                "verify-context:{}:{}",
                selector.kind,
                selector.identifier.as_deref().unwrap_or("*")
            );
            let content_digest: String = Sha256::digest(seed.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ContextReference {
                kind: selector.kind.clone(),
                identifier: selector
                    .identifier
                    .clone()
                    .unwrap_or_else(|| "verify-synthetic".into()),
                provenance: "verify-synthetic".into(),
                work_order_scope: None,
                content_digest: Some(content_digest),
                content_revision: Some(revision.into()),
                snapshot_revision: revision.into(),
                fresh_until: selector
                    .requires_freshness
                    .then(|| clock + chrono::Duration::hours(1)),
                sensitivity: "public".into(),
                byte_size: 1,
                readable: true,
                unavailable_reason: None,
            }
        })
        .collect();
    Request {
        role: role.role.clone(),
        definitions: vec![definition],
        capability_facts,
        context_catalog,
        work_order: WorkOrderRef {
            node_id: "verify-pack".into(),
            attempt_id: "verify-attempt".into(),
            principal_id: None,
            role_id: Some(role.role.id.clone()),
        },
        clock,
        snapshot_revision: revision.into(),
        bounds: ContextBundleBounds {
            max_references: 32,
            max_bytes: 10_000_000,
            max_sensitivity: "sensitive".into(),
            max_combinations: 4096,
        },
        unchecked: Vec::new(),
    }
}

fn compatibility_conditions(manifest: &PackManifest) -> Vec<Condition> {
    let running = env!("CARGO_PKG_VERSION");
    let compat = &manifest.footnote_compat;
    let (result, detail) =
        match version::in_range(running, &compat.minimum, compat.maximum.as_deref()) {
            Ok(true) => (
                "passed",
                format!("running footnote {running} within declared range"),
            ),
            Ok(false) => (
                "failed",
                format!(
                    "running footnote {running} outside {}",
                    range_text(&compat.minimum, compat.maximum.as_deref())
                ),
            ),
            Err(e) => (
                "failed",
                format!("declared compat range is not parseable: {e}"),
            ),
        };
    let mut out = vec![
        cond(COMPATIBILITY, "footnote-compat-range", result, Some(detail)),
        cond(
            COMPATIBILITY,
            "role-manifest-schema",
            "passed",
            Some(format!(
                "{} role(s) parsed as RoleManifest",
                manifest.roles.len()
            )),
        ),
    ];
    out.extend(topology_conditions(manifest));
    out.extend(resolution_conditions(manifest));
    out
}

fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access(2) reads a NUL-terminated path and writes nothing.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

/// `shutil.which`: the first executable, non-directory `PATH` entry.
fn which(token: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        let candidate = dir.join(token);
        candidate.exists() && !candidate.is_dir() && executable(&candidate)
    })
}

fn token_resolves(token: &str, base: &Path) -> bool {
    if token.contains('/') || token.contains('\\') {
        let candidate = base.join(token);
        return candidate.is_file() && executable(&candidate);
    }
    which(token)
}

/// Static runnability: the runner resolves on `PATH` or as an executable
/// pack-relative path, and an interpreter naming a script file needs that
/// script to exist. Never executes the command.
fn command_is_runnable(command: &str, base: &Path) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let Some(&runner) = parts.first() else {
        return false;
    };
    if !token_resolves(runner, base) {
        return false;
    }
    let runner_name = runner.rsplit('/').next().unwrap_or(runner);
    if INTERPRETERS.contains(&runner_name) {
        if let Some(script) = parts[1..].iter().find(|p| !p.starts_with('-')) {
            let scriptish =
                script.contains('/') || SCRIPT_SUFFIXES.iter().any(|s| script.ends_with(s));
            if scriptish && !base.join(script).is_file() {
                return false;
            }
        }
    }
    true
}

fn first_token(command: &str) -> String {
    py_repr_str(command.split_whitespace().next().unwrap_or(""))
}

fn declared_test_conditions(manifest: &PackManifest, base: &Path) -> Vec<Condition> {
    let mut out: Vec<Condition> = manifest
        .scenarios
        .iter()
        .map(|scenario| {
            let name = format!("scenario:{}", scenario.id);
            if command_is_runnable(&scenario.command, base) {
                cond(
                    DECLARED_TEST,
                    name,
                    &scenario.recorded_result,
                    Some(format!(
                        "{} command runnable; recorded={}",
                        scenario.id, scenario.recorded_result
                    )),
                )
            } else {
                cond(
                    DECLARED_TEST,
                    name,
                    "blocked",
                    Some(format!(
                        "{} command {} is absent or not executable",
                        scenario.id,
                        first_token(&scenario.command)
                    )),
                )
            }
        })
        .collect();
    if manifest.scenarios.is_empty() {
        out.push(cond(
            DECLARED_TEST,
            "no-declared-scenarios",
            "passed",
            Some("pack declares no benchmark scenarios; nothing to fail".into()),
        ));
    }
    out
}

fn evaluator_conditions(manifest: &PackManifest, base: &Path) -> Vec<Condition> {
    // Evaluators are required evidence producers: a command that does not
    // resolve blocks, so a pack cannot verify green on vaporware producers.
    let mut out: Vec<Condition> = manifest
        .evaluators
        .iter()
        .map(|evaluator| {
            let name = format!("evaluator-runnable:{}", evaluator.id);
            if command_is_runnable(&evaluator.command, base) {
                cond(
                    SCHEMA,
                    name,
                    "passed",
                    Some(format!(
                        "{} command runnable: {}",
                        evaluator.id, evaluator.command
                    )),
                )
            } else {
                cond(
                    SCHEMA,
                    name,
                    "blocked",
                    Some(format!(
                        "{} command {} is absent or not executable",
                        evaluator.id,
                        first_token(&evaluator.command)
                    )),
                )
            }
        })
        .collect();
    if manifest.evaluators.is_empty() {
        out.push(cond(
            SCHEMA,
            "no-declared-evaluators",
            "passed",
            Some("pack declares no evaluators; nothing to fail".into()),
        ));
    }
    out
}

/// Verify an already-checked manifest, so activation verifies the exact
/// object it will project. `base` resolves declared sources and commands.
pub fn verify_manifest(
    manifest: &PackManifest,
    installed: &BTreeMap<String, String>,
    base: &Path,
) -> Report {
    let mut conditions = identity_conditions(manifest);
    conditions.extend(schema_conditions(manifest));
    conditions.extend(declared_source_conditions(manifest, base));
    conditions.extend(agent_binding_conditions(manifest, base));
    conditions.extend(dependency_conditions(manifest, installed));
    // No declaration is a grant: a permission is an effect_class and
    // destination pair with no approval field, so this holds structurally.
    conditions.push(cond(
        CAPABILITY,
        "declarations-are-not-grants",
        "passed",
        None,
    ));
    conditions.extend(compatibility_conditions(manifest));
    conditions.extend(declared_test_conditions(manifest, base));
    conditions.extend(evaluator_conditions(manifest, base));
    Report {
        pack_path: base.to_string_lossy().into_owned(),
        conditions,
    }
}

/// Verify the pack at `path` (a directory or its `plugin.yaml`).
pub fn verify_pack(path: &Path, installed: &BTreeMap<String, String>) -> Report {
    match load_manifest(path) {
        Err(failure) => Report {
            pack_path: path.to_string_lossy().into_owned(),
            conditions: vec![failure],
        },
        Ok(manifest) => {
            let base = if path.is_file() {
                path.parent().unwrap_or(Path::new("")).to_path_buf()
            } else {
                path.to_path_buf()
            };
            verify_manifest(&manifest, installed, &base)
        }
    }
}

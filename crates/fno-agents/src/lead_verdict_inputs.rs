//! The one native owner of every non-journal input to `lead-verdict` :
//! caller team, canonical scope, manifest, config values, graph rows, the
//! check-in window, and the inherited/filed delivery split. `lead_history.rs`
//! keeps the journal scan, verdict, payload, and rendering it already owns.
//!
//! Python assembled these nine values and passed them on the argv (PR 1911);
//! this module is the port that lets the Python shell shrink to a transport
//! (law d-b6cc1a2a: new code in `crates/`, existing Python ported, never
//! extended). Every refusal is a typed `Err` naming the failed reading -
//! nothing here degrades to fleet-wide, zero, or a default verdict.

use crate::AgentStatus;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The default check-in interval (`config.lead.checkin_interval`, "55m").
const DEFAULT_CHECKIN_SECS: i64 = 3300;
/// The window is three check-in intervals.
const WINDOW_INTERVALS: i64 = 3;
/// The default compaction ceiling for a harness with no override.
pub(crate) const DEFAULT_COMPACTION_CEILING: i64 = 3;
/// Codex compacts about four times as often per hour (the 2026-10-03 lead
/// comparison), so its built-in handoff point sits four claude ceilings out.
const DEFAULT_COMPACTION_CEILING_CODEX: i64 = 12;

/// A sortable instant: the `Z` and `+00:00` UTC spellings must compare equal,
/// so a Z-suffixed manifest date and a +00:00 graph date cannot misorder at
/// The same-second teaming boundary.
fn ts_key(raw: &str) -> String {
    if let Some(stripped) = raw.strip_suffix('Z') {
        format!("{stripped}+00:00")
    } else {
        raw.to_string()
    }
}

/// The delivery trend split so filing cannot inflate it (Python `scope_split`,
/// lead/scope.py). A lead filing real nodes raises undelivered by working
/// well, so the undelivered sides stay split inherited vs filed. Closes read
/// the other way: a close by ANYONE is delivery evidence, except a node the
/// lead's own session filed (`lead_session`, the manifest's teamed session) -
/// the lead closing its own filings must not read as convergence, so that
/// close stays invisible to both close counts (lead ruling, 2026-10-26). A row
/// with no birth date reads as inherited: an old undated backlog is work the
/// lead can be stalled on, never fresh filing that excuses it. A row with no
/// filer reads as not-own-filed, so its close counts.
pub(crate) fn scope_split(
    ids: &std::collections::HashSet<String>,
    entries: &[Value],
    generation_start: &str,
    window_start: &str,
    lead_session: Option<&str>,
) -> (u64, u64, u64, u64) {
    let generation_start = ts_key(generation_start);
    let window = ts_key(window_start);
    let mut inherited_undelivered: u64 = 0;
    let mut filed_undelivered: u64 = 0;
    let mut inherited_closed_in_window: u64 = 0;
    let mut filed_closed_in_window: u64 = 0;
    for row in entries {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !ids.contains(id) {
            continue;
        }
        let created = ts_key(row.get("created_at").and_then(Value::as_str).unwrap_or(""));
        let inherited = created < generation_start;
        if !crate::graph_store::is_terminal_entry(row) {
            if inherited {
                inherited_undelivered += 1;
            } else {
                filed_undelivered += 1;
            }
        } else {
            let own_filed = lead_session.is_some_and(|session| {
                row.get("source_session_id").and_then(Value::as_str) == Some(session)
            });
            if own_filed {
                continue;
            }
            let completed = ts_key(
                row.get("completed_at")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            if completed >= window {
                if inherited {
                    inherited_closed_in_window += 1;
                } else {
                    filed_closed_in_window += 1;
                }
            }
        }
    }
    (
        inherited_undelivered,
        filed_undelivered,
        inherited_closed_in_window,
        filed_closed_in_window,
    )
}

/// `f"{window_s // 3600}h" if window_s % 3600 == 0 else f"{window_s // 60}m"`
/// - the display spelling Python passed as `--window`.
fn window_display(window_secs: i64) -> String {
    if window_secs % 3600 == 0 {
        format!("{}h", window_secs / 3600)
    } else {
        format!("{}m", window_secs / 60)
    }
}

/// Every input the verdict read needs, resolved. `window` is the display
/// spelling the payload carries.
#[derive(Debug)]
pub(crate) struct VerdictInputs {
    pub scope: String,
    /// The parsed manifest, carried so the verdict command never re-reads it.
    pub manifest: crate::loopcheck::LeadManifest,
    pub manifest_path: PathBuf,
    pub harness: String,
    /// Where the harness answer came from: `manifest`, or `registry` when the
    /// holder row outranked a manifest naming a foreign harness.
    pub harness_source: String,
    /// Set when the holder row's harness differs from the manifest's claim:
    /// the manifest is stale, the row wins, and the payload says so.
    pub harness_disagreement: Option<String>,
    pub now: chrono::DateTime<chrono::Utc>,
    pub window: String,
    pub checkin_interval_secs: i64,
    pub team_age_secs: i64,
    pub generation_start: String,
    pub generation_start_source: String,
    pub team_inherited: Option<bool>,
    pub team_from_session: Option<String>,
    pub compaction_ceiling: u64,
    pub inherited_undelivered: u64,
    pub filed_undelivered: u64,
    pub inherited_closed_in_window: u64,
    /// In-window closes on rows another session filed. The lead's own
    /// filings' closes count in neither field (the guard in `scope_split`).
    pub filed_closed_in_window: u64,
}

/// The caller's canonical team scope: explicit `--scope` wins (canonicalized,
/// refused when it names nothing); else the live registry row for this
/// session's own identity, requiring a stamped team on a row the
/// `row_verdict` door has not finished. Shared by the
/// verdict, checkin and history commands: the Rust reader tolerates unknown
/// keys, so a registry row carrying a field this binary predates no longer
/// blinds the team resolution the way the strict Python reader did.
pub(crate) fn resolve_scope(
    explicit_scope: Option<&str>,
    registry_path: &Path,
) -> Result<String, String> {
    if let Some(raw) = explicit_scope {
        let canonical = crate::territory::canonical_scope(raw.trim());
        if canonical.is_empty() {
            return Err("--scope names no team territory.".to_string());
        }
        return Ok(canonical);
    }
    let (session, harness) = crate::claims::resolve_identity();
    let (Some(session), Some(harness)) = (session, harness) else {
        return Err("cannot resolve the caller's team: no teamed registry row. \
             Pass --scope explicitly."
            .to_string());
    };
    let registry = crate::state::load_registry(registry_path).map_err(|e| {
        format!("cannot resolve the caller's team: the agent registry could not be read: {e}")
    })?;
    let Some(row) = crate::lead_state::find_by_session(&registry.entries, &session, Some(&harness))
    else {
        return Err(format!(
            "cannot resolve the caller's team: no registry row names session {session}"
        ));
    };
    if matches!(row.status, AgentStatus::Failed) {
        return Err(format!(
            "the registry row for {session} is failed, a terminal state"
        ));
    }
    if let crate::row_verdict::RowVerdict::Finished(why) = crate::row_verdict::fno_verdict(row) {
        return Err(format!(
            "the registry row for {session} is finished ({why}), a terminal state"
        ));
    }
    let team = row
        .role_scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "the registry row for {session} carries no role_scope: the row \
                 is unstamped, so no team proves this session's authority"
            )
        })?;
    let canonical = crate::territory::canonical_scope(team);
    if canonical.is_empty() {
        return Err("this session holds no team. Pass --scope <territory>.".to_string());
    }
    Ok(canonical)
}

/// The manifest the verdict reads: explicit and authoritative, else
/// `<space>/leads/<scope>.md` for the named scope (a deliberate change from
/// Python, which resolved the CALLER's manifest even under an explicit scope
/// - reading one team's lead through another team's manifest is the bug
/// closes).
fn resolve_manifest(
    cwd: &Path,
    scope: &str,
    explicit_manifest: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(p) = explicit_manifest {
        let path = p.to_path_buf();
        if !path.is_file() {
            return Err(format!("{}: manifest not found", path.display()));
        }
        return Ok(path);
    }
    let path = crate::lead_state::manifest_path(&crate::paths::space_dir(cwd), scope)?;
    if !path.is_file() {
        return Err(format!("{}: manifest not found", path.display()));
    }
    Ok(path)
}

fn generation_start(
    registry: &crate::state::Registry,
    harness: &str,
    manifest: &crate::loopcheck::LeadManifest,
    manifest_created_at: &str,
    manifest_created: &chrono::DateTime<chrono::FixedOffset>,
    registry_path: &Path,
) -> Result<(String, String), String> {
    let Some(session_id) = manifest
        .harness_session_id
        .as_deref()
        .filter(|session_id| !session_id.trim().is_empty())
    else {
        return Ok((manifest_created_at.to_string(), "manifest".to_string()));
    };
    let Some(row) = registry.find_by_session(harness, session_id) else {
        return Ok((manifest_created_at.to_string(), "manifest".to_string()));
    };
    let row_created = chrono::DateTime::parse_from_rfc3339(&row.created_at).map_err(|error| {
        format!(
            "{}: registry row for session {session_id} has invalid created_at: {error}",
            registry_path.display()
        )
    })?;
    let (start, source) = if row_created < manifest_created.clone() {
        (row.created_at.clone(), "registry")
    } else {
        (manifest_created_at.to_string(), "manifest")
    };
    Ok((start, source.to_string()))
}

/// Alias members (a project's `short_name` spelling) resolve to their
/// canonical project, so the manifest path and the graph compile select what
/// the team actually wrote at `<space>/leads/<canonical>.md`. Canonical
/// members map to themselves, so a registry-derived scope is unchanged.
pub(crate) fn canonical_members_with_aliases(scope: &str, cwd: &Path) -> String {
    let map = crate::org_board::scope::project_map(cwd).unwrap_or_default();
    let members: Vec<String> = scope
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|m| map.get(m).cloned().unwrap_or_else(|| m.to_string()))
        .collect();
    crate::territory::canonical_scope(&members.join(","))
}

/// `lead.checkin_interval` through the layered config lookup, with Python's
/// default. An unparseable interval IS the default (the Pydantic validator's
/// behavior). Shared by the verdict window and the hook beat's due check.
pub(crate) fn checkin_interval_secs(cwd: &Path) -> i64 {
    match crate::agents_config::config_lookup(cwd, &["lead", "checkin_interval"]) {
        Some(v) => crate::territory::parse_duration_to_seconds(&v).unwrap_or(DEFAULT_CHECKIN_SECS),
        None => DEFAULT_CHECKIN_SECS,
    }
}

/// The built-in handoff point for a harness with no config override.
pub(crate) fn default_ceiling(harness: &str) -> i64 {
    if harness == "codex" {
        DEFAULT_COMPACTION_CEILING_CODEX
    } else {
        DEFAULT_COMPACTION_CEILING
    }
}

/// The lead's handoff point for one harness: `lead.handoff.<harness>.
/// compaction_ceiling`, then the global `lead.compaction_ceiling`, then the
/// built-in default. A non-integer or negative value is a named refusal
/// naming the full key, never zero. Zero itself is a legitimate ceiling: it
/// declares that no compaction is ever acceptable.
pub(crate) fn compaction_ceiling_for(cwd: &Path, harness: &str) -> Result<i64, String> {
    let specific = format!("lead.handoff.{harness}.compaction_ceiling");
    for (keys, name) in [
        (
            vec!["lead", "handoff", harness, "compaction_ceiling"],
            specific,
        ),
        (
            vec!["lead", "compaction_ceiling"],
            "lead.compaction_ceiling".to_string(),
        ),
    ] {
        let Some(v) = crate::agents_config::config_lookup(cwd, &keys) else {
            continue;
        };
        return match v.as_integer() {
            Some(n) if n >= 0 => Ok(n),
            Some(_) => Err(format!("config {name} must be a non-negative integer")),
            None => Err(format!("config {name} must be an integer")),
        };
    }
    Ok(default_ceiling(harness))
}

/// `lead.checkin_interval` and the per-harness handoff point through the
/// layered config lookup, with the defaults above.
fn read_config(cwd: &Path, harness: &str) -> Result<(i64, i64), String> {
    let interval = checkin_interval_secs(cwd);
    let ceiling = compaction_ceiling_for(cwd, harness)?;
    Ok((interval, ceiling))
}

/// The one entry point. `now` is injectable so tests pin the window; `cwd`
/// anchors the config candidates, the graph, and the space dir exactly as the
/// Python shell did.
pub(crate) fn resolve_verdict_inputs(
    cwd: &Path,
    explicit_scope: Option<&str>,
    explicit_manifest: Option<&Path>,
    registry_path: &Path,
    now: impl Fn() -> chrono::DateTime<chrono::Utc>,
) -> Result<VerdictInputs, String> {
    if crate::graph_get::external_backend_selected() {
        return Err("tracker metadata unavailable under an external tracker backend".to_string());
    }
    let scope = resolve_scope(explicit_scope, registry_path)?;
    let scope = canonical_members_with_aliases(&scope, cwd);
    let manifest_path = resolve_manifest(cwd, &scope, explicit_manifest)?;

    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: unreadable manifest: {e}", manifest_path.display()))?;
    let manifest = crate::loopcheck::parse_lead_manifest(&content)
        .ok_or_else(|| "lead manifest has no frontmatter".to_string())?;
    let registry = crate::state::load_registry(registry_path)
        .map_err(|error| format!("{}: registry unreadable: {error}", registry_path.display()))?;
    let manifest_harness = crate::loopcheck::scan_manifest_field(&content, "harness")
        .unwrap_or_else(|| "claude".into());
    let holder_session = manifest
        .harness_session_id
        .clone()
        .filter(|s| !s.trim().is_empty());
    // The row outranks the manifest on a harness disagreement: a role write
    // that stamped the arming caller's harness (fixed at the write) leaves
    // older manifests naming a foreign harness, and every transcript reader
    // keyed on the manifest harness resolves blind. Prefer the holder row's
    // own harness and name the source so the payload can print the split.
    let (harness, harness_source, harness_disagreement) = match holder_session.as_deref() {
        Some(session_id) => {
            let row = crate::lead_state::find_by_session(
                &registry.entries,
                session_id,
                Some(&manifest_harness),
            )
            .or_else(|| crate::lead_state::find_by_session(&registry.entries, session_id, None));
            match row {
                Some(row) if row.harness.as_deref().is_some_and(|h| !h.is_empty()) => {
                    let row_harness = row.harness.clone().unwrap();
                    let disagreement = (row_harness != manifest_harness).then(|| {
                        format!(
                            "manifest names harness {manifest_harness}; the holder row says \
                             {row_harness}, which wins"
                        )
                    });
                    (row_harness, "registry".to_string(), disagreement)
                }
                _ => (manifest_harness.clone(), "manifest".to_string(), None),
            }
        }
        None => (manifest_harness.clone(), "manifest".to_string(), None),
    };
    let (interval, ceiling) = read_config(cwd, &harness)?;
    let teamed_at = manifest.created_at.clone().ok_or_else(|| {
        format!(
            "{}: no created_at; no measurable split.",
            manifest_path.display()
        )
    })?;

    let created_at = chrono::DateTime::parse_from_rfc3339(&teamed_at).map_err(|e| {
        format!(
            "{}: created_at is not RFC3339: {e}",
            manifest_path.display()
        )
    })?;
    let holder_row = holder_session
        .as_deref()
        .and_then(|session_id| registry.find_by_session(&harness, session_id));
    let (generation_start, generation_start_source) = generation_start(
        &registry,
        &harness,
        &manifest,
        &teamed_at,
        &created_at,
        registry_path,
    )?;
    let (team_inherited, team_from_session) = match holder_row {
        Some(row) => {
            let parent = row
                .spawned_by_session
                .as_deref()
                .filter(|session| !session.trim().is_empty());
            let inherited = parent.is_some() && row.role_grantor.as_deref() != Some("human");
            (
                Some(inherited),
                inherited.then(|| parent.map(str::to_string)).flatten(),
            )
        }
        None => (None, None),
    };
    let current = now();
    let team_age_secs = current
        .signed_duration_since(created_at.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    let window_secs = WINDOW_INTERVALS * interval;
    let window_start = (current - chrono::Duration::seconds(window_secs))
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false);

    let entries = crate::graph_store::read_rows_where(
        &crate::org_board::scope::graph_json_path(cwd),
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "deferred_kind",
                    "superseded_by",
                    "completed_at",
                    "id",
                    "type",
                    "parent",
                    "project",
                    "status",
                    "created_at",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .map_err(|e| format!("scope {scope} graph unreadable: {e}"))?;
    let projects: Result<HashMap<String, String>, String> =
        crate::org_board::scope::project_map(cwd);
    let ids = crate::territory::compile_scope_ids(&scope, &entries, &projects)
        .map_err(|e| format!("scope {scope} unreadable: {e}"))?;
    let (
        inherited_undelivered,
        filed_undelivered,
        inherited_closed_in_window,
        filed_closed_in_window,
    ) = scope_split(
        &ids,
        &entries,
        &generation_start,
        &window_start,
        manifest.harness_session_id.as_deref(),
    );

    Ok(VerdictInputs {
        scope,
        manifest,
        manifest_path,
        harness,
        harness_source,
        harness_disagreement,
        now: current,
        window: window_display(window_secs),
        checkin_interval_secs: interval,
        team_age_secs,
        generation_start,
        generation_start_source,
        team_inherited,
        team_from_session,
        compaction_ceiling: ceiling as u64,
        inherited_undelivered,
        filed_undelivered,
        inherited_closed_in_window,
        filed_closed_in_window,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kvi-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: &str, parent: &str, created_at: &str, extra: Value) -> Value {
        let mut v = serde_json::json!({"id": id, "type": "feature", "project": "fno"});
        if !parent.is_empty() {
            v["parent"] = Value::String(parent.to_string());
        }
        if !created_at.is_empty() {
            v["created_at"] = Value::String(created_at.to_string());
        }
        if let Value::Object(map) = extra {
            for (k, val) in map {
                v[k] = val;
            }
        }
        v
    }

    // --- AC1/AC2-ERR: the resolver's refusal paths ---

    fn pinned_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1789500000, 0).unwrap()
    }

    fn write_manifest(dir: &Path, scope: &str) -> PathBuf {
        let path = dir.join(format!("leads/{scope}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "---\nscope: x\nshape: pass\nfno_id: kd-test\ncreated_at: 2026-09-10T00:00:00Z\n\
             harness_session_id: sess-k\nmax_iterations: 40\n---\n",
        )
        .unwrap();
        path
    }

    /// Snapshot-and-restore scope for env vars a test pins; the original
    /// value (or its absence) is put back on drop, panic included.
    struct EnvPin(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvPin {
        fn take(vars: &[&'static str]) -> Self {
            Self(
                vars.iter()
                    .map(|var| (*var, std::env::var_os(var)))
                    .collect(),
            )
        }
    }

    impl Drop for EnvPin {
        fn drop(&mut self) {
            for (var, saved) in &self.0 {
                match saved {
                    Some(v) => std::env::set_var(var, v),
                    None => std::env::remove_var(var),
                }
            }
        }
    }

    /// The graph store the resolver reads lives at `$FNO_HOME/graph.json`
    /// (SQLite beside it); `paths.graph_json` no longer redirects it.
    fn pin_home(dir: &Path) -> EnvPin {
        let pin = EnvPin::take(&["FNO_HOME"]);
        std::env::set_var("FNO_HOME", dir.join("home"));
        pin
    }

    fn generation_setup(dir: &Path, registry_rows: &[Value]) -> (PathBuf, PathBuf) {
        let manifest = dir.join("leads/x-root.md");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(
            &manifest,
            "---\nscope: x-root\nshape: pass\nfno_id: kd-test\n\
             created_at: 2026-09-18T20:48:22Z\nharness: claude\nharness_session_id: sess-k\n---\n",
        )
        .unwrap();

        fs::create_dir_all(dir.join(".fno")).unwrap();
        let graph = dir.join("home/graph.json");
        fs::create_dir_all(graph.parent().unwrap()).unwrap();
        fs::write(
            dir.join(".fno/config.toml"),
            format!(
                "[paths]\ngraph_json = {:?}\n[[work.workspaces.t.projects]]\nname = \"fno\"\n",
                graph.display()
            ),
        )
        .unwrap();
        crate::graph_store::seed_rows(
            &graph,
            &[
                entry(
                    "x-root",
                    "",
                    "2026-09-18T01:00:00Z",
                    serde_json::json!({"type": "epic", "project": "fno"}),
                ),
                entry(
                    "x-delivery",
                    "x-root",
                    "2026-09-18T01:14:01Z",
                    serde_json::json!({"project": "fno", "status": "ready"}),
                ),
            ],
        )
        .unwrap();
        let registry = dir.join("registry.json");
        fs::write(
            &registry,
            serde_json::json!({"schema_version": 11, "agents": registry_rows}).to_string(),
        )
        .unwrap();
        (manifest, registry)
    }

    fn inherited_holder_row(created_at: &str) -> Value {
        serde_json::json!({
            "name": "lead",
            "cwd": "/tmp",
            "status": "busy",
            "created_at": created_at,
            "harness": "claude",
            "harness_session_id": "sess-k",
            "spawned_by_session": "grantor-session",
            "role_level": 2,
            "role_scope": "x-root",
            "role_grantor": "spawn"
        })
    }

    #[test]
    fn a_missing_manifest_refuses() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // This test holds the env lock; declare() takes it too, so the
        // held variant is the one that cannot deadlock.
        let _root = crate::paths::DeclaredRoot::declare_held("kvi");
        let dir = tmp("no-manifest");
        let err = resolve_verdict_inputs(
            &dir,
            Some("x-root"),
            None,
            &dir.join("registry.json"),
            pinned_now,
        )
        .expect_err("no manifest exists");
        assert!(err.contains("manifest not found"), "{err}");
    }

    #[test]
    fn a_manifest_without_created_at_refuses_the_split() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tmp("no-created-at");
        let path = dir.join("leads/x-root.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "---\nscope: x-root\nshape: pass\nfno_id: kd-test\n---\n",
        )
        .unwrap();
        let err = resolve_verdict_inputs(
            &dir,
            Some("x-root"),
            Some(&path),
            &dir.join("registry.json"),
            pinned_now,
        )
        .expect_err("no created_at means no measurable split");
        assert!(err.contains("no created_at"), "{err}");
    }

    #[test]
    fn an_external_tracker_backend_refuses() {
        let dir = tmp("external-backend");
        write_manifest(&dir, "x-root");
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FNO_TRACKER_BACKEND", "github");
        let err = resolve_verdict_inputs(
            &dir,
            Some("x-root"),
            None,
            &dir.join("registry.json"),
            pinned_now,
        );
        std::env::remove_var("FNO_TRACKER_BACKEND");
        let err = err.expect_err("external backend cannot measure the split");
        assert!(err.contains("tracker metadata unavailable"), "{err}");
    }

    #[test]
    fn delivery_split_starts_at_the_holder_generation() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("FNO_CONFIG");
        let dir = tmp("holder-generation-start");
        let (manifest, registry) =
            generation_setup(&dir, &[inherited_holder_row("2026-09-17T22:47:08Z")]);
        let _home = pin_home(&dir);
        let inputs =
            resolve_verdict_inputs(&dir, Some("x-root"), Some(&manifest), &registry, pinned_now)
                .expect("the holder generation makes the split measurable");
        assert_eq!(inputs.generation_start, "2026-09-17T22:47:08Z");
        assert_eq!(inputs.generation_start_source, "registry");
        assert_eq!(inputs.team_inherited, Some(true));
        assert_eq!(inputs.team_from_session.as_deref(), Some("grantor-session"));
        assert_eq!(inputs.inherited_undelivered, 0);
        assert_eq!(inputs.filed_undelivered, 2);
    }

    #[test]
    fn an_unparseable_ceiling_refuses_and_never_reads_zero() {
        // Hold the env lock: the external-backend test flips
        // FNO_TRACKER_BACKEND process-wide, and this read must not race it.
        // The config anchors on the test's own cwd, so a dangling FNO_CONFIG
        // left by an earlier test's window is cleared first - under the same
        // lock, so no setter can race the clear.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("FNO_CONFIG");
        let dir = tmp("bad-ceiling");
        let manifest = write_manifest(&dir, "x-root");
        fs::create_dir_all(dir.join(".fno")).unwrap();
        // The scope read needs work.workspaces in a candidate config; without
        // a stanza here the suite only passes where the operator's live
        // config supplies one (the flake shape the FNO_CONFIG pin fixed).
        fs::write(
            dir.join(".fno/config.toml"),
            "[[work.workspaces.t.projects]]\nname = \"x-root\"\n[lead]\ncompaction_ceiling = \"three\"\n",
        )
        .unwrap();
        let err = resolve_verdict_inputs(
            &dir,
            Some("x-root"),
            Some(&manifest),
            &dir.join("registry.json"),
            pinned_now,
        )
        .expect_err("a non-integer ceiling is a named refusal");
        assert!(err.contains("compaction_ceiling"), "{err}");
        // AC1-ERR: a bad per-harness value names the full per-harness key.
        // The helper manifest carries no harness field, so the effective
        // harness reads claude and the claude key is the one read.
        fs::write(
            dir.join(".fno/config.toml"),
            "[[work.workspaces.t.projects]]\nname = \"x-root\"\n[lead.handoff.claude]\ncompaction_ceiling = \"x\"\n",
        )
        .unwrap();
        let err = resolve_verdict_inputs(
            &dir,
            Some("x-root"),
            Some(&manifest),
            &dir.join("registry.json"),
            pinned_now,
        )
        .expect_err("a bad per-harness value names the full key");
        assert!(
            err.contains("lead.handoff.claude.compaction_ceiling"),
            "{err}"
        );
    }

    #[test]
    fn a_corrupt_graph_refuses_instead_of_reading_zero() {
        // Same discipline as the ceiling test: lock held, dangling
        // FNO_CONFIG cleared, the config anchors on this test's own cwd.
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("FNO_CONFIG");
        let dir = tmp("corrupt-graph");
        let manifest = write_manifest(&dir, "x-root");
        let graph = dir.join("graph.json");
        fs::write(&graph, "{not json").unwrap();
        // Point the graph read at the corrupt file through the cwd-anchored
        // config: no env pin, no race with a parallel test's resolution.
        fs::create_dir_all(dir.join(".fno")).unwrap();
        fs::write(
            dir.join(".fno/config.toml"),
            format!("[paths]\ngraph_json = {:?}\n", graph.display()),
        )
        .unwrap();
        let err = resolve_verdict_inputs(
            &dir,
            Some("x-root"),
            Some(&manifest),
            &dir.join("registry.json"),
            pinned_now,
        )
        .expect_err("a corrupt graph must never read as drained");
        assert!(
            err.contains("unreadable") || err.contains("corrupt"),
            "{err}"
        );
    }

    // --- the caller-team resolution survives an unknown field ----

    fn write_registry(dir: &Path, rows: Value) -> PathBuf {
        let path = dir.join("registry.json");
        crate::registry_store::seed_raw(
            &path,
            serde_json::json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": rows,
            })
            .to_string(),
        );
        path
    }

    fn teamed_row(extra: Value) -> Value {
        let mut row = serde_json::json!({
            "name": "lead-a792",
            "cwd": "/tmp/x",
            "created_at": "2026-09-01T00:00:00Z",
            "status": "idle",
            "harness": "claude",
            "harness_session_id": "ses-team",
            "role_level": 2,
            "role_scope": "probe fleet",
        });
        if let Value::Object(map) = extra {
            for (k, v) in map {
                row[k] = v;
            }
        }
        row
    }

    /// Identity env for the caller, with every vendor marker cleared so the
    /// canonical markers win unambiguously. Callers hold the env lock.
    fn with_teamed_identity<T>(f: impl FnOnce() -> T) -> T {
        for (marker, _) in crate::claims::HARNESS_SESSION_MARKERS
            .iter()
            .chain(crate::claims::LEGACY_HARNESS_SESSION_MARKERS.iter())
        {
            std::env::remove_var(marker);
        }
        std::env::set_var("FNO_HARNESS_NAME", "claude");
        std::env::set_var("FNO_HARNESS_SESSION_ID", "ses-team");
        let out = f();
        std::env::remove_var("FNO_HARNESS_NAME");
        std::env::remove_var("FNO_HARNESS_SESSION_ID");
        out
    }

    /// The node's requested test, on the surviving leg: an equal-version
    /// registry whose teamed row carries a field this binary does not
    /// model still resolves the caller's team. The strict Python reader
    /// refused the whole file in this shape and took checkin down with it.
    #[test]
    fn teamed_row_with_unknown_field_still_resolves_the_scope() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tmp("unknown-field");
        let path = write_registry(
            &dir,
            serde_json::json!([teamed_row(serde_json::json!({"future_field": "x"}))]),
        );
        with_teamed_identity(|| {
            let scope = resolve_scope(None, &path).expect("unknown key must not blind the read");
            assert_eq!(scope, crate::territory::canonical_scope("probe fleet"));
        });
        // An orphaned row with a live pid is not finished: the row_verdict
        // door decides, not a copied terminal-status list.
        let live = write_registry(
            &dir,
            serde_json::json!([teamed_row(serde_json::json!({
                "status": "orphaned",
                "pid": std::process::id(),
            }))]),
        );
        with_teamed_identity(|| {
            let scope =
                resolve_scope(None, &live).expect("an orphaned row with a live pid resolves");
            assert_eq!(scope, crate::territory::canonical_scope("probe fleet"));
        });
        // The same orphaned shape with a reaped pid is finished.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let dead = write_registry(
            &dir,
            serde_json::json!([teamed_row(serde_json::json!({
                "status": "orphaned",
                "pid": gone_pid,
            }))]),
        );
        with_teamed_identity(|| {
            let err = resolve_scope(None, &dead)
                .expect_err("an orphaned row with a gone pid is finished");
            assert!(err.contains("terminal"), "{err}");
        });
        fs::remove_dir_all(&dir).ok();
    }

    /// An unreadable registry refuses naming the read failure, never the
    /// misleading "no teamed registry row" the Python leg printed for this
    /// case. A MISSING file is a valid empty registry, so the fixture is a
    /// corrupt one: the defect class is a registry the reader cannot parse.
    #[test]
    fn unreadable_registry_refuses_by_name_not_no_teamed_row() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tmp("unreadable");
        let path = dir.join("registry.json");
        fs::write(&path, "{not json").unwrap();
        with_teamed_identity(|| {
            let err = resolve_scope(None, &path).expect_err("must refuse");
            assert!(
                err.contains("the agent registry could not be read"),
                "{err}"
            );
            assert!(!err.contains("no teamed registry row"), "{err}");
        });
        fs::remove_dir_all(&dir).ok();
    }

    // --- the close-count guard (lead ruling, 2026-10-26) ---

    fn scope_ids() -> std::collections::HashSet<String> {
        ["x-root".to_string(), "x-kid".to_string()]
            .into_iter()
            .collect()
    }

    fn close_row(id: &str, created_at: &str, extra: Value) -> Value {
        let mut v = entry(
            id,
            "",
            created_at,
            serde_json::json!({
                "status": "done",
                "completed_at": "2026-10-26T04:00:00+00:00",
            }),
        );
        if let (Value::Object(map), Value::Object(base)) = (&extra, &mut v) {
            for (k, val) in map {
                base.insert(k.clone(), val.clone());
            }
        }
        v
    }

    #[test]
    fn an_other_session_filed_close_counts_in_window() {
        let rows = [close_row(
            "x-kid",
            "2026-10-25T23:00:30+00:00",
            serde_json::json!({"source_session_id": "worker-session"}),
        )];
        let (inherited_open, filed_open, inherited_closed, filed_closed) = scope_split(
            &scope_ids(),
            &rows,
            "2026-10-25T20:13:46+00:00",
            "2026-10-26T01:51:00+00:00",
            Some("lead-session"),
        );
        assert_eq!(inherited_open, 0);
        assert_eq!(filed_open, 0);
        assert_eq!(inherited_closed, 0);
        assert_eq!(filed_closed, 1);
    }

    #[test]
    fn the_lead_s_own_filed_close_counts_in_neither_close_counter() {
        let rows = [close_row(
            "x-kid",
            "2026-10-25T20:13:03+00:00",
            serde_json::json!({"source_session_id": "lead-session"}),
        )];
        let (_, _, inherited_closed, filed_closed) = scope_split(
            &scope_ids(),
            &rows,
            "2026-10-25T20:13:46+00:00",
            "2026-10-26T01:51:00+00:00",
            Some("lead-session"),
        );
        assert_eq!(inherited_closed, 0);
        assert_eq!(filed_closed, 0);
    }

    #[test]
    fn an_inherited_close_still_counts_and_a_filerless_row_is_not_own_filed() {
        let rows = [
            // created before generation_start, no filer recorded: inherited
            // close that still counts (old undated backlog behavior kept).
            close_row("x-root", "2026-10-03T22:26:33+00:00", serde_json::json!({})),
            // filed after generation_start, no filer recorded: counts as a
            // filed close - an absence of a filer is not the lead's own hand.
            close_row("x-kid", "2026-10-25T23:00:30+00:00", serde_json::json!({})),
        ];
        let (inherited_open, filed_open, inherited_closed, filed_closed) = scope_split(
            &scope_ids(),
            &rows,
            "2026-10-25T20:13:46+00:00",
            "2026-10-26T01:51:00+00:00",
            Some("lead-session"),
        );
        assert_eq!(inherited_open, 0);
        assert_eq!(filed_open, 0);
        assert_eq!(inherited_closed, 1);
        assert_eq!(filed_closed, 1);
    }

    #[test]
    fn an_open_filed_child_still_lands_in_filed_undelivered() {
        let rows = [entry(
            "x-kid",
            "x-root",
            "2026-10-25T23:00:30+00:00",
            serde_json::json!({"status": "in_progress"}),
        )];
        let (inherited_open, filed_open, _, _) = scope_split(
            &scope_ids(),
            &rows,
            "2026-10-25T20:13:46+00:00",
            "2026-10-26T01:51:00+00:00",
            Some("lead-session"),
        );
        assert_eq!(inherited_open, 0);
        assert_eq!(filed_open, 1);
    }
}

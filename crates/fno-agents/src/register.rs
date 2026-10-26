//! The `fno agents register` client verb: the /fno-me self-service join.
//!
//! Port of the Python `cmd_register` front (cli.py) over
//! `registry.register_existing_session`'s verb path: resolve the ambient
//! harness identity with the owned-identity ladder, then mint or refresh the
//! caller's own idle roster row under the shared registry lock, so peers can
//! `fno agents mail send` the canonical handle. Client-side and daemon-free:
//! the verb starts nothing, matching the registry-json posture.
//!
//! Contract pinned by `tests/golden/register/` (captured from the Python leg)
//! and `tests/register_parity.rs`. Not in scope here: the hook's pane-worker
//! restamp (`register_session.py main`), which is the SessionStart transport,
//! not this verb.

use crate::identity::{canonical_handle, session_handle_tier};
use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, session_identity_key, OwnedDisposition};
use crate::state::{load_registry, update_registry, RegistryEntry};
use std::io::IsTerminal as _;
use std::path::Path;

const NO_IDENTITY_LINE: &str =
    "no ambient harness identity - nothing to register (run /fno-me inside a claude/codex session)";

pub fn run_register(args: &[String], home: &AgentsHome) -> i32 {
    let mut json_out = false;
    let mut delivery_policy: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" | "-J" => json_out = true,
            "--delivery-policy" => match it.next() {
                Some(v) => delivery_policy = Some(v.clone()),
                None => {
                    eprintln!("error: --delivery-policy wants a value");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("usage: fno agents register [--json] [--delivery-policy bus-only|off]");
                return 0;
            }
            other => {
                eprintln!("register: unknown argument {other:?}");
                return 2;
            }
        }
    }
    if let Some(policy) = &delivery_policy {
        if policy != "bus-only" && policy != "off" {
            eprintln!("error: --delivery-policy accepts 'bus-only' or 'off', got '{policy}'");
            return 2;
        }
    }

    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, home);
    if matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!("{}", ambiguity_message(&ident));
        return 4;
    }
    let session_id = match ident.session_id.clone() {
        Some(sid) if !sid.is_empty() => sid,
        _ => {
            eprintln!("{NO_IDENTITY_LINE}");
            return 3;
        }
    };
    let harness = ident
        .harness
        .clone()
        .filter(|h| !h.is_empty())
        .or_else(|| Some("claude".to_string()))
        .unwrap_or_default();
    if harness.is_empty() {
        eprintln!("{NO_IDENTITY_LINE}");
        return 3;
    }

    // A session that already IS a mesh worker never mints a second row for
    // itself: the session-start restamp keeps the existing row pointed here.
    let self_name = std::env::var("FNO_AGENT_SELF").unwrap_or_default();
    if !self_name.is_empty() {
        let pending = std::env::var("FNO_AGENT_ROW_PENDING").as_deref() == Ok(self_name.as_str());
        let row_holds = load_registry(&home.shared_registry_json())
            .map(|registry| registry.entries.iter().any(|e| e.name == self_name))
            .unwrap_or(false);
        if pending || row_holds {
            eprintln!(
                "register failed: this session already IS mesh worker '{self_name}' \
(FNO_AGENT_SELF); registering would mint a duplicate row for one worker - the \
session-start restamp keeps the existing row pointed at this session"
            );
            return 1;
        }
    }

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let origin_request: Option<String> = Some("operator".to_string());
    let manifest_role = manifest_role_for_session(&session_id, &cwd);

    // Address collisions refuse before any write (the Python closure raises,
    // which aborts the update; a pre-check reaches the same no-write outcome).
    let base = canonical_handle(&session_id);
    if let Ok(registry) = load_registry(&home.shared_registry_json()) {
        let taken_by_same_session = registry.entries.iter().any(|row| {
            row.harness_session_id
                .as_deref()
                .and_then(|sid| session_handle_tier(&base, sid))
                == Some(0)
        });
        if taken_by_same_session {
            eprintln!(
                "register failed: canonical handle '{base}' collision while registering \
session '{session_id}'; use the full session id directly"
            );
            return 1;
        }
    }

    let outcome = update_registry(&home.shared_registry_json(), |registry| {
        let existing = registry.entries.iter_mut().find(|e| {
            e.harness.as_deref() == Some(harness.as_str())
                && e.harness_session_id.as_deref() == Some(session_id.as_str())
        });
        if let Some(row) = existing {
            // The verb passes no explicit status, so the refresh is
            // unconditional: a hand-started session is never live-transport.
            row.status = crate::AgentStatus::Idle;
            row.cwd = cwd.clone();
            // Origin is WRITE-ONCE: a refresh fills an empty origin and may
            // upgrade `adopted` to `operator` (the one transition with no
            // downside); it never touches `spawn`.
            let upgradeable = row.origin.is_none() || row.origin.as_deref() == Some("adopted");
            if upgradeable {
                row.origin = origin_request.clone();
            }
            if let Some(policy) = &delivery_policy {
                row.delivery_policy = if policy == "off" {
                    None
                } else {
                    Some(policy.clone())
                };
            }
            apply_manifest_role(row, manifest_role.as_ref());
            let policy_after = row.delivery_policy.clone();
            return (row.name.clone(), harness.clone(), policy_after);
        }
        let mut name = base.clone();
        let mut suffix = 2;
        while registry.entries.iter().any(|e| e.name == name) {
            name = format!("{base}-{suffix}");
            suffix += 1;
        }
        let mut fresh = RegistryEntry::new(
            Some(session_id.clone()),
            crate::state::Lineage::unproven(
                "operator register: self-registration carries no parent edge",
            ),
        );
        fresh.name = name.clone();
        fresh.harness = Some(harness.clone());
        fresh.cwd = cwd.clone();
        fresh.status = crate::AgentStatus::Idle;
        fresh.origin = origin_request.clone();
        fresh.delivery_policy = match &delivery_policy {
            None => None,
            Some(p) if p == "off" => None,
            Some(p) => Some(p.clone()),
        };
        if harness == "claude" {
            // The derived 8-hex attach key, skipped (not raised) when it
            // collides with an existing address: the full id still resolves.
            let derived = canonical_handle(&session_id);
            let derived_taken = registry
                .entries
                .iter()
                .any(|e| e.name == derived || e.short_id == derived);
            let hex_guarded = derived.chars().count() == 8
                && derived
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
            if hex_guarded && !derived_taken {
                fresh.short_id = derived;
            }
        }
        apply_manifest_role(&mut fresh, manifest_role.as_ref());
        let policy_after = fresh.delivery_policy.clone();
        registry.entries.push(fresh);
        (name, harness.clone(), policy_after)
    });
    let (name, harness, policy_after) = match outcome {
        Ok(found) => found,
        Err(exc) => {
            eprintln!("register failed: registry write failed: {exc}");
            return 1;
        }
    };

    // The delivery-policy clock: the identity key clears any standing clock,
    // and the flip to an injectable policy also drops a pre-migration
    // name-keyed one. Best effort on the stamp: the row is already stamped,
    // and an older binary must not fail the register.
    if delivery_policy.is_some() {
        let clock_key = session_identity_key(&session_id);
        if run_mail_hold(&["--clear", &clock_key]) != 0 {
            eprintln!("register failed: fno-agents mail-hold failed");
            return 1;
        }
        if policy_after.as_deref() != Some("bus-only") {
            if run_mail_hold(&["--clear", &name]) != 0 {
                eprintln!("register failed: fno-agents mail-hold failed");
                return 1;
            }
        } else {
            let _ = run_mail_hold(&["--record-stamp", &session_id]);
        }
    }

    crate::events::EventEmitter::new(home.events_jsonl(), "rust")
        .emit_fields(
            "session_registered",
            [
                ("provider".to_string(), serde_json::json!(harness)),
                ("name".to_string(), serde_json::json!(name)),
                ("session_id".to_string(), serde_json::json!(session_id)),
                ("cwd".to_string(), serde_json::json!(cwd)),
            ]
            .into_iter()
            .collect(),
        )
        .ok();

    let policy_json = match &policy_after {
        Some(p) => format!("\"{p}\""),
        None => "null".to_string(),
    };
    if json_out || !std::io::stdout().is_terminal() {
        println!(
            "{{\"registered\": true, \"name\": \"{name}\", \"harness\": \"{harness}\", \
\"delivery_policy\": {policy_json}}}"
        );
    } else {
        let policy_note = if policy_after.as_deref() == Some("bus-only") {
            " [bus-only: mail to this session queues durable, never injects]"
        } else {
            ""
        };
        println!(
            "joined the mesh as {name}{policy_note} - peers can now reach you with \
`fno agents mail send {name} \"...\"`"
        );
    }
    0
}

fn run_mail_hold(argv: &[&str]) -> i32 {
    let owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    crate::mail_hold::run_mail_hold(&owned)
}

/// The lead manifest naming `session_id` as holder, for the role re-stamp a
/// register fills into a bare row (a restore strips the stamp; the manifest
/// is the durable record). Reads `<space>/leads/*.md` frontmatter.
fn manifest_role_for_session(session_id: &str, cwd: &str) -> Option<ManifestRole> {
    let space = crate::paths::space_dir_opt(Path::new(cwd))?;
    let leads = space.join("leads");
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&leads)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();
    for path in paths {
        if let Some(manifest) = parse_manifest(&path) {
            if manifest
                .get("harness_session_id")
                .map(|v| v.trim() == session_id)
                .unwrap_or(false)
            {
                let role_level = manifest
                    .get("role_level")
                    .and_then(|v| v.trim().parse::<u32>().ok());
                return Some(ManifestRole {
                    role_level,
                    role_scope: path.file_stem()?.to_string_lossy().to_string(),
                    role_grantor: manifest.get("role_grantor").and_then(|v| {
                        let v = v.trim();
                        (!v.is_empty()).then(|| v.to_string())
                    }),
                });
            }
        }
    }
    None
}

struct ManifestRole {
    role_level: Option<u32>,
    role_scope: String,
    role_grantor: Option<String>,
}

/// Minimal frontmatter read: the `key: value` lines between the first two
/// `---` fences. The lead manifests are flat, so a flat parse is lossless
/// for the three keys this reads.
fn parse_manifest(path: &Path) -> Option<std::collections::BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut fields = std::collections::BTreeMap::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Some(fields)
}

fn apply_manifest_role(row: &mut RegistryEntry, role: Option<&ManifestRole>) {
    let Some(role) = role else { return };
    if row.role_scope.is_none() {
        row.role_level = role.role_level;
        row.role_scope = Some(role.role_scope.clone());
        row.role_grantor = role.role_grantor.clone();
    }
}

/// The ambiguity refusal, ported from `identity_ambiguity_message`: the
/// opening names what blocked (a live-row-owned id vs mixed families), the
/// marker list names what was seen, the resolve hint names where to look,
/// and the strip section names the self-rescue.
fn ambiguity_message(ident: &crate::spawn_context::OwnedIdentity) -> String {
    let markers = ident
        .markers_present
        .iter()
        .map(|(marker, _harness, _value)| marker.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let lookup_id = ident
        .markers_present
        .first()
        .map(|(_m, _h, value)| value.clone())
        .unwrap_or_else(|| "<session-id>".to_string());
    let opening = match ident.rejected.first() {
        Some(rejection) => format!(
            "cannot decide which session is 'self': id {} is owned by live row {}",
            rejection.session_id,
            rejection.owner.as_deref().unwrap_or("")
        ),
        None => "cannot decide which session is 'self': multiple harness markers present \
(inherited env?)"
            .to_string(),
    };
    format!(
        "{opening}\nmarkers: {markers}\nresolve with: find ~/.codex/sessions \
~/.claude/projects -name '*{lookup_id}*'"
    )
}

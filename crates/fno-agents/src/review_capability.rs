//! Can this session's configured reviewers actually run, here (node)? The
//! native port of `cli/src/fno/review_capability.py`, reached through the
//! `review-level capability` group under the existing review-level door.
//!
//! `config.review.reviewers` names a gate that only a head-pinned
//! `review_attestation` clears. Whether anything in THIS session can produce
//! that attestation depends on the harness and the substrate, not on the
//! reviewer's name. On PR #618 nothing checked: a `reviewers: [sigma]` gate
//! was handed to a session that would not dispatch subagents, so the gate was
//! unsatisfiable from the first turn and said so only at the stop gate,
//! fifteen turns later.
//!
//! Every refusal, verdict and reason string is byte-for-byte the Python
//! module's, asserted against goldens captured from the live leg
//! (`tests/review_capability_parity.rs`) before the leg deletes. The built-in
//! reviewer descriptors stay single-sourced in Python config
//! (`_RESOLVABLE_REVIEWERS`) and ride the door payload; the Rust
//! `reviewer_entry` table feeds only the invocation renderer's base spelling.
//! It never selects a reviewer for you, and never substitutes `declare` for
//! an unavailable one.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::claude_ask::py_repr;

/// The statuses a verdict can carry. A plain string, not an enum, on purpose:
/// `blocks_autonomy` is the fail-closed default ("not in the non-blocking
/// set"), so a status added later blocks until someone deliberately clears
/// it. An enum would make the compiler the one who refuses to compile a new
/// status into the blocking set - the wrong default for a gate whose whole
/// point is fail-closed.
type Status = &'static str;

/// The allowlist half of the fail-closed default; see `blocks_autonomy`.
fn blocks_autonomy(status: &str) -> bool {
    !matches!(status, "satisfiable" | "unverifiable")
}

/// Python `sorted(per_harness)` rendered as a list repr: `['a', 'b']`.
fn py_string_list(mut items: Vec<&str>) -> String {
    items.sort_unstable();
    let inner: Vec<String> = items.iter().map(|s| py_repr(s)).collect();
    format!("[{}]", inner.join(", "))
}

// ── session detection ─────────────────────────────────────────────────

/// What this session can do, as far as the environment will admit.
pub(crate) struct SessionCapability {
    harness: String,
    substrate: String,
    attended: bool,
    provider: String,
}

impl SessionCapability {
    /// `SessionCapability.describe()`: the one display line refusal messages
    /// embed.
    fn describe(&self) -> String {
        format!(
            "harness={} substrate={} provider={}",
            self.harness, self.substrate, self.provider
        )
    }

    /// The door's session object, Python dict key order.
    fn json(&self) -> String {
        format!(
            "{{\"harness\": {}, \"substrate\": {}, \"attended\": {}, \"provider\": {}, \"describe\": {}}}",
            esc(&self.harness),
            esc(&self.substrate),
            lit(self.attended),
            esc(&self.provider),
            esc(&self.describe()),
        )
    }
}

/// `config.unattended.enabled` - the manifest's second `attended` input.
/// The settings model is `extra: ignore`, so this key never survives a
/// settings load; reading the candidate files directly is the only way to
/// see what `hooks/helpers/init-target-state.sh` sees. Per USABLE VALUE,
/// not per block or per key: a candidate whose value is null or empty is
/// skipped and the walk continues, so stopping at the first file that
/// merely MENTIONS the key would answer differently from both authorities
/// this claims parity with.
fn unattended_in_config() -> bool {
    for doc in crate::backlog::settings::config_candidates() {
        let Some(block) = doc.get("unattended") else {
            continue;
        };
        let Some(enabled) = block.get("enabled") else {
            continue;
        };
        if enabled.is_null() || enabled.as_str() == Some("") {
            continue;
        }
        return crate::backlog::birth::truthy_value(enabled);
    }
    false
}

/// Read harness + substrate from the ambient environment. `attended` must
/// match the manifest's own derivation (`TARGET_UNATTENDED=1` OR
/// `config.unattended.enabled`) or the check clears a gate the run cannot
/// satisfy. The env markers are additive: a bg or spawned session is also
/// unattended, and neither the shell nor this function may be the only one
/// to know it.
pub(crate) fn detect_session() -> SessionCapability {
    let env = |name: &str| std::env::var(name).ok();
    let harness = crate::spawn_context::resolve_self_identity(
        &env,
        None,
        None,
        &crate::paths::AgentsHome::from_env(),
    )
    .harness
    .filter(|h| !h.is_empty())
    .unwrap_or_else(|| "unknown".to_string());
    let env_unattended = crate::backlog::birth::env_marks_unattended(&env);
    let unattended_configured = unattended_in_config();
    let attended = !(env_unattended || unattended_configured);
    let stamped = env("FNO_AGENT_SUBSTRATE")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let substrate = match stamped {
        Some(s) => s,
        // Spawned, or declared unattended, by a path that stamps no
        // substrate. Naming a guess here is what labelled every headless
        // one-shot a `pane`.
        None if env_unattended => "unknown".to_string(),
        None => "interactive".to_string(),
    };
    // The stamp a bound route writes at spawn. Read verbatim and never
    // inferred from an endpoint or a model name: the point is to carry the
    // decision fno already made, not to make a second one.
    let provider = env("FNO_ROUTE_PROVIDER")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    SessionCapability {
        harness,
        substrate,
        attended,
        provider,
    }
}

// ── reviewer resolution ───────────────────────────────────────────────

/// What each rung is allowed to claim, printed wherever a reviewer is
/// reported. `review-evidence` gets no note: it is the ordinary case. The
/// two weaker rungs are annotated so an operator learns what is behind
/// their green checkmark here, rather than at the stop gate.
fn rung_note(asserts: Option<&str>) -> &'static str {
    match asserts {
        Some("self-cert") => "  [self-cert: satisfies the gate, asserts no review evidence]",
        Some("invocation") => {
            "  [invocation: proves the reviewer ran at the reviewed commit, \
             asserts nothing about its verdict]"
        }
        _ => "",
    }
}

/// One configured reviewer, resolved against one session. `desc` is None
/// for a name absent from the payload table: borrowing another reviewer's
/// descriptor would render an unknown name with that reviewer's kind,
/// invocation, and self-cert note.
struct ReviewerVerdict {
    name: String,
    status: Status,
    reason: String,
    kind: Option<String>,
    requires: Option<String>,
    asserts: Option<String>,
    invocation: Option<String>,
}

impl ReviewerVerdict {
    /// One report line. A rung weaker than review-evidence always says so.
    fn line(&self) -> String {
        let note = rung_note(self.asserts.as_deref());
        format!("{}: {} - {}{}", self.status, self.name, self.reason, note)
    }

    fn row_json(&self) -> String {
        format!(
            "{{\"name\": {}, \"status\": {}, \"reason\": {}, \"line\": {}, \"blocks_autonomy\": {}, \"kind\": {}, \"requires\": {}, \"asserts\": {}, \"invocation\": {}}}",
            esc(&self.name),
            esc(self.status),
            esc(&self.reason),
            esc(&self.line()),
            lit(blocks_autonomy(self.status)),
            opt_esc(self.kind.as_deref()),
            opt_esc(self.requires.as_deref()),
            opt_esc(self.asserts.as_deref()),
            opt_esc(self.invocation.as_deref()),
        )
    }
}

/// The descriptor fields the resolver reads, off the door payload's
/// `descriptors` map (the caller serializes `resolvable_reviewers(registry)`).
struct Descriptor {
    kind: Option<String>,
    requires: Option<String>,
    invocation: Option<String>,
    asserts: Option<String>,
    invocations: Option<Map<String, Value>>,
}

fn descriptor_from(row: &Value) -> Descriptor {
    let text = |key: &str| -> Option<String> {
        row.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    Descriptor {
        kind: text("kind"),
        requires: text("requires"),
        invocation: text("invocation"),
        asserts: text("asserts"),
        invocations: row
            .get("invocations")
            .and_then(Value::as_object)
            .filter(|m| !m.is_empty())
            .cloned(),
    }
}

/// The skill name to resolve, derived from the descriptor's INVOCATION, not
/// the registry key. Falls back to the key for an invocation with no
/// leading token, so a malformed descriptor degrades to the old behavior
/// rather than probing "".
fn skill_id(descriptor: &Descriptor, name: &str) -> String {
    let invocation = descriptor.invocation.as_deref().unwrap_or("");
    let head = invocation.trim().split_whitespace().next().unwrap_or("");
    if head.is_empty() || head.trim_matches('/').is_empty() {
        return name.to_string();
    }
    head.trim_start_matches('/').to_string()
}

fn resolve_one(
    name: &str,
    descriptor: Option<&Descriptor>,
    session: &SessionCapability,
) -> ReviewerVerdict {
    let Some(d) = descriptor else {
        // The config validator already rejects these, so this is only
        // reachable when a caller hands in an unvalidated list.
        return ReviewerVerdict {
            name: name.to_string(),
            status: "unavailable",
            reason: "unknown reviewer; not in the descriptor table".to_string(),
            kind: None,
            requires: None,
            asserts: None,
            invocation: None,
        };
    };
    let verdict = |status: Status, reason: String| ReviewerVerdict {
        name: name.to_string(),
        status,
        reason,
        kind: d.kind.clone(),
        requires: d.requires.clone(),
        asserts: d.asserts.clone(),
        invocation: d.invocation.clone(),
    };
    let invocation = d.invocation.as_deref().unwrap_or("");
    let requires = d.requires.as_deref().unwrap_or("");
    if requires == "retired" {
        // One-way state, checked FIRST so nothing downstream can rescue it.
        // The invocation field of a retired descriptor names the REPLACEMENT,
        // never a hint that the reviewer runs.
        return verdict(
            "unavailable",
            format!(
                "{name} is retired; run `{invocation}` (the default review \
                 lane) or change config.review.reviewers"
            ),
        );
    }
    if requires == "none" {
        // A self-serve verb whose `invocations` map scopes it to specific
        // harnesses draws the same line the subagent-dispatch branch draws
        // below: a known harness outside the map has no verb, so resolving
        // it satisfiable would floor the stop gate onto a reviewer whose
        // attestation nothing there produces. The invocations map IS the
        // allowlist; the scalar `invocation` is the unknown-harness default.
        if let Some(per_harness) = &d.invocations {
            if let Some(verb) = per_harness.get(&session.harness).and_then(Value::as_str) {
                let mut reason = format!("run `{verb}`");
                // Capability resolution runs before a diff exists, so a verb
                // carrying a <level> placeholder teaches the sizing rule
                // rather than baking one concrete level into the receipt.
                if verb.contains("<level>") {
                    reason += " (size <level> from the diff)";
                }
                return verdict("satisfiable", reason);
            }
            if session.harness == "unknown" {
                return verdict(
                    "unverifiable",
                    format!(
                        "needs a {name} verb; no harness marker in this \
                         environment, so capability cannot be verified. \
                         Proceeding - if the gate does go unmet, run \
                         `{invocation}`"
                    ),
                );
            }
            let keys: Vec<&str> = per_harness.keys().map(String::as_str).collect();
            return verdict(
                "unavailable",
                format!(
                    "no {name} verb for {}; scoped to {}",
                    session.describe(),
                    py_string_list(keys)
                ),
            );
        }
        return verdict("satisfiable", format!("run `{invocation}`"));
    }
    if requires == "operator" {
        if session.attended {
            return verdict(
                "satisfiable",
                format!(
                    "ask {} to run `{invocation}`",
                    crate::backlog::birth::display_name(
                        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                    )
                ),
            );
        }
        return verdict(
            "needs-operator",
            format!(
                "kind={}, never autonomously satisfiable; this run is \
                 unattended ({}). Not a misconfiguration - run attended, or \
                 configure a reviewer this session can drive",
                d.kind.as_deref().unwrap_or(""),
                session.describe()
            ),
        );
    }
    if requires == "skill" {
        let skill = skill_id(d, name);
        let (status, reason) = crate::backlog::dispatch_resolve::skill_presence(
            &skill,
            &session.harness,
            "config.review.reviewers",
        );
        return verdict(status, reason);
    }
    if requires == "subagent-dispatch" {
        if session.harness == "claude" || session.harness == "codex" {
            return verdict("satisfiable", format!("run `{invocation}`"));
        }
        if session.harness == "unknown" {
            return verdict(
                "unverifiable",
                format!(
                    "needs subagent-dispatch; no harness marker in this \
                     environment, so capability cannot be verified. \
                     Proceeding - if the gate does go unmet, run `{invocation}`"
                ),
            );
        }
        return verdict(
            "unavailable",
            format!(
                "needs subagent-dispatch, unavailable on {}",
                session.describe()
            ),
        );
    }
    verdict(
        "unavailable",
        format!("declares an unknown capability {}", py_repr(requires)),
    )
}

/// Resolve every configured reviewer against this session. Read-only; the
/// session is detected once by the caller and the descriptor table rides
/// the payload, so two sessions on one repo resolve their own harness and
/// substrate.
fn resolve_reviewers(
    reviewers: &[Value],
    descriptors: &Map<String, Value>,
    session: &SessionCapability,
) -> Vec<ReviewerVerdict> {
    let mut out = Vec::new();
    for entry in reviewers {
        let Some(raw) = entry.as_str() else {
            continue;
        };
        let name = raw.trim().trim_start_matches('/');
        let descriptor = descriptors.get(name).map(descriptor_from);
        out.push(resolve_one(name, descriptor.as_ref(), session));
    }
    out
}

/// The init refusal, or None when every reviewer can run here. Names the
/// reviewer, the missing capability, the harness and substrate, and both
/// ways out. Never proposes `declare` as the fix.
fn reviewers_refusal_message(
    verdicts: &[ReviewerVerdict],
    session: &SessionCapability,
) -> Option<String> {
    let blocked: Vec<&ReviewerVerdict> = verdicts
        .iter()
        .filter(|v| blocks_autonomy(v.status))
        .collect();
    if blocked.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "fno do target init: config.review.reviewers cannot be satisfied on {}.",
        session.describe()
    )];
    lines.extend(blocked.iter().map(|v| format!("  {}", v.line())));
    // Per-status, because attendedness is irrelevant to `unavailable`, and
    // per-CAPABILITY, because a remedy that cannot work is the failure
    // class this check exists to delete.
    let mut remedies = vec!["change config.review.reviewers".to_string()];
    if blocked.iter().any(|v| v.status == "needs-operator") {
        remedies.push("run attended so an operator can drive it".to_string());
    }
    let unavailable_requires: BTreeSet<&str> = blocked
        .iter()
        .filter(|v| v.status == "unavailable")
        .filter_map(|v| v.requires.as_deref())
        .collect();
    if unavailable_requires.contains("skill") {
        remedies.push("install the named skill in one of the roots listed above".to_string());
    }
    if blocked.iter().any(|v| v.status == "unavailable")
        && unavailable_requires != BTreeSet::from(["skill"])
    {
        remedies.push(
            "run on a harness that dispatches subagents, or run the review by \
             hand and attest with `bash skills/review/scripts/emit-attestation.sh \
             <reviewer>`"
                .to_string(),
        );
    }
    lines.push(String::new());
    lines.push(
        "The gate is fail-closed: without a head-pinned review_attestation the \
         session will block at the stop gate after the work is done."
            .to_string(),
    );
    lines.push(format!("To proceed: {}.", remedies.join("; or ")));
    Some(lines.join("\n"))
}

// ── local peers ───────────────────────────────────────────────────────

/// One identity-free peer candidate for the composite local gate.
struct LocalPeerVerdict {
    name: String,
    status: Status,
    reason: String,
}

impl LocalPeerVerdict {
    fn eligible(&self) -> bool {
        self.status == "satisfiable"
    }

    fn line(&self) -> String {
        format!("{}: {} - {}", self.status, self.name, self.reason)
    }

    fn row_json(&self) -> String {
        format!(
            "{{\"name\": {}, \"status\": {}, \"reason\": {}, \"line\": {}, \"eligible\": {}}}",
            esc(&self.name),
            esc(self.status),
            esc(&self.reason),
            esc(&self.line()),
            lit(self.eligible()),
        )
    }
}

/// Python truthiness for a JSON value (only `identity` reads this way: an
/// absent, null, empty-string or false identity keeps the peer here).
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Resolve the model family a peer actually runs, independent of transport.
/// Only the Claude transport executes an explicit route.
fn model_family(provider: &str, model: Option<&str>) -> String {
    let mut family = provider.trim().to_lowercase();
    let route = model.map(str::trim).unwrap_or("");
    if family == "claude" && route.contains(',') {
        let routed_provider = route.split(',').next().unwrap_or("").trim().to_lowercase();
        if !routed_provider.is_empty() {
            family = routed_provider;
        }
    }
    match family.as_str() {
        "anthropic" => "claude".to_string(),
        "openai" => "codex".to_string(),
        "google" => "gemini".to_string(),
        _ => family,
    }
}

/// Classify identity-free peers for one composite cross-model gate. Entries
/// with a per-peer identity, or all entries when a shared identity is
/// configured, retain the legacy GitHub-posting carrier and are absent here.
fn resolve_local_peers(
    peers: &[Value],
    peer_identity: Option<&str>,
    session: &SessionCapability,
) -> Vec<LocalPeerVerdict> {
    let mut verdicts = Vec::new();
    if peer_identity.is_some_and(|p| !p.is_empty()) {
        return verdicts;
    }
    for entry in peers {
        let (provider, model, identity) = match entry {
            Value::String(s) => (s.trim().to_string(), None, None),
            Value::Object(m) => (
                m.get("provider")
                    .map(|v| match v {
                        Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default()
                    .trim_matches('"')
                    .trim()
                    .to_string(),
                m.get("model").and_then(Value::as_str),
                m.get("identity").filter(|v| py_truthy(v)).cloned(),
            ),
            _ => continue,
        };
        if identity.is_some() {
            continue;
        }
        let route = model.map(str::trim).unwrap_or("");
        let name = if !route.is_empty() && provider.to_lowercase() == "claude" {
            format!("{provider} via {route}")
        } else {
            provider.clone()
        };
        let peer_family = model_family(&provider, model);
        let author_family = model_family(&session.harness, None);
        if session.harness == "unknown" {
            verdicts.push(LocalPeerVerdict {
                name,
                status: "unverifiable",
                reason: "author model is unknown; cross-model eligibility cannot be verified"
                    .to_string(),
            });
        } else if peer_family == author_family {
            verdicts.push(LocalPeerVerdict {
                name,
                status: "unavailable",
                reason: format!("same model family as the author ({author_family})"),
            });
        } else {
            verdicts.push(LocalPeerVerdict {
                name,
                status: "satisfiable",
                reason: format!("cross-model peer for {author_family} author"),
            });
        }
    }
    verdicts
}

/// Return an init refusal only when a configured local peer gate has no
/// option. An unknown author is not proof of an impossible gate: preserve
/// the stop gate as the backstop, matching reviewer capability's
/// unverifiable policy.
fn local_peers_refusal_message(
    verdicts: &[LocalPeerVerdict],
    session: &SessionCapability,
) -> Option<String> {
    if verdicts.is_empty() || verdicts.iter().any(|v| v.eligible()) {
        return None;
    }
    if verdicts.iter().any(|v| v.status == "unverifiable") {
        return None;
    }
    let mut lines = vec![format!(
        "fno do target init: config.review.peers has no eligible cross-model \
         local peer on {}.",
        session.describe()
    )];
    lines.extend(verdicts.iter().map(|v| format!("  {}", v.line())));
    lines.push(String::new());
    lines.push(
        "The gate is fail-closed: configure at least one peer from a different \
         model family, or remove config.review.peers."
            .to_string(),
    );
    Some(lines.join("\n"))
}

// ── self-review invocation ────────────────────────────────────────────

/// The effort levels an autonomous agent may issue. `ultra` is billed
/// separately, so it is absent from the tuple, which makes
/// `self_review_invocation` raise on it.
const ALLOWED_REVIEW_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The recommended self-review invocation: the fno lane, every harness. The
/// base is the parity-checked `code-review` row; the spelling renders
/// through the same `normalize_command` every dispatch surface uses, a
/// harness whose surface refuses the rewrite getting the claude spelling
/// (advisory text, never a gate). `--fix` is never appended; `--comment`
/// rides by default and drops for the pre-push branch form.
pub(crate) fn self_review_invocation(
    harness: Option<&str>,
    level: Option<&str>,
    comment: bool,
) -> Result<String, String> {
    if let Some(level) = level {
        if !ALLOWED_REVIEW_LEVELS.contains(&level) {
            return Err(format!(
                "review level {} is not one of ('low', 'medium', 'high', \
                 'xhigh', 'max'); ultra is billed separately and no \
                 autonomous surface may issue it",
                py_repr(level)
            ));
        }
    }
    let base = crate::loopcheck::reviewer_entry("code-review")
        .map(|(inv, _, _)| inv)
        .unwrap_or("/fno:review");
    let spelled =
        crate::backlog::dispatch_resolve::normalize_command(base, harness.unwrap_or("claude"))
            .unwrap_or_else(|_| base.to_string());
    let flag = if comment { " --comment" } else { "" };
    match level {
        None => Ok(format!("{spelled} <level>{flag}")),
        Some(level) => Ok(format!("{spelled} {level}{flag}")),
    }
}

/// Python `re.fullmatch(r"[0-9a-fA-F]{7,64}", sha)`.
fn is_commit_sha(sha: &str) -> bool {
    (7..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// Render the inline fno review request, pinned to one PR head or local
/// branch. Sizing always goes through the in-process diff-sized level,
/// never a hardcoded one; with no measurable diff the level is None and the
/// `<level>` placeholder stays. Two explicit targets exist and are mutually
/// exclusive: a PR (all of pr_number, head_sha, and base_branch required)
/// or a local branch (branch, head_sha, and base_branch required). A
/// validation failure is `Err` with the exact message when any explicit
/// target was given, and the unsized placeholder form otherwise. Returns
/// the resolved harness beside the render for the door's answer.
pub(crate) fn render_self_review_invocation(
    harness: Option<&str>,
    project_root: Option<&Path>,
    pr_number: Option<i64>,
    branch: Option<&str>,
    head_sha: Option<&str>,
    base_branch: Option<&str>,
) -> (Result<String, String>, String) {
    let h = harness
        .filter(|h| !h.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| detect_session().harness);
    // Advisory only; never raises. Only a diff-sized answer counts.
    let level = project_root
        .and_then(crate::review_level::diff_review_level)
        .map(str::to_string);
    let has_target =
        pr_number.is_some() || branch.is_some() || head_sha.is_some() || base_branch.is_some();
    let build = || -> Result<String, String> {
        if branch.is_some() && pr_number.is_some() {
            return Err(
                "explicit self-review target takes pr_number or branch, not both".to_string(),
            );
        }
        // A branch target has no PR for --comment to write to, so the
        // pre-push payload never carries it.
        let with_comment = branch.is_none();
        let mut rendered = self_review_invocation(Some(&h), level.as_deref(), with_comment)?;
        if has_target {
            if branch.is_none()
                && !(pr_number.is_some() && head_sha.is_some() && base_branch.is_some())
            {
                return Err("explicit self-review target requires pr_number, head_sha, \
                     and base_branch"
                    .to_string());
            }
            if branch.is_some() && (head_sha.is_none() || base_branch.is_none()) {
                return Err("explicit self-review target requires branch, head_sha, \
                     and base_branch"
                    .to_string());
            }
            let sha = head_sha.unwrap_or("");
            if !is_commit_sha(sha) {
                return Err("explicit self-review target requires a commit SHA".to_string());
            }
            let mut base = base_branch.unwrap_or("").trim().to_string();
            if let Some(stripped) = base.strip_prefix("origin/") {
                base = stripped.to_string();
            }
            if base.is_empty() || base.chars().any(char::is_whitespace) || base.starts_with('-') {
                return Err("explicit self-review target requires a branch name".to_string());
            }
            // Target slot first: the bare PR number or branch name is the
            // first non-flag token after the level, so a strict reader
            // resolves the target to it. HEAD and base ride behind it as
            // trailing context.
            match branch {
                Some(raw_branch) => {
                    let name = raw_branch.trim();
                    if name.is_empty()
                        || name.chars().any(char::is_whitespace)
                        || name.starts_with('-')
                    {
                        return Err(
                            "explicit self-review target requires a branch name".to_string()
                        );
                    }
                    rendered = format!("{rendered} {name} HEAD {sha} against origin/{base}");
                }
                None => {
                    let number = pr_number.unwrap_or(0);
                    if number <= 0 {
                        return Err(
                            "explicit self-review target requires a positive PR number".to_string()
                        );
                    }
                    rendered = format!("{rendered} {number} HEAD {sha} against origin/{base}");
                }
            }
        }
        Ok(rendered)
    };
    let result = match build() {
        Ok(rendered) => Ok(rendered),
        // Advisory text; the gate stays the gate. An explicit target that
        // fails validation re-raises instead of degrading to a placeholder.
        Err(err) => {
            if has_target {
                Err(err)
            } else {
                self_review_invocation(harness, None, true)
            }
        }
    };
    (result, h)
}

// ── github_apps axis ──────────────────────────────────────────────────

/// Whether a CONFIGURED `github_apps` login corresponds to a known review
/// App. Directional on purpose: the login is recognized only when it is a
/// substring of a known login (an exact login or an intentional short alias
/// like "codex"), NOT when a known login is a substring of it, so a
/// suffixed typo can never be waved through.
fn app_is_recognized(login: &str) -> bool {
    let lo = login.to_lowercase();
    crate::loopcheck::bot_profile_logins().any(|k| lo.contains(k))
}

/// True if `login` authored an issue/PR comment in this repo, False if a
/// clean search returned none, None if the probe could not run. Fail-open
/// by contract: None on ANY doubt so init never refuses on a probe it could
/// not run. A clean zero is the only False, and only a login footnote does
/// not recognize ever reaches this probe.
fn app_ever_acted(login: &str, cwd: Option<&Path>) -> Option<bool> {
    let dir = cwd
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let run = |args: &[&str]| -> Option<String> {
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let out =
            crate::backlog::advance::bounded_command_env(Path::new("gh"), &argv, 30, &[]).ok()?;
        if out.code != 0 {
            return None;
        }
        Some(out.stdout)
    };
    let slug = run(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ])?;
    let slug = slug.trim();
    if slug.is_empty() {
        return None;
    }
    // A `commenter:` login with no account answers 422, which lands here as
    // None and proceeds rather than refusing: the likeliest typo shape stays
    // unverifiable, because a configured entry is matched by substring, so a
    // short alias is a SUPPORTED config and is not an account of its own.
    let total = run(&[
        "api",
        "-X",
        "GET",
        "search/issues",
        "-f",
        &format!("q=repo:{slug} commenter:{login}"),
        "--jq",
        ".total_count",
    ])?;
    let count = total.trim().parse::<i64>().ok()?;
    Some(count > 0)
}

/// One configured `github_apps` login, resolved against this repository.
struct AppVerdict {
    login: String,
    status: Status,
    reason: String,
}

impl AppVerdict {
    fn line(&self) -> String {
        format!("{}: {} - {}", self.status, self.login, self.reason)
    }

    fn row_json(&self) -> String {
        format!(
            "{{\"login\": {}, \"status\": {}, \"reason\": {}, \"line\": {}, \"blocks_autonomy\": {}}}",
            esc(&self.login),
            esc(self.status),
            esc(&self.reason),
            esc(&self.line()),
            lit(blocks_autonomy(self.status)),
        )
    }
}

/// Resolve every `github_apps` login against this repo. Read-only. A
/// recognized login is `satisfiable` with no probe. An unrecognized login is
/// probed: `satisfiable` if it has acted here, `unavailable` (a likely
/// typo) if a clean search found nothing, and `unverifiable` if the probe
/// could not run. `nudge_logins` are the keys of `config.review.nudge`: an
/// operator who wrote an explicit nudge profile for an App has named a real
/// bot, not a typo.
fn resolve_github_apps(
    apps: &[Value],
    nudge_logins: &[Value],
    cwd: Option<&Path>,
) -> Vec<AppVerdict> {
    let nudge: Vec<String> = nudge_logins
        .iter()
        .filter_map(Value::as_str)
        .map(|n| n.trim().to_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    let mut out = Vec::new();
    for entry in apps {
        let Some(raw) = entry.as_str() else {
            continue;
        };
        let login = raw.trim();
        if login.is_empty() {
            continue;
        }
        // Same directional rule as `app_is_recognized`: the configured login
        // must be a substring of a nudge key, never the reverse.
        let lo = login.to_lowercase();
        if app_is_recognized(login) || nudge.iter().any(|nk| lo.contains(nk.as_str())) {
            out.push(AppVerdict {
                login: login.to_string(),
                status: "satisfiable",
                reason: "recognized review App; footnote knows how to reach it".to_string(),
            });
            continue;
        }
        match app_ever_acted(login, cwd) {
            Some(true) => out.push(AppVerdict {
                login: login.to_string(),
                status: "satisfiable",
                reason: "has reviewed or commented in this repo".to_string(),
            }),
            Some(false) => out.push(AppVerdict {
                login: login.to_string(),
                status: "unavailable",
                reason: "never reviewed or commented in this repository - likely a typo, \
                         or a GitHub App not installed here"
                    .to_string(),
            }),
            None => out.push(AppVerdict {
                login: login.to_string(),
                status: "unverifiable",
                reason: "could not check whether this App has acted here (no repo \
                         remote, token scope, rate limit, or a login GitHub does \
                         not resolve as an account); proceeding"
                    .to_string(),
            }),
        }
    }
    out
}

/// The init refusal for an unreachable `github_apps` login, or None. Names
/// the login, why it cannot review, and the two ways out. Never widens or
/// drops the gate on its own.
fn github_apps_refusal_message(verdicts: &[AppVerdict]) -> Option<String> {
    let blocked: Vec<&AppVerdict> = verdicts
        .iter()
        .filter(|v| blocks_autonomy(v.status))
        .collect();
    if blocked.is_empty() {
        return None;
    }
    let mut lines = vec![
        "fno do target init: config.review.github_apps names a bot that cannot review here."
            .to_string(),
    ];
    lines.extend(blocked.iter().map(|v| format!("  {}", v.line())));
    lines.push(String::new());
    lines.push(
        "The gate is fail-closed: a required App that never reviews wedges the \
         session until the budget ceiling."
            .to_string(),
    );
    lines.push(
        "To proceed: fix the login in config.review.github_apps; or move it to \
         config.review.optional_apps (honored-if-present, never waited on)."
            .to_string(),
    );
    Some(lines.join("\n"))
}

// ── serialization helpers (Python json.dumps default separators) ──────

/// One JSON string literal escaped the way serde (and json.dumps
/// ensure_ascii=False) escape.
fn esc(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn opt_esc(s: Option<&str>) -> String {
    match s {
        Some(v) => esc(v),
        None => "null".to_string(),
    }
}

fn lit(v: bool) -> &'static str {
    if v {
        "true"
    } else {
        "false"
    }
}

// ── the door ──────────────────────────────────────────────────────────

const USAGE: &str =
    "usage: fno-agents review-level capability <reviewers|github-apps|invocation|skill-presence>";

fn read_stdin_json() -> Result<Value, String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| format!("stdin unreadable: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("stdin is not one JSON object: {e}"))
}

fn action_reviewers(payload: &Value) -> (String, i32) {
    let session = detect_session();
    let empty = Map::new();
    let reviewers = payload
        .get("reviewers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let descriptors = payload
        .get("descriptors")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let peers = payload
        .get("peers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let peer_identity = payload.get("peer_identity").and_then(Value::as_str);
    let verdicts = resolve_reviewers(&reviewers, descriptors, &session);
    let peer_verdicts = resolve_local_peers(&peers, peer_identity, &session);
    let reviewers_refusal = reviewers_refusal_message(&verdicts, &session);
    let peers_refusal = local_peers_refusal_message(&peer_verdicts, &session);
    let reviewer_rows: Vec<String> = verdicts.iter().map(|v| v.row_json()).collect();
    let peer_rows: Vec<String> = peer_verdicts.iter().map(|v| v.row_json()).collect();
    let out = format!(
        "{{\"session\": {}, \"reviewers\": [{}], \"local_peers\": [{}], \
         \"reviewers_refusal\": {}, \"local_peers_refusal\": {}}}",
        session.json(),
        reviewer_rows.join(", "),
        peer_rows.join(", "),
        match &reviewers_refusal {
            Some(m) => esc(m),
            None => "null".to_string(),
        },
        match &peers_refusal {
            Some(m) => esc(m),
            None => "null".to_string(),
        },
    );
    (out, 0)
}

fn action_github_apps(payload: &Value) -> (String, i32) {
    let apps = payload
        .get("apps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let nudge = payload
        .get("nudge_logins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let verdicts = resolve_github_apps(&apps, &nudge, None);
    let refusal = github_apps_refusal_message(&verdicts);
    let rows: Vec<String> = verdicts.iter().map(|v| v.row_json()).collect();
    let out = format!(
        "{{\"verdicts\": [{}], \"refusal\": {}}}",
        rows.join(", "),
        match &refusal {
            Some(m) => esc(m),
            None => "null".to_string(),
        },
    );
    (out, 0)
}

/// `capability invocation [--harness H] [--root DIR] [--pr-number N |
/// --branch B] [--head-sha S] [--base-branch B]`: no stdin is read. A
/// target that fails validation exits 2 with the Python message on stderr.
fn action_invocation(args: &[String]) -> (String, String, i32) {
    let mut harness: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    let mut pr_number: Option<i64> = None;
    let mut branch: Option<String> = None;
    let mut head_sha: Option<String> = None;
    let mut base_branch: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--harness" => harness = it.next().cloned(),
            "--root" => root = it.next().map(PathBuf::from),
            "--pr-number" => match it.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(n) => pr_number = Some(n),
                None => {
                    return (
                        String::new(),
                        "--pr-number needs an integer\n".to_string(),
                        2,
                    );
                }
            },
            "--branch" => branch = it.next().cloned(),
            "--head-sha" => head_sha = it.next().cloned(),
            "--base-branch" => base_branch = it.next().cloned(),
            other => {
                return (String::new(), format!("unknown flag: {other}\n"), 2);
            }
        }
    }
    let (result, h) = render_self_review_invocation(
        harness.as_deref(),
        root.as_deref(),
        pr_number,
        branch.as_deref(),
        head_sha.as_deref(),
        base_branch.as_deref(),
    );
    match result {
        Ok(rendered) => (
            format!(
                "{{\"invocation\": {}, \"harness\": {}}}",
                esc(&rendered),
                esc(&h)
            ),
            String::new(),
            0,
        ),
        Err(msg) => (String::new(), format!("{msg}\n"), 2),
    }
}

fn action_skill_presence(args: &[String]) -> (String, String, i32) {
    let mut name: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut context = "config.review.reviewers".to_string();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--name" => name = it.next().cloned(),
            "--harness" => harness = it.next().cloned(),
            "--context" => context = it.next().cloned().unwrap_or(context),
            other => {
                return (String::new(), format!("unknown flag: {other}\n"), 2);
            }
        }
    }
    let (Some(name), Some(harness)) = (name, harness) else {
        return (
            String::new(),
            "usage: capability skill-presence --name N --harness H [--context C]\n".to_string(),
            2,
        );
    };
    let (status, reason) =
        crate::backlog::dispatch_resolve::skill_presence(&name, &harness, &context);
    (
        format!(
            "{{\"status\": {}, \"reason\": {}}}",
            esc(status),
            esc(&reason)
        ),
        String::new(),
        0,
    )
}

/// The `review-level capability <action>` door: one JSON object on stdout,
/// exit 0 unless the action refuses. Routed by `review_doors::dispatch`
/// before the level parser, so no valid level call changes.
pub fn run(args: &[String]) -> i32 {
    let Some(action) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return 2;
    };
    match action {
        "reviewers" | "github-apps" => {
            let payload = match read_stdin_json() {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{action}: {e}");
                    return 2;
                }
            };
            let (out, code) = if action == "reviewers" {
                action_reviewers(&payload)
            } else {
                action_github_apps(&payload)
            };
            println!("{out}");
            code
        }
        "invocation" => {
            let (out, err, code) = action_invocation(&args[1..]);
            if !err.is_empty() {
                eprint!("{err}");
            }
            if !out.is_empty() {
                println!("{out}");
            }
            code
        }
        "skill-presence" => {
            let (out, err, code) = action_skill_presence(&args[1..]);
            if !err.is_empty() {
                eprint!("{err}");
            }
            if !out.is_empty() {
                println!("{out}");
            }
            code
        }
        other => {
            eprintln!("{USAGE} (unknown action: {other})");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fail-closed default a golden cannot show: a status the resolver
    /// does not know blocks, because the allowlist is the enumerated half.
    #[test]
    fn unknown_status_blocks_autonomy() {
        assert!(blocks_autonomy("needs-human-eyes"));
        assert!(blocks_autonomy("retired-lane"));
        assert!(!blocks_autonomy("satisfiable"));
        assert!(!blocks_autonomy("unverifiable"));
        assert!(blocks_autonomy("unavailable"));
        assert!(blocks_autonomy("needs-operator"));
    }

    /// AC3-EDGE, dispatch side: a codex dispatch probing a `requires =
    /// "skill"` verb answers unverifiable (admitted), never unavailable
    /// (refused) - the dual this port collapsed.
    #[test]
    fn codex_dispatch_skill_probe_answers_unverifiable() {
        let (status, reason) = crate::backlog::dispatch_resolve::skill_presence(
            "code-review",
            "codex",
            "config.dispatch.verb_registry",
        );
        assert_eq!(status, "unverifiable");
        assert!(
            reason.contains("harness=codex"),
            "reason must name the harness, got: {reason}"
        );
    }

    /// AC3-EDGE, claude side: a searched-and-absent skill refuses, naming
    /// the roots searched.
    #[test]
    fn claude_dispatch_absent_skill_refuses() {
        let (status, reason) = crate::backlog::dispatch_resolve::skill_presence(
            "no-such-skill-anywhere",
            "claude",
            "config.dispatch.verb_registry",
        );
        assert_eq!(status, "unavailable");
        assert!(
            reason.contains("resolves in none of the roots searched"),
            "reason must name the miss, got: {reason}"
        );
    }
}

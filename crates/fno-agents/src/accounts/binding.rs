//! The one effective-account answer every claude launch and usage reader
//! shares: who the credential serving a launch provably belongs to.
//!
//! A port of `binding.py` and the `managed.py` reads it stands on (slot
//! blobs, principal proof, the per-credential proof cache, record principals,
//! the bearer verdict). A path says where a credential belongs and a stamp
//! says who put it there; neither says who the credential presents as, so
//! every answer here is proven against the profile endpoint or a fresh cached
//! proof, and every failure is a typed `unknown`. Nothing here writes a
//! record; the only write is the proof cache beside the slot stamp.

use crate::accounts::records::Record;
use crate::claude_vault::{External, ExternalFailure, Principal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MATCHED: &str = "matched";
pub const MISMATCH: &str = "mismatch";
pub const UNKNOWN: &str = "unknown";
pub const AMBIGUOUS: &str = "ambiguous";

pub const MISMATCH_RECEIPT: &str = "account_identity_mismatch";
pub const UNKNOWN_RECEIPT: &str = "account_identity_unknown";

/// `managed._PRINCIPAL_TTL_S`: how long a cached proof stands.
pub const PRINCIPAL_TTL_S: f64 = 900.0;

const UNSCOPED_SERVICE: &str = "Claude Code-credentials";

/// The two reads a binding needs from outside the process: one Keychain item
/// (`Ok(None)` when absent, `Err` when the read itself failed) and the
/// profile endpoint for one bearer.
pub trait SlotProbe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure>;
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure>;
}

/// The live probe: `security` and the profile endpoint, through the vault's
/// one transport.
pub struct SystemProbe;

impl SlotProbe for SystemProbe {
    fn keychain(&self, service: &str) -> Result<Option<String>, ExternalFailure> {
        crate::claude_vault::SystemExternal.keychain_raw(service)
    }
    fn profile(&self, bearer: &str) -> Result<Principal, ExternalFailure> {
        crate::claude_vault::SystemExternal.profile(bearer)
    }
}

/// Who the credential serving this launch provably belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountBinding {
    pub harness: String,
    pub status: &'static str,
    pub credential_root: Option<PathBuf>,
    pub requested_record: Option<String>,
    pub observed_principal: Option<String>,
    pub observed_label: Option<String>,
    pub matched_record: Option<String>,
    pub observed_at: f64,
    pub reason: Option<String>,
}

impl AccountBinding {
    /// A line safe to print. It never names an account it cannot prove.
    pub fn receipt(&self) -> String {
        let where_ = match &self.credential_root {
            Some(root) => root.display().to_string(),
            None => "the shared ~/.claude slot".to_string(),
        };
        match self.status {
            MATCHED => {
                let served = self
                    .matched_record
                    .as_deref()
                    .or(self.requested_record.as_deref())
                    .unwrap_or("None");
                format!("account_identity_matched: {where_} serves {served}")
            }
            MISMATCH => {
                // A cached proof carries no label, so the record name beats a uuid.
                let who = self
                    .observed_label
                    .as_deref()
                    .or(self.matched_record.as_deref())
                    .or(self.observed_principal.as_deref())
                    .unwrap_or("another account");
                let requested = match &self.requested_record {
                    Some(id) => format!("'{id}'"),
                    None => "None".to_string(),
                };
                format!(
                    "{MISMATCH_RECEIPT}: {requested} is pinned, but {where_} serves {who}; \
                     sign that account in where it belongs, or pin the record whose identity it is"
                )
            }
            _ => format!(
                "{UNKNOWN_RECEIPT}: {where_} could not be attributed ({})",
                self.reason.as_deref().unwrap_or("None")
            ),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "harness": self.harness,
            "status": self.status,
            "credential_root": self.credential_root.as_ref().map(|p| p.display().to_string()),
            "requested_record": self.requested_record,
            "observed_principal": self.observed_principal,
            "observed_label": self.observed_label,
            "matched_record": self.matched_record,
            "observed_at": self.observed_at,
            "reason": self.reason,
            "receipt": self.receipt(),
        })
    }
}

/// The dir whose credential serves `record`; `None` is the shared slot.
pub fn credential_root(record: &Record) -> Option<PathBuf> {
    record
        .config_dir
        .clone()
        .or_else(|| record.credentials_source.clone())
}

/// The scoped Keychain service claude reads for `dir`.
pub fn keychain_service(dir: &Path) -> Option<String> {
    crate::claude_vault::scoped_service(dir).ok()
}

/// `managed._token_present`: a usable credential (access or refresh token);
/// an opaque non-JSON blob counts, a logged-out residue does not.
pub(crate) fn token_present(blob: &str) -> bool {
    let Ok(data) = serde_json::from_str::<Value>(blob) else {
        return !blob.trim().is_empty();
    };
    let Some(map) = data.as_object() else {
        return !blob.trim().is_empty();
    };
    let truthy = |v: Option<&Value>| match v {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Null) | None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => true,
    };
    if let Some(oauth) = map.get("claudeAiOauth").and_then(Value::as_object) {
        if truthy(oauth.get("accessToken")) || truthy(oauth.get("refreshToken")) {
            return true;
        }
    }
    truthy(map.get("accessToken")) || truthy(map.get("access_token"))
}

/// `usage._token_from_blob`: `claudeAiOauth.accessToken`, the one bearer the
/// profile endpoint takes.
fn bearer_of(blob: &str) -> Option<String> {
    let data: Value = serde_json::from_str(blob).ok()?;
    let token = data.get("claudeAiOauth")?.get("accessToken")?.as_str()?;
    (!token.is_empty()).then(|| token.to_string())
}

/// `managed.credential_digest`: a stable digest of the credential inside a
/// blob (the claude access token when present), never the secret itself.
pub fn credential_digest(blob: &str) -> Option<String> {
    let material = blob.trim();
    if material.is_empty() {
        return None;
    }
    let token = serde_json::from_str::<Value>(material).ok().and_then(|v| {
        v.get("claudeAiOauth")?
            .get("accessToken")?
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    });
    let material = token.as_deref().unwrap_or(material);
    Some(format!("{:x}", Sha256::digest(material.as_bytes())))
}

/// `managed.slot_blobs` for claude: every distinct credential a reader of
/// `root` can be served. `None` is the shared slot, which also has the
/// unscoped Keychain item; a dir of its own reads only its scoped item and
/// its own file. A failed read is an error, never a smaller candidate set.
pub fn slot_blobs(root: Option<&Path>, probe: &dyn SlotProbe) -> Result<Vec<String>, ()> {
    let cfg = match root {
        Some(root) => root.to_path_buf(),
        None => home_slot(),
    };
    let mut services = vec![keychain_service(&cfg).ok_or(())?];
    if root.is_none() {
        services.push(UNSCOPED_SERVICE.to_string());
    }
    let mut out: Vec<String> = Vec::new();
    for service in services {
        let blob = probe.keychain(&service).map_err(|_| ())?;
        if let Some(blob) = blob.map(|b| b.trim().to_string()) {
            if !blob.is_empty() && token_present(&blob) && !out.contains(&blob) {
                out.push(blob);
            }
        }
    }
    let file = std::fs::read_to_string(cfg.join(".credentials.json")).unwrap_or_default();
    if !file.trim().is_empty() && token_present(&file) && !out.contains(&file) {
        out.push(file);
    }
    Ok(out)
}

fn home_slot() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude")
}

fn failure_reason(f: &ExternalFailure) -> &'static str {
    match f {
        ExternalFailure::Rejected => "credential-rejected",
        ExternalFailure::Unavailable => "profile-unavailable",
        ExternalFailure::Malformed => "malformed-profile",
    }
}

/// `managed.principal_of_blobs`: every candidate must prove and all must
/// name one account, else the slot is not attributable.
pub fn principal_of_blobs(
    blobs: &[String],
    probe: &dyn SlotProbe,
) -> Result<Principal, &'static str> {
    if blobs.is_empty() {
        return Err("no-slot-credential");
    }
    let mut resolved: Vec<Principal> = Vec::with_capacity(blobs.len());
    for blob in blobs {
        let bearer = bearer_of(blob).ok_or("credential-rejected")?;
        resolved.push(probe.profile(&bearer).map_err(|f| failure_reason(&f))?);
    }
    if resolved.iter().any(|p| *p != resolved[0]) {
        return Err("ambiguous-slot");
    }
    Ok(resolved.swap_remove(0))
}

/// `managed.identity_key`: account and organization, both required.
pub fn identity_key(p: &Principal) -> String {
    format!("{}/{}", p.account_uuid, p.organization_uuid)
}

/// `managed.record_principal`: the principal bound to `record_id` in its
/// private store metadata, `None` when it has never been bound.
pub fn record_principal(record_id: &str, root: &Path) -> Option<Principal> {
    let meta = crate::claude_vault::read_json(&root.join(record_id).join("meta.json"))?;
    crate::claude_vault::principal_from_meta(&meta)
}

/// `managed.cached_slot_principal`: the identity last proven for exactly this
/// credential, and when, while still fresh. Successful proofs only.
pub fn cached_slot_principal(
    harness: &str,
    root: &Path,
    credential: &str,
    now: f64,
    ttl: f64,
) -> Option<(String, f64)> {
    let digest = credential_digest(credential)?;
    let data = crate::claude_vault::read_json(&crate::accounts::store::principal_cache_path(
        root, harness,
    ))?;
    if data.get("credential").and_then(Value::as_str) != Some(digest.as_str()) {
        return None;
    }
    let identity = data.get("account_uuid")?.as_str()?.to_string();
    let at = data.get("at")?.as_f64()?;
    (now - at < ttl).then_some((identity, at))
}

/// `managed.note_slot_principal`: record a fresh proof for this credential.
pub fn note_slot_principal(harness: &str, root: &Path, identity: &str, credential: &str, now: f64) {
    let Some(digest) = credential_digest(credential) else {
        return;
    };
    let body = json!({ "account_uuid": identity, "credential": digest, "at": now });
    let _ = crate::claude_vault::atomic_write(
        &crate::accounts::store::principal_cache_path(root, harness),
        &body.to_string(),
    );
}

/// `managed.bearer_principal_verdict`: does `bearer` belong to `record_id`?
/// `match`, `mismatch`, or `unprovable`, which is never a pass.
pub fn bearer_principal_verdict(
    harness: &str,
    record_id: &str,
    root: &Path,
    bearer: &str,
    now: f64,
    ttl: f64,
    probe: &dyn SlotProbe,
) -> &'static str {
    let Some(bound) = record_principal(record_id, root) else {
        return "unprovable";
    };
    let want = identity_key(&bound);
    if let Some((got, _)) = cached_slot_principal(harness, root, bearer, now, ttl) {
        return if got == want { "match" } else { "mismatch" };
    }
    let Ok(principal) = probe.profile(bearer) else {
        return "unprovable";
    };
    let got = identity_key(&principal);
    note_slot_principal(harness, root, &got, bearer, now);
    if got == want {
        "match"
    } else {
        "mismatch"
    }
}

/// What one binding asks about.
pub struct BindingRequest<'a> {
    /// The pinned record; `None` asks who is signed in right now.
    pub record: Option<&'a Record>,
    pub harness: &'a str,
    /// Narrows the question to one exact credential.
    pub bearer: Option<&'a str>,
    /// The managed store root.
    pub root: &'a Path,
    /// Every record, for naming the observed identity.
    pub records: &'a [Record],
    pub now: f64,
    pub ttl: f64,
}

/// `binding.resolve_account_binding`: bind the record, or the unpinned shared
/// slot, to a proven principal. Never fails; every failure is a typed
/// `unknown`.
pub fn resolve(req: &BindingRequest<'_>, probe: &dyn SlotProbe) -> AccountBinding {
    let requested = req.record.map(|r| r.id.clone());
    let harness = req
        .record
        .map_or(req.harness, |r| r.harness.as_str())
        .to_string();
    let cred_root = req.record.and_then(credential_root);
    let mut observed_at = req.now;
    let at = |status: &'static str, reason: Option<&str>, observed_at: f64| AccountBinding {
        harness: harness.clone(),
        status,
        credential_root: cred_root.clone(),
        requested_record: requested.clone(),
        observed_principal: None,
        observed_label: None,
        matched_record: None,
        observed_at,
        reason: reason.map(str::to_string),
    };

    if harness != "claude" {
        return at(UNKNOWN, Some("unsupported-harness"), observed_at);
    }
    if req.record.is_some_and(|r| r.auth == "api_key") {
        return at(UNKNOWN, Some("api-key-route"), observed_at);
    }

    if let Some(bearer) = req.bearer {
        let Some(rid) = requested.as_deref() else {
            return at(UNKNOWN, Some("bearer-needs-record"), observed_at);
        };
        // claude reads the scoped item and the probe reads the unscoped one,
        // so the shared slot must present ONE principal however many blobs
        // hold it.
        if cred_root.is_none() {
            let Ok(blobs) = slot_blobs(None, probe) else {
                return at(UNKNOWN, Some("credential-unreadable"), observed_at);
            };
            if let Err(failure) = principal_of_blobs(&blobs, probe) {
                let status = if failure == "ambiguous-slot" {
                    AMBIGUOUS
                } else {
                    UNKNOWN
                };
                return at(status, Some(failure), observed_at);
            }
        }
        let verdict =
            bearer_principal_verdict(&harness, rid, req.root, bearer, req.now, req.ttl, probe);
        let status = match verdict {
            "match" => MATCHED,
            "mismatch" => MISMATCH,
            _ => UNKNOWN,
        };
        let mut out = at(status, (status != MATCHED).then_some(verdict), observed_at);
        if status == MATCHED {
            out.matched_record = Some(rid.to_string());
        }
        return out;
    }

    // An unbound record has nothing to compare against, so answering here
    // keeps the launch path offline on the common path.
    let mut want = None;
    if let Some(rid) = requested.as_deref() {
        match record_principal(rid, req.root) {
            Some(p) => want = Some(identity_key(&p)),
            None => return at(UNKNOWN, Some("unbound-principal"), observed_at),
        }
    }

    let Ok(blobs) = slot_blobs(cred_root.as_deref(), probe) else {
        return at(UNKNOWN, Some("credential-unreadable"), observed_at);
    };
    if blobs.is_empty() {
        return at(UNKNOWN, Some("no-slot-credential"), observed_at);
    }

    let mut observed = None;
    let mut label = None;
    if blobs.len() == 1 {
        if let Some((identity, when)) =
            cached_slot_principal(&harness, req.root, &blobs[0], req.now, req.ttl)
        {
            observed = Some(identity);
            observed_at = when;
        }
    }
    let observed = match observed {
        Some(identity) => identity,
        None => {
            let principal = match principal_of_blobs(&blobs, probe) {
                Ok(p) => p,
                Err(failure) => {
                    let status = if failure == "ambiguous-slot" {
                        AMBIGUOUS
                    } else {
                        UNKNOWN
                    };
                    return at(status, Some(failure), observed_at);
                }
            };
            let identity = identity_key(&principal);
            label = principal
                .email
                .clone()
                .filter(|e| !e.is_empty())
                .or_else(|| Some(principal.account_uuid.clone()));
            if blobs.len() == 1 {
                note_slot_principal(&harness, req.root, &identity, &blobs[0], req.now);
            }
            identity
        }
    };

    let mut matches: Vec<&str> = req
        .records
        .iter()
        .filter(|other| other.harness == harness)
        .filter(|other| {
            record_principal(&other.id, req.root).is_some_and(|p| identity_key(&p) == observed)
        })
        .map(|other| other.id.as_str())
        .collect();
    matches.sort_unstable();
    let matched = (matches.len() == 1).then(|| matches[0].to_string());
    let status_reason = match (&want, matches.len(), &matched) {
        (None, n, _) if n > 1 => (AMBIGUOUS, Some("ambiguous-match")),
        (None, _, None) => (UNKNOWN, Some("zero-match")),
        (None, _, Some(_)) => (MATCHED, None),
        (Some(want), _, _) if *want == observed => (MATCHED, None),
        (Some(_), _, _) => (MISMATCH, None),
    };
    let mut out = at(status_reason.0, status_reason.1, observed_at);
    out.observed_principal = Some(observed);
    out.observed_label = label;
    out.matched_record = matched;
    out
}

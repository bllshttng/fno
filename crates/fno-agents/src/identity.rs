//! Harness-session address rules shared by every Rust producer and resolver.

/// Footnote's one session-id mint: a random v4 UUID in 8-4-4-4-12 form.
/// Errors when the OS gives no randomness; there is no clock fallback.
pub fn mint_fno_id() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| format!("could not mint an fno_id: {e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// fno's own 8-hex handle for a registry row: the head of the row's
/// fno-minted `fno_id`. None for a legacy row whose fno_id is a harness
/// copy, a short id or a name - those keep the harness-head address.
pub(crate) fn fno_handle(fno_id: Option<&str>, own_ids: &[&str]) -> Option<String> {
    let id = fno_id?;
    let groups: Vec<&str> = id.split('-').collect();
    let v4 = groups.len() == 5
        && [
            groups[0].len(),
            groups[1].len(),
            groups[2].len(),
            groups[3].len(),
            groups[4].len(),
        ] == [8, 4, 4, 4, 12]
        && groups
            .iter()
            .all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_hexdigit()))
        && groups[2].as_bytes()[0] == b'4';
    if !v4 || own_ids.iter().any(|own| own.eq_ignore_ascii_case(id)) {
        return None;
    }
    Some(id[..8].to_ascii_lowercase())
}

/// mint_fno_id, redrawn until its 8-hex head is not in `taken` (lowercase
/// heads). Errors after 64 draws rather than looping.
pub fn mint_unique_fno_id(taken: &std::collections::HashSet<String>) -> Result<String, String> {
    mint_unique_fno_id_with(taken, mint_fno_id)
}

fn mint_unique_fno_id_with(
    taken: &std::collections::HashSet<String>,
    mut mint: impl FnMut() -> Result<String, String>,
) -> Result<String, String> {
    for _ in 0..64 {
        let id = mint()?;
        if !taken.contains(&id[..8]) {
            return Ok(id);
        }
    }
    Err("could not mint a unique fno_id head after 64 draws".to_string())
}

/// The harness head (the first eight of the session id), kept as a read-only
/// tier so handles printed before fno minted row handles keep resolving, and
/// kept as the mail bus key. For codex rows the list address is instead
/// [`fno_handle`], the head of the id fno itself mints; every other harness
/// keeps this head as its address until Python mail send and peek resolve
/// the fno handle. Codex ids are time-prefixed, so their first-8 collides
/// across same-window sessions; a shared head still resolves while it names
/// one row and refuses naming every match when it names two.
///
/// The rule the frozen address table in the tests pins (captured from the
/// deleted Python `canonical_handle`): if the two shapes differ a durable
/// send can address one handle while its recipient drains another and
/// silently strands on the bus.
pub(crate) fn canonical_handle(session_id: &str) -> String {
    let head: String = session_id.chars().take(8).collect();
    if session_id.starts_with("ses_") {
        head
    } else {
        head.to_ascii_lowercase()
    }
}

/// The agent handle this process resolves as, or None when the caller is no
/// agent session: the ancestry prover's session id through the canonical
/// handle. One read shared by the law door and the clear door, so the two
/// never disagree about who is at the door.
pub(crate) fn ambient_agent_handle() -> Option<String> {
    let ident = crate::spawn_context::resolve_self_identity(
        &|name| std::env::var(name).ok(),
        None,
        None,
        &crate::paths::AgentsHome::from_env(),
    );
    let session_id = ident.session_id.as_deref()?;
    ident.harness.as_deref()?;
    let handle = canonical_handle(session_id);
    (!handle.is_empty()).then_some(handle)
}

/// The retired last-eight address, read-only lookup compatibility only. Mail
/// addressed before the 2026-08-10 flip back to first-8 (when last-8 was the
/// address) still drains via this tier; it is never generated for new mail.
pub(crate) fn legacy_suffix_handle(session_id: &str) -> String {
    let mut tail = session_id.chars().rev().take(8).collect::<Vec<_>>();
    tail.reverse();
    let tail: String = tail.into_iter().collect();
    if session_id.starts_with("ses_") {
        tail
    } else {
        tail.to_ascii_lowercase()
    }
}

/// The bus address forms `session_id` drains, minus the short forms a second
/// stored session also answers to. Same-window codex ids share the first-eight
/// head, so a durable row staged under the head answers to both workers and
/// the first to drain it reads the sibling's mail. A short form with two
/// stored answers is therefore refused, never read; the full-id form carries
/// the session's own mail and never refuses. `stored` is every
/// `harness_session_id` the registry carries; an empty slice (an unreadable
/// registry) refuses nothing, because an unprovable ambiguity is not a proven
/// one. Returns `(forms, refused_short_forms)`.
pub(crate) fn bus_drain_forms(session_id: &str, stored: &[String]) -> (Vec<String>, Vec<String>) {
    let full = crate::spawn_context::session_identity_key(session_id);
    let mut forms: Vec<String> = Vec::new();
    for form in [
        canonical_handle(session_id),
        full.clone(),
        legacy_suffix_handle(session_id),
    ] {
        if !forms.contains(&form) {
            forms.push(form);
        }
    }
    let mut kept = Vec::new();
    let mut refused = Vec::new();
    for form in forms {
        if form == full {
            kept.push(form);
            continue;
        }
        let lowered = form.to_ascii_lowercase();
        let mut answers: std::collections::HashSet<String> = Default::default();
        for sid in stored {
            let sid = sid.trim();
            if sid.is_empty() {
                continue;
            }
            let hit = canonical_handle(sid).to_ascii_lowercase() == lowered
                || legacy_suffix_handle(sid).to_ascii_lowercase() == lowered;
            if hit {
                answers.insert(crate::spawn_context::session_identity_key(sid));
            }
        }
        if answers.len() > 1 {
            refused.push(form);
        } else {
            kept.push(form);
        }
    }
    (kept, refused)
}

/// Full id, canonical (first-8), and retired suffix (last-8) tiers shared across
/// Rust paths. Tier 1 is the canonical address; tier 2 is the read-only
/// transition lookup.
pub(crate) fn session_handle_tier(token: &str, session_id: &str) -> Option<u8> {
    let token = token.trim();
    if token.is_empty() || session_id.is_empty() {
        return None;
    }
    let exact_case = session_id.starts_with("ses_");
    let equal = |value: &str| {
        if exact_case {
            token == value
        } else {
            token.eq_ignore_ascii_case(value)
        }
    };
    [
        session_id.to_string(),
        canonical_handle(session_id),
        legacy_suffix_handle(session_id),
    ]
    .iter()
    .position(|value| equal(value))
    .map(|tier| tier as u8)
}

/// Whether an id's shape can definitively name `harness`. Only claude
/// (UUIDv4), codex (time-prefixed UUIDv7), and opencode (`ses_`) ids carry a
/// known shape; grok and pi threads run fno-minted UUIDv4s under their own
/// harness, so a v4 shape alone cannot convict them of being claude.
pub(crate) fn shape_known_harness(harness: &str) -> bool {
    matches!(harness, "claude" | "codex" | "opencode")
}

/// The harness an id's own shape names, or None when the shape is silent.
/// The keeper re-validates under the lock; the shape classes here are the
/// ones the deleted Python `harness_of_session_id` named, and a new shape
/// class edits this match, not a second table.
pub(crate) fn harness_of_session_id(session_id: &str) -> Option<&'static str> {
    let sid = session_id.trim();
    if sid.is_empty() {
        return None;
    }
    let bytes = sid.as_bytes();
    if sid.starts_with("ses_") && bytes[4..].iter().all(|b| b.is_ascii_alphanumeric()) {
        return Some("opencode");
    }
    // 8-4-4-4-12 hex; the version nibble is the first hex of group three.
    let groups: Vec<&str> = sid.split('-').collect();
    if groups.len() == 5
        && [
            groups[0].len(),
            groups[1].len(),
            groups[2].len(),
            groups[3].len(),
            groups[4].len(),
        ] == [8, 4, 4, 4, 12]
        && groups
            .iter()
            .all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        let version = groups[2].as_bytes()[0].to_ascii_lowercase();
        return match version {
            b'4' => Some("claude"),
            b'7' => Some("codex"),
            _ => None,
        };
    }
    None
}

/// The person behind this machine: the proven user, else `fleet.owner`,
/// else the login name.
pub(crate) fn person() -> String {
    proven_user()
        .map(|p| p.user)
        .or_else(stated_person)
        .unwrap_or_else(login_name)
}

/// The user an installed certificate proves for this machine.
pub(crate) struct ProvenUser {
    pub user: String,
    /// The certificate line, carried on every row this machine signs.
    pub cert: String,
}

/// The user this machine's own certificate proves: the user key signed it
/// and it names this machine's key. Read on every call, never cached, so a
/// long-lived daemon sees an install at once.
pub(crate) fn proven_user() -> Option<ProvenUser> {
    let root = crate::paths::AgentsHome::from_env_opt()?;
    let (line, cert) = crate::machine_key::installed_cert(root.root())?;
    let own = crate::machine_key::own().ok()?.public_key();
    (cert.machine_key == own).then_some(ProvenUser {
        user: cert.user,
        cert: line,
    })
}

/// The user key whose signature wide law needs to bind here: the installed
/// certificate's, even one that names a rotated machine key, so a rotation
/// never reopens this machine to unsigned law.
pub(crate) fn trusted_user_key() -> Option<String> {
    let root = crate::paths::AgentsHome::from_env_opt()?;
    crate::machine_key::installed_cert(root.root()).map(|(_, cert)| cert.user_key)
}

/// Sign a law-store row with this machine's key under its own certificate:
/// add `user_cert`, then `user_sig` over the row's canonical bytes. A
/// machine with no certificate adds nothing. Call it after every other
/// field is set.
pub(crate) fn sign_law_row(
    data: &mut serde_json::Map<String, serde_json::Value>,
    event_type: &str,
    ts: &str,
) {
    let Some(user) = proven_user() else {
        return;
    };
    let Ok(key) = crate::machine_key::own() else {
        return;
    };
    data.insert("user_cert".into(), user.cert.into());
    let mut row = data.clone();
    row.insert("_event_type".into(), event_type.into());
    row.insert("ts".into(), ts.into());
    let signature = key.sign(&law_row_bytes(&row));
    data.insert("user_sig".into(), signature.into());
}

/// The user key that proves a flattened store row: its certificate verifies
/// and the machine key that certificate names signed the row.
pub(crate) fn verify_law_row(row: &serde_json::Value) -> Option<String> {
    let row = row.as_object()?;
    let signature = row.get("user_sig")?.as_str()?;
    let cert = crate::machine_key::read_cert(row.get("user_cert")?.as_str()?)?;
    crate::machine_key::verify(&cert.machine_key, &law_row_bytes(row), signature)
        .then_some(cert.user_key)
}

/// The bytes a row signature covers: the flattened row (its data plus `ts`
/// and `_event_type`, the two keys both store flattens add) with every
/// object's keys sorted, because `preserve_order` keeps insertion order.
/// `subject` stays out, because a node rename rewrites it in place.
fn law_row_bytes(row: &serde_json::Map<String, serde_json::Value>) -> Vec<u8> {
    fn sorted(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let keys: std::collections::BTreeMap<_, _> = map.iter().collect();
                keys.into_iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect::<serde_json::Map<_, _>>()
                    .into()
            }
            serde_json::Value::Array(items) => items.iter().map(sorted).collect(),
            other => other.clone(),
        }
    }
    let mut signed = row.clone();
    signed.remove("subject");
    signed.remove("user_sig");
    let body = sorted(&serde_json::Value::Object(signed)).to_string();
    [b"fno-law-row-v1\n".as_slice(), body.as_bytes()].concat()
}

/// The nonblank `fleet.owner`, when the owner stated one. A display name,
/// never proof: the law door's wide-scope gate reads the certificate.
pub(crate) fn stated_person() -> Option<String> {
    crate::agents_config::config_lookup_global(&["fleet", "owner"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|v| !v.trim().is_empty())
}

/// The person as a mail sender token: the user part of a `from_id` and the
/// raw allowlist head. A display name that is no plain token (`Jason Smith`,
/// `José`) keeps the login name, because the reply parser routes only a
/// token there.
pub(crate) fn person_token() -> String {
    Some(person())
        .filter(|p| is_sender_token(p))
        .unwrap_or_else(login_name)
}

/// The one sender-token rule, shared by the writer and the reply parser.
pub(crate) fn is_sender_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn login_name() -> String {
    let set = |v: Result<String, std::env::VarError>| v.ok().filter(|u| !u.is_empty());
    // A launchd-started daemon can carry LOGNAME without USER.
    set(std::env::var("USER"))
        .or_else(|| set(std::env::var("LOGNAME")))
        .unwrap_or_else(|| "unknown".into())
}

/// Add `person` and `machine_id` to a record's data, keeping any value the
/// caller already set. An unreadable machine id writes no key: readers treat
/// a present `machine_id` as authoritative, so the hostname never stands in.
pub(crate) fn stamp_person(data: &mut serde_json::Map<String, serde_json::Value>) {
    if !data.contains_key("person") {
        data.insert("person".into(), person().into());
    }
    let machine_id = crate::claims::machine_id();
    if !machine_id.is_empty() && !data.contains_key("machine_id") {
        data.insert("machine_id".into(), machine_id.into());
    }
}

#[cfg(test)]
mod tests {
    use super::canonical_handle;

    /// The address table, frozen from Python \`canonical_handle\` the day its
    /// module deleted: first eight chars, lowercased unless the id is
    /// \`ses_\`-prefixed. One row per id family the proptest used to draw.
    const FROZEN_HANDLES: &[(&str, &str)] = &[
        ("d23c3d68-7d5c-4c1e-9a51-2f8e0a9b1c3d", "d23c3d68"),
        ("D23C3D68-7D5C-4C1E-9A51-2F8E0A9B1C3D", "d23c3d68"),
        (
            "20261102T101112-abcdef01234567890123456789abcdef",
            "20261102",
        ),
        ("ses_AbCdEfGhIjKlMnOpQrStUv", "ses_AbCd"),
        ("ses_lowercase-nope", "ses_lowe"),
        ("abcd", "abcd"),
        ("", ""),
        ("ABCDEF", "abcdef"),
        ("fno-0aa3a68f-9f5d-4c11-9d3e-3f1be2f5f1f2", "fno-0aa3"),
    ];

    #[test]
    fn canonical_handle_answers_the_frozen_table() {
        for (input, expected) in FROZEN_HANDLES {
            assert_eq!(&canonical_handle(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn a_short_form_two_stored_sessions_answer_to_is_refused_from_the_drain_forms() {
        // Same-window codex ids: both start with the shared head.
        let a = "01a1ad25-f04f-7220-8b8e-6c41a3bad0dd";
        let b = "01a1ad25-7752-7020-a858-4253ed259833";
        let stored = vec![a.to_string(), b.to_string()];
        let (kept, refused) = super::bus_drain_forms(a, &stored);
        assert!(
            !kept.iter().any(|f| f == "01a1ad25"),
            "the shared head drains nothing: {kept:?}"
        );
        assert_eq!(refused, vec!["01a1ad25".to_string()]);
        assert!(
            kept.contains(&crate::spawn_context::session_identity_key(a)),
            "the full-id form still drains: {kept:?}"
        );
        // A unique head keeps its short form.
        let (kept, refused) = super::bus_drain_forms(a, &[a.to_string()]);
        assert!(refused.is_empty(), "{refused:?}");
        assert!(kept.contains(&"01a1ad25".to_string()));
        // An unreadable registry (empty store) refuses nothing.
        let (kept, refused) = super::bus_drain_forms(a, &[]);
        assert!(
            refused.is_empty() && kept.len() >= 2,
            "{kept:?} {refused:?}"
        );
    }

    #[test]
    fn person_reads_fleet_owner_then_the_login_name() {
        use crate::claims::EnvVarGuard;
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _global = EnvVarGuard::set(
            "FNO_GLOBAL_SETTINGS_PATH",
            dir.path().join("settings.json").to_str().unwrap(),
        );
        let _user = EnvVarGuard::set("USER", "bb16");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[fleet]\nowner = \"jason\"\n").unwrap();
        assert_eq!(super::person(), "jason");
        std::fs::write(&config, "[fleet]\nowner = \"  \"\n").unwrap();
        assert_eq!(super::person(), "bb16", "a blank owner falls to USER");
        // A display name stays the person, but the mail token keeps the login.
        std::fs::write(&config, "[fleet]\nowner = \"Jason Smith\"\n").unwrap();
        assert_eq!(super::person(), "Jason Smith");
        assert_eq!(super::person_token(), "bb16");
    }
}

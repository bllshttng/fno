//! The successor name draw: the machine names the successor, the model only
//! accepts the draw or asks for a redraw.
//!
//! Why: a lead hand-off let the model pick the successor name, and the model
//! kept reaching for a short list of favorites (Rowan led the same scope
//! twice in nine days; the iMac drew the same name while the MacBook Rowan
//! lived). Two locks close it. `team_names` refuses every name already used
//! (the history ledger, merged across machines by `name_ledger`, plus this
//! host's store, registry, bus and handoffs), and every
//! draw takes a `name:` claim - shared across hosts when `store.remote_url`
//! is set - so two hosts never hold one name at once.
//!
//! The wire: a succession spawn with no `--name` draws and answers
//! `{"draw": {"name": ...}}`; an explicit `--name` must equal the scope's
//! pending draw (accept) or refuse naming it; `--name redraw` draws fresh.
//! Every draw is consumed into the history ledger, so a redraw and a peer
//! host never see the same name twice.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The draw pool: first names, all letter-only so `valid_person_name`
/// accepts every one. The model never picks from this list; the machine
/// draws at random. Deliberately excludes the favorites the model kept
/// reaching for (rowan, sorrel, wren, maya) and every name this fleet has
/// already burned (avery, barnaby, folio, harriet, kestrel, marlow, vellum);
/// the used-name filter is the general rule, this list is belt-and-suspenders.
const NAME_POOL: &[&str] = &[
    "aida",
    "alba",
    "alda",
    "anouk",
    "ansel",
    "arlet",
    "arlo",
    "asher",
    "aslan",
    "aubin",
    "aurel",
    "aurora",
    "aviva",
    "adler",
    "agnes",
    "aiden",
    "alanis",
    "albert",
    "aldous",
    "alfred",
    "alice",
    "alina",
    "amara",
    "amber",
    "amos",
    "andrew",
    "anita",
    "anton",
    "arden",
    "aria",
    "arjun",
    "arlen",
    "armel",
    "arnav",
    "atticus",
    "auden",
    "august",
    "ayla",
    "basil",
    "briar",
    "brook",
    "caius",
    "calla",
    "caris",
    "casper",
    "cedar",
    "celine",
    "clark",
    "clement",
    "clio",
    "colm",
    "conrad",
    "cora",
    "cyril",
    "dahlia",
    "dana",
    "darius",
    "darla",
    "dawn",
    "deirdre",
    "delia",
    "deniz",
    "dexter",
    "diana",
    "dominic",
    "dorian",
    "dorothy",
    "duncan",
    "edith",
    "edmund",
    "elena",
    "elias",
    "eliot",
    "elisa",
    "ella",
    "elsie",
    "emery",
    "emile",
    "emilia",
    "enid",
    "erasmus",
    "erin",
    "esme",
    "esther",
    "ethan",
    "eugenia",
    "evelyn",
    "ezra",
    "faye",
    "felix",
    "fern",
    "flora",
    "gale",
    "gavin",
    "gemma",
    "geoffrey",
    "gilbert",
    "gina",
    "giselle",
    "glenn",
    "gloria",
    "greta",
    "gunnar",
    "gus",
    "hana",
    "hansel",
    "harlan",
    "harvey",
    "hazel",
    "helen",
    "helena",
    "henri",
    "henry",
    "hilda",
    "hollis",
    "hugh",
    "ida",
    "idris",
    "ilse",
    "imogen",
    "ingrid",
    "ira",
    "iris",
    "isabel",
    "ivan",
    "ivy",
    "jade",
    "jasper",
    "jules",
    "julia",
    "june",
    "kai",
    "kara",
    "kate",
    "keira",
    "kelsey",
    "kendall",
    "kenji",
    "kira",
    "kit",
    "knut",
    "kora",
    "kurt",
    "kyle",
    "lara",
    "laura",
    "leila",
    "lena",
    "lenore",
    "leon",
    "leona",
    "leslie",
    "lila",
    "lilian",
    "linda",
    "lloyd",
    "lola",
    "loren",
    "lotte",
    "lucas",
    "lucia",
    "lucille",
    "lulu",
    "luther",
    "lyra",
    "mabel",
    "magnus",
    "malik",
    "manon",
    "mara",
    "marcus",
    "maren",
    "margot",
    "marian",
    "mario",
    "marisol",
    "marjorie",
    "marvin",
    "mason",
    "matilda",
    "maureen",
    "max",
    "maxine",
    "meadow",
    "melvin",
    "mercedes",
    "merle",
    "mia",
    "milo",
    "mina",
    "mirabel",
    "miriam",
    "misty",
    "monica",
    "mordecai",
    "moses",
    "muriel",
    "myra",
    "nadia",
    "nancy",
    "naomi",
    "nolan",
    "nora",
    "nova",
    "oakley",
    "olga",
    "oliver",
    "olivia",
    "omar",
    "orla",
    "oscar",
    "otto",
    "owen",
    "pablo",
    "pamela",
    "patricia",
    "paul",
    "pearl",
    "penelope",
    "peregrine",
    "perry",
    "petra",
    "philip",
    "phoebe",
    "pia",
    "pilar",
    "quincy",
    "quinn",
    "rafael",
    "ramona",
    "raquel",
    "rasmus",
    "ravi",
    "rebecca",
    "regina",
    "renata",
    "rey",
    "rhys",
    "rita",
    "robert",
    "robin",
    "ronda",
    "rosa",
    "rosalind",
    "rose",
    "roxanne",
    "ruby",
    "rufus",
    "rupert",
    "rusty",
    "sadie",
    "samuel",
    "sandra",
    "santiago",
    "scarlett",
    "scott",
    "sebastian",
    "serena",
    "seth",
    "sidney",
    "silas",
    "simon",
    "simone",
    "sofia",
    "sol",
    "sonia",
    "sophie",
    "stella",
    "stephen",
    "sylvia",
    "tamara",
    "tamsin",
    "tania",
    "thea",
    "theo",
    "theodora",
    "theresa",
    "thomas",
    "tiberius",
    "tilda",
    "timothy",
    "titus",
    "tomas",
    "tracy",
    "trevor",
    "tristan",
    "ursula",
    "val",
    "valentina",
    "vera",
    "vernon",
    "verity",
    "victor",
    "vincent",
    "viola",
    "vivian",
    "wanda",
    "wendell",
    "werner",
    "wesley",
    "willow",
    "wilson",
    "winifred",
    "xenia",
    "yasmin",
    "yvette",
    "yvonne",
    "zelda",
    "zenobia",
    "zephyr",
    "zora",
];

/// A rejection-sampled uniform index into `len` buckets: no modulo bias
/// leans on the first names. `len` is never zero (the caller checked).
fn random_index(len: usize) -> Result<usize, String> {
    loop {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes)
            .map_err(|e| format!("entropy unavailable for the name draw: {e}"))?;
        let value = u64::from_le_bytes(bytes);
        let limit = u64::MAX - (u64::MAX % len as u64);
        if value < limit {
            return Ok((value % len as u64) as usize);
        }
    }
}

/// Take the `name:` claim for `name`: the cross-host gate. The holder names
/// this machine, so a peer's live claim refuses our draw (HeldByOther) and
/// our own re-acquire (the accept) is idempotent. The lease sits at the
/// claim store's one-day ceiling: the lease only arbitrates the tight race
/// between two hosts drawing the same free name, and the row outlives its
/// lease - the used-name read takes every `name:` row including expired
/// ones, so a name whose claim expired stays un drawable forever.
fn claim_name(name: &str, root: Option<&Path>) -> Result<(), String> {
    let key = format!("name:{}", name.to_ascii_lowercase());
    let opts = crate::claims::AcquireOpts {
        pid_unavailable: true,
        ttl_ms: Some(86_400_000),
        reason: Some("successor name draw; a name is used once".into()),
        root: root.map(Path::to_path_buf),
        ..Default::default()
    };
    match crate::claims::acquire(&key, &claim_holder(), opts) {
        crate::claims::AcquireOutcome::Acquired(_) => Ok(()),
        crate::claims::AcquireOutcome::HeldByOther { holder, .. } => Err(format!(
            "the name {} is claimed by {holder} on another host",
            crate::team_names::display(name, 1)
        )),
        crate::claims::AcquireOutcome::Error(e) => Err(e),
    }
}

/// The holder names THIS machine, so two hosts never read as one holder and
/// the claim store's same-holder overwrite can never merge two fleets.
fn claim_holder() -> String {
    format!("team-names@{}", crate::claims::hostname())
}

/// Draw the successor's person name. The machine draws at random from
/// [`NAME_POOL`] minus every name ever used (history, store, registry, bus,
/// handoffs, and the `name:` claims the peer hosts wrote), so a retired
/// lead's name never returns and two hosts never draw the same name. An
/// explicit proposal must equal the scope's pending draw (accept) or refuses
/// naming it; `redraw` forces a fresh draw. Every draw is consumed: it
/// appends to the history and takes the `name:` claim before anyone
/// launches with it.
///
/// `outgoing` names the holders this succession replaces. An outgoing name
/// is already used when its row sits in this machine's registry; naming it
/// here keeps the rule when the outgoing lead lived on a peer machine.
pub fn draw_successor_name(
    store_path: &Path,
    registry_path: &Path,
    scope: &str,
    proposed: Option<&str>,
    redraw: bool,
    claims_root: Option<&Path>,
    outgoing: &[String],
) -> Result<String, String> {
    let canon = crate::territory::canonical_scope(scope);
    let pending = crate::team_names::pending_draw(store_path, scope)?;
    if let Some(name) = proposed.map(str::trim).filter(|p| !p.is_empty()) {
        if !redraw {
            let accepted = pending
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(name));
            if !accepted {
                return Err(match pending.as_deref() {
                    Some(p) => format!(
                        "the machine draws successor names; the draw over {canon} is {}. Accept it (--name {}) or ask for a redraw (--name redraw)",
                        crate::team_names::display(p, 1),
                        crate::team_names::display(p, 1),
                    ),
                    None => format!(
                        "no successor name is drawn over {canon} yet; drop --name to take the machine's draw, or pass --name redraw to draw one now"
                    ),
                });
            }
            claim_name(name, claims_root)?;
            // The accept keeps the proposal as typed: the row already
            // carries this exact spelling, and the seam's adoption compares
            // against it.
            return Ok(name.to_string());
        }
    }
    // Fresh draw (no proposal, or an explicit redraw): the history flock is
    // held across filter, claim and append, so two draws on this host can
    // never pick the same free name; a peer host is closed out by the claim
    // itself.
    let lock = crate::state::acquire_exclusive(&crate::state::lock_path(
        &crate::team_names::history_path(store_path),
    ))
    .map_err(|e| e.to_string())?;
    let drawn = (|| -> Result<String, String> {
        let mut used = crate::team_names::used_names(store_path, registry_path)?;
        used.extend(outgoing.iter().map(|n| n.to_ascii_lowercase()));
        let pool: Vec<&str> = NAME_POOL
            .iter()
            .copied()
            .filter(|n| !used.contains(&n.to_ascii_lowercase()))
            .collect();
        if pool.is_empty() {
            return Err("the name pool is exhausted; extend NAME_POOL in name_draw.rs".into());
        }
        let mut last_error = String::from("no draw was attempted");
        for _ in 0..pool.len().min(5) {
            let pick = pool[random_index(pool.len())?];
            match claim_name(pick, claims_root) {
                Ok(()) => {
                    crate::team_names::history_append_locked(
                        store_path,
                        crate::team_names::NameHistoryEntry::here(pick, &canon, None, "drawn"),
                    )?;
                    return Ok(crate::team_names::display(pick, 1));
                }
                Err(e) => {
                    last_error = e;
                }
            }
        }
        Err(format!(
            "every drawn name is claimed on another host ({last_error}); ask for another redraw"
        ))
    })();
    let _ = lock.unlock();
    drawn
}

/// The succession draw on a successful plan answer: the machine names the
/// successor, the model accepts or asks for a redraw. A refusal turns the
/// answer into a decline, so the spawn never launches with a hand-typed
/// name. Silent on every answer the draw does not own: a refused spawn
/// burns no name, and the apply pass (a payload carrying `plan`) binds the
/// drawn name without redrawing.
pub fn settle_draw(payload: &Value, answer: &mut Value, store: &std::path::Path) {
    if payload.get("plan").is_some() {
        return;
    }
    if payload
        .get("slot_probe")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        // A gate occupancy probe: it asks whether a succession would
        // replace the caller, never for a name. Drawing here would burn a
        // name on every gate retry.
        return;
    }
    let succession = payload
        .get("succession")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let registry = store.with_file_name("registry.json");
    let claims_root = payload
        .get("claims_root")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let scope = payload.get("scope").and_then(Value::as_str).unwrap_or("");
    let redraw = payload
        .get("redraw")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let outcome = answer.get("outcome").and_then(Value::as_str);
    if !succession && !payload.get("take_over").is_some_and(Value::is_object) {
        // A fresh grant: `--name redraw` draws the new lead's name, and a
        // retired `--name` refusal names a fresh draw to retry with.
        let retired = answer.get("retired").and_then(Value::as_str).is_some();
        let wants =
            (redraw && outcome == Some("granted")) || (retired && outcome == Some("declined"));
        if !wants {
            return;
        }
        let drawn = draw_successor_name(
            store,
            &registry,
            scope,
            None,
            true,
            claims_root.as_deref(),
            &[],
        );
        match (drawn, retired) {
            (Ok(drawn), false) => answer["draw"] = json!({"name": drawn}),
            (Ok(drawn), true) => {
                let refusal = answer["refusal"].as_str().unwrap_or("").to_string();
                answer["refusal"] = json!(format!(
                    "{refusal}. The machine drew {drawn}: retry with --name {drawn}"
                ));
            }
            (Err(refusal), false) => {
                answer["outcome"] = json!("declined");
                answer["refusal"] = json!(refusal);
            }
            (Err(e), true) => {
                let refusal = answer["refusal"].as_str().unwrap_or("").to_string();
                answer["refusal"] = json!(format!(
                    "{refusal}. The machine could not draw a fresh name ({e}); retry with --name redraw"
                ));
            }
        }
        return;
    }
    if !succession || outcome != Some("succeeded") {
        return;
    }
    // The plan answer names the holders this succession vacates.
    let outgoing: Vec<String> = answer
        .get("holders")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    match draw_successor_name(
        store,
        &registry,
        scope,
        payload
            .get("proposed_name")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty()),
        redraw,
        claims_root.as_deref(),
        &outgoing,
    ) {
        Ok(drawn) => answer["draw"] = json!({"name": drawn}),
        Err(refusal) => {
            answer["outcome"] = json!("declined");
            answer["refusal"] = json!(refusal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_explicit_name_must_match_the_pending_draw_and_a_redraw_never_repeats() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("team_names.json");
        let registry = tmp.path().join("registry.json");
        crate::registry_store::seed_raw(
            &registry,
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": []})
                .to_string(),
        );
        // No pending draw: a hand-typed name refuses with the teaching text.
        let err = draw_successor_name(
            &store,
            &registry,
            "x-aaaa",
            Some("imogen"),
            false,
            Some(tmp.path()),
            &[],
        )
        .unwrap_err();
        assert!(
            err.contains("no successor name is drawn over x-aaaa"),
            "{err}"
        );
        assert!(err.contains("--name redraw"), "{err}");
        // The drawn name is accepted; anything else refuses.
        let drawn = draw_successor_name(
            &store,
            &registry,
            "x-aaaa",
            None,
            false,
            Some(tmp.path()),
            &[],
        )
        .unwrap();
        let ok = draw_successor_name(
            &store,
            &registry,
            "x-aaaa",
            Some(drawn.trim()),
            false,
            Some(tmp.path()),
            &[],
        )
        .unwrap();
        assert_eq!(ok, drawn);
        let err = draw_successor_name(
            &store,
            &registry,
            "x-aaaa",
            Some("someone-else"),
            false,
            Some(tmp.path()),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("Accept it"), "{err}");
        // A redraw never returns the consumed draw.
        let second = draw_successor_name(
            &store,
            &registry,
            "x-aaaa",
            None,
            true,
            Some(tmp.path()),
            &[],
        )
        .unwrap();
        assert_ne!(drawn, second);
    }

    /// The successor never draws an outgoing holder's name, even when that
    /// holder's row lives on a peer machine this registry never saw: every
    /// pool name but one is outgoing, so the draw has one answer.
    #[test]
    fn the_draw_never_returns_an_outgoing_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("team_names.json");
        crate::registry_store::seed_raw(
            &tmp.path().join("registry.json"),
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": []})
                .to_string(),
        );
        let holders: Vec<&str> = NAME_POOL
            .iter()
            .copied()
            .filter(|n| *n != "zelda")
            .collect();
        let mut answer = json!({"outcome": "succeeded", "holders": holders});
        let payload = json!({"succession": true, "scope": "x-aaaa", "claims_root": tmp.path()});
        settle_draw(&payload, &mut answer, &store);
        assert_eq!(answer["draw"]["name"], "Zelda", "{answer}");
    }

    #[test]
    fn settle_draw_declines_a_spawn_that_refuses_the_draw_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("team_names.json");
        let registry = tmp.path().join("registry.json");
        crate::registry_store::seed_raw(
            &registry,
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": []})
                .to_string(),
        );
        // A pending draw exists, so the hand-typed mismatch names the
        // machine-draw contract; with no pending draw the same input gets
        // the no-draw arm instead.
        let drawn = draw_successor_name(
            &store,
            &registry,
            "x-aaaa",
            None,
            false,
            Some(tmp.path()),
            &[],
        )
        .unwrap();
        let mut answer = json!({"outcome": "succeeded"});
        let payload = json!({
            "succession": true, "scope": "x-aaaa",
            "proposed_name": "hand-typed",
        });
        settle_draw(&payload, &mut answer, &store);
        assert_eq!(answer["outcome"], "declined");
        assert!(answer["refusal"]
            .as_str()
            .unwrap()
            .contains("the machine draws successor names"));
        assert!(answer["refusal"].as_str().unwrap().contains(&drawn));
        // The apply pass (a plan in the payload) never redraws.
        let mut answer = json!({"outcome": "succeeded"});
        let payload = json!({
            "succession": true, "scope": "x-aaaa", "plan": {"holder_ids": []},
        });
        settle_draw(&payload, &mut answer, &store);
        assert_eq!(answer["outcome"], "succeeded");
        assert!(answer.get("draw").is_none());
    }

    /// A fresh grant draws only when asked: `--name redraw` names the new
    /// lead, and a retired `--name` refusal names a fresh name to retry with.
    #[test]
    fn a_fresh_grant_draws_on_redraw_and_names_a_draw_in_a_retired_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("team_names.json");
        crate::registry_store::seed_raw(
            &tmp.path().join("registry.json"),
            json!({"schema_version": crate::state::REGISTRY_SCHEMA_VERSION, "agents": []})
                .to_string(),
        );
        let root = tmp.path().to_str().unwrap();
        let mut answer = json!({"outcome": "granted"});
        settle_draw(
            &json!({"scope": "x-aaaa", "claims_root": root}),
            &mut answer,
            &store,
        );
        assert!(answer.get("draw").is_none(), "{answer}");
        settle_draw(
            &json!({"scope": "x-aaaa", "redraw": true, "claims_root": root}),
            &mut answer,
            &store,
        );
        let drawn = answer["draw"]["name"].as_str().unwrap().to_string();
        assert_eq!(
            crate::team_names::pending_draw(&store, "x-aaaa").unwrap(),
            Some(drawn.to_ascii_lowercase())
        );
        let mut answer = json!({"outcome": "declined", "retired": "Rowan",
            "refusal": "--promote --name requires a fresh person name: Rowan is retired"});
        settle_draw(
            &json!({"scope": "x-bbbb", "proposed_name": "Rowan", "claims_root": root}),
            &mut answer,
            &store,
        );
        let refusal = answer["refusal"].as_str().unwrap();
        let fresh = refusal.rsplit("--name ").next().unwrap();
        assert!(refusal.contains("The machine drew"), "{refusal}");
        assert_ne!(fresh, drawn, "{refusal}");
        assert_eq!(answer["outcome"], "declined");
    }
}

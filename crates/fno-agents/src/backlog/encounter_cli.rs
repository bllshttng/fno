//! `fno backlog encounter`: record ONE encounter with a node, from this
//! session, with evidence. The vote is the demand signal `fno backlog
//! demand` reads, so the refusals are the feature: evidence is required,
//! identity must be provable, over-length evidence is refused rather than
//! truncated, and one voter votes once per node.
//!
//! Exits: 0 recorded, 1 missing node / empty evidence / backend, 2 usage,
//! 3 duplicate, 4 style, 5 no provable identity.

use serde_json::{Map, Value};
use std::cell::RefCell;
use std::io::Write;
use std::path::Path;

const ENCOUNTER_HELP: &str = "Usage: fno backlog encounter [OPTIONS] TASK_ID

  Record ONE encounter with this node, from this session, with evidence.

  An encounter is a thing that happened and cannot be edited or withdrawn:
  there is no correction verb, and a later correction is a `fno backlog note`.
  Full contract: docs/architecture/backlog-graph-verb-contracts.md

Arguments:
  TASK_ID  Node id this session hit while doing something else.  [required]

Options:
  -e, --evidence TEXT  What it cost, in a sentence or two. Required, and capped
                       by config.style.word_cap.encounter.  [required]
  --operator           Record this as the operator's vote under the stable
                       'operator' voter key. A declaration, not proof; one per
                       node, and the casting session's id is kept on the record
                       when provable.
  -J, --json           Emit the appended record as JSON.
  --friction TEXT      The fingerprint of a friction offer this session was
                       shown. Adds the working node, stage and fingerprint to
                       the vote; an unknown fingerprint refuses (exit 2).
  -h, --help           Show this message and exit.
";

struct Args {
    task_id: String,
    evidence: String,
    operator: bool,
    json: bool,
    friction: Option<String>,
}

fn parse(tail: &[String]) -> Result<Args, String> {
    let mut task_id = None;
    let mut evidence = None;
    let mut operator = false;
    let mut json = false;
    let mut friction = None;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-e" | "--evidence" => {
                let value = inline.or_else(|| it.next().cloned());
                evidence = Some(value.ok_or("Option '--evidence' requires an argument.")?);
            }
            "--friction" => {
                let value = inline.or_else(|| it.next().cloned());
                friction = Some(value.ok_or("Option '--friction' requires an argument.")?);
            }
            "--operator" => operator = true,
            "-J" | "--json" => json = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("No such option: {other}"));
            }
            _ if task_id.is_none() => task_id = Some(arg.clone()),
            _ => return Err(format!("Got unexpected extra argument ({arg})")),
        }
    }
    Ok(Args {
        task_id: task_id.ok_or("Missing argument 'TASK_ID'.")?,
        evidence: evidence.ok_or("Missing option '-e' / '--evidence'.")?,
        operator,
        json,
        friction,
    })
}

/// Model and effort provenance for a vote, or nothing: claude only, and
/// the model only when a gateway (`ANTHROPIC_BASE_URL`) names it, because a
/// lone `ANTHROPIC_MODEL` is not proof of what ran.
pub(crate) fn vote_provenance(harness: Option<&str>) -> Map<String, Value> {
    let mut out = Map::new();
    if harness != Some("claude") {
        return out;
    }
    let set = |v: Result<String, std::env::VarError>| {
        v.ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if let Some(effort) = set(std::env::var("CLAUDE_EFFORT")) {
        out.insert("effort".into(), Value::String(effort));
    }
    if set(std::env::var("ANTHROPIC_BASE_URL")).is_some() {
        if let Some(model) = set(std::env::var("ANTHROPIC_MODEL")) {
            out.insert("model".into(), Value::String(model));
        }
    }
    out
}

/// The style gate on vote evidence: `Some(report)` when the evidence breaks
/// a rule. `FNO_STYLE_ENFORCE=0` and a `style-exception` line both escape it;
/// neither escapes the evidence or identity refusals.
pub(crate) fn style_refusal(evidence: &str) -> Option<String> {
    if std::env::var("FNO_STYLE_ENFORCE").as_deref() == Ok("0")
        || super::style_check::has_exception(evidence).is_some()
    {
        return None;
    }
    let cap = super::settings::config_candidates()
        .iter()
        .find_map(|doc| {
            doc.get("style")?
                .get("word_cap")?
                .get("encounter")?
                .as_u64()
        })
        .map(|n| n as usize);
    let violations = super::style_check::check(evidence, "encounter", cap);
    (!violations.is_empty())
        .then(|| super::style_check::format_violations(&violations, "encounter"))
}

/// The first 8 characters of a session id: the short handle `fno_id` stores.
pub(crate) fn canonical_handle(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

/// Who casts the vote. `session_id` and `harness` are what this process
/// can prove; `operator` is the declared lane.
pub(crate) struct Voter {
    pub session_id: Option<String>,
    pub harness: Option<String>,
    pub operator: bool,
}

/// The vote record, keyed in the order every reader has seen since the
/// verb was Python: stamp, evidence, voter, then provenance.
pub(crate) fn vote_record(stamp_key: &str, evidence: &str, voter: &Voter) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert(
        stamp_key.into(),
        Value::String(crate::graph_store::now_isoformat()),
    );
    record.insert("evidence".into(), Value::String(evidence.to_string()));
    let string = |s: &str| Value::String(s.to_string());
    if voter.operator {
        // voter_key stays `operator` for the dedupe and the demand split;
        // the casting session rides along when it is provable.
        record.insert("voter_key".into(), string("operator"));
        record.insert("voter_kind".into(), string("operator"));
        if let Some(sid) = &voter.session_id {
            record.insert("session_id".into(), string(sid));
            record.insert("fno_id".into(), Value::String(canonical_handle(sid)));
        }
        if let Some(h) = &voter.harness {
            record.insert("harness".into(), string(h));
        }
    } else if let (Some(sid), Some(h)) = (&voter.session_id, &voter.harness) {
        record.insert("session_id".into(), string(sid));
        record.insert("voter_key".into(), string(sid));
        record.insert("voter_kind".into(), string("agent"));
        record.insert("harness".into(), string(h));
        record.insert("fno_id".into(), Value::String(canonical_handle(sid)));
    }
    record.extend(vote_provenance(voter.harness.as_deref()));
    record
}

#[derive(Debug, PartialEq)]
pub(crate) enum VoteRefusal {
    /// No node resolves, or the record names no voter (exit 1).
    Missing(String),
    /// This voter already voted on this node (exit 3).
    Duplicate(String),
    /// The store refused the write (exit 1).
    Store(String),
}

/// Append one vote through the single-row write seam. `token` resolves the
/// way `_find_node` did (exact id, `ab-` short prefix, dash variants), then
/// by exact slug. Returns the resolved id and the node's vote count.
pub(crate) fn append_vote(
    graph: &Path,
    token: &str,
    record: Map<String, Value>,
) -> Result<(String, usize), VoteRefusal> {
    let refusal: RefCell<Option<VoteRefusal>> = RefCell::new(None);
    let landed: RefCell<Option<(String, usize)>> = RefCell::new(None);
    let written = crate::backlog::mutate_single_row(graph, "encounter", |rows| {
        let idx = super::node_ref::find_node_index(rows, token).or_else(|| {
            rows.iter().position(|r| {
                r.get("slug")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(token))
            })
        });
        let Some(id) = idx
            .and_then(|i| crate::graph_store::entry_id(&rows[i]))
            .map(str::to_string)
        else {
            *refusal.borrow_mut() = Some(VoteRefusal::Missing(format!(
                "no node resolves to '{token}'"
            )));
            return Ok(false);
        };
        let out =
            super::encounters::append_refusing_duplicate(rows, &id, Value::Object(record.clone()));
        if out["appended"] != Value::Bool(true) {
            let error = out["error"].as_str().unwrap_or_default().to_string();
            *refusal.borrow_mut() = Some(match out["reason"].as_str() {
                Some("duplicate") => VoteRefusal::Duplicate(error),
                _ => VoteRefusal::Missing(error),
            });
            return Ok(false);
        }
        let total = rows
            .iter()
            .find(|r| crate::graph_store::entry_id(r) == Some(id.as_str()))
            .and_then(|r| r.get("encounters"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        *landed.borrow_mut() = Some((id, total));
        Ok(true)
    })
    .map_err(VoteRefusal::Store)?;
    if let Some(r) = refusal.into_inner() {
        return Err(r);
    }
    match (written, landed.into_inner()) {
        (true, Some(hit)) => Ok(hit),
        _ => Err(VoteRefusal::Store("the vote did not land".into())),
    }
}

pub fn run(tail: &[String]) -> i32 {
    if tail.iter().any(|t| t == "--help" || t == "-h") {
        print!("{ENCOUNTER_HELP}");
        return 0;
    }
    if let Some(code) = super::workflows::refuse_non_graph_backend("encounter") {
        return code;
    }
    let get = |name: &str| std::env::var(name).ok();
    let ident = crate::spawn_context::resolve_self_identity(
        &get,
        None,
        None,
        &crate::paths::AgentsHome::from_env(),
    );
    let clean = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    run_with(
        tail,
        clean(ident.session_id),
        clean(ident.harness),
        &super::settings::graph_path(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// The verb over an already-resolved identity, so a test injects the one
/// prover that walks the process tree and runs everything else for real.
fn run_with(
    tail: &[String],
    session_id: Option<String>,
    harness: Option<String>,
    graph: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let args = match parse(tail) {
        Ok(a) => a,
        Err(message) => {
            let _ = writeln!(
                err,
                "Usage: fno backlog encounter [OPTIONS] TASK_ID\nTry 'fno backlog encounter --help' for help.\n\nError: {message}"
            );
            return 2;
        }
    };
    let evidence = args.evidence.trim();
    if evidence.is_empty() {
        let _ = writeln!(
            err,
            "Error: an encounter with no evidence is a poll. Name what it cost."
        );
        return 1;
    }
    if !args.operator && (session_id.is_none() || harness.is_none()) {
        let _ = writeln!(
            err,
            "Error: no provable session identity, so this encounter would not be \
             readable back to a transcript. Run `fno whoami` to see what this \
             session can prove."
        );
        return 5;
    }
    if let Some(report) = style_refusal(evidence) {
        let _ = writeln!(err, "{report}");
        return 4;
    }
    let voter = Voter {
        session_id,
        harness,
        operator: args.operator,
    };
    let mut record = vote_record("created_at", evidence, &voter);
    if let Some(fp) = &args.friction {
        let offer = voter
            .session_id
            .as_deref()
            .ok_or_else(|| std::path::PathBuf::from("<no provable session>"))
            .and_then(|sid| crate::friction::offer_for(sid, fp));
        let offer = match offer {
            Ok(offer) => offer,
            Err(ledger) => {
                let _ = writeln!(
                    err,
                    "Error: no friction offer {fp} in this session's ledger ({}). \
                     Vote without --friction, or use the fingerprint the offer named.",
                    ledger.display()
                );
                return 2;
            }
        };
        for (key, from) in [("working_node", "node"), ("stage", "stage")] {
            if let Some(v) = offer.get(from).filter(|v| !v.is_null()) {
                record.insert(key.into(), v.clone());
            }
        }
        record.insert("friction".into(), Value::String(fp.clone()));
        if !record.contains_key("model") {
            if let Some(m) = offer.get("model").filter(|v| !v.is_null()) {
                record.insert("model".into(), m.clone());
            }
        }
    }
    let total = match append_vote(graph, &args.task_id, record.clone()) {
        Ok((_, total)) => total,
        Err(VoteRefusal::Duplicate(e)) => {
            let _ = writeln!(err, "Error: {e}");
            let _ = writeln!(
                err,
                "Add a `fno backlog note` instead if there is more to say."
            );
            return 3;
        }
        Err(VoteRefusal::Missing(e) | VoteRefusal::Store(e)) => {
            let _ = writeln!(err, "Error: {e}");
            return 1;
        }
    };
    if args.json {
        let receipt = serde_json::json!({"id": args.task_id, "encounter": record, "total": total});
        let _ = writeln!(out, "{receipt}");
    } else if voter.operator {
        let _ = writeln!(
            out,
            "encounter recorded on {} (operator, {total} total)",
            args.task_id
        );
    } else {
        let handle = canonical_handle(voter.session_id.as_deref().unwrap_or_default());
        let _ = writeln!(
            out,
            "encounter recorded on {} (session {handle}, {total} total)",
            args.task_id
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::EnvVarGuard;
    use serde_json::json;

    const A: &str = "aaaaaaaa-1111-4111-8111-111111111111";
    const B: &str = "bbbbbbbb-2222-4222-8222-222222222222";

    /// One hermetic store and config per test. The guards restore the env
    /// on drop; the lock holds every env reader out while they are set.
    struct Fixture {
        _lock: std::sync::MutexGuard<'static, ()>,
        _env: Vec<EnvVarGuard>,
        _dir: tempfile::TempDir,
        graph: std::path::PathBuf,
    }

    fn fixture(style_off: bool) -> Fixture {
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.yaml");
        std::fs::write(&settings, "schema_version: 1\n").unwrap();
        let mut env = vec![
            EnvVarGuard::set("FNO_GLOBAL_SETTINGS_PATH", settings.to_str().unwrap()),
            EnvVarGuard::set("FNO_CONFIG", settings.to_str().unwrap()),
        ];
        env.push(EnvVarGuard::set(
            "FNO_STYLE_ENFORCE",
            if style_off { "0" } else { "1" },
        ));
        let graph = dir.path().join("graph.json");
        let node = json!({"id": "zz-0001", "slug": "slug-zz-0001", "title": "seed node",
                          "status": "ready", "priority": "p2"});
        crate::graph_store::seed_rows_with_slugs(&graph, &[node]).unwrap();
        Fixture {
            _lock: lock,
            _env: env,
            _dir: dir,
            graph,
        }
    }

    fn vote(graph: &Path, argv: &[&str], sid: Option<&str>) -> (i32, String, String) {
        let tail: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(
            &tail,
            sid.map(str::to_string),
            sid.map(|_| "codex".to_string()),
            graph,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn encounters(graph: &Path) -> Vec<Value> {
        crate::graph_store::read_rows(graph)
            .unwrap()
            .into_iter()
            .find(|r| r["id"] == "zz-0001")
            .and_then(|r| r.get("encounters").and_then(Value::as_array).cloned())
            .unwrap_or_default()
    }

    #[test]
    fn each_refusal_has_its_exit_code_and_marker() {
        let f = fixture(true);
        // A typo is a usage error: 2, distinct from every refusal below.
        assert_eq!(
            vote(&f.graph, &["zz-0001", "--evidenc", "x."], Some(A)).0,
            2
        );
        let (code, _, err) = vote(&f.graph, &["zz-0001", "-e", "   "], Some(A));
        assert_eq!((code, err.contains("evidence")), (1, true), "{err}");
        let (code, _, err) = vote(&f.graph, &["zz-0001", "-e", "hit a wall."], None);
        assert_eq!((code, err.contains("fno whoami")), (5, true), "{err}");
        let (code, _, err) = vote(&f.graph, &["zz-9999", "-e", "hit a wall."], Some(A));
        assert_eq!((code, err.contains("no node resolves")), (1, true), "{err}");
        assert!(encounters(&f.graph).is_empty());
    }

    #[test]
    fn one_session_votes_once_and_a_second_session_votes_freely() {
        let f = fixture(true);
        let (code, out, _) = vote(&f.graph, &["zz-0001", "-e", "cost me a rerun."], Some(A));
        assert_eq!(code, 0);
        assert!(out.contains("session aaaaaaaa, 1 total"), "{out}");
        let (code, _, err) = vote(&f.graph, &["slug-zz-0001", "-e", "again."], Some(A));
        assert_eq!(code, 3);
        assert!(err.contains("fno backlog note"), "{err}");
        assert_eq!(vote(&f.graph, &["zz-0001", "-e", "me too."], Some(B)).0, 0);
        assert_eq!(encounters(&f.graph).len(), 2);
    }

    #[test]
    fn a_vote_is_readable_back_to_a_transcript_and_the_json_is_the_record() {
        let f = fixture(true);
        let argv = ["zz-0001", "-e", "cost me a rerun.", "--json"];
        let (code, out, _) = vote(&f.graph, &argv, Some(A));
        assert_eq!(code, 0);
        let receipt: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            (receipt["id"].clone(), receipt["total"].clone()),
            (json!("zz-0001"), json!(1))
        );
        let stored = &encounters(&f.graph)[0];
        for key in [
            "session_id",
            "voter_key",
            "voter_kind",
            "harness",
            "fno_id",
            "evidence",
        ] {
            assert_eq!(stored[key], receipt["encounter"][key], "{key}");
        }
        assert_eq!(
            (stored["voter_kind"].clone(), stored["fno_id"].clone()),
            (json!("agent"), json!("aaaaaaaa"))
        );
    }

    #[test]
    fn the_operator_lane_needs_no_identity_and_dedupes_on_its_key() {
        let f = fixture(true);
        let (code, out, _) = vote(
            &f.graph,
            &["zz-0001", "--operator", "-e", "the seam."],
            None,
        );
        assert_eq!(code, 0);
        assert!(out.contains("(operator, 1 total)"), "{out}");
        let stored = encounters(&f.graph);
        let keys: Vec<&String> = stored[0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["created_at", "evidence", "voter_key", "voter_kind"]);
        let (code, _, err) = vote(
            &f.graph,
            &["zz-0001", "--operator", "-e", "again."],
            Some(B),
        );
        assert_eq!(code, 3);
        assert!(err.contains("voter operator already recorded"), "{err}");
    }

    #[test]
    fn over_length_evidence_is_refused_and_the_kill_switch_reaches_it() {
        let f = fixture(false);
        let long = "word word word word word. ".repeat(24);
        let (code, _, err) = vote(&f.graph, &["zz-0001", "-e", &long], Some(A));
        assert_eq!(code, 4, "{err}");
        assert!(err.contains("120"), "{err}");
        assert!(encounters(&f.graph).is_empty());
        let _off = EnvVarGuard::set("FNO_STYLE_ENFORCE", "0");
        // The kill switch never escapes the identity refusal.
        assert_eq!(vote(&f.graph, &["zz-0001", "-e", &long], None).0, 5);
        assert_eq!(vote(&f.graph, &["zz-0001", "-e", &long], Some(A)).0, 0);
    }

    #[test]
    fn provenance_is_claude_only_and_needs_a_gateway_for_the_model() {
        let _f = fixture(true);
        let _effort = EnvVarGuard::set("CLAUDE_EFFORT", "high");
        let _model = EnvVarGuard::set("ANTHROPIC_MODEL", "glm-5");
        let gateway = EnvVarGuard::set("ANTHROPIC_BASE_URL", "");
        assert_eq!(
            Value::Object(vote_provenance(Some("claude"))),
            json!({"effort": "high"})
        );
        assert!(vote_provenance(Some("codex")).is_empty());
        drop(gateway);
        let _gateway = EnvVarGuard::set("ANTHROPIC_BASE_URL", "https://gw.example");
        assert_eq!(vote_provenance(Some("claude"))["model"], "glm-5");
    }

    #[test]
    fn a_friction_vote_carries_the_offer_and_an_unknown_one_refuses() {
        let f = fixture(true);
        let home = f._dir.path().join("agents");
        let _home = EnvVarGuard::set("FNO_AGENTS_HOME", home.to_str().unwrap());
        std::fs::create_dir_all(home.join("friction")).unwrap();
        let ledger = json!({"offers": [{"fingerprint": "0123456789ab", "node": "zz-0009",
                             "stage": "execute", "candidates": ["zz-0001"]}], "seen": []});
        let path = home.join("friction").join(format!("{A}.json"));
        std::fs::write(path, ledger.to_string()).unwrap();
        let unknown = ["zz-0001", "-e", "x.", "--friction", "ffffffffffff"];
        let (code, _, err) = vote(&f.graph, &unknown, Some(A));
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("ffffffffffff"), "{err}");
        assert!(encounters(&f.graph).is_empty());
        let argv = [
            "zz-0001",
            "-e",
            "the keeper lock.",
            "--friction",
            "0123456789ab",
        ];
        assert_eq!(vote(&f.graph, &argv, Some(A)).0, 0);
        let stored = &encounters(&f.graph)[0];
        assert_eq!(stored["working_node"], "zz-0009");
        assert_eq!(stored["stage"], "execute");
        assert_eq!(stored["friction"], "0123456789ab");
    }
}

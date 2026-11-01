//! The disposition-complete pass condition, Rust gate side.
//!
//! Locked Decision 1: a head is covered when the chain tiles AND every
//! finding in that chain is TERMINAL. Locked Decision 6: the gate re-derives
//! blocking from the per-finding primitives with its own copy of the rule and
//! never trusts the producer's `findings_blocking` count.
//!
//! The chain itself is collected by [`crate::loopcheck::in_scope_chain`]; the
//! predicates here answer one question: which blocking findings in that chain
//! are non-terminal?

use serde_json::Value;
use std::collections::HashSet;

/// The gate re-derives blocking from the primitive's own fields with the
/// ONE shipped allowlist, shared with the producer side
/// (`review_findings::DEFAULT_NONBLOCKING_CATEGORIES`); the old
/// Python-side twin and this module's own copy both went with the port.
/// It still never reads the config key: a configured extension widens the
/// producer, not the gate.
fn gate_finding_blocks(primitive: &Value) -> bool {
    if primitive
        .get("has_required_fields")
        .and_then(|v| v.as_bool())
        != Some(true)
    {
        return true;
    }
    let verdict = primitive
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if verdict.trim().eq_ignore_ascii_case("confirmed") {
        return true;
    }
    let category = primitive
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let lowered = category.trim().to_lowercase();
    !crate::review_findings::DEFAULT_NONBLOCKING_CATEGORIES.contains(&lowered.as_str())
}

/// Why the gate still refuses a finding whose disposition row EXISTS: the
/// post itself is the defect, and the refusal names the fix so one retry is
/// enough (PR 4260 burned verify-fixes retries on each of these).
#[derive(Debug, Clone, PartialEq)]
pub enum Diagnosis {
    /// The disposition was posted under a key missing the raised key's
    /// final `:category` segment, so the lookup missed and the finding
    /// still reads open. `posted` is the key the row actually carries.
    KeyMissingCategory { posted: String },
    /// A later unstamped row's disposition entry replaced an earlier
    /// `verify_fixes`-stamped one for this key, so a fixed disposition
    /// lost its scoped-verification witness.
    UnstampedOverwroteStamped,
}

impl Diagnosis {
    /// The refusal clause that names the exact fix. Rendered by the
    /// coverage gate's composer; kept beside the derivation it diagnoses.
    pub fn clause(&self, finding_key: &str) -> String {
        match self {
            Diagnosis::KeyMissingCategory { posted } => format!(
                "the disposition for {finding_key} was posted as '{posted}', \
                 missing the ':category' suffix; re-post it under the exact \
                 raised key"
            ),
            Diagnosis::UnstampedOverwroteStamped => format!(
                "the fixed disposition for {finding_key} rides an unstamped \
                 row that overwrote the stamped one; re-run the review pass \
                 with --verify-fixes so the disposing row carries the stamp"
            ),
        }
    }
}

/// One non-terminal blocking finding, named by its finding_key plus the axis
/// that failed, for the block message and the emitted row.
#[derive(Debug, Clone, PartialEq)]
pub struct DispositionBlocker {
    pub finding_key: String,
    /// "open", "fixed-unreviewed", "declined-without-reason", or
    /// "truncated-remainder".
    pub axis: &'static str,
    /// A CONFIRMED correctness or security finding (or the truncated
    /// remainder, which cannot be inspected). Recorded on the receipt and
    /// gates nothing: at the configured rounds the budget discharges every
    /// open finding, hard included.
    pub hard: bool,
    /// Set when the disposition row for this key is itself the defect;
    /// `None` on the plain open finding, which the standing fix-delta
    /// remedy already covers.
    pub diagnosis: Option<Diagnosis>,
}

/// The two categories a finding the reviewer CONFIRMED as a correctness or
/// security defect can carry. Recorded on the blocker for the receipt; it
/// reads the same primitive fields `gate_finding_blocks` re-derives from,
/// never the producer's count.
const HARD_CATEGORIES: &[&str] = &["correctness", "security"];

fn hard_finding(primitive: &Value) -> bool {
    let verdict = primitive
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !verdict.trim().eq_ignore_ascii_case("confirmed") {
        return false;
    }
    let category = primitive
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_lowercase();
    HARD_CATEGORIES.contains(&category.as_str())
}

/// Whether the blockers still withhold `reviewed`: any open finding
/// withholds below the cap, and at the cap nothing does - the configured
/// rounds are the whole review gate, so the PR merges on green CI.
pub fn blockers_withhold(blockers: &[DispositionBlocker], rounds_exhausted: bool) -> bool {
    !rounds_exhausted && !blockers.is_empty()
}

/// The head an attestation row pinned, for the delta-witness read.
fn row_head(row: &Value) -> &str {
    row.pointer("/data/head_sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
}

/// The non-terminal blocking findings in an already-collected attestation
/// chain (the caller that parsed the events once passes the same chain to
/// every predicate).
///
/// `verify_invocations` names the invocation ids whose `review_invocation`
/// row carried `--verify-fixes`. A finding disposed `fixed` by one of those
/// rows is terminal at the disposing round's own head: a scoped
/// fix-verification re-read the finding's site there, so no later commit is
/// owed - and when the fix landed before the first attested head, the fix
/// delta is empty and no later head can ever exist to witness it. The
/// disposing row's own `data.verify_fixes` stamp (written by the attest
/// emitter from the same resolved flags its empty-diff carve-out reads) is a
/// second, join-free witness for the same acceptance: the invocation join
/// dies when the review-hold claim expires before the attest read it, and
/// the lost join must not strand a scoped verification the attest side
/// already accepted. A caller that holds only the chain passes an empty set
/// and keeps the head-move requirement: fail closed.
///
/// Terminal means: fixed (and a LATER round reviewed a head the finding's
/// last raise did not sit on, or the disposing round carried --verify-fixes),
/// non-blocking by the gate's own re-derivation, or declined WITH a recorded
/// reason. Origin never gates: whoever attested
/// the disposition, the terminality is the same. Pure: no IO. An empty chain
/// has no findings and blocks nothing.
pub fn disposition_blockers_on_chain(
    chain: &[Value],
    verify_invocations: &HashSet<String>,
) -> Vec<DispositionBlocker> {
    if chain.is_empty() {
        return Vec::new();
    }

    let mut findings: Vec<(String, &Value, usize, String)> = Vec::new(); // key, primitive, raised round, raised head
                                                                         // key -> (disposition, reason, disposing row's invocation_id, row stamped
                                                                         // verify_fixes, an unstamped row replaced a stamped entry for this key)
    let mut dispositions: std::collections::HashMap<
        String,
        (&str, &str, Option<&str>, bool, bool),
    > = std::collections::HashMap::new();
    let mut truncated = false;
    for (index, val) in chain.iter().enumerate() {
        if val
            .pointer("/data/findings_truncated")
            .and_then(|v| v.as_bool())
            == Some(true)
        {
            truncated = true;
        }
        let head = row_head(val).to_string();
        for primitive in val
            .pointer("/data/findings")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            if let Some(key) = primitive.get("finding_key").and_then(|v| v.as_str()) {
                if !key.is_empty() {
                    if let Some(slot) = findings.iter_mut().find(|(k, _, _, _)| k == key) {
                        slot.1 = primitive;
                        slot.2 = index;
                        slot.3 = head.clone();
                    } else {
                        findings.push((key.to_string(), primitive, index, head.clone()));
                    }
                }
            }
        }
        let row_stamped = val.pointer("/data/verify_fixes").and_then(|v| v.as_bool()) == Some(true);
        for entry in val
            .pointer("/data/dispositions")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            let key = entry
                .get("finding_key")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let disposition = entry
                .get("disposition")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let reason = entry.get("reason").and_then(|v| v.as_str()).unwrap_or("");
            let invocation = val
                .pointer("/data/invocation_id")
                .and_then(|v| v.as_str())
                .filter(|id| !id.is_empty() && *id != "UNJOINED");
            if !key.is_empty() {
                let overwrote = match dispositions.get(key) {
                    Some((_, _, _, prev_stamped, prev_overwrote)) => {
                        !row_stamped && (*prev_stamped || *prev_overwrote)
                    }
                    None => false,
                };
                dispositions.insert(
                    key.to_string(),
                    (disposition, reason, invocation, row_stamped, overwrote),
                );
            }
        }
    }

    let mut blockers: Vec<DispositionBlocker> = Vec::new();
    if truncated {
        blockers.push(DispositionBlocker {
            finding_key: "(truncated remainder)".to_string(),
            axis: "truncated-remainder",
            hard: true,
            diagnosis: None,
        });
    }
    for (key, primitive, raised, raised_head) in &findings {
        if !gate_finding_blocks(primitive) {
            continue; // non-blocking by class: no action clears the gate
        }
        match dispositions.get(key) {
            None => {
                // A row posted under a key that misses only the raised key's
                // final ':category' segment is the retry-loop defect: the
                // worker's fix landed but the gate can never match it.
                let diagnosis = key.rsplit_once(':').and_then(|(prefix, _)| {
                    dispositions
                        .contains_key(prefix)
                        .then(|| Diagnosis::KeyMissingCategory {
                            posted: prefix.to_string(),
                        })
                });
                blockers.push(DispositionBlocker {
                    finding_key: key.clone(),
                    axis: "open",
                    hard: hard_finding(primitive),
                    diagnosis,
                })
            }
            Some(("fixed", _, disposed_by, row_stamped, overwrote)) => {
                // Terminal when a LATER round reviewed the fix delta, or the
                // disposing round itself carried --verify-fixes: the scoped
                // fix-verification re-read the finding's site at the head it
                // attested, which is the witness when the fix delta is empty
                // (a fix that landed before the first attested head can never
                // produce a later head to review). The delta witness reads the
                // rows AFTER the last raise, not the disposition entry: the
                // disposition rides one event, so a last-wins copy of its
                // head pinned the gate to a stale same-head dispose forever
                // when the real fix's round re-emitted nothing. The row stamp
                // (`data.verify_fixes`, written by emit-attestation from the
                // same resolved flags its empty-diff carve-out reads) is the
                // second witness: it survives the join being
                // lost when the review-hold claim expired before the attest
                // read it - the attestation carries its own scoped-verify
                // declaration, so the join is not the only path.
                let verify_witnessed = *row_stamped
                    || match disposed_by {
                        Some(id) => verify_invocations.contains(*id),
                        None => false,
                    };
                let delta_witnessed = verify_witnessed
                    || chain[*raised + 1..]
                        .iter()
                        .any(|row| row_head(row) != raised_head.as_str());
                if !delta_witnessed {
                    blockers.push(DispositionBlocker {
                        finding_key: key.clone(),
                        axis: "fixed-unreviewed",
                        hard: hard_finding(primitive),
                        diagnosis: (!row_stamped && *overwrote)
                            .then_some(Diagnosis::UnstampedOverwroteStamped),
                    });
                }
            }
            Some(("declined", reason, _, _, _)) => {
                if reason.trim().is_empty() {
                    blockers.push(DispositionBlocker {
                        finding_key: key.clone(),
                        axis: "declined-without-reason",
                        hard: hard_finding(primitive),
                        diagnosis: None,
                    });
                }
            }
            // A "nonblocking" disposition over a gate-blocking finding: the
            // gate's re-derivation wins (Locked Decision 6).
            Some(_) => blockers.push(DispositionBlocker {
                finding_key: key.clone(),
                axis: "open",
                hard: hard_finding(primitive),
                diagnosis: None,
            }),
        }
    }
    blockers
}

#[cfg(test)]
mod tests {
    use super::{blockers_withhold, disposition_blockers_on_chain, Diagnosis};
    use std::collections::HashSet;

    #[test]
    fn same_head_rerun_never_clears_a_fixed_finding() {
        // A fixed disposition is terminal only when a later round attested a
        // head the finding's last raise did not sit on. A same-head re-run is
        // one more row under the same branch invocation with no new commit;
        // keyed on the round index alone, an author clears a CONFIRMED
        // finding by re-attesting the head that raised it.
        use serde_json::json;
        let finding = json!({
            "finding_key": "f.py:1:correctness",
            "category": "correctness",
            "verdict": "CONFIRMED",
            "has_required_fields": true,
        });
        let row = |head: &str,
                   verdict: &str,
                   findings: Vec<serde_json::Value>,
                   dispositions: Vec<serde_json::Value>| {
            json!({
                "type": "review_attestation",
                "data": {
                    "head_sha": head,
                    "verdict": verdict,
                    "branch": "feature/x",
                    "findings": findings,
                    "dispositions": dispositions,
                }
            })
        };
        let fixed_disp = json!({
            "finding_key": "f.py:1:correctness",
            "disposition": "fixed",
            "reason": "re-attested at the same head",
        });
        // Same head, two rounds: the clearance must NOT read terminal.
        let chain = vec![
            row("h1", "fail", vec![finding.clone()], vec![]),
            row("h1", "pass", vec![], vec![fixed_disp.clone()]),
        ];
        let blockers = disposition_blockers_on_chain(&chain, &HashSet::new());
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].axis, "fixed-unreviewed");

        // A different head on the disposing round reviews the fix delta.
        let chain = vec![
            row("h1", "fail", vec![finding], vec![]),
            row("h2", "pass", vec![], vec![fixed_disp]),
        ];
        assert!(disposition_blockers_on_chain(&chain, &HashSet::new()).is_empty());
    }

    #[test]
    fn a_verify_fixes_disposition_is_terminal_at_the_head_it_reviewed() {
        // The fix landed before the first attested head: round 1 raises at
        // h1 with the fix already in it, and the --verify-fixes round
        // re-attests h1 recording the fixed disposition. The fix delta is
        // empty, so no later row can ever sit on a different head; the
        // scoped verification itself is the witness. The same chain with no
        // verify invocation in evidence stays blocked (fail closed).
        use serde_json::json;
        let finding = json!({
            "finding_key": "s.rs:82:security",
            "category": "security",
            "verdict": null,
            "has_required_fields": true,
        });
        let row = |head: &str,
                   verdict: &str,
                   invocation: &str,
                   findings: Vec<serde_json::Value>,
                   dispositions: Vec<serde_json::Value>| {
            json!({
                "type": "review_attestation",
                "data": {
                    "head_sha": head,
                    "verdict": verdict,
                    "branch": "feature/x",
                    "invocation_id": invocation,
                    "findings": findings,
                    "dispositions": dispositions,
                }
            })
        };
        let fixed_disp = json!({
            "finding_key": "s.rs:82:security",
            "disposition": "fixed",
            "reason": "verified at h1 by the scoped pass",
        });
        let chain = vec![
            row("h1", "fail", "v1", vec![finding], vec![]),
            row("h1", "pass", "v2", vec![], vec![fixed_disp]),
        ];
        let mut verify = HashSet::new();
        verify.insert("v2".to_string());
        assert!(disposition_blockers_on_chain(&chain, &verify).is_empty());
        assert_eq!(
            disposition_blockers_on_chain(&chain, &HashSet::new()).len(),
            1
        );
    }

    #[test]
    fn a_stamped_same_head_row_clears_without_a_join() {
        // The attest emitter stamps data.verify_fixes on the row whose
        // dispositions came from a scoped --verify-fixes round - the same
        // evidence the empty-diff carve-out accepts. The raise and the fix
        // share one head and the
        // invocation join is lost (UNJOINED: the review-hold claim expired
        // before the attest read it), yet the scoped verification is the
        // witness - the same acceptance the attest side already gives.
        // Fail closed: the identical chain without the stamp still blocks.
        use serde_json::json;
        let finding = json!({
            "finding_key": "f.py:1:correctness",
            "category": "correctness",
            "verdict": "CONFIRMED",
            "has_required_fields": true,
        });
        let row = |head: &str,
                   verdict: &str,
                   stamped: bool,
                   findings: Vec<serde_json::Value>,
                   dispositions: Vec<serde_json::Value>| {
            let mut data = json!({
                "head_sha": head,
                "verdict": verdict,
                "branch": "feature/x",
                "invocation_id": "UNJOINED",
                "findings": findings,
                "dispositions": dispositions,
            });
            if stamped {
                data.as_object_mut()
                    .unwrap()
                    .insert("verify_fixes".into(), json!(true));
            }
            json!({"type": "review_attestation", "data": data})
        };
        let fixed_disp = json!({
            "finding_key": "f.py:1:correctness",
            "disposition": "fixed",
            "reason": "verified in place by the scoped pass",
        });
        let stamped_chain = vec![
            row("h1", "fail", false, vec![finding.clone()], vec![]),
            row("h1", "pass", true, vec![], vec![fixed_disp.clone()]),
        ];
        assert!(disposition_blockers_on_chain(&stamped_chain, &HashSet::new()).is_empty());

        let unstamped_chain = vec![
            row("h1", "fail", false, vec![finding], vec![]),
            row("h1", "pass", false, vec![], vec![fixed_disp]),
        ];
        let blockers = disposition_blockers_on_chain(&unstamped_chain, &HashSet::new());
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].axis, "fixed-unreviewed");
    }

    #[test]
    fn a_declined_finding_with_a_reason_is_terminal() {
        // Origin never gates: the author's own decline with a recorded
        // reason reads exactly like a second session's.
        use serde_json::json;
        let finding = json!({
            "finding_key": "s.rs:1:security",
            "category": "security",
            "verdict": null,
            "blocking": true,
            "has_required_fields": true,
        });
        let chain = vec![json!({"type": "review_attestation", "data": {
            "head_sha": "h1", "verdict": "fail", "branch": "feature/x",
            "findings": [finding],
            "dispositions": [{"finding_key": "s.rs:1:security",
                              "disposition": "declined",
                              "reason": "not applicable here"}],
        }})];
        assert!(disposition_blockers_on_chain(&chain, &HashSet::new()).is_empty());

        // A reason-less decline stays non-terminal.
        let chain = vec![json!({"type": "review_attestation", "data": {
            "head_sha": "h1", "verdict": "fail", "branch": "feature/x",
            "findings": [finding],
            "dispositions": [{"finding_key": "s.rs:1:security",
                              "disposition": "declined",
                              "reason": "  "}],
        }})];
        assert_eq!(
            disposition_blockers_on_chain(&chain, &HashSet::new()).len(),
            1
        );
    }

    #[test]
    fn withhold_below_the_cap_any_open_at_the_cap_none() {
        // One predicate: below the configured rounds any open finding
        // withholds; at the cap nothing does, hard included.
        let open_hard = vec![super::DispositionBlocker {
            finding_key: "f.py:1:correctness".to_string(),
            axis: "open",
            hard: true,
            diagnosis: None,
        }];
        assert!(blockers_withhold(&open_hard, false));
        assert!(!blockers_withhold(&open_hard, true));
        assert!(!blockers_withhold(&[], false));
    }

    #[test]
    fn a_suffix_less_key_names_the_exact_repost_fix() {
        // The disposition was posted under the raised key minus its
        // ':category' segment: the fix landed, but the map can never match,
        // so the blocker names the posted key instead of reading open.
        use serde_json::json;
        let finding = json!({
            "finding_key": "f.py:1:correctness",
            "category": "correctness",
            "verdict": "CONFIRMED",
            "has_required_fields": true,
        });
        let row =
            |head: &str, findings: Vec<serde_json::Value>, dispositions: Vec<serde_json::Value>| {
                json!({
                    "type": "review_attestation",
                    "data": {
                        "head_sha": head,
                        "verdict": "fail",
                        "branch": "feature/x",
                        "invocation_id": "v1",
                        "findings": findings,
                        "dispositions": dispositions,
                    }
                })
            };
        let suffix_less = json!({
            "finding_key": "f.py:1",
            "disposition": "fixed",
            "reason": "the guard is gone",
        });
        let blockers = disposition_blockers_on_chain(
            &vec![row("h1", vec![finding], vec![suffix_less])],
            &HashSet::new(),
        );
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].axis, "open");
        assert_eq!(
            blockers[0].diagnosis,
            Some(Diagnosis::KeyMissingCategory {
                posted: "f.py:1".to_string(),
            })
        );
        // An unrelated posted key is no near-miss: the blocker stays plain.
        let wrong = json!({
            "finding_key": "g.rs:9:security",
            "disposition": "fixed",
            "reason": "not this finding",
        });
        let plain = disposition_blockers_on_chain(
            &vec![
                row("h1", vec![], vec![wrong]),
                row(
                    "h1",
                    vec![json!({
                        "finding_key": "f.py:1:correctness",
                        "category": "correctness",
                        "verdict": "CONFIRMED",
                        "has_required_fields": true,
                    })],
                    vec![],
                ),
            ],
            &HashSet::new(),
        );
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].diagnosis, None);
    }

    #[test]
    fn an_unstamped_overwrite_names_the_restamped_fix() {
        // The stamped row's disposition was last-wins-replaced by an
        // unstamped re-attest: the finding keeps its fixed-unreviewed axis
        // and the diagnosis names the stamp as the fix, not the code.
        use serde_json::json;
        let finding = json!({
            "finding_key": "f.py:1:correctness",
            "category": "correctness",
            "verdict": "CONFIRMED",
            "has_required_fields": true,
        });
        let row = |stamped: bool,
                   findings: Vec<serde_json::Value>,
                   dispositions: Vec<serde_json::Value>| {
            let mut data = json!({
                "head_sha": "h1",
                "verdict": "fail",
                "branch": "feature/x",
                "invocation_id": "UNJOINED",
                "findings": findings,
                "dispositions": dispositions,
            });
            if stamped {
                data.as_object_mut()
                    .unwrap()
                    .insert("verify_fixes".into(), json!(true));
            }
            json!({"type": "review_attestation", "data": data})
        };
        let fixed_disp = json!({
            "finding_key": "f.py:1:correctness",
            "disposition": "fixed",
            "reason": "verified in place by the scoped pass",
        });
        // The fix predates the first attested head, so no later row can
        // move the head: the stamp (or the invocation join) is the only
        // witness. Last-wins: the stamped row clears the gate, and an
        // unstamped re-attest at the same head overwrites the stamped
        // entry and loses the witness.
        let chain = vec![
            row(false, vec![finding.clone()], vec![fixed_disp.clone()]),
            row(true, vec![], vec![fixed_disp.clone()]),
        ];
        assert!(
            disposition_blockers_on_chain(&chain, &HashSet::new()).is_empty(),
            "the stamped row wins the map: the witness survives"
        );
        let overwritten = vec![
            row(false, vec![finding], vec![]),
            row(true, vec![], vec![fixed_disp.clone()]),
            row(false, vec![], vec![fixed_disp]),
        ];
        let blockers = disposition_blockers_on_chain(&overwritten, &HashSet::new());
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].axis, "fixed-unreviewed");
        assert_eq!(
            blockers[0].diagnosis,
            Some(Diagnosis::UnstampedOverwroteStamped)
        );
    }
}

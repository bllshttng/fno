//! parity-stage: characterization
//! parity-oracle: fno.delivery.reader.evaluate_plan_delivery
//!
//! Characterization harness for the native `delivery-evaluate` command. The
//! cases were driven through the Python leg before its deletion, and the
//! goldens under `tests/golden/delivery_evaluate/` are the frozen contract
//! for the sole implementation. Normal runs never invoke Python.
//!
//! Every case writes its plan and journal fixtures at run time with the same
//! bytes the capture ran, so the sha256 fact revisions in the JSON match the
//! frozen goldens byte for byte. The fixture directory folds to `<FIXTURE>`
//! in both directions, matching the shared assert_golden helper's convention.

use common::{assert_golden, Golden};
use std::fs;
use std::process::Command;
use tempfile::TempDir;

#[path = "common/mod.rs"]
mod common;

const BIN: &str = env!("CARGO_BIN_EXE_fno-agents");

/// The activated plan fixture every single-slot case evaluates against.
const PLAN_PASS: &str = "\
---
node: x-delivery
status: ready
created: 2026-08-02
completion: delivery
company_work:
  work_order:
    node_id: x-delivery
    attempt_id: attempt-1
  deliverables:
    - id: output
      kind: arbitrary-output
      work_order_id: x-delivery
      attempt_id: attempt-1
      required_evidence_ids: [artifact-ready]
  evidence:
    - id: artifact-ready
      work_order_id: x-delivery
      attempt_id: attempt-1
      subject_kind: artifact
      subject_id: artifact-1
      result: passed
---
# Plan
";

/// The same declaration without the activation key: the inactive branch.
const PLAN_INACTIVE: &str = "\
---
node: x-delivery
status: ready
created: 2026-08-02
company_work:
  work_order:
    node_id: x-delivery
    attempt_id: attempt-1
  deliverables:
    - id: output
      kind: arbitrary-output
      work_order_id: x-delivery
      attempt_id: attempt-1
      required_evidence_ids: [artifact-ready]
  evidence:
    - id: artifact-ready
      work_order_id: x-delivery
      attempt_id: attempt-1
      subject_kind: artifact
      subject_id: artifact-1
      result: passed
---
# Plan
";

/// The three-slot declaration the approval and effect cases evaluate:
/// approval, effect, and acknowledgment evidence bound to one effect.
const PLAN_TWO_SLOTS: &str = "\
---
node: x-delivery
status: ready
created: 2026-08-02
completion: delivery
company_work:
  work_order:
    node_id: x-delivery
    attempt_id: attempt-1
  deliverables:
    - id: external-send
      kind: arbitrary-output
      work_order_id: x-delivery
      attempt_id: attempt-1
      effect_id: effect-1
      required_evidence_ids: [approval-ready, effect-ready, ack-ready]
  effects:
    - id: effect-1
      work_order_id: x-delivery
      attempt_id: attempt-1
      deliverable_id: external-send
      effect_class: communication.external
      destination: email:customer@example.com
      idempotency_key: effect-key-1
      approval_id: request-digest-1
  evidence:
    - id: approval-ready
      work_order_id: x-delivery
      attempt_id: attempt-1
      subject_kind: approval
      subject_id: request-digest-1
      result: unknown
    - id: effect-ready
      work_order_id: x-delivery
      attempt_id: attempt-1
      subject_kind: effect
      subject_id: effect-1
      result: unknown
    - id: ack-ready
      work_order_id: x-delivery
      attempt_id: attempt-1
      subject_kind: acknowledgment
      subject_id: effect-1
      result: unknown
---
# Plan
";

/// The one-row journal every single-slot case starts from: one passed
/// observed fact, captured byte for byte.
const JOURNAL_PASSED: &str = "{\"ts\":\"2026-08-02T12:00:00Z\",\"type\":\"delivery_evidence_observed\",\"source\":\"target\",\"data\":{\"version\":\"delivery-evidence-fact.v1\",\"evidence\":{\"id\":\"artifact-ready\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"subject_kind\":\"artifact\",\"subject_id\":\"artifact-1\",\"result\":\"passed\"},\"producer\":\"adapter:test\",\"observed_at\":\"2026-08-02T12:00:00Z\",\"source_revision\":\"artifact-sha\",\"fresh_until\":\"2099-08-02T12:05:00Z\",\"adapter_version\":\"test.v1\",\"fact_revision\":\"ignored-input-revision\"}}\n";

const JOURNAL_FAILED: &str = "{\"ts\":\"2026-08-02T12:00:00Z\",\"type\":\"delivery_evidence_observed\",\"source\":\"target\",\"data\":{\"version\":\"delivery-evidence-fact.v1\",\"evidence\":{\"id\":\"artifact-ready\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"subject_kind\":\"artifact\",\"subject_id\":\"artifact-1\",\"result\":\"failed\"},\"producer\":\"adapter:test\",\"observed_at\":\"2026-08-02T12:00:00Z\",\"source_revision\":\"artifact-sha\",\"fresh_until\":\"2099-08-02T12:05:00Z\",\"adapter_version\":\"test.v1\",\"fact_revision\":\"ignored-input-revision\"}}\n";

const JOURNAL_NON_TARGET: &str = "{\"ts\":\"2026-08-02T12:00:00Z\",\"type\":\"delivery_evidence_observed\",\"source\":\"approvals\",\"data\":{\"version\":\"delivery-evidence-fact.v1\",\"evidence\":{\"id\":\"artifact-ready\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"subject_kind\":\"artifact\",\"subject_id\":\"artifact-1\",\"result\":\"passed\"},\"producer\":\"adapter:test\",\"observed_at\":\"2026-08-02T12:00:00Z\",\"source_revision\":\"artifact-sha\",\"fresh_until\":\"2099-08-02T12:05:00Z\",\"adapter_version\":\"test.v1\",\"fact_revision\":\"ignored-input-revision\"}}\n";

const JOURNAL_STALE: &str = "{\"ts\":\"2026-08-02T12:00:00Z\",\"type\":\"delivery_evidence_observed\",\"source\":\"target\",\"data\":{\"version\":\"delivery-evidence-fact.v1\",\"evidence\":{\"id\":\"artifact-ready\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"subject_kind\":\"artifact\",\"subject_id\":\"artifact-1\",\"result\":\"passed\"},\"producer\":\"adapter:test\",\"observed_at\":\"2026-08-02T12:00:00Z\",\"source_revision\":\"artifact-sha\",\"fresh_until\":\"2026-08-02T12:00:01Z\",\"adapter_version\":\"test.v1\",\"fact_revision\":\"ignored-input-revision\"}}\n";

const JOURNAL_UNFRESH: &str = "{\"ts\":\"2026-08-02T12:00:00Z\",\"type\":\"delivery_evidence_observed\",\"source\":\"target\",\"data\":{\"version\":\"delivery-evidence-fact.v1\",\"evidence\":{\"id\":\"artifact-ready\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"subject_kind\":\"artifact\",\"subject_id\":\"artifact-1\",\"result\":\"passed\"},\"producer\":\"adapter:test\",\"observed_at\":\"2026-08-02T12:00:00Z\",\"source_revision\":\"artifact-sha\",\"fresh_until\":\"2020-01-01T00:00:00Z\",\"adapter_version\":\"test.v1\",\"fact_revision\":\"ignored-input-revision\"}}\n";

const JOURNAL_FUTURE: &str = "{\"ts\":\"2099-01-01T00:00:00Z\",\"type\":\"delivery_evidence_observed\",\"source\":\"target\",\"data\":{\"version\":\"delivery-evidence-fact.v1\",\"evidence\":{\"id\":\"artifact-ready\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"subject_kind\":\"artifact\",\"subject_id\":\"artifact-1\",\"result\":\"passed\"},\"producer\":\"adapter:test\",\"observed_at\":\"2099-01-01T00:00:00Z\",\"source_revision\":\"artifact-sha\",\"fresh_until\":\"2099-01-01T01:00:00Z\",\"adapter_version\":\"test.v1\",\"fact_revision\":\"ignored-input-revision\"}}\n";

const JOURNAL_REQUEST: &str = "{\"ts\":\"2026-08-02T12:00:00Z\",\"type\":\"approval_requested\",\"source\":\"approvals\",\"data\":{\"request_digest\":\"request-digest-1\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"effect_id\":\"effect-1\",\"request_id\":\"request-1\",\"principal_id\":\"principal-1\",\"effect_class\":\"communication.external\",\"destination\":\"email:customer@example.com\",\"action_digest\":\"action-digest-1\",\"expires_at\":\"2099-08-03T12:00:00Z\",\"event_id\":\"event-request-1\"}}\n";

const JOURNAL_DECISION: &str = "{\"ts\":\"2026-08-02T12:01:00Z\",\"type\":\"approval_decided\",\"source\":\"approvals\",\"data\":{\"request_digest\":\"request-digest-1\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"effect_id\":\"effect-1\",\"decision\":\"approved\",\"deciding_principal_id\":\"principal-1\",\"event_id\":\"event-decision-1\"}}\n";

const JOURNAL_EFFECT_EXECUTING: &str = "{\"ts\":\"2026-08-02T12:02:00Z\",\"type\":\"effect_state_changed\",\"source\":\"approvals\",\"data\":{\"request_digest\":\"request-digest-1\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"effect_id\":\"effect-1\",\"idempotency_key\":\"effect-key-1\",\"state\":\"executing\",\"previous_state\":\"prepared\",\"event_id\":\"event-effect-1\"}}\n";

const JOURNAL_EFFECT_ACK: &str = "{\"ts\":\"2026-08-02T12:03:00Z\",\"type\":\"effect_state_changed\",\"source\":\"approvals\",\"data\":{\"request_digest\":\"request-digest-1\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"effect_id\":\"effect-1\",\"idempotency_key\":\"effect-key-1\",\"state\":\"acknowledged\",\"previous_state\":\"executing\",\"external_ref\":\"message-42\",\"event_id\":\"event-effect-2\"}}\n";

const JOURNAL_DECISION_DUPLICATE: &str = "{\"ts\":\"2026-08-02T12:01:00Z\",\"type\":\"approval_decided\",\"source\":\"approvals\",\"data\":{\"request_digest\":\"request-digest-1\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"effect_id\":\"effect-1\",\"decision\":\"approved\",\"deciding_principal_id\":\"principal-1\",\"event_id\":\"event-decision-1\"}}\n";

const JOURNAL_DECISION_CONFLICT: &str = "{\"ts\":\"2026-08-02T12:06:00Z\",\"type\":\"approval_decided\",\"source\":\"approvals\",\"data\":{\"request_digest\":\"request-digest-1\",\"work_order_id\":\"x-delivery\",\"attempt_id\":\"attempt-1\",\"effect_id\":\"effect-1\",\"decision\":\"declined\",\"deciding_principal_id\":\"principal-1\",\"event_id\":\"event-decision-1\"}}\n";

/// One frozen command invocation: identical fixtures, argv shape, and stream
/// normalization as the capture that wrote the goldens.
fn run_case(plan: Option<&str>, journal: Option<&str>, json_flag: bool) -> (i32, String, String) {
    let tmp = TempDir::new().expect("temp dir");
    let fixture = fs::canonicalize(tmp.path()).expect("canonical fixture dir");
    let plan_path = fixture.join("plan.md");
    let events_path = fixture.join("events.jsonl");
    if let Some(plan) = plan {
        fs::write(&plan_path, plan).expect("write plan");
    }
    if let Some(journal) = journal {
        fs::write(&events_path, journal).expect("write journal");
    }
    let mut command = Command::new(BIN);
    command
        .arg("delivery-evaluate")
        .arg("--plan-path")
        .arg(&plan_path)
        .arg("--events")
        .arg(&events_path);
    if json_flag {
        command.arg("--json");
    }
    let output = command.output().expect("run delivery-evaluate");
    // The fixture dir is the only run-specific token in the streams: the
    // ENOENT diagnostics spell the paths the command was handed.
    let normalize = |text: String| -> String {
        text.replace(fixture.to_str().expect("utf8 fixture path"), "<FIXTURE>")
    };
    (
        output.status.code().expect("exit code"),
        normalize(String::from_utf8_lossy(&output.stdout).into_owned()),
        normalize(String::from_utf8_lossy(&output.stderr).into_owned()),
    )
}

fn golden_for(label: &str, result: (i32, String, String)) {
    let (exit, out, err) = result;
    assert_golden(
        "delivery_evaluate",
        label,
        &Golden {
            exit: Some(exit),
            streams: vec![out, err],
        },
        None,
    );
}

#[test]
fn delivery_command_matches_frozen_goldens() {
    // The passing snapshot: the full evaluated response byte order, both
    // revisions, and the verdict row's provenance columns.
    golden_for(
        "passed_snapshot_json",
        run_case(Some(PLAN_PASS), Some(JOURNAL_PASSED), true),
    );
    // A failed runtime read: aggregate failed, the row failed, no
    // diagnostics.
    golden_for(
        "failing_requirement_json",
        run_case(Some(PLAN_PASS), Some(JOURNAL_FAILED), true),
    );
    // An empty journal: the human output form names the missing fact.
    golden_for(
        "missing_fact_human",
        run_case(Some(PLAN_PASS), Some(""), false),
    );
    // A declaration without the activation key: the inactive response.
    golden_for(
        "inactive_plan_json",
        run_case(Some(PLAN_INACTIVE), Some(JOURNAL_PASSED), true),
    );
    // The unreadable-plan error path, message spelled like the Python leg.
    golden_for(
        "missing_plan_path_json",
        run_case(None, Some(JOURNAL_PASSED), true),
    );
    // A journal line that is not valid JSON is undeterminable, not a crash.
    golden_for(
        "malformed_journal_json",
        run_case(
            Some(PLAN_PASS),
            Some("{\"type\":\"delivery_evidence_observed\"\n"),
            true,
        ),
    );
    // A missing journal is the same unreadable error shape.
    golden_for(
        "missing_events_path_json",
        run_case(Some(PLAN_PASS), None, true),
    );
    // A producer read from the wrong source rejects its requirement with the
    // source named.
    golden_for(
        "non_target_source_json",
        run_case(Some(PLAN_PASS), Some(JOURNAL_NON_TARGET), true),
    );
    // A fact whose freshness window closed while its observed_at stands:
    // stale binding, spelled through the Python isoformat form.
    golden_for(
        "stale_fact_human",
        run_case(Some(PLAN_PASS), Some(JOURNAL_STALE), false),
    );
    // A fact whose fresh_until precedes its observed_at fails the fact
    // contract itself: malformed observed event, not a stale binding.
    golden_for(
        "unfresh_interval_json",
        run_case(Some(PLAN_PASS), Some(JOURNAL_UNFRESH), true),
    );
    // A future-stamped fact: the future-observation binding diagnostic
    // carries the Python isoformat rendering of the fixture timestamp.
    golden_for(
        "future_observation_json",
        run_case(Some(PLAN_PASS), Some(JOURNAL_FUTURE), true),
    );
    // The approval and effect adapters: request, decision, and the two
    // effect states fold onto their declared slots with provenance.
    golden_for(
        "approval_effect_flow_json",
        run_case(
            Some(PLAN_TWO_SLOTS),
            Some(&format!(
                "{JOURNAL_REQUEST}{JOURNAL_DECISION}{JOURNAL_EFFECT_EXECUTING}{JOURNAL_EFFECT_ACK}"
            )),
            true,
        ),
    );
    // A byte-identical duplicate event line (same event_id, same data) is
    // skipped, and the answer still passes.
    golden_for(
        "duplicate_event_deduped_json",
        run_case(
            Some(PLAN_TWO_SLOTS),
            Some(&format!(
                "{JOURNAL_REQUEST}{JOURNAL_DECISION}{JOURNAL_EFFECT_EXECUTING}{JOURNAL_EFFECT_ACK}{JOURNAL_DECISION_DUPLICATE}"
            )),
            true,
        ),
    );
    // The same event_id carrying different data is a producer conflict and
    // answers undeterminable.
    golden_for(
        "conflicting_event_id_json",
        run_case(
            Some(PLAN_TWO_SLOTS),
            Some(&format!(
                "{JOURNAL_REQUEST}{JOURNAL_DECISION}{JOURNAL_EFFECT_EXECUTING}{JOURNAL_EFFECT_ACK}{JOURNAL_DECISION_CONFLICT}"
            )),
            true,
        ),
    );
}

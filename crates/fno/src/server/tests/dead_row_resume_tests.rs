//! When does a registry row resume: the dead-row disposition family.
use super::*;

#[test]
fn row_resume_disposition_unmeasured_names_the_absent_reading() {
    // (x-d401, AC2-EDGE) Liveness::Unmeasured is NOT a dead backend, and
    // must not print one. The old fold returned the dead-backend refusal
    // for every non-Alive reading, so a row whose pane was live eight rows
    // down the same sideline told the operator its backend was not live.
    // An unmeasured reading names the absent reading; a positive dead
    // reading resumes.
    let base = || RegistryAgent {
        harness_session_id: Some("01a027ad".into()),
        harness: Some("codex".into()),
        name: "w".into(),
        cwd: "/w".into(),
        exited: true,
        liveness: agents_view::Liveness::Alive,
        ..Default::default()
    };
    let mut unmeasured = base();
    unmeasured.exited = false;
    unmeasured.liveness = agents_view::Liveness::Unmeasured;
    assert_eq!(
        Core::row_resume_disposition(&unmeasured),
        RowResumeDisposition::NoPane(AgentNoPaneReason::LivenessUnmeasured),
        "an unmeasured backend must read as no-reading, never as dead"
    );
    let mut dead = base();
    dead.exited = false;
    dead.liveness = agents_view::Liveness::Dead;
    assert_eq!(
        Core::row_resume_disposition(&dead),
        RowResumeDisposition::Resumable,
        "a positive dead reading resumes whatever the status word says"
    );
}

use fno_agents::run_state::{
    append_transition, fold_run_state, step, RunEvent, RunState, RunStateError,
};

#[test]
fn every_legal_transition_matches_the_plan_table() {
    let arms = [
        (
            RunState::Open,
            RunEvent::DispatchClassified,
            RunState::Working,
        ),
        (
            RunState::Working,
            RunEvent::DispatchClassified,
            RunState::Working,
        ),
        (
            RunState::Working,
            RunEvent::PrepareHandoff,
            RunState::Delegating,
        ),
        (
            RunState::Delegating,
            RunEvent::SuccessorProven,
            RunState::Closed,
        ),
        (
            RunState::Delegating,
            RunEvent::SuccessorUnproven,
            RunState::Working,
        ),
        (
            RunState::Working,
            RunEvent::TerminalDecided,
            RunState::Sealing,
        ),
        (RunState::Sealing, RunEvent::FinalizeDone, RunState::Closed),
    ];

    for (from, event, to) in arms {
        assert_eq!(step(from, event), Ok(to));
    }
    for from in [
        RunState::Open,
        RunState::Working,
        RunState::Delegating,
        RunState::Sealing,
    ] {
        assert_eq!(step(from, RunEvent::Cancel), Ok(RunState::Aborted));
        assert_eq!(step(from, RunEvent::Abort), Ok(RunState::Aborted));
    }
}

#[test]
fn corrupt_run_log_is_an_error_never_open() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("run-log.jsonl");
    std::fs::write(&log, "{not-json}\n").unwrap();
    assert!(matches!(
        fold_run_state(&log, "20260823T060900Z-cx73523-e04109"),
        Err(RunStateError::MalformedLine { line: 1, .. })
    ));
}

#[test]
fn rejected_transition_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("run-log.jsonl");
    let run = "20260823T060900Z-cx73523-e04109";

    append_transition(&log, run, RunEvent::DispatchClassified).unwrap();
    append_transition(&log, run, RunEvent::PrepareHandoff).unwrap();
    append_transition(&log, run, RunEvent::SuccessorProven).unwrap();
    let before = std::fs::read_to_string(&log).unwrap();

    assert!(matches!(
        append_transition(&log, run, RunEvent::DispatchClassified),
        Err(RunStateError::InvalidTransition(_))
    ));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), before);
}

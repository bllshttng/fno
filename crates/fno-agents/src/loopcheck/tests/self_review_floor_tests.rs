use super::super::block_reason::unattested_reviewers_reason;
use super::super::self_review_floor::{classify_payload, payload_is_code};
use super::*;

#[test]
fn reviewer_invocation_resolves_the_author_harness_verb() {
    // The owned lane is the invocation on EVERY harness: an inline lane
    // runs wherever the plugin runs, so a transport-dependent verb here
    // would be the fragile part. sigma is retired and also names the lane
    // (the refusal tells a wedged session what to run instead).
    for harness in ["codex", "claude", "opencode", "agy"] {
        assert_eq!(
            reviewer_invocation_for("code-review", Some(harness)),
            Some(("/fno:review", false)),
            "harness {harness} must name the portable lane"
        );
    }
    assert_eq!(
        reviewer_invocation_for("code-review", None),
        Some(("/fno:review", false))
    );
    assert_eq!(
        reviewer_invocation_for("sigma", Some("codex")),
        Some(("/fno:review", false))
    );
}

#[test]
fn code_payload_classifies_code_and_docs() {
    // Code: source, config, lockfile, script. Docs: markdown, docs/.
    assert!(payload_is_code(&["cli/src/fno/x.py".into()]));
    assert!(payload_is_code(&["crates/fno-agents/src/lib.rs".into()]));
    assert!(payload_is_code(&[".fno/config.toml".into()]));
    assert!(payload_is_code(&["Cargo.lock".into()]));
    assert!(payload_is_code(&["scripts/ci/gate.sh".into()]));
    assert!(!payload_is_code(&["README.md".into()]));
    assert!(!payload_is_code(&[
        "docs/architecture/review-lanes.md".into()
    ]));
    assert!(!payload_is_code(&["docs/preflight.txt".into()]));
}

#[test]
fn self_review_gate_classifies_unreadable_diff_as_code() {
    // AC4-ERR: a git that cannot produce a diff fails CLOSED - code with
    // assumed=true - so a degraded probe cannot wave the obligation away.
    let (is_code, assumed) = classify_payload("definitely-not-a-real-git-binary", Path::new("."));
    assert!(is_code);
    assert!(assumed);
}

#[test]
fn self_review_gate_fails_closed_when_main_resolves_but_diffs_fail() {
    // origin/main RESOLVES but its diff fails (unrelated histories exit
    // 128 "no merge base"): the fallback must not quietly re-base onto a
    // sibling origin/master, which can be a stale pre-migration ancestor
    // and would size the payload from a whole era. Fail closed instead.
    let dir = tempfile::tempdir().unwrap();
    let script = write_exec(
            dir.path(),
            "git",
            "#!/bin/sh\ncase \"$*\" in\n  rev-parse*origin/main*) printf 'sha\\n' ;;\n  *origin/main*) exit 1 ;;\n  rev-parse*origin/master*) printf 'sha\\n' ;;\n  *origin/master*) printf 'src/lib.rs\\n' ;;\n  *) exit 1 ;;\nesac\n",
        );
    let (is_code, assumed) = classify_payload(script.to_str().unwrap(), Path::new("."));
    assert!(is_code);
    assert!(assumed);
}

#[test]
fn floor_payload_classifies_the_named_pr_for_the_coverage_verb() {
    // The review-coverage verb evaluates a PR it names (--pr), from a
    // checkout that need not sit on its branch: the floor must classify
    // THAT PR, and a named PR that resolves to nothing fails closed
    // rather than falling back to the foreign checkout's empty diff.
    let dir = tempfile::tempdir().unwrap();
    let git = write_exec(
            dir.path(),
            "git",
            "#!/bin/sh\ncase \"$*\" in\n  rev-parse*origin/main*) printf 'sha\\n' ;;\n  *origin/main*) printf '' ;;\n  *) exit 1 ;;\nesac\n",
        );
    let gh = write_exec(
            dir.path(),
            "gh",
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'gh version 2.x' ;;\n  *view*) echo '{\"number\":7}' ;;\n  *files*) printf '[{\"filename\":\"src/x.rs\"}]' ;;\n  *) exit 1 ;;\nesac\n",
        );
    let (is_code, assumed) = classify_payload_for_floor(
        gh.to_str().unwrap(),
        git.to_str().unwrap(),
        Path::new("."),
        Some("7"),
    );
    assert!(
        is_code,
        "a named code PR must floor even from a foreign checkout"
    );
    assert!(!assumed);

    // The same gh refusing the named PR (no view answer): fail closed,
    // never the foreign checkout's empty-diff answer.
    let gh = write_exec(
            dir.path(),
            "gh-refuses",
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'gh version 2.x' ;;\n  *) echo 'no pull requests found' >&2; exit 1 ;;\nesac\n",
        );
    let (is_code, assumed) = classify_payload_for_floor(
        gh.to_str().unwrap(),
        git.to_str().unwrap(),
        Path::new("."),
        Some("7"),
    );
    assert!(is_code, "an unreadable named PR must fail closed");
    assert!(assumed);
}

#[test]
fn self_review_gate_floor_respects_opt_out_lanes_docs_and_existing() {
    // AC6-CON: opt-out -> None.
    assert_eq!(floor_self_review(&[], false, true, false), None);
    // A configured lane -> None: the lane already expresses review intent.
    assert_eq!(floor_self_review(&[], true, true, true), None);
    // A docs payload -> None: nothing to review.
    assert_eq!(floor_self_review(&[], false, false, true), None);
    // code-review already named -> None: no double-add.
    assert_eq!(
        floor_self_review(&["code-review".to_string()], false, true, true),
        None
    );
    // A leading slash on an existing entry is still recognized as present.
    assert_eq!(
        floor_self_review(&["/code-review".to_string()], false, true, true),
        None
    );
}

#[test]
fn self_review_gate_held_reason_names_code_review_and_its_verb() {
    // AC1-HP: a code payload that reaches the stop gate with no head-pinned
    // code-review attestation is held, and the reason names the reviewer
    // and the verb served by the ambient harness. Both render branches are
    // pinned through the sizing seam (`unattested_reviewers_reason` takes
    // the sized line explicitly): a working render substitutes its
    // invocation for the map value's `<level>` placeholder, and a failed
    // render keeps the placeholder (fail-open). The production caller feeds
    // the seam from the native render; the stub-script era ended with the
    // shell-out it stubbed, and the harness rides in pinned so the verb
    // form never depends on the host's ambient markers.
    let mut pr = reviewers_gate_pr();
    pr.unattested_reviewers[0].name = "code-review".to_string();

    let sized_reason = unattested_reviewers_reason(
        &pr,
        "abc",
        Some("claude"),
        Some("/code-review from-stub --comment --fix"),
    );
    assert!(
        sized_reason.contains("`/code-review from-stub --comment --fix`"),
        "got: {sized_reason}"
    );
    assert!(
        !sized_reason.contains("<level>"),
        "the placeholder must not survive a working render: {sized_reason}"
    );

    let reason = unattested_reviewers_reason(&pr, "abc", Some("claude"), None);
    let expected = reviewer_invocation_for("code-review", Some("claude"))
        .expect("code-review descriptor")
        .0;
    assert!(reason.contains("reviewers gate unmet"), "got: {reason}");
    assert!(reason.contains("code-review"), "got: {reason}");
    assert!(reason.contains(&format!("`{expected}`")), "got: {reason}");
    assert!(
        reason.contains("skills/review/scripts/emit-attestation.sh code-review"),
        "got: {reason}"
    );
    assert!(
        reason.contains("local work to DO, not a wait"),
        "got: {reason}"
    );
}

#[test]
fn unresolved_harness_floors_the_self_review_gate() {
    // `None` means UNATTRIBUTABLE, not verbless. A claude session
    // started from a codex shell resolves no single family, and reading
    // that ambiguity as "no floor" silently disengaged the only review a
    // stock install demands. Ambiguity is not permission; the explicit
    // `--author-harness none` pin stays the hermetic opt-out.
    assert!(self_review_floor_applies(None, false));
    assert!(!self_review_floor_applies(None, true));
    assert!(self_review_floor_applies(Some("claude"), false));
    assert!(self_review_floor_applies(Some("codex"), true));
    // The owned fno review lane runs wherever the plugin runs, so no
    // KNOWN harness is verbless any more: gemini/agy/opencode floor too.
    assert!(self_review_floor_applies(Some("opencode"), false));
    assert!(self_review_floor_applies(Some("gemini"), false));
    assert!(self_review_floor_applies(Some("agy"), false));
    // An unrecognized spelling is an unattributed run: it floors rather
    // than passing an unknown name through the verb table.
    assert!(self_review_floor_applies(Some("hermes"), false));
}

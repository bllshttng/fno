use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Default)]
struct Fake {
    facts: Option<PrFacts>,
    facts_error: Option<String>,
    /// Answer for the binding probe alone; `None` (the default) reads Clear.
    node_binding: Option<ProbeOutcome>,
    dispatch_hold: Option<ProbeOutcome>,
    outside_pr: Option<ProbeOutcome>,
    pr_hold: Option<ProbeOutcome>,
    merge_freeze: Option<ProbeOutcome>,
    review_posted: Option<ProbeOutcome>,
    /// The `reviewed_head` argument review_posted_gate last passed in, so a
    /// test can prove it threaded the attestation's own head, not a guess.
    review_posted_head_seen: RefCell<Option<String>>,
    review_hold: Option<ProbeOutcome>,
    lineage: Option<ProbeOutcome>,
    merge_result: Option<ProbeOutcome>,
    ci_base: Option<ProbeOutcome>,
    fresh_ci: Option<bool>,
    live_lanes: usize,
    ci_base_calls: RefCell<u32>,
    checks: Option<String>,
    /// GitHub's ruleset hold for the PR under decision; `None` (the
    /// default) reads no hold, so every pre-existing test keeps its behavior.
    github_block: Option<String>,
    /// Optional-review answer for the PR under decision; the default
    /// `Some(Some(0))` keeps every pre-existing test passing.
    optional_unresolved: Option<Option<i64>>,
    /// Rerun-recovery answer for the PR under decision.
    rerun_recovered: Option<bool>,
    /// The coverage verdict's exit for the PR under decision, armed into
    /// `coverage_verdict`; None reads 0.
    coverage_exit: Option<i32>,
    /// The plan-fidelity gate's verdict for a test-armable refusal.
    plan_fidelity_refused: bool,
    /// The `fno backlog decisions` exit and stdout the head-grant read
    /// gets. `None` exit reads 0; `None` stdout reads empty (malformed).
    decisions_exit: Option<i32>,
    decisions_stdout: Option<Vec<u8>>,
    /// Every fno-shell argv, so a test can assert the subject the grant
    /// read asked for.
    fno_calls: RefCell<Vec<Vec<String>>>,
    covered_head: Option<String>,
    /// The merge queue's answer; `None` (the default) reads no queue.
    merge_queue: Option<Result<Option<String>, String>>,
    enabled: bool,
    floor: Option<String>,
    gh_ok: bool,
    gh_output: String,
    /// Answer for the REST recovery call alone, so a test can fail the
    /// `gh pr merge` and let the retry succeed.
    gh_recovery_ok: Option<bool>,
    /// Per-endpoint answers: the first key the joined argv contains wins,
    /// ahead of `gh_output`.
    gh_routes: Vec<(String, String)>,
    gh_calls: RefCell<Vec<Vec<String>>>,
    /// Simulated merge-slot claim: `None` is free, `Some(pr)` is held.
    slot: RefCell<Option<u64>>,
    slot_holder_err: bool,
    take_slot_err: bool,
    take_slot_calls: RefCell<Vec<u64>>,
    release_slot_calls: RefCell<Vec<u64>>,
    /// Simulated merge-slot metadata: has `mark_slot_retested` fired since
    /// the slot was last taken (mirrors the real claim's `metadata`).
    slot_retested: RefCell<bool>,
    /// Facts and checks for a PR other than the one under decision, keyed
    /// by PR number - a holder read in the slot logic.
    other_facts: RefCell<HashMap<u64, PrFacts>>,
    other_checks: RefCell<HashMap<u64, String>>,
    /// Dispatch holds keyed by PR, so a test can hold a holder without
    /// holding the PR under decision.
    other_holds: RefCell<HashMap<u64, ProbeOutcome>>,
    /// The main-ci verdict the gate reads; `None` (the default) reads the
    /// `pending` word, so every pre-existing test keeps its behavior.
    main_ci: Option<Result<Value, String>>,
    /// The main-repair exemption answer; `None` (the default) reads exempt.
    main_repair: Option<Option<String>>,
}

fn open_facts() -> PrFacts {
    PrFacts {
        number: 7,
        head_sha: "abc123".to_string(),
        head_ref: "feature/x".to_string(),
        base_ref: "main".to_string(),
        url: "https://github.com/o/r/pull/7".to_string(),
        body: Some("Backlog-Closure: x-aaaa\n".to_string()),
        state: "OPEN".to_string(),
        armed: false,
    }
}

fn clean() -> Fake {
    Fake {
        facts: Some(open_facts()),
        covered_head: Some("abc123".to_string()),
        enabled: true,
        gh_ok: true,
        optional_unresolved: Some(Some(0)),
        ..Default::default()
    }
}

struct ClaimsRootRestore(Option<std::ffi::OsString>);

impl Drop for ClaimsRootRestore {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
            None => std::env::remove_var("FNO_CLAIMS_ROOT"),
        }
    }
}

fn with_claims_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    let _env_lock = claims::test_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let restore = ClaimsRootRestore(std::env::var_os("FNO_CLAIMS_ROOT"));
    std::env::set_var("FNO_CLAIMS_ROOT", root);
    let result = f();
    drop(restore);
    result
}

#[test]
fn take_slot_refuses_a_live_legacy_lockfile_without_creating_a_second_slot() {
    let temp = tempfile::TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(init.success());

    let current_root = temp.path().join("current");
    with_claims_root(&current_root, || {
        let key = slot_key("main");
        assert!(matches!(
            claims::acquire(
                &key,
                &slot_holder_key(8),
                claims::AcquireOpts {
                    root: Some(repo.clone()),
                    pid_unavailable: true,
                    ttl_ms: Some(MERGE_SLOT_TTL_MS),
                    ..Default::default()
                }
            ),
            claims::AcquireOutcome::Acquired(_)
        ));

        let error = RealProbes.take_slot(&repo, "main", 9).unwrap_err();
        assert!(error.contains("pr:8"), "{error}");
        assert!(!claims::claim_path(&key, None).unwrap().exists());
    });
}

/// code+class for each scenario: no attestation (stays silent, the gap
/// belongs to review_coverage_uncovered), an attestation with GitHub
/// lacking the post (Held, self-clearing), posted (clear), an unreadable
/// probe (Unknown, never a silent pass), and a round carried forward to a
/// new head with no fresh attestation event of its own (still found,
/// branch-wide, never silenced by an exact-head miss).
#[test]
fn review_posted_gate_covers_its_five_cases() {
    let temp = tempfile::TempDir::new().unwrap();
    with_claims_root(temp.path(), || {
        let spaces = temp.path().join("spaces");
        std::fs::create_dir_all(&spaces).unwrap();
        let saved = std::env::var("FNO_SPACES_DIR").ok();
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let facts = PrFacts {
            number: 931,
            head_sha: "h1sha".to_string(),
            head_ref: "feature/x".to_string(),
            base_ref: "main".to_string(),
            url: "https://github.com/o/r/pull/931".to_string(),
            body: None,
            state: "OPEN".to_string(),
            armed: false,
        };
        let gate = |outcome: ProbeOutcome| {
            crate::merge_gates::review_posted_gate(
                &Fake {
                    review_posted: Some(outcome),
                    ..Default::default()
                },
                &repo,
                &facts,
            )
        };

        assert_eq!(
            gate(ProbeOutcome::Refused("must not be asked".into())),
            None
        );

        let journal = crate::paths::events_path(&repo);
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(
            &journal,
            "{\"type\":\"review_attestation\",\"ts\":\"2026-10-16T10:00:00Z\",\"data\":{\"branch\":\"feature/x\",\"head_sha\":\"h1sha\",\"verdict\":\"pass\",\"reviewer\":\"code-review\"}}\n",
        )
        .unwrap();

        let blocker = gate(ProbeOutcome::Refused("not yet posted".into())).unwrap();
        assert_eq!(blocker.code, "review_not_posted");
        assert_eq!(blocker.class, BlockerClass::Held);

        assert_eq!(gate(ProbeOutcome::Clear), None);

        let blocker = gate(ProbeOutcome::Inconclusive("gh timed out".into())).unwrap();
        assert_eq!(blocker.code, "review_posted_unknown");
        assert_eq!(blocker.class, BlockerClass::Unknown);

        // A round carried forward under law d-608344c1 (rebase, interdiff
        // under the cap) never gets a fresh attestation EVENT at the new
        // head - only the original head carries one. The gate must still
        // find it (branch-wide, not exact-head) and probe for THAT head's
        // marker, never facts.head_sha.
        std::fs::write(
            &journal,
            "{\"type\":\"review_attestation\",\"ts\":\"2026-10-16T11:00:00Z\",\"data\":{\"branch\":\"feature/x\",\"head_sha\":\"h0sha-pre-rebase\",\"verdict\":\"pass\",\"reviewer\":\"code-review\"}}\n",
        )
        .unwrap();
        let fake = Fake {
            review_posted: Some(ProbeOutcome::Refused("not yet posted".into())),
            ..Default::default()
        };
        let blocker = crate::merge_gates::review_posted_gate(&fake, &repo, &facts)
            .expect("a carried-forward round with no posted review must still block");
        assert_eq!(blocker.code, "review_not_posted");
        assert_eq!(
            fake.review_posted_head_seen.borrow().as_deref(),
            Some("h0sha-pre-rebase")
        );

        match saved {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
    });
}

impl Probes for Fake {
    fn pr_facts(&self, _cwd: &Path, pr: Option<u64>) -> Result<PrFacts, String> {
        if let Some(error) = &self.facts_error {
            return Err(error.clone());
        }
        let main = self.facts.clone().ok_or_else(|| "no facts".to_string())?;
        match pr {
            Some(n) if n != main.number => self
                .other_facts
                .borrow()
                .get(&n)
                .cloned()
                .ok_or_else(|| format!("no facts for pr {n}")),
            _ => Ok(main),
        }
    }
    fn node_binding(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.node_binding.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn dispatch_hold(&self, _cwd: &Path, facts: &PrFacts) -> ProbeOutcome {
        if let Some(hold) = self.other_holds.borrow().get(&facts.number) {
            return hold.clone();
        }
        self.dispatch_hold.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn outside_pr(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.outside_pr.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn pr_hold(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.pr_hold.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn merge_freeze(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.merge_freeze.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn review_posted(&self, _cwd: &Path, _facts: &PrFacts, reviewed_head: &str) -> ProbeOutcome {
        *self.review_posted_head_seen.borrow_mut() = Some(reviewed_head.to_string());
        self.review_posted.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn review_hold(&self, _cwd: &Path, _pr: u64) -> ProbeOutcome {
        self.review_hold.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn base_lineage(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.lineage.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn merge_result(&self, _cwd: &Path, _facts: &PrFacts) -> ProbeOutcome {
        self.merge_result.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn ci_base(&self, _cwd: &Path, _facts: &PrFacts, merge_clean: bool) -> ProbeOutcome {
        *self.ci_base_calls.borrow_mut() += 1;
        if merge_clean {
            return ProbeOutcome::Clear;
        }
        self.ci_base.clone().unwrap_or(ProbeOutcome::Clear)
    }
    fn require_fresh_ci(&self, _cwd: &Path) -> bool {
        self.fresh_ci.unwrap_or(true)
    }
    fn live_lanes(&self, _cwd: &Path) -> usize {
        self.live_lanes
    }
    fn slot_holder(&self, _cwd: &Path, _base_ref: &str) -> Result<Option<u64>, String> {
        if self.slot_holder_err {
            return Err("merge slot claim corrupted".to_string());
        }
        Ok(*self.slot.borrow())
    }
    fn take_slot(&self, _cwd: &Path, _base_ref: &str, pr: u64) -> Result<(), String> {
        self.take_slot_calls.borrow_mut().push(pr);
        if self.take_slot_err {
            return Err("merge slot already held by pr:99".to_string());
        }
        *self.slot.borrow_mut() = Some(pr);
        *self.slot_retested.borrow_mut() = false;
        Ok(())
    }
    fn release_slot(&self, _cwd: &Path, _base_ref: &str, pr: u64) {
        self.release_slot_calls.borrow_mut().push(pr);
        let mut held = self.slot.borrow_mut();
        if *held == Some(pr) {
            *held = None;
            *self.slot_retested.borrow_mut() = false;
        }
    }
    fn slot_retested(&self, _cwd: &Path, _base_ref: &str, pr: u64) -> bool {
        *self.slot.borrow() == Some(pr) && *self.slot_retested.borrow()
    }
    fn mark_slot_retested(&self, _cwd: &Path, _base_ref: &str, _pr: u64) {
        *self.slot_retested.borrow_mut() = true;
    }
    fn checks_read(&self, _cwd: &Path, pr: u64) -> ChecksRead {
        let mine = self.facts.as_ref().map(|f| f.number) == Some(pr);
        let verdict = if mine {
            self.checks.clone().unwrap_or_else(|| "green".to_string())
        } else {
            self.other_checks
                .borrow()
                .get(&pr)
                .cloned()
                .unwrap_or_else(|| "green".to_string())
        };
        ChecksRead {
            verdict,
            github_block: if mine {
                self.github_block.clone()
            } else {
                None
            },
            optional_unresolved: if mine {
                self.optional_unresolved
            } else {
                Some(Some(0))
            },
            rerun_recovered: if mine { self.rerun_recovered } else { None },
            rerun_failures: None,
        }
    }
    fn covered_head(&self, _cwd: &Path) -> Option<String> {
        self.covered_head.clone()
    }
    fn auto_merge_enabled(&self, _cwd: &Path) -> bool {
        self.enabled
    }
    fn posture_floor_block(&self, _cwd: &Path) -> Option<String> {
        self.floor.clone()
    }
    fn strategy(&self, _cwd: &Path) -> String {
        "squash".to_string()
    }
    fn main_ci_token(&self, _cwd: &Path) -> Result<Value, String> {
        self.main_ci
            .clone()
            .unwrap_or_else(|| Ok(Value::String("pending".to_string())))
    }
    fn main_repair_hold(&self, _cwd: &Path, _facts: &PrFacts) -> Option<String> {
        self.main_repair.clone().flatten()
    }
    fn merge_queue(
        &self,
        _cwd: &Path,
        _facts: &PrFacts,
        _head: &str,
    ) -> Option<Result<Option<String>, String>> {
        self.merge_queue.clone()
    }
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String), String> {
        self.gh_calls.borrow_mut().push(args.to_vec());
        if args.first().map(String::as_str) == Some("api") {
            if let Some(ok) = self.gh_recovery_ok {
                return Ok((ok, String::new()));
            }
        }
        let joined = args.join(" ");
        if let Some((_, out)) = self
            .gh_routes
            .iter()
            .find(|(k, _)| joined.contains(k.as_str()))
        {
            return Ok((true, out.clone()));
        }
        Ok((self.gh_ok, self.gh_output.clone()))
    }
    fn fno_shell(
        &self,
        _cwd: &Path,
        args: &[String],
    ) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
        self.fno_calls.borrow_mut().push(args.to_vec());
        // The head-grant ask: `backlog decisions <subject> --lane law
        // --state live --json`.
        if args.len() >= 2 && args[0] == "backlog" && args[1] == "decisions" {
            return Ok((
                self.decisions_exit.or(Some(0)),
                self.decisions_stdout.clone().unwrap_or_default(),
                Vec::new(),
            ));
        }
        // The only other fno-shell ask in these tests is the plan-fidelity
        // probe: the verdict answer reads `refused` from the test arm.
        let covered = if self.plan_fidelity_refused {
            br#"{"refused": true, "reason": "test"}"#.to_vec()
        } else {
            br#"{"refused": false}"#.to_vec()
        };
        Ok((Some(0), covered, Vec::new()))
    }

    fn coverage_verdict(
        &self,
        _cwd: &Path,
        _pr: u64,
    ) -> crate::loopcheck::coverage_gate::GateVerdict {
        use crate::loopcheck::coverage_gate::{GateState, GateVerdict};
        let (state, refusal, note) = match self.coverage_exit {
            None | Some(0) => (GateState::Covered, "", ""),
            Some(3) => (
                GateState::Refused,
                "no review coverage row at this head",
                "",
            ),
            _ => (GateState::Unanswered, "", "coverage read unavailable"),
        };
        GateVerdict {
            state,
            refusal: refusal.to_string(),
            covered_head: String::new(),
            note: note.to_string(),
        }
    }
}

fn request(effect: Effect) -> Request {
    Request {
        cwd: PathBuf::from("/tmp"),
        pr: Some(7),
        effect,
        approved: Some(true),
        auto_merge_source: Some("config".to_string()),
        require_checks: false,
        covered_head: None,
        decide_only: false,
        authority: None,
        accept_flake: false,
        supplied_verdict: None,
        supplied_counts: None,
        supplied_rerun_recovered: None,
        supplied_optional_unresolved: None,
        supplied_github_blockers: None,
        supplied_dispatch_hold: None,
        supplied_review_hold: None,
        supplied_facts: None,
    }
}

fn granted_decisions() -> Vec<u8> {
    br#"{"decisions":[{"authority_source":"operator","decision":"merge authorized for this head"}]}"#
        .to_vec()
}

// ── the head-scoped operator merge grant ──────────────────────────────

#[test]
fn a_head_grant_clears_the_per_run_no_merge_and_the_receipt_names_it() {
    // AC2-HP.
    let mut req = request(Effect::Merge);
    req.approved = Some(false);
    let fake = Fake {
        decisions_stdout: Some(granted_decisions()),
        ..clean()
    };
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "merged", "{}", outcome.detail());
    let receipt = outcome.to_json();
    assert_eq!(
        receipt["merge_grant"],
        json!("operator head grant abc123"),
        "{}",
        receipt
    );
}

#[test]
fn an_absent_grant_refuses_and_names_only_the_attended_command() {
    // AC2-ERR: absence refuses, the remedy is the one sanctioned grant
    // command, and the out-of-band escape is gone from the refusal.
    let mut req = request(Effect::Merge);
    req.approved = Some(false);
    req.auto_merge_source = Some("flag-no-merge".to_string());
    let fake = Fake {
        decisions_stdout: Some(br#"{"decisions":[]}"#.to_vec()),
        ..clean()
    };
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "refused");
    let detail = outcome.detail();
    assert!(detail.contains("absent"), "{detail}");
    assert!(detail.contains("flag-no-merge"), "{detail}");
    assert!(detail.contains("'merge-grant:#7@abc123'"), "{detail}");
    assert!(
        detail.contains("'merge authorized for this head'"),
        "{detail}"
    );
    assert!(detail.ends_with("--authority operator"), "{detail}");
    assert!(!detail.contains("out-of-band"), "{detail}");
    assert!(fake.gh_calls.borrow().is_empty());
}

#[test]
fn an_unreadable_decisions_read_is_never_a_grant() {
    // AC2-ERR: a nonzero exit or a malformed payload fails closed as
    // unreadable; neither becomes a grant. One contract, two transports.
    let mut req = request(Effect::Merge);
    req.approved = Some(false);
    for fake in [
        Fake {
            decisions_exit: Some(1),
            ..clean()
        },
        Fake {
            decisions_stdout: Some(br#"{"error":"damaged"}"#.to_vec()),
            ..clean()
        },
    ] {
        let outcome = run(&fake, &req);
        assert_eq!(outcome.word(), "refused");
        assert!(
            outcome.detail().contains("unreadable"),
            "{}",
            outcome.detail()
        );
    }
}

#[test]
fn a_holder_with_fresh_ci_clears_and_releases_the_slot_on_merge_while_a_racer_waits() {
    // AC2-HP.
    let mut fake = Fake {
        slot: RefCell::new(Some(7)),
        facts: Some(PrFacts {
            number: 9,
            head_sha: "nine".to_string(),
            ..open_facts()
        }),
        covered_head: Some("nine".to_string()),
        ..clean()
    };
    let req9 = Request {
        require_checks: true,
        pr: Some(9),
        ..request(Effect::Merge)
    };
    let outcome9 = run(&fake, &req9);
    assert_eq!(outcome9.word(), "held");
    let detail9 = outcome9.detail();
    assert!(detail9.contains("merge_slot_held"));
    assert!(detail9.contains("PR 7"));
    assert_eq!(
        *fake.slot.borrow(),
        Some(7),
        "the waiting PR must not take the slot"
    );

    fake.facts = Some(PrFacts {
        number: 7,
        head_sha: "abc123".to_string(),
        ..open_facts()
    });
    fake.covered_head = Some("abc123".to_string());
    let req7 = Request {
        require_checks: true,
        pr: Some(7),
        ..request(Effect::Merge)
    };
    let outcome7 = run(&fake, &req7);
    assert_eq!(
        outcome7.word(),
        "merged",
        "the slot holder with fresh CI clears and merges"
    );
    assert_eq!(
        *fake.slot.borrow(),
        None,
        "the slot releases once the Merged outcome lands"
    );
}

#[test]
fn a_holder_that_went_red_releases_its_slot_to_a_waiting_stale_pr() {
    // AC3-EDGE. Inconclusive merge result: the fold path, so the waiting PR
    // walks the slot dance instead of merging straight through.
    let fake = Fake {
        slot: RefCell::new(Some(7)),
        facts: Some(PrFacts {
            number: 8,
            head_sha: "eight".to_string(),
            ..open_facts()
        }),
        covered_head: Some("eight".to_string()),
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 4 behind".to_string())),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    fake.other_facts.borrow_mut().insert(
        7,
        PrFacts {
            number: 7,
            state: "OPEN".to_string(),
            ..open_facts()
        },
    );
    fake.other_checks.borrow_mut().insert(7, "red".to_string());

    let req8 = Request {
        require_checks: true,
        pr: Some(8),
        ..request(Effect::Merge)
    };
    let outcome8 = run(&fake, &req8);
    assert_eq!(outcome8.word(), "held");
    assert!(outcome8.detail().contains("PR 8 now holds the merge slot"));
    assert_eq!(*fake.slot.borrow(), Some(8));
    assert_eq!(*fake.release_slot_calls.borrow(), vec![7]);
}

#[test]
fn an_inconclusive_hold_read_keeps_the_slot_with_its_holder() {
    // The eviction hold read is fail_open: an unreadable hold must not
    // evict, so the lease stays the bound.
    let fake = Fake {
        slot: RefCell::new(Some(7)),
        facts: Some(PrFacts {
            number: 8,
            head_sha: "eight".to_string(),
            ..open_facts()
        }),
        covered_head: Some("eight".to_string()),
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 4 behind".to_string())),
        ..clean()
    };
    fake.other_facts.borrow_mut().insert(
        7,
        PrFacts {
            number: 7,
            state: "OPEN".to_string(),
            ..open_facts()
        },
    );
    fake.other_checks
        .borrow_mut()
        .insert(7, "green".to_string());
    fake.other_holds.borrow_mut().insert(
        7,
        ProbeOutcome::Inconclusive("hold-check could not run".to_string()),
    );

    let req8 = Request {
        require_checks: true,
        pr: Some(8),
        ..request(Effect::Merge)
    };
    let outcome8 = run(&fake, &req8);
    assert_eq!(outcome8.word(), "held");
    assert!(outcome8.detail().contains("merge_slot_held"));
    assert_eq!(*fake.slot.borrow(), Some(7));
    assert!(fake.release_slot_calls.borrow().is_empty());
}

#[test]
fn slot_holder_error_fails_open_to_the_stale_hold_without_taking_a_slot() {
    // AC4-ERR. Inconclusive merge result: an unproven merge owes the fold,
    // so the claims fault falls back to the stale hold.
    let fake = Fake {
        slot_holder_err: true,
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 5 behind".to_string())),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    let req7 = Request {
        require_checks: true,
        pr: Some(7),
        ..request(Effect::Merge)
    };
    let outcome = run(&fake, &req7);
    assert_eq!(outcome.word(), "held");
    let detail = outcome.detail();
    assert!(detail.contains("ci_base_stale"));
    assert!(detail.contains("remedy: merge origin/main into the branch"));
    assert!(
        !fake.gh_calls.borrow().iter().any(|c| c[0] == "api"),
        "a faulted read must not side-effect the branch"
    );
    assert!(fake.take_slot_calls.borrow().is_empty());
}

#[test]
fn a_green_pr_whose_ci_predates_main_merges_main_in_and_waits_for_the_retest() {
    // The replay: one PR lands, then a second PR shares a file with it
    // and is green on a run created before that landing. It holds, takes
    // the slot, and has main merged into its branch; once the rerun is
    // green against the current main it merges.
    let mut fake = Fake {
        ci_base: Some(ProbeOutcome::Refused(
            "ci_base_stale: run predates base".to_string(),
        )),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "held");
    assert!(outcome.detail().contains("ci_base_stale"));
    assert!(outcome.detail().contains("merged main into PR 7's branch"));
    assert_eq!(*fake.slot.borrow(), Some(7));
    assert_eq!(
        *fake.gh_calls.borrow(),
        vec![vec![
            "api".to_string(),
            "-X".to_string(),
            "PUT".to_string(),
            "repos/{owner}/{repo}/pulls/7/update-branch".to_string(),
            "-f".to_string(),
            "expected_head_sha=abc123".to_string(),
        ]]
    );

    fake.checks = Some("pending".to_string());
    fake.ci_base = Some(ProbeOutcome::Clear);
    assert_eq!(run(&fake, &req).word(), "held");
    fake.checks = None;
    assert_eq!(run(&fake, &req).word(), "merged");
}

#[test]
fn a_self_held_slot_retests_the_base_at_most_once_then_merges_on_the_next_settle() {
    // The observed bug (2026-10-17): on a base busy enough that a full CI
    // run outlasts the next landing, retesting on every settle restarts
    // CI forever and the slot never converges. One retest per hold; the
    // settle after that merges on the real `gh pr merge` call instead.
    let fake = Fake {
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 3 behind".to_string())),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;

    let first = run(&fake, &req);
    assert_eq!(first.word(), "held");
    assert!(first.detail().contains("merged main into PR 7's branch"));
    assert_eq!(fake.gh_calls.borrow().len(), 1);

    // Base keeps moving (still ci_base_stale) and the retested head still
    // reads green - the exact shape that looped forever before this fix.
    fake.gh_calls.borrow_mut().clear();
    let second = run(&fake, &req);
    assert_eq!(second.word(), "merged", "{second:?}");
    assert!(
        fake.gh_calls.borrow().iter().all(|c| c[0] != "api"),
        "a second settle must not retest the base again: {:?}",
        fake.gh_calls.borrow()
    );
}

#[test]
fn a_failed_retest_still_counts_as_the_one_attempt_and_the_next_settle_relies_on_the_real_merge_check(
) {
    // `update-branch` can fail on a genuine conflict. Marking the attempt
    // regardless of its own outcome keeps the loop bounded either way; the
    // real merge call is the backstop that catches an actual conflict.
    let mut fake = Fake {
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 3 behind".to_string())),
        gh_recovery_ok: Some(false),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    let first = run(&fake, &req);
    assert_eq!(first.word(), "held");
    assert!(first.detail().contains("update-branch failed"));

    fake.gh_ok = false;
    fake.gh_output = "not mergeable (conflicts or base changed)".to_string();
    let second = run(&fake, &req);
    assert_eq!(second.word(), "failed", "{second:?}");
    assert_eq!(
        *fake.slot.borrow(),
        None,
        "a durable merge failure must free the slot"
    );
}

#[test]
fn a_proven_stale_pr_merges_without_folding_from_free_slot_or_self_held_alike() {
    // The proven path of the slot-time rule: green head, merge tree into
    // the current base clean, repo static step clean. The fold it replaces
    // restarted a full CI run per settle and never converged on a base
    // that moves faster than CI. Both slot states land the same way: the
    // free-slot PR takes no slot, and the self-held PR's merge frees its
    // own. No fold, no update-branch call.
    let mut fake = Fake {
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 3 behind".to_string())),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "merged", "{outcome:?}");
    assert!(fake.take_slot_calls.borrow().is_empty());
    assert_eq!(*fake.slot.borrow(), None);
    assert!(
        fake.gh_calls.borrow().iter().all(|c| c[0] != "api"),
        "a proven merge must not fold the base: {:?}",
        fake.gh_calls.borrow()
    );

    // The holder path: the slot this PR already holds frees through the
    // merge instead of feeding another retest.
    fake = Fake {
        slot: RefCell::new(Some(7)),
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 4 behind".to_string())),
        ..clean()
    };
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "merged", "{outcome:?}");
    assert_eq!(*fake.slot.borrow(), None, "the merge frees the held slot");
    assert!(fake.gh_calls.borrow().iter().all(|c| c[0] != "api"));
}

#[test]
fn the_retest_cap_binds_in_the_legacy_slot_window() {
    // The migration window can hold the live slot in the legacy repo-root
    // store, and take_slot accepts a self-held legacy row without creating
    // a primary one. The retest mark must land in the store the slot
    // actually lives in, or the one-retest cap never engages and the old
    // per-settle loop runs until the legacy lease ends.
    let temp = tempfile::TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(init.success());

    let current_root = temp.path().join("current");
    with_claims_root(&current_root, || {
        let key = slot_key("main");
        assert!(matches!(
            claims::acquire(
                &key,
                &slot_holder_key(7),
                claims::AcquireOpts {
                    root: Some(repo.clone()),
                    pid_unavailable: true,
                    ttl_ms: Some(MERGE_SLOT_TTL_MS),
                    ..Default::default()
                }
            ),
            claims::AcquireOutcome::Acquired(_)
        ));

        assert!(!RealProbes.slot_retested(&repo, "main", 7));
        RealProbes.mark_slot_retested(&repo, "main", 7);
        assert!(
            RealProbes.slot_retested(&repo, "main", 7),
            "the mark must reach the legacy store the self-held slot lives in"
        );
    });
}

#[test]
fn a_proven_or_retested_stale_preview_reads_no_ci_base_stale_blocker() {
    // Preview parity: the merge command merges both shapes, so pr status may
    // not hold them behind a fold that never runs.
    let proven = Fake {
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 3 behind".to_string())),
        ..clean()
    };
    let req = preview_request(7);
    assert!(preview_blockers(&proven, &req).is_empty());

    let retested = Fake {
        slot: RefCell::new(Some(7)),
        slot_retested: RefCell::new(true),
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: 3 behind".to_string())),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    assert!(preview_blockers(&retested, &req).is_empty());
}

#[test]
fn ci_base_freshness_gates_the_chain() {
    let fake = Fake {
        ci_base: Some(ProbeOutcome::Inconclusive("gh unavailable".to_string())),
        merge_result: Some(ProbeOutcome::Inconclusive(
            "merge result unreadable".to_string(),
        )),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    let outcome = run(&fake, &req);
    assert_eq!(outcome.word(), "held");
    assert!(outcome.detail().contains("retry later"));
    assert!(fake.gh_calls.borrow().is_empty());
    assert!(fake.take_slot_calls.borrow().is_empty());
    // Off by default, the base_overlap hold rides the same key: live lanes
    // never read the base move, so a mergeable PR merges as it is.
    let reads_refs = |fake: &Fake| {
        fake.gh_calls
            .borrow()
            .iter()
            .any(|call| call.iter().any(|arg| arg == "baseRefName,headRefName"))
    };
    let fake = Fake {
        ci_base: Some(ProbeOutcome::Refused("ci_base_stale: old".to_string())),
        fresh_ci: Some(false),
        live_lanes: 2,
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    assert_eq!(run(&fake, &req).word(), "merged");
    assert_eq!(*fake.ci_base_calls.borrow(), 0);
    assert!(!reads_refs(&fake));
    let armed = Fake {
        live_lanes: 2,
        ..clean()
    };
    run(&armed, &req);
    assert!(reads_refs(&armed));
}

#[test]
fn an_unreadable_binding_reads_unknown_not_bound() {
    // AC3-ERR: an instrument that cannot answer never becomes a verdict.
    // Bound still flows: every other test runs the default Clear fake,
    // which proves the gate reads the binding and refuses nothing else.
    for effect in [Effect::Merge, Effect::Arm] {
        let fake = Fake {
            node_binding: Some(ProbeOutcome::Inconclusive(
                "graph unreadable; refusing to assume bound".to_string(),
            )),
            ..clean()
        };
        let outcome = run(&fake, &request(effect));
        assert_eq!(outcome.word(), "unknown", "{effect:?}");
        assert!(outcome.detail().contains("refusing to assume bound"));
        assert!(fake.gh_calls.borrow().is_empty(), "{effect:?} ran gh");
    }
}

#[test]
fn an_unreadable_body_or_graph_refuses_to_assume_bound() {
    // AC4-ERR: a missing body key is an out-of-date deployed fno, never a
    // bound PR; the remedy names the update.
    let entries = vec![json!({"id": "x-bbbb", "cwd": "/this/repo", "project": "fno"})];
    let facts = PrFacts {
        body: None,
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &entries, &facts);
    let ProbeOutcome::Inconclusive(reason) = outcome else {
        unreachable!("a missing body is Inconclusive, never a verdict")
    };
    assert!(reason.contains("fno doctor update"));

    // A repo-scoped PR with no binding key at all is the refusal, with
    // the bind remedy.
    let facts = PrFacts {
        head_ref: "docs/team-succeed-faq".to_string(),
        body: Some(String::new()),
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &entries, &facts);
    let ProbeOutcome::Refused(reason) = outcome else {
        unreachable!("three missing keys refuse")
    };
    assert!(reason.contains("closure-trailer"));
    assert!(reason.contains("no flag bypasses this gate"));

    // No comparable url leaves the backref key unevaluable: Unknown, not
    // a refusal on a PR that may be bound through the back-pointer.
    let facts = PrFacts {
        head_ref: "docs/team-succeed-faq".to_string(),
        url: String::new(),
        body: Some(String::new()),
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &entries, &facts);
    let ProbeOutcome::Inconclusive(reason) = outcome else {
        unreachable!("an unscopeable backref key is Inconclusive")
    };
    assert!(reason.contains("could not be scoped"));

    // AC4-ERR, retarget: the body hands the branch's node to another
    // node, but the graph still points the old node at this PR; the
    // refusal names both fno backlog update commands.
    let url = "https://github.com/o/r/pull/7";
    let body = "Fixes x-bbbb\nRetarget x-aaaa x-bbbb msg-447f8f".to_string();
    let unmoved = vec![
        json!({"id": "x-aaaa", "cwd": "/this/repo", "project": "fno",
               "pr_number": 7, "pr_url": url}),
        json!({"id": "x-bbbb", "cwd": "/this/repo", "project": "fno"}),
    ];
    let facts = PrFacts {
        head_ref: "feature/x-aaaa".to_string(),
        body: Some(body.clone()),
        ..open_facts()
    };
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &unmoved, &facts);
    let ProbeOutcome::Refused(reason) = outcome else {
        unreachable!("an unmoved retarget binding refuses")
    };
    assert!(reason.contains("msg-447f8f"), "{reason}");
    assert!(
        reason.contains("fno backlog update x-aaaa pr_number=null pr_url=null"),
        "{reason}"
    );
    assert!(
        reason.contains("fno backlog update x-bbbb pr_number=7 pr_url"),
        "{reason}"
    );

    // AC4-HP: the graph moved (the old node holds no ref, the new one
    // carries this PR), so the probe is Clear.
    let moved = vec![
        json!({"id": "x-aaaa", "cwd": "/this/repo", "project": "fno"}),
        json!({"id": "x-bbbb", "cwd": "/this/repo", "project": "fno",
               "pr_number": 7, "pr_url": url}),
    ];
    let outcome = node_binding_from_entries(Path::new("/this/repo"), &moved, &facts);
    assert_eq!(outcome, ProbeOutcome::Clear);
}

#[test]
fn an_unreadable_review_probe_holds_rather_than_assuming_none_runs() {
    let fake = Fake {
        review_hold: Some(ProbeOutcome::Inconclusive(
            "review-activity-unreadable (exit Some(4))".to_string(),
        )),
        ..clean()
    };
    assert_eq!(run(&fake, &request(Effect::Arm)).word(), "held");
    assert!(fake.gh_calls.borrow().is_empty());
}

#[test]
fn a_disabled_standing_switch_refuses_and_names_the_operator_levers() {
    // AC1-EDGE: the authority evidence is specific, never "not allowed".
    let fake = Fake {
        enabled: false,
        ..clean()
    };
    let outcome = run(&fake, &request(Effect::Arm));
    assert_eq!(outcome.word(), "refused");
    assert!(outcome.detail().contains("auto_merge.enabled"));
    assert!(outcome.detail().contains("TARGET_AUTO_MERGE=1"));
}

#[test]
fn the_posture_floor_refuses_both_effects() {
    for effect in [Effect::Merge, Effect::Arm] {
        let fake = Fake {
            floor: Some("review posture no_review is below the automerge floor".to_string()),
            ..clean()
        };
        let outcome = run(&fake, &request(effect));
        assert_eq!(outcome.word(), "refused");
        assert!(fake.gh_calls.borrow().is_empty());
    }
}

#[test]
fn an_unreadable_covered_head_never_produces_an_unpinned_request() {
    // AC2-HP. This is the finalize defect: the old arm dropped
    // --match-head-commit when the journal could not be read.
    for effect in [Effect::Merge, Effect::Arm] {
        let fake = Fake {
            covered_head: None,
            ..clean()
        };
        let outcome = run(&fake, &request(effect));
        assert_eq!(outcome.word(), "unknown", "{effect:?}");
        assert!(outcome.detail().contains("unpinned"));
        assert!(fake.gh_calls.borrow().is_empty(), "{effect:?} ran gh");
    }
}

#[test]
fn a_push_between_validation_and_effect_reads_as_head_changed() {
    // AC2-HP.
    let fake = Fake {
        facts: Some(PrFacts {
            head_sha: "def456".to_string(),
            ..open_facts()
        }),
        ..clean()
    };
    let outcome = run(&fake, &request(Effect::Merge));
    assert_eq!(outcome.word(), "head_changed");
    assert!(fake.gh_calls.borrow().is_empty());
}

#[test]
fn an_arm_receipt_names_the_head_and_is_never_merged() {
    // AC2-EDGE. A queue entry is not a landed merge.
    let journal = crate::merge_provenance::TestJournal::opt_in();
    let fake = clean();
    let outcome = run(&fake, &request(Effect::Arm));
    assert_eq!(
        outcome,
        Outcome::Armed {
            head: "abc123".to_string(),
            merge_grant: None,
            queued: false,
        }
    );
    let calls = fake.gh_calls.borrow();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains(&"--auto".to_string()));
    assert!(calls[0].contains(&"--match-head-commit".to_string()));
    assert!(calls[0].contains(&"abc123".to_string()));
    // An arm records merge_armed, never a landed merge.
    let journal_rows = journal.rows();
    let armed: Vec<_> = journal_rows
        .iter()
        .filter(|r| r["data"]["span_kind"] == "merge_armed")
        .collect();
    assert_eq!(armed.len(), 1, "{journal_rows:?}");
    assert_eq!(armed[0]["data"]["pr"], 7);
    let landed: Vec<_> = journal_rows
        .iter()
        .filter(|r| r["data"]["span_kind"] == "merge_landed")
        .collect();
    assert!(landed.is_empty(), "{journal_rows:?}");
}

#[test]
fn an_unreadable_optional_review_count_never_holds_a_merge() {
    // Specimen 2026-10-22: the gh helpers went missing, the optional count
    // read unknown, and the old fail-closed gate held every merge.
    let fake = Fake {
        optional_unresolved: Some(None),
        ..clean()
    };
    assert!(preview_blockers(&fake, &preview_request(7)).is_empty());
    assert!(matches!(
        run(&fake, &request(Effect::Merge)),
        Outcome::Merged { .. }
    ));
    // No count at all means the status read itself failed: that still holds.
    let unread = Fake {
        optional_unresolved: None,
        ..clean()
    };
    let blockers = preview_blockers(&unread, &preview_request(7));
    assert!(
        blockers
            .iter()
            .any(|b| b.code == "optional_reviews_unknown"),
        "{blockers:?}"
    );
}

#[test]
fn an_immediate_merge_never_passes_auto() {
    let journal = crate::merge_provenance::TestJournal::opt_in();
    let fake = clean();
    let outcome = run(&fake, &request(Effect::Merge));
    assert_eq!(
        outcome,
        Outcome::Merged {
            head: "abc123".to_string(),
            note: None,
            cleanup_failure: None,
            merge_grant: None,
        }
    );
    let calls = fake.gh_calls.borrow();
    assert!(!calls[0].contains(&"--auto".to_string()));
    assert!(calls[0].contains(&"--match-head-commit".to_string()));
    // The owner's record: one merge_landed span for the run, naming the
    // command lane and the merged PR.
    let spans: Vec<_> = journal
        .rows()
        .into_iter()
        .filter(|r| r["data"]["span_kind"] == "merge_landed")
        .collect();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["data"]["path"], "pr_merge");
    assert_eq!(spans[0]["data"]["pr"], 7);
    assert_eq!(spans[0]["data"]["repo"], "o/r");
    assert_eq!(spans[0]["data"]["head"], "abc123");
}

#[test]
fn a_queued_merge_runs_no_gh_merge_and_an_ejected_head_holds() {
    let mut fake = clean();
    fake.merge_queue = Some(Ok(None));
    let outcome = run(&fake, &request(Effect::Merge));
    assert_eq!(
        outcome,
        Outcome::Armed {
            head: "abc123".to_string(),
            merge_grant: None,
            queued: true,
        }
    );
    assert!(outcome.detail().contains("fno merge queue"));
    assert!(!fake
        .gh_calls
        .borrow()
        .iter()
        .any(|call| call.get(1).map(String::as_str) == Some("merge")));
    fake.merge_queue = Some(Ok(Some("the merge queue ejected head abc123".to_string())));
    assert!(matches!(
        run(&fake, &request(Effect::Merge)),
        Outcome::Held { .. }
    ));
}

#[test]
fn an_already_armed_pr_stands_down_on_merge_and_no_ops_on_arm() {
    let armed = PrFacts {
        armed: true,
        ..open_facts()
    };
    let merge_fake = Fake {
        facts: Some(armed.clone()),
        ..clean()
    };
    assert_eq!(run(&merge_fake, &request(Effect::Merge)).word(), "held");
    assert!(merge_fake.gh_calls.borrow().is_empty());

    let arm_fake = Fake {
        facts: Some(armed),
        ..clean()
    };
    assert_eq!(run(&arm_fake, &request(Effect::Arm)).word(), "armed");
    assert!(arm_fake.gh_calls.borrow().is_empty());
}

#[test]
fn an_unreadable_pr_fetch_is_unknown_never_a_verdict() {
    let fake = Fake {
        facts_error: Some("gh api pulls/7 failed".to_string()),
        ..clean()
    };
    assert_eq!(run(&fake, &request(Effect::Arm)).word(), "unknown");
}

#[test]
fn a_red_verdict_holds_on_the_durable_grant_lane_and_fails_interactive() {
    // Red CI is the state a working session is in while it pushes fixes.
    // Spending a retry on it parked six open PRs in one afternoon.
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    req.authority = Some("durable_grant".to_string());
    let red = Fake {
        checks: Some("red".to_string()),
        ..clean()
    };
    let mut observed_head = None;
    let held = run_observed(&red, &req, &mut observed_head);
    assert_eq!(observed_head.as_deref(), Some("abc123"));
    assert_eq!(held.word(), "held");
    assert!(held
        .detail()
        .contains("the healer or the worker owns the next push"));
    assert!(red.gh_calls.borrow().is_empty());

    // The interactive lane keeps `failed`: the merge_status=failed stamp is
    // the worker's signal, and the existing tests above depend on it.
    let mut interactive = request(Effect::Merge);
    interactive.require_checks = true;
    interactive.authority = Some("manifest".to_string());
    assert_eq!(run(&red, &interactive).word(), "failed");
}

#[test]
fn a_gh_failure_over_a_merge_that_landed_reads_as_merged() {
    // The local post-merge step can fail after the server-side merge landed.
    let fake = Fake {
        gh_ok: false,
        gh_output: "failed to delete local branch".to_string(),
        facts: Some(PrFacts {
            state: "MERGED".to_string(),
            ..open_facts()
        }),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "merged");
    // The cleanup failure rides its OWN field. A bare success would lose
    // it, and folding it into `note` would report the merge as partial.
    let Outcome::Merged {
        note,
        cleanup_failure,
        ..
    } = outcome
    else {
        unreachable!()
    };
    assert_eq!(note.as_deref(), Some("merged server-side"));
    assert!(cleanup_failure
        .expect("a cleanup failure")
        .contains("failed to delete local branch"));
}

#[test]
fn a_checks_read_that_could_not_run_holds_instead_of_failing() {
    // `fno do pr status` answers `error` when the fetch is rate-limited or
    // the network is down. Failing on it stamps the node merge status
    // failed for a read that never described the checks at all.
    for verdict in ["error", "pending", "unknown"] {
        let fake = Fake {
            checks: Some(verdict.to_string()),
            ..clean()
        };
        let mut req = request(Effect::Merge);
        req.require_checks = true;
        assert_eq!(run(&fake, &req).word(), "held", "verdict {verdict}");
    }
    let red = Fake {
        checks: Some("red".to_string()),
        ..clean()
    };
    let mut req = request(Effect::Merge);
    req.require_checks = true;
    assert_eq!(run(&red, &req).word(), "failed");
}

#[test]
fn a_secondary_rate_limit_behind_a_config_warning_reads_retryable_not_merge_method() {
    // Specimen 2026-09-29: the fno gh proxy printed its config warning on
    // stderr first, so the real gh error - a secondary rate limit - read
    // as a merge-method fault and the worker burned three tries on it.
    let fake = Fake {
        gh_ok: false,
        gh_output: "fno config: guards.preset is not a modeled config key; ignored\n\
                    gh: You have exceeded a secondary rate limit. Please wait a bit \
                    before you try again."
            .to_string(),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "held");
    let detail = outcome.detail();
    assert!(detail.contains("secondary rate limit"), "{detail}");
    assert!(!detail.contains("merge method"), "{detail}");
    assert!(!detail.contains("guards.preset"), "{detail}");
}

#[test]
fn a_gh_proxy_admit_banner_ahead_of_a_real_refusal_reads_the_refusal_not_the_banner() {
    // Specimen 2026-10-19: the gh proxy prints its admit banner to stderr
    // right before os.execve hands off to the real gh, so both land in one
    // combined stream and the banner is line one. Three merge attempts on a
    // green, mergeable PR all surfaced the generic fallback reason with the
    // banner instead of gh's own refusal.
    let fake = Fake {
        gh_ok: false,
        gh_output: "gh proxy: draft guard unavailable; admitting\n\
                    gh: GraphQL: Pull request is not in a mergeable state"
            .to_string(),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "failed");
    let detail = outcome.detail();
    assert!(
        detail.contains("Pull request is not in a mergeable state"),
        "{detail}"
    );
    assert!(!detail.contains("draft guard unavailable"), "{detail}");
}

#[test]
fn a_gh_proxy_refusal_behind_its_own_admit_banner_is_never_skipped_as_noise() {
    // Review finding on this fix's own first draft: when the proxy admits
    // the draft-guard RPC (prints the "admitting" banner) and then re-enters
    // itself on the execve (a `resolve_real_gh()` failure, see gh_proxy.py's
    // ProxyIdentityError), both lines share the "gh proxy:" prefix and the
    // SAME stderr stream, but only the first is noise. The proxy's own
    // refusal is the entire story here - nothing real ever ran - so
    // skipping every "gh proxy:" line by prefix alone would report "no
    // error output" instead of the refusal.
    let fake = Fake {
        gh_ok: false,
        gh_output: "gh proxy: draft guard unavailable; admitting\n\
                    gh proxy: re-entered itself. resolve_real_gh() gave back \
                    the proxy shim instead of a real gh binary. Refusing to loop."
            .to_string(),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    let outcome = effect(&fake, &request(Effect::Merge), &authorized);
    assert_eq!(outcome.word(), "failed");
    let detail = outcome.detail();
    assert!(detail.contains("re-entered itself"), "{detail}");
    assert!(!detail.contains("draft guard unavailable"), "{detail}");
}

#[test]
fn a_worktree_held_branch_recovers_through_the_rest_endpoint_with_the_pin() {
    let fake = Fake {
        gh_ok: false,
        gh_output: "fatal: 'feature/x' is already used by worktree at /w".to_string(),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    // run_gh answers the same failure for both calls, so the recovery here
    // is the argv, not the outcome: the retry must carry sha=<pinned head>.
    let _ = effect(&fake, &request(Effect::Merge), &authorized);
    let calls = fake.gh_calls.borrow();
    assert_eq!(
        calls.len(),
        2,
        "the checkout refusal must retry through REST"
    );
    assert!(
        calls[1].contains(&"sha=abc123".to_string()),
        "{:?}",
        calls[1]
    );
    assert!(calls[1].contains(&"merge_method=squash".to_string()));
}

#[test]
fn an_arm_never_takes_the_worktree_recovery() {
    // A queue arm is not a merge, so a checkout refusal has nothing to
    // recover: PUT .../merge would merge NOW, past the queue's own wait.
    let fake = Fake {
        gh_ok: false,
        gh_output: "fatal: 'feature/x' is already used by worktree at /w".to_string(),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    let _ = effect(&fake, &request(Effect::Arm), &authorized);
    assert_eq!(fake.gh_calls.borrow().len(), 1);
}

#[test]
fn a_dead_state_read_after_a_failed_effect_is_unknown() {
    let fake = Fake {
        gh_ok: false,
        gh_output: "boom".to_string(),
        facts_error: Some("gh api pulls/7 failed".to_string()),
        ..clean()
    };
    let authorized = Authorized {
        facts: open_facts(),
        head: "abc123".to_string(),
        strategy: "squash".to_string(),
        merge_grant: None,
    };
    assert_eq!(
        effect(&fake, &request(Effect::Merge), &authorized).word(),
        "unknown"
    );
}

#[test]
fn hold_probe_fails_closed_on_every_non_success() {
    assert_eq!(
        classify_hold_probe(true, b"unheld", b""),
        ProbeOutcome::Clear
    );
    assert_eq!(
        classify_hold_probe(false, b"", b"dispatch-hold:x-owner"),
        ProbeOutcome::Refused("dispatch-hold:x-owner".to_string())
    );
    assert_eq!(
        classify_hold_probe(false, b"", b""),
        ProbeOutcome::Refused(
            "dispatch hold state unreadable; refusing to assume unheld".to_string()
        )
    );
}

#[test]
fn the_command_refuses_a_payload_that_names_no_effect() {
    assert!(parse_request(&json!({"cwd": "/tmp"})).is_err());
    assert!(parse_request(&json!({"effect": "arm"})).is_err());
    assert!(parse_request(&json!({"cwd": "/tmp", "effect": "sideways"})).is_err());
    let ok = parse_request(&json!({"cwd": "/tmp", "effect": "arm", "pr": 7}))
        .expect("a well-formed payload parses");
    assert_eq!(ok.effect, Effect::Arm);
    assert_eq!(ok.pr, Some(7));
}

fn preview_request(pr: u64) -> Request {
    Request {
        pr: Some(pr),
        effect: Effect::Preview,
        require_checks: true,
        ..request(Effect::Preview)
    }
}

fn preview_blockers(fake: &Fake, req: &Request) -> Vec<Blocker> {
    let facts = fake.facts.clone().unwrap_or_else(open_facts);
    match preview_walk(fake, req, &facts) {
        PreviewVerdict::Go { .. } => Vec::new(),
        PreviewVerdict::Blocked(rows) => rows,
    }
}

#[test]
fn merge_and_preview_name_the_same_first_blocker_for_every_gate() {
    // AC2: for each gate made to refuse in turn, the Merge outcome's
    // detail and the Preview's first blocker are the SAME answer - one
    // decision, two collectors.
    let base = || Fake { ..clean() };
    let scenarios: Vec<(&str, Fake)> = vec![
        (
            "pr_terminal",
            Fake {
                facts: Some(PrFacts {
                    state: "CLOSED".to_string(),
                    ..open_facts()
                }),
                ..base()
            },
        ),
        (
            "node_unbound",
            Fake {
                node_binding: Some(ProbeOutcome::Refused(
                    "unbound: no node names this PR".to_string(),
                )),
                ..base()
            },
        ),
        (
            "main_red",
            Fake {
                main_ci: Some(Ok(red_main())),
                main_repair: Some(Some("the declared repair lane is x-fix".to_string())),
                ..base()
            },
        ),
        (
            "dispatch_hold",
            Fake {
                dispatch_hold: Some(ProbeOutcome::Refused(
                    "dispatch_hold: held by tgt-x".to_string(),
                )),
                ..base()
            },
        ),
        (
            "outside_pr",
            Fake {
                outside_pr: Some(ProbeOutcome::Refused(
                    "outside_pr: PR #7 is from stranger/footnote; fleet automation never \
                     merges, heals, binds or reviews an outside pull request"
                        .to_string(),
                )),
                ..base()
            },
        ),
        (
            "pr_hold",
            Fake {
                pr_hold: Some(ProbeOutcome::Refused(
                    "pr_hold: the owner holds PR #7 (d-1); release it to clear".to_string(),
                )),
                ..base()
            },
        ),
        (
            "merge_freeze",
            Fake {
                merge_freeze: Some(ProbeOutcome::Refused(
                    "merge_freeze: PR #7 is off the pin merge-freeze:o/r@prs:6".to_string(),
                )),
                ..base()
            },
        ),
        (
            "review_in_flight",
            Fake {
                review_hold: Some(ProbeOutcome::Refused(
                    "review_in_flight: held by tgt-x at abc123".to_string(),
                )),
                ..base()
            },
        ),
        (
            "red_merge_result",
            Fake {
                merge_result: Some(ProbeOutcome::Refused("F821 in the merged tree".to_string())),
                ..base()
            },
        ),
        (
            "stacked_base",
            Fake {
                lineage: Some(ProbeOutcome::Refused(
                    "base no longer reaches the default branch".to_string(),
                )),
                ..base()
            },
        ),
        (
            "github_blocked",
            Fake {
                github_block: Some("smoke".to_string()),
                ..base()
            },
        ),
        (
            "review_coverage_uncovered",
            Fake {
                coverage_exit: Some(3),
                ..base()
            },
        ),
        (
            "optional_reviews_unresolved",
            Fake {
                optional_unresolved: Some(Some(2)),
                ..base()
            },
        ),
        (
            "ci_pending",
            Fake {
                checks: Some("pending".to_string()),
                ..base()
            },
        ),
    ];
    for (code, fake) in scenarios {
        let req_merge = Request {
            require_checks: true,
            ..request(Effect::Merge)
        };
        let outcome = run(&fake, &req_merge);
        let blockers = preview_blockers(&fake, &preview_request(7));
        assert!(
            !blockers.is_empty(),
            "{code}: preview cleared while merge refused ({outcome:?})"
        );
        let first = blockers[0].code.clone();
        assert_eq!(
            outcome.detail(),
            blockers[0].detail,
            "{code}: merge detail and the preview's first blocker ({first}) disagree"
        );
    }

    // The two admission gates answer the Arm effect identically: finalize's
    // auto-merge arm passes through decide, so the same gate holds it.
    let outside = Fake {
        outside_pr: Some(ProbeOutcome::Refused(
            "outside_pr: PR #7 is from stranger/footnote".to_string(),
        )),
        ..base()
    };
    let held = Fake {
        pr_hold: Some(ProbeOutcome::Refused(
            "pr_hold: the owner holds PR #7".to_string(),
        )),
        ..base()
    };
    let frozen = Fake {
        merge_freeze: Some(ProbeOutcome::Refused(
            "merge_freeze: PR #7 is off the pin".to_string(),
        )),
        ..base()
    };
    // An unreadable freeze fails closed on both effects.
    let unreadable = Fake {
        merge_freeze: Some(ProbeOutcome::Inconclusive(
            "merge_freeze: the freeze is unreadable".to_string(),
        )),
        ..base()
    };
    for (code, fake, word) in [
        ("outside_pr", outside, "refused"),
        ("pr_hold", held, "held"),
        ("merge_freeze", frozen, "held"),
        ("merge_freeze_unreadable", unreadable, "held"),
    ] {
        for effect in [Effect::Arm, Effect::Merge] {
            let req = Request {
                require_checks: true,
                ..request(effect)
            };
            let outcome = run(&fake, &req);
            assert_eq!(
                outcome.word(),
                word,
                "{code}: the {effect:?} did not answer {word}: {outcome:?}"
            );
        }
    }
}

/// The lead check-in's red token, stubbed: the run name and the sha it
/// failed on. A stub verdict, never a live merge.
fn red_main() -> Value {
    serde_json::json!({
        "verdict": "red",
        "workflow": "cli-ci",
        "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    })
}

#[test]
fn the_main_red_gate_refuses_red_and_exempts_only_the_declared_repair() {
    // Reproduction of 2026-09-30 (four merges landed on a red main): the
    // merge preview walks a stubbed red main verdict, never a live merge.
    let lane = "no node is declared the repair lane; tag the repair node \
                (fno backlog update <node> tags+=main-repair)";
    let refusing = Fake {
        main_ci: Some(Ok(red_main())),
        main_repair: Some(Some(lane.to_string())),
        ..clean()
    };
    let codes: Vec<String> = preview_blockers(&refusing, &preview_request(7))
        .into_iter()
        .map(|b| b.code.to_string())
        .collect();
    assert!(
        codes.contains(&"main_red".to_string()),
        "the preview walked a red main with no main_red blocker: {codes:?}"
    );
    let err = decide(&refusing, &request(Effect::Merge)).unwrap_err();
    assert_eq!(err.word(), "refused", "{}", err.detail());
    let detail = err.detail();
    assert!(detail.contains("cli-ci"), "{detail}");
    assert!(detail.contains("deadbeef"), "{detail}");
    assert!(detail.contains("repair lane"), "{detail}");

    // The exemption: a PR whose node carries the tag is the declared
    // repair, and it lands through the red.
    let repair = Fake {
        main_ci: Some(Ok(red_main())),
        main_repair: Some(None),
        ..clean()
    };
    assert!(decide(&repair, &request(Effect::Merge)).is_ok());

    // A pending main is not red, and an unreadable verdict never
    // manufactures one; the gate names main, not every base.
    for clearing in [
        Fake {
            main_ci: Some(Ok(Value::String("pending".into()))),
            main_repair: Some(Some(lane.to_string())),
            ..clean()
        },
        Fake {
            main_ci: Some(Err("gh api failed: rate limited".to_string())),
            main_repair: Some(Some(lane.to_string())),
            ..clean()
        },
        Fake {
            facts: Some(PrFacts {
                base_ref: "release".to_string(),
                ..open_facts()
            }),
            main_ci: Some(Ok(red_main())),
            ..clean()
        },
    ] {
        assert!(
            decide(&clearing, &request(Effect::Merge)).is_ok(),
            "{}",
            err_words(&clearing)
        );
    }
}

/// The refused word of a decide on `fake`, for a failure message.
fn err_words(fake: &Fake) -> String {
    match decide(fake, &request(Effect::Merge)) {
        Ok(_) => "authorized".to_string(),
        Err(outcome) => format!("{}: {}", outcome.word(), outcome.detail()),
    }
}

#[test]
fn live_lanes_counts_live_lane_claims_at_the_canonical_root() {
    // The overlap hold arms on this count; the trait default 0 would
    // leave it dead in production. The claim is planted through the same
    // acquire API the lane runtime uses, in a fresh git repo, so the
    // canonical-root resolution and the claims scan both run for real.
    let base = std::env::temp_dir().join(format!("x53c5-lanes-{}", std::process::id()));
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let init = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "git init failed for the lane-count fixture"
    );
    let opts = crate::claims::AcquireOpts {
        pid: Some(std::process::id()),
        ttl_ms: Some(60_000),
        root: Some(repo.clone()),
        ..Default::default()
    };
    let claimed = matches!(
        crate::claims::acquire("lane-slot:0", "parallel-lane:live-lanes-test", opts),
        crate::claims::AcquireOutcome::Acquired(_)
    );
    assert!(claimed, "the planted lane claim must acquire");
    assert_eq!(
        RealProbes.live_lanes(&repo),
        1,
        "the live lane claim must count"
    );
    let _ = crate::claims::release(
        "lane-slot:0",
        "parallel-lane:live-lanes-test",
        Some(repo.as_path()),
        None,
    );
    assert_eq!(
        RealProbes.live_lanes(&repo),
        0,
        "released lane must not count"
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn a_paint_pr_holds_until_an_answered_page_names_it() {
    let tmp = std::env::temp_dir().join(format!("xc129-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join(".fno")).unwrap();
    // questions_dir falls back to the space dir, and the hermetic guard
    // refuses a HOME-derived one under test; point it at a tempdir. The
    // graph store reads the state root too (the bound-node lookup), so
    // FNO_HOME is declared as well.
    struct RestoreEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match self.1.take() {
                Some(v) => std::env::set_var("FNO_HOME", v),
                None => std::env::remove_var("FNO_HOME"),
            }
        }
    }
    let spaces = std::env::temp_dir().join(format!("xc129-spaces-{}", std::process::id()));
    std::fs::create_dir_all(&spaces).unwrap();
    let _env_lock = crate::pr_status::cache_env_lock();
    // The bound-node lookup transitively resolves the global claims root.
    crate::paths::pin_test_claims_root(&spaces);
    let _env = {
        let prior = std::env::var_os("FNO_SPACES_DIR");
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let prior_home = std::env::var_os("FNO_HOME");
        std::env::set_var("FNO_HOME", &tmp);
        RestoreEnv(prior, prior_home)
    };
    std::fs::write(
        tmp.join(".fno/config.toml"),
        "merge.visual_paint_paths = [\"crates/fno/src/client/**\"]\n",
    )
    .unwrap();
    let mut fake = clean();
    fake.gh_output = "crates/fno/src/client/theme.rs\nREADME.md\n".to_string();
    let head: String = "a".repeat(40);
    let held = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).expect("held");
    assert_eq!(held.code, "visual_approval");
    assert!(
        held.detail.contains("crates/fno/src/client/theme.rs"),
        "{}",
        held.detail
    );
    // An OPEN page naming the PR never clears the hold.
    let qdir = crate::escalation::questions_dir(&tmp);
    std::fs::create_dir_all(&qdir).unwrap();
    let page = qdir.join("q-xc129test.md");
    std::fs::write(
        &page,
        "---\nquestion_id: q-xc129\nstatus: open\ntitle: May PR 7 merge?\n---\nbody\n",
    )
    .unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_some());
    // An answer that rejects, or picks no option, keeps the hold.
    for answer in [
        "2. Hold for changes",
        "Hold for changes",
        "none of these",
        "''",
    ] {
        std::fs::write(
            &page,
            format!(
                "---\nquestion_id: q-xc129\nstatus: answered\ntitle: May PR 7 merge?\n\
                 answer: {answer}\n---\nbody\n"
            ),
        )
        .unwrap();
        assert!(
            crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_some(),
            "answer {answer:?} must not clear"
        );
    }
    // An approve answer on a page that names no head keeps the hold: the
    // user's look binds to the head they saw.
    let approved = |body: &str| {
        format!(
            "---\nquestion_id: q-xc129\nstatus: answered\ntitle: May PR 7 merge?\n\
             answer: 1. Merge as painted\n---\n{body}\n"
        )
    };
    std::fs::write(&page, approved("body")).unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_some());
    // The page answers with the approve option at this head, the hold clears.
    std::fs::write(&page, approved(&format!("PR 7 at head {head}"))).unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none());
    // A push after the user looked holds again.
    let pushed: String = "b".repeat(40);
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_some());
    // A main merge or a test-only commit leaves the PR's paint patch alone:
    // the look carries. Only a hunk header moved, and a test file changed.
    let paint = |hunk: &str, line: &str| {
        format!(r#"["crates/fno/src/client/theme.rs","modified","{hunk}\n-old\n+{line}","s1"]"#)
    };
    let test_file = r#"["cli/tests/test_x.py","modified","@@ -1 +1 @@\n+t","s2"]"#;
    let routes = |now: String| {
        vec![
            ("/commits".to_string(), format!("{head}\n{pushed}\n")),
            (".base.ref".to_string(), "main\n".to_string()),
            (
                format!("compare/main...{head}"),
                paint("@@ -1,2 +1,2 @@", "new"),
            ),
            (
                format!("compare/main...{pushed}"),
                format!("{now}\n{test_file}"),
            ),
        ]
    };
    fake.gh_routes = routes(paint("@@ -9,2 +9,2 @@", "new"));
    assert!(
        crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_none(),
        "an unchanged paint patch carries the look to the new head"
    );
    // A real paint change at the new head holds.
    fake.gh_routes = routes(paint("@@ -9,2 +9,2 @@", "newer"));
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &pushed).is_some());
    fake.gh_routes.clear();
    // The page archives into done/; the approval must not lapse.
    let donedir = qdir.join("done");
    std::fs::create_dir_all(&donedir).unwrap();
    std::fs::rename(&page, donedir.join("q-xc129test.md")).unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none());
    // No paint paths in any candidate config: the gate is disarmed and
    // spends no gh read.
    let bare = std::env::temp_dir().join(format!("xc129-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&bare);
    std::fs::create_dir_all(&bare).unwrap();
    let mut off = clean();
    off.gh_output = "crates/fno/src/client/theme.rs\n".to_string();
    assert!(crate::merge_gates::visual_approval_blocker(&off, &bare, 7, &head).is_none());
    assert!(off.gh_calls.borrow().is_empty());
    // A checkout that carries the mux client arms the built-in paint list
    // with no config key: a stock footnote tree holds its own UI PRs. An
    // explicit empty list is still the opt-out.
    let stock = std::env::temp_dir().join(format!("xc129-stock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stock);
    std::fs::create_dir_all(stock.join("crates/fno/src/client")).unwrap();
    std::fs::create_dir_all(stock.join(".fno")).unwrap();
    let mut armed = clean();
    armed.gh_output = "crates/fno/src/client/org_board.rs\n".to_string();
    let held = crate::merge_gates::visual_approval_blocker(&armed, &stock, 7, &head)
        .expect("a stock footnote tree holds a mux client PR");
    assert_eq!(held.code, "visual_approval");
    std::fs::write(
        stock.join(".fno/config.toml"),
        "[merge]\nvisual_paint_paths = []\n",
    )
    .unwrap();
    assert!(crate::merge_gates::visual_approval_blocker(&armed, &stock, 7, &head).is_none());
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::remove_dir_all(&bare).ok();
    std::fs::remove_dir_all(&stock).ok();
}

#[test]
fn a_role_chat_approval_clears_the_paint_hold_only_at_its_head() {
    let tmp = std::env::temp_dir().join(format!("xc39f-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join(".fno")).unwrap();
    struct RestoreEnv(Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
                None => std::env::remove_var("FNO_SPACES_DIR"),
            }
            match self.1.take() {
                Some(v) => std::env::set_var("FNO_HOME", v),
                None => std::env::remove_var("FNO_HOME"),
            }
        }
    }
    let spaces = std::env::temp_dir().join(format!("xc39f-spaces-{}", std::process::id()));
    std::fs::create_dir_all(&spaces).unwrap();
    let _env_lock = crate::pr_status::cache_env_lock();
    // The bound-node lookup transitively resolves the global claims root.
    crate::paths::pin_test_claims_root(&spaces);
    let _env = {
        let prior = std::env::var_os("FNO_SPACES_DIR");
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let prior_home = std::env::var_os("FNO_HOME");
        std::env::set_var("FNO_HOME", &tmp);
        RestoreEnv(prior, prior_home)
    };
    std::fs::write(
        tmp.join(".fno/config.toml"),
        "merge.visual_paint_paths = [\"crates/fno/src/client/**\"]\n",
    )
    .unwrap();
    let head: String = "a".repeat(40);
    let pushed: String = "b".repeat(40);
    let row_at = |subject: &str,
                  decision: &str,
                  rationale: &str,
                  authority: &str,
                  lifecycle: &str| {
        format!(
            r#"{{"decisions":[{{"decision_id":"d-xc39f","subject":"{subject}","authority_source":"{authority}","lifecycle":"{lifecycle}","decision":"{decision}","rationale":"{rationale}"}}]}}"#
        )
    };
    let row = |decision: &str, rationale: &str, authority: &str, lifecycle: &str| {
        row_at("x-paint", decision, rationale, authority, lifecycle)
    };
    let mut fake = clean();
    fake.gh_output = "crates/fno/src/client/theme.rs\n".to_string();
    let clears = |fake: &Fake, head: &str| {
        crate::merge_gates::role_chat_clears_pr(fake, &tmp, 7, head, "x-paint")
    };
    // The lead's transcription at the PR's node, head-scoped: clears.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "superuser in chat: approved",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(clears(&fake, &head));
    // A head push invalidates the approval.
    assert!(
        !clears(&fake, &pushed),
        "an approval recorded for head a..a must not clear head b..b"
    );
    // With no node bound to the PR, even that row holds: the subject rule
    // has no node to match.
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_some());
    // A role row on another subject never clears: the door scopes the role
    // to the lead whose territory covers the subject, so only the PR's own
    // node carries that scope.
    fake.decisions_stdout = Some(
        row_at(
            "release-cadence",
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // No chat attestation in the rationale: held.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "lead judges it good",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // The row's authority is not role: held.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "agent",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // A retracted approval never clears: held.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "role",
            "retracted",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(!clears(&fake, &head));
    // A board answer: the rationale cites the answered page instead of chat.
    // The page names the PR and no head; the row carries the head.
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user answered q-board7 on the board",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    let qdir = crate::escalation::questions_dir(&tmp);
    std::fs::create_dir_all(&qdir).unwrap();
    let page = |status: &str, answer: &str| {
        format!(
            "---\nquestion_id: q-board7\nstatus: {status}\ntitle: May PR 7 merge?\n\
             answer: {answer}\n---\nbody\n"
        )
    };
    // No page, an open page, or a rejecting answer: held.
    assert!(!clears(&fake, &head));
    for (status, answer) in [("open", "''"), ("answered", "2. Hold for changes")] {
        std::fs::write(qdir.join("q-board7.md"), page(status, answer)).unwrap();
        assert!(!clears(&fake, &head), "{status} {answer} must hold");
    }
    // The approving answer clears, at the row's head only.
    std::fs::write(
        qdir.join("q-board7.md"),
        page("answered", "Approve as shown"),
    )
    .unwrap();
    assert!(clears(&fake, &head));
    assert!(!clears(&fake, &pushed));
    std::fs::remove_file(qdir.join("q-board7.md")).unwrap();
    // The gate is closed again with the empty index the Fake serves by
    // default; the head-scoped grant text stays out of a held detail.
    fake.decisions_stdout = None;
    let held = crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).expect("held");
    assert!(
        held.detail.contains("role-recorded decision"),
        "{}",
        held.detail
    );
    // A split: the node holds PR 6 and lists PR 7 in additional_prs. PR 7
    // binds to it, so the approval recorded on that node clears PR 7. A row
    // whose own pr_number is the PR still wins over a row that only lists it.
    crate::graph_store::seed_rows(
        &crate::org_board::scope::graph_json_path(&tmp),
        &[
            json!({"id": "x-lists", "status": "in_review", "additional_prs": [{"number": 6}]}),
            json!({"id": "x-paint", "status": "in_review", "pr_number": 6,
                   "additional_prs": [{"number": 7}]}),
        ],
    )
    .unwrap();
    let bound = |pr| {
        crate::authorized_merge::pr_bound_entry(&tmp, pr)
            .and_then(|e| e.get("id").and_then(Value::as_str).map(str::to_owned))
    };
    assert_eq!(bound(7).as_deref(), Some("x-paint"));
    assert_eq!(bound(6).as_deref(), Some("x-paint"));
    assert_eq!(bound(8), None);
    // PR numbers collide across repositories: a ref whose URL names another
    // repository never binds, primary or additional; a ref with no URL does.
    let rows = [
        json!({"id": "x-foreign", "pr_number": 7,
               "pr_url": "https://github.com/o/other/pull/7"}),
        json!({"id": "x-also-foreign", "additional_prs": [
               {"number": 7, "url": "https://github.com/o/other/pull/7"}]}),
        json!({"id": "x-here", "additional_prs": [
               {"number": 7, "url": "https://github.com/O/Here/pull/7"}]}),
    ];
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 7, Some("o/here")),
        Some(2)
    );
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows[..2], 7, Some("o/here")),
        None
    );
    assert_eq!(
        crate::authorized_merge::pr_bound_index(&rows, 7, None),
        Some(0)
    );
    fake.decisions_stdout = Some(
        row(
            "Approved: PR 7 at <head> as built",
            "user in chat: approved",
            "role",
            "unscoped",
        )
        .replace("<head>", &head)
        .into_bytes(),
    );
    assert!(crate::merge_gates::visual_approval_blocker(&fake, &tmp, 7, &head).is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

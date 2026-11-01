//! The coverage row's one-word answer: reviewed, refused, findings open, or
//! unreviewed. A module named by its question: `loopcheck.rs` is far over the file budget
//! and shrink-only, so the state deriver lives here with the tests that pin
//! it.
//!
//! The predicate that gives the answer its politics: a refusal names the
//! state only when the verdict was OWED. An optional GitHub App is honored
//! when it responds and owed nothing when it does not, so its quota bounce
//! must not rename an uncovered row after a reviewer nobody was waiting on.
//!
//! A failed review is a review, so a fail round at the head never reads
//! unreviewed. Both derivations (the report here and the stored row in
//! `pr_status::reviews`) share one precedence and walk one golden table,
//! `cli/tests/config/review_state_table.json`.

use super::*;

impl ReviewState {
    /// The one precedence. Refusal outranks an open fail because the coverage
    /// gate prescribes reviewer recovery from that word, and `reviewed_count`
    /// still shows the round.
    pub(crate) fn from_evidence(reviewed: bool, refusal_owed: bool, fail_at_head: bool) -> Self {
        if reviewed {
            Self::Reviewed
        } else if refusal_owed {
            Self::ReviewerRefused
        } else if fail_at_head {
            Self::FindingsOpen
        } else {
            Self::Unreviewed
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Reviewed => "reviewed",
            Self::ReviewerRefused => "reviewer_refused",
            Self::FindingsOpen => "findings_open",
            Self::Unreviewed => "unreviewed",
        }
    }
}

impl CoverageReport {
    pub fn review_state(&self) -> Option<ReviewState> {
        if matches!(self.coverage, Coverage::Unknown) {
            return None;
        }
        let reviewed = self.verdicts.iter().any(|verdict| {
            verdict.verdict == CoverageVerdict::Reviewed
                && human_approval_counts(verdict, self.github_approval_satisfies)
        });
        let refusal_owed = self
            .verdicts
            .iter()
            .any(|verdict| verdict.verdict == CoverageVerdict::Refused && verdict.required);
        Some(ReviewState::from_evidence(
            reviewed,
            refusal_owed,
            self.fail_at_head,
        ))
    }

    /// The state read at the round budget (d-0fa92eb9): a spent budget on a
    /// covered row IS reviewed, whatever the verdict freshness says. Under
    /// the budget, or on an uncovered row, the plain derivation stands.
    pub fn review_state_at(&self, budget_spent: bool) -> Option<ReviewState> {
        if budget_spent && matches!(self.coverage, Coverage::Covered(n) if n > 0) {
            return Some(ReviewState::Reviewed);
        }
        self.review_state()
    }

    pub fn refused_reviewers(&self) -> Vec<&str> {
        self.verdicts
            .iter()
            .filter(|verdict| verdict.verdict == CoverageVerdict::Refused)
            .map(|verdict| verdict.name.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopcheck::{classify_coverage, Freshness};

    fn human_github_approval(name: &str, author: bool) -> ReviewerVerdict {
        ReviewerVerdict {
            producer: CoverageProducer::GithubApp,
            name: name.to_string(),
            verdict: CoverageVerdict::Reviewed,
            human_approval: true,
            author_approval: author,
            attestation_origin: AttestationOrigin::Unknown,
            reviewed_sha: "h1".to_string(),
            freshness: Some(Freshness::Fresh),
            scope: None,
            refusal_reason: None,
            reviewer_context: None,
            attest_machine: None,
            required: true,
            passed: true,
        }
    }

    #[test]
    fn a_non_author_approval_counts_once_the_flag_is_on() {
        // The github_approval_satisfies contract, carried over from the
        // retired Python derivation: bob approving alice's PR reads
        // reviewed, so the flag widens who can satisfy the review gate.
        let rep = CoverageReport {
            fail_at_head: false,
            github_approval_satisfies: true,
            coverage: Coverage::Covered(0),
            verdicts: vec![human_github_approval("bob", false)],
        };
        assert_eq!(rep.review_state(), Some(ReviewState::Reviewed));
    }

    #[test]
    fn an_author_approval_is_never_counted_even_with_the_flag_on() {
        // alice approving her own PR stays recorded and never derives
        // reviewed, so a run that collected no approval at all cannot
        // pass on the strength of the author's own.
        let rep = CoverageReport {
            fail_at_head: false,
            github_approval_satisfies: true,
            coverage: Coverage::Covered(0),
            verdicts: vec![human_github_approval("alice", true)],
        };
        assert_eq!(rep.review_state(), Some(ReviewState::Unreviewed));
    }

    #[test]
    fn flag_off_keeps_the_exclusion_of_human_approvals() {
        // With the flag off every human approval is excluded, exactly the
        // pre-flag semantics: the flag only ever widens the count.
        let rep = CoverageReport {
            fail_at_head: false,
            github_approval_satisfies: false,
            coverage: Coverage::Covered(0),
            verdicts: vec![human_github_approval("bob", false)],
        };
        assert_eq!(rep.review_state(), Some(ReviewState::Unreviewed));
    }

    #[test]
    fn local_attestation_refusal_is_always_owed() {
        // A local refusal records an attempt that RAN and declined - real
        // work with a real remedy - so it keeps naming itself. The wire form
        // carries NO `required` key: true is omitted on serialize, which is
        // byte-identical to what every pre-field row looked like.
        let events = r#"{"type":"review_invocation","data":{"stage":"refused","head_sha":"abc12345","verb":"code-review","reason":"empty_diff"}}"#;
        let rep = classify_coverage(
            &[],
            &[],
            events,
            &[],
            true,
            None,
            &|_| Freshness::Fresh,
            "",
            "abc12345",
        );
        let verdict = rep.verdicts.first().expect("the refused verdict");
        assert_eq!(verdict.verdict, CoverageVerdict::Refused);
        assert!(verdict.required);
        let wire = serde_json::to_value(verdict).unwrap();
        assert!(wire.get("required").is_none(), "{wire}");
        assert_eq!(rep.review_state(), Some(ReviewState::ReviewerRefused));
    }

    #[test]
    fn a_row_with_no_required_key_reads_required() {
        // The deserialization half of the default: a stored verdict emitted
        // before the field existed carries no `required` key, and reading it
        // back must keep today's semantics - the refusal was owed.
        let stored = serde_json::json!({
            "producer": "github_app",
            "name": "chatgpt-codex-connector",
            "verdict": "refused"
        });
        assert!(stored.get("required").is_none());
        let verdict: ReviewerVerdict = serde_json::from_value(stored).unwrap();
        assert!(verdict.required, "absence reads REQUIRED, never skipped");
        let rep = CoverageReport {
            fail_at_head: false,
            github_approval_satisfies: false,
            coverage: Coverage::Covered(0),
            verdicts: vec![verdict],
        };
        assert_eq!(rep.review_state(), Some(ReviewState::ReviewerRefused));
    }

    #[test]
    fn every_golden_table_row_derives_its_expected_word() {
        let table: Value = serde_json::from_str(include_str!(
            "../../../../cli/tests/config/review_state_table.json"
        ))
        .unwrap();
        for row in table["rows"].as_array().unwrap() {
            let spent = row["rounds_exhausted"].as_bool().unwrap_or(false);
            let rep = CoverageReport {
                fail_at_head: row["fail_at_head"].as_bool().unwrap_or(false),
                github_approval_satisfies: false,
                coverage: Coverage::Covered(usize::from(spent)),
                verdicts: serde_json::from_value(row["verdicts"].clone()).unwrap(),
            };
            let word = rep.review_state_at(spent).map(ReviewState::word);
            assert_eq!(word, row["expected_state"].as_str(), "{}", row["name"]);
        }
    }
}

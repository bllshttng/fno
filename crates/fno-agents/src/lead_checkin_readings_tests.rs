#[test]
fn cause_rows() {
    let stderr = "fno config: a is not modeled\nfno config: b is not modeled\ngh: API rate limit exceeded for user ID 4994564. (HTTP 403)";
    assert_eq!(
        stderr_cause(stderr),
        "gh: API rate limit exceeded for user ID 4994564. (HTTP 403)"
    );

    let error = "gh api repos/{owner}/{repo}/commits/<sha>/check-runs failed: fno config: x is not modeled\ngh: API rate limit exceeded (HTTP 403)";
    assert_eq!(
        gh_error_cause(error),
        "gh: API rate limit exceeded (HTTP 403)"
    );

    assert_eq!(
        stderr_cause("fno config: first\nfno config: last"),
        "fno config: last"
    );
    assert_eq!(stderr_cause(" \n\t"), "no stderr");

    let cause = "é".repeat(300);
    let result = stderr_cause(&cause);
    assert_eq!(result.chars().count(), 120);
    assert_eq!(result, "é".repeat(120));
}

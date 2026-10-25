//! Time each probe of the authorization chain on stderr.
//!
//! The chain is one subprocess the merge command kills at its bound, so a
//! timeout alone says nothing about where the time went. Under [`TRACE_ENV`]
//! every step names itself when it begins and when it ends, and the killed
//! child's last unmatched `began` line is the step that held the bound.
//! Without the trace nothing prints: the probes also run in-process (the
//! stop hook's finalize) and behind doors that read stderr as the error.

use std::time::Instant;

/// Set by a caller that will read the child's stderr after a timeout.
pub const TRACE_ENV: &str = "FNO_MERGE_STEP_TRACE";

/// The line prefix both ends agree on.
pub const BEGAN: &str = "authorized-merge: step began: ";

pub struct Step {
    name: String,
    start: Instant,
    trace: bool,
}

pub fn step(name: impl Into<String>) -> Step {
    let name = name.into();
    let trace = std::env::var_os(TRACE_ENV).is_some_and(|v| !v.is_empty());
    if trace {
        eprintln!("{BEGAN}{name}");
    }
    Step {
        name,
        start: Instant::now(),
        trace,
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if self.trace {
            eprintln!(
                "authorized-merge: step {} took {:.1}s",
                self.name,
                self.start.elapsed().as_secs_f64()
            );
        }
    }
}

/// The step a killed child was inside: the last `began` line with no `took`
/// line after it for the same name. None when every step finished.
pub fn unfinished(stderr: &str) -> Option<String> {
    let mut open: Vec<&str> = Vec::new();
    for line in stderr.lines() {
        if let Some(name) = line.strip_prefix(BEGAN) {
            open.push(name);
        } else if let Some(rest) = line.strip_prefix("authorized-merge: step ") {
            if let Some((name, _)) = rest.rsplit_once(" took ") {
                if let Some(at) = open.iter().rposition(|n| *n == name) {
                    open.remove(at);
                }
            }
        }
    }
    open.last().map(|name| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_names_the_innermost_open_step() {
        let stderr = format!(
            "{BEGAN}pr_facts\nauthorized-merge: step pr_facts took 0.4s\n\
             {BEGAN}checks_read\n{BEGAN}gh api repos/x\nauthorized-merge: step gh api repos/x took 3.0s\n\
             {BEGAN}gh pr view\n"
        );
        assert_eq!(unfinished(&stderr).as_deref(), Some("gh pr view"));
        let done = format!("{BEGAN}a\nauthorized-merge: step a took 0.1s\n");
        assert_eq!(unfinished(&done), None);
    }
}

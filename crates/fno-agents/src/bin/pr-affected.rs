use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
struct Selection {
    cargo: bool,
    python_full: bool,
    skill_bundles: bool,
}

fn select_jobs(event: &str, paths: Option<&[String]>, bundles: Option<&[String]>) -> Selection {
    // Main runs every lane on every push, and the nightly builds only a green
    // main sha, so a break a narrowed PR misses is caught within the hour and
    // never ships. Only the bundle freshness job selects on main: it rides
    // the full lanes there.
    if event != "pull_request" {
        return Selection {
            cargo: true,
            python_full: true,
            skill_bundles: false,
        };
    }
    // An unreadable diff or manifest fails closed: run the check rather than
    // trust a selection we could not compute.
    let skill_bundles = match (paths.filter(|paths| !paths.is_empty()), bundles) {
        (Some(paths), Some(watched)) => paths.iter().any(|path| names_a_bundle_path(path, watched)),
        _ => true,
    };
    // A PR that touches only prose, skills, hooks or images runs no full
    // lane. Anything else, or an unreadable diff, runs them all.
    let full = !paths.is_some_and(|paths| !paths.is_empty() && paths.iter().all(|p| is_light(p)));
    Selection {
        cargo: full,
        python_full: full,
        skill_bundles,
    }
}

/// Files a full lane reads at build or test time although they sit under a
/// light tree. Keep in step with the include_str! sites under crates/ and
/// the docs paths cli-ci.yml's push filter names.
const LANE_INPUTS: &[&str] = &[
    "docs/architecture/activity-feed.md",
    "docs/architecture/help-routing.md",
    "docs/architecture/product-boundaries.md",
    "docs/env-vars.md",
    "docs/harnesses/capability-matrix.md",
    "docs/reaping-faq.md",
    "docs/state-root-layout.tsv",
];

const LIGHT_TREES: &[&str] = &["docs/", "skills/", "hooks/"];

const LIGHT_EXTENSIONS: &[&str] = &[".md", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp"];

fn is_light(path: &str) -> bool {
    if LANE_INPUTS.contains(&path) {
        return false;
    }
    if LIGHT_TREES.iter().any(|tree| path.starts_with(tree)) {
        return true;
    }
    // Prose and images outside the code trees; a crate or cli .md can be a
    // compiled prompt or a test fixture.
    !path.starts_with("crates/")
        && !path.starts_with("cli/")
        && LIGHT_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

/// Exact file match, or a change anywhere beneath a directory-valued
/// source/dest (pack rows and some canonical trees name directories).
fn names_a_bundle_path(path: &str, watched: &[String]) -> bool {
    watched.iter().any(|w| {
        path == w
            || path
                .strip_prefix(w.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Paths skill-bundles.yaml pins: every declared source plus every generated
/// destination (dest values are relative to `skills/<skill>/`). A change to
/// any of these must run the bundle freshness check: PR 2719 changed
/// scripts/validate-plan.sh green while both bundled copies drifted, because
/// no lane member owned them. Pack rows are invisible to this scan on
/// purpose: their paths live in plugins/*/plugin.yaml, which the workflow
/// trigger never fires on. Line shapes mirror the Python parser's fallback
/// (comment runs to end of line, `- ` prefixes the list entries), so the
/// header's doc examples never scan as paths.
fn bundle_watch_set() -> Option<Vec<String>> {
    let text = std::fs::read_to_string("skill-bundles.yaml").ok()?;
    Some(bundle_watch_set_from(&text))
}

fn bundle_watch_set_from(text: &str) -> Vec<String> {
    let mut skill = String::new();
    // The manifest pins itself: an edit that adds or retargets a row without
    // regenerating the copies is the exact drift this lane exists to catch,
    // and that edit names no source or dest.
    let mut set = vec!["skill-bundles.yaml".to_string()];
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let line = line.strip_prefix("- ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("skill:") {
            skill = rest.trim().trim_matches('"').trim_matches('\'').to_string();
        } else if let Some(rest) = line.strip_prefix("source:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                set.push(value.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("dest:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() && !skill.is_empty() {
                set.push(format!("skills/{skill}/{value}"));
            }
        }
    }
    set
}

fn changed_paths(base: &str, head: &str) -> Option<Vec<String>> {
    let range = format!("{base}...{head}");
    let output = Command::new("git")
        .args(["diff", "--name-only", "--no-renames", &range])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let paths: Vec<String> = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::to_owned)
        .collect();
    (!paths.is_empty()).then_some(paths)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let event = args.first().map(String::as_str).unwrap_or("");
    let selection = if event == "pull_request" {
        let paths = args
            .get(1)
            .zip(args.get(2))
            .and_then(|(base, head)| changed_paths(base, head));
        let bundles = bundle_watch_set();
        select_jobs(event, paths.as_deref(), bundles.as_deref())
    } else {
        select_jobs(event, None, None)
    };
    println!("cargo={}", selection.cargo);
    println!("python_full={}", selection.python_full);
    println!("skill_bundles={}", selection.skill_bundles);
}

#[cfg(test)]
mod tests {
    use super::{select_jobs, Selection};

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn a_light_only_diff_skips_the_full_lanes() {
        for changed in [
            paths(&["docs/usage.md", "README.md", "docs/img/flow.png"]),
            paths(&["skills/target/SKILL.md", "skills/target/scripts/x.sh"]),
            paths(&["hooks/context-nudge.sh"]),
        ] {
            let sel = select_jobs("pull_request", Some(&changed), Some(&[]));
            assert!(!sel.cargo && !sel.python_full, "{changed:?}: {sel:?}");
        }
    }

    #[test]
    fn any_code_or_lane_input_runs_every_lane() {
        for changed in [
            paths(&["docs/usage.md", "cli/tests/unit/test_example.py"]),
            paths(&["crates/fno-agents/src/lib.rs"]),
            paths(&["crates/fno-agents/src/pr_steward_prompt.md"]),
            paths(&["docs/env-vars.md"]),
            paths(&["skills/x/SKILL.md", "scripts/ci/check.sh"]),
            paths(&[]),
        ] {
            let sel = select_jobs("pull_request", Some(&changed), Some(&[]));
            assert!(sel.cargo && sel.python_full, "{changed:?}: {sel:?}");
            assert_eq!(sel.skill_bundles, changed.is_empty(), "{changed:?}");
        }
        for event in ["push", "schedule", "workflow_dispatch"] {
            let sel = select_jobs(event, None, None);
            assert!(sel.cargo && sel.python_full, "{event}");
        }
        assert_eq!(
            select_jobs("pull_request", None, Some(&[])),
            Selection {
                cargo: true,
                python_full: true,
                skill_bundles: true,
            }
        );
    }

    #[test]
    fn a_changed_bundled_canonical_script_selects_the_freshness_check() {
        // The 2026-09-28 specimen: scripts/validate-plan.sh is a source named
        // in skill-bundles.yaml; skills/blueprint and skills/execute carry
        // the destinations it drifted.
        let watched = paths(&[
            "scripts/validate-plan.sh",
            "skills/blueprint/scripts/validate-plan.sh",
            "skills/execute/scripts/validate-plan.sh",
        ]);
        let changed = paths(&["scripts/validate-plan.sh"]);
        let sel = select_jobs("pull_request", Some(&changed), Some(&watched));
        assert!(
            sel.skill_bundles,
            "a source change must select the freshness check"
        );
        let changed = paths(&["docs/usage.md", "skills/blueprint/scripts/validate-plan.sh"]);
        let sel = select_jobs("pull_request", Some(&changed), Some(&watched));
        assert!(
            sel.skill_bundles,
            "a destination change must select the freshness check"
        );
    }

    #[test]
    fn an_unreadable_manifest_fails_the_freshness_job_on() {
        let changed = paths(&["docs/usage.md"]);
        let sel = select_jobs("pull_request", Some(&changed), None);
        assert!(sel.skill_bundles);
    }

    #[test]
    fn bad_git_revision_fails_closed() {
        let Some(paths) = super::changed_paths("missing-pr-base", "missing-pr-head") else {
            assert_eq!(
                select_jobs("pull_request", None, Some(&[])),
                Selection {
                    cargo: true,
                    python_full: true,
                    skill_bundles: true,
                }
            );
            return;
        };
        assert!(!paths.is_empty());
        panic!("an invalid revision range unexpectedly produced changed paths");
    }
}

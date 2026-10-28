//! The idle-PR alarm: a PR that sits green, mergeable and review-covered with
//! nobody merging it. The owning worker merges by law; this arm is the
//! backstop for the worker that left and the lead that swept only its wave.
//! Each pass lists every open PR of every registry repo and reads each
//! non-draft one through the status door, so a read rides the shared status
//! cache and the account poller instead of new GitHub calls. The status
//! `ready` verdict is the merge gate's own answer, so a held, uncovered or
//! visual-approval PR never reads ready. The first ready sighting of a head
//! starts the clock under `pr-idle/<repo>-<pr>.json`. At 2 hours every live
//! lead gets mail and the operator gets a phone notice; at 6 hours both are
//! raised again. A new head, a not-ready read or a closed PR resets it.
//! Contract: docs/architecture/pr-heal.md, "The idle-PR alarm".

use crate::paths::AgentsHome;
use crate::state;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const RAISE_AFTER_MS: i64 = 2 * 3_600_000;
pub(crate) const RERAISE_AFTER_MS: i64 = 6 * 3_600_000;
const SENDER: &str = "fno/pr-idle";
/// A 2 hour threshold needs no finer beat, and a slower beat spends less of
/// the shared status cache.
const INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Seen {
    head: String,
    ready_since_ms: i64,
    /// The highest alarm level fully delivered: 0 none, 1 at 2h, 2 at 6h.
    raised: u8,
    /// The level `told` and `phoned` belong to. A delivery still pending when
    /// the next level comes due starts over for that level.
    pending: u8,
    /// The leads the pending level reached, so a retry mails only the rest.
    told: Vec<String>,
    phoned: bool,
}

/// One status-door answer for one open PR.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Read {
    pub pr: u64,
    pub head: String,
    pub ready: bool,
}

fn dir(home: &AgentsHome) -> PathBuf {
    home.root().join("pr-idle")
}

fn stem(slug: &str) -> String {
    slug.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn file(home: &AgentsHome, slug: &str, pr: u64) -> PathBuf {
    dir(home).join(format!("{}-{pr}.json", stem(slug)))
}

fn level_at(age_ms: i64) -> u8 {
    if age_ms >= RERAISE_AFTER_MS {
        2
    } else if age_ms >= RAISE_AFTER_MS {
        1
    } else {
        0
    }
}

fn age_words(age_ms: i64) -> String {
    let minutes = age_ms / 60_000;
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

fn notice(slug: &str, pr: u64, age_ms: i64, level: u8) -> String {
    let again = if level >= 2 {
        " This is the second alarm."
    } else {
        ""
    };
    format!(
        "PR {pr} in {slug} has been green, mergeable and reviewed for {}, and nobody merged it.{again} Merge it from that repo: `fno do pr merge {pr}`.",
        age_words(age_ms)
    )
}

/// Fold one repo's reads into its clocks and deliver every alarm that is due.
/// `open` is the full open-PR listing, or None when it was unreadable: only a
/// clean listing drops the clock of a PR that left it.
pub(crate) fn run_repo(
    home: &AgentsHome,
    now: i64,
    slug: &str,
    open: Option<&HashSet<u64>>,
    reads: &[Read],
    leads: &[String],
    runner: crate::burn_watch::Runner<'_>,
    phone: &mut dyn FnMut(&str, &str, Option<&str>) -> bool,
) -> Result<(), String> {
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut first_error = None;
    for read in reads {
        let path = file(home, slug, read.pr);
        if !read.ready {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let mut seen: Seen = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if seen.head != read.head || seen.ready_since_ms <= 0 {
            seen = Seen {
                head: read.head.clone(),
                ready_since_ms: now,
                ..Seen::default()
            };
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
        let age = now.saturating_sub(seen.ready_since_ms);
        let level = level_at(age);
        if level <= seen.raised {
            continue;
        }
        let text = notice(slug, read.pr, age, level);
        let before = seen.clone();
        if seen.pending != level {
            seen.pending = level;
            seen.told.clear();
            seen.phoned = false;
        }
        let pending: Vec<&String> = leads.iter().filter(|l| !seen.told.contains(*l)).collect();
        for lead in pending {
            match crate::first_check::send_mail(&mut *runner, SENDER, lead, &text) {
                Ok(()) => seen.told.push(lead.clone()),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        if !seen.phoned {
            let title = format!("PR {} green and unmerged {}", read.pr, age_words(age));
            let body = format!("{slug}: nobody merged it; run fno do pr merge {}", read.pr);
            let pointer = format!("fno do pr status {}", read.pr);
            seen.phoned = phone(&title, &body, Some(&pointer));
            if !seen.phoned {
                first_error.get_or_insert(format!("PR {}: the phone notice did not send", read.pr));
            }
        }
        if seen.phoned && leads.iter().all(|l| seen.told.contains(l)) {
            seen.raised = level;
        }
        if seen != before {
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
    }
    if let Some(open) = open {
        let prefix = format!("{}-", stem(slug));
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let pr = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok());
            if pr.is_some_and(|pr| !open.contains(&pr)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The status door's verdict for one PR, or None when the door did not answer.
fn status_read(root: &Path, pr: u64) -> Option<Read> {
    let payload = json!({"cwd": root.display().to_string(), "pr": pr});
    let (code, stdout, _) = crate::pr_status::cache::run_door("status-read", &payload);
    if code > 3 {
        return None;
    }
    let answer: Value = serde_json::from_str(stdout.trim()).ok()?;
    Some(Read {
        pr,
        head: answer.get("head").and_then(Value::as_str)?.to_string(),
        ready: answer.get("ready").and_then(Value::as_bool)?,
    })
}

pub(crate) fn run_pass(home: &AgentsHome, now: i64) -> Result<(), String> {
    let leads: Vec<String> = match crate::territory::live_teams(&home.registry_json()) {
        Ok(teams) => {
            let mut out: Vec<String> = Vec::new();
            for sid in teams.into_iter().filter_map(|t| t.holder_session) {
                if !sid.is_empty() && !out.contains(&sid) {
                    out.push(sid);
                }
            }
            out
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut slugs = HashSet::new();
    let mut first_error = None;
    for root in crate::daemon::worktree_sweep::registry_repo_roots(home) {
        let root = Path::new(&root);
        // Two clones of one repo are one set of clocks.
        let slug = crate::pr_status::cache::git_slug(root).filter(|s| slugs.insert(s.clone()));
        let Some(slug) = slug else { continue };
        let listing = crate::pr_push::gh_api_pages_conditional(
            "gh",
            root,
            "repos/{owner}/{repo}/pulls?state=open&per_page=100",
        );
        let pulls: Vec<Value> = match &listing {
            Ok(pages) => crate::heal_sweep::pull_rows(pages),
            Err(e) => {
                first_error.get_or_insert(format!("{slug}: open PR listing failed: {e}"));
                continue;
            }
        };
        // A draft is not open for merging: leaving it out of the set drops its
        // clock, so a PR back from draft starts a fresh one.
        let open: HashSet<u64> = pulls
            .iter()
            .filter(|p| p.get("draft").and_then(Value::as_bool) != Some(true))
            .filter_map(|p| p.get("number").and_then(Value::as_u64))
            .collect();
        let reads: Vec<Read> = open
            .iter()
            .filter_map(|pr| status_read(root, *pr))
            .collect();
        let mut phone = crate::operator_notice::notify_operator;
        if let Err(e) = run_repo(
            home,
            now,
            &slug,
            Some(&open),
            &reads,
            &leads,
            &mut crate::burn_watch::run_command,
            &mut phone,
        ) {
            first_error.get_or_insert(format!("{slug}: {e}"));
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        let busy = last.is_some_and(|t| t.elapsed() < INTERVAL);
        if busy || arm.in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Err(e) = run_pass(&home, crate::claims::now_ms()) {
            eprintln!("pr-idle: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn ready(pr: u64, head: &str) -> Read {
        Read {
            pr,
            head: head.into(),
            ready: true,
        }
    }

    #[test]
    fn a_ready_pr_alarms_every_lead_and_the_phone_at_two_hours_and_again_at_six() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let leads = vec!["lead-a".to_string(), "lead-b".to_string()];
        let mail: RefCell<Vec<(String, String)>> = Default::default();
        let mut deliver = |argv: &[String], _: &str| {
            mail.borrow_mut().push((argv[8].clone(), argv[9].clone()));
            (0, "delivered (hosted)".into(), String::new())
        };
        let phoned: RefCell<Vec<String>> = Default::default();
        let mut phone = |title: &str, _: &str, _: Option<&str>| {
            phoned.borrow_mut().push(title.to_string());
            true
        };
        let open: HashSet<u64> = [7].into();
        let reads = [ready(7, "abc")];
        let mut pass = |now: i64| {
            run_repo(
                &home,
                now,
                "o/r",
                Some(&open),
                &reads,
                &leads,
                &mut deliver,
                &mut phone,
            )
            .unwrap()
        };
        let t0 = 1_000_000;
        pass(t0);
        pass(t0 + RAISE_AFTER_MS - 1);
        assert!(mail.borrow().is_empty(), "a fresh green PR is no news");
        pass(t0 + RAISE_AFTER_MS);
        pass(t0 + RAISE_AFTER_MS + 60_000);
        {
            let mail = mail.borrow();
            let to: Vec<&str> = mail.iter().map(|(to, _)| to.as_str()).collect();
            assert_eq!(to, vec!["lead-a", "lead-b"], "each lead once at 2h");
            assert!(mail[0].1.contains("`fno do pr merge 7`"), "{}", mail[0].1);
            assert!(mail[0].1.contains("for 2h00m"), "{}", mail[0].1);
        }
        assert_eq!(
            phoned.borrow().as_slice(),
            ["PR 7 green and unmerged 2h00m"]
        );
        pass(t0 + RERAISE_AFTER_MS);
        pass(t0 + RERAISE_AFTER_MS + 60_000);
        assert_eq!(mail.borrow().len(), 4, "each lead once more at 6h");
        assert!(mail.borrow()[3].1.contains("second alarm"));
        assert_eq!(phoned.borrow().len(), 2);
    }

    #[test]
    fn a_new_head_a_not_ready_read_or_a_closed_pr_resets_the_clock() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let mut quiet = |_: &[String], _: &str| (0, "delivered (hosted)".into(), String::new());
        let mut phone = |_: &str, _: &str, _: Option<&str>| true;
        let open: HashSet<u64> = [7, 8].into();
        let leads: Vec<String> = Vec::new();
        let since = |pr: u64| -> Option<i64> {
            let bytes = std::fs::read(file(&home, "o/r", pr)).ok()?;
            serde_json::from_slice::<Seen>(&bytes)
                .ok()
                .map(|s| s.ready_since_ms)
        };
        let reads = [ready(7, "abc"), ready(8, "def")];
        run_repo(
            &home,
            10,
            "o/r",
            Some(&open),
            &reads,
            &leads,
            &mut quiet,
            &mut phone,
        )
        .unwrap();
        assert_eq!((since(7), since(8)), (Some(10), Some(10)));
        // A push restarts the clock at the new head.
        let pushed = [ready(7, "new"), ready(8, "def")];
        run_repo(
            &home,
            20,
            "o/r",
            Some(&open),
            &pushed,
            &leads,
            &mut quiet,
            &mut phone,
        )
        .unwrap();
        assert_eq!((since(7), since(8)), (Some(20), Some(10)));
        // An unreadable listing drops nothing; a not-ready read drops its clock.
        let red = [Read {
            ready: false,
            ..ready(7, "new")
        }];
        run_repo(&home, 30, "o/r", None, &red, &leads, &mut quiet, &mut phone).unwrap();
        assert_eq!((since(7), since(8)), (None, Some(10)));
        // A clean listing without PR 8 drops its clock; another repo's stays.
        run_repo(
            &home,
            40,
            "o/other",
            None,
            &[ready(8, "x")],
            &leads,
            &mut quiet,
            &mut phone,
        )
        .unwrap();
        let only_seven: HashSet<u64> = [7].into();
        run_repo(
            &home,
            50,
            "o/r",
            Some(&only_seven),
            &[],
            &leads,
            &mut quiet,
            &mut phone,
        )
        .unwrap();
        assert_eq!(since(8), None);
        assert!(file(&home, "o/other", 8).exists());
    }

    #[test]
    fn a_failed_send_retries_only_the_missed_recipients_on_the_next_pass() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let leads = vec!["lead-a".to_string(), "lead-b".to_string()];
        let open: HashSet<u64> = [7].into();
        let reads = [ready(7, "abc")];
        let mut ok = |_: &str, _: &str, _: Option<&str>| true;
        let mut quiet = |_: &[String], _: &str| (0, "delivered (hosted)".into(), String::new());
        // The clock reads 0 as unset, so the passes start past it.
        let t0 = 1_000;
        run_repo(
            &home,
            t0,
            "o/r",
            Some(&open),
            &reads,
            &leads,
            &mut quiet,
            &mut ok,
        )
        .unwrap();
        let sent: RefCell<Vec<String>> = Default::default();
        let mut b_down = |argv: &[String], _: &str| {
            if argv[8] == "lead-b" {
                (1, String::new(), "transport unavailable".to_string())
            } else {
                sent.borrow_mut().push(argv[8].clone());
                (0, "delivered (hosted)".into(), String::new())
            }
        };
        let phones = RefCell::new(0);
        let mut phone = |_: &str, _: &str, _: Option<&str>| {
            *phones.borrow_mut() += 1;
            true
        };
        let at = t0 + RAISE_AFTER_MS;
        assert!(run_repo(
            &home,
            at,
            "o/r",
            Some(&open),
            &reads,
            &leads,
            &mut b_down,
            &mut phone
        )
        .is_err());
        let mut all_up = |argv: &[String], _: &str| {
            sent.borrow_mut().push(argv[8].clone());
            (0, "delivered (hosted)".into(), String::new())
        };
        run_repo(
            &home,
            at + 1,
            "o/r",
            Some(&open),
            &reads,
            &leads,
            &mut all_up,
            &mut phone,
        )
        .unwrap();
        run_repo(
            &home,
            at + 2,
            "o/r",
            Some(&open),
            &reads,
            &leads,
            &mut all_up,
            &mut phone,
        )
        .unwrap();
        assert_eq!(sent.borrow().as_slice(), ["lead-a", "lead-b"]);
        assert_eq!(*phones.borrow(), 1, "the phone notice sent once");
        // A 2h delivery still pending at 6h starts over: lead-a hears the
        // second alarm even though it heard the first.
        let nine: HashSet<u64> = [9].into();
        let reads = [ready(9, "def")];
        let mut run = |now: i64, runner: crate::burn_watch::Runner<'_>| {
            run_repo(
                &home,
                now,
                "o/r",
                Some(&nine),
                &reads,
                &leads,
                runner,
                &mut phone,
            )
        };
        run(t0, &mut all_up).unwrap();
        sent.borrow_mut().clear();
        assert!(run(t0 + RAISE_AFTER_MS, &mut b_down).is_err());
        assert!(run(t0 + RERAISE_AFTER_MS, &mut b_down).is_err());
        assert_eq!(sent.borrow().as_slice(), ["lead-a", "lead-a"]);
    }
}

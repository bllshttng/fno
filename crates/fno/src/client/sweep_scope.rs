//! One sweep choice, from modal row to prune flags : the
//! counts the modal offers, and the exact flag expansion each choice
//! promises - pure, so the promised expansion is unit-testable without
//! spawning the command.

use super::*;

/// One default `mux workspace prune --dry-run --json` reading, as the modal
/// draws it. Each row's count bounds what its tap closes, and each bucket the
/// sweep leaves in place gets a `kept` line naming why, so a board that still
/// looks full says why on screen instead of reading as a broken button.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SweepCounts {
    pub(super) tabs: usize,
    pub(super) used: usize,
    pub(super) dead: usize,
    /// Named-workspace tabs a `--tabs-only --include-named` apply closes.
    pub(super) named: usize,
    /// Stale squad rows a bare prune removes (`pruned_count`).
    pub(super) squads: usize,
    /// Stopped session rows the registry reaper would retire.
    pub(super) stopped: usize,
    /// One `what count - why` line per non-zero kept bucket.
    pub(super) kept: Vec<String>,
}

/// Read a finished prune's JSON receipt into the message the UI loop acts on.
/// A missing field means the two processes disagree about the JSON shape (a
/// stale deployed binary). The probe fails loud and names the remedy: a zero
/// default would grey a row out and LIE about a population the stale CLI
/// cannot count.
pub(super) fn parse_sweep_receipt(action: SweepAction, receipt: &serde_json::Value) -> SweepMsg {
    let n = |key: &str| receipt[key].as_u64().map(|v| v as usize);
    let stale = || {
        SweepMsg::Failed(
            "prune output missing count fields - stale fno CLI? run fno doctor update".into(),
        )
    };
    match action {
        SweepAction::Counts => {
            let Some(counts) = read_counts(&n) else {
                return stale();
            };
            match receipt["notice"].as_str() {
                Some(notice) if !notice.is_empty() => SweepMsg::Failed(notice.to_string()),
                _ => SweepMsg::Counts(counts),
            }
        }
        SweepAction::Apply(_) => match (n("tabs_closed"), n("members_reaped"), n("pruned_count")) {
            (Some(closed), Some(reaped), Some(removed)) => SweepMsg::Applied {
                closed,
                reaped,
                removed,
                stopped: 0,
            },
            _ => stale(),
        },
    }
}

fn read_counts(n: &impl Fn(&str) -> Option<usize>) -> Option<SweepCounts> {
    let (named_tabs, named) = (n("tabs_skipped_named")?, n("tabs_named_would_close")?);
    let used = n("tabs_used_shells")?;
    let kept = [
        // With the flag off, each spent shell is also counted not-pristine,
        // and the + used shells row already offers it.
        (
            n("tabs_kept_not_pristine")?.saturating_sub(used),
            "not-pristine tabs",
            "agent, command, or unmeasured shell",
        ),
        (
            n("tabs_kept_last_in_squad")?,
            "last tabs",
            "closing one removes its workspace",
        ),
        (n("tabs_kept_zero_panes")?, "empty tabs", "no pane to judge"),
        (
            n("tabs_kept_not_probed")?,
            "unprobed tabs",
            "no server answered",
        ),
        (
            named_tabs.saturating_sub(named),
            "named tabs",
            "last tab, empty, or in use",
        ),
        (
            n("kept_protected")?,
            "workspace rows",
            "live or origin present",
        ),
        (n("kept_unknown")?, "workspace rows", "liveness unknown"),
        (
            n("skipped_named")?,
            "named workspace rows",
            "CLI only: prune --include-named",
        ),
        (n("members_kept_live")?, "agents", "still live"),
        (n("members_kept_unknown")?, "agents", "liveness unknown"),
    ]
    .into_iter()
    .filter(|(count, ..)| *count > 0)
    .map(|(count, what, why)| format!("{what} {count} - {why}"))
    .collect();
    Some(SweepCounts {
        tabs: n("tabs_would_close")?,
        used,
        dead: n("members_reaped")?,
        named,
        squads: n("pruned_count")?,
        stopped: 0,
        kept,
    })
}

/// Build the centered sweep-threads choice modal from one
/// `mux workspace prune --dry-run` reading. A zero count greys its entry out
/// (0 targets, so arrows skip it and a click is swallowed); with every count
/// zero there is nothing to choose, and the header says so. Each row carries
/// its OWN count, and the tap IS the confirmation - a widening is a separate
/// row, never a rider on an existing choice, so the sweep's posture is
/// visible before it acts.
pub(super) fn build_sweep_modal(c: &SweepCounts) -> AuxPopup {
    let (tabs, used, dead) = (c.tabs, c.used, c.dead);
    let any = tabs > 0 || dead > 0 || used > 0;
    let stopped = c.stopped;
    // (review) Every "+" row is ADDITIVE on the CLI: its apply also closes
    // the surplus pristine tabs, so the hint names the real total - the
    // count a row shows must bound what its tap closes.
    let choices = [
        (
            format!("tabs ({tabs})"),
            "close surplus shell tabs".to_string(),
            tabs > 0,
            AuxAction::SweepTabs,
        ),
        (
            format!("+ used shells ({used})"),
            format!(
                "close spent shells plus the {tabs} surplus tabs ({} total)",
                tabs + used
            ),
            used > 0,
            AuxAction::SweepUsedShells,
        ),
        (
            format!("dead agents ({dead})"),
            "reap dead member rows".to_string(),
            dead > 0,
            AuxAction::SweepDeadAgents,
        ),
        (
            "both".to_string(),
            "tabs, spent shells, and dead agents".to_string(),
            any,
            AuxAction::SweepBoth,
        ),
        (
            format!("+ named tabs ({})", c.named),
            format!(
                "close named-workspace tabs plus the {tabs} surplus tabs ({} total)",
                tabs + c.named
            ),
            c.named > 0,
            AuxAction::SweepNamed,
        ),
        (
            format!("+ stale workspace rows ({})", c.squads),
            format!("remove them plus the {tabs} surplus tabs and {dead} dead agents"),
            c.squads > 0,
            AuxAction::SweepSquads,
        ),
        (
            format!("stopped sessions ({stopped})"),
            "retire finished session rows the reaper would remove (transcripts stay)".to_string(),
            stopped > 0,
            AuxAction::SweepStopped,
        ),
    ];
    let mut rows = vec![PopupRow::Header("sweep threads".into()), PopupRow::Rule];
    let mut actions: Vec<AuxAction> = Vec::new();
    for (label, hint, enabled, action) in choices {
        rows.push(PopupRow::Entry {
            glyph: "♺".into(),
            label,
            hint,
            enabled,
        });
        if enabled {
            actions.push(action);
        }
    }
    if actions.is_empty() {
        rows.push(PopupRow::Header("nothing to sweep".into()));
    }
    if !c.kept.is_empty() {
        rows.push(PopupRow::Rule);
        rows.push(PopupRow::Header("left in place, and why".into()));
        rows.extend(c.kept.iter().cloned().map(PopupRow::Header));
    }
    AuxPopup {
        popup: Popup::new(rows, Anchor::Center)
            .title("sweep threads")
            .footer("esc close"),
        actions,
    }
}

/// The prune flags one sweep scope maps to, pure so the expansion each
/// choice promises is unit-testable without spawning the command. `both` means
/// all three populations: tabs, spent shells, and dead agents -
/// with tabs 0 and used shells 21, a both that skipped the shells half
/// closed nothing while the operator watched.
pub(super) fn sweep_apply_args(scope: SweepScope, args: &mut Vec<String>) {
    match scope {
        SweepScope::Tabs => args.push("--tabs-only".to_string()),
        // The opt-in half: tabs-only PLUS the flag that
        // widens the tab fold to spent shells. Never the default.
        SweepScope::UsedShells => {
            args.push("--tabs-only".to_string());
            args.push("--include-used-shells".to_string());
        }
        SweepScope::Dead => args.push("--dead-only".to_string()),
        // Both halves, and nothing else: bare prune would also remove
        // stale squad rows, which only their own row offers to remove.
        SweepScope::Both => {
            args.push("--tabs-only".to_string());
            args.push("--dead-only".to_string());
            args.push("--include-used-shells".to_string());
        }
        // Tab fold only: `--include-named` alone would also remove named
        // squad rows, which the modal names as CLI-only.
        SweepScope::Named => {
            args.push("--tabs-only".to_string());
            args.push("--include-named".to_string());
        }
        // The bare prune, the one scope that removes stale squad rows.
        SweepScope::Squads => {}
        // Never reaches prune: `run_sweep_command` runs the registry reaper.
        SweepScope::Stopped => {}
    }
}

/// The operator tapped a choice: the modal named the counts, so the tap IS
/// the confirmation. Queue the apply for the run loop (or say why not).
pub(super) fn begin_sweep_apply(view: &mut View, scope: SweepScope) {
    view.aux = None;
    if view.sweep_inflight {
        view.set_notice("a sweep is already running".into());
    } else {
        view.sweep_action = Some(SweepAction::Apply(scope));
    }
}

/// Run one `mux workspace prune` command off the UI thread: a `--dry-run --json`
/// probe for the choice modal's counts, or a scoped apply whose JSON receipt
/// becomes the notice. Bounded so a wedged server cannot hold the UI loop's
/// sweep slot forever; the counts parse fails loud rather than opening a
/// modal with fabricated zeros.
pub(super) async fn run_sweep_command(action: SweepAction) -> SweepMsg {
    if action == SweepAction::Apply(SweepScope::Stopped) {
        return match run_json("reap", reap_args(false), 60).await {
            Ok(v) => SweepMsg::Applied {
                closed: 0,
                reaped: 0,
                removed: 0,
                stopped: retired_count(&v),
            },
            Err(e) => SweepMsg::Failed(e),
        };
    }
    let mut args = vec![
        "mux".to_string(),
        "workspace".to_string(),
        "prune".to_string(),
        "--json".to_string(),
    ];
    let apply_secs = match action {
        SweepAction::Counts => {
            args.push("--dry-run".to_string());
            10
        }
        SweepAction::Apply(scope) => {
            sweep_apply_args(scope, &mut args);
            // Each folded tab is one control roundtrip; a big workspace
            // sweep legitimately outlasts a modal probe.
            60
        }
    };
    let probe = action == SweepAction::Counts;
    // Both counts read at once: the modal waits for the slower, not the sum.
    let (pruned, reaped) = tokio::join!(run_json("prune", args, apply_secs), async {
        if probe {
            run_json("reap", reap_args(true), 60).await.ok()
        } else {
            None
        }
    });
    let parsed = match pruned {
        Ok(v) => v,
        Err(e) => return SweepMsg::Failed(e),
    };
    let mut msg = parse_sweep_receipt(action, &parsed);
    if let SweepMsg::Counts(counts) = &mut msg {
        // A failed reap probe leaves the row greyed out: zero, never a guess.
        counts.stopped = reaped.map_or(0, |v| retired_count(&v));
    }
    msg
}

fn retired_count(receipt: &serde_json::Value) -> usize {
    receipt["retired"].as_array().map_or(0, Vec::len)
}

/// The daemon's reaper, so the modal's count is what the tap retires.
fn reap_args(dry_run: bool) -> Vec<String> {
    let mut args: Vec<String> = ["agents", "registry", "reap", "--json"]
        .map(String::from)
        .into();
    if dry_run {
        args.push("--dry-run".into());
    }
    args
}

/// Run one bounded `fno` command and parse its JSON stdout. `what` names the
/// command in the failure line.
async fn run_json(what: &str, args: Vec<String>, secs: u64) -> Result<serde_json::Value, String> {
    let mut command = crate::process_admission::tokio_command(crate::server::fno_bin());
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let output = match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("{what} spawn failed: {e}")),
        Err(_) => return Err(format!("{what} timed out")),
    };
    if !output.status.success() {
        return Err(format!(
            "{what} exited {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("{what} output unparseable ({e})"))
}

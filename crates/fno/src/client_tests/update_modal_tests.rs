//! The update menu/modal restart and release-upgrade surface (kept out of
//! the over-budget client_tests.rs; each shrink is banked).

use super::tests::view_with_agents;
use super::*;
use crate::client::release_check::{Channel, ReleaseNotesSection, ReleaseOutcome};
use crate::client::update_menu::{
    ReleaseNoteLine, ReleaseNotes, ReleaseNotesGroup, RevisionsView, RunningRow, UpdateOutcome,
    UpdateProbe, UpdateReadiness,
};

fn degraded_release_probe(release: ReleaseOutcome, running: Vec<RunningRow>) -> UpdateProbe {
    let running_stale = running.len();
    UpdateProbe {
        readiness: UpdateOutcome::Ok(UpdateReadiness {
            update_ready: false,
            installed_rev: None,
            source_rev: None,
            installed_version: None,
            source_prs_ahead: None,
            changelog: vec![],
            release_notes: None,
            guidance: "update check degraded (local source tree unavailable)".into(),
            degraded: Some("local source tree unavailable".into()),
            running,
            running_stale,
            source_pin: None,
            revisions: None,
        }),
        release,
    }
}

fn entry_labels(popup: &AuxPopup) -> Vec<String> {
    popup
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect()
}

/// Every row's visible label (headers, body text, entries): for assertions
/// that care about presence, not row kind.
fn row_labels(popup: &AuxPopup) -> Vec<String> {
    popup
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Header(h) | PopupRow::Text(h) | PopupRow::FullWidth(h) => Some(h.clone()),
            PopupRow::Entry { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect()
}

fn newer_uv() -> ReleaseOutcome {
    ReleaseOutcome::Newer {
        channel: Channel::Uv,
        installed: "0.3.1".into(),
        latest: "0.3.2".into(),
        notes: vec![],
    }
}

/// A release install with a degraded source check still learns a newer
/// release exists, and the modal's upgrade entry maps to its action.
#[test]
fn release_newer_shows_menu_row_and_modal_upgrade_entry() {
    let probe = degraded_release_probe(newer_uv(), vec![]);
    let menu = build_sideline_menu(Anchor::Center, Some(&probe), experimental::Flags::default());
    assert_eq!(entry_labels(&menu)[0], "release 0.3.2 available");
    assert_eq!(menu.actions[0], AuxAction::OpenUpdate);

    let modal = build_update_modal(Some(&probe));
    let labels = entry_labels(&modal);
    let i = labels
        .iter()
        .position(|l| l == "upgrade now: fno update")
        .expect("upgrade entry");
    assert_eq!(modal.actions[i], AuxAction::UpgradeRelease(Channel::Uv));
}
/// A failed release check is named, never read as current.
#[test]
fn release_degraded_and_current_render_one_header_and_no_action() {
    for (release, want) in [
        (
            ReleaseOutcome::Degraded("GitHub latest release: curl exited 6".into()),
            "release check failed: GitHub latest release: curl exited 6",
        ),
        (
            ReleaseOutcome::Current {
                channel: Channel::Brew,
            },
            "release: current (brew)",
        ),
    ] {
        let modal = build_update_modal(Some(&degraded_release_probe(release, vec![])));
        assert!(
            modal
                .popup
                .rows
                .iter()
                .any(|r| matches!(r, PopupRow::Header(h) if h == want)),
            "{want}"
        );
        assert!(modal.actions.is_empty());
    }
}

/// A tap queues the upgrade once; a second tap says one is running; the
/// verdict lands as a notice and re-arms the probe. The restart tap queues
/// nothing: nothing inflight, it detaches with the restart armed (run_inner
/// runs the foreground restart); anything inflight, it says so and stays.
#[tokio::test]
async fn upgrade_tap_queues_once_and_verdict_rearms_probe() {
    let mut v = view_with_agents(vec![]);
    let mut buf = Vec::new();
    assert!(matches!(
        execute_aux_action(&mut v, AuxAction::RestartAgents, &mut buf)
            .await
            .unwrap(),
        DispatchFlow::Detach
    ));
    assert!(v.restart_pending, "the clean tap arms the restart unwind");
    assert_eq!(v.update_command_want, None, "the restart never queues");
    v.restart_pending = false;
    execute_aux_action(&mut v, AuxAction::UpgradeRelease(Channel::Uv), &mut buf)
        .await
        .unwrap();
    assert_eq!(v.update_command_want, Some(Channel::Uv));
    v.update_command_want = None;
    v.update_command_inflight = true;
    execute_aux_action(&mut v, AuxAction::RestartAgents, &mut buf)
        .await
        .unwrap();
    assert_eq!(v.update_command_want, None);
    assert!(v
        .notice
        .as_ref()
        .is_some_and(|(n, _)| n == "an update action is already running"));

    v.update_probe_want = false;
    v.land_update_verdict("upgrade ok: Updated fno v0.3.1 -> v0.3.2".into());
    assert!(!v.update_command_inflight);
    assert!(v.update_probe_want);
    assert!(v
        .notice
        .as_ref()
        .is_some_and(|(n, _)| n.starts_with("upgrade ok:")));
}

/// x-f188 AC7-HP: two stale components and no update pending -> the menu
/// shows the restart row and the modal names each component with what
/// restart does and what survives, then offers the action.
#[test]
fn update_modal_names_stale_processes_and_offers_restart() {
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("same".into()),
        source_rev: Some("same".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance: "installed same is current; 2 running process(es) are older builds".into(),
        degraded: None,
        running: vec![
            RunningRow {
                component: "daemon".into(),
                name: Some("agents home".into()),
                verdict: "stale".into(),
                on_restart: "restarts".into(),
                survives: "workers and panes".into(),
            },
            RunningRow {
                component: "pane-keeper".into(),
                name: Some("main-1991".into()),
                verdict: "stale".into(),
                on_restart: "kept".into(),
                survives: "its pane; current only when that pane ends".into(),
            },
            RunningRow {
                component: "store-keeper".into(),
                name: Some("g".into()),
                verdict: "current".into(),
                on_restart: "cycles; the next read respawns it".into(),
                survives: "the graph on disk".into(),
            },
        ],
        running_stale: 2,
        source_pin: None,
        revisions: None,
    });
    let menu = build_sideline_menu(
        Anchor::Center,
        Some(&outcome.clone().into()),
        experimental::Flags::default(),
    );
    let labels: Vec<&str> = menu
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        labels.contains(&"restart: 2 stale, panes kept"),
        "the menu names the stale count: {labels:?}"
    );

    // Twenty stale pane keepers fold into one count line; full revs cut to
    // ten characters; no row runs past the popup width.
    let mut wide = outcome.clone();
    if let UpdateOutcome::Ok(r) = &mut wide {
        r.installed_rev = Some("a".repeat(40));
        r.source_rev = Some("b".repeat(40));
        let keeper = r.running[1].clone();
        r.running.extend(std::iter::repeat_n(keeper, 19));
        r.changelog
            .push(format!("feat: {}", "a long subject ".repeat(8)));
    }
    let wide = build_update_modal(Some(&wide.into()));
    let headers: Vec<&str> = wide
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Header(h) => Some(h.as_str()),
            _ => None,
        })
        .collect();
    assert!(headers.contains(&"aaaaaaaaaa -> bbbbbbbbbb"), "{headers:?}");
    assert_eq!(
        row_labels(&wide)
            .iter()
            .filter(|h| h.contains("keeper"))
            .count(),
        2,
        "the count line and the promise: {headers:?}"
    );
    assert!(
        row_labels(&wide).contains(&"20 pane keepers on the old build".to_string()),
        "{headers:?}"
    );
    let modal = build_update_modal(Some(&outcome.clone().into()));
    let text: Vec<String> = modal
        .popup
        .rows
        .iter()
        .map(|r| match r {
            PopupRow::Header(h) | PopupRow::Text(h) => h.clone(),
            PopupRow::Entry { label, .. } => label.clone(),
            PopupRow::Rule => "-".into(),
            _ => String::new(),
        })
        .collect();
    let body = text.join(" ");
    assert!(
        body.contains("daemon agents home: restarts; keeps workers and panes"),
        "each stale row names what restart does: {body}"
    );
    assert!(body.contains("1 pane keeper on the old build"), "{body}");
    assert!(
        !body.contains("store-keeper"),
        "current rows are not listed: {body}"
    );
    assert!(
        body.contains("restart keeps panes. keepers stay on the old build until their pane ends."),
        "{body}"
    );
    assert!(
        body.contains(
            "restart detaches, runs `fno agents restart --mux` in the foreground, reattaches."
        ),
        "the modal names the flow the tap starts: {body}"
    );
    assert!(
        modal.actions.contains(&AuxAction::RestartAgents),
        "the modal offers the restart action"
    );
    let entry_label = modal
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .find(|l| *l == "restart now (keeps panes)");
    assert!(entry_label.is_some(), "the action row is present");
}
/// Only card message previews use ellipsis; every other row field keeps the no-marker clipping rule (d-36438ea4).
#[test]
fn no_row_cuts_text_with_an_ellipsis() {
    use super::tests::{agent_row, named_meta, shot_view};

    let long_name = "n".repeat(120);
    let long_tail = format!("{} end", "a long worker message ".repeat(12));
    let long_cwd = "c".repeat(60);
    let mut long_agent = agent_row(&long_name, 10, None, false);
    long_agent.tail = Some(long_tail.clone());
    long_agent.cwd_base = Some(long_cwd.clone());
    let plain = agent_row("w", 11, None, false);

    for cols in [50u16, 80, 120, 200] {
        for density in [
            crate::view_store::Density::Regular,
            crate::view_store::Density::Extended,
            crate::view_store::Density::Slim,
        ] {
            let mut v = shot_view(
                (40, cols),
                vec![named_meta(1, "footnote", &["main"], 0)],
                vec![long_agent.clone(), plain.clone()],
            );
            v.density = density;
            let text = crate::vt::frame_text(&v.compose());
            for line in text.lines() {
                assert!(
                    !line.contains('\u{2026}') || line.contains(" tok · …"),
                    "{density:?} cut a row at {cols} columns: {line:?}"
                );
            }
        }

        // The backlog board's stats and card lines clip too.
        let mut b = super::backlog_board::BoardView::new(0);
        let rows = vec![
            serde_json::json!({"id": "x-1", "status": "ready", "priority": "p2", "title": long_tail}),
            serde_json::json!({"id": "x-2", "status": "in_progress", "priority": "p2", "title": long_tail}),
        ];
        b.inputs = Some(crate::backlog_model::fixture(rows));
        let q = b.query.to_query().expect("the default query parses");
        b.body = Some(crate::backlog_model::board(b.inputs.as_ref().unwrap(), &q));
        let (lines, _) = super::backlog_board::render(&b, cols as usize);
        for line in &lines {
            assert!(
                !line.text.contains('\u{2026}'),
                "board cut a row at {cols} columns: {:?}",
                line.text
            );
        }

        // A pane border with a long name and long edge fields.
        let fields = crate::pane_border::EdgeFields {
            name: &long_name,
            status: Some(('●', "Working")),
            model: Some("claude-opus-5-5[1m]"),
            node: Some(&long_cwd),
            branch: Some("feature/x-a38d-fixed-height-mux-rows"),
            ctx: Some("ctx 48% of 1.0M"),
            observers: 0,
        };
        let e = crate::pane_border::edges(
            &fields,
            crate::tree::Rect {
                x: 0,
                y: 0,
                cols,
                rows: 12,
            },
            false,
        );
        for edge in [&e.top, &e.bottom] {
            let text: String = edge.iter().map(|(c, _)| *c).collect();
            assert!(
                !text.contains('\u{2026}'),
                "pane border cut a span at {cols} columns: {text:?}"
            );
        }
    }
}

/// x-f188 AC7-EDGE: a readiness payload from an older fno with no `running`
/// key still parses and offers no restart action.
#[test]
fn update_payload_without_running_key_parses_and_offers_no_restart() {
    let payload = serde_json::json!({
        "update_ready": false,
        "installed_rev": "aaa1111",
        "source_rev": "aaa1111",
        "guidance": "up to date at aaa1111 - no update pending, 0 shell(s) unaffected",
        "degraded": null
    });
    let parsed: UpdateReadiness = serde_json::from_value(payload).expect("parses");
    assert!(parsed.running.is_empty());
    assert_eq!(parsed.running_stale, 0);

    let outcome = UpdateOutcome::Ok(parsed);
    let modal = build_update_modal(Some(&outcome.clone().into()));
    assert!(
        !modal.actions.contains(&AuxAction::RestartAgents),
        "no restart action without stale rows"
    );
}

/// The named revisions line and the mixed-build header print verbatim from
/// the resolver's revisions block; a payload without the key (an older fno)
/// renders neither row.
#[test]
fn update_modal_prints_the_named_revisions_and_the_mixed_flag() {
    let rev_a = "2e55d52aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let rev_b = "19bbc27bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let rev_c = "ffc2af6ccccccccccccccccccccccccccccccccccc";
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("aaa1111".into()),
        source_rev: Some("bbb2222".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance: "up to date at bbb2222 - no update pending, 0 shell(s) unaffected".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: Some(RevisionsView {
            summary: Some(format!(
                "build revs: fno-agents {}, fno {}; crates/ pin {}",
                &rev_a[..12],
                &rev_b[..12],
                &rev_c[..12]
            )),
            mixed_detail: Some(format!(
                "mixed component builds: fno-agents at {} vs fno at {}",
                &rev_a[..12],
                &rev_b[..12]
            )),
        }),
    });
    let modal = build_update_modal(Some(&outcome.into()));
    let labels = row_labels(&modal);
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("build revs: fno-agents ") && l.contains("crates/ pin")),
        "the side-by-side line prints: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("mixed component builds:")),
        "the mixed flag prints: {labels:?}"
    );

    let plain = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("same".into()),
        source_rev: Some("same".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance: "up to date at same - no update pending, 0 shell(s) unaffected".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let modal = build_update_modal(Some(&plain.into()));
    assert!(
        !row_labels(&modal).iter().any(|l| l.contains("build revs:")),
        "no revisions row without the block"
    );
    assert!(!row_labels(&modal)
        .iter()
        .any(|l| l.starts_with("mixed component builds:")));
}

/// AC3-HP mirrored client-side: not-ready builds the menu with no row.
#[test]
fn sideline_menu_omits_update_row_when_not_ready() {
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("same".into()),
        source_rev: Some("same".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance: "up to date at same - no update pending, 0 shell(s) unaffected".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let menu = build_sideline_menu(
        Anchor::Center,
        Some(&outcome.clone().into()),
        experimental::Flags::default(),
    );
    assert!(!menu.actions.contains(&AuxAction::OpenUpdate));
}
/// Regression: a successfully-parsed probe (Python `--check` always exits
/// 0) can still be internally degraded - `update_ready: false` with
/// `degraded: Some(_)`. That must still surface a menu row rather than
/// silently falling to the `_ => {}` arm, which would hide a real check
/// failure the operator has no other way to see.
#[test]
fn sideline_menu_shows_row_for_ok_but_internally_degraded_probe() {
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("same".into()),
        source_rev: Some("same".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance: "update check degraded (fno mux ls --json failed) - ...".into(),
        degraded: Some("fno mux ls --json failed".into()),
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let menu = build_sideline_menu(
        Anchor::Center,
        Some(&outcome.clone().into()),
        experimental::Flags::default(),
    );
    let labels: Vec<&str> = menu
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(labels[0], "update check degraded");
    assert_eq!(menu.actions[0], AuxAction::OpenUpdate);
}

/// AC7-HP: a behind source with no update pending outranks the
/// restart row - the menu names the distance and offers the modal first.
#[test]
fn sideline_menu_names_source_behind_origin() {
    let payload = serde_json::json!({
        "update_ready": false,
        "installed_rev": "aaa1111",
        "source_rev": "aaa1111",
        "guidance": "source checkout aaa1111 is 14 commit(s) behind origin/main bbb2222; \
                     merged changes there are not installed. Sync it, then run fno doctor update",
        "degraded": null,
        "running": [],
        "running_stale": 2,
        "source_pin": {"behind": 14}
    });
    let parsed: UpdateReadiness = serde_json::from_value(payload).expect("parses");
    let outcome = UpdateOutcome::Ok(parsed);
    let menu = build_sideline_menu(
        Anchor::Center,
        Some(&outcome.clone().into()),
        experimental::Flags::default(),
    );
    let labels: Vec<&str> = menu
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(labels[0], "source 14 behind origin");
    assert_eq!(menu.actions[0], AuxAction::OpenUpdate);
}

/// AC8-EDGE: a payload with no `source_pin` key, or with a null
/// `behind`, parses and builds today's rows (no behind row).
#[test]
fn sideline_menu_without_source_pin_keeps_rows() {
    for pin in [serde_json::Value::Null, serde_json::json!({"behind": null})] {
        let mut payload = serde_json::json!({
            "update_ready": false,
            "installed_rev": "same",
            "source_rev": "same",
            "guidance": "up to date at same - no update pending, 0 shell(s) unaffected",
            "degraded": null,
            "running": [],
            "running_stale": 0
        });
        if !pin.is_null() {
            payload["source_pin"] = pin.clone();
        }
        let parsed: UpdateReadiness = serde_json::from_value(payload).expect("parses");
        let menu = build_sideline_menu(
            Anchor::Center,
            Some(&UpdateOutcome::Ok(parsed).into()),
            experimental::Flags::default(),
        );
        assert!(
            !menu.actions.contains(&AuxAction::OpenUpdate),
            "no behind row for pin {pin:?}"
        );
    }
}
/// AC5-HP (moved from the over-budget client_tests.rs): the overlay carries
/// the version pair, changelog, and guidance.
#[test]
fn update_modal_renders_version_pair_changelog_and_guidance() {
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: true,
        installed_rev: Some("aaa1111".into()),
        source_rev: Some("bbb2222".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec!["fix(x): thing".into(), "feat(y): other thing".into()],
        release_notes: None,
        guidance: "update ready bbb2222 - wire unchanged - 14 shells survive".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let modal = build_update_modal(Some(&outcome.clone().into()));
    let headers: Vec<&str> = modal
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Header(h) => Some(h.as_str()),
            _ => None,
        })
        .collect();
    assert!(headers.contains(&"aaa1111 -> bbb2222"));
    assert!(row_labels(&modal).contains(&"fix(x): thing".to_string()));
    assert!(row_labels(&modal).contains(&"feat(y): other thing".to_string()));
    assert!(row_labels(&modal)
        .iter()
        .any(|h| h.contains("14 shells survive")));

    // Shaped notes win over the raw changelog: highlights lead as tappable
    // Entries carrying OpenPr (actions pair with selectable rows by index),
    // area groups follow as Headers, the hidden count renders, and a notes
    // payload with no rows falls back to the raw subjects.
    let notes = ReleaseNotes {
        highlights: vec![ReleaseNoteLine {
            pr: Some(105),
            url: Some("https://github.com/o/r/pull/105".into()),
            text: "card rows".into(),
        }],
        groups: vec![ReleaseNotesGroup {
            area: "mux".into(),
            lines: vec![ReleaseNoteLine {
                pr: Some(104),
                url: None,
                text: "stop the crash".into(),
            }],
        }],
        hidden_line: Some("3 test/docs/ci/chore PRs hidden".into()),
    };
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: true,
        installed_rev: Some("aaa1111".into()),
        source_rev: Some("bbb2222".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec!["fix(x): raw subject".into()],
        release_notes: Some(notes),
        guidance: "update ready bbb2222 - wire unchanged - 14 shells survive".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let modal = build_update_modal(Some(&outcome.clone().into()));
    let entry_labels: Vec<&str> = modal
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    // Only the URL line is an Entry; no stale rows means no restart row.
    assert_eq!(entry_labels, vec!["card rows (#105)"]);
    let headers: Vec<&str> = modal
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Header(h) => Some(h.as_str()),
            _ => None,
        })
        .collect();
    assert!(headers.contains(&"mux"));
    assert!(row_labels(&modal).contains(&"stop the crash (#104)".to_string()));
    assert!(row_labels(&modal).contains(&"3 test/docs/ci/chore PRs hidden".to_string()));
    assert!(!row_labels(&modal).contains(&"fix(x): raw subject".to_string()));
    assert_eq!(
        modal.actions,
        vec![AuxAction::OpenPr("https://github.com/o/r/pull/105".into())]
    );

    let empty = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: true,
        installed_rev: Some("aaa1111".into()),
        source_rev: Some("bbb2222".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec!["fix(x): raw subject".into()],
        release_notes: Some(ReleaseNotes {
            highlights: vec![],
            groups: vec![],
            hidden_line: None,
        }),
        guidance: "update ready bbb2222 - wire unchanged - 14 shells survive".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let modal = build_update_modal(Some(&empty.clone().into()));
    assert!(row_labels(&modal).contains(&"fix(x): raw subject".to_string()));
}
/// The source modal leads with the installed version and the distance in
/// PRs; an older payload (or a zero/unknown distance) falls back to the
/// bare sha pair.
#[test]
fn update_modal_shows_version_and_pr_distance_or_falls_back() {
    let ready = |version: Option<&str>, ahead: Option<u64>| {
        UpdateOutcome::Ok(UpdateReadiness {
            update_ready: true,
            installed_rev: Some("af56e2f24e".into()),
            source_rev: Some("6c3024996f".into()),
            installed_version: version.map(str::to_string),
            source_prs_ahead: ahead,
            changelog: vec![],
            release_notes: None,
            guidance: "update ready - detach, fno doctor update, reattach".into(),
            degraded: None,
            running: vec![],
            running_stale: 0,
            source_pin: None,
            revisions: None,
        })
    };
    let headers = |probe: UpdateProbe| -> Vec<String> {
        build_update_modal(Some(&probe))
            .popup
            .rows
            .iter()
            .filter_map(|r| match r {
                PopupRow::Header(h) => Some(h.clone()),
                _ => None,
            })
            .collect()
    };
    let has = |probe: UpdateProbe, want: &str| headers(probe).iter().any(|h| h == want);
    assert!(has(
        ready(Some("0.4.1"), Some(2)).into(),
        "fno 0.4.1 at af56e2f24e, main is 2 PRs ahead"
    ));
    assert!(has(
        ready(Some("0.4.1"), Some(1)).into(),
        "fno 0.4.1 at af56e2f24e, main is 1 PR ahead"
    ));
    for fallback in [
        ready(None, None),
        ready(Some("0.4.1"), Some(0)),
        ready(None, Some(2)),
    ] {
        assert!(has(fallback.into(), "af56e2f24e -> 6c3024996f"));
        // A release install has neither rev: no sha line at all.
        let bare = UpdateOutcome::Ok(UpdateReadiness {
            update_ready: false,
            installed_rev: None,
            source_rev: None,
            installed_version: Some("0.4.0".into()),
            source_prs_ahead: None,
            changelog: vec![],
            release_notes: None,
            guidance: "release install refreshes with the upgrade command".into(),
            degraded: Some("local source tree unavailable".into()),
            running: vec![],
            running_stale: 0,
            source_pin: None,
            revisions: None,
        });
        assert!(!headers(bare.into()).iter().any(|h| h.contains("->")));
    }
}

/// A newer release renders the GitHub release body under the version pair:
/// intro prose and area bullets as Headers (a release body carries no PR
/// urls), and the upgrade entry stays the one action.
#[test]
fn release_newer_renders_release_body_notes() {
    let release = ReleaseOutcome::Newer {
        channel: Channel::Uv,
        installed: "0.4.0".into(),
        latest: "0.4.1".into(),
        notes: vec![
            ReleaseNotesSection {
                area: String::new(),
                bullets: vec!["42 merged pull requests since v0.4.0.".into()],
            },
            ReleaseNotesSection {
                area: "mux".into(),
                bullets: vec!["Portals open operator-owned windows".into()],
            },
        ],
    };
    let modal = build_update_modal(Some(&degraded_release_probe(release, vec![])));
    let headers: Vec<&str> = modal
        .popup
        .rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Header(h) => Some(h.as_str()),
            _ => None,
        })
        .collect();
    assert!(headers
        .contains(&"release 0.4.0 -> 0.4.1 (uv wheel; fno update moves it to ~/.fno/install/bin)"));
    assert!(row_labels(&modal).contains(&"- 42 merged pull requests since v0.4.0.".to_string()));
    assert!(headers.contains(&"mux"));
    assert!(row_labels(&modal).contains(&"- Portals open operator-owned windows".to_string()));
    assert_eq!(entry_labels(&modal), vec!["upgrade now: fno update"]);
    assert_eq!(modal.actions, vec![AuxAction::UpgradeRelease(Channel::Uv)]);
}

/// x-4bfe: a current build leads the dialog with the verdict and offers the
/// optional mux restart when no stale section already offers one.
#[test]
fn update_modal_leads_with_up_to_date_when_current() {
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("same".into()),
        source_rev: Some("same".into()),
        installed_version: Some("0.4.1".into()),
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance: "up to date at same - no update pending, 0 shell(s) unaffected".into(),
        degraded: None,
        running: vec![],
        running_stale: 0,
        source_pin: None,
        revisions: None,
    });
    let modal = build_update_modal(Some(&outcome.into()));
    assert!(matches!(
        modal.popup.rows.first(),
        Some(PopupRow::Header(h)) if h == "up to date"
    ));
    assert!(row_labels(&modal).contains(
        &"optional: restart the mux server to load new mux code. panes are kept.".to_string()
    ));
    assert!(modal.actions.is_empty(), "nothing pending, no action");
}

/// x-4bfe: the keeper count prints once. The payload guidance drops the
/// clause and the stale section's count line is the one print; a degraded
/// probe never reads as up to date.
#[test]
fn update_modal_keeper_count_prints_once() {
    let stale = |name: &str| RunningRow {
        component: "daemon".into(),
        name: Some(name.into()),
        verdict: "stale".into(),
        on_restart: "restarts".into(),
        survives: "panes".into(),
    };
    let keeper = RunningRow {
        component: "pane-keeper".into(),
        name: Some("main-1991".into()),
        verdict: "stale".into(),
        on_restart: "kept".into(),
        survives: "its pane; current only when that pane ends".into(),
    };
    let outcome = UpdateOutcome::Ok(UpdateReadiness {
        update_ready: false,
        installed_rev: Some("same".into()),
        source_rev: Some("same".into()),
        installed_version: None,
        source_prs_ahead: None,
        changelog: vec![],
        release_notes: None,
        guidance:
            "installed same is current; 2 running process(es) are older builds - restart cycles 1"
                .into(),
        degraded: None,
        running: vec![stale("a"), keeper],
        running_stale: 2,
        source_pin: None,
        revisions: None,
    });
    let modal = build_update_modal(Some(&outcome.into()));
    assert!(matches!(
        modal.popup.rows.first(),
        Some(PopupRow::Header(h)) if h == "up to date"
    ));
    let labels = row_labels(&modal);
    assert_eq!(
        labels.iter().filter(|l| l.contains("pane keeper")).count(),
        1,
        "the count prints once: {labels:?}"
    );
    assert!(labels.contains(&"1 pane keeper on the old build".to_string()));
    assert!(modal.actions.contains(&AuxAction::RestartAgents));

    // The producer side of the same contract: the payload guidance stays
    // clause-free, so a regression there cannot re-double the count.
    assert_eq!(
        crate::doctor_update::current_but_stale("same", 2, 1),
        "installed same is current; 2 running process(es) are older builds - restart cycles 1"
    );

    // A degraded probe never claims the build is current.
    let degraded_modal = build_update_modal(Some(
        &(UpdateOutcome::Ok(UpdateReadiness {
            update_ready: false,
            installed_rev: None,
            source_rev: None,
            installed_version: None,
            source_prs_ahead: None,
            changelog: vec![],
            release_notes: None,
            guidance: String::new(),
            degraded: Some("local source tree unavailable".into()),
            running: vec![],
            running_stale: 0,
            source_pin: None,
            revisions: None,
        })
        .into()),
    ));
    assert!(!row_labels(&degraded_modal).contains(&"up to date".to_string()));
}

/// AC1-HP: the skew popup's only action is the update modal's restart, and
/// its rows name the refused command and both wire versions.
#[test]
fn skew_popup_names_the_command_and_versions_and_offers_one_restart() {
    let aux = super::server_skew::build_skew_popup(Some(120), "open a row from another machine");
    assert_eq!(aux.actions, vec![AuxAction::RestartAgents]);
    let labels = row_labels(&aux).join("\n");
    assert!(
        labels.contains("open a row from another machine"),
        "the refused command renders: {labels}"
    );
    assert!(
        labels.contains("v120"),
        "the server's version renders: {labels}"
    );
    let client_v = format!("v{}", crate::proto::PROTO_VERSION);
    assert!(
        labels.contains(&client_v),
        "the client's version renders: {labels}"
    );
}

/// AC2-HP: the tap runs the update modal's restart path - the popup closes,
/// the reattach arm sets, and the client unwinds to detach.
#[tokio::test]
async fn skew_popup_tap_detaches_like_the_update_restart() {
    let mut v = super::tests::view_with_agents(vec![]);
    v.server_proto = Some(120);
    super::server_skew::offer_server_restart(&mut v, "split panes in a direction");
    assert!(v.aux.is_some());
    let mut buf: Vec<u8> = Vec::new();
    let flow = execute_aux_action(&mut v, AuxAction::RestartAgents, &mut buf)
        .await
        .unwrap();
    assert!(matches!(flow, DispatchFlow::Detach));
    assert!(v.restart_pending, "the reattach exec arms");
    assert!(v.aux.is_none(), "the tap closes the popup");
}

/// AC4-HP: on a current server the far-row reach writes its command and
/// opens no popup - the popup is the refusal path only.
#[tokio::test]
async fn far_row_opens_on_a_current_server_without_a_popup() {
    let mut v = super::tests::view_with_agents(vec![]);
    v.server_proto = Some(crate::proto::PROTO_VERSION);
    let mut buf: Vec<u8> = Vec::new();
    super::machine_tree::reach(&mut v, Default::default(), Default::default(), &mut buf)
        .await
        .unwrap();
    assert!(!buf.is_empty(), "ReachRemote is written");
    assert!(v.aux.is_none(), "no skew popup on a current server");
}

/// AC5-HP: the report says what came back and names each pane that did not.
#[test]
fn restart_report_names_missing_panes_and_the_count() {
    let pane = |id: u64| super::server_skew::PreservedPane {
        pane_id: id,
        name: Some("archer".into()),
    };
    let receipt = super::server_skew::RestartReceipt {
        exit_code: 0,
        verdict: "restarted".into(),
        preserved: vec![pane(3), pane(5), pane(9)],
    };
    let report = super::server_skew::restart_report(&receipt, &[3, 9]);
    assert!(
        report.contains("2 of 3 kept panes came back"),
        "the count renders: {report}"
    );
    assert!(
        report.contains("pane 5 (archer) did not come back"),
        "the missing pane is named: {report}"
    );
}

/// AC6-EDGE: a refused restart (the unkept-pane refusal) reads as refused.
#[test]
fn refused_receipt_reads_as_refused() {
    let receipt = super::server_skew::RestartReceipt {
        exit_code: 1,
        verdict: "session \"main\" hosts 1 unkept pane(s)".into(),
        preserved: vec![],
    };
    let report = super::server_skew::restart_report(&receipt, &[]);
    assert!(
        report.starts_with("restart refused:"),
        "the refusal names itself: {report}"
    );
}

/// The receipt lifecycle: the fresh attach reads it once (the notice posts),
/// and the file is gone after the read (AC6's exactly-once).
#[test]
fn receipt_reads_once_and_is_deleted() {
    let mut v = super::tests::view_with_agents(vec![]);
    v.session = "receipt-lifecycle-test".into();
    let dir = crate::proto::mux_dir().join("restart-receipts");
    std::fs::create_dir_all(&dir).unwrap();
    let receipt = super::server_skew::RestartReceipt {
        exit_code: 0,
        verdict: "restarted".into(),
        preserved: vec![super::server_skew::PreservedPane {
            pane_id: 10,
            name: Some("archer".into()),
        }],
    };
    let path = dir.join("receipt-lifecycle-test.json");
    std::fs::write(&path, serde_json::to_string(&receipt).unwrap()).unwrap();

    // two_pane_view holds panes 10 and 11, so pane 10 came back.
    super::server_skew::report_restart_receipt(&mut v);
    let notice = v.notice.as_ref().map(|(s, _)| s.clone());
    assert!(
        notice
            .as_deref()
            .is_some_and(|s| s.contains("1 of 1 kept panes came back")),
        "the report posts: {notice:?}"
    );
    assert!(!path.exists(), "the receipt is gone after the read");
}

use super::*;

#[test]
fn nav_filter_text_is_case_insensitive_substring() {
    // AC2-HP + AC2-UI: typed text narrows to matching labels; case-folded.
    let v = two_pane_view();
    let nav = NavView {
        query: "NOTES".into(),
        state_filter: None,
        cursor: 0,
    };
    let rows = v.nav_filtered(&nav);
    assert_eq!(
        rows.len(),
        2,
        "notes squad + its one tab, footnote excluded"
    );
    assert!(rows
        .iter()
        .all(|r| r.label.to_lowercase().contains("notes")));
}
#[test]
fn nav_filter_finds_an_agent_by_node_pr_harness_and_model() {
    // The screenshot specimen: `find › 3706` read `no matches` for the live
    // worker on that PR. A squad-hosted row and an orphan row (squad 99 is
    // not live) both carry the agent's identity tokens.
    let mut v = two_pane_view();
    let mut hosted = agent_row("worker", 11, None, false);
    hosted.node = Some("ab-c0de".into());
    hosted.pr = Some(3706);
    hosted.harness = Some("codex".into());
    hosted.model = Some("claude-opus-5-5".into());
    let mut orphan = agent_row("stray", 12, None, false);
    orphan.squad = Some(99);
    orphan.pr = Some(3706);
    orphan.model = Some("glm-5.3-flash".into());
    v.layout.agents = vec![hosted, orphan];
    let q = |s: &str| NavView {
        query: s.into(),
        state_filter: None,
        cursor: 0,
    };
    let labels =
        |s: &str| -> Vec<String> { v.nav_filtered(&q(s)).into_iter().map(|r| r.label).collect() };
    for query in ["ab-c0de", "codex", "opus", "claude-opus-5-5"] {
        let hits = labels(query);
        assert_eq!(hits.len(), 1, "{query} finds the one worker: {hits:?}");
        assert!(hits[0].contains("worker"));
    }
    for query in ["3706", "#3706"] {
        assert_eq!(labels(query).len(), 2, "{query} finds both PR 3706 rows");
    }
    assert_eq!(labels("glm-5.3-flash"), vec!["stray".to_string()]);
    let hits = v.nav_filtered(&q("3706"));
    let lines = nav_overlay_lines(&hits, &q("3706"));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("worker") && l.contains("·#3706")),
        "a PR hit names its reason: {lines:?}"
    );
}

#[test]
fn nav_rows_fold_done_through_the_seen_bit() {
    // AC1-HP/AC2-HP (x-4328), at the navigator seam: a seen Done row
    // folds to Idle (the unseen glyph clears); an unseen Done row stays
    // DoneUnseen (surfaced) - `nav_agent_state` must forward `a.seen`,
    // not hardcode it.
    let mut v = two_pane_view();
    v.layout.agents = vec![
        AgentRow {
            spawned_by_name: None,
            lineage_reason: None,
            harness: None,
            model: None,
            route: None,
            reach: Reach::Locate,
            spawned_by_session: None,
            lineage_kind: None,
            harness_session_id: None,
            squad: Some(2),
            name: "finished-seen".into(),
            pane_id: Some(9),
            portal: None,
            badge: Some(AgentBadge::Done),
            reason: None,
            exited: false,
            dnd: false,
            unmeasured: false,
            answerable: None,
            attach_id: None,
            external: false,
            seen: true,
            cwd_base: None,
            tombstone: false,
            subline: None,
            tab: None,
            account: None,
            updated_at: None,
            pr: None,
            pr_session_short: None,
            tail: None,
            role_level: None,
            role_scope: None,
            role_title: None,
            basis: None,
            last_activity_age_s: None,
            resumable: false,
            no_pane_reason: None,
            pane_activity: None,
            ..Default::default()
        },
        AgentRow {
            spawned_by_name: None,
            lineage_reason: None,
            harness: None,
            model: None,
            route: None,
            reach: Reach::Locate,
            spawned_by_session: None,
            lineage_kind: None,
            harness_session_id: None,
            squad: Some(2),
            name: "finished-unseen".into(),
            pane_id: Some(10),
            portal: None,
            badge: Some(AgentBadge::Done),
            reason: None,
            exited: false,
            dnd: false,
            unmeasured: false,
            answerable: None,
            attach_id: None,
            external: false,
            seen: false,
            cwd_base: None,
            tombstone: false,
            subline: None,
            tab: None,
            account: None,
            updated_at: None,
            pr: None,
            pr_session_short: None,
            tail: None,
            role_level: None,
            role_scope: None,
            role_title: None,
            basis: None,
            last_activity_age_s: None,
            resumable: false,
            no_pane_reason: None,
            pane_activity: None,
            ..Default::default()
        },
    ];
    let rows = v.nav_rows();
    let seen_row = rows.iter().find(|r| r.label.ends_with("finished-seen"));
    let unseen_row = rows.iter().find(|r| r.label.ends_with("finished-unseen"));
    assert_eq!(seen_row.map(|r| r.state), Some(PaneState::Idle));
    assert_eq!(unseen_row.map(|r| r.state), Some(PaneState::DoneUnseen));
}

#[test]
fn nav_overlay_lines_show_query_chip_cursor_and_no_matches() {
    // AC1-UI: query line + [all] chip + row 0 with no cursor glyph (the fill
    // marks it). AC2-ERR: an
    // empty filtered result renders `no matches`.
    let v = two_pane_view();
    let nav = NavView {
        query: String::new(),
        state_filter: None,
        cursor: 0,
    };
    let rows = v.nav_filtered(&nav);
    let lines = nav_overlay_lines(&rows, &nav);
    assert!(lines[0].contains("find ›") && lines[0].contains("[all]"));
    assert!(
        !lines[1].contains('▸'),
        "the fill is the cursor, not a glyph: {:?}",
        lines[1]
    );
    let empty = NavView {
        query: "zzzz".into(),
        state_filter: None,
        cursor: 0,
    };
    let rows = v.nav_filtered(&empty);
    let lines = nav_overlay_lines(&rows, &empty);
    assert!(lines.iter().any(|l| l.contains("no matches")));
}
/// Open the navigator filtered to `query`, the way prefix+f plus typing does.
fn nav_on(v: &mut View, query: &str) {
    v.nav = Some(NavView {
        query: query.into(),
        state_filter: None,
        cursor: 0,
    });
}

#[tokio::test]
async fn ctrl_o_then_x_on_a_stopped_navigator_row_removes_it_like_the_sideline() {
    // x on a stopped navigator row sends the same RemoveAgent as the sideline.
    let mut v = view_with_agents(vec![lifecycle_row("dead-nav", true, false)]);
    v.set_squad_view(1, SectionView::Expanded);
    nav_on(&mut v, "dead-nav");
    let mut buf: Vec<u8> = Vec::new();
    nav_keys(&mut v, b"\x0f", &mut buf).await.unwrap();
    assert!(v.row_menu.is_some(), "ctrl-o opens the row menu");
    row_menu_keys(&mut v, b"x", &mut buf).await.unwrap();
    confirm_keys(&mut v, b"\r", &mut buf).await.unwrap();
    let from_nav = decode_cmds(buf);
    assert!(v.nav.is_none());

    let mut s = view_with_agents(vec![lifecycle_row("dead-nav", true, false)]);
    s.set_squad_view(1, SectionView::Expanded);
    let idx = agent_row_at(&s, |a| a.name == "dead-nav");
    assert!(s.open_row_menu(idx, Anchor::Center));
    let mut buf: Vec<u8> = Vec::new();
    row_menu_keys(&mut s, b"x", &mut buf).await.unwrap();
    confirm_keys(&mut s, b"\r", &mut buf).await.unwrap();
    assert_eq!(from_nav, decode_cmds(buf));
    assert_eq!(from_nav.len(), 1, "one RemoveAgent, nothing else");
}
#[test]
fn find_with_the_toggle_off_lists_the_seat_pane_as_it_is() {
    // AC6-EDGE: with the toggle off there is no fleet read and no remote
    // rows, so find lists the seat pane as the local pane it is, as before.
    let mut twin = crate::client::tests::sideline_table_tests::sideline_card_tests::seat_twin();
    twin.portal = Some(0);
    let v = wide_view(vec![twin]);
    let nav = NavView {
        query: "mercedes".into(),
        state_filter: None,
        cursor: 0,
    };
    let rows = v.nav_filtered(&nav);
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .label
            .contains(&format!("{} mercedes@iMac2D2", crate::proto::REMOTE_GLYPH)),
        "the seat twin stays findable as the local pane row it is"
    );
    assert!(
        matches!(rows[0].hit, ChromeHit::Cmds(ref cs) if cs.iter().any(|c| matches!(c, Command::FocusPane(7)))),
        "the seat pane focuses as a local pane"
    );
}

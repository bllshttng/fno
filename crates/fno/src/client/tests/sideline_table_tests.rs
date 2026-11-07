//! The acceptance family for the sideline-as-a-Table rewrite: status words,
//! ellipsized names, the markup-stripped message column, and the
//! TableState-driven selection scroll.

use super::*;

#[path = "sideline_name_fit_tests.rs"]
mod sideline_name_fit_tests;

#[path = "sideline_card_tests.rs"]
pub(super) mod sideline_card_tests;

#[test]
fn list_layout_paints_the_same_cells_as_an_untouched_view() {
    // AC4/AC10: list mode is byte-identical to a view that never set the
    // field (both read the same startup default).
    let agents = vec![
        {
            let mut lead = agent_row("lead-a", 4, Some(AgentBadge::Working), false);
            lead.harness_session_id = Some("sess-lead".into());
            lead
        },
        {
            let mut w1 = agent_row("w1", 5, Some(AgentBadge::Working), false);
            w1.lineage_kind = Some("child".into());
            w1.spawned_by_session = Some("sess-lead".into());
            w1.harness_session_id = Some("sess-w1".into());
            w1
        },
    ];
    let mut agents = agents;
    agents[1].context_used_pct = Some(26);
    agents[1].started_at = Some(crate::digest_overlay::now_secs() - 10800);
    let mut a = wide_view(agents.clone());
    set_density(&mut a, Density::Extended);
    let mut b = wide_view(agents);
    set_density(&mut b, Density::Extended);
    b.sideline_layout = sideline_color::SidelineLayout::List;
    let fa = a.compose();
    let fb = b.compose();
    assert_eq!(fa.cells, fb.cells, "list mode is byte-identical");
    let text = frame_text(&fa);
    assert!(text.contains("26%"), "{text:?}");
    assert!(text.contains("3h"), "{text:?}");
    assert!(text.contains("ctx"), "{text:?}");
    assert!(text.contains("up"), "{text:?}");
}

#[test]
fn team_and_worker_rows_rely_on_their_registry_labels_without_bracket_tags() {
    let mut view = wide_view(vec![
        {
            let mut row = agent_row("folio", 4, Some(AgentBadge::Working), false);
            row.role_level = Some(1);
            row.role_scope = Some("fno".into());
            row.role_title = Some("Folio".into());
            row
        },
        {
            let mut row = agent_row("worker-a", 5, Some(AgentBadge::Working), false);
            row.role_title = Some("Folio".into());
            row
        },
    ]);
    set_density(&mut view, Density::Extended);

    let frame = view.compose();
    let rendered: String = frame.cells.iter().map(|cell| cell.c).collect();
    crate::frame_html::write_shot(
        &frame,
        "team-worker-labels",
        "Team and worker rows use their registry labels",
    );

    assert!(rendered.contains("folio"), "{rendered:?}");
    assert!(rendered.contains("worker-a"), "{rendered:?}");
    assert!(!rendered.contains("Folio"), "{rendered:?}");
    assert!(!rendered.contains("[L1 fno]"), "{rendered:?}");
}

// ---------------------------------------------------------------------------
// the table rewrite: the sideline is a Table (status word, name, message, PR, age)
// ---------------------------------------------------------------------------

#[test]
fn sideline_name_clips_markerless_and_keeps_the_distinguishing_suffix() {
    // acceptance: a long worker name at a 60-column panel cuts with no
    // marker (the ROW rule) and keeps its distinguishing hex suffix, the
    // identity two same-node workers share nothing else of.
    let mut view = two_pane_view();
    view.sideline_width = 60;
    let mut a = tab_agent(None, None, false);
    a.name = "territories-fno-8bef7b".into();
    a.tail = Some("**PR 2113 merged as `84fa`.**".into());
    view.layout.agents = vec![a];
    let frame = view.compose();
    let cols = frame.cols as usize;
    let text_w = (view.panel_w() - 1) as usize;
    let rects = view.worker_column_rects(text_w as u16);
    let row = 2; // row 0 is the strip, row 1 the squad header
    let name = &frame.cells
        [row * cols + rects[1].x as usize..row * cols + (rects[1].x + rects[1].width) as usize];
    assert!(
        !name.iter().any(|c| c.c == '\u{2026}'),
        "the name cell cuts with no marker"
    );
    let rendered_name: String = name.iter().map(|c| c.c).collect();
    assert!(
        rendered_name.contains("8bef7b"),
        "the name cell keeps the distinguishing suffix: {rendered_name:?}"
    );
}

#[test]
fn sideline_message_reads_the_sentence_not_the_markup() {
    // acceptance: `**PR 2113 merged as `84fa`.**` paints as
    // `· PR 2113 merged as 84fa.` - bold markers and backticks stripped, the
    // separator leading the message column.
    let mut view = two_pane_view();
    view.term = (30, 140);
    view.sideline_width = 80;
    let mut a = tab_agent(None, None, false);
    a.tail = Some("**PR 2113 merged as `84fa`.**".into());
    view.layout.agents = vec![a];
    let frame = view.compose();
    let cols = frame.cols as usize;
    let text_w = (view.panel_w() - 1) as usize;
    let rects = view.worker_column_rects(text_w as u16);
    let row = 2; // under the strip row and the squad header
    let msg: String = frame.cells
        [row * cols + rects[2].x as usize..row * cols + (rects[2].x + rects[2].width) as usize]
        .iter()
        .map(|c| c.c)
        .collect();
    assert_eq!(msg.trim_end(), "\u{b7} PR 2113 merged as 84fa.", "{msg:?}");
}

#[test]
fn sideline_status_cell_reads_the_state_word_in_the_lane_color() {
    // acceptance: a working agent row's status cell reads `Working`
    // in the lane color.
    let mut view = two_pane_view();
    view.sideline_width = 60;
    let mut a = tab_agent(None, None, false);
    a.badge = Some(AgentBadge::Working);
    a.pane_activity = None;
    a.harness = Some("codex".into());
    view.layout.agents = vec![a];
    let frame = view.compose();
    let cols = frame.cols as usize;
    let text_w = (view.panel_w() - 1) as usize;
    let rects = view.worker_column_rects(text_w as u16);
    let row = 2; // under the strip row and the squad header
    let status: String = frame.cells
        [row * cols + rects[0].x as usize..row * cols + (rects[0].x + rects[0].width) as usize]
        .iter()
        .map(|c| c.c)
        .collect();
    assert!(status.trim_start().starts_with("●Work"), "{status:?}");
    let want = sideline_color::resolve_lane_color(Some("codex"), None, None, None)
        .unwrap_or(Color::Default);
    let fg = frame.cells[row * cols + rects[0].x as usize].fg;
    assert_eq!(fg, want, "the status word wears the lane color");
}

#[test]
fn sideline_selection_scrolls_into_view_and_paints_the_band() {
    // acceptance: 80 rows on a short panel with the selection past
    // the bottom -> the Table's offset scrolls the selected row into view
    // and that row paints the explicit hover band.
    let mut view = two_pane_view();
    let agents = (0..80)
        .map(|i| {
            let mut a = tab_agent(None, Some(AgentBadge::Working), false);
            a.name = format!("agent-{i}");
            a.pane_activity = None;
            a.pane_id = Some(2 + i as u64);
            a
        })
        .collect::<Vec<_>>();
    view.layout.agents = agents;
    view.selector = Some(60); // display row 60, after the squad header at 0
    let frame = view.compose();
    let cols = frame.cols as usize;
    let visible = view.sideline_visible_rows();
    let sel_row = visible; // rows paint from 1 (strip at 0); the last is `visible`
    assert_eq!(
        frame.cells[sel_row * cols].flags & cell_flags::INVERSE,
        cell_flags::INVERSE,
        "the selected row scrolls into view and inverts"
    );
    assert_ne!(
        frame.cells[0].c, '\u{25be}',
        "the squad header scrolled off the top"
    );
}

#[test]
fn status_sort_arrow_fits_inside_the_status_header_span() {
    // The TableHead row is the paint now: the status column carries the
    // sort arrow where the column hit-test finds it.
    let mut v = wide_view(vec![agent_row(
        "agent",
        4,
        Some(AgentBadge::Working),
        false,
    )]);
    set_density(&mut v, Density::Extended);
    v.agent_sort = AgentSort::Attention;
    let frame = v.compose();
    let cols = frame.cols as usize;
    let rects = v.worker_column_rects((v.panel_w() - 1) as u16);
    // The TableHead paints at row 1 (the strip row owns row 0).
    let status: String = frame.cells
        [cols + rects[0].x as usize..cols + (rects[0].x + rects[0].width) as usize]
        .iter()
        .map(|c| c.c)
        .collect();
    assert!(
        status.contains('\u{2191}'),
        "status header shows direction: {status:?}"
    );
}

#[test]
fn extended_pr_cell_shows_number_or_neutral_value() {
    // The PR cell is the paint now: `#482` when known, the em-dash when not.
    let mut known = agent_row("known", 4, Some(AgentBadge::Working), false);
    known.pr = Some(482);
    let unknown = agent_row("unknown", 5, Some(AgentBadge::Working), false);
    let mut v = wide_view(vec![known, unknown]);
    set_density(&mut v, Density::Extended);
    let frame = v.compose();
    let cols = frame.cols as usize;
    let rects = v.worker_column_rects((v.panel_w() - 1) as u16);
    let cell_text = |row: usize| {
        frame.cells
            [row * cols + rects[3].x as usize..row * cols + (rects[3].x + rects[3].width) as usize]
            .iter()
            .map(|c| c.c)
            .collect::<String>()
    };
    // Row 0 is the strip, row 1 the TableHead, row 2 the squad band; the
    // agents paint at rows 3 and 4.
    assert!(cell_text(3).contains("#482"), "known PR renders");
    assert!(
        cell_text(4).contains('\u{2014}'),
        "unknown PR renders the neutral dash"
    );
}

#[test]
fn sort_label_survives_every_column_configuration() {
    // The sort toggle must stay VISIBLE at every width the table renders
    // at: the head row's arrows must survive the paint at the widest and
    // narrowest admitted tables.
    // Below ~40 columns the solver crushes the age cell out of the row,
    // so the toggle's honest floor is the table's own; at and above it the
    // arrow must always survive.
    for cols in [EXTENDED_PANEL_W, 44] {
        let mut v = wide_view(vec![agent_row("a", 4, Some(AgentBadge::Working), false)]);
        set_density(&mut v, Density::Extended);
        v.term = (24, cols + MIN_CONTENT_COLS + 4);
        v.agent_sort = AgentSort {
            column: AgentSortColumn::Age,
            direction: SortDirection::Descending,
        };
        let first_line = frame_text(&v.compose()).lines().nth(1).unwrap().to_string();
        assert!(
            first_line.contains("age\u{2193}"),
            "age header visible at width {cols}: {first_line:?}"
        );
        if cols == EXTENDED_PANEL_W {
            assert!(
                first_line.contains("ctx"),
                "wide meter header: {first_line:?}"
            );
            assert!(
                first_line.contains("up"),
                "wide runtime header: {first_line:?}"
            );
        } else {
            assert!(
                !first_line.contains("ctx"),
                "narrow identity budget: {first_line:?}"
            );
            assert!(
                !first_line.contains("up"),
                "narrow sort budget: {first_line:?}"
            );
        }
    }
}

#[test]
fn status_word_sits_one_column_from_the_name_cell_parent_and_child() {
    // Acceptance: the gap is the test, the widths are not. A short status
    // word right-aligns inside its fixed cell, so its last glyph sits exactly
    // one spacing column from the name cell - for a parent row and for a
    // spawned child (depth 1), whose indent must not widen the gap.
    let parent = {
        let mut a = agent_row(
            "architect-with-a-very-long-name",
            4,
            Some(AgentBadge::Working),
            false,
        );
        a.pane_activity = None;
        a.harness_session_id = Some("sess-arch".into());
        a
    };
    let child = {
        let mut a = agent_row(
            "child-with-a-very-long-name-too",
            5,
            Some(AgentBadge::Working),
            false,
        );
        a.pane_activity = None;
        a.lineage_kind = Some("child".into());
        a.spawned_by_session = Some("sess-arch".into());
        a
    };
    let mut v = wide_view(vec![parent, child]);
    set_density(&mut v, Density::Extended);
    let frame = v.compose();
    let cols = frame.cols as usize;
    let rects = v.worker_column_rects((v.panel_w() - 1) as u16);
    // Rows: 0 strip, 1 TableHead, 2 squad band, 3 parent, 4 child.
    for (row, label) in [(3usize, "parent"), (4, "child")] {
        let status: String = frame.cells
            [row * cols + rects[0].x as usize..row * cols + (rects[0].x + rects[0].width) as usize]
            .iter()
            .map(|c| c.c)
            .collect();
        // The still spin glyph fills the word's blank lead column.
        assert_eq!(
            status.trim(),
            "●Work",
            "{label} status word reads the state: {status:?}"
        );
        // The acceptance IS the gap: from the word's last glyph to the name
        // cell, exactly the one inter-column space - the widths are not the
        // test. Left-aligned paint fails this (5 columns for a 7-wide word).
        assert_eq!(
            rects[1].x - rects[0].x - status.trim_end().chars().count() as u16,
            1,
            "{label}: one column between the status word and the name cell"
        );
    }
}

#[test]
fn list_hover_band_is_one_color_across_every_column_gap() {
    // The list hover band paints ONE explicit bg across the whole row, the
    // column gaps included. The striped band - lane-colored cells against
    // lighter gaps - was per-cell inversion: a lane fg inverted into a
    // colored background, a Default fg into the terminal's own, so the row
    // read two colors.
    let mut view = two_pane_view();
    let mut worker = tab_agent(None, Some(AgentBadge::Working), false);
    worker.harness = Some("codex".into()); // a lane-colored row: the stripe source
    worker.name = "worker".into();
    view.layout.agents = vec![worker];
    let agent_i = view
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(_)))
        .expect("an agent row renders");
    view.hover_row = Some(agent_i);
    let frame = view.compose();
    let cols = frame.cols as usize;
    let text_w = view.sideline_paint_w().saturating_sub(1);
    let row = agent_i - view.sideline_offset() + 1; // the strip row owns row 0
    let cells = &frame.cells[row * cols..row * cols + text_w];
    assert!(cells.iter().any(|c| c.c != ' '), "the row has text");
    for (j, cell) in cells.iter().enumerate() {
        assert_eq!(cell.bg, Color::Indexed(0), "one band bg, gaps included");
        // The status column keeps the row's lane accent on the band (the
        // operator's color ruling); every other cell carries the band text.
        if j >= 5 {
            assert_eq!(cell.fg, Color::Indexed(3), "one band accent text");
        }
        assert_eq!(cell.flags, 0, "no INVERSE inside the band");
    }
}

#[test]
fn composed_list_bands_hold_contrast_on_dark_and_light_frames() {
    // The list layout's hover band, judged by the same lens as the card
    // bands: the REAL painter's cells clear the 3:1 hover floor on a dark
    // and a light terminal.
    let mut view = two_pane_view();
    let mut worker = tab_agent(None, Some(AgentBadge::Working), false);
    worker.harness = Some("codex".into());
    worker.name = "worker".into();
    view.layout.agents = vec![worker];
    view.term = (30, 140);
    view.sideline_width = 80;
    let agent_i = view
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(_)))
        .expect("an agent row renders");
    view.hover_row = Some(agent_i);
    let frame = view.compose();
    let cols = frame.cols as usize;
    let text_w = view.sideline_paint_w().saturating_sub(1);
    let row = agent_i - view.sideline_offset() + 1; // the strip row owns row 0
    let cells = &frame.cells[row * cols..row * cols + text_w];
    for lens in crate::frame_html::THEMES {
        for cell in cells.iter().filter(|c| c.c != ' ') {
            let ratio = crate::frame_html::contrast_ratio(cell, lens);
            assert!(
                ratio >= 3.0,
                "list hover band on {}: {ratio:.2}:1",
                lens.name
            );
        }
    }
}

#[test]
fn sticky_footer_and_the_in_list_new_squad_row_never_paint_twice() {
    // x-b5b8: once the list overflows, the pinned footer AND the in-list
    // NewSquad row both painted `+ new workspace` in one frame. Exactly one
    // instance may show at any scroll offset.
    let mut view = two_pane_view();
    view.term = (10, 72);
    let panes = view.layout.panes.clone();
    let agents: Vec<AgentRow> = (0..12)
        .map(|i| AgentRow {
            harness: None,
            model: None,
            route: None,
            name: format!("a{i}"),
            pane_id: Some(100 + i),
            portal: None,
            ..focus_agent(0)
        })
        .collect();
    view.set_layout(LayoutView {
        squads: vec![meta(1, "footnote", 1, 0)],
        active_squad: 1,
        panes,
        focus: 100,
        area: (10, 72),
        agents,
        focus_node: None,
    });
    let label = view.footer_buttons(28).text;
    let count = |view: &View| {
        let (rows, cols, panel_w) = (10usize, 72usize, 28usize);
        let mut cells = vec![Cell::default(); rows * cols];
        view.draw_sideline(&mut cells, rows, cols, panel_w);
        cells
            .iter()
            .map(|c| c.c)
            .collect::<String>()
            .matches(label.as_str())
            .count()
    };
    // Park the selector on the NewSquad row and clamp: the window scrolls so
    // the in-list row is visible - the state where the pinned footer painted
    // beside it and the label showed twice.
    let ns = view
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::NewSquad))
        .expect("the row list carries a new-workspace row");
    view.selector = Some(ns);
    view.clamp_sideline_scroll();
    assert_eq!(count(&view), 1, "scrolled so the in-list row is visible");
    // Scrolled back to the top: the in-list row is off-screen, so the pinned
    // footer is the one instance.
    view.selector = Some(0);
    view.clamp_sideline_scroll();
    assert_eq!(count(&view), 1, "scrolled to the top");
}

#[test]
fn xf331_focus_band_and_selector_are_distinct_treatments() {
    // x-f331, restated: the focus row wears the surface band, never an
    // accent fill (x-b5b8). The selector row inverts apart from it.
    let mut view = two_pane_view();
    view.layout.agents.push(focus_agent(11)); // owns focused pane 11 -> row 1
    view.hover_row = None;
    let frame = view.compose();
    let cols = frame.cols as usize;

    let focus_cell = frame.cells[2 * cols]; // display row 1 paints at outer row 2
    assert_eq!(
        focus_cell.bg,
        Color::Indexed(0),
        "the focus row wears the surface band, never an accent fill"
    );
}

// The identity-suffix family: lane color, deviation tokens, and the hold
// badges ([DND] for a hold you set, [HELD] for one the machine armed).

#[test]
fn sideline_lane_color_and_deviation_token_render_on_the_row() {
    // x-1b35 AC3: the lane renders as zero-width color on the agent row
    // (here through the built-in codex table entry), the model-deviation
    // token appends ` glm` on a claude row OFF its default lane, and the
    // retired `@<account>` text prefix is gone from the composition.
    let mut view = two_pane_view();
    let mut codex_row = focus_agent(21);
    codex_row.harness = Some("codex".into());
    let mut glm_row = focus_agent(22);
    glm_row.harness = Some("claude".into());
    glm_row.model = Some("glm-5.3-flash[1m]".into());
    glm_row.account = Some("makers".into());
    view.layout.agents.push(codex_row);
    view.layout.agents.push(glm_row);
    // The deviation token rides the name cell; give the columns room for it.
    view.sideline_width = 100;
    let frame = view.compose();
    let cols = frame.cols as usize;
    let panel_w = view.panel_w() as usize;
    let name_x = view.worker_column_rects((panel_w - 1) as u16)[1].x as usize;
    let line = |row: usize| -> (String, Color) {
        let start = row * cols;
        let end = start + panel_w.min(cols);
        let text: String = frame.cells[start..end].iter().map(|c| c.c).collect();
        // A name cell, not the row lead: the focused row's band and the
        // selector own the first columns.
        let fg = frame.cells[start + name_x].fg;
        (text, fg)
    };
    // Row 2 (1 + the strip row): the codex row - the lane color now rides
    // the status cell only (the operator's color ruling), so the NAME cell
    // reads default. No account prefix.
    let (text, fg) = line(2);
    assert_eq!(
        fg,
        Color::Default,
        "the name cell carries no lane color anymore"
    );
    assert!(
        !text.contains('@'),
        "the @account prefix is retired: `{text}`"
    );
    // Row 3 (2 + the strip row): the claude/glm row - the deviation token
    // is the textual channel; claude itself carries no builtin color, so fg
    // stays default and the token does the naming.
    let (text, fg) = line(3);
    assert!(
        text.contains(" glm"),
        "the deviation token renders: `{text}`"
    );
    assert!(!text.contains("@makers"), "no @account prefix: `{text}`");
    assert_eq!(fg, Color::Default, "claude carries no builtin lane color");
}

#[test]
fn sideline_marks_active_squad_and_focused_agent_row() {
    // x-4374 / AC2-HP: the active squad header accents its caret, and the
    // agent row whose pane holds focus wears the full-width INVERSE band
    // (replacing the near-invisible one-cell gutter x-5a52 painted). Both
    // stand regardless of the selector (parked elsewhere) or hover.
    let mut view = two_pane_view(); // active_squad = 1, focus = pane 11
    view.layout.agents.push(focus_agent(11));
    view.selector = Some(3); // squad 2's header, not row 0 or 1
    view.hover_row = None;
    let frame = view.compose();
    let cols = frame.cols as usize;
    let panel_w = view.panel_w() as usize;

    // Display row 0 -> outer row 1 (the strip owns outer row 0): the active
    // squad header caret is amber.
    let caret = frame.cells[cols];
    assert_eq!(caret.c, '▾', "active expanded squad shows the caret");
    assert_eq!(caret.fg, LATTICE_ACCENT, "active squad caret is accented");

    // Display row 1 -> outer row 2: the focused agent row is a full-width
    // surface band (accent text on the deep index, never a full accent
    // fill), and the `▎` gutter glyph is gone.
    let lead = frame.cells[2 * cols]; // outer row 2, col 0
    assert_ne!(
        lead.c, '▎',
        "the list layout keeps no ▎ gutter; the card layout owns the bar"
    );
    assert_eq!(
        lead.bg,
        Color::Indexed(0),
        "the focused row carries the standing surface band"
    );
    // The band fills the panel width (a right-edge text cell is still banded).
    assert_eq!(
        frame.cells[2 * cols + panel_w - 2].bg,
        Color::Indexed(0),
        "the focus band fills the panel width"
    );
}

#[test]
fn client_agent_row_renders_dnd_as_presence_not_liveness() {
    // The hold-badge vertical (sidecar -> overlay -> wire -> render): a
    // conversation-sourced sidecar marks the row, and the render reads
    // [HELD] for the machine-armed hold while a hold you set keeps [DND].
    let now = chrono::Utc::now().timestamp() as u64;
    let stamp = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let dir = std::env::temp_dir().join(format!("hold-badge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("aaaa1111-2222-3333-4444-555566667777.json"),
        format!(
            "{{\"until\": \"{}\", \"window_s\": 3600, \"clock_kind\": \"wall\", \"ceiling\": null, \"source\": \"conversation\"}}\n",
            stamp(chrono::Utc::now() + chrono::Duration::seconds(600))
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("bbbb2222-2222-3333-4444-555566667777.json"),
        format!(
            "{{\"until\": \"{}\", \"window_s\": 3600, \"clock_kind\": \"wall\", \"ceiling\": null}}\n",
            stamp(chrono::Utc::now() + chrono::Duration::seconds(600))
        ),
    )
    .unwrap();
    let mut rows = vec![
        crate::agents_view::RegistryAgent {
            name: "auto".into(),
            dnd: true,
            harness_session_id: Some("AAAA1111-2222-3333-4444-555566667777".into()),
            ..Default::default()
        },
        crate::agents_view::RegistryAgent {
            name: "manual".into(),
            dnd: true,
            harness_session_id: Some("bbbb2222-2222-3333-4444-555566667777".into()),
            ..Default::default()
        },
    ];
    crate::agents_view::overlay_hold_marks_at(&mut rows, &dir, now);
    assert!(
        rows[0].held_conversation,
        "conversation sidecar marks the row"
    );
    assert!(
        !rows[1].held_conversation,
        "a sourceless sidecar stays manual"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let held: AgentRow = serde_json::from_str(
        r#"{"squad":1,"name":"held","pane_id":10,
                "badge":"working","reason":null,"exited":false,
                "dnd":true,"held_conversation":true}"#,
    )
    .unwrap();
    let manual: AgentRow = serde_json::from_str(
        r#"{"squad":1,"name":"dnd","pane_id":10,
                "badge":"working","reason":null,"exited":false,"dnd":true}"#,
    )
    .unwrap();
    let mut view = two_pane_view();
    view.sideline_width = 60;
    view.layout.agents = vec![held, manual];
    let text = frame_text(&view.compose());
    let held_row = text.lines().find(|line| line.contains("held")).unwrap();
    assert!(
        held_row.contains("[HELD]") && !held_row.contains("[DND]"),
        "machine-armed hold reads HELD: {held_row:?}"
    );
    let manual_row = text.lines().find(|line| line.contains("dnd")).unwrap();
    assert!(
        manual_row.contains("[DND]") && !manual_row.contains("[HELD]"),
        "a manual hold keeps DND, riding identity without liveness: {manual_row:?}"
    );
}

#[test]
fn a_marked_row_paints_the_build_glyph_in_place_of_the_working_one() {
    // The mark's paint contract (user ruling 2026-10-09): compiling swaps
    // the working glyph for the BRAILLE_SIX rest frame, waiting swaps the
    // glyph for the hourglass and its queue position; a row the mark does
    // not apply to keeps the plain working cell. Deterministic: the test
    // env has no spin epoch, so compiling paints the set's rest glyph.
    let mut builder = agent_row("builder", 4, Some(AgentBadge::Working), false);
    builder.build = Some(crate::proto::BuildMark::compiling());
    let mut waiter = agent_row("waiter", 5, Some(AgentBadge::Working), false);
    waiter.build = Some(crate::proto::BuildMark::waiting(Some(2)));
    let plain = agent_row("plain", 6, Some(AgentBadge::Working), false);
    let mut v = wide_view(vec![builder, waiter, plain]);
    // The list cell is the contract under test: pin the layout so the test
    // reads the code, never the ambient config's own choice.
    v.sideline_layout = sideline_color::SidelineLayout::List;
    let text = frame_text(&v.compose());
    let builder_row = text.lines().find(|line| line.contains("builder")).unwrap();
    assert!(
        builder_row.contains('\u{283f}') && !builder_row.contains('\u{25cf}'),
        "compiling paints the braille frame in place of the working cell: {builder_row:?}"
    );
    assert!(
        !builder_row.contains("Work"),
        "the status word drops so the cell stays within its 5 columns: {builder_row:?}"
    );
    assert!(
        !builder_row.contains('\u{25cf}'),
        "never both glyphs on one row: {builder_row:?}"
    );
    let waiter_row = text.lines().find(|line| line.contains("waiter")).unwrap();
    assert!(
        waiter_row.contains("\u{29d7}2"),
        "waiting paints the hourglass and queue position: {waiter_row:?}"
    );
    let plain_row = text.lines().find(|line| line.contains("plain")).unwrap();
    assert!(
        plain_row.contains('\u{25cf}'),
        "an unmarked working row keeps the plain glyph: {plain_row:?}"
    );
}

#[test]
fn a_marked_card_renders_the_waiting_mark_in_its_one_live_glyph_cell() {
    // The card's status column is two cells wide and right-aligned, and the
    // resting edge bar paints cell 0 after the blit - so the card's mark
    // gets ONE live cell and the waiting mark drops the position digit
    // there (the list cell keeps it). Without the budget the bar ate the
    // hourglass and the row read a bare digit (the 2026-11-06 shot finding).
    let mut waiter = agent_row("waiter", 5, Some(AgentBadge::Working), false);
    waiter.build = Some(crate::proto::BuildMark::waiting(Some(2)));
    let mut v = wide_view(vec![waiter]);
    set_density(&mut v, Density::Extended);
    v.sideline_layout = sideline_color::SidelineLayout::Card;
    let text = frame_text(&v.compose());
    let waiter_line = text.lines().find(|line| line.contains("waiter")).unwrap();
    assert!(
        waiter_line.contains('\u{29d7}'),
        "the card paints the hourglass in its live glyph cell: {waiter_line:?}"
    );
    assert!(
        !waiter_line.contains("\u{29d7}2"),
        "the one-cell slot drops the position digit, never a bar-eaten glyph: {waiter_line:?}"
    );
}

#[test]
fn overlay_build_marks_stamps_live_sidecars_and_ages_out_stale_ones() {
    // The mark's push leg: a fresh db/build-marks sidecar stamps its row,
    // a stale one (dead daemon) does not, and an unrelated row carries no
    // mark - the same fail-quiet contract the hold mark keeps.
    let now = chrono::Utc::now().timestamp() as u64;
    let stamp = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let dir = std::env::temp_dir().join(format!("build-marks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let fresh = stamp(chrono::Utc::now());
    let stale = stamp(chrono::Utc::now() - chrono::Duration::seconds(120));
    std::fs::write(
        dir.join("cccc3333-3333-3333-4444-555566667777.json"),
        format!("{{\"state\": \"compiling\", \"at\": \"{fresh}\", \"ttl_ms\": 30000}}\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("dddd4444-4444-4444-4444-555566667777.json"),
        format!(
            "{{\"state\": \"waiting\", \"position\": 3, \"at\": \"{stale}\", \"ttl_ms\": 30000}}\n"
        ),
    )
    .unwrap();
    let mut rows = vec![
        crate::agents_view::RegistryAgent {
            name: "builder".into(),
            harness_session_id: Some("CCCC3333-3333-3333-4444-555566667777".into()),
            ..Default::default()
        },
        crate::agents_view::RegistryAgent {
            name: "waiter".into(),
            harness_session_id: Some("dddd4444-4444-4444-4444-555566667777".into()),
            ..Default::default()
        },
        crate::agents_view::RegistryAgent {
            name: "idle".into(),
            harness_session_id: Some("eeee5555-5555-5555-5555-555566667777".into()),
            ..Default::default()
        },
    ];
    crate::agents_view::overlay_build_marks_at(&mut rows, &dir, now);
    assert_eq!(
        rows[0].build.map(|b| b.stage),
        Some(crate::proto::BuildStage::Compiling),
        "a fresh sidecar stamps the row"
    );
    assert_eq!(
        rows[1].build, None,
        "a stale sidecar ages out: no mark, never a frozen hourglass"
    );
    assert_eq!(
        rows[2].build, None,
        "a session with no sidecar carries no mark"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn group_by_axes_build_labelled_bands_with_rollups() {
    // the non-workspace group axes emit labelled bands with rollup
    // counts, members in layout order (the run sort orders inside each band),
    // and blank separators between bands.
    let mut lead = agent_row("lead", 4, Some(AgentBadge::Working), false);
    lead.role_level = Some(2);
    lead.harness_session_id = Some("sess-lead".into());
    let mut w1 = agent_row("w1", 5, Some(AgentBadge::Working), false);
    w1.lineage_kind = Some("child".into());
    w1.spawned_by_session = Some("sess-lead".into());
    w1.cwd_base = Some("footnote".into());
    let mut w2 = agent_row("w2", 6, Some(AgentBadge::Blocked), false);
    w2.cwd_base = Some("other".into());
    let mut v = wide_view(vec![lead, w1, w2]);

    v.agent_group = AgentGroup::Team;
    let rows = v.display_rows();
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Header { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec!["lead", "~ unteamed"],
        "team bands in paint order"
    );
    let agents: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Agent(a) => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, vec!["lead", "w1", "w2"], "members under their lead");

    v.agent_group = AgentGroup::Status;
    let rows = v.display_rows();
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Header { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec!["Needs input", "Working"],
        "status bands, empty Completed omitted"
    );
    let agents: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Agent(a) => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        agents,
        vec!["w2", "lead", "w1"],
        "blocked first, working after"
    );

    v.agent_group = AgentGroup::Cwd;
    let rows = v.display_rows();
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Header { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec!["(no cwd)", "footnote", "other"],
        "cwd bands, unspecified cwd last-shown by layout order"
    );

    // group_by_harness: one band per harness, a bare pane in its own band,
    // and the band's summed spend is the header hint.
    v.layout.agents[0].harness = Some("claude".into());
    v.layout.agents[0].session_cost_cents = Some(150);
    v.layout.agents[0].session_tokens = Some(2_000_000);
    v.layout.agents[2].harness = Some("claude".into());
    v.layout.agents[2].session_cost_cents = Some(100);
    v.layout.agents[2].session_tokens = Some(1_000_000);
    v.agent_group = AgentGroup::Harness;
    let rows = v.display_rows();
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Header { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["claude", "(no harness)"], "harness bands");
    assert_eq!(
        crate::client::agent_group::band_spend(&v, "claude").as_deref(),
        Some("~$2.50 · 3M tok"),
        "the band sums its rows' spend"
    );
    assert_eq!(
        crate::client::agent_group::band_spend(&v, "(no harness)"),
        None
    );
    assert_eq!(AgentGroup::Harness.next(), AgentGroup::Flat);
    assert_eq!(AgentGroup::Flat.next(), AgentGroup::Workspace);
    // Flat: one headerless run of every live row.
    v.agent_group = AgentGroup::Flat;
    let rows = v.display_rows();
    assert!(!rows.iter().any(|r| matches!(r, DisplayRow::Header { .. })));
    let live = rows.iter().filter(|r| matches!(r, DisplayRow::Agent(_)));
    assert_eq!(live.count(), 3, "flat keeps every live row");

    // The machine layer wraps the active axis only while its toggle is on.
    let fleet = serde_json::json!({"machines": [{"host": "imac2d2", "fresh": true, "age_s": 9,
        "sessions": [{"name": "boss", "harness": "claude", "role": "L1", "session_id": "aaaa1111-x"},
                     {"name": "w", "harness": "claude", "parent": "aaaa1111", "status": "writing"}]}]});
    let headers = |v: &View| -> Vec<String> {
        let rows = v.display_rows();
        let labels = rows.iter().filter_map(|r| match r {
            DisplayRow::Header { label, .. } => Some(label.clone()),
            _ => None,
        });
        labels.collect()
    };
    v.agent_group = AgentGroup::Status;
    crate::client::machine_tree::apply(&mut v, Ok(fleet.clone()));
    assert_eq!(
        headers(&v),
        ["Needs input", "Working"],
        "toggle off drops the read"
    );
    v.machine_tree.on = true;
    crate::client::machine_tree::apply(&mut v, Ok(fleet));
    let here = format!("{} · offline · 0s", crate::agents_view::hostname());
    let remote_rows = ["imac2d2 · ready · 9s", "  Working", "  Completed"];
    assert_eq!(
        headers(&v),
        [here.as_str(), "  Needs input", "  Working"]
            .into_iter()
            .chain(remote_rows)
            .collect::<Vec<_>>()
    );
    v.agent_group = AgentGroup::Team;
    assert_eq!(
        headers(&v)[3..],
        ["imac2d2 · ready · 9s", "  boss"],
        "a remote worker joins its lead"
    );
    // Under workspace a remote worker nests under its parent; flat keeps only
    // the machine bands.
    v.agent_group = AgentGroup::Workspace;
    let (rows, depths) = v.tree_rows_with_depths();
    let remote_rows: Vec<(&str, usize)> = (rows.iter().zip(&depths))
        .filter_map(|(r, d)| match r {
            DisplayRow::Agent(a) if a.remote.is_some() => Some((a.name.as_str(), *d)),
            _ => None,
        })
        .collect();
    assert_eq!(remote_rows, [("boss@imac2d2", 0), ("w@imac2d2", 1)]);
    v.agent_group = AgentGroup::Flat;
    assert_eq!(headers(&v), [here.as_str(), "imac2d2 · ready · 9s"]);
}

#[test]
fn manual_sort_and_pins_reorder_the_run() {
    // the Manual column reads the persisted reorder sequence, and a
    // pinned row leads every column.
    let mk = |name: &str, pane: u64| agent_row(name, pane, Some(AgentBadge::Working), false);
    let mut v = wide_view(vec![mk("a", 4), mk("b", 5), mk("c", 6)]);
    v.manual_order = vec!["c".into(), "a".into(), "b".into()];
    v.agent_sort = AgentSort {
        column: AgentSortColumn::Manual,
        direction: SortDirection::Ascending,
    };
    let agents: Vec<&str> = v
        .display_rows()
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Agent(a) => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, vec!["c", "a", "b"], "manual order holds");

    // A pin outranks the column: c stays first even on the status sort.
    let mut pinned = mk("c", 6);
    pinned.pinned = true;
    let mut v = wide_view(vec![mk("a", 4), mk("b", 5), pinned]);
    v.agent_sort = AgentSort::Attention;
    let agents: Vec<&str> = v
        .display_rows()
        .iter()
        .filter_map(|r| match r {
            DisplayRow::Agent(a) => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        agents.first(),
        Some(&"c"),
        "the pinned row leads the status sort: {agents:?}"
    );
}

#[test]
fn header_hint_takes_only_the_gap_the_fit_left_over() {
    let rollup = [(LatticeState::Working, 3)];
    // Room to spare: the hint follows the label, the counts keep the edge.
    let roomy = header_band::header_band_hinted("sq", "(no folder)", &rollup, 40);
    assert!(roomy.starts_with("sq (no folder)") && roomy.ends_with("\u{25cf}3"));
    assert_eq!(roomy.chars().count(), 40);
    // No room: the hint drops whole and never clips the label or a count.
    let tight = header_band::header_band_hinted("sq", "(no folder)", &rollup, 12);
    assert_eq!(tight, header_band_text("sq", &rollup, 12));
    assert_eq!(header_band::folder_hint(""), "(no folder)");
}

fn top_row(v: &View) -> String {
    let f = v.compose();
    f.cells[..v.panel_w() as usize]
        .iter()
        .map(|c| c.c)
        .collect()
}

#[test]
fn agents_tab_strip_counts_rows_that_need_input() {
    let mk = |name: &str, pane: u64, b: AgentBadge| agent_row(name, pane, Some(b), false);
    let now = crate::agents_view::rfc3339_like_to_secs("2027-01-15T08:00:00Z").unwrap();
    for harness in ["claude", "opencode", "codex", "agy", "pi"] {
        let raw = serde_json::json!({"schema_version": 6, "agents": [{
            "name": "parked", "cwd": "/tmp", "status": "live", "harness": harness,
            "inside_leg": {"state": "working", "seq": 1,
                           "received_at": "2027-01-15T07:59:30Z", "ttl_ms": 120000},
            "screen_state": {"state": "blocked", "rule": "permission_prompt", "seq": 1,
                             "at": "2027-01-15T07:59:30Z", "ttl_ms": 120000}
        }]})
        .to_string();
        let rows = crate::agents_view::derive_rows(&raw, now).unwrap();
        let mut far = mk("b2", 6, AgentBadge::Blocked);
        far.cwd_base = Some("/tmp/other".into());
        let mut v = wide_view(vec![
            agent_row("parked", 4, rows[0].badge, false),
            far,
            mk("d1", 5, AgentBadge::Done),
        ]);
        let row = top_row(&v);
        // The mark turns on the spin clock, so only the count and words are pinned.
        assert!(
            row[row.chars().next().map_or(0, char::len_utf8)..].starts_with("2 need input"),
            "{harness}: {row:?}"
        );
        let rollup = section_rollup(v.layout.agents.iter().map(agent_lattice_state));
        assert_eq!(rollup.first(), Some(&(LatticeState::Blocked, 2)));
        v.needs_open = true;
        assert_eq!(
            v.display_rows()
                .iter()
                .filter(|r| matches!(r, DisplayRow::Agent(a) if a.name == "parked"))
                .count(),
            2,
            "{harness}: parked row stays in its group and Needs input"
        );
        if harness == "claude" {
            crate::frame_html::write_shot(
                &v.compose(),
                "parked-permission-blocked",
                "Parked permission prompt appears in Needs input and its group",
            );
        }
    }

    let calm = wide_view(vec![mk("d1", 5, AgentBadge::Done)]);
    let row = top_row(&calm);
    let start = calm.top_row_spans()[0].0;
    assert!(row.chars().take(start).all(|c| c == ' '), "{row:?}");
}

#[test]
fn needs_input_section_holds_agent_rows_only() {
    // An open question answers from the notifications panel, so it joins
    // neither the strip count nor the pinned section (operator, 2026-11-04):
    // a question line here was never tappable, and the same question is
    // already listed, tappable, on the Questions tab.
    let mut v = wide_view(vec![
        agent_row("b1", 4, Some(AgentBadge::Blocked), false),
        agent_row("w1", 5, Some(AgentBadge::Working), false),
    ]);
    v.apply_questions_fold(Ok(crate::needs_overlay::QuestionsFold {
        items: vec![crate::needs_overlay::QuestionItem {
            id: "q-demo-a1".into(),
            title: "Hold the demo until the shot lands?".into(),
            state: "open".into(),
            ..Default::default()
        }],
        ..Default::default()
    }));
    assert_eq!(v.need_input_count(), 1, "questions leave the count");
    v.needs_open = true;
    let pinned: Vec<DisplayRow> = v
        .display_rows()
        .into_iter()
        .take_while(|r| !matches!(r, DisplayRow::Blank))
        .collect();
    assert!(
        matches!(&pinned[..], [DisplayRow::Header { label, .. }, DisplayRow::Agent(_)]
            if label.as_str() == "Needs input"),
        "header + the blocked agent, nothing else, got {} rows",
        pinned.len()
    );
    let text = frame_text(&v.compose());
    assert!(
        !text.contains("Hold the demo"),
        "the question never paints in the sideline, got:\n{text}"
    );
}

#[test]
fn pinned_needs_agent_row_taps_like_its_group_row() {
    let mut v = wide_view(vec![
        agent_row("b1", 4, Some(AgentBadge::Blocked), false),
        agent_row("w1", 5, Some(AgentBadge::Working), false),
    ]);
    v.needs_open = true;
    let at: Vec<usize> = v
        .display_rows()
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, DisplayRow::Agent(a) if a.name == "b1").then_some(i))
        .collect();
    assert_eq!(
        at.len(),
        2,
        "the blocked row paints pinned and in its group"
    );
    let top = v.sideline_top() as u16;
    let hit = |i: usize| v.chrome_hit(top + i as u16, 3);
    assert_eq!(
        format!("{:?}", hit(at[0])),
        format!("{:?}", hit(at[1])),
        "a tap on the pinned copy resolves like its group copy"
    );
    assert!(
        matches!(hit(at[0]), Some(ChromeHit::Cmds(c)) if c == vec![Command::FocusPane(4)]),
        "the tap opens the row's pane, got {:?}",
        hit(at[0])
    );
}

#[tokio::test]
async fn agents_tab_ctrl_t_pins_the_session_and_the_row_shows_it() {
    let mk = |name: &str, pane: u64| agent_row(name, pane, Some(AgentBadge::Working), false);
    let mut b = mk("b", 5);
    b.harness_session_id = Some("bbbb2222-1111-2222-3333-444444444444".into());
    // The pin is the reapers' keep mark, flipped by session id.
    assert_eq!(
        agent_sort::pin_argv(&b).unwrap(),
        vec!["reap", "--keep", "bbbb2222-1111-2222-3333-444444444444"]
    );
    let mut pinned_b = b.clone();
    pinned_b.pinned = true;
    assert_eq!(agent_sort::pin_argv(&pinned_b).unwrap()[1], "--unkeep");
    // A remote row's mark lives on its own machine: the handle names the host,
    // and a row with no session id of its own still has a key to send.
    let mut remote_row = mk("muriel@bb16", 6);
    remote_row.remote = Some(crate::proto::RemoteRow {
        host: "bb16".into(),
        key: "133a419b".into(),
        name: remote_row.name.clone(),
    });
    assert_eq!(
        agent_sort::pin_argv(&remote_row).unwrap(),
        vec!["reap", "--keep", "133a419b@bb16"]
    );
    remote_row.pinned = true;
    assert_eq!(
        agent_sort::pin_argv(&remote_row).unwrap(),
        vec!["reap", "--unkeep", "133a419b@bb16"]
    );

    let mut v = wide_view(vec![mk("a", 4), b]);
    let at = v
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(a) if a.name == "b"))
        .unwrap();
    v.selector = Some(at);
    let mut buf: Vec<u8> = Vec::new();
    selector_keys(&mut v, &[0x14], &mut buf).await.unwrap();
    assert!(v.notice.as_ref().is_some_and(|(n, _)| n == "pinning b"));

    // A row with no session yet has nothing to pin, and says so.
    let at = v
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(a) if a.name == "a"))
        .unwrap();
    v.selector = Some(at);
    selector_keys(&mut v, &[0x14], &mut buf).await.unwrap();
    assert!(v
        .notice
        .as_ref()
        .is_some_and(|(n, _)| n == "a has no session id to pin yet"));

    // A pinned row wears one accent star; no row did before the pin. Its
    // menu offers Unpin.
    v.selector = None;
    let stars = |v: &View| {
        let f = v.compose();
        f.cells
            .iter()
            .filter(|c| c.c == sideline::PIN_STAR)
            .map(|c| c.fg)
            .collect::<Vec<_>>()
    };
    assert!(stars(&v).is_empty(), "an unpinned row paints nothing");
    // Hover offers an outline star (the chosen row may show one too), and a
    // click on it hits that row.
    v.hover_row = v
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(a) if a.name == "b"));
    let f = v.compose();
    let hit: Vec<String> = (f.cells.iter().enumerate())
        .filter(|(_, c)| c.c == sideline::PIN_OUTLINE)
        .filter_map(|(o, _)| v.pin_star_at(o as u16 / f.cols, o as u16 % f.cols))
        .map(|a| a.name)
        .collect();
    assert!(
        hit.contains(&"b".to_string()),
        "hovered b offers its star: {hit:?}"
    );
    v.hover_row = None;
    v.layout.agents[1] = pinned_b.clone();
    assert_eq!(
        stars(&v),
        vec![v.theme.brand],
        "the pinned row shows its star"
    );
    let menu = row_menu::build_row_menu(&pinned_b, Anchor::Center);
    assert!(menu.actions.contains(&MenuAction::TogglePin));
}

#[test]
fn a_keep_marker_pins_its_session_and_the_sessions_it_resumed() {
    // The pin reads the lineage the reapers read: the row's own id or a
    // recorded predecessor. An unmarked row stays plain.
    let dir = std::env::temp_dir().join(format!("keep-marks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for sid in ["own-1", "old-2"] {
        std::fs::write(dir.join(format!("{sid}.json")), "{}").unwrap();
    }
    let row = |sid: &str, pred: &[&str]| crate::agents_view::RegistryAgent {
        harness_session_id: Some(sid.into()),
        predecessor_session_ids: pred.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let mut rows = vec![
        row("own-1", &[]),
        row("new-2", &["old-2"]),
        row("plain-3", &[]),
    ];
    crate::agents_view::overlay_keep_marks_at(&mut rows, &dir);
    let pinned: Vec<bool> = rows.iter().map(|r| r.pinned).collect();
    assert_eq!(pinned, vec![true, true, false]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn agents_tab_selector_hint_names_pin_and_move() {
    let hint = crate::keys::selector_hint();
    for word in [
        "space reply",
        ". or ctrl+t pin",
        "shift+up/down move",
        "r rename",
    ] {
        assert!(hint.contains(word), "missing {word}: {hint}");
    }
}

#[test]
fn agents_tab_selector_row_inverts_from_the_theme() {
    // The selector's cursor row takes the theme's text color as its ground
    // and the base as its text, every cell of the row.
    let theme = crate::theme::theme_footnote_superscript();
    assert_eq!(
        crate::theme::cell_style(crate::theme::Role::BodyFocus, &theme),
        crate::cursor::cursor_style(&theme)
    );
    let mk = |name: &str, pane: u64| agent_row(name, pane, Some(AgentBadge::Working), false);
    let mut v = wide_view(vec![mk("a", 4), mk("b", 5)]);
    v.sideline_layout = sideline_color::SidelineLayout::List;
    v.theme = theme.clone();
    let at = v
        .display_rows()
        .iter()
        .position(|r| matches!(r, DisplayRow::Agent(a) if a.name == "b"))
        .unwrap();
    v.selector = Some(at);
    let f = v.compose();
    let cols = f.cols as usize;
    let row = at - v.sideline_offset() + 1;
    let text_w = (v.panel_w() - 1) as usize;
    assert!(
        f.cells[row * cols..row * cols + text_w]
            .iter()
            .all(|c| c.bg == theme.title && c.fg == theme.base),
        "the whole cursor row inverts"
    );
}

#[test]
fn parked_prompt_attention_is_readable_on_both_grounds() {
    let mut failures = Vec::new();
    for (name, theme) in [
        ("dark", crate::theme::theme_footnote_superscript()),
        ("light", crate::theme::theme_footnote_paper()),
    ] {
        for banded in [true, false] {
            let lens = crate::frame_html::theme_by_name(name).unwrap();
            let pane = if banded { 11 } else { 10 };
            let mut v = wide_view(vec![agent_row(
                "parked",
                pane,
                Some(AgentBadge::Blocked),
                false,
            )]);
            set_density(&mut v, Density::Extended);
            v.theme = theme.clone();
            v.needs_open = true;
            let frame = v.compose();
            let count = &frame.cells[..v.need_input_label_cols()];
            assert!(
                count.iter().any(|c| c.c.is_ascii_digit()),
                "count must paint"
            );
            let ratio = count
                .iter()
                .filter(|c| c.c != ' ')
                .map(|c| crate::frame_html::contrast_ratio(c, lens))
                .fold(f64::INFINITY, f64::min);
            if ratio < 4.5 {
                failures.push(format!("{name} count: {ratio:.2}:1"));
            }
            let mut tokens = 0;
            for (i, display) in v.display_rows().iter().enumerate() {
                if !matches!(display, DisplayRow::Agent(a) if a.name == "parked") {
                    continue;
                }
                let y = v.sideline_top() as usize + i - v.sideline_offset();
                let row = &frame.cells
                    [y * frame.cols as usize..y * frame.cols as usize + v.panel_w() as usize - 1];
                for token in row
                    .windows(5)
                    .filter(|t| t.iter().map(|c| c.c).eq("Input".chars()))
                {
                    tokens += 1;
                    let ratio = token
                        .iter()
                        .map(|c| crate::frame_html::contrast_ratio(c, lens))
                        .fold(f64::INFINITY, f64::min);
                    if ratio < 4.5 {
                        failures.push(format!("{name} Input banded={banded}: {ratio:.2}:1"));
                    }
                }
            }
            assert_eq!(tokens, 2, "measure the pinned and normal group tokens");
        }
    }
    assert!(
        failures.is_empty(),
        "attention text must clear 4.5:1: {failures:?}"
    );
}

// A filed user question raises its asking row through the badge path: the
// row stamps Blocked (reason `filed_question`), the strip count and the
// pinned Needs input section read it, and answering the question unstamps.
#[test]
fn a_filed_question_stamps_the_asking_row_blocked() {
    let item =
        |state: &str, sid: Option<&str>, name: Option<&str>| crate::needs_overlay::QuestionItem {
            kind: "question".into(),
            state: state.into(),
            origin: Some(crate::needs_overlay::QuestionOrigin {
                session_id: sid.map(str::to_string),
                name: name.map(str::to_string),
                harness: Some("claude".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
    let fold =
        |items: Vec<crate::needs_overlay::QuestionItem>| crate::needs_overlay::QuestionsFold {
            items,
            ..Default::default()
        };
    let mut v = wide_view(vec![
        agent_row("asker", 4, Some(AgentBadge::Working), false),
        agent_row("bystander", 5, Some(AgentBadge::Working), false),
    ]);
    v.layout.agents[0].harness_session_id = Some("sid-asker".into());

    v.apply_questions_fold(Ok(fold(vec![item(
        "open",
        Some("sid-asker"),
        Some("asker"),
    )])));
    assert_eq!(v.layout.agents[0].badge, Some(AgentBadge::Blocked));
    assert_eq!(v.layout.agents[0].reason.as_deref(), Some("filed_question"));
    assert_eq!(v.layout.agents[1].badge, Some(AgentBadge::Working));
    assert_eq!(v.layout.agents[1].reason, None);
    assert_eq!(v.need_input_count(), 1, "the strip count reads the stamp");
    let row = top_row(&v);
    assert!(
        row[row.chars().next().map_or(0, char::len_utf8)..].starts_with("1 need input"),
        "{row:?}"
    );

    // The stamped row sits in the pinned section and stays in its group.
    v.needs_open = true;
    assert_eq!(
        v.display_rows()
            .iter()
            .filter(|r| matches!(r, DisplayRow::Agent(a) if a.name == "asker"))
            .count(),
        2,
        "the asking row sits pinned and in its group"
    );

    // An answer clears the flag the same way it landed.
    v.apply_questions_fold(Ok(fold(vec![item(
        "answered",
        Some("sid-asker"),
        Some("asker"),
    )])));
    assert_eq!(v.layout.agents[0].badge, Some(AgentBadge::Working));
    assert_eq!(v.layout.agents[0].reason, None);
    assert_eq!(v.need_input_count(), 0);
}

// The stamp's two guards: a question at a lead rung is not the user's to
// answer, so its row keeps working; an exited row keeps its exit fact even
// when its question is still open on the board.
#[test]
fn a_lead_rung_question_and_a_dead_row_never_stamp() {
    let item = |level: Option<&str>, sid: &str| crate::needs_overlay::QuestionItem {
        kind: "question".into(),
        state: "open".into(),
        origin: Some(crate::needs_overlay::QuestionOrigin {
            session_id: Some(sid.into()),
            ..Default::default()
        }),
        rung: level.map(|l| crate::needs_overlay::QuestionRung {
            level: Some(l.into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut v = wide_view(vec![
        agent_row("climbed", 4, Some(AgentBadge::Working), false),
        {
            let mut d = agent_row("dead", 5, Some(AgentBadge::Working), false);
            d.exited = true;
            d
        },
        agent_row("wren", 6, Some(AgentBadge::Working), false),
    ]);
    v.layout.agents[0].harness_session_id = Some("sid-climb".into());
    v.layout.agents[1].harness_session_id = Some("sid-dead".into());
    let mut far = item(None, "sid-far");
    far.origin.as_mut().unwrap().name = Some("wren".into());
    // A cross-machine copy carries a host; its name is another machine's
    // registry name and must never join a local row.
    far.host = Some("imac2d2".into());
    v.apply_questions_fold(Ok(crate::needs_overlay::QuestionsFold {
        items: vec![item(Some("lead"), "sid-climb"), item(None, "sid-dead"), far],
        ..Default::default()
    }));
    assert_eq!(
        v.layout.agents[0].badge,
        Some(AgentBadge::Working),
        "a lead rung is not the user's to answer"
    );
    assert_eq!(
        v.layout.agents[1].badge,
        Some(AgentBadge::Working),
        "an exited row keeps its exit fact"
    );
    assert_eq!(
        v.layout.agents[2].badge,
        Some(AgentBadge::Working),
        "a cross-machine copy's name never joins a local row"
    );
    assert_eq!(v.need_input_count(), 0);
}

// The registry name is the fallback join when the ask carried no session
// id: the row still stamps, or a rename would orphan the flag.
#[test]
fn the_asking_name_matches_when_the_session_id_is_absent() {
    let item = crate::needs_overlay::QuestionItem {
        kind: "question".into(),
        state: "open".into(),
        origin: Some(crate::needs_overlay::QuestionOrigin {
            name: Some("wren".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut v = wide_view(vec![agent_row("wren", 4, Some(AgentBadge::Working), false)]);
    v.apply_questions_fold(Ok(crate::needs_overlay::QuestionsFold {
        items: vec![item],
        ..Default::default()
    }));
    assert_eq!(v.layout.agents[0].badge, Some(AgentBadge::Blocked));
    assert_eq!(v.need_input_count(), 1);
}

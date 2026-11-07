use super::*;

pub(in crate::client) fn check_fixture(view: &mut View) {
    use serde_json::json;
    open(view);
    let gen = view
        .messages_board
        .as_ref()
        .expect("the fixture opens the board")
        .gen;
    apply_gather(view, gen, Ok(std::sync::Arc::new(json!({}))));
    assert!(view.messages_board.is_some(), "the fixture opens the board");
}

#[tokio::test]
async fn messages_reply_board_contracts() {
    use super::super::LayoutView;
    use serde_json::json;
    let mut view = View::new(
        (24, 100),
        "main".into(),
        LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: Vec::new(),
            focus_node: None,
        },
    );
    check_fixture(&mut view);
    let prefs = tempfile::tempdir().unwrap();
    crate::view_store::set_test_path(prefs.path());
    let b = view.messages_board.as_mut().expect("open");
    b.snapshot.apply(json!({
        "participants": [
            {"key":"s1","name":"first","system":false,"live":true,"last_ts":"2026-10-01T09:00:00Z"},
            {"key":"s-q","name":"quill","archive_scope":"team","system":false,"live":false,"last_ts":"2026-10-01T09:20:00Z"},
            {"key":"s-c","name":"candor","system":false,"live":true,"last_ts":"2026-10-01T09:10:00Z"},
            {"key":"s-ivy","name":"ivy","system":false,"live":true},
            {"key":"s-maple","name":"maple","system":false,"live":true},
            {"key":"s-oak","name":"oak","system":false,"live":true},
            {"key":"unknown","name":"unknown","system":false,"live":false},
            {"key":"user","name":"user","system":false,"live":false},
            {"key":"01a18820-1111-4111-8111-111111111111","name":"01a18820-1111-4111-8111-111111111111","harness":"claude","system":false,"live":true},
            {"key":"fno/pr-nudge","name":"fno/pr-nudge","system":true,"live":false},
        ],
        "threads": [
            {"chat_id":"chat-a1","participants":["s1","s-c"],
             "rows":[
               {"id":"m1","ts":"2026-10-01T09:00:00Z","from":"candor","from_key":"s-c","to_key":"s1","summary":"Ship it.","body":"Ship it.","system":false},
               {"id":"m2","ts":"2026-10-01T09:01:00Z","from":"first","from_key":"s1","to_key":"s-c","summary":"On it.","body":"On it.","system":false},
               {"id":"m3","ts":"2026-10-01T09:02:00Z","from":"candor","from_key":"s-c","to_key":"s1","summary":"Merged.","body":"Merged.","system":false},
               {"id":"m4","ts":"2026-10-01T09:20:00Z","from":"candor","from_key":"s-c","to_key":"s1","summary":"Landed.","body":"Landed.\nSee the log.","system":false,"state":"parked"}],
             "last_ts":"2026-10-01T09:20:00Z"},
            {"chat_id":"chat-a2","participants":["s-c","s-q"],
             "rows":[
               {"id":"m5","ts":"2026-10-01T08:55:00Z","from":"quill","from_key":"s-q","to_key":"s-c","summary":"one","body":"one","system":false},
               {"id":"m6","ts":"2026-10-01T08:56:00Z","from":"quill","from_key":"s-q","to_key":"s-c","summary":"two","body":"two","system":false},
               {"id":"m7","ts":"2026-10-01T08:57:00Z","from":"candor","from_key":"s-c","to_key":"s-q","to":"quill","summary":"three","body":"three","system":false,"state":"failed","state_reason":"waited-3s;no-pane-ref;not-confirmed","state_failures":2},
               {"id":"m8","ts":"2026-10-01T08:58:00Z","from":"quill","from_key":"s-q","to_key":"s-c","summary":"four","body":"four","system":false}],
             "last_ts":"2026-10-01T08:58:00Z"}
        ],
        "system": {"s-c": [
            {"id":"m9","ts":"2026-10-01T09:10:00Z","from":"fno/pr-nudge","from_key":"fno/pr-nudge","to_key":"s-c","summary":"Nudge.","body":"Nudge.","system":true}]},
        "channels": [
            {"scope":"fno","rows":[{"id":"m10","ts":"2026-10-01T09:00:00Z","from":"first","from_key":"s1","to":"fleet:fno","summary":"Standup.","body":"Standup.","system":false}]},
            {"scope":"leads","rows":[]},
        ],
        "announcements": [], "unreadable": 0,
    }));
    // Column 1 (AC1-HP, AC2-HP, AC14-HP, AC15-HP): the channels, then
    // every non-system participant - the Agents tab lists everyone with
    // reaped last and the sort modes ordering the live ones.
    let agent_names = |rows: &[TreeRow]| -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                TreeRow::Agent { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    // A raw session id shows as its harness and handle; the `unknown`
    // placeholder and the operator's `user` voice never list.
    let names = agent_names(&b.tree_rows());
    assert_eq!(names.len(), 7, "{names:?}");
    assert!(names.contains(&"claude 01a18820".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n == "unknown" || n == "user"),
        "{names:?}"
    );
    b.sort_mode = SortMode::Last;
    let last_order = agent_names(&b.tree_rows());
    assert_eq!(&last_order[..2], ["candor", "first"], "{last_order:?}");
    assert_eq!(last_order.last().map(String::as_str), Some("quill"));
    b.sort_mode = SortMode::Alpha;
    assert_eq!(
        agent_names(&b.tree_rows()),
        [
            "claude 01a18820",
            "candor",
            "first",
            "ivy",
            "maple",
            "oak",
            "quill"
        ]
    );
    b.sort_mode = SortMode::Last;
    // The Archive tab lists the reaped agent only (AC15-HP).
    b.list_tab = ListTab::Archive;
    assert_eq!(agent_names(&b.tree_rows()), ["quill"]);
    b.list_tab = ListTab::Agents;
    let lines1 = b.tree_column(100);
    let text1: String = lines1
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    // The filter row leads (item 5); the All filter lists agents only -
    // no broadcast row among them, no `#`.
    assert!(text1.contains("All"), "{text1}");
    assert!(text1.contains("Broadcasts"), "{text1}");
    assert!(!text1.contains('#'), "{text1}");
    assert!(!text1.contains('▸') && !text1.contains('▾'), "{text1}");
    // The Broadcasts filter lists the groups alone, with the retired
    // `leads` scope reading as `leads` (item 5).
    b.filter = ListFilter::Broadcasts;
    let text_b: String = b
        .tree_column(100)
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text_b.contains("fleet:fno"), "{text_b}");
    assert!(text_b.contains("fleet:leads"), "{text_b}");
    assert!(!text_b.contains("candor"), "{text_b}");
    b.filter = ListFilter::All;

    // Column 2 (AC5-HP, AC13-HP): the strip leads with Chats; the Chats
    // tab lists the live thread only, System one entry per arm, Archive
    // the thread whose other party is reaped.
    b.sel_agent = Some("s-c".into());
    let chats = b.chat_rows("s-c");
    assert_eq!(chats.len(), 1, "{chats:?}");
    let ChatRow::Thread {
        unread, partner, ..
    } = &chats[0]
    else {
        panic!("thread row: {chats:?}")
    };
    assert!(unread, "no mark reads unread");
    assert_eq!(partner, "first");
    let chats_lines = b.chats_column(100);
    assert!(
        chats_lines[0].text.starts_with(" Chats "),
        "{:?}",
        chats_lines[0].text
    );
    // The active tab is one full inverted cell, pad included; the
    // inactive cells never invert.
    // A parked send of candor's: `!` shows beside the unread `*`, never in
    // its place.
    assert!(
        chats_lines.iter().any(|l| l.text.starts_with("!*first")),
        "{:?}",
        chats_lines.iter().map(|l| &l.text).collect::<Vec<_>>()
    );
    let strip = &chats_lines[0];
    assert!(strip.roles[..7].iter().all(|r| *r == TextStyle::Tab));
    assert!(!strip.roles[7..].contains(&TextStyle::Tab));
    b.col2 = ChatTab::System;
    let sys = b.chat_rows("s-c");
    assert!(
        matches!(&sys[0], ChatRow::SystemEntry { arm } if arm == "fno/pr-nudge"),
        "{sys:?}"
    );
    b.col2 = ChatTab::Archive;
    let archived = b.chat_rows("s-c");
    assert_eq!(archived.len(), 1, "{archived:?}");
    assert!(
        matches!(&archived[0], ChatRow::Thread { partner, .. } if partner == "quill"),
        "{archived:?}"
    );
    b.col2 = ChatTab::Chats;
    let all_text: String = b
        .columns(100, split(100, &Default::default()))
        .0
        .into_iter()
        .chain(b.columns(100, split(100, &Default::default())).1)
        .map(|l| l.text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!all_text.contains("Partners"), "{all_text}");
    // Column 3: the channel's rows resolve; the System exchange shows
    // the fno/<arm> sender (AC14-HP).
    b.sel_thread = Some("channel:fno".into());
    assert_eq!(b.conversation_rows().len(), 1);
    b.sel_thread = Some("system:fno/pr-nudge".into());
    let rows = b.conversation_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("from").and_then(Value::as_str),
        Some("fno/pr-nudge")
    );
    // Item 6: a second arm in the same agent's system listing filters to
    // its own rows, never the aggregate. col2 returns to Chats, the tab
    // the audit block below Enters on.
    b.col2 = ChatTab::System;
    let sys_arms = b.chat_rows("s-c");
    assert!(sys_arms.len() >= 1, "{sys_arms:?}");
    b.col2 = ChatTab::Chats;
    // The bubble thread (AC6-HP, AC16-HP, AC17-HP, AC19-HP): the title
    // names the other party with the info affordances, runs share one
    // label, a time line closes each five-minute burst, blanks sit
    // between runs and never inside one, and no delivered-header or
    // envelope text shows.
    b.sel_thread = Some("chat-a1".into());
    let (lines3, owners3) = b.thread_lines(50);
    let texts: Vec<String> = lines3.iter().map(|l| l.text.clone()).collect();
    assert!(texts[0].starts_with("first"), "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t.ends_with("parked \u{b7} enter submit")),
        "{texts:?}"
    );
    assert!(
        texts[0].contains("[i]") && texts[0].contains("..."),
        "{texts:?}"
    );
    let time_of = |ts: &str| {
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string()
    };
    // Each burst closes with its last message's time on that sender's
    // side: both bursts end on candor, the selected agent, so both sit
    // flush right.
    let t3 = time_of("2026-10-01T09:02:00Z");
    let t4 = time_of("2026-10-01T09:20:00Z");
    let times: Vec<&String> = texts
        .iter()
        .filter(|t| t.trim() == t3 || t.trim() == t4)
        .collect();
    assert_eq!(times.len(), 2, "{texts:?}");
    assert!(times.iter().all(|t| t.len() == 48), "{texts:?}");
    assert_eq!(texts.last().map(|t| t.trim()), Some(t4.as_str()));
    // Four labels here: the 09:20 time line starts a new run (item 9).
    let label_count = texts
        .iter()
        .filter(|t| t.trim() == "candor" || t.trim() == "first")
        .count();
    assert_eq!(label_count, 4, "{texts:?}");
    // The pure run shape (AC16-HP): A, A, B, A within five minutes paints
    // exactly three labels and the second A bubble carries none.
    b.sel_thread = Some("chat-a2".into());
    let (lines4, _) = b.thread_lines(50);
    let texts4: Vec<String> = lines4.iter().map(|l| l.text.clone()).collect();
    let labels4 = texts4
        .iter()
        .filter(|t| t.trim() == "quill" || t.trim() == "candor")
        .count();
    assert_eq!(labels4, 3, "{texts4:?}");
    // A retry that failed again names its cause in words, then the retry
    // key, then the copy rung last, in the warning color.
    let warn: Vec<&str> = lines4
        .iter()
        .filter(|l| l.roles.contains(&TextStyle::ERROR))
        .map(|l| l.text.trim())
        .collect();
    assert_eq!(
        warn.join(" ").replace('\u{a0}', " "),
        "retry failed: quill has no pane to type into \u{b7} r retry \u{b7} y copy",
        "{texts4:?}"
    );
    let one_i = texts4.iter().position(|t| t.ends_with("one")).unwrap();
    let two_i = texts4.iter().position(|t| t.ends_with("two")).unwrap();
    assert_eq!(two_i, one_i + 1, "one run, no blank between: {texts4:?}");
    b.sel_thread = Some("chat-a1".into());
    // The selected agent's mail sits flush right, the other party's on the
    // left under its label; a stored newline starts a new line and never
    // paints as a cell.
    let ship_i = texts
        .iter()
        .position(|t| t.trim() == "Ship it." && t.len() == 48)
        .expect("my bubble flush right");
    assert!(texts.iter().any(|t| t.as_str() == "  On it."), "{texts:?}");
    assert!(texts.iter().all(|t| !t.contains('\n')), "{texts:?}");
    let see_i = texts.iter().position(|t| t.trim() == "See the log.");
    assert_eq!(
        see_i,
        texts
            .iter()
            .position(|t| t.trim() == "Landed.")
            .map(|i| i + 1)
    );
    assert!(!texts
        .iter()
        .any(|t| t.contains("fmail-") || t.contains("<fno_mail")));
    let merged_i = texts
        .iter()
        .position(|t| t.ends_with("Merged."))
        .expect("merged bubble");
    let landed_i = texts
        .iter()
        .position(|t| t.ends_with("Landed."))
        .expect("landed bubble");
    // Each run's first bubble sits directly under its label; a blank sits
    // between runs (AC19-HP); the within-run adjacency is asserted on
    // chat-a2 above.
    assert_eq!(texts[merged_i - 1].trim(), "candor", "{texts:?}");
    assert_eq!(texts[landed_i - 1].trim(), "candor", "{texts:?}");
    assert!(texts[ship_i + 1].trim().is_empty(), "{texts:?}");
    assert_eq!(owners3[ship_i], Some(0), "{owners3:?}");
    let onit_i = texts.iter().position(|t| t.as_str() == "  On it.").unwrap();
    assert_eq!(owners3[onit_i], Some(1), "{owners3:?}");
    assert_eq!(owners3[landed_i], Some(3), "{owners3:?}");
    // The shared row builder: a field no source holds prints nothing.
    let mut rows2 = Vec::new();
    super::super::feed_detail::info_row("x", None, &mut rows2);
    assert!(rows2.is_empty());

    // Audit gate: protects row navigation, receiver-first recipient choice,
    // exact reply prefill, and the tap hit map plus no-pane refusal. A cursor,
    // default, quote or wrapped-row regression can misroute a human reply;
    // existing fixture coverage owns projection only, so these assertions extend
    // that owner. It uses the real View/wire and needs no production seam.
    b.col = Col::Chats;
    b.cursors[1] = 0;
    super::keys(&mut view, b"\r", &mut tokio::io::sink())
        .await
        .unwrap();
    assert_eq!(
        view.messages_board.as_ref().unwrap().cursors[2],
        3,
        "opening a long thread shows the newest message"
    );
    super::keys(&mut view, b"k", &mut tokio::io::sink())
        .await
        .unwrap();
    super::keys(&mut view, b"\r", &mut tokio::io::sink())
        .await
        .unwrap();
    let choices = super::super::messages_reply::popup(&view).unwrap();
    assert_eq!(choices.selected(), Some((2, 0)));
    let labels = choices
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::popup::PopupRow::Entry { label, .. } => Some(label.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(labels, ["first", "candor"]);
    super::super::messages_reply::keys(&mut view, b"\r", &mut tokio::io::sink())
        .await
        .unwrap();
    let seed = super::super::messages_reply::popup(&view)
        .unwrap()
        .rows
        .iter()
        .find_map(|row| match row {
            crate::popup::PopupRow::Input { text, .. } => Some(text.as_str()),
            _ => None,
        });
    assert_eq!(seed, Some("re candor/m3: \"Merged.\" "));
    super::super::messages_reply::keys(&mut view, b"ok", &mut tokio::io::sink())
        .await
        .unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(256);
    super::super::messages_reply::keys(&mut view, b"\r", &mut writer)
        .await
        .unwrap();
    let mut bytes = [0; 256];
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            tokio::io::AsyncReadExt::read(&mut reader, &mut bytes),
        )
        .await
        .is_err(),
        "no wire bytes are sent without a live pane"
    );
    assert_eq!(
        view.notice.as_ref().map(|(text, _)| text.as_str()),
        Some("reply: first has no pane on screen; open a portal first")
    );
    super::super::messages_reply::keys(&mut view, b"\x1b", &mut writer)
        .await
        .unwrap();
    view.notice = None;
    view.layout.agents = vec![crate::proto::AgentRow {
        name: "first".into(),
        harness_session_id: Some("session-uuid".into()),
        pane_id: Some(7),
        ..Default::default()
    }];
    view.layout.panes = vec![(
        7,
        crate::tree::Rect {
            x: 0,
            y: 0,
            rows: 1,
            cols: 1,
        },
    )];
    assert_eq!(
        super::super::messages_reply::endpoint_pane(&view, "first", "s1"),
        Some((7, "session-uuid".into())),
        "a registry fno_id resolves through its unique participant name"
    );
    view.layout.agents.push(crate::proto::AgentRow {
        name: "first".into(),
        harness_session_id: Some("another-session".into()),
        pane_id: Some(8),
        ..Default::default()
    });
    view.layout.panes.push((
        8,
        crate::tree::Rect {
            x: 1,
            y: 0,
            rows: 1,
            cols: 1,
        },
    ));
    assert_eq!(
        super::super::messages_reply::endpoint_pane(&view, "first", "s1"),
        None,
        "ambiguous aliases must not resolve to an arbitrary session"
    );
    view.layout.agents.truncate(1);
    view.layout.panes.truncate(1);
    super::super::messages_reply::open(
        &mut view,
        json!({"id":"m1","from_key":"s-c","to_key":"s1","summary":"Ship it."}),
    );
    super::super::messages_reply::keys(&mut view, b"\r", &mut tokio::io::sink())
        .await
        .unwrap();
    super::super::messages_reply::keys(&mut view, b"ok", &mut tokio::io::sink())
        .await
        .unwrap();
    let (mut reply_writer, mut reply_reader) = tokio::io::duplex(512);
    super::super::messages_reply::keys(&mut view, b"\r", &mut reply_writer)
        .await
        .unwrap();
    let request = crate::proto::read_msg::<_, crate::proto::ClientMsg>(&mut reply_reader)
        .await
        .unwrap();
    let crate::proto::ClientMsg::PaneInput(request) = request else {
        panic!("reply must address one pane and wait for its receipt")
    };
    assert_eq!(request.pane, 7);
    assert_eq!(request.expected_identity, "session-uuid");
    assert!(request.bytes.ends_with(b"ok\r"));
    assert_eq!(view.pending_reply_journals.len(), 1);
    super::super::messages_reply::input_result(
        &mut view,
        request.request_id,
        request.pane,
        Err("pane exited before delivery".into()),
    );
    assert!(view.pending_reply_journals.is_empty());
    assert_eq!(
        view.notice.as_ref().map(|(text, _)| text.as_str()),
        Some("reply not delivered: pane exited before delivery")
    );
    // A bubble tap selects and copies its fmail id (AC18-HP); the reply
    // composer opens from Enter, never a tap.
    let tap_row = (ship_i + 1) as u16;
    super::mouse(
        &mut view,
        crate::mouse::MouseReport {
            kind: crate::proto::MouseKind::Press(crate::proto::MouseButton::Left),
            row: tap_row,
            col: 60,
            shift: false,
            alt: false,
        },
        &mut writer,
    )
    .await
    .unwrap();
    assert!(super::super::messages_reply::popup(&view).is_none());
    assert!(view.notice.is_some(), "the tap reported a copy");
    // A title [i] click opens the detail modal; the ... click opens the
    // chat-id popup (AC17-HP). The thread column starts at split(100).0 +
    // split(100).1; the title line paints at screen row 1.
    let (col1_w, col2_w) = split(100, &Default::default());
    let tap = |col: usize| crate::mouse::MouseReport {
        kind: crate::proto::MouseKind::Press(crate::proto::MouseButton::Left),
        row: 1,
        col: col as u16,
        shift: false,
        alt: false,
    };
    let mut sink = tokio::io::sink();
    let x0 = col1_w + col2_w;
    super::mouse(&mut view, tap(x0 + 7), &mut sink)
        .await
        .unwrap();
    assert!(
        view.messages_board.as_ref().unwrap().detail.is_some(),
        "the [i] click opens the detail modal"
    );
    view.messages_board.as_mut().unwrap().detail = None;
    super::mouse(&mut view, tap(x0 + 12), &mut sink)
        .await
        .unwrap();
    assert!(
        view.messages_board.as_ref().unwrap().detail.is_some(),
        "the ... click opens the chat popup"
    );
    view.messages_board.as_mut().unwrap().detail = None;
    // The band follows focus: the focused column's cursor row is the
    // terminal pair swapped, every other column's selection a dim swap.
    {
        let b = view.messages_board.as_mut().unwrap();
        b.sel_agent = Some("s-c".into());
        b.col2 = ChatTab::Chats;
        b.cursors = [0, 0, 0];
    }
    let band_fg = |view: &View, x: usize| {
        let mut cells = vec![Cell::default(); 24 * 100];
        super::paint(view, &mut cells, 24, 100, 100, 24);
        (1..23)
            .map(|r| cells[r * 100 + x])
            .find(|c| c.flags & crate::proto::cell_flags::INVERSE != 0 && c.c == ' ')
            .map(|c| c.fg)
    };
    let dim = crate::theme::dim_fg(&view.theme);
    // Each column's last cell: past the row text, so only the band paints it.
    let ends = [(Col::Tree, col1_w - 1), (Col::Chats, col1_w + col2_w - 1)];
    for (col, _) in ends {
        view.messages_board.as_mut().unwrap().col = col;
        for (owner, x) in ends {
            let want = if owner == col {
                crate::proto::Color::Default
            } else {
                dim
            };
            assert_eq!(band_fg(&view, x), Some(want), "{col:?} x={x}");
        }
    }
    crate::view_store::clear_test_path();
}

/// The Me thread: picking an agent lists the user's own thread with it
/// first (unread off the chat-id read marks), and the detail paints the
/// user's turn flush right and the reply left-aligned, per item.
#[tokio::test]
async fn messages_you_tab_contract() {
    use super::super::LayoutView;
    use serde_json::json;
    let mut view = View::new(
        (24, 100),
        "main".into(),
        LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: Vec::new(),
            focus_node: None,
        },
    );
    check_fixture(&mut view);
    let prefs = tempfile::tempdir().unwrap();
    crate::view_store::set_test_path(prefs.path());
    let b = view.messages_board.as_mut().expect("open");
    b.snapshot.apply(json!({
        "participants": [],
        "threads": [],
        "system": {},
        "channels": [],
        "announcements": [],
        "unreadable": 0,
        "you": [
            {"session_id":"older","name":"older-worker","last_ts":"2026-10-01T09:00:00Z","items":[{"ts":"2026-10-01T09:00:00Z","typed_text":"earlier question","reply_text":""}]},
            {"session_id":"fresh","name":"gavin","last_ts":"2026-10-01T10:00:00Z","items":[{"ts":"2026-10-01T09:30:00Z","typed_text":"<pasted_content id=\"7\">\nmid-turn follow-up\n\n</pasted_content id=\"7\">","reply_text":"Working on it."},{"ts":"2026-10-01T10:00:00Z","typed_text":"why did the gate refuse?","reply_text":"Because the base was stale."}]},
        ],
    }));
    // The drill is Agent, then Me, then the chat: column 1 lists agents
    // only, so a session no mail named still gets its own row.
    let rows = b.tree_rows();
    let gavin = rows
        .iter()
        .position(
            |r| matches!(r, TreeRow::Agent { key, name, .. } if key == "fresh" && name == "gavin"),
        )
        .expect("gavin row");
    b.cursors[0] = gavin;
    b.col = Col::Tree;
    super::keys(&mut view, b"\r", &mut tokio::io::sink())
        .await
        .unwrap();
    let b = view.messages_board.as_mut().unwrap();
    assert_eq!(b.sel_agent.as_deref(), Some("fresh"));
    assert_eq!(b.col2, ChatTab::Chats);
    // Me heads the agent's chats: the user's own thread with gavin only.
    let rows = b.active_chat_rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        matches!(&rows[0], ChatRow::YouThread { name, unread, session_id, .. }
            if name == "Me" && *unread && session_id == "fresh"),
        "{rows:?}"
    );
    // The Me row keeps a thread row's two marker slots, so its name lines
    // up with the agent names under it.
    assert!(
        b.chats_column(40)
            .iter()
            .any(|l| l.text.starts_with(" *Me")),
        "Me row aligned"
    );
    b.cursors[1] = 0;
    b.col = Col::Chats;
    // Enter opens the thread: sel_thread you:<sid>, the read mark saves.
    super::keys(&mut view, b"\r", &mut tokio::io::sink())
        .await
        .unwrap();
    let b = view.messages_board.as_ref().unwrap();
    assert_eq!(b.sel_thread.as_deref(), Some("you:fresh"));
    let marks = crate::view_store::load_messages_read_marks();
    assert_eq!(
        marks.get("you:fresh"),
        Some(&"2026-10-01T10:00:00Z".to_string())
    );
    // The detail: the user's turn flush right, the reply left-aligned.
    let (lines3, _) = b.thread_lines(50);
    let texts: Vec<String> = lines3.iter().map(|l| l.text.clone()).collect();
    assert!(texts[0].starts_with("gavin"), "{texts:?}");
    let typed_i = texts
        .iter()
        .position(|t| t.trim() == "why did the gate refuse?")
        .expect("typed bubble");
    assert_eq!(texts[typed_i].len(), 48, "flush right: {texts:?}");
    assert_eq!(texts[typed_i - 1].trim(), "Me", "{texts:?}");
    assert_eq!(texts[typed_i - 1].len(), 48, "Me flush right: {texts:?}");
    // The harness's paste wrapper lines never paint; the pasted words do.
    assert!(
        texts
            .iter()
            .any(|t| t.trim() == "mid-turn follow-up" && t.len() == 48),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("pasted_content")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.as_str() == "  Because the base was stale."),
        "{texts:?}"
    );
    // The detail's j/k walks the item bubbles, not conversation rows.
    let b = view.messages_board.as_mut().unwrap();
    b.col = Col::Thread;
    b.cursors[2] = 1;
    super::keys(&mut view, b"k", &mut tokio::io::sink())
        .await
        .unwrap();
    assert_eq!(
        view.messages_board.as_ref().unwrap().cursors[2],
        0,
        "k walks the You thread's items"
    );
}

/// A live fleet's volume: thousands of participants and threads. The paint
/// reads rows built once per landed projection, never per frame, and a
/// frame stays cheap at this size.
#[test]
fn messages_paint_reads_rows_built_once_per_projection() {
    use serde_json::json;
    let prefs = tempfile::tempdir().unwrap();
    crate::view_store::set_test_path(prefs.path());
    let participants: Vec<_> = (0..3000)
        .map(|i| {
            json!({"key":format!("s{i}"),"name":format!("a{i}"),"live":true,
            "last_ts":format!("2026-10-01T09:{:02}:00Z", i % 60)})
        })
        .collect();
    let threads: Vec<_> = (0..4000)
        .map(|i| {
            json!({"chat_id":format!("c{i}"),
            "participants":[format!("s{}", i % 3000), format!("s{}", (i + 1) % 3000)],
            "rows":[{"id":format!("m{i}"),"ts":"2026-10-01T09:00:00Z","summary":"hi",
                "from_key":format!("s{}", i % 3000)}]})
        })
        .collect();
    let you: Vec<_> = (0..12)
        .map(|i| {
            json!({"session_id":format!("y{i}"),"name":format!("you{i}"),
            "last_ts":"2026-10-01T09:00:00Z"})
        })
        .collect();
    let mut b = MessagesBoard::new(0);
    b.snapshot
        .apply(json!({"participants":participants,"threads":threads,"you":you}));
    b.sel_agent = Some("s1".into());
    let cold = Instant::now();
    let _ = b.columns(200, (50, 50));
    let cold = cold.elapsed();
    let first = b.tree_rows();
    assert_eq!(
        first.len(),
        3013,
        "the pinned asks row, every agent, each unheld You thread"
    );
    let started = Instant::now();
    for _ in 0..30 {
        let _ = b.columns(200, (50, 50));
    }
    eprintln!(
        "messages paint: first frame {cold:?}, then {:?} a frame",
        started.elapsed() / 30
    );
    assert!(
        Rc::ptr_eq(&first, &b.tree_rows()),
        "frames reuse the built rows"
    );
    assert_eq!(
        b.active_chat_rows().len(),
        4,
        "s1 sits in threads 0, 1, 3000, 3001"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "30 frames took {:?}",
        started.elapsed()
    );
    b.snapshot
        .apply(json!({"participants":[{"key":"s1","name":"a1","live":true}]}));
    assert_eq!(
        b.tree_rows().len(),
        2,
        "the asks row plus the one agent; a landed projection rebuilds the rows"
    );
}

/// AC7+AC8: the asks row flattens two sessions' `you` rows, pending first
/// even when the answered ask is newer, column 3 paints the state mark, the
/// session name and both texts, and an empty projection paints one
/// empty-state line and no error.
#[tokio::test]
async fn ac7_ac8_asks_paint_pending_first_and_the_empty_state() {
    use serde_json::json;
    let mut view = View::new(
        (24, 100),
        "main".into(),
        super::super::LayoutView {
            squads: Vec::new(),
            active_squad: 0,
            panes: Vec::new(),
            focus: 0,
            area: (0, 0),
            agents: Vec::new(),
            focus_node: None,
        },
    );
    check_fixture(&mut view);
    let prefs = tempfile::tempdir().unwrap();
    crate::view_store::set_test_path(prefs.path());
    let b = view.messages_board.as_mut().expect("open");
    b.snapshot.apply(json!({
        "participants": [],
        "threads": [],
        "you": [
            {"session_id": "sess-a", "name": "atlas", "last_ts": "2026-11-06T10:00:00Z",
             "items": [
                {"ts": "2026-11-06T10:00:00Z", "typed_text": "ship the asks view",
                 "reply_text": "Shipped; the PR is open.", "turn_id": "t-a1",
                 "end_reply": "", "state": "answered"}]},
            {"session_id": "sess-b", "name": "brisk", "last_ts": "2026-11-06T11:00:00Z",
             "items": [
                {"ts": "2026-11-06T11:00:00Z", "typed_text": "status on the gate?",
                 "reply_text": "", "turn_id": "t-b1", "end_reply": "", "state": "pending"}]}
        ],
    }));
    b.sel_thread = Some("asks".into());
    b.cursors[2] = 0;
    // The pinned column-1 row exists with the pending count.
    let tree = b.tree_rows();
    match tree.first() {
        Some(TreeRow::Asks { pending }) => assert_eq!(*pending, 1, "one pending ask"),
        other => panic!("the asks row pins first, got {other:?}"),
    }
    let (lines, _) = b.thread_lines(80);
    let text = lines
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let brisk_at = text.find("brisk").expect("pending session painted");
    let atlas_at = text.find("atlas").expect("answered session painted");
    assert!(
        brisk_at < atlas_at,
        "pending session paints before the answered one"
    );
    assert!(text.contains("status on the gate?"), "typed text paints");
    assert!(
        text.contains("Shipped; the PR is open."),
        "reply paints under its ask"
    );
    // AC8: an empty projection paints the title and one empty-state line,
    // no error.
    b.snapshot.apply(json!({
        "participants": [],
        "threads": [],
        "you": [],
    }));
    b.sel_thread = Some("asks".into());
    let (lines, _) = b.thread_lines(80);
    let body: Vec<String> = lines
        .iter()
        .map(|l| l.text.clone())
        .filter(|t| !t.trim().is_empty())
        .collect();
    assert_eq!(body.len(), 2, "title plus one empty-state line: {body:?}");
    assert!(
        body[0].starts_with("Your asks"),
        "the title names the view: {body:?}"
    );
    assert_eq!(body[1], "(no asks yet)", "one empty-state line");
}

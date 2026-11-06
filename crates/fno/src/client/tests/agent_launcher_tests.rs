//! The new-agent composer's unit suite : editor semantics, focus
//! order, submit refusals, update correlation, and the render table. The
//! subprocess-boundary journeys live in the integration suite
//! (`tests/agent_launcher_journey.rs`).

use super::agent_launcher::{
    apply_launch_update, close, open, CatalogOutcome, Focus, HarnessChoice, LauncherEsc, Phase,
    ProjectFacts,
};
use super::*;
use crate::model_catalog::ModelState;
use crate::proto::agent_launch::{AgentLaunchUpdate, LaunchState};
use ratatui_core::buffer::Buffer as RtBuffer;
use ratatui_core::layout::Rect as RtRect;

struct WireVersionFixture(std::path::PathBuf);

impl Drop for WireVersionFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn wire_fixture_at(version: u32) -> (String, WireVersionFixture) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let session = format!("x50ed-wire-fixture-{}-{nonce}", std::process::id());
    let socket = crate::proto::socket_path(&session).unwrap();
    let sidecar = crate::proto::version_sidecar_path(&socket);
    std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    std::fs::write(&sidecar, format!("{version}\n")).unwrap();
    (session, WireVersionFixture(sidecar))
}

fn plain_view() -> View {
    let mut v = two_pane_view();
    v.term = (24, 80);
    v
}

fn view_with_launcher() -> View {
    let mut v = plain_view();
    open(&mut v);
    v
}

fn catalog(names: &[(&str, bool, bool)]) -> Option<CatalogOutcome> {
    Some(CatalogOutcome::Ok(
        names
            .iter()
            .map(|(n, native, installed)| HarnessChoice {
                name: n.to_string(),
                native: *native,
                installed: *installed,
                models: Vec::new(),
                more: Vec::new(),
                catalog_error: None,
                models_error: None,
                // Free-text surface by default: the effort chip stays
                // offered in tests that do not name a list.
                efforts: Some(Vec::new()),
                permission_modes: Some(Vec::new()),
                launch_flags: None,
            })
            .collect(),
        None,
        Vec::new(),
    ))
}

fn sync_catalog(v: &mut View) {
    // Mirror the run loop's rx arm: land the catalog and sync the draft's
    // harness names through the same seam the client calls.
    if let Some(l) = v.launcher.as_mut() {
        super::agent_launcher::sync_harness_names(l, &v.launcher_catalog);
    }
}

fn type_message(v: &mut View, text: &str) {
    // Focus the message field from a fresh dock: Harness -> Project ->
    // Message.
    if let Some(l) = v.launcher.as_mut() {
        l.focus = Focus::Message;
    }
    let mut esc = LauncherEsc::default();
    let keys = esc.fold(text.as_bytes());
    v.launcher_esc = esc;
    let _ = keys;
    // Route the same bytes through the real key folder so editor state and
    // folding agree.
    let bytes = text.as_bytes().to_vec();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(v, &bytes, &mut sock).await;
    });
}
#[test]
fn dock_lifecycle_rows() {
    // The unit twin of the pty repro: the scanner flushes [0x1b] into
    // launcher_keys after its quiet window, and the dock's own carry must
    // release a trailing lone ESC as a bare Esc press, not re-buffer it.
    let mut v = view_with_launcher();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(v.launcher.is_none(), "one lone Esc byte closes the dock");
    assert!(
        v.launcher_closed.is_some(),
        "the draft is retained for reopen"
    );

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    type_message(&mut v, "ship it");
    let revision_before = v.launcher.as_ref().unwrap().draft.revision;
    close(&mut v);
    assert!(v.launcher.is_none(), "dock hidden");
    assert!(v.launcher_closed.is_some(), "draft retained");
    // Reopen: same draft, same revision, nothing lost.
    open(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "ship it");
    assert_eq!(l.draft.revision, revision_before);
    assert_eq!(l.draft.harnesses, vec!["claude", "codex"]);

    let mut v = view_with_launcher();
    v.sideline_full = true;
    type_message(&mut v, "keep me");
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b ", &mut sock).await;
    });
    assert!(!v.sideline_full, "Esc leaves full-screen sideline");
    assert!(v.launcher.is_none(), "composer closed");
    assert_eq!(
        v.launcher_closed.as_ref().unwrap().draft.message,
        "keep me",
        "draft retained"
    );
}

#[test]
fn arrow_reaches_the_dock_whole_or_scanner_rejoined() {
    // AC1-EDGE: an arrow in one chunk, or as the scanner-rejoined pair a
    // lone-Esc read plus a follow-up, moves the cursor / focus and never
    // reads as Esc.
    let mut v = view_with_launcher();
    if let Some(l) = v.launcher.as_mut() {
        l.focus = Focus::Message;
    }
    type_message(&mut v, "ab");
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[A", &mut sock).await;
    });
    // Whole-chunk arrow: the cursor moves inside the editor, no close.
    assert!(v.launcher.is_some(), "an arrow never closes the dock");
    // The scanner rejoins an arrow into ONE chunk, so the fold sees
    // `\x1b` + `[A` in a single read and folds Up, never Esc.
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x1b[A"),
        vec![super::agent_launcher::LKey::Up],
        "a rejoined arrow folds Up, not Esc"
    );
}
#[test]
fn paste_rows() {
    let mut v = view_with_launcher();
    type_message(&mut v, "");
    // A paste whose payload looks like an arrow sequence must land as text.
    let payload = b"\x1b[200~\x1b[Ahello\x1b[201~";
    type_message(&mut v, std::str::from_utf8(payload).unwrap());
    let l = v.launcher.as_ref().unwrap();
    assert!(
        l.draft.message.contains("\x1b[Ahello"),
        "pasted bytes are data: {}",
        l.draft.message
    );

    let mut esc = LauncherEsc::default();
    // The emoji U+1F600 is four bytes; split after the first.
    let full = "\u{1f600}".as_bytes();
    let first = esc.fold(&full[..1]);
    assert!(first.is_empty(), "an incomplete sequence waits");
    let rest = esc.fold(&full[1..]);
    assert_eq!(rest, vec![super::agent_launcher::LKey::Char('\u{1f600}')]);
}

#[test]
fn the_line_edit_grammar_folds_and_edits() {
    // The Cmd line-edit grammar, three sections: each byte spelling folds to
    // exactly one key, never Esc-then-something; the keys edit the draft
    // (word motion, Home/End, word kill); and a pasted path lands once, at
    // the cursor, even when the paste arrives split across reads.
    let cases: &[(&[u8], super::agent_launcher::LKey)] = &[
        (b"\x1b\x7f", super::agent_launcher::LKey::KillWord),
        (b"\x15", super::agent_launcher::LKey::KillLeft),
        (b"\x1bb", super::agent_launcher::LKey::WordLeft),
        (b"\x1bf", super::agent_launcher::LKey::WordRight),
        (b"\x1b[1;3D", super::agent_launcher::LKey::WordLeft),
        (b"\x1b[1;3C", super::agent_launcher::LKey::WordRight),
        (b"\x1b[1;5D", super::agent_launcher::LKey::Left),
        (b"\x1b[D", super::agent_launcher::LKey::Left),
        (b"\x1b[H", super::agent_launcher::LKey::Home),
        (b"\x1b[F", super::agent_launcher::LKey::End),
        (b"\x1bOH", super::agent_launcher::LKey::Home),
        (b"\x1bOF", super::agent_launcher::LKey::End),
        (b"\x1b[127;3u", super::agent_launcher::LKey::KillWord),
    ];
    for (bytes, want) in cases {
        let mut esc = LauncherEsc::default();
        assert_eq!(esc.fold(bytes), vec![want.clone()], "{bytes:?}");
    }

    let mut v = view_with_launcher();
    type_message(&mut v, "launch the /Users/bb16/x.png thing");
    let cur = |v: &View| v.launcher.as_ref().unwrap().draft.cursor_chars;
    let text = |v: &View| v.launcher.as_ref().unwrap().draft.message.clone();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Option+Left (ESC b) from the end lands on `thing`'s first char.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1bb", &mut sock).await;
    });
    assert_eq!(cur(&v), 29, "one word left");
    // Cmd+Left/Right spell as Home/End.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[H", &mut sock).await;
    });
    assert_eq!(cur(&v), 0, "Home jumps to the row start");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[F", &mut sock).await;
    });
    assert_eq!(cur(&v), 34, "End jumps to the row end");
    // Two Option+Right from the row start land after `the`; the word kill
    // takes `the ` in one press.
    rt.block_on(async {
        let _ =
            super::agent_launcher::launcher_keys(&mut v, b"\x1b[H\x1bf\x1bf\x1b\x7f", &mut sock)
                .await;
    });
    assert_eq!(cur(&v), 6, "cursor after the kill");
    assert_eq!(
        text(&v),
        "launch /Users/bb16/x.png thing",
        "the word and its trailing space are gone"
    );
    // Word motion is row-aware through a newline.
    v.launcher.as_mut().unwrap().draft.message.clear();
    v.launcher.as_mut().unwrap().draft.cursor_chars = 0;
    type_message(&mut v, "ab\ncd");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[H", &mut sock).await;
    });
    assert_eq!(cur(&v), 3, "Home lands on the second row's head");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1bb", &mut sock).await;
    });
    assert_eq!(cur(&v), 0, "word left is flat: over the newline onto `ab`");

    // The paste section: a fresh draft, cursor parked mid-word.
    v.launcher.as_mut().unwrap().draft.message.clear();
    v.launcher.as_mut().unwrap().draft.cursor_chars = 0;
    type_message(&mut v, "claude-code-claude-code.png");
    type_message(&mut v, "\x1b[D\x1b[D");
    let mid = "claude-code-claude-code.png".chars().count() - 2;
    assert_eq!(v.launcher.as_ref().unwrap().draft.cursor_chars, mid);
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(
            &mut v,
            b"\x1b[200~/Users/bb16/Pictures/x",
            &mut sock,
        )
        .await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b".png\x1b[201~", &mut sock).await;
    });
    let want = "claude-code-claude-code.p".to_string() + "/Users/bb16/Pictures/x.png" + "ng";
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, want, "the paste lands once, at the cursor");
    assert_eq!(
        l.draft.cursor_chars,
        mid + "/Users/bb16/Pictures/x.png".chars().count(),
        "the cursor rides the paste's tail"
    );
}
#[test]
fn submit_refusal_rows() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[
        ("claude", true, true),
        ("gemini", true, false),
        ("hermes", false, true),
    ]);
    sync_catalog(&mut v);
    // Point the draft at the not-installed one; the submission door refuses
    // it AT SUBMIT with its own reason (the body lists selectable rows only).
    if let Some(l) = v.launcher.as_mut() {
        let idx = l
            .draft
            .harnesses
            .iter()
            .position(|h| h == "hermes")
            .unwrap();
        l.draft.harness_idx = idx;
        l.focus = Focus::Message;
    }
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    match &l.phase {
        Phase::Refused { reason, .. } => {
            assert!(
                reason.contains("no native fno spawn") || reason.contains("not installed"),
                "reason: {reason}"
            );
        }
        other => panic!("expected a pre-wire refusal, got {other:?}"),
    }
    assert_eq!(l.draft.message, "", "draft untouched");
    assert!(sock.is_empty(), "nothing went on the wire");

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    let update = AgentLaunchUpdate {
        request_id: 1,
        state: LaunchState::Unknown {
            reason: "launch timed out".into(),
        },
    };
    apply_launch_update(&mut v, update);
    assert!(matches!(
        v.launcher.as_ref().unwrap().phase,
        Phase::Unknown { .. }
    ));
    // The attempt is remembered BEYOND the sheet (close + reopen).
    close(&mut v);
    open(&mut v);
    assert!(v.launch_attempt.is_some(), "attempt outlives the sheet");
    // Esc is the explicit cancel while pending: the block resolves, the
    // draft survives, the sheet stays open, and launch is possible again.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(matches!(v.launcher.as_ref().unwrap().phase, Phase::Editing));
    assert!(v.launcher.is_some(), "the canceling Esc does not close");

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
        l.phase = Phase::Submitting { request_id: 1 };
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Unknown {
                reason: "launch timed out".into(),
            },
        },
    );
    // Arm-released Unknown: the exact state the old gate leaked through.
    assert_eq!(v.launcher.as_ref().unwrap().armed, None);
    // ^j (the launch key) is inert until the operator cancels explicitly.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\n", &mut sock).await;
    });
    assert!(matches!(
        v.launcher.as_ref().unwrap().phase,
        Phase::Unknown { .. }
    ));
    assert!(sock.is_empty(), "nothing went on the wire: {sock:?}");
}

#[test]
fn draft_staleness_rows() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    // Arm request 1, then edit the draft (a newer revision).
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
        l.phase = Phase::Submitting { request_id: 1 };
    }
    let update = AgentLaunchUpdate {
        request_id: 1,
        state: LaunchState::Refused {
            reason: "capacity".into(),
        },
    };
    apply_launch_update(&mut v, update);
    assert!(matches!(
        v.launcher.as_ref().unwrap().phase,
        Phase::Refused { .. }
    ));
    // A STALE id (2 never armed by this dock) applies nowhere: the dock's
    // newer draft state survives.
    let stale = AgentLaunchUpdate {
        request_id: 2,
        state: LaunchState::Refused {
            reason: "bogus".into(),
        },
    };
    apply_launch_update(&mut v, stale);
    if let Some(l) = v.launcher.as_ref() {
        assert!(matches!(l.phase, Phase::Refused { ref reason, .. } if reason == "capacity"));
    }

    // AC9-EDGE: the binding rides only while the message's second word
    // (sentence punctuation trimmed) still names the node. A retarget, a
    // prose rewrite or an erasure never binds a stale node; appended
    // prose after the same id keeps it.
    let mut v = plain_view();
    open(&mut v);
    super::agent_launcher::open_with(&mut v, "/fno:target x-1".into(), None, "x-1".into())
        .expect("a fresh draft yields");
    let cases: &[(&str, Option<&str>)] = &[
        ("/fno:target x-2", None),
        ("fix the flake", None),
        ("", None),
        ("/fno:target x-1 focus on the flake", Some("x-1")),
        ("/fno:target x-1.", Some("x-1")),
    ];
    for (message, want) in cases {
        if let Some(l) = v.launcher.as_mut() {
            l.draft.message = message.to_string();
        }
        let req = v.launcher.as_ref().unwrap().draft.request(1);
        assert_eq!(
            req.node.as_deref(),
            *want,
            "message {message:?} binds {want:?}"
        );
    }
}

#[test]
fn launched_pane_update_returns_the_focus_pane_and_the_seed_note() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    let update = AgentLaunchUpdate {
        request_id: 1,
        state: LaunchState::Launched {
            name: "quiet-otter".into(),
            pane: Some(7),
            seed_delivered: Some(false),
        },
    };
    let pane = apply_launch_update(&mut v, update);
    assert_eq!(pane, Some(7), "the requesting client focuses the new pane");
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Launched {
            name, seed_note, ..
        } => {
            assert_eq!(name, "quiet-otter");
            assert_eq!(*seed_note, Some("interactive launch; no seed sent"));
        }
        other => panic!("expected Launched, got {other:?}"),
    }
    // The arm is released: a deliberate retry can arm a fresh request.
    assert_eq!(v.launcher.as_ref().unwrap().armed, None);
}

#[test]
fn footer_names_the_lifecycle_and_the_refusal_reason() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Refused {
                reason: "spawn gate: no free slot".into(),
            },
        },
    );
    let footer = v.launcher.as_ref().unwrap().footer();
    assert!(
        footer.contains("refused") && footer.contains("no free slot"),
        "footer: {footer}"
    );
}
/// A Starting attempt whose update is lost must stay escapable: Esc during
/// Submitting cancels the attempt and returns the sheet to Editing (retry
/// then arms a fresh id, one attempt each).
#[test]
fn submitting_sheet_offers_cancel_and_recovers_to_editing() {
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
        l.phase = Phase::Submitting { request_id: 1 };
    }
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    assert!(matches!(v.launcher.as_ref().unwrap().phase, Phase::Editing));
    assert_eq!(v.launcher.as_ref().unwrap().armed, None);
}

/// A degraded catalog re-probes on the next open instead of sticking for
/// the session.

#[test]
fn click_rows() {
    // AC2-HP, mouse half: a press on a chip focuses it and drops that
    // axis's picker one row under the chip.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(&v).unwrap();
    let (_, r) = *sl
        .chips
        .iter()
        .find(|(f, _)| *f == Focus::Project)
        .expect("the Project chip is painted");
    let rep = crate::mouse::MouseReport {
        kind: crate::proto::MouseKind::Press(crate::proto::MouseButton::Left),
        row: sl.origin.0 + 1 + r.y,
        col: sl.origin.1 + 1 + r.x,
        shift: false,
        alt: false,
    };
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let consumed = super::agent_launcher::launcher_mouse(&mut v, rep, &mut sock)
            .await
            .unwrap();
        assert!(consumed, "a click on the sheet is consumed");
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.focus, Focus::Project, "the click landed on the chip");
    let picker = l.picker.as_ref().expect("the click opened the picker");
    assert_eq!(picker.field, Focus::Project);

    // AC3-CLICK: the chip is a real target in the picker's mouse path - the
    // click reads exactly as pressing Esc (Main list closes the picker).
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    type_message(&mut v, "keep me");
    // Drop the Project picker directly: the opener's key choreography is the
    // chip-click test's subject, not this one's.
    {
        let l = v.launcher.as_mut().unwrap();
        let opened = super::agent_launcher::open_picker_at(
            l,
            &v.launcher_catalog,
            &v.backlog,
            Some((4, 6)),
            Focus::Project,
        );
        assert!(opened, "the Project picker opened");
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let l = v.launcher.as_ref().unwrap();
    let picker = l.picker.as_ref().expect("the picker is open");
    let r = picker.popup.render((v.term.0, v.term.1));
    // The chip's click span on the framed title border.
    let (hit_row, hit_col) = r
        .lines
        .iter()
        .enumerate()
        .find_map(|(i, line)| {
            line.hits
                .iter()
                .find(|(t, _, _)| *t == crate::chrome::ESC_CLOSE_HIT)
                .map(|(_, off, len)| {
                    (
                        r.origin.0 as u16 + i as u16,
                        r.origin.1 as u16 + (off + len / 2) as u16,
                    )
                })
        })
        .expect("the picker's esc chip carries a hit span");
    rt.block_on(super::click_close(&mut v, (hit_row, hit_col)));
    let l = v.launcher.as_ref().unwrap();
    assert!(l.picker.is_none(), "the chip click closed the picker");
    assert_eq!(l.draft.message, "keep me", "the draft keeps its value");

    // AC3-CLICK: the composer's keybar esc word is the chip; the click is
    // the Esc key's gesture (hide + retain at rest).
    let mut v = view_with_launcher();
    type_message(&mut v, "keep me");
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(&v).unwrap();
    let r = sl.esc_rect.expect("the keybar names an esc word");
    let rep = crate::mouse::MouseReport {
        kind: crate::proto::MouseKind::Press(crate::proto::MouseButton::Left),
        row: sl.origin.0 + 1 + r.y,
        col: sl.origin.1 + 1 + r.x + 2,
        shift: false,
        alt: false,
    };
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let consumed = super::agent_launcher::launcher_mouse(&mut v, rep, &mut sock)
            .await
            .unwrap();
        assert!(consumed, "a click on the sheet's esc word is consumed");
    });
    assert!(v.launcher.is_none(), "the esc word closed the sheet");
    assert_eq!(
        v.launcher_closed
            .as_ref()
            .expect("hidden with retain")
            .draft
            .message,
        "keep me",
        "the draft is retained"
    );
}
#[test]
fn chips_carry_values_not_axis_names() {
    // AC1-HP, values half: every chip reads its axis's current VALUE, never
    // the axis label the old tab bar painted.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.chip_label(Focus::Harness, &v.launcher_catalog), "claude");
    assert_eq!(l.chip_label(Focus::Model, &v.launcher_catalog), "default");
    assert_eq!(l.chip_label(Focus::Where, &v.launcher_catalog), "Local");
    assert_eq!(l.chip_label(Focus::Permission, &v.launcher_catalog), "auto");
    assert_eq!(l.chip_label(Focus::Effort, &v.launcher_catalog), "default");
    // A non-default placement rides the Where chip's value.
    let mut l = l.clone();
    l.draft.placement = super::agent_launcher::Placement::ThreadSplitBeside;
    assert!(l
        .chip_label(Focus::Where, &v.launcher_catalog)
        .contains("split beside"));
}
/// The regression behind the model-floor contract: a claude account with no
/// pinned model
/// emptied the Model tab, because the catalog read every harness but
/// opencode solely from account records. The capability table now floors
/// each harness's list with its own measured model ids.
#[test]
fn model_floor_rows() {
    let parsed: toml::Value = toml::from_str(super::agent_launcher::CAPABILITY_TOML).unwrap();
    let floor = |harness: &str| -> Vec<String> {
        parsed["harness"][harness]["models"]
            .as_array()
            .unwrap_or_else(|| panic!("{harness} carries a models floor"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let claude = floor("claude");
    for want in ["opus", "sonnet", "haiku", "fable", "claude-opus-5-5"] {
        assert!(
            claude.iter().any(|m| m == want),
            "claude floor names {want}: {claude:?}"
        );
    }
    let codex = floor("codex");
    for want in ["gpt-6-luna", "gpt-6-astra", "gpt-5.5"] {
        assert!(
            codex.iter().any(|m| m == want),
            "codex floor names {want}: {codex:?}"
        );
    }
    assert!(
        parsed["harness"]["opencode"].get("models").is_none(),
        "opencode owns its list through its model command; no floor"
    );

    let cache = r#"{"models":[
        {"slug":"gpt-6-luna","visibility":"list"},
        {"slug":"gpt-reserve","visibility":"hide"},
        {"slug":"gpt-6-luna"},
        {"slug":"gpt-6-astra"}
    ]}"#;
    let (models, hidden) = super::agent_launcher::parse_codex_models(cache);
    let ids: Vec<&str> = models.iter().map(|m| m.model.as_str()).collect();
    assert_eq!(
        ids,
        vec!["gpt-6-luna", "gpt-6-astra"],
        "hide drops, dupes collapse"
    );
    assert_eq!(
        hidden,
        vec!["gpt-reserve"],
        "hidden slugs come back by name"
    );
    assert!(
        models
            .iter()
            .all(|m| m.provider.is_none() && matches!(m.state, ModelState::Ready)),
        "cache slugs are harness-native choices"
    );
    let (empty_models, empty_hidden) = super::agent_launcher::parse_codex_models("not json");
    assert!(
        empty_models.is_empty() && empty_hidden.is_empty(),
        "an unreadable cache is the floor-stands case, never an error"
    );
    assert!(
        super::agent_launcher::parse_codex_models("{}").0.is_empty(),
        "a cache without a models list is the same floor-stands case"
    );

    let floor = vec![super::agent_launcher::ModelChoice {
        name: "opus".into(),
        model: "opus".into(),
        route: String::new(),
        provider: None,
        state: ModelState::Ready,
        key_env: None,
        key_file: None,
    }];
    let pins = super::agent_launcher::parse_configured_account_models(
        r#"{"value":[{"id":"a","harness":"claude","route_provider_id":"zai","model_name":"glm-5.3-flash[1m]","route":"zai/glm-5.3-flash[1m]"}]}"#,
    )
    .unwrap();
    let mut merged = floor;
    super::agent_launcher::merge_model_choices(&mut merged, &pins["claude"]);
    super::agent_launcher::merge_model_choices(&mut merged, &pins["claude"]);
    assert_eq!(merged.len(), 2, "floor first, the routed pin merged once");
    assert_eq!(merged[0].name, "opus", "the floor leads");
    assert_eq!(
        merged[1].provider.as_deref(),
        Some("zai"),
        "the pin keeps its route"
    );
}

/// The regression that forced the tab rewrite: the unavailable row's LABEL
/// used to be ellipsized to fit the long error hint, so "model list
/// unavailable" never reached the screen in CI. Labels stay whole; the
/// hint truncates instead; the selection spans the full inner width.

#[test]
fn degraded_inventory_rows() {
    // When the inventory read fails, the model list shows a disabled entry
    // carrying the reason and the harness default stays launchable;
    // No model row is fabricated when the configured inventory is absent.
    let mut v = view_with_launcher();
    v.launcher_catalog = Some(CatalogOutcome::Ok(
        vec![HarnessChoice {
            name: "claude".into(),
            native: true,
            installed: true,
            models: Vec::new(),
            more: Vec::new(),
            catalog_error: None,
            models_error: None,
            efforts: Some(Vec::new()),
            permission_modes: Some(Vec::new()),
            launch_flags: None,
        }],
        Some("routing inventory unavailable".into()),
        Vec::new(),
    ));
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let (rows, _) =
        super::agent_launcher::picker_rows(&l, Focus::Model, &v.launcher_catalog, &v.backlog);
    let disabled: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Entry {
                label,
                enabled: false,
                ..
            } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    let enabled: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Entry {
                label,
                enabled: true,
                ..
            } => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        disabled.iter().any(|d| d.contains("unavailable")),
        "the failure is named: {disabled:?}"
    );
    assert!(
        enabled.contains(&"harness default"),
        "the default row still launches: {enabled:?}"
    );
    assert!(
        !enabled.iter().any(|d| d.contains("type a model")),
        "no invented model row: {enabled:?}"
    );
    assert!(
        !v.launcher.as_ref().unwrap().draft.harnesses.is_empty(),
        "harness choices survive the degraded model list"
    );

    let mut v = plain_view();
    v.launcher_catalog = Some(CatalogOutcome::Degraded("probe failed".into()));
    open(&mut v);
    assert!(v.catalog_want, "a degraded read re-arms the probe");
}

/// A claude catalog with a captured flags list and the facts row the
/// worktree resolve wants; the sidecar answers at the worktree generation.
fn pill_harness_view() -> (View, std::sync::MutexGuard<'static, ()>, WireVersionFixture) {
    // One shared FNO_STATE_DIR lock with the model_catalog tests, held for
    // the whole body. Merged tests that already hold the guard call
    // pill_view directly so the non-reentrant lock is taken exactly once.
    let guard = crate::model_catalog::state_env_lock();
    let (v, fx) = pill_view(&guard);
    (v, guard, fx)
}

fn pill_view(_guard: &std::sync::MutexGuard<'static, ()>) -> (View, WireVersionFixture) {
    let mut v = view_with_launcher();
    let (session, _wire_fixture) = wire_fixture_at(95);
    v.session = session;
    let own = std::env::current_dir().unwrap().display().to_string();
    v.launcher_catalog = Some(CatalogOutcome::Ok(
        vec![HarnessChoice {
            name: "claude".into(),
            native: true,
            installed: true,
            models: Vec::new(),
            more: Vec::new(),
            catalog_error: None,
            models_error: None,
            efforts: Some(Vec::new()),
            permission_modes: Some(Vec::new()),
            launch_flags: Some(vec!["--agent <agent>".into(), "--verbose".into()]),
        }],
        None,
        vec![super::agent_launcher::ProjectFacts {
            cwd: own,
            current: Some("main".into()),
            branches: vec!["main".into(), "feature/x".into()],
            policy: Ok("external".into()),
        }],
    ));
    sync_catalog(&mut v);
    (v, _wire_fixture)
}

fn launch_argv(v: &mut View, keys: &[u8]) -> Vec<String> {
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(v, keys, &mut sock).await;
    });
    if sock.is_empty() {
        panic!("no launch went to the wire for {keys:?}");
    }
    let mut wire = std::io::Cursor::new(sock);
    match crate::proto::read_msg_sync(&mut wire).unwrap() {
        crate::proto::ClientMsg::AgentLaunch(request) => request.extra_flags,
        other => panic!("composer wrote a different client message: {other:?}"),
    }
}
#[test]
fn a_chip_owned_typed_flag_still_refuses_at_submit() {
    // AC11-ERR: `--cwd /x` committed verbatim refuses at submit, naming the
    // composer chip that owns it.
    let (mut v, _lock, _fx) = pill_harness_view();
    v.launcher
        .as_mut()
        .unwrap()
        .draft
        .pills
        .push(("--cwd".to_string(), Some("/x".to_string())));
    let mut sock: Vec<u8> = Vec::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    match &l.phase {
        super::agent_launcher::Phase::Refused { reason, .. } => {
            assert!(
                reason.contains("composer chip"),
                "the refusal names the chip: {reason}"
            );
        }
        other => panic!("expected the chip refusal, got {other:?}"),
    }
}
#[test]
fn shell_mode_keys_glyph_and_placeholders() {
    // The composer's bang mode: `!` on an empty input is the mode switch
    // (AC1), every later key is command text with the launch gestures
    // literal (AC3), Backspace on an empty line leaves again (AC2), and
    // the glyph + placeholder paint the mode. Folds the retired
    // paint-marker test's contract (plain glyph and placeholder on a
    // fresh dock and after typing).
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    assert_eq!(v.launcher.as_ref().unwrap().focus, Focus::Message);
    assert_painted(&v, '\u{276f}', "prompt \u{b7} -- flags");

    // AC1: the `!` is consumed as the mode switch.
    type_message(&mut v, "!");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(l.shell);
        assert!(l.draft.message.is_empty(), "the ! is consumed");
    }
    assert_painted(&v, '!', "shell command in");

    // AC3: in shell mode the launch gestures land as text and open no
    // picker.
    type_message(&mut v, "git log --oneline @x");
    {
        let l = v.launcher.as_ref().unwrap();
        assert_eq!(l.draft.message, "git log --oneline @x");
        assert!(l.picker.is_none(), "no picker in shell mode");
    }

    // Typing replaces the placeholder; the glyph stays.
    assert_painted(&v, '!', "git log");

    // AC2: Backspace through the text, then one more leaves shell mode.
    for _ in 0..30 {
        type_message(&mut v, "\x7f");
        if v.launcher
            .as_ref()
            .is_some_and(|l| l.draft.message.is_empty())
        {
            break;
        }
    }
    assert!(v.launcher.as_ref().unwrap().shell, "still in shell mode");
    type_message(&mut v, "\x7f");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(!l.shell, "the empty-line Backspace left shell mode");
        assert!(l.draft.message.is_empty());
    }
    assert_painted(&v, '\u{276f}', "prompt \u{b7} -- flags");

    // Folded from the retired newline test: ^j inserts a newline in the
    // message and Enter never fires from inside it.
    type_message(&mut v, "line one");
    type_message(&mut v, "\nline two");
    {
        let l = v.launcher.as_ref().unwrap();
        assert_eq!(l.draft.message, "line one\nline two");
        assert_eq!(l.armed, None, "Enter inside the message never submits");
    }
    for _ in 0..30 {
        type_message(&mut v, "\x7f");
        if v.launcher
            .as_ref()
            .is_some_and(|l| l.draft.message.is_empty())
        {
            break;
        }
    }

    // AC3: a `!` mid-text is literal, and a pasted `!ls` never enters
    // shell mode.
    type_message(&mut v, "fix the !bang parser");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(!l.shell);
        assert_eq!(l.draft.message, "fix the !bang parser");
    }
    type_message(&mut v, "\x1b[200~!ls\x1b[201~");
    {
        let l = v.launcher.as_ref().unwrap();
        assert!(!l.shell, "a paste never enters shell mode");
        assert!(l.draft.message.contains("!ls"), "paste is literal text");
        assert!(l.picker.is_none());
    }
}

/// Paint the sheet and assert the first editor row carries `glyph` and
/// `placeholder` (the placeholder text may be a prefix of a longer one).
fn assert_painted(v: &View, glyph: char, placeholder: &str) {
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(v).unwrap();
    let (rows_n, cols) = (v.term.0 as usize, v.term.1 as usize);
    let mut cells = vec![crate::proto::Cell::default(); rows_n * cols];
    l.paint_sheet(v, &mut cells, rows_n, cols, &sl);
    let (oy, ox) = (sl.origin.0 as usize + 1, sl.origin.1 as usize + 1);
    let row: String = (0..cols.saturating_sub(ox + 1))
        .map(|x| cells[(oy + sl.message.y as usize) * cols + ox + x].c)
        .collect();
    assert!(row.contains(glyph), "glyph {glyph} painted: {row:?}");
    assert!(
        row.contains(placeholder),
        "placeholder {placeholder:?} painted: {row:?}"
    );
}

#[test]
fn picker_filter_rows() {
    // AC2-HP: Enter on a chip opens its picker; typing narrows the rows in
    // place; Backspace widens again; the chip's own value is untouched.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Walk to the Harness chip and open its picker.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\t\t", &mut sock).await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    assert!(v.launcher.as_ref().unwrap().picker.is_some());
    // Type `cod`: only the codex row survives the filter.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"cod", &mut sock).await;
    });
    let read = |v: &View| -> Vec<String> {
        let l = v.launcher.as_ref().unwrap();
        let picker = l.picker.as_ref().unwrap();
        picker
            .popup
            .rows
            .iter()
            .filter_map(|r| match r {
                crate::popup::PopupRow::Entry { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        read(&v),
        vec!["codex"],
        "the query narrows the rows: {:?}",
        read(&v)
    );
    assert_eq!(
        v.launcher.as_ref().unwrap().picker.as_ref().unwrap().filter,
        "cod",
        "the query is live"
    );
    // Backspace once: the query `co` still narrows to codex.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, &[0x7f], &mut sock).await;
    });
    assert_eq!(read(&v), vec!["codex"], "`co` still filters");
    // Clearing the query fully restores every row.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, &[0x7f, 0x7f], &mut sock).await;
    });
    assert_eq!(read(&v).len(), 2, "widened");

    // Filtering keeps the target/action mapping on the actual harness row.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Walk to the Harness chip, open the picker, and narrow to codex.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\t\t", &mut sock).await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"co", &mut sock).await;
    });
    {
        let l = v.launcher.as_ref().unwrap();
        let picker = l.picker.as_ref().unwrap();
        assert_eq!(picker.filter, "co", "the query is live");
        let rows: Vec<_> = picker
            .popup
            .rows
            .iter()
            .filter(|r| matches!(r, crate::popup::PopupRow::Entry { .. }))
            .collect();
        assert_eq!(rows.len(), 1, "narrowed to one row: {rows:?}");
    }
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, &[0x0d], &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.harness(), "codex", "the highlighted row committed");
    assert!(l.picker.is_none(), "the pick closed the picker");
}

#[test]
fn at_opens_the_node_picker_and_picking_inserts_the_id() {
    // Change 7: `@` in the message opens a node picker over the layout's
    // backlog cards; the glyph never lands in the draft; the pick inserts
    // `<id> ` at the cursor.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.focus = Focus::Message;
    }
    // One backlog card for the picker to list (the fixture ships none).
    v.backlog = vec![crate::proto::BacklogCard {
        id: "x-6233".into(),
        slug: "a-node".into(),
        priority: "p1".into(),
        state: crate::proto::CardState::Ready,
        pane_id: None,
        attach_id: None,
        where_hint: None,
        project: None,
        lane: None,
        plan_path: None,
        head: false,
        link: None,
    }];
    type_message(&mut v, "plan ");
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"@", &mut sock).await;
    });
    let picker = v.launcher.as_ref().unwrap().picker.as_ref().unwrap();
    assert_eq!(
        picker.field,
        Focus::Message,
        "the node picker rides the message field"
    );
    let entry_count = picker
        .popup
        .rows
        .iter()
        .filter(|r| matches!(r, crate::popup::PopupRow::Entry { .. }))
        .count();
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.message,
        "plan ",
        "the @ never lands in the draft"
    );
    // Pick the first card: its id lands at the cursor.
    let first_id: String = picker
        .actions
        .iter()
        .find_map(|a| match a {
            Some(super::agent_launcher::PickerAction::InsertNode(id)) => Some(id.clone()),
            _ => None,
        })
        .expect("the fixture carries at least one InsertNode row");
    assert!(entry_count >= 1, "cards list: {entry_count}");
    let mut l = v.launcher.take().unwrap();
    let action = super::agent_launcher::PickerAction::InsertNode(first_id.clone());
    super::agent_launcher::apply_picker_action(
        &mut l,
        &v.launcher_catalog,
        action,
        0,
        Focus::Message,
    );
    v.launcher = Some(l);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.message,
        format!("plan {first_id} "),
        "the node id lands at the cursor"
    );
}

#[test]
fn open_with_rows() {
    // AC6-HP: the board prefill lands the message, the cursor at its end,
    // the node's project (appended when it was not a candidate) and the
    // node binding; the wire request carries the node. The phase resets to
    // Editing so the operator's next Launch submits fresh.
    let mut v = plain_view();
    open(&mut v);
    super::agent_launcher::open_with(
        &mut v,
        "/fno:target x-1".into(),
        Some("/r/footnote"),
        "x-1".into(),
    )
    .expect("a fresh draft yields");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "/fno:target x-1");
    assert_eq!(l.draft.cursor_chars, "/fno:target x-1".chars().count());
    let idx = l.draft.project_idx;
    assert_eq!(l.draft.projects[idx], "/r/footnote");
    assert_eq!(l.draft.node.as_deref(), Some("x-1"));
    assert_eq!(l.draft.request(9).node.as_deref(), Some("x-1"));
    assert_eq!(l.phase, Phase::Editing);
    assert_eq!(l.focus, Focus::Message);

    // AC7-EDGE: a kept draft with typed text is never overwritten; the
    // error names the way out and the draft is unchanged.
    let mut v = plain_view();
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.message = "fix the flake".into();
        l.draft.revision += 1;
    }
    let err = super::agent_launcher::open_with(
        &mut v,
        "/fno:target x-1".into(),
        Some("/r/footnote"),
        "x-1".into(),
    )
    .expect_err("a held draft refuses");
    assert!(
        err.contains("holds a draft"),
        "the refusal names the kept draft: {err}"
    );
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "fix the flake");
    assert_eq!(l.draft.node, None);

    // The in-flight guard: even an EMPTY draft does not yield while an
    // owned attempt is Starting. A seedless launch's outcome must fold
    // onto the dock it belongs to, never onto a fresh board prefill.
    let mut v = plain_view();
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.armed = Some(1);
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Starting,
        },
    );
    let err =
        super::agent_launcher::open_with(&mut v, "/fno:target x-1".into(), None, "x-1".into())
            .expect_err("an in-flight attempt holds the draft");
    assert!(err.contains("holds a draft"), "err: {err}");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.phase, Phase::Submitting { request_id: 1 });
    assert_eq!(l.draft.node, None);

    // AC8-EDGE: a terminal `Launched` attempt makes way for another node's
    // prefill; the phase is Editing again.
    let mut v = plain_view();
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.message = "/fno:target x-9".into();
        l.armed = Some(1);
    }
    apply_launch_update(
        &mut v,
        AgentLaunchUpdate {
            request_id: 1,
            state: LaunchState::Launched {
                name: "w".into(),
                pane: Some(3),
                seed_delivered: Some(true),
            },
        },
    );
    super::agent_launcher::open_with(&mut v, "/fno:target x-1".into(), None, "x-1".into())
        .expect("a launched draft yields");
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "/fno:target x-1");
    assert_eq!(l.draft.node.as_deref(), Some("x-1"));
    assert_eq!(l.phase, Phase::Editing);
}
#[test]
fn probe_projects_prefers_the_open_draft_and_falls_back_to_candidates() {
    // The probe reads facts for the projects the dock actually shows; with
    // the dock closed it still probes the fresh-draft list so the first
    // open lands facts together with the catalog.
    let mut v = plain_view();
    let own = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    assert!(
        super::agent_launcher::probe_projects(&v)
            .iter()
            .any(|p| *p == own),
        "a closed dock probes the candidate list: {:?}",
        super::agent_launcher::probe_projects(&v)
    );
    open(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        l.draft.projects = vec!["/tmp/proj-a".into(), "/tmp/proj-b".into()];
    }
    assert_eq!(
        super::agent_launcher::probe_projects(&v),
        vec!["/tmp/proj-a".to_string(), "/tmp/proj-b".to_string()]
    );
}
#[test]
fn the_composer_picker_never_lists_the_retired_gemini() {
    // The user retired gemini: load_catalog filters the capability table's
    // row out of the picker list (the table keeps it for resume argv and
    // state grants).
    let _env = crate::model_catalog::state_env_lock();
    let dir = fresh_state_dir();
    std::env::set_var("FNO_STATE_DIR", &dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let outcome = rt.block_on(super::agent_launcher::load_catalog(Vec::new()));
    let rows = match outcome {
        CatalogOutcome::Ok(rows, _, _) => rows,
        CatalogOutcome::Degraded(reason) => panic!("harness rows must not degrade: {reason}"),
    };
    assert!(
        rows.iter().all(|r| r.name != "gemini"),
        "gemini never lists in the composer picker"
    );
    assert!(
        rows.iter().any(|r| r.name == "claude"),
        "the rest of the catalog still loads"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh state root with no cache/ subdir, so a fetch cannot even write.
fn fresh_state_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fno-launcher-catalog-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sheet_paint_rows() {
    // AC1-HP: no tab strip and no values strip; a chip row above the input
    // and the bottom row (`mode` left, harness/model/effort right). The
    // `+` chip left the row: typed flags are pills now.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(&v).unwrap();
    let inner_w = sl.framed_w.saturating_sub(2) as usize;
    let (rows_n, cols) = (v.term.0 as usize, v.term.1 as usize);
    let mut cells = vec![crate::proto::Cell::default(); rows_n * cols];
    l.paint_sheet(&v, &mut cells, rows_n, cols, &sl);
    let (oy, ox) = (sl.origin.0 as usize + 1, sl.origin.1 as usize + 1);
    let row_text = |y: usize| -> String {
        (0..inner_w)
            .map(|x| cells[(oy + y as usize) * cols + ox + x].c)
            .collect()
    };
    // The chip values paint; the capitalized axis names of the old tab bar
    // never do.
    for chip in ["Local", "auto", "claude", "default"] {
        let seen = (0..sl.framed_h)
            .map(row_text)
            .any(|text| text.contains(chip));
        assert!(seen, "chip {chip:?} paints somewhere on the sheet");
    }
    for tab in ["Harness", "Mode", "Flags"] {
        let seen = (0..sl.framed_h)
            .map(row_text)
            .any(|text| text.contains(tab));
        assert!(!seen, "no tab strip: {tab:?} never paints");
    }

    // AC1-EDGE: a narrow sheet wraps the right group (harness/model/effort)
    // to its own row instead of truncating a value.
    let mut v = plain_view();
    v.term = (24, 50);
    open(&mut v);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(&v).unwrap();
    let y_of = |f: Focus| -> u16 {
        sl.chips
            .iter()
            .find(|(f2, _)| *f2 == f)
            .expect("chip present")
            .1
            .y
    };
    assert!(
        y_of(Focus::Harness) > y_of(Focus::Permission),
        "the right group wrapped below the left: {:?}",
        sl.chips
    );
    // A chip value is never truncated below its full text: each rect fits
    // its whole label plus the caret.
    for (f, r) in &sl.chips {
        assert!(
            l.chip_label(*f, &v.launcher_catalog).chars().count() + 1 <= r.width as usize,
            "chip {:?} rect fits its value: {:?} width {}",
            f,
            l.chip_label(*f, &v.launcher_catalog),
            r.width
        );
    }

    // AC3-HP: the line above the chips reads `Working directory` (bold) and
    // the full cwd (regular), only while Project holds focus or the mouse.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Message holds focus on a fresh open: the line stays hidden.
    let paint_row0 = |v: &View| -> String {
        let l = v.launcher.as_ref().unwrap();
        let sl = l.sheet_layout(v).unwrap();
        let inner_w = sl.framed_w.saturating_sub(2) as usize;
        let (rows_n, cols) = (v.term.0 as usize, v.term.1 as usize);
        let mut cells = vec![crate::proto::Cell::default(); rows_n * cols];
        l.paint_sheet(v, &mut cells, rows_n, cols, &sl);
        let (oy, ox) = (sl.origin.0 as usize + 1, sl.origin.1 as usize + 1);
        (0..inner_w)
            .map(|x| cells[(oy + sl.cwd_line.y as usize) * cols + ox + x].c)
            .collect()
    };
    assert!(
        !paint_row0(&v).contains("Working directory"),
        "the cwd line hides while Message holds focus"
    );
    // Shift-Tab walks back: worktree box, Branch chip, then Project; the
    // line paints with the full path.
    rt.block_on(async {
        let _ =
            super::agent_launcher::launcher_keys(&mut v, b"\x1b[Z\x1b[Z\x1b[Z", &mut sock).await;
    });
    assert_eq!(v.launcher.as_ref().unwrap().focus, Focus::Project);
    let line = paint_row0(&v);
    let own = std::env::current_dir().unwrap().display().to_string();
    // The first path characters prove the cwd painted beside the label on
    // any host; the tail truncates when the sheet cannot admit the path.
    let head = own.get(..10).unwrap_or(&own).to_string();
    assert!(
        line.contains("Working directory") && line.contains(head.as_str()),
        "the cwd-line assert derives from the real cwd, never a host shape"
    );

    // 40 cols: inner 30, the right group alone is wider. The sheet lays out
    // without overflowing a chip past the row, and paint stays in bounds.
    let mut v = plain_view();
    v.term = (24, 40);
    open(&mut v);
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(&v).unwrap();
    let inner_w = sl.framed_w.saturating_sub(2) as usize;
    for (_, r) in &sl.chips {
        assert!(r.width >= 1, "every chip keeps a paintable rect");
        assert!(
            r.x as usize + r.width as usize <= inner_w,
            "chip rect {r:?} fits the {inner_w}-col row"
        );
    }
    let (rows_n, cols) = (v.term.0 as usize, v.term.1 as usize);
    let mut cells = vec![crate::proto::Cell::default(); rows_n * cols];
    l.paint_sheet(&v, &mut cells, rows_n, cols, &sl);
}

#[test]
fn a_picker_open_across_the_catalog_landing_refreshes_on_input() {
    // The picker opens while the catalog read pends; when the read lands,
    // the next key refreshes the STORED rows before it is handled, so a
    // commit resolves through the rows the operator sees.
    let mut v = view_with_launcher();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\t\t\r", &mut sock).await;
    });
    let picker = v.launcher.as_ref().unwrap().picker.as_ref().unwrap();
    assert!(
        picker.all_rows.iter().any(
            |r| matches!(r, crate::popup::PopupRow::Entry { label, .. } if label == "reading harnesses...")
        ),
        "opened on the pending rows"
    );
    // The catalog lands; one key refreshes the picker in place.
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"x", &mut sock).await;
    });
    let picker = v.launcher.as_ref().unwrap().picker.as_ref().unwrap();
    assert!(
        picker
            .all_rows
            .iter()
            .any(|r| matches!(r, crate::popup::PopupRow::Entry { label, .. } if label == "claude")),
        "the stored rows refreshed: {:?}",
        picker.all_rows
    );
    assert_eq!(picker.filter, "x", "the query survives the refresh");
}

/// One git project's facts row for the draft cwd, with a knobbed policy
/// word: the shape every worktree-box test starts from.
fn git_facts(policy: &str) -> Vec<ProjectFacts> {
    let own = std::env::current_dir().unwrap().display().to_string();
    vec![ProjectFacts {
        cwd: own,
        current: Some("main".into()),
        branches: vec!["main".into(), "feature/x".into()],
        policy: Ok(policy.into()),
    }]
}

#[test]
fn the_box_defaults_to_the_policy_and_never_greys_out() {
    // AC6-HP: an `external` project defaults to checked with Branch `main`;
    // a `never` project paints unchecked and greyed with its reason on the
    // facts line, and the box never toggles there.
    let mut v = view_with_launcher();
    let never = catalog(&[("claude", true, true)]).unwrap();
    let mut never = never;
    if let CatalogOutcome::Ok(rows, err, facts) = &mut never {
        *facts = git_facts("never");
        let _ = (rows, err);
    }
    v.launcher_catalog = Some(never);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "[ ] worktree",
        "never keeps the box unchecked"
    );
    assert_eq!(
        l.chip_label(Focus::Branch, &v.launcher_catalog),
        "main",
        "the Branch chip reads the current branch under never"
    );
    // The facts line names the policy: the greyed state carries its reason.
    let (rows_n, cols) = (v.term.0 as usize, v.term.1 as usize);
    v.launcher.as_mut().unwrap().focus = Focus::Worktree;
    let l = v.launcher.as_ref().unwrap();
    let sl = l.sheet_layout(&v).unwrap();
    let mut cells = vec![crate::proto::Cell::default(); rows_n * cols];
    l.paint_sheet(&v, &mut cells, rows_n, cols, &sl);
    let row0: String = (0..60)
        .map(|x| cells[(sl.origin.0 as usize + 1) * cols + sl.origin.1 as usize + 1 + x].c)
        .collect();
    assert!(
        row0.contains("policy never: runs in place"),
        "the greyed box names its reason: {row0}"
    );
    // Enter toggles nothing under never.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "[ ] worktree",
        "never never toggles"
    );

    // The same draft on an `external` project defaults to checked, Branch
    // `main`, and Enter toggles the box off.
    let mut v = view_with_launcher();
    let mut ext = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(_, _, facts) = &mut ext {
        *facts = git_facts("external");
    }
    v.launcher_catalog = Some(ext);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "[x] worktree",
        "external defaults to checked"
    );
    assert_eq!(
        l.chip_label(Focus::Branch, &v.launcher_catalog),
        "main",
        "a checked box launches `main` by default"
    );
    // Shift-tab once: Message -> Worktree. Enter toggles the box off.
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[Z", &mut sock).await;
    });
    assert_eq!(
        v.launcher.as_ref().unwrap().focus,
        Focus::Worktree,
        "shift-tab reaches the box"
    );
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    assert_eq!(
        v.launcher
            .as_ref()
            .unwrap()
            .chip_label(Focus::Worktree, &v.launcher_catalog),
        "[ ] worktree",
        "Enter on the box toggles it off"
    );
}

#[test]
fn git_policy_refusal_rows() {
    // AC6-EDGE: with the facts unread and no explicit pick, the box reads
    // `worktree ?` and the submit refuses with its reason; toggling the box
    // resolves the state and the launch proceeds.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("claude", true, true)]);
    sync_catalog(&mut v);
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.chip_label(Focus::Worktree, &v.launcher_catalog),
        "worktree ?",
        "the unknown state names itself"
    );
    let (session, _wire_fixture) = wire_fixture_at(95);
    v.session = session;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Refused { reason, .. } => assert!(
            reason.contains("worktree policy unread"),
            "the refusal names the unread policy: {reason}"
        ),
        other => panic!("expected a pre-wire refusal, got {other:?}"),
    }
    // An explicit toggle resolves it: the box turns on and the draft
    // launches past the policy gate.
    v.launcher.as_mut().unwrap().phase = Phase::Editing;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b[Z", &mut sock).await;
    });
    assert_eq!(
        v.launcher.as_ref().unwrap().focus,
        Focus::Worktree,
        "shift-tab reaches the box"
    );
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b" ", &mut sock).await;
    });
    assert_eq!(
        v.launcher
            .as_ref()
            .unwrap()
            .chip_label(Focus::Worktree, &v.launcher_catalog),
        "[x] worktree",
        "the explicit pick clears the ? state"
    );

    // AC6-EDGE covers the failed read as well as the missing one: a facts
    // row whose policy read failed paints `worktree ?` and refuses the
    // launch with its reason instead of guessing a checked default.
    let mut v = view_with_launcher();
    let mut failed = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(_, _, facts) = &mut failed {
        *facts = vec![ProjectFacts {
            cwd: std::env::current_dir().unwrap().display().to_string(),
            current: Some("main".into()),
            branches: vec!["main".into()],
            policy: Err("policy command failed".into()),
        }];
    }
    v.launcher_catalog = Some(failed);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher
            .as_ref()
            .unwrap()
            .chip_label(Focus::Worktree, &v.launcher_catalog),
        "worktree ?",
        "a failed read names itself"
    );
    let (session, _wire_fixture) = wire_fixture_at(95);
    v.session = session;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Refused { reason, .. } => assert!(
            reason.contains("worktree policy unread"),
            "the refusal names the unread policy: {reason}"
        ),
        other => panic!("expected a pre-wire refusal, got {other:?}"),
    }

    use super::agent_launcher::{version_at_least, LAUNCH_EXTRA_AXES_PROTO, LAUNCH_WORKTREE_PROTO};
    assert!(!version_at_least(None, LAUNCH_EXTRA_AXES_PROTO));
    assert!(!version_at_least(Some(90), LAUNCH_EXTRA_AXES_PROTO));
    assert!(version_at_least(Some(91), LAUNCH_EXTRA_AXES_PROTO));
    assert!(version_at_least(Some(92), LAUNCH_EXTRA_AXES_PROTO));
    // The worktree gate sits one generation later than the launch extras.
    assert!(!version_at_least(Some(93), LAUNCH_WORKTREE_PROTO));
    assert!(!version_at_least(Some(94), LAUNCH_WORKTREE_PROTO));
    assert!(version_at_least(Some(95), LAUNCH_WORKTREE_PROTO));

    // AC9-HP: under a wire-91 sidecar a checked box refuses before the wire
    // with the reconnect reason; the same draft on wire 94 submits.
    let mut v = view_with_launcher();
    let mut ext = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(_, _, facts) = &mut ext {
        *facts = git_facts("external");
    }
    v.launcher_catalog = Some(ext);
    sync_catalog(&mut v);
    let (session, _wire_fixture) = wire_fixture_at(91);
    v.session = session;
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    match &v.launcher.as_ref().unwrap().phase {
        Phase::Refused { reason, .. } => assert!(
            reason.contains("does not support worktree launches"),
            "the refusal names the reconnect: {reason}"
        ),
        other => panic!("expected the wire refusal, got {other:?}"),
    }
}
#[test]
fn the_harness_preselect_is_claude_then_codex_never_alphabetical() {
    // First sync on a fresh dock: claude when the catalog has it, codex when
    // claude is missing, the first row only when neither exists. In-session,
    // the retained draft IS the last-harness-used memory.
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[
        ("agy", true, true),
        ("claude", true, true),
        ("codex", true, true),
    ]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "claude",
        "claude preselects over the alphabetical first row"
    );

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("agy", true, true), ("codex", true, true)]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "codex",
        "a missing claude falls to codex"
    );

    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[("agy", true, true)]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "agy",
        "a catalog with neither still shows a value"
    );

    // The mux-dir store outranks the ladder: the harness the last launch
    // used preselects even when claude sits first in the catalog.
    std::fs::create_dir_all(crate::proto::mux_dir()).unwrap();
    std::fs::write(
        crate::proto::mux_dir().join("composer-last-harness"),
        "agy\n",
    )
    .unwrap();
    let mut v = view_with_launcher();
    v.launcher_catalog = catalog(&[
        ("claude", true, true),
        ("codex", true, true),
        ("agy", true, true),
    ]);
    sync_catalog(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "agy",
        "the stored last-used harness outranks the claude ladder"
    );

    // The retained draft carries the last harness used across close/open.
    if let Some(l) = v.launcher.as_mut() {
        l.draft.harness_idx = l.draft.harnesses.iter().position(|h| h == "agy").unwrap();
    }
    close(&mut v);
    open(&mut v);
    assert_eq!(
        v.launcher.as_ref().unwrap().draft.harness(),
        "agy",
        "the retained draft is the last-harness memory"
    );
}
fn floor(name: &str) -> super::agent_launcher::ModelChoice {
    super::agent_launcher::ModelChoice {
        name: name.to_string(),
        model: name.to_string(),
        route: String::new(),
        provider: None,
        state: ModelState::Ready,
        key_env: None,
        key_file: None,
    }
}
fn choice_ready(name: &str, provider: &str) -> super::agent_launcher::ModelChoice {
    let mut m = floor(name);
    m.provider = Some(provider.to_string());
    m
}

fn nokey(name: &str, provider: &str, env: &str) -> super::agent_launcher::ModelChoice {
    let mut m = choice_ready(name, provider);
    m.state = ModelState::NoKey {
        key_env: env.to_string(),
        steps: vec![],
    };
    m.key_env = Some(env.to_string());
    m
}
fn unreachable(name: &str, provider: &str, reason: &str) -> super::agent_launcher::ModelChoice {
    let mut m = choice_ready(name, provider);
    m.state = ModelState::Unreachable {
        reason: reason.to_string(),
    };
    m
}

fn row_labels(rows: &[crate::popup::PopupRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Entry { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect()
}

fn glyph_of(rows: &[crate::popup::PopupRow], label: &str) -> String {
    rows.iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Entry {
                glyph, label: l, ..
            } if l == label => Some(glyph.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .next()
        .unwrap_or_default()
}
#[test]
fn nokey_row_enter_shows_connect_steps_and_esc_returns_to_more() {
    let mut v = view_with_launcher();
    let mut rows = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(choices, _, _) = &mut rows {
        let c = &mut choices[0];
        c.more = vec![nokey_steps(
            "MiniMax-M2",
            "minimax",
            "MINIMAX_API_KEY",
            "export MINIMAX_API_KEY=<your key>",
        )];
    }
    v.launcher_catalog = Some(rows);
    sync_catalog(&mut v);
    let mut l = v.launcher.take().unwrap();
    let anchor = crate::popup::Anchor::At { row: 4, col: 10 };
    super::agent_launcher::open_more(&mut l, &v.launcher_catalog, anchor);
    let action = more_row_action(&l, "MiniMax-M2").expect("the nokey row carries an action");
    assert!(
        matches!(
            action,
            super::agent_launcher::PickerAction::ShowSteps { .. }
        ),
        "a nokey rows enter names the connect steps: {action:?}"
    );
    if let super::agent_launcher::PickerAction::ShowSteps { title, lines } = action {
        assert_eq!(title, "connect minimax");
        assert_eq!(lines, vec!["export MINIMAX_API_KEY=<your key>".to_string()]);
        super::agent_launcher::show_steps(&mut l, title, lines, anchor);
    }
    let picker = l.picker.as_ref().expect("steps sheet open");
    assert_eq!(
        picker.mode,
        super::agent_launcher::PickerMode::Steps {
            title: "connect minimax".to_string()
        }
    );
    let rows_now: Vec<String> = picker
        .all_rows
        .iter()
        .filter_map(|r| match r {
            crate::popup::PopupRow::Header(h) => Some(format!("# {h}")),
            crate::popup::PopupRow::Entry { label, enabled, .. } => {
                Some(format!("{} {}", if *enabled { "+" } else { "-" }, label))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        rows_now,
        vec![
            "# connect minimax".to_string(),
            "- export MINIMAX_API_KEY=<your key>".to_string()
        ],
        "the steps sheet is one header plus disabled lines: {rows_now:?}"
    );
    assert_eq!(l.draft.model, "", "nothing was picked");
    let current = l.picker.take().unwrap();
    super::agent_launcher::picker_step_down(&mut l, &v.launcher_catalog, &v.backlog, current);
    let picker = l.picker.as_ref().expect("back on the more list");
    assert_eq!(picker.mode, super::agent_launcher::PickerMode::More);
}
fn nokey_steps(
    name: &str,
    provider: &str,
    env: &str,
    step: &str,
) -> super::agent_launcher::ModelChoice {
    let mut m = nokey(name, provider, env);
    m.state = ModelState::NoKey {
        key_env: env.to_string(),
        steps: vec![step.to_string()],
    };
    m
}

fn more_row_action(
    l: &super::agent_launcher::Launcher,
    label: &str,
) -> Option<super::agent_launcher::PickerAction> {
    let picker = l.picker.as_ref()?;
    let row = picker.all_rows.iter().position(|r| {
        matches!(r,
        crate::popup::PopupRow::Entry { label: l, .. } if l == label)
    })?;
    picker.all_actions.get(row)?.clone()
}
#[test]
fn unreachable_row_enter_names_the_gap_and_picks_nothing() {
    let mut v = view_with_launcher();
    let mut rows = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(choices, _, _) = &mut rows {
        let c = &mut choices[0];
        c.more = vec![unreachable(
            "glm-5",
            "zai-openai",
            "claude speaks anthropic; zai-openai serves openai",
        )];
    }
    v.launcher_catalog = Some(rows);
    sync_catalog(&mut v);
    let mut l = v.launcher.take().unwrap();
    let anchor = crate::popup::Anchor::At { row: 4, col: 10 };
    super::agent_launcher::open_more(&mut l, &v.launcher_catalog, anchor);
    let action = more_row_action(&l, "glm-5").expect("the unreachable row carries an action");
    let (title, lines) = match action {
        super::agent_launcher::PickerAction::ShowSteps { title, lines } => (title, lines),
        other => panic!("expected ShowSteps, got {other:?}"),
    };
    assert_eq!(title, "zai-openai on claude");
    assert_eq!(
        lines,
        vec!["claude speaks anthropic; zai-openai serves openai".to_string()]
    );
    super::agent_launcher::show_steps(&mut l, title, lines, anchor);
    assert_eq!(l.draft.model, "", "nothing was picked");
}
#[test]
fn launch_refuses_a_pinned_pick_whose_key_does_not_resolve() {
    let mut v = view_with_launcher();
    let mut rows = catalog(&[("claude", true, true)]).unwrap();
    if let CatalogOutcome::Ok(choices, _, _) = &mut rows {
        let c = &mut choices[0];
        c.models = vec![keyed("deepseek-chat", "deepseek", "FNO_TEST_DS_KEY", None)];
    }
    v.launcher_catalog = Some(rows);
    sync_catalog(&mut v);
    if let Some(l) = v.launcher.as_mut() {
        let idx = l
            .draft
            .harnesses
            .iter()
            .position(|h| h == "claude")
            .unwrap();
        l.draft.harness_idx = idx;
    }
    let mut l = v.launcher.take().unwrap();
    let pin = super::agent_launcher::PickerAction::PickRow {
        harness: "claude".to_string(),
        name: "deepseek-chat".to_string(),
        model: "deepseek-chat".to_string(),
        route: String::new(),
        provider: Some("deepseek".to_string()),
    };
    super::agent_launcher::apply_picker_action(&mut l, &v.launcher_catalog, pin, 0, Focus::Model);
    v.launcher = Some(l);
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\r", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    match &l.phase {
        Phase::Refused { reason, .. } => {
            assert!(
                reason.contains("FNO_TEST_DS_KEY is not set"),
                "the refusal names the env var: {reason}"
            );
        }
        other => panic!("expected a key refusal, got {other:?}"),
    }
    assert!(sock.is_empty(), "nothing went on the wire");
}
fn keyed(
    name: &str,
    provider: &str,
    env: &str,
    key_file: Option<String>,
) -> super::agent_launcher::ModelChoice {
    let mut m = choice_ready(name, provider);
    m.key_env = Some(env.to_string());
    m.key_file = key_file;
    m
}

// -- KillLeft (Ctrl+U / Cmd+Backspace) --------------------------------------

/// The composer's contract, one walk: the fold maps both Cmd+Backspace
/// spellings (and Shift+Enter's CSI-u), the kill line edits every editor
/// layer, admission refusals map to canned one-sentence heads with the raw
/// text behind Ctrl+O, `?` opens the help sheet on an empty input and stays
/// text once words exist, and the runtime flags parser reads beside and
/// next-line descriptions while never suggesting a flag the doors own.
#[test]
fn composer_keys_refusals_help_and_flag_parsing_contract() {
    // -- the key fold ------------------------------------------------------
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x15"),
        vec![super::agent_launcher::LKey::KillLeft],
        "Ctrl+U"
    );
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x1b\x7f"),
        vec![super::agent_launcher::LKey::KillWord],
        "ESC+DEL is one key (Option+Backspace, the word kill), never Esc then Backspace"
    );
    let mut esc = LauncherEsc::default();
    assert_eq!(
        esc.fold(b"\x1b[13;2u"),
        vec![super::agent_launcher::LKey::ShiftEnter]
    );

    // -- the draft and the InputField editors ------------------------------
    let mut v = plain_view();
    open(&mut v);
    let l = v.launcher.as_mut().unwrap();
    l.draft.message = "first\nsecond".to_string();
    l.draft.cursor_chars = "first\nsecond".chars().count();
    let sock: Vec<u8> = Vec::new();
    let mut sock = sock;
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x15", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(l.draft.message, "first\n");
    assert_eq!(l.draft.cursor_chars, "first\n".chars().count());

    let mut f = super::input_field::InputField::new("name", 64).with_text("zai");
    f.feed(b"\x15");
    assert_eq!(f.text(), "");
    let mut f = super::input_field::InputField::new("name", 64).with_text("zai");
    f.feed(b"\x1b[D\x1b[D"); // cursor after "z"
    f.feed(b"\x15");
    assert_eq!(f.text(), "ai");

    // -- refusal heads and the Ctrl+O raw toggle ---------------------------
    use super::agent_launcher::first_sentence;
    assert_eq!(
        first_sentence("process admission refused: count=unknown, ceiling=29, reason=measurement-unavailable"),
        "process limit: fno cannot read the machine's load (count=unknown); wait a beat or run it in a terminal",
    );
    assert_eq!(
        first_sentence("process admission refused: machine runaway brake holds (900s left): hot; largest group cargo x40"),
        "process limit: machine runaway brake on; run it in a terminal or wait for the all-clear",
    );
    assert_eq!(
        first_sentence("plain first line\nsecond line"),
        "plain first line",
    );

    let mut v = plain_view();
    open(&mut v);
    let l = v.launcher.as_mut().unwrap();
    l.phase = Phase::Refused {
        request_id: 1,
        reason: "process admission refused: count=unknown, ceiling=29".into(),
    };
    assert_eq!(
        l.footer(),
        "refused: process limit: fno cannot read the machine's load (count=unknown); wait a beat or run it in a terminal (^o raw)",
    );
    l.show_detail = true;
    assert_eq!(
        l.footer(),
        "refused: process admission refused: count=unknown, ceiling=29",
        "Ctrl+O shows the raw text"
    );

    // -- ? opens help on empty input, types otherwise ----------------------
    let mut v = plain_view();
    open(&mut v);
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"?", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert!(
        l.picker
            .as_ref()
            .is_some_and(|p| p.mode == super::agent_launcher::PickerMode::Help),
        "? on an empty input opens the help sheet"
    );
    assert!(l.draft.message.is_empty());
    // Esc closes the sheet outright; a second ? stays text on a non-empty
    // input.
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"\x1b", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert!(l.picker.is_none(), "esc closes the help sheet");
    rt.block_on(async {
        let _ = super::agent_launcher::launcher_keys(&mut v, b"what?", &mut sock).await;
    });
    let l = v.launcher.as_ref().unwrap();
    assert_eq!(
        l.draft.message, "what?",
        "? is text once the input holds words"
    );
    assert!(l.picker.is_none());

    // -- the runtime flags parser ------------------------------------------
    let rows = super::harness_flags::parse_help(
        "Usage: claude [options] [prompt]\n\
         \n\
         Options:\n\
         \x20 --version          Show version number\n\
         \x20 --add-dir <directories...>\n\
         \x20                    Directories the session may read\n\
         \x20 --dangerously-skip-permissions\n\
         \x20 --model, -m <model>\n\
         \x20                    Model override\n",
    );
    assert_eq!(
        rows,
        vec![
            (
                "--add-dir <directories...>".to_string(),
                "Directories the session may read".to_string()
            ),
            ("--dangerously-skip-permissions".to_string(), String::new()),
        ],
        "owned flags never suggest; beside and next-line descriptions both land",
    );
}

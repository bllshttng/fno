//! Pane frame flow: counters, emission, and the x-a600 repaint gesture - moved
//! verbatim out of server.rs (file budget shrink). Parent helpers resolve
//! through the glob.
use super::*;

/// Rig: a Core with one live `/bin/cat` pane fed `bytes` through the real
/// pty-output drain path (our own channel stands in for the pty reader
/// thread). Returns the core and the pane id. The pane is tiny (2x4) on
/// purpose: a serialized Frame scales with cells, and a full-size grid
/// overflows the 4096-byte duplex the emission tests read only AFTER the
/// write returns, deadlocking the writer.
fn counter_core(bytes: &[u8]) -> (Core, u64) {
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let pid = core.spawn_pane(2, 4, "/tmp").expect("pane");
    core.panes.get_mut(&pid).unwrap().node = Some("x-deadbeef".into());
    core.panes.get_mut(&pid).unwrap().name = Some("peer".into());
    core.panes.get_mut(&pid).unwrap().cmd = Some("claude".into());
    let (tx, mut rx) = mpsc::channel::<(u64, PaneChunk)>(8);
    tx.try_send((pid, PaneChunk::Output(bytes.to_vec())))
        .unwrap();
    drop(tx);
    let mut first_out = HashSet::new();
    drain_pty_output(&mut core, &mut rx, None, &mut first_out);
    (core, pid)
}
#[test]
fn pane_counters_count_fed_bytes_and_hidden_panes_never_composite() {
    let (core, pid) = counter_core(b"hello");
    let c = &core.panes[&pid].stats;
    assert_eq!(c.bytes_in.load(Ordering::Relaxed), 5);
    assert_eq!(c.grid_updates.load(Ordering::Relaxed), 1);
    assert!(
        c.cpu_ns.load(Ordering::Relaxed) > 0,
        "feeding one burst must attribute nonzero handling time"
    );
    // No client is attached, so broadcast_pane's visible-gate returns
    // before vt.frame(): the counter set must show fed-but-never-
    // composited, the exact reading that separates feeding from display.
    let (core, pid) = counter_core(b"data");
    let c = &core.panes[&pid].stats;
    assert_eq!(c.frames_composited.load(Ordering::Relaxed), 0);
    assert_eq!(c.bytes_in.load(Ordering::Relaxed), 4);
}
#[test]
fn redraw_pane_on_a_gone_pane_notices_fail_closed() {
    // (x-a600, AC4-ERR) A stale pane id is refused with a notice and
    // nothing is seeded: the handler must not unwrap a missing entry.
    let (mut core, pid) = counter_core(b"x");
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(8);
    let dirty: DirtyMap = Arc::default();
    let notify = Arc::new(Notify::new());
    core.clients.push(Client {
        id: 9,
        reliable_tx: tx,
        dirty: dirty.clone(),
        notify: notify.clone(),
        synced_modes: Modes::default(),
        view: (1, 1),
        visible: HashSet::from([pid]),
        dims: (24, 80),
        passive: false,
        last_press: None,
    });
    core.command(9, Command::RedrawPane { pane: Some(4242) });
    assert!(
        dirty.lock().unwrap().is_empty(),
        "a gone pane seeds nothing"
    );
    match rx.try_recv() {
        Ok(ServerMsg::Notice { text }) => {
            assert!(text.contains("4242"), "refusal named the pane: {text}")
        }
        other => panic!("expected a refusal notice, got {other:?}"),
    }
}

#[test]
fn dropped_dirty_frame_composites_but_never_emits() {
    // Two broadcasts with no writer drain: the newest-wins map keeps one
    // frame, so composited advances twice and emitted once - the gap is
    // the measurement, never "fixed" by counting the dropped frame.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (mut core, pid) = counter_core(b"x");
            core.panes.get_mut(&pid).unwrap().vt.feed(b"$ ");
            let (tx, _rx) = mpsc::channel::<ServerMsg>(8);
            let dirty: DirtyMap = Arc::default();
            let notify = Arc::new(Notify::new());
            core.clients.push(Client {
                id: 9,
                reliable_tx: tx,
                dirty: dirty.clone(),
                notify: notify.clone(),
                synced_modes: Modes::default(),
                view: (1, 1),
                visible: HashSet::from([pid]),
                dims: (24, 80),
                passive: false,
                last_press: None,
            });
            let base = core.panes[&pid]
                .stats
                .frames_composited
                .load(Ordering::Relaxed);
            core.broadcast_pane(pid);
            core.broadcast_pane(pid);
            let c = &core.panes[&pid].stats;
            assert_eq!(c.frames_composited.load(Ordering::Relaxed), base + 2);
            assert_eq!(dirty.lock().unwrap().len(), 1, "newest-wins coalescing");

            // The Bye flush is one wire path: one emitted frame.
            let (mut writer, mut reader) = tokio::io::duplex(4096);
            let stats = core.pane_stats.clone();
            write_reliable(
                &mut writer,
                &ServerMsg::Bye { reason: "d".into() },
                &dirty,
                &stats,
            )
            .await
            .unwrap();
            let _ = read_msg::<_, ServerMsg>(&mut reader).await;
            assert_eq!(c.frames_emitted.load(Ordering::Relaxed), 1);

            // A reliable Frame (the cold-attach snapshot path) emits too.
            write_reliable(
                &mut writer,
                &ServerMsg::Frame {
                    pane_id: pid,
                    frame: core.panes[&pid].vt.frame(),
                },
                &dirty,
                &stats,
            )
            .await
            .unwrap();
            let _ = read_msg::<_, ServerMsg>(&mut reader).await;
            assert_eq!(c.frames_emitted.load(Ordering::Relaxed), 2);
            // A pane with no registry row (reaped mid-drain) skips
            // silently rather than counting a phantom emit.
            count_frame_emitted(&stats, u64::MAX);
            assert_eq!(c.frames_emitted.load(Ordering::Relaxed), 2);
        });
}
#[test]
fn an_unfocused_pane_s_osc_query_resolves_its_own_ground() {
    // The query scan answers with the theme the PANE's directory
    // resolves (the server's own cwd names no project), and an unhosted
    // pane answers nothing. The write into the pane's stdin is the one
    // line after this seam; delivery is the pty writer's own contract.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
    std::fs::write(
        dir.path().join(".fno/config.toml"),
        "[mux]\ntheme = \"footnote-paper\"\n",
    )
    .unwrap();
    let dark = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dark.path().join(".fno")).unwrap();
    std::fs::write(
        dark.path().join(".fno/config.toml"),
        "[mux]\ntheme = \"footnote-superscript\"\n",
    )
    .unwrap();
    let mut core = empty_core();
    core.shells = vec!["/bin/cat".into()];
    let pid = core
        .spawn_pane(2, 4, dir.path().to_str().unwrap())
        .expect("pane");
    let query: &[u8] = b"\x1b]11;?\x1b\\";
    assert_eq!(
        core.osc_reply_for(pid, query).as_deref(),
        Some(&b"\x1b]11;rgb:f7f7/f7f7/f7f7\x1b\\"[..]),
        "the pane's own ground answers its probe"
    );
    // A pane whose directory pins the dark twin answers with its ground.
    core.panes.get_mut(&pid).unwrap().cwd = dark.path().to_str().unwrap().into();
    assert_eq!(
        core.osc_reply_for(pid, query).as_deref(),
        Some(&b"\x1b]11;rgb:1414/1414/1414\x1b\\"[..])
    );
    core.panes.remove(&pid);
    assert!(core.osc_reply_for(pid, query).is_none());
}

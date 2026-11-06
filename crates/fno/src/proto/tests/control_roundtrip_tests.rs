//! control round-trip families: moved verbatim out of proto.rs
//! (file budget shrink; the v75 RetireSession entries ride the same lists).
//! Parent helpers resolve through the glob.
use super::*;

#[test]
fn proto_v4_control_commands_roundtrip() {
    // Every control command survives the codec inside the versioned Control
    // envelope (mirrors the v3 discipline). PROTO_VERSION rides along so a
    // skew is detectable server-side.
    for command in [
        ControlCommand::PaneLs,
        ControlCommand::PaneRead {
            pane: 3,
            lines: Some(40),
            block: None,
        },
        ControlCommand::PaneRead {
            pane: 3,
            lines: None,
            block: Some(BlockSel::Last),
        },
        ControlCommand::PaneRead {
            pane: 3,
            lines: None,
            block: Some(BlockSel::Seq(7)),
        },
        ControlCommand::PaneRun {
            cwd: "/code/footnote".into(),
            argv: vec!["claude".into(), "--print".into()],
            cols: Some(120),
            rows: None,
            claim: true,
            placement: PanePlacement::default(),
            worker: None,
            fno_id: None,
        },
        ControlCommand::PaneClaim {
            pane: 5,
            holder_pid: 4242,
        },
        ControlCommand::PaneRelease { pane: 5 },
        ControlCommand::PaneSend {
            pane: 5,
            bytes: b"hello\r".to_vec(),
            guarded: true,
            expected_identity: None,
            hold_pass: true,
        },
        ControlCommand::PaneWait {
            pane: 5,
            quiet_ms: Some(200),
            pattern: Some("done".into()),
            timeout_ms: 5000,
            command_done: true,
        },
        ControlCommand::PaneKill {
            pane: 5,
            hand_off_to: None,
        },
        ControlCommand::RetireSession {
            harness: "codex".into(),
            session_id: "01a03a85-1111-7222-8333-444455556666".into(),
        },
        ControlCommand::WorkspaceOrigin {
            workspace: "footnote".into(),
            path: "/code/footnote".into(),
            remove: true,
        },
    ] {
        let msg = ClientMsg::Control {
            proto: PROTO_VERSION,
            build: BUILD_VERSION.into(),
            command,
        };
        let bytes = encode(&msg).unwrap();
        let mut cursor = std::io::Cursor::new(bytes);
        let decoded: ClientMsg = read_msg_sync(&mut cursor).unwrap();
        assert_eq!(decoded, msg);
    }
}

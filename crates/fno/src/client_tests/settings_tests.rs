//! The settings modal on its real key and tap paths: every chunk goes
//! through `handle_stdin`, and the echo checks read the composed frame.

use super::*;
use crate::client::input_field::InputField;

/// Settings open on `tab`, on a 30x100 terminal.
fn settings_on(tab: SettingsTab) -> View {
    let mut v = two_pane_view();
    v.settings_tab = tab;
    v.aux = Some(v.build_settings_modal());
    v
}

struct Keys {
    scanner: Scanner,
    carry: Vec<u8>,
    buf: Vec<u8>,
}

impl Keys {
    fn new() -> Self {
        Keys {
            scanner: Scanner::default(),
            carry: Vec::new(),
            buf: Vec::new(),
        }
    }

    async fn send(&mut self, v: &mut View, bytes: &[u8]) {
        handle_stdin(v, &mut self.scanner, &mut self.carry, bytes, &mut self.buf)
            .await
            .unwrap();
        assert!(self.buf.is_empty(), "settings keys never reach a pane");
    }

    /// A left press and release on the settings cell showing `needle`.
    async fn tap(&mut self, v: &mut View, needle: &str) {
        let (row, col) = aux_cell(v, needle);
        for end in ['M', 'm'] {
            let report = format!("\x1b[<0;{};{}{end}", col + 1, row + 1);
            self.send(v, report.as_bytes()).await;
        }
    }
}

/// The screen cell where `needle` starts inside the open aux popup.
fn aux_cell(v: &View, needle: &str) -> (u16, u16) {
    let r = v.aux.as_ref().expect("settings open").popup.render(v.term);
    r.lines
        .iter()
        .enumerate()
        .find_map(|(i, l)| {
            let at = l.text.find(needle)?;
            Some((
                (r.origin.0 + i) as u16,
                (r.origin.1 + l.text[..at].chars().count()) as u16,
            ))
        })
        .unwrap_or_else(|| panic!("{needle:?} is not on the popup"))
}

/// The composed frame, row by row.
fn frame_text(v: &mut View) -> Vec<String> {
    crate::vt::frame_text(&v.compose())
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn settings_rows() {
    // The Keys rows and the assertion both read FNO_CONFIG from the process
    // env; hold the environment lock so a parallel IsolatedConfig cannot
    // flip it between the two reads.
    let _env = crate::digest_overlay::ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let v = two_pane_view();
    let (rows, actions) = v.settings_rows_for(SettingsTab::Theme);
    // One ApplyTheme action per shipped theme, in display order.
    let names: Vec<&str> = actions
        .iter()
        .filter_map(|a| match a {
            AuxAction::ApplyTheme(n) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, crate::theme::THEME_NAMES.to_vec());
    assert!(
        rows.iter()
            .any(|r| matches!(r, PopupRow::Entry { glyph, .. } if glyph == "●")),
        "active theme is marked"
    );
    assert!(matches!(
        rows.last(),
        Some(PopupRow::Entry { label, .. }) if label == "add theme file"
    ));

    let (rows, actions) = v.settings_rows_for(SettingsTab::General);
    assert!(actions.contains(&AuxAction::ToggleHoverFocus));
    assert!(actions.contains(&AuxAction::ToggleResourceMeter));
    assert!(rows.iter().all(|row| !matches!(
        row,
        PopupRow::Entry { hint, .. } if hint == "session only"
    )));

    // The experimental tab holds one toggle per flag, paired by index.
    let (rows, actions) = v.settings_rows_for(SettingsTab::Experimental);
    let labels: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r {
            PopupRow::Entry { glyph, label, .. } if glyph == "☐" => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            "fleet org board",
            "pane share",
            "other machines · fleet map read every 15s"
        ]
    );
    assert_eq!(
        actions,
        experimental::Flag::ALL.map(AuxAction::ToggleExperimental)
    );
}

#[tokio::test]
async fn settings_tabs_switch_by_tab_and_by_tap() {
    let mut v = settings_on(SettingsTab::General);
    let mut keys = Keys::new();
    // The tab cycle walks the header order and wraps, never leaving the
    // modal: general, theme, colors, experimental, general.
    for want in [
        SettingsTab::Theme,
        SettingsTab::Colors,
        SettingsTab::Experimental,
        SettingsTab::General,
    ] {
        keys.send(&mut v, b"\t").await;
        assert_eq!(v.settings_tab, want);
        assert!(
            v.aux.is_some() && v.keys_modal.is_none(),
            "tab stays in settings"
        );
    }
    let tabs: Vec<&str> = v
        .aux
        .as_ref()
        .unwrap()
        .popup
        .chrome
        .tabs
        .iter()
        .map(|(t, _)| t.as_str())
        .collect();
    assert_eq!(tabs, ["general", "theme", "colors", "experimental"]);
    // A hover over a tab label moves no row selection.
    let sel = v.aux.as_ref().unwrap().popup.sel;
    let (row, col) = aux_cell(&v, "colors");
    keys.send(
        &mut v,
        format!("\x1b[<35;{};{}M", col + 1, row + 1).as_bytes(),
    )
    .await;
    assert_eq!(v.aux.as_ref().unwrap().popup.sel, sel);
    keys.tap(&mut v, "colors").await;
    assert_eq!(v.settings_tab, SettingsTab::Colors);
    keys.tap(&mut v, "experimental").await;
    assert_eq!(v.settings_tab, SettingsTab::Experimental);
    // A toggle here flips the flag and keeps the settings modal open on
    // the same tab, its glyph tracking the new state.
    keys.tap(&mut v, "pane share").await;
    assert!(v.experimental_pane_share);
    assert_eq!(v.settings_tab, SettingsTab::Experimental);
    assert!(v.aux.as_ref().unwrap().popup.rows.iter().any(|r| matches!(
        r,
        PopupRow::Entry { glyph, label, .. } if glyph == "☑" && label == "pane share"
    )));
    // Keybindings lives on the main menu: its row opens the which-key
    // table, the prefix line leads, and the editor button rides the table.
    v.open_sideline_menu(Anchor::Center);
    keys.tap(&mut v, "keybindings").await;
    let m = v.keys_modal.as_ref().expect("the which-key table opens");
    assert!(matches!(
        m.popup.rows.first(),
        Some(PopupRow::Entry { glyph, label, .. })
            if *glyph == crate::keys::prefix_display() && label == "prefix"
    ));
    let edit = m.edit_row.expect("the table carries an editor button");
    assert!(matches!(
        &m.popup.rows[edit],
        PopupRow::FullWidth(l) if l.starts_with("[ edit keys in ") && l.ends_with(" ]")
    ));
}
#[tokio::test]
async fn lane_custom_entry_enter_refuses_an_invalid_color_without_saving() {
    let mut v = settings_on(SettingsTab::Colors);
    let mut keys = Keys::new();
    let mut buf = Vec::new();
    let custom = AuxAction::LaneColorCustom("route".into(), "zai".into());
    execute_aux_action(&mut v, custom, &mut buf).await.unwrap();
    keys.send(&mut v, b"#12a").await;
    assert!(frame_text(&mut v)
        .iter()
        .any(|l| l.contains("route.zai: #12a ")));
    keys.send(&mut v, b"\r").await;
    assert!(
        v.notice
            .as_ref()
            .is_some_and(|(text, _)| text.contains("invalid color (name, indexed(n), #rrggbb)")),
        "the refusal names the accepted shapes: {:?}",
        v.notice
    );
    assert_eq!(
        v.lane.custom_entry.as_ref().map(InputField::text),
        Some("#12a"),
        "nothing saved; the field stays open for a fix"
    );
}

//! Every mux view paints a text hierarchy: the paint check refuses a view
//! whose body text is one style, under the dark and the light default theme.

use super::tests::{blocked_row, view_with_agents};
use super::*;
use crate::chrome::text_hierarchy;
use crate::client::overlay_paint::layout_body_overlay;
use crate::text_style::TLine;

const THEMES: [&str; 2] = ["footnote-superscript", "footnote-paper"];

fn popup_framed(popup: &Popup, term: (u16, u16)) -> chrome::Framed {
    let r = popup.render(term);
    chrome::Framed {
        width: r.width,
        lines: r
            .lines
            .iter()
            .zip(&r.body_spans)
            .map(|(l, body)| chrome::FramedLine {
                text: l.text.clone(),
                roles: l.roles.clone(),
                hits: vec![],
                body: *body,
            })
            .collect(),
    }
}

fn lines_framed(title: &str, lines: &[TLine]) -> chrome::Framed {
    let chrome = chrome::Chrome::new(title, Anchor::Center);
    layout_lines_overlay(
        (0, 0),
        (60, 120),
        &chrome,
        lines,
        None,
        OverlayAnchor::Center,
    )
    .framed
}

fn ready_connections() -> crate::connections_view::ConnectionsView {
    use crate::connections_view::{parse_accounts, parse_combos, ConnectionsView, ReadOutcome};
    let mut v = ConnectionsView::new();
    v.apply_read(ReadOutcome::Ok {
        accounts: parse_accounts(
            br#"[{"id":"ccm","harness":"claude","auth":"managed","active":true,"headroom":"ok"},
                 {"id":"ccr","harness":"claude","auth":"managed","active":false,"headroom":"low"}]"#,
        )
        .expect("accounts parse"),
        combos: parse_combos(
            br#"[{"name":"main","strategy":"fallback","members":["ccm","ccr"],"active":true}]"#,
        )
        .expect("combos parse"),
    });
    v
}

#[test]
fn every_mux_view_has_a_text_hierarchy() {
    let mut v = view_with_agents(vec![blocked_row("w", 10, None)]);
    v.term = (40, 120);
    let mut views: Vec<(String, chrome::Framed)> = Vec::new();

    for tab in crate::client::settings_modal::ALL {
        v.settings_tab = tab;
        let popup = v.build_settings_modal().popup;
        views.push((format!("settings {tab:?}"), popup_framed(&popup, v.term)));
    }
    v.open_keys_modal();
    let popup = &v.keys_modal.as_ref().expect("keys modal").popup;
    views.push(("keys".into(), popup_framed(popup, v.term)));
    v.open_sideline_menu(Anchor::Center);
    let popup = &v.aux.as_ref().expect("sideline menu").popup;
    views.push(("sideline menu".into(), popup_framed(popup, v.term)));

    let conn = ready_connections();
    let (body, _) = conn.render_body(&[], usize::MAX);
    let chrome = chrome::Chrome::new("connections", Anchor::Center);
    let framed = layout_body_overlay(
        (0, 0),
        (60, 120),
        &chrome,
        &body,
        None,
        OverlayAnchor::Center,
    )
    .framed;
    views.push(("connections".into(), framed));
    let projection = v.needs_projection();
    let needs = needs_overlay_lines(&projection, 0, v.mine_footer(), v.needs_footer());
    views.push(("needs me".into(), lines_framed("needs me", &needs)));
    let nav = NavView {
        query: String::new(),
        state_filter: None,
        cursor: 0,
    };
    let nav_lines = nav_overlay_lines(&v.nav_filtered(&nav), &nav);
    views.push(("navigator".into(), lines_framed("navigator", &nav_lines)));
    let squads: Vec<u64> = v.layout.squads.iter().map(|s| s.id).collect();
    let attach = AttachPlace {
        id: "a".into(),
        cursor: 0,
        squads: squads.clone(),
        esc: vec![],
    };
    views.push((
        "attach".into(),
        lines_framed("attach", &v.attach_place_lines(&attach)),
    ));
    let portal = PortalPick {
        remote: None,
        id: "p".into(),
        cursor: 0,
        esc: vec![],
    };
    views.push((
        "portal".into(),
        lines_framed("portal", &v.portal_pick_lines(&portal)),
    ));
    let mv = MovePick {
        src: MoveSrc::Pane(1),
        squads,
        cursor: 0,
        esc: vec![],
    };
    views.push((
        "move to".into(),
        lines_framed("move to", &v.move_pick_lines(&mv)),
    ));
    let catch_up = crate::digest_overlay::decorate(vec!["2 agents done".into()]);
    views.push(("catch up".into(), lines_framed("catch up", &catch_up)));

    for name in THEMES {
        let theme = Theme::from_name(name).0;
        for (title, framed) in &views {
            if let Err(why) = text_hierarchy(title, framed, &theme) {
                panic!("{name}: {why}");
            }
        }
    }
}

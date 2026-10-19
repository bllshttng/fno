use super::*;

use crate::proto::TemplateName;

// -- auto-reflow on pane count change ----------------------------------

/// Structural shape of a tree with every leaf's pane id collapsed to a
/// placeholder, so two trees built from different real pane ids can still
/// compare equal when their branch/axis/ratio structure matches.
fn shape_only(n: &Node) -> Node {
    match n {
        Node::Leaf(_) => Node::Leaf(0),
        Node::Branch { axis, children } => Node::Branch {
            axis: *axis,
            children: children.iter().map(|(r, c)| (*r, shape_only(c))).collect(),
        },
    }
}

#[test]
fn reflow_reshapes_four_panes_to_grid_2x2_not_a_stack() {
    let (mut core, _p1) = template_core();
    core.clients.push(client(1, 5, (24, 80), false));
    let tab_root = |core: &Core| {
        core.session
            .squad(1)
            .unwrap()
            .tabs
            .iter()
            .find(|t| t.id == 5)
            .unwrap()
            .root
            .clone()
    };
    let want = crate::templates::topology(TemplateName::Grid2x2, 4).unwrap();
    for _ in 0..3 {
        core.command(1, Command::SplitDir(Dir::Down));
        // The split reaps the test client's receiver (dropped); re-register
        // so the next split still resolves a live view.
        core.clients.push(client(1, 5, (24, 80), false));
    }
    let root = tab_root(&core);
    assert_eq!(tree::leaves(&root).len(), 4, "grew to 4 panes");
    assert_ne!(
        shape_only(&root),
        shape_only(&want),
        "a hand-drawn split keeps its stack, no reflow"
    );
    core.command(1, Command::SplitDir(Dir::Right));
    core.clients.push(client(1, 5, (24, 80), false));
    let victim = tree::leaves(&tab_root(&core))[0];
    core.pane_break(victim, None).unwrap();

    let root = tab_root(&core);
    assert_eq!(tree::leaves(&root).len(), 4, "one of 5 panes broke out");
    assert_eq!(
        shape_only(&root),
        shape_only(&want),
        "a pane count change reflows 4 panes to grid-2x2, not a stack"
    );
}

#[test]
fn reflow_shrinks_a_tab_back_to_main_left() {
    let (mut core, _p1) = template_core();
    core.clients.push(client(1, 5, (24, 80), false));
    for _ in 0..3 {
        core.command(1, Command::SplitDir(Dir::Down));
        core.clients.push(client(1, 5, (24, 80), false));
    }
    let tab = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap();
    assert_eq!(tree::leaves(&tab.root).len(), 4, "grew to 4 panes first");
    let victim = tree::leaves(&tab.root)[0];
    core.pane_break(victim, None).unwrap();

    let tab = core
        .session
        .squad(1)
        .unwrap()
        .tabs
        .iter()
        .find(|t| t.id == 5)
        .unwrap();
    assert_eq!(tree::leaves(&tab.root).len(), 3, "one pane broke out");
    let want = crate::templates::topology(TemplateName::MainLeft, 3).unwrap();
    assert_eq!(
        shape_only(&tab.root),
        shape_only(&want),
        "3 panes auto-reflow to main-left (viewport is wider than tall)"
    );
}

use super::*;
use crate::popup::{self, Anchor, Popup, PopupRow};

const URL: &str = "https://claude.ai/artifact/CH98LfTcNYUhXevAd3HFtV";
const TERM: (u16, u16) = (24, 40);

/// Paint `popup` through the real draw path into a fresh frame, the way the
/// client does every tick, and hand back the cells.
fn paint(popup: &Popup) -> Vec<Cell> {
    let (rows, cols) = (TERM.0 as usize, TERM.1 as usize);
    let mut cells = vec![Cell::default(); rows * cols];
    frame_begin();
    popup::draw(
        &mut cells,
        rows,
        cols,
        &popup.render(TERM),
        &Theme::default_theme(),
    );
    frame_end();
    cells
}

fn info_popup(value: &str) -> Popup {
    Popup::new(
        vec![PopupRow::Info {
            label: "why asked".into(),
            value: value.into(),
        }],
        Anchor::Center,
    )
    .title("question")
    .plain_body()
}

/// Whether a selection is on screen.
fn active() -> bool {
    STATE.with(|s| s.borrow().sel.is_some())
}

/// The first and last body cells the last frame painted, and the row count.
fn bounds() -> ((usize, usize), (usize, usize), usize) {
    STATE.with(|s| {
        let s = s.borrow();
        let block = s.last.last().expect("the frame painted a block");
        let first = block.lines.first().expect("the block has a body row");
        let last = block.lines.last().expect("the block has a body row");
        (
            (first.row, first.lo),
            (last.row, last.hi - 1),
            block.lines.len(),
        )
    })
}

/// Drag from the first body cell to the last and release.
fn drag_everything() -> Release {
    let (from, to, _) = bounds();
    assert_eq!(press(from.0, from.1, Instant::now()), Press::Armed);
    assert!(drag(to.0, to.1));
    release()
}

#[test]
fn a_wrapped_value_copies_as_one_logical_line() {
    // A URL has no space to wrap at, so the layout splits the word: the copy
    // joins the pieces with nothing between, border glyphs left out.
    paint(&info_popup(URL));
    assert!(bounds().2 >= 2, "the URL wraps across body rows");
    match drag_everything() {
        Release::Copy(text) => {
            assert!(text.ends_with(URL), "the URL pastes whole: {text:?}");
            assert!(!text.contains('\n'), "a wrap is no line break: {text:?}");
            assert!(!text.contains('\u{2502}'), "no border glyphs: {text:?}");
        }
        other => panic!("a drag copies, got {other:?}"),
    }
    // Prose wraps at spaces, so each wrap stands for one space.
    let prose = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
    paint(&info_popup(prose));
    assert!(bounds().2 >= 2, "the prose wraps across body rows");
    match drag_everything() {
        Release::Copy(text) => assert!(text.ends_with(prose), "one space per wrap: {text:?}"),
        other => panic!("a drag copies, got {other:?}"),
    }
}

#[test]
fn a_press_is_a_click_until_it_moves_and_a_double_click_takes_the_word() {
    paint(&info_popup(URL));
    let (from, _, _) = bounds();
    let t = Instant::now();

    // A press off the body (the border cell) is not a selection at all.
    assert_eq!(press(from.0, from.1 - 1, t), Press::Pass);
    assert!(!active());

    // A press that never moves replays as the click it was.
    let at = (from.0, from.1 + 15);
    assert_eq!(press(at.0, at.1, t), Press::Armed);
    assert_eq!(release(), Release::Click(at.0, at.1));
    assert!(!active(), "a click leaves no selection behind");

    // The URL starts 13 cells in: two pad cells, the leading space, the
    // label's nine and the gap. A second press on the cell inside its first
    // row takes the whole URL, across the wrap.
    let second = press(at.0, at.1, t + Duration::from_millis(100));
    assert_eq!(second, Press::Word(URL.to_string()));
    // A drag after it reshapes the selection, so the release copies again.
    assert!(drag(at.0, at.1 + 2));
    assert!(matches!(release(), Release::Copy(t) if !t.is_empty()));
}

#[test]
fn a_selection_paints_with_the_theme_sel_surface_and_drops_with_its_text() {
    let popup = info_popup(URL);
    paint(&popup);
    drag_everything();
    let cells = paint(&popup);
    let cols = TERM.1 as usize;
    let (from, _, _) = bounds();
    let (_, sel_bg, _) = cell_style(Role::BodySel, &Theme::default_theme());
    assert_eq!(
        cells[from.0 * cols + from.1].bg,
        sel_bg,
        "body cell selected"
    );
    assert_ne!(
        cells[from.0 * cols + from.1 - 1].bg,
        sel_bg,
        "the border is never part of a selection"
    );
    assert!(active());
    paint(&info_popup("something else entirely"));
    assert!(!active(), "the cells no longer name what was selected");
}

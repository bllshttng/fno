# Mux modal text selection

A mux modal lets you drag the mouse over its text. The release copies the selection. A double-click takes the word under the pointer.

## How it works

The painters report each body row they draw. `chrome::blit` and `popup::draw` call `modal_select::paint_line`. The recorder keeps the rows of the last frame as blocks of cells. It skips the border, the title and the footer, so a selection never holds them.

The selection paints with the theme `sel` surface from the same call. When the text under a selection changes, the selection drops. A scroll or a refresh changes it.

The copy goes through `clipboard::deliver`. A local tool takes it first. Over ssh the client sends OSC 52 to your terminal.

## Wrapped text

A wrap is a layout choice, not a line break. Each row carries a `chrome::Join`.

- `Line`: a new logical line.
- `Space`: the row continues the one above, and the wrap stood at a space.
- `Seam`: the row continues the one above, and the wrap split a word.

A copy joins `Space` rows with one space and `Seam` rows with nothing. A long URL pastes whole. The wrap sites set the join. These are `wrap_line_joined`, `BLine::wrap` and the popup `wrap_rows`.

## Clicks still work

A press inside a body arms a selection. The client holds the press back. A release on the same cell replays the press as a click, and the row action runs as before. A release elsewhere copies the drag.

`region_focus::text_modal_open` lists the modals that take part: the event provenance modal, the question page and the Messages session details. A new text modal joins by adding itself to that list.

## The event provenance modal

Every recorded field is a cursor target. The arrow keys reach each row, and `y` copies the value of the row under the cursor. Enter acts only on rows with an action (`node`, `session-id`, `pane`, `resume`).

## Screenshots

`fno mux serve --snapshot --view feed-detail` and `--view question-detail` paint a selection across the longest line. The `bell-shots` CI job shoots both.

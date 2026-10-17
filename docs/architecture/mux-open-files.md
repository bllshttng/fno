# How the mux opens a file or link

The mux shows agent output in panes. Agents print paths to screenshots, PDFs, plans and source files. This page says how a click on such a path or link opens it: inside the mux in a viewer, or in an outside app. It records one design and the order of the slices that build it. Each slice section names the code it starts from, by file and line, as read on main on 2026-10-12.

## Is this page for you?

For: a worker who builds or reviews a slice of the mux file viewer, the open router, or the image layer. Not for: clicks on fno's own pseudo links (`@handle`, mail ids, a sender). `crates/fno/src/client/open_link.rs` routes those. Not for: the keys a user can rebind. `docs/mux-keys.md` covers those.

## 1. Links and paths in pane text

This part has shipped. OSC 8 hyperlinks win over text matching in `Pane::link_span` (`crates/fno/src/vt.rs:497`). Plain `http(s)` URLs come from `find_urls` (`crates/fno/src/link.rs:76`). Bare paths come from `find_paths` (`crates/fno/src/link.rs:230`). The matcher resolves each path candidate against the pane cwd. If the path does not exist on disk, it gets no link. So a prose word dies at the stat and not at the scan. A `path:line` tail keeps the line.

A path becomes an internal `fno-file:` URI (`FILE_SCHEME`, `crates/fno/src/link.rs:177`) that carries the canonical absolute path. `file_uri_parts` (`crates/fno/src/link.rs:183`) parses it strictly: absolute, with no whitespace or control characters. The server sends only a URI that passes `is_openable` or `is_file_uri` (`crates/fno/src/server/open_link.rs:25`). `OPENABLE_SCHEMES` (`crates/fno/src/link.rs:24`) stays at `http://` and `https://`, so a pane can never make the platform opener run a scheme fno did not build.

The client routes every clicked URI in `route` (`crates/fno/src/client/open_link.rs:29`). Today `Route::File` calls `open_fno_path` and drops the line (`crates/fno/src/client/open_link.rs:83`). The open router in section 6 replaces that one call. Nothing else in detection changes.

## 2. The viewer

The viewer is a client surface. The server never sees it, and it never becomes a pane. It paints into the client `Frame` the way the other overlays do, so two attached clients can each show a different file.

`mux.viewer` picks the shape: `panel` (the default) or `modal`.

- `panel` docks at the right edge and takes its columns from the panes, as the feed panel does. The width is a percent of the terminal (40 by default). `View::viewer_panel_w` in `crates/fno/src/client/file_viewer.rs` reads it, and `content_dims` subtracts it, so the server resizes the panes to fit. The width lasts for the client session and is not saved. The viewer and the feed share the right edge. While the viewer holds it, `feed_panel_w` reads 0. Closing the viewer gives the edge back to the feed with its state intact. A terminal too narrow for a 24-column panel shows the viewer as a modal.
- `modal` is a centered box, 80% of the content area in each direction. Both shapes paint one framed block through `chrome::frame`. The esc chip, the footer's `esc close` words and the scroll bar come from that shared chrome.

The viewer opens focused, because a click asked for it. A modal always holds the keyboard. A click on a pane takes the keyboard from a panel. A click inside the panel gives it back. While it has focus it takes these keys, with no prefix:

| Key | Action |
|---|---|
| Esc, `q` | close |
| `-`, `+` (or `=`) | narrow or widen the panel by 5% of the terminal width, between 20% and 80% |
| `j`, `k`, arrows, mouse wheel | scroll |
| space, `b`, PgDn, PgUp | scroll a page |
| `g`, `G` | jump to the top or the end |
| `o` | hand the file to the platform opener |
| `n`, `p` | next or previous page (PDF only, section 4) |

Prefix+`<` and prefix+`>` were the first pick for resize. Those keys already move a tab (`move-tab-left` and `move-tab-right`, `crates/fno/src/keys.rs:1207` and `:1214`). Keys local to a focused viewer avoid the clash and need no new prefix action. The close chip works with no extra code: the painter records it, and `esc_close::tap` (`crates/fno/src/client/esc_close.rs`) presses Esc through the normal key chain. An unfocused panel takes no keys, so a tap on its chip closes it directly.

The first viewer slice shows text only. It reads the file off the UI loop, the way `open_link::start` already uses `spawn_blocking`. It caps the read at 1 MiB and says so in the header. It numbers each line and wraps a long line without losing its indent. If the URI carries a `:line`, the viewer scrolls to that line and marks it. It replaces every control character with a dot, so file bytes never reach the terminal as an escape sequence. For a binary file it shows "no preview" with the `o` hint.

The viewer code is in `crates/fno/src/client/file_viewer.rs`. `crates/fno/src/client.rs` is shrink-only under `scripts/ci/check-file-budget.sh`. The `link_rx` arm that used to build the notice inline now calls `open_link::land`, which paid for the few lines the viewer adds there.

## 3. Images

Use `ratatui-image` (11.1.0, MIT) with default features off. It encodes the kitty graphics protocol, iTerm2 inline images and sixel, and it has a half-block fallback. Its `ratatui ^0.30.1` dependency matches the split crates this crate already uses (`ratatui-core` 0.1 and `ratatui-widgets` 0.3, `crates/fno/Cargo.toml:40`). Its MSRV is 1.86. Default features off keeps out its `crossterm` feature and the `chafa-dyn` library. Enable only the `image` decoders the viewer needs (png, jpeg, gif, webp).

A frame cell holds one `char` (`Cell` in `crates/fno/src/proto.rs`). Kitty needs a placeholder char plus combining marks in every cell. iTerm2 and sixel put kilobytes of escape bytes in one cell. None of that fits, so those three draw as a second layer. `crates/fno/src/client/image_layer.rs` owns it:

1. `image_layer::render` draws the `ratatui-image` widget into a scratch `Buffer` the size of the body rect. For kitty, iTerm2 and sixel it turns that buffer into terminal bytes positioned at the rect. The viewer caches the result per rect, so a frame at the same size never encodes again.
2. The viewer fills its rect with `HOLE` cells (a Unicode noncharacter) and stages one placement for the frame.
3. `Compositor::draw_to` (`crates/fno/src/client/compositor.rs`) does its row diff. A row that crosses an intact rect jumps the cursor past it, so text never overwrites the image. A `HOLE` cell prints as a space and never reaches the terminal.
4. After the rows, the compositor blanks the rect and writes the bytes. It writes them once, and again only after a full redraw or a new placement.
5. When the viewer closes, or a later layer paints over any cell of the rect, the image retires. Every row it covered redraws, even rows whose cells did not change. Kitty also gets a delete (`a=d,d=A`), because the mux client places no other images.

Half-blocks are plain `▀` cells with a foreground and a background color. They blit into the frame through `ratatui_blit` and need no layer, so the snapshot renderer shows them too.

The client picks the protocol once, in `stdin_reader::spawn`, before the reader thread owns stdin. The crate's query reads the terminal's reply from stdin, so a query after that point would hand the reply to a pane. The query keeps the crate's 2 s bound. It ends on a status reply every terminal sends, so the bound costs nothing on a terminal that answers. On a timeout the crate's query thread stays blocked on stdin, then restores cooked mode, so a short bound is unsafe. With no reply, the crate falls back to env hints for iTerm2 and WezTerm, then half-blocks. `FNO_MUX_IMAGE_PROTOCOL` forces a protocol. With no tty, the client uses half-blocks and sends no query.

The viewer reads PNG, JPEG, GIF and WebP by their first bytes, off the UI loop. It refuses a file over 32 MiB and an image over 8192 px on a side. It keeps a copy at most 2048 px on its long edge, so encode cost stays bounded. A GIF shows its first frame.

## 4. PDF

Use `hayro` (0.8.0, Apache-2.0 OR MIT). It is pure Rust and needs no pdfium library. `pdfium-render` was rejected because it needs a pdfium library shipped beside the binary.

The viewer renders the current page off the UI loop to a pixmap and shows it through the image path in section 3. `n` and `p` change the page. The header shows the page number and the page count. The viewer keeps only the current page in memory.

`hayro` declares MSRV 1.92. This crate declares `rust-version = "1.89"` (`crates/fno/Cargo.toml:11`). CI builds on stable, so CI stays green. But the PDF slice must raise `rust-version` to 1.92 in the same PR. Then a user on an older compiler gets a clear refusal. The PR body also records how much the release binary grows.

On a terminal with only half-blocks, page text is hard to read. The page still shows its layout, and `o` hands the file to the outside opener.

## 5. Markdown

Markdown rendering comes later, as the user asked. Until then a `.md` file shows as plain text in the viewer. With `mux.open_with = obsidian`, it opens in Obsidian.

The slice will be small. `md_lines` (`crates/fno/src/client/backlog_md.rs:7`) already renders markdown to styled lines for the backlog detail pane, with no new dependency. The markdown slice feeds the viewer through it and adds no parser.

## 6. Outside handoff and the open router

`mux.open_with` picks where a clicked file goes: `inmux` (the default), `obsidian`, or `system`.

- `inmux` sends text, markdown, images and PDFs to the viewer. Other types go to `system`. Today the viewer shows text, source files, and PNG, JPEG, GIF and WebP images. Other image formats, PDFs, archives, audio, video and office documents, and any directory, still go to `system` (`OUTSIDE_EXTS` and `viewable` in `crates/fno/src/open_router.rs`). Each of those joins the viewer with its own slice. A file with no telling extension goes to the viewer, which says "no preview" for a binary.
- `obsidian` covers files under the vault root. For such a file the router builds an `obsidian://open` URI. It uses the existing builder `obsidian_open_uri` (`crates/fno/src/link.rs:690`) and the root that `resolve_vault_root` gives (`crates/fno/src/link.rs:681`). Move the vault-relative step out of `plan_link` (`crates/fno/src/link.rs:625`) into one shared function. Then `plan_link` and the router call the same code. The URI opens through `open_fno_uri` (`crates/fno/src/link.rs:719`). A file outside the vault, or `obsidian.enabled = false` (`ObsidianCfg`, `crates/fno/src/digest_overlay.rs:1202`), falls back to `system`.
- `system` calls `open_fno_path` (`crates/fno/src/link.rs:739`), the behavior today.

The router is the one function that `Route::File` calls in `open_link::start`. It is pure: it takes the path, the line and the config and returns a target. Tests assert the target, and only the caller spawns. The client reads both keys with `config_str` (`crates/fno/src/digest_overlay.rs:962`) and latches them at client start, like `mux.card_graph`.

Both keys need two Python edits. One is a field on `MuxBlock` (`cli/src/fno/config/__init__.py:3633`), the schema that `fno config set` resolves against. The other is a row in `cli/src/fno/config/registry.py`, the presentation metadata. Without them, each config load warns about an unknown key, and `fno config set` refuses both keys. The user granted both edits, so the router ships with the keys that `fno config set` can set. The router lives in `crates/fno/src/open_router.rs`, and `link::vault_file_uri` is the shared vault-relative step. The client reads `mux.open_with` and `mux.viewer` on each click, on the blocking thread that loads the file.

## 7. Slices, in order

1. Clickable file paths in pane text. Shipped (section 1).
2. The open router with `mux.open_with` and `mux.viewer`, and obsidian URIs for vault files (section 6). It paints nothing, so it needs no screenshots. Shipped.
3. The viewer surface: panel or modal, the keys, and the text preview (section 2). Shipped.
4. Images through `ratatui-image`, drawn as a layer after the cell frame (section 3).
5. PDF pages through `hayro` and the image path (section 4).
6. Markdown in the viewer through `md_lines` (section 5). It can start once slice 3 lands, and it does not wait for slices 4 or 5.

Each slice is one PR. Every slice that paints puts before and after screenshots, dark and light, in the internal `fno/mux` folder and links them in the PR body.

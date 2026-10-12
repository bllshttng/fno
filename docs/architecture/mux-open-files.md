# How the mux opens a file or link

The mux shows agent output in panes. Agents print paths to screenshots, PDFs, plans and source files. This page says how a click on such a path or link opens it: inside the mux in a viewer, or in an outside app. It records one design and the order of the slices that build it. Each slice section names the code it starts from, by file and line, as read on main on 2026-10-12.

## Is this page for you?

For: a worker who builds or reviews a slice of the mux file viewer, the open router, or the image layer. Not for: clicks on fno's own pseudo links (`@handle`, mail ids, a sender). `crates/fno/src/client/open_link.rs` routes those. Not for: the keys a user can rebind. `docs/mux-keys.md` covers those.

## 1. Links and paths in pane text

This part has shipped. OSC 8 hyperlinks win over text matching in `Pane::link_span` (`crates/fno/src/vt.rs:497`). Plain `http(s)` URLs come from `find_urls` (`crates/fno/src/link.rs:76`). Bare paths come from `find_paths` (`crates/fno/src/link.rs:230`). The matcher resolves each path candidate against the pane cwd. If the path does not exist on disk, it gets no link. So a prose word dies at the stat and not at the scan. A `path:line` tail keeps the line.

A path becomes an internal `fno-file:` URI (`FILE_SCHEME`, `crates/fno/src/link.rs:177`) that carries the canonical absolute path. `file_uri_parts` (`crates/fno/src/link.rs:183`) parses it strictly: absolute, with no whitespace or control characters. The server sends only a URI that passes `is_openable` or `is_file_uri` (`crates/fno/src/server/open_link.rs:25`). `OPENABLE_SCHEMES` (`crates/fno/src/link.rs:24`) stays at `http://` and `https://`, so a pane can never make the platform opener run a scheme fno did not build.

The client routes every clicked URI in `route` (`crates/fno/src/client/open_link.rs:29`). Today `Route::File` calls `open_fno_path` and drops the line (`crates/fno/src/client/open_link.rs:70`). The open router in section 6 replaces that one call. Nothing else in detection changes.

## 2. The viewer

The viewer is a client surface. The server never sees it, and it never becomes a pane. It paints into the client `Frame` the way the other overlays do, so two attached clients can each show a different file.

`mux.viewer` picks the shape: `panel` (the default) or `modal`.

- `panel` docks at the right edge and copies the feed panel. The width is in columns, and the border drags. The width persists through `view_store`. See `feed_panel_w` and the drag functions (`crates/fno/src/client/feed_view.rs:724` to `:797`). The viewer and the feed share the right edge. The one opened last owns it. If the feed was open, closing the viewer gives the edge back to the feed.
- `modal` is a centered family-B overlay (`OverlayAnchor::Center`, `crates/fno/src/client/overlay_paint.rs`).

The viewer opens focused, because a click asked for it. While it has focus it takes these keys, with no prefix:

| Key | Action |
|---|---|
| Esc, `q` | close |
| `-`, `+` (or `=`) | narrow or widen the panel by 5% of the terminal width |
| arrows, PgUp, PgDn | scroll |
| `o` | hand the file to the outside opener (section 6) |
| `n`, `p` | next or previous page (PDF only, section 4) |

Prefix+`<` and prefix+`>` were the first pick for resize. Those keys already move a tab (`move-tab-left` and `move-tab-right`, `crates/fno/src/keys.rs:1207` and `:1214`). Keys local to a focused viewer avoid the clash and need no new prefix action. The close chip works with no extra code: the painter records it, and `esc_close::tap` (`crates/fno/src/client/esc_close.rs`) presses Esc through the normal key chain.

The first viewer slice shows text only. It reads the file off the UI loop, the way `open_link::start` already uses `spawn_blocking`. It caps the read at 1 MiB. If the URI carries a `:line`, the viewer scrolls to that line. For a binary file it shows "no preview" with the `o` hint.

The viewer code goes in a new `crates/fno/src/client/file_viewer.rs`. `crates/fno/src/client.rs` is 10,489 lines and shrink-only under `scripts/ci/check-file-budget.sh`, so each line the viewer adds there must move another line out in the same PR.

## 3. Images

Use `ratatui-image` (11.1.0, MIT) with default features off. It encodes the kitty graphics protocol, iTerm2 inline images and sixel, and it has a half-block fallback. Its `ratatui ^0.30.1` dependency matches the split crates this crate already uses (`ratatui-core` 0.1 and `ratatui-widgets` 0.3, `crates/fno/Cargo.toml:40`). Its MSRV is 1.86. Default features off keeps out its `crossterm` feature and the `chafa-dyn` library. Enable only the `image` decoders the viewer needs (png, jpeg, gif, webp).

A frame cell holds one `char` (`Cell`, `crates/fno/src/proto.rs:2261`). Protocol bytes cannot ride in a cell. So the image draws as a second layer:

1. The viewer paints its image rect as blank cells.
2. `Compositor::draw` (`crates/fno/src/client/compositor.rs:42`) does its row diff as today.
3. After the diff, the compositor moves the cursor to the rect origin and writes the encoded image bytes. `draw_row` writes a changed row whole, from column 0 (`crates/fno/src/client/compositor.rs:80`). Its blank cells erase a sixel or iTerm2 image on that row. So the compositor writes the image again after any frame that wrote a row inside the rect. A changed image or rect also gets a new write. A kitty image sits on its own layer, so a row write does not erase it.
4. When the viewer closes, or any overlay covers part of the rect, the compositor removes the image. Kitty needs an explicit delete command. Text that overwrites the cells clears a sixel or iTerm2 image. So the compositor marks those rows dirty for one full redraw. The spike confirms this in both terminals.

The client picks the protocol once at start, before the input loop owns stdin, because the probe reads the terminal's reply. If the probe gets no reply, an env check (`TERM_PROGRAM`, `KITTY_WINDOW_ID` and the like) is the fallback. Half-blocks come last, and they paint as ordinary cells, so they need no second layer. The client decodes and scales the image off the UI loop.

The slice starts with a spike in kitty and in iTerm2. The spike answers two questions. First: can the encoded bytes come out of `ratatui-image` without its `Buffer` render path? If not, the slice renders into a scratch `Buffer` and copies the escape strings out. Second: does `cargo tree -d` show one `crossterm` and no `chafa`? The PR body records both answers.

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

- `inmux` sends text, markdown, images and PDFs to the viewer. Other types go to `system`. Until the viewer ships, `inmux` acts as `system`, so the default changes nothing on the day the router lands.
- `obsidian` covers files under the vault root. For such a file the router builds an `obsidian://open` URI. It uses the existing builder `obsidian_open_uri` (`crates/fno/src/link.rs:690`) and the root that `resolve_vault_root` gives (`crates/fno/src/link.rs:681`). Move the vault-relative step out of `plan_link` (`crates/fno/src/link.rs:625`) into one shared function. Then `plan_link` and the router call the same code. The URI opens through `open_fno_uri` (`crates/fno/src/link.rs:719`). A file outside the vault, or `obsidian.enabled = false` (`ObsidianCfg`, `crates/fno/src/digest_overlay.rs:1202`), falls back to `system`.
- `system` calls `open_fno_path` (`crates/fno/src/link.rs:739`), the behavior today.

The router is the one function that `Route::File` calls in `open_link::start`. It is pure: it takes the path, the line and the config and returns a target. Tests assert the target, and only the caller spawns. The client reads both keys with `config_str` (`crates/fno/src/digest_overlay.rs:962`) and latches them at client start, like `mux.card_graph`.

Both keys need two Python edits. One is a field on `MuxBlock` (`cli/src/fno/config/__init__.py:3633`), the schema that `fno config set` resolves against. The other is a row in `cli/src/fno/config/registry.py`, the presentation metadata. Without them, each config load warns about an unknown key, and `fno config set` refuses both keys. A standing ruling bars new Python, so these edits wait for the user's grant. The router slice waits for that grant too. It does not ship keys that the supported config path cannot set.

## 7. Slices, in order

1. Clickable file paths in pane text. Shipped (section 1).
2. The open router with `mux.open_with` and `mux.viewer`, and obsidian URIs for vault files (section 6). It paints nothing, so it needs no screenshots. It starts after the user grants the two Python edits.
3. The viewer surface: panel or modal, the keys, and the text preview (section 2).
4. Images through `ratatui-image`, drawn as a layer after the cell frame (section 3). It starts with the spike.
5. PDF pages through `hayro` and the image path (section 4).
6. Markdown in the viewer through `md_lines` (section 5). It can start once slice 3 lands, and it does not wait for slices 4 or 5.

Each slice is one PR. Every slice that paints puts before and after screenshots, dark and light, in the internal `fno/mux` folder and links them in the PR body.

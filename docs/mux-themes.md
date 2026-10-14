# Mux themes

Mux themes set the colors for the fno interface. The terminal keeps its own colors for the text inside the interface.

## Theme roles

| Role | Use |
|---|---|
| `base` | The theme background, also called the ground. |
| `stamp` | The label color in the `[no]` stamp. |
| `brand` | Selection, the active tab, the focused pane label, and accent text. |
| `needs_you` | A question or block that waits for you. |
| `border` | Pane frames, modal borders, and popover borders. It defaults to `brand`. |
| `title` | Titles in the interface. |
| `sel` | The selected row highlight. |
| `dim` | Secondary interface text. |
| `chip` | The Escape key hint. |

Use `#rrggbb`, `indexed(n)`, or an ANSI-16 name for each role. Examples are `#1e1e2e`, `indexed(4)`, `blue`, and `light_yellow`.

`inherit` names a built-in theme. The built-in `terminal` theme follows the terminal's own colors. Other built-ins provide a palette for any role that the user theme does not set.

## Matching Claude Code themes

Each brand mux theme has a Claude Code theme with the same name. The plugin ships both in `themes/`. Run `/theme` in Claude Code and look for "Footnote Paper from fno" and "Footnote Superscript from fno".

| Mux theme | Claude Code theme | Ground and text | Your message band |
|---|---|---|---|
| `footnote-superscript` | `custom:fno:footnote-superscript` | Dark ground, light text | White band, near-black text |
| `footnote-paper` | `custom:fno:footnote-paper` | Light ground, dark text | `#373737` band, near-white text |

A Claude Code pane that the mux starts picks the matching theme for you. It passes the theme with `--settings` for that session only, and it never edits your `settings.json`. A pane that already names a settings file keeps that file. A mux theme with no brand match (`terminal`, `catppuccin`, and so on) leaves Claude Code on its own theme.

Claude Code 2.1.296 lets a theme set the band background, but not the band text. The text keeps the base theme color, so the band reads dark on dark (Paper) or white on white (Superscript). The mux fixes this. It repaints the text of cells whose background is exactly a shipped band color, and it leaves code blocks, diffs, and every other cell alone. The repaint reaches every pane, not only Claude Code panes. Another program that paints one of those exact backgrounds keeps readable text, but loses its own text colors on those cells. The browser viewer (`fno mux serve --web`) applies the same repaint. The `terminal` theme skips it in the terminal client. Outside the mux, neither band has readable text, so use these two themes inside the mux.

A light ground needs a white terminal background. The mux does not paint pane bodies, so the terminal decides. On a light terminal the mux picks Paper and leaves the terminal background alone. In Ghostty, set `background = f7f7f7` and `foreground = 141414` to match Paper exactly. Set `mux.theme = "footnote-paper"` to have the mux paint that ground itself.

## Save a theme in config.toml

Add a `[mux.themes.<name>]` table to a config file:

```toml
[mux.themes.midnight]
inherit = "footnote-superscript"
base = "#1e1e2e"
stamp = "#cdd6f4"
brand = "#89b4fa"
needs_you = "#f9e2af"
```

The mux combines theme values by role. The imported theme folder has the lowest priority, then the global config, then the project config. A higher layer replaces only the roles it sets. Built-in names always win, so a user theme cannot replace one.

The `base` role paints the ground and sets the terminal background. A non-RGB `base` value does not paint a ground. `inherit = "terminal"` also paints no ground. In these cases, applying the theme restores the terminal's own background.

The settings values `mux.theme.brand`, `mux.theme.needs_you`, and `mux.theme.border` override those roles after the theme is loaded.

## Import a theme from Settings

Open **Settings > Theme** and choose **add theme file**. Enter a local file path, a folder path, or a public GitHub theme file URL. A relative path uses the mux working directory. A path that starts with `~/` uses your home directory. You can also drop a file onto the field. On a local macOS session, **choose file…** opens the system file dialog, and so does Enter on an empty field. Esc steps back one level.

The importer reads fno theme files with `[mux.themes.<name>]` tables or top-level role keys. It also reads Ghostty theme files. It previews the result before it saves the theme to `<state root>/mux/themes/<name>.toml` and applies it.

A file must be a regular file, no larger than 65,536 bytes, and valid UTF-8. A folder import reads regular, non-hidden files directly inside that folder. It does not scan subfolders. It accepts up to 32 files. It skips files that are not theme files. If a file has an invalid color, role, or `inherit` value, the preview shows the warning and disables save.

GitHub imports accept a file URL in one of these forms:

```text
https://github.com/<owner>/<repo>/blob/<branch>/<path>
https://github.com/<owner>/<repo>/raw/<branch>/<path>
https://raw.githubusercontent.com/<owner>/<repo>/<branch>/<path>
```

The importer fetches over HTTPS only. It accepts public files on `github.com` and `raw.githubusercontent.com`. Do not include a query, credentials, or an explicit port. A repository root or `/tree/` URL does not identify a file. Paste the file's `/blob/<branch>/<path>` URL instead. The importer does not run code from theme files or follow `config-file` entries.

If the imported name matches a built-in or saved theme, the importer adds a numeric suffix, such as `midnight-2`. It does not replace an existing theme file.

## Ghostty color mapping

Ghostty theme files use `key = value` lines. The importer reads the following keys and ignores other keys, including `config-file`.

| fno role | Ghostty value |
|---|---|
| `base` | `background` |
| `stamp`, `title` | `foreground` |
| `brand`, `border` | `palette = 4=<color>` |
| `needs_you` | `palette = 3=<color>` |
| `chip` | `palette = 1=<color>` |
| `sel` | `selection-background`, or `palette = 0=<color>` when absent |
| `dim` | `palette = 8=<color>` |
| `inherit` | `footnote-paper` when the background luminance is above 0.5; otherwise `footnote-superscript` |

The importer accepts six hex digits with or without a leading `#`. When its Ghostty source value is missing, the importer leaves that role unset. The selected built-in supplies it.

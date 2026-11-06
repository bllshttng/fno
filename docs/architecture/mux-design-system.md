# The mux design system

This page is the rulebook for how the mux looks. The `design-critic` skill judges paint PRs against it, and a worker reads it before a paint change. Every rule names the file and the symbol that paints it, so a reader can check the rule against the code. The rules come from the shipped code first. Some rules come from outside design guides, cut down to what a terminal can do. Those rules name their source.

## How to read this page

The mux paints a grid of cells in the emulator's palette. A cell has a character, a foreground, a background and flags. So the design levers are few: bold, italic, inverse, palette indexes, the theme's `brand` color, box borders, blank rows and the words. The cell type is `Cell` in `crates/fno/src/proto.rs`, and the flags are `cell_flags` in the same file.

Each `###` heading below is one rule. The critic names a broken rule by its exact heading text. A `##` heading groups rules. When a finding fits no `###` rule, the critic names the `##` group.

A rule that a shot cannot show (a key binding, a hidden state) goes to the critic's `left` list, never to `findings`.

## Text hierarchy

A view picks a level for each piece of text. The theme picks how that level looks. The level type is `TextStyle` in `crates/fno/src/text_style.rs`, and `TextStyle::resolve` is the whole style policy. A view never sets a color or a flag on text by hand.

| Level | Foreground | Flags | Use |
|---|---|---|---|
| `TextStyle::Title` | default fg | bold | A view or node title. |
| `TextStyle::Section` | theme `brand` | bold | A group or section heading. |
| `TextStyle::Item` | default fg | none | Ordinary row text. |
| `TextStyle::Value` | default fg | bold | The value of a field or toggle. |
| `TextStyle::Meta` | palette index 8 | none | Counts, ages, ids and other secondary facts. |
| `TextStyle::Hint` | palette index 8 | italic | Footers, key hints and notices. |
| `TextStyle::Badge` | palette index 3 | none | A handle, account or bracketed token. |
| `TextStyle::Tab` | default fg | inverse | The active tab cell of a tab strip. |
| `TextStyle::Status(Status::Ok)` | palette index 2 | none | A state token that reads good. |
| `TextStyle::Status(Status::Warn)` | palette index 3 | none | A state token that needs a look. |
| `TextStyle::Status(Status::Error)` | palette index 1 | none | A state token that failed. |
| `TextStyle::Added` | palette index 2 | none | A diff's added line. |
| `TextStyle::Removed` | palette index 1 | none | A diff's removed line. |

All levels are defined in `crates/fno/src/text_style.rs`. The shortcuts `TextStyle::OK`, `TextStyle::WARN` and `TextStyle::ERROR` name the three `Status` levels.

Meta and Hint use palette index 8, not the DIM flag. DIM washes the default fg out on a light terminal. Index 8 stays a readable gray on both grounds. The doc comment on `TextStyle::resolve` records this.

Older code names the same looks as theme roles. `Role::PanelHead` is a Title, `Role::PanelMeta` and `Role::PanelRule` are Meta, `Role::PanelLabel` and `Role::PanelPill` are a Badge, `Role::PanelWarn` is an Error and `Role::BodyAccent` is a Section. The text levels replace these roles through `Role::Text` in `crates/fno/src/theme.rs`. A shot of either code reads the same.

### One level per piece of text

Each piece of text wears exactly one level from the table. A heading is a Section, a fact the reader compares is a Value, and a secondary fact is Meta. A view that makes up a new mix of color and flags breaks this rule. Code: `TextStyle::resolve` in `crates/fno/src/text_style.rs`. The `type_scale` module in `crates/fno/src/theme.rs` maps body meanings (`HEADING`, `NAME`, `STRONG`, `BODY`, `META`) onto the same levels.

### A view never paints one style

A view whose body text paints in one style has no hierarchy. A view with four or more text lines and no bold level has nothing to scan by. Code: `chrome::text_hierarchy` in `crates/fno/src/chrome.rs` refuses both shapes in tests. The critic applies the same test to a shot.

### Bold has one job

Bold marks a Title, a Section or a Value. Bold on ordinary row text or on a hint competes with the heading. Two bold leads of equal weight in one view are a high finding. Code: only `Title`, `Value` and `Section` carry `cell_flags::BOLD` in `TextStyle::resolve`. Source: taste-skill `references/app-screens.md` section 9.1 (one weight does one job per screen).

### The key fact is never Meta or Hint

The fact the view exists to show sits at Item, Value or Title. It never sits in a Meta or Hint line, which the eye skips. Code: `TextStyle::Meta` and `TextStyle::Hint` both resolve to palette index 8 in `crates/fno/src/text_style.rs`. Source: taste-skill `references/app-screens.md` section 2.2 (the finding goes where it is read first).

### One question per view

A view answers one question first, and the first bold line answers it. The squint test checks this: blur the shot to weight and indent, and the primary line, the groups and their order still read. Code: `needs_overlay_lines` in `crates/fno/src/client/needs_view.rs` opens with the Section " Mine" and then " They need you", one question per group. Source: taste-skill `references/app-screens.md` section 2.2, impeccable `reference/layout.md` (the squint test).

### Four options per group

A footer hint line or a menu group offers at most four choices. More than four exceeds working memory, so split the group or move rare keys to the keys modal. Code: the needs view footer in `needs_overlay_lines` (`crates/fno/src/client/needs_view.rs`) carries three key groups. Source: impeccable `reference/critique.md` (the working-memory rule, four items).

## Theme roles

A theme is the `Theme` struct in `crates/fno/src/theme.rs`. A view never names a palette color. It names a `Role`, and `cell_style` in `crates/fno/src/theme.rs` turns the role into `(fg, bg, flags)` under the active theme. There are two layers: the palette (the `Theme` fields) and then the role. The text levels resolve above the theme split, so they follow the emulator's palette under every theme.

### A view binds a role, never a color

A view paints through a `Role` or a `TextStyle`. A literal `Color::Indexed` or `Color::Rgb` in view code is a defect, because it ignores the theme and the user's override. The one exception is `Role::Swatch`, where the color is the content (the colors tab). Code: `cell_style` in `crates/fno/src/theme.rs` (the doc comment calls it the single place a role becomes a style). Source: ai-design-skills `skills/figma-designer/SKILL.md` (two-layer color tokens).

### Borders wear the brand

Every border paints the theme's `border` color, and a named theme sets `border` to its own `brand`. The `mux.theme.border` key overrides every border at once. Code: the `border` field doc on `Theme` and the `Role::Border` arm of `cell_style` in `crates/fno/src/theme.rs`.

### Needs-you is never the brand

A question or block waiting on the user wears `needs_you`, never `brand`. Attention and selection are different states. If they share a color, a waiting worker looks like a chosen one. Code: the `needs_you` field doc on `Theme` in `crates/fno/src/theme.rs`, and `lattice_style` in `crates/fno/src/lattice.rs` (the Blocked glyph takes `needs_you`).

### The terminal theme inherits

The `terminal` theme is the only theme with `inherit: true`. It paints with default colors and flags only, so nothing clashes with the user's emulator. A change that makes a `terminal` render differ from the pre-theme render breaks this rule. Code: the `inherit` field doc on `Theme` and the `if t.inherit` branch of `cell_style` in `crates/fno/src/theme.rs`.

## Cursor, focus, selection and disabled

### One cursor

The row the keys act on is the cursor, and every surface paints it the same way. A surface never reads `Theme::sel` to paint a cursor row. Code: `cursor_style` in `crates/fno/src/cursor.rs`. `Role::BodySel`, `Role::BodyCursor` and `Role::BodyFocus` all resolve through it in `cell_style`. The test `no_surface_paints_a_cursor_from_the_sel_color` in the same file guards it.

### Marked is not driven

A row that is marked but not driven (the focused agent, the row under the pointer) wears a quiet band, never the cursor. Two rows must never both read as the row the keys act on. Code: `resting_band_style` in `crates/fno/src/cursor.rs`. Source: ai-design-skills `skills/figma-designer/SKILL.md` (focus, selected and disabled each get a distinct variant), taste-skill `references/app-screens.md` section 7.8.

### Disabled reads dim and inert

A disabled entry paints whole in Meta, takes no cursor and gets no accent segments. Arrow keys, Enter and a click skip it. Code: the `disabled` branches of `Popup::render` in `crates/fno/src/popup.rs`, where a disabled row takes `Role::Text(TextStyle::Meta)` and no segments.

### A control names its action

A button or chip reads as a control only with brackets, inverse or a key hint beside it. A link has no surface. The label names what it does. Code: `Role::ChipFocus` in `crates/fno/src/theme.rs` (the one filled chip) and the `esc close` chip, `Role::Chip`. Source: stageshow `references/craft.md` (links have no surface, a control has one at rest), impeccable `reference/craft-floor.md` (copy).

## Spacing and alignment

### Blank rows group before borders

Group related lines by blank rows before adding a border or a rule. Use zero, one or two blank rows only. Put more space above a heading than below it. Code: `PopupRow::Header` in `Popup::render` (`crates/fno/src/popup.rs`), where an empty header is the spacer line between sections. Source: impeccable `reference/layout.md` (group by proximity before containers, tight inside a group and wide between).

### Text never touches a border

When a framed row carries text (body, subtitle, tab strip, footer), it keeps the shared two-cell side pad from its border on both sides. The pad shrinks only on a row too narrow to carry it. A text cell beside a border glyph with no space between is a high finding on a shot. In code, `frame` asserts the same rule via `chrome::pad_violations` (`crates/fno/src/chrome.rs`), and the overlay-pad demo test (`crates/fno/src/client/tests/overlay_pad_tests.rs`) walks it. Popups add their own leading space in `Popup::render` (`crates/fno/src/popup.rs`), which stacks on the shared pad. Source: stageshow `references/craft.md` (nested shapes pad evenly).

### Popups cap at 60 columns

Popup content renders no wider than 60 columns. A longer line ellipsizes. Code: `WIDTH_CAP` in `crates/fno/src/popup.rs`.

### One column template per table

Every row of a table shares one column template. An optional cell keeps its column empty, and a wide glyph does not shift the columns after it. Code: the test `wide_glyph_name_keeps_the_pr_column_aligned` in `crates/fno/src/client/tests/glyph_tests.rs`. Source: stageshow `references/craft.md` (columns line up).

### Numbers align right

A number the reader compares across rows aligns right in a fixed width. Code: `window_spans` in `crates/fno/src/connections_view.rs` paints the used percent as `{:>3}%` at `type_scale::STRONG`. Source: taste-skill `references/app-screens.md` section 7.4.

### Few left edges

Count the distinct left edges in a view. Each indent step means one level of nesting. An edge that sits one cell off its neighbors is a defect. Source: taste-skill `references/critique.md` pass 4 (edges). Code: the fixed key column (`pad(glyph, kw)`) in `Popup::render`, `crates/fno/src/popup.rs`.

## Color and contrast

### Color marks data and state only

Color goes on data, on state and on the one accent. Chrome, labels and fine rows stay in the default fg. Code: `lattice_style` in `crates/fno/src/lattice.rs`, where only Blocked and an unseen Done wear a color, and the test `lattice_color_only_on_blocked_and_done` in `crates/fno/src/client/tests/glyph_tests.rs`. Source: taste-skill `references/app-screens.md` section 13.

### One red mark per object

Status color sits on the row at risk only. One object carries at most one red mark. A screen of red strings reads as noise. Code: `state_line` in `crates/fno/src/client/messages_view.rs` paints only a failed row with `TextStyle::ERROR`. Source: taste-skill `references/app-screens.md` sections 11 and 13.

### Never color alone

A state never relies on color alone. It pairs color with a glyph or a word. Code: `lattice_style` in `crates/fno/src/lattice.rs` gives each state its own glyph, and `lattice_glyphs_are_pairwise_distinct_and_single_cell` in `crates/fno/src/client/tests/glyph_tests.rs` guards it. Source: ai-design-skills `skills/figma-designer/SKILL.md` (never rely on color alone).

### Readable on dark and light

Body text reads on a dark and a light ground. Washed-out text, DIM on a light ground or a fixed light color on a light ground, is a high finding. Code: the text levels resolve above the theme split in `cell_style` (`crates/fno/src/theme.rs`). A theme test pins 4.5:1 for `ok`, `warn` and `error` (the `ok` field doc on `Theme`). Source: impeccable `reference/craft-floor.md` (contrast 4.5:1 for body text).

## Copy

### Sentence case, no caps labels

Headings and labels use sentence case. No all-caps headers, no tracked caps and no eyebrow label above a heading. Code: the Section lines " Mine" and " They need you" in `needs_overlay_lines` (`crates/fno/src/client/needs_view.rs`). Source: impeccable `reference/craft-floor.md` (no eyebrow), taste-skill house rules.

### One term per thing

One thing has one name across the whole path. The mux object that houses threads is a workspace in every string a person reads. Code: `docs/architecture/vocabulary-user-and-operator.md`, section "Workspace, squad, project, project group". Source: impeccable `reference/polish.md` (keep terminology consistent).

### Errors name the problem and the recovery

An error line says what failed and how to recover, inside the block that failed. Code: `state_line` in `crates/fno/src/client/messages_view.rs` paints "failed: <reason> · r retry". Source: impeccable `reference/craft-floor.md` (errors name the problem and the recovery).

### Say a status once

A view states a status once. A second copy of the same signal in a header, a badge and every row is noise. Source: stageshow `references/craft.md` (say it once). Code: `lattice_style` in `crates/fno/src/lattice.rs` puts the state in one glyph per row.

### No emoji

The mux uses single-cell glyphs, never emoji. Code: `lattice_glyphs_are_pairwise_distinct_and_single_cell` in `crates/fno/src/client/tests/glyph_tests.rs` refuses an astral emoji glyph. Source: taste-skill house rules.

## States

### Every block has an empty state

An empty block keeps its frame and its header and shows one line that says it is empty. Blank space with no line reads as broken. Code: `DisplayRow::TableEmpty` in `crates/fno/src/client.rs` is the sideline's empty row, and `crates/fno/src/client/sideline.rs` paints it as "no agents". Also, `needs_overlay_lines` paints "nothing needs you". Source: taste-skill `references/app-screens.md` sections 10 and 11.

### Loading keeps the layout

A loading cell holds its final width with a skeleton. The layout does not shift as data lands. Code: `skeleton_cell` in `crates/fno/src/client/row_meter.rs`. Source: taste-skill `references/app-screens.md` section 11 (loading).

### A missing value is a Meta dash

A missing or unmeasured value prints `-` in Meta. A real zero prints as a number. An unmeasured value never prints `0%`. Code: `ctx_cell` in `crates/fno/src/client/row_meter.rs`. Source: taste-skill `references/app-screens.md` section 11.

## Consistency

### Same role, same style

Two things with the same meaning wear the same level in every view. A section heading in one popup and a section heading in a panel both read as Section. Code: `type_scale` in `crates/fno/src/theme.rs` (views that share a level share its look).

### Fix drift at the narrowest level

When a view drifts from these rules, name the kind of drift: a missing level, a one-off, a conceptual mismatch or a local defect. Fix it at the narrowest level that cures it. Never add a new level for one exception. Source: impeccable `reference/polish.md` (drift classes). Code: `TextStyle` in `crates/fno/src/text_style.rs` is the one place to add a level.

## Dark and light

### Every rule holds on both grounds

Every rule on this page holds under the dark default theme and the light default theme. A paint PR carries shots in both. Code: `Theme::default_for` in `crates/fno/src/theme.rs` picks `footnote-paper` on a light ground and `footnote-superscript` otherwise.

## Rules the mux does not take

A terminal grid cannot use the rules below, so the critic never raises them.

- Font sizes, type scales, line height and measure.
- Corner radius, shadows, gradients and blur.
- Hover states.
- Breakpoints, touch targets and phone layouts.
- Images, icons and alt text.
- Motion and easing.
- Pixel spacing tokens and Figma token plumbing.
- Landing-page and hero rules.
- Browser scrollbars and ARIA.
- Chart rules.

Of stageshow, only `references/craft.md` and `references/review.md` apply.

The outside sources are paraphrased, not copied. impeccable is Apache-2.0. taste-skill, ai-design-skills and stageshow are MIT.

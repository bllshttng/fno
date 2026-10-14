//! The user-message band text fix for the shipped Claude Code themes.
//!
//! Claude Code paints the user's own message on a band whose background a
//! theme file sets (`userMessageBackground`), but it has no key for the text
//! on that band: the text keeps the base theme's color. Footnote Paper
//! (light base, `#373737` band) therefore reads dark on dark, and Footnote
//! Superscript (dark base, white band) reads white on white.
//!
//! The mux repaints the text of exactly those band cells. The match is the
//! band's exact background, so code blocks, diffs and every other cell stay
//! untouched. The rule can only raise contrast: the text it writes is the
//! theme's own ground color, the far end of the band. Another program that
//! happens to paint one of these backgrounds stays readable but loses its own
//! text colors on those cells.
//!
//! `terminal` keeps every pane cell byte-identical (it paints nothing the
//! emulator does not), so the caller skips the remap under an `inherit` theme.

use crate::proto::{cell_flags, Cell, Color};

/// One band: the backgrounds `themes/footnote-*.json` give it (rest, hover)
/// and the text color that reads on them.
struct Band {
    bg: [(u8, u8, u8); 2],
    text: (u8, u8, u8),
}

const BANDS: [Band; 2] = [
    // Footnote Paper: dark band on a light ground; text is Paper's ground.
    Band {
        bg: [(0x37, 0x37, 0x37), (0x44, 0x44, 0x44)],
        text: (0xf7, 0xf7, 0xf7),
    },
    // Footnote Superscript: white band on a dark ground; text is
    // Superscript's ground.
    Band {
        bg: [(0xff, 0xff, 0xff), (0xe6, 0xe6, 0xe6)],
        text: (0x14, 0x14, 0x14),
    },
];

/// Set a band cell's text to the band's contrast color. A cell outside every
/// band, or one drawn with INVERSE (its bg is really the text), is left alone.
pub(crate) fn remap(cell: &mut Cell) {
    if cell.flags & cell_flags::INVERSE != 0 {
        return;
    }
    let Color::Rgb(r, g, b) = cell.bg else {
        return;
    };
    if let Some(band) = BANDS.iter().find(|band| band.bg.contains(&(r, g, b))) {
        let (tr, tg, tb) = band.text;
        cell.fg = Color::Rgb(tr, tg, tb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_html::{contrast_ratio, DARK, LIGHT};

    fn cell(fg: Color, bg: Color, flags: u8) -> Cell {
        Cell {
            c: 'x',
            fg,
            bg,
            flags,
        }
    }

    #[test]
    fn a_band_cell_takes_the_contrast_text_and_keeps_its_ground() {
        for (bg, ground, shown_on) in [
            ((0x37, 0x37, 0x37), (0xf7, 0xf7, 0xf7), LIGHT),
            ((0x44, 0x44, 0x44), (0xf7, 0xf7, 0xf7), LIGHT),
            ((0xff, 0xff, 0xff), (0x14, 0x14, 0x14), DARK),
            ((0xe6, 0xe6, 0xe6), (0x14, 0x14, 0x14), DARK),
        ] {
            // The cell as Claude Code paints it: the base theme's text.
            let base_text = match shown_on.bg.0 {
                0..=127 => Color::Rgb(0xe8, 0xe8, 0xe8),
                _ => Color::Rgb(0x14, 0x14, 0x14),
            };
            let mut c = cell(base_text, Color::Rgb(bg.0, bg.1, bg.2), 0);
            let before = contrast_ratio(&c, shown_on);
            remap(&mut c);
            assert_eq!(c.fg, Color::Rgb(ground.0, ground.1, ground.2));
            assert_eq!(
                c.bg,
                Color::Rgb(bg.0, bg.1, bg.2),
                "the band is not repainted"
            );
            assert!(
                contrast_ratio(&c, shown_on) >= 7.0,
                "band {bg:?} reads below AAA after the remap (was {before:.2})"
            );
        }
    }

    /// The table and the shipped theme files describe the same bands: a
    /// color edited in one place alone leaves the band unreadable.
    #[test]
    fn the_table_matches_the_shipped_theme_files() {
        let themes = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes");
        for (file, band) in [
            ("footnote-paper.json", &BANDS[0]),
            ("footnote-superscript.json", &BANDS[1]),
        ] {
            let text = std::fs::read_to_string(themes.join(file)).expect("shipped theme file");
            let json: serde_json::Value = serde_json::from_str(&text).expect("valid json");
            let hex = |key: &str| {
                let v = json["overrides"][key].as_str().expect("band key");
                let n = u32::from_str_radix(v.trim_start_matches('#'), 16).expect("hex");
                ((n >> 16) as u8, (n >> 8) as u8, n as u8)
            };
            assert_eq!(hex("userMessageBackground"), band.bg[0], "{file} rest band");
            assert_eq!(
                hex("userMessageBackgroundHover"),
                band.bg[1],
                "{file} hover band"
            );
        }
    }

    #[test]
    fn cells_outside_the_band_are_untouched() {
        let code = cell(
            Color::Rgb(0x14, 0x14, 0x14),
            Color::Rgb(0xf0, 0xf0, 0xf0),
            0,
        );
        let diff = cell(
            Color::Rgb(0xe8, 0xe8, 0xe8),
            Color::Rgb(0x22, 0x44, 0x22),
            0,
        );
        let plain = cell(Color::Default, Color::Default, 0);
        let indexed = cell(Color::Indexed(7), Color::Indexed(0), 0);
        // INVERSE swaps the pair at paint time: the band color is the text.
        let inverse = cell(
            Color::Rgb(0xe8, 0xe8, 0xe8),
            Color::Rgb(0x37, 0x37, 0x37),
            cell_flags::INVERSE,
        );
        for original in [code, diff, plain, indexed, inverse] {
            let mut c = original;
            remap(&mut c);
            assert_eq!(c, original);
        }
    }
}

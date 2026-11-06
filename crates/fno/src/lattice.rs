//! The unified icon lattice: ONE state->style mapping every renderer
//! (sideline rows, tab rollups, overlays) calls, so glyph, weight,
//! and accent read as one system. Outline `○` = waiting/idle, filled `●` =
//! active, `▲` = needs-attention (the accent state; an unseen `✓` wears the
//! done color). Exhaustive by design: a new variant is a compile error at
//! every call site, never a silent glyph.

use crate::proto::{cell_flags, Color};

/// One worker's lattice state: the glyph the whole chrome agrees on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LatticeState {
    Working,
    Idle,
    Blocked,
    DoneUnseen,
    Exited,
    /// A terminal row with no positive corroboration: no confirmed-
    /// dead pid, no confirmed-gone pane. Distinct from `Exited` because the
    /// operator's routing decision turns on it - `Exited` means respawn is
    /// safe, `Unmeasured` means look before you spawn.
    Unmeasured,
    /// A live pane that positively read as nothing-running-yet: OSC
    /// 133 markers active, no command open, no completed block. Distinct from
    /// `Idle` (a completed block, the prompt back) and from `Unmeasured` (no
    /// reading at all): a pristine shell is an honest zero, not a waiting
    /// worker and not an unknown.
    Empty,
}

/// The terminal theme's attention color (index 3 = the emulator's own
/// amber/yellow), kept as the reference value the lattice tests assert
/// against. Production reads the live theme's `needs_you`, so this is
/// test-only - the tests that assert it pin the view to the `terminal`
/// theme, where the two are the same `Indexed(3)`.
#[cfg(test)]
pub(crate) const LATTICE_ACCENT: Color = Color::Indexed(3);

pub(crate) struct LatticeStyle {
    pub(crate) glyph: char,
    pub(crate) flags: u8,
    pub(crate) fg: Color,
}

/// The single source of glyph/weight/color per state. Every state differs from
/// every other by GLYPH alone (BOLD/DIM/accent are reinforcement, never the
/// sole discriminator), so a weak-BOLD or monochrome terminal still reads.
///
/// `needs_you` is the needs-attention color, the active theme's dedicated
/// attention accent rather than a hardcoded yellow: under `terminal` it is
/// `Indexed(3)` (the emulator's own amber, preserved exactly), under a named
/// theme it is the palette's pick - never the brand color, so waiting and
/// selected never read alike. Only the one caller that reads `.fg` supplies
/// it; callers that want only the glyph/flags use [`lattice_glyph`] and stay
/// out of color. `ok` is the theme's done color: an unseen Done row reads
/// green, the one other state that wears a color (the user's 2026-10-27 pick).
pub(crate) fn lattice_style(s: LatticeState, needs_you: Color, ok: Color) -> LatticeStyle {
    let (glyph, flags, fg) = match s {
        LatticeState::Working => ('●', cell_flags::BOLD, Color::Default),
        LatticeState::Idle => ('○', 0, Color::Default),
        LatticeState::Blocked => ('▲', cell_flags::BOLD, needs_you),
        LatticeState::DoneUnseen => ('✓', cell_flags::BOLD, ok),
        LatticeState::Exited => ('✗', cell_flags::DIM, Color::Default),
        LatticeState::Unmeasured => ('?', cell_flags::DIM, Color::Default),
        LatticeState::Empty => ('∅', cell_flags::DIM, Color::Default),
    };
    LatticeStyle { glyph, flags, fg }
}

/// The glyph + flags for a state, with no color. For every caller that does not
/// read `.fg` (i.e. every caller except the one accent-colored span), so they
/// do not have to thread a theme accent they never use.
pub(crate) fn lattice_glyph(s: LatticeState) -> (char, u8) {
    let st = lattice_style(s, Color::Default, Color::Default);
    (st.glyph, st.flags)
}

/// The frames a Working row's status glyph turns through, so a working row
/// moves and a done row is still. Only the interactive client turns them
/// ([`start_spin`]); a snapshot, a test, or reduced motion draws the still
/// `●` - the set's own `full`. The set is the throbber crate's
/// BLACK_CIRCLE: the same quarter-turn family the hand-built table drew,
/// now a maintained crate's vocabulary.
pub(crate) fn spin_symbols() -> &'static [&'static str] {
    throbber_widgets_tui::symbols::throbber::BLACK_CIRCLE.symbols
}
pub(crate) const SPIN_FRAME_MS: u64 = 250;
static SPIN_EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Start the spin for this process, unless the splash's motion gate says no
/// (not a TTY, CI, or `REDUCED_MOTION`).
pub(crate) fn start_spin() {
    if crate::splash::animated() {
        SPIN_EPOCH.get_or_init(std::time::Instant::now);
    }
}

/// Start the spin whatever the motion gate says: a snapshot draws one still
/// frame, and a live client's working row is a spin frame, not the rest `●`.
pub(crate) fn freeze_spin() {
    SPIN_EPOCH.get_or_init(std::time::Instant::now);
}

/// When the spin started; `None` while it is off.
pub(crate) fn spin_epoch() -> Option<std::time::Instant> {
    SPIN_EPOCH.get().copied()
}

pub(crate) fn spin_frame(elapsed_ms: u64) -> char {
    let syms = spin_symbols();
    syms[(elapsed_ms / SPIN_FRAME_MS) as usize % syms.len()]
        .chars()
        .next()
        .unwrap_or('\u{25cf}')
}

/// The status-cell glyph: a spin frame for a Working row while the spin
/// runs, else the lattice glyph.
pub(crate) fn status_glyph(s: LatticeState) -> char {
    match (s, spin_epoch()) {
        (LatticeState::Working, Some(t0)) => spin_frame(t0.elapsed().as_millis() as u64),
        _ => lattice_glyph(s).0,
    }
}

/// The status column's word per lattice state, shortened to fit the
/// 5-column cell (operator, 2026-09-21; the fleet's mail vocabulary keeps
/// the long forms). `Unmeasured` and `Empty` keep their glyphs.
pub(crate) fn status_word(s: LatticeState) -> &'static str {
    match s {
        LatticeState::Working => "Work",
        LatticeState::Idle => "Idle",
        LatticeState::Blocked => "Input",
        LatticeState::DoneUnseen => "Done",
        LatticeState::Exited => "Stop",
        LatticeState::Unmeasured => "?",
        LatticeState::Empty => "\u{2205}",
    }
}

/// The build throbber's frame set: the same crate's BRAILLE_SIX, the user's
/// 2026-10-09 pick for a compiling row (one cell wide, throbber-widgets
/// vocabulary, distinct from the BLACK_CIRCLE working spin at a glance).
pub(crate) fn build_spin_symbols() -> &'static [&'static str] {
    throbber_widgets_tui::symbols::throbber::BRAILLE_SIX.symbols
}

/// The compiling row's still glyph (a snapshot, a test, reduced motion, or
/// the card line's rest shape): the set's own `full`.
pub(crate) fn build_rest_glyph() -> char {
    throbber_widgets_tui::symbols::throbber::BRAILLE_SIX
        .full
        .chars()
        .next()
        .unwrap_or('\u{283f}')
}

/// One frame of the build throbber at `elapsed_ms` into the spin epoch (the
/// SAME clock the working spin turns on, so no new timer exists).
pub(crate) fn build_frame(elapsed_ms: u64) -> char {
    let syms = build_spin_symbols();
    syms[(elapsed_ms / SPIN_FRAME_MS) as usize % syms.len()]
        .chars()
        .next()
        .unwrap_or('\u{283f}')
}

/// The build mark's status-cell override. Only a Working row carries it: a
/// blocked row needs a human (the accent outranks build info), an idle row
/// runs no cargo, and the mark never fights the lattice. Compiling renders
/// the BRAILLE_SIX frame IN PLACE OF the working cell - never both. The
/// waiting arm renders the static hourglass with the queue position when
/// the lane read can name it. Both stay within the `cells`-column status
/// cell the caller names: the list column is 5, the card glyph column 1
/// (its second cell is the edge bar's, which paints over cell 0 after the
/// blit - a two-cell mark there loses its left glyph to the bar, the
/// 2026-11-06 shot finding), the slim rail 2.
pub(crate) fn build_status_cell(
    mark: Option<crate::proto::BuildMark>,
    s: LatticeState,
    cells: usize,
) -> Option<String> {
    if !matches!(s, LatticeState::Working) {
        return None;
    }
    let mark = mark?;
    match mark.stage {
        // The frame REPLACES the working cell: the status column is
        // Constraint::Length(5) and its words are shortened to fit it, so
        // the glyph alone carries the state (the card line's own
        // still-glyph convention).
        crate::proto::BuildStage::Compiling => Some(match spin_epoch() {
            Some(t0) => build_frame(t0.elapsed().as_millis() as u64).to_string(),
            None => build_rest_glyph().to_string(),
        }),
        crate::proto::BuildStage::Waiting => Some(match (cells >= 2, mark.position) {
            (true, Some(n)) => format!("{HOURGLASS}{n}"),
            _ => HOURGLASS.to_string(),
        }),
    }
}

/// The waiting mark's glyph (user pick, 2026-10-09: an hourglass and the
/// position). The BLACK HOURGLASS codepoint, not the emoji one: the
/// snapshot grid draws each cell at an exact textLength, which squeezes
/// the emoji-presentation hourglass to nothing (the 2026-11-06 probe shot
/// provably dropped it), while this text-presentation spelling renders in
/// the grid, in the terminal, and distinct from every lattice glyph.
pub(crate) const HOURGLASS: char = '\u{29d7}';

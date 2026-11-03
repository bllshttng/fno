//! The one text hierarchy every mux view shares. A view picks a level for
//! each piece of text ([`TextStyle`]); the theme picks how that level looks
//! ([`TextStyle::resolve`]). Backlog panels, popups and lines overlays all
//! build [`TLine`]s, so a section, a value and a hint read apart everywhere.

use super::chrome::Join;
use super::proto::{cell_flags, Color};
use super::theme::{Role, Theme};

/// A state token's urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Error,
}

/// What a piece of text is. The view picks the level; [`TextStyle::resolve`]
/// is the whole style policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    /// A view or node title: the terminal's bold fg.
    Title,
    /// A group or section heading: the theme's brand accent, bold.
    Section,
    /// Ordinary row text.
    Item,
    /// The value of a field or toggle: bold, so it stands out from its label.
    Value,
    /// Counts, ages, ids and other secondary facts: the dim slot.
    Meta,
    /// Footers, key hints and notices: the dim slot, italic.
    Hint,
    /// A handle, account or bracketed token: the accent slot.
    Badge,
    /// The active tab cell of a tab strip (inverted).
    Tab,
    /// A state token that needs the reader.
    Status(Status),
    /// A diff's added line.
    Added,
    /// A diff's removed line.
    Removed,
}

impl TextStyle {
    pub const OK: TextStyle = TextStyle::Status(Status::Ok);
    pub const WARN: TextStyle = TextStyle::Status(Status::Warn);
    pub const ERROR: TextStyle = TextStyle::Status(Status::Error);

    /// `(fg, bg, flags)` of this level under `t`. Every level but `Section`
    /// follows the emulator's palette (attributes and palette indexes, never
    /// a fixed color), so the same render reads on a dark and a light
    /// ground. Index 8, not the DIM flag: DIM washes the default fg out on a
    /// light terminal, while index 8 stays a readable gray under both.
    pub fn resolve(self, t: &Theme) -> (Color, Color, u8) {
        let d = Color::Default;
        match self {
            TextStyle::Title | TextStyle::Value => (d, d, cell_flags::BOLD),
            TextStyle::Section => (t.brand, d, cell_flags::BOLD),
            TextStyle::Item => (d, d, 0),
            TextStyle::Meta => (Color::Indexed(8), d, 0),
            TextStyle::Hint => (Color::Indexed(8), d, cell_flags::ITALIC),
            TextStyle::Badge | TextStyle::Status(Status::Warn) => (Color::Indexed(3), d, 0),
            TextStyle::Tab => (d, d, cell_flags::INVERSE),
            TextStyle::Status(Status::Ok) => (Color::Indexed(2), d, 0),
            TextStyle::Status(Status::Error) | TextStyle::Removed => (Color::Indexed(1), d, 0),
            TextStyle::Added => (Color::Indexed(2), d, 0),
        }
    }

    /// The theme role that paints this level.
    pub fn role(self) -> Role {
        Role::Text(self)
    }
}

/// One styled segment.
#[derive(Clone)]
pub struct TSeg {
    pub(crate) text: String,
    pub(crate) role: TextStyle,
}

/// One styled line: flattened text plus a per-char role walk, and - when
/// this is the cursor's row - the explicit highlight band pair the
/// sideline wears (a real fg/bg pair, zero DIM inside the band).
#[derive(Clone, Debug)]
pub struct TLine {
    pub(crate) text: String,
    pub(crate) roles: Vec<TextStyle>,
    /// The role for chars past the role walk (whole-line constructors).
    pub(crate) default_role: TextStyle,
    pub(crate) band: bool,
    /// How this line follows the one above; a wrapped line joins back on copy.
    pub(crate) join: Join,
}

/// Helper: append one segment's chars.
fn push_seg(text: &mut String, roles: &mut Vec<TextStyle>, seg: &TSeg) {
    for ch in seg.text.chars() {
        text.push(ch);
        roles.push(seg.role);
    }
}

/// Anything a lines overlay can show as one styled line. A bare string is a
/// plain `Item` line; a view that wants levels builds a [`TLine`].
pub trait ToTLine {
    fn to_tline(&self) -> TLine;
}

impl ToTLine for TLine {
    fn to_tline(&self) -> TLine {
        self.clone()
    }
}

impl ToTLine for String {
    fn to_tline(&self) -> TLine {
        TLine::plain(self.as_str())
    }
}

impl ToTLine for &str {
    fn to_tline(&self) -> TLine {
        TLine::plain(*self)
    }
}

impl TLine {
    /// A whole line at one level.
    pub(crate) fn line(style: TextStyle, s: impl Into<String>) -> TLine {
        let mut l = TLine::plain(s);
        l.default_role = style;
        l
    }

    /// A line from `(text, level)` parts.
    pub(crate) fn parts(parts: &[(&str, TextStyle)]) -> TLine {
        let segs: Vec<TSeg> = parts
            .iter()
            .map(|(text, role)| TSeg {
                text: (*text).to_string(),
                role: *role,
            })
            .collect();
        TLine::of(&segs)
    }

    /// Drop trailing spaces and their roles.
    pub(crate) fn trim_end(mut self) -> TLine {
        let keep = self.text.trim_end().chars().count();
        self.text.truncate(self.text.trim_end().len());
        self.roles.truncate(keep);
        self
    }

    /// Drop the first `n` chars and their roles.
    pub(crate) fn strip_front(mut self, n: usize) -> TLine {
        let cut = self
            .text
            .char_indices()
            .nth(n)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len());
        self.text.drain(..cut);
        let drop = n.min(self.roles.len());
        self.roles.drain(..drop);
        self
    }

    /// Split into lines of at most `w` display columns, breaking after the
    /// last space that fits, or mid-word when none does. Each char keeps its
    /// role.
    pub(crate) fn wrap(self, w: usize) -> Vec<TLine> {
        let w = w.max(1);
        let chars: Vec<char> = self.text.chars().collect();
        if chars.iter().map(|c| char_w(*c)).sum::<usize>() <= w {
            return vec![self];
        }
        let piece = |from: usize, to: usize, join: Join| TLine {
            text: chars[from..to].iter().collect(),
            roles: self
                .roles
                .get(from..to.min(self.roles.len()))
                .unwrap_or_default()
                .to_vec(),
            default_role: self.default_role,
            band: self.band,
            join,
        };
        let mut out = Vec::new();
        let mut join = Join::Line;
        let (mut start, mut used, mut space) = (0usize, 0usize, None);
        let mut i = 0usize;
        while i < chars.len() {
            let cw = char_w(chars[i]);
            if used + cw > w && i > start {
                let soft = space.filter(|&s| s > start);
                let cut = soft.unwrap_or(i);
                out.push(piece(start, cut, join));
                join = if soft.is_some() {
                    Join::Space
                } else {
                    Join::Seam
                };
                start = cut;
                used = chars[start..i].iter().map(|c| char_w(*c)).sum();
                space = None;
                continue;
            }
            if chars[i] == ' ' {
                space = Some(i + 1);
            }
            used += cw;
            i += 1;
        }
        out.push(piece(start, chars.len(), join));
        out
    }

    /// Build from segments.
    pub(crate) fn of(segs: &[TSeg]) -> TLine {
        let mut text = String::new();
        let mut roles = Vec::new();
        for seg in segs {
            push_seg(&mut text, &mut roles, seg);
        }
        TLine {
            text,
            roles,
            default_role: TextStyle::Item,
            band: false,
            join: Join::Line,
        }
    }

    /// A plain body line.
    pub(crate) fn plain(s: impl Into<String>) -> TLine {
        TLine {
            text: s.into(),
            roles: Vec::new(),
            default_role: TextStyle::Item,
            band: false,
            join: Join::Line,
        }
    }

    /// An all-meta line (summary, query, errors).
    pub(crate) fn meta(s: impl Into<String>) -> TLine {
        TLine {
            text: s.into(),
            roles: Vec::new(),
            default_role: TextStyle::Meta,
            band: false,
            join: Join::Line,
        }
    }

    /// An all-head line (lane names).
    pub(crate) fn head(s: impl Into<String>) -> TLine {
        TLine {
            text: s.into(),
            roles: Vec::new(),
            default_role: TextStyle::Title,
            band: false,
            join: Join::Line,
        }
    }

    /// Append another line's chars and roles. Text and roles grow in
    /// lockstep: a merged-in plain line contributes no role entries, so the
    /// walk pads the gap with that line's default role - without this, every
    /// later column's roles shift and a header's style lands mid-word.
    pub(crate) fn push_line(&mut self, other: TLine) {
        let had = self.text.chars().count();
        while self.roles.len() < had {
            self.roles.push(self.default_role);
        }
        self.text.push_str(&other.text);
        self.roles.extend(other.roles);
        let chars = self.text.chars().count();
        while self.roles.len() < chars {
            self.roles.push(other.default_role);
        }
    }

    /// Pad with spaces to `w` display columns.
    pub(crate) fn pad_to(self, w: usize) -> TLine {
        let mut line = self;
        let used: usize = line.text.chars().map(char_w).sum();
        for _ in used..w {
            line.text.push(' ');
            line.roles.push(line.default_role);
        }
        line
    }

    /// Flatten every char to `Meta`, the same dim slot [`paint_title_row`]
    /// gives an unfocused title: the hint reads as inert chrome while its
    /// pane does not hold the keyboard, instead of looking equally ready to
    /// act as a focused one's. Clearing `roles` is enough - paint_bline
    /// falls back to `default_role` past the role walk's end, the same way
    /// [`TLine::meta`] leaves it empty.
    pub(crate) fn dimmed(mut self) -> TLine {
        self.roles.clear();
        self.default_role = TextStyle::Meta;
        self
    }
}

impl std::fmt::Display for TLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// A styled line reads as its text, so string-shaped call sites (tests,
/// notices) keep working unchanged.
impl std::ops::Deref for TLine {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl PartialEq<&str> for TLine {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<String> for TLine {
    fn eq(&self, other: &String) -> bool {
        self.text == *other
    }
}

/// Display width of one char (fullwidth = 2, else 1).
pub(crate) fn char_w(ch: char) -> usize {
    unicode_width::UnicodeWidthChar::width(ch)
        .unwrap_or(1)
        .max(1)
}

impl TLine {
    /// Truncate to `w` display columns; wide glyphs never straddle the cut.
    pub(crate) fn trunc(self, w: usize) -> TLine {
        let mut used = 0usize;
        let cut = self
            .text
            .char_indices()
            .find(|(_, ch)| {
                used += char_w(*ch);
                used > w
            })
            .map(|(i, _)| i)
            .unwrap_or(self.text.len());
        let keep = self.text[..cut].chars().count();
        TLine {
            text: self.text[..cut].to_string(),
            roles: self.roles.into_iter().take(keep).collect(),
            default_role: self.default_role,
            band: self.band,
            join: self.join,
        }
    }
}

/// A line borrows as its text, so `[TLine]::join` and string-shaped helpers
/// keep working.
impl std::borrow::Borrow<str> for TLine {
    fn borrow(&self) -> &str {
        &self.text
    }
}

impl From<&str> for TLine {
    fn from(s: &str) -> TLine {
        TLine::plain(s)
    }
}

impl From<String> for TLine {
    fn from(s: String) -> TLine {
        TLine::plain(s)
    }
}

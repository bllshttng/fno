//! Client-side desktop-notification delivery (the OSC 777 leg). A pane's OSC
//! 9 / 99 / 777 arrives re-emitted as [`ServerMsg::DesktopNotice`], and
//! `fno notify`'s control command lands the same way: this module turns
//! `(title, body)` into the one escape shape the outer terminal understands,
//! `ESC ] 777 ; notify ; TITLE ; BODY BEL` (rxvt-style, honored by Ghostty),
//! and writes it to the client tty. The title is the pane name on the
//! pane-capture path, so the desktop alert names the pane it came from.
//!
//! The trust boundary: both fields are pane- or peer-controlled text headed
//! for a raw tty. [`osc777_payload`] strips every control character before
//! anything is emitted, so a payload can never smuggle an escape sequence
//! (`ESC`, BEL, CSI) into the terminal - the allowlist is the shape, never a
//! passthrough. Failure is best-effort (a terminal that drops OSC 777 loses
//! the toast, never a crash), the same posture the OSC 52 clipboard fallback
//! takes.

/// Per-field character cap. A desktop toast is a one-glance line; a longer
/// body is truncated, never emitted at unbounded length.
const MAX_FIELD_CHARS: usize = 1024;

/// Build the OSC 777 notification escape for `(title, body)`: sanitize both
/// fields, then wrap `777;notify;<title>;<body>` in `ESC ]` ... BEL. Pure: the
/// caller owns the write, so tests pin the bytes without a tty.
pub fn osc777_payload(title: &str, body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(title.len() + body.len() + 16);
    out.extend_from_slice(b"\x1b]777;notify;");
    out.extend_from_slice(sanitize(title, MAX_FIELD_CHARS).as_bytes());
    out.push(b';');
    out.extend_from_slice(sanitize(body, MAX_FIELD_CHARS).as_bytes());
    out.push(0x07);
    out
}

/// Deliver `(title, body)` through `emit` (the caller's tty write, injected so
/// tests capture bytes). Best-effort: a failed write is dropped - a desktop
/// notice is never load-bearing.
pub fn deliver<F>(title: &str, body: &str, emit: F)
where
    F: FnOnce(&[u8]) -> std::io::Result<()>,
{
    let payload = osc777_payload(title, body);
    let _ = emit(&payload);
}

/// Strip control characters (`C0`, DEL) and cap length on a char boundary.
/// Printable text survives verbatim; a control byte is dropped, never
/// replaced - a toast with a hole beats a toast that executes a sequence.
fn sanitize(field: &str, max_chars: usize) -> String {
    field
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc777_wraps_title_and_body_with_escape_and_bel() {
        let payload = osc777_payload("pane w1", "done");
        assert_eq!(payload.first(), Some(&0x1b));
        assert_eq!(payload.last(), Some(&0x07));
        let mid = std::str::from_utf8(&payload[1..payload.len() - 1]).unwrap();
        assert_eq!(mid, "]777;notify;pane w1;done");
    }

    #[test]
    fn sanitize_strips_controls_and_caps_length() {
        // An embedded escape attempt never reaches the tty as an escape.
        assert_eq!(sanitize("a\u{1b}]0;evil\u{7}b", 1024), "a]0;evilb");
        // Over-cap text truncates on a char boundary (chars, not bytes).
        let big = "é".repeat(MAX_FIELD_CHARS + 5);
        assert_eq!(
            sanitize(&big, MAX_FIELD_CHARS).chars().count(),
            MAX_FIELD_CHARS
        );
        // Printable text passes through unchanged.
        assert_eq!(sanitize("clean", 1024), "clean");
    }
}

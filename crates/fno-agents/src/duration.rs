//! One human-written duration, parsed the same way everywhere.

/// Parse a `<number><s|m|h|d>` duration into seconds (`24h`, `90m`, `30s`,
/// `7d`). `None` on anything else - an unparsable window is a usage error,
/// never "everything".
pub fn parse_duration_secs(raw: &str) -> Option<u64> {
    let (digits, unit) = raw.split_at(raw.len().saturating_sub(1));
    let multiplier = match unit {
        "s" => 1u64,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
}

/// Parse a `spawn --wait` duration into seconds, `cmd_spawn`'s grammar:
/// digits with an optional fraction and an optional `s`, `m` or `h` (any
/// case), seconds when bare, surrounding whitespace ignored.
pub fn parse_wait_secs(raw: &str) -> Option<f64> {
    let raw = raw.trim();
    let split = raw
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(raw.len());
    let (number, unit) = raw.split_at(split);
    let multiplier = match unit.to_ascii_lowercase().as_str() {
        "" | "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        _ => return None,
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, "0"));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) {
        return None;
    }
    let secs = number.parse::<f64>().ok()? * multiplier;
    // A long digit run parses as infinity, and `Duration::from_secs_f64`
    // panics past `u64::MAX` seconds: refuse it as a bad duration.
    (secs.is_finite() && secs < 1e12).then_some(secs)
}

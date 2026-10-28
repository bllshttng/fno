//! The glob matcher every rule trigger shares: a port of Python
//! `fnmatch.fnmatch` (POSIX case rules), where `*` crosses `/`, `?` is one
//! character, and `[...]` is a class with `!` negation and `-` ranges.

/// Port of Python fnmatch.fnmatch (POSIX case rules): `*` crosses `/`, `?`
/// is one char, `[...]` a class with `!` negation and `-` ranges.
pub fn fnmatch(name: &str, pat: &str) -> bool {
    match_class(name.as_bytes(), pat.as_bytes())
}

fn match_class(n: &[u8], p: &[u8]) -> bool {
    match p.split_first() {
        None => n.is_empty(),
        Some((b'*', rest)) => {
            if rest.is_empty() {
                return true;
            }
            (0..=n.len()).any(|i| match_class(&n[i..], rest))
        }
        Some((b'?', rest)) => !n.is_empty() && match_class(&n[1..], rest),
        Some((b'[', _)) => match char_class(n.first().copied(), p) {
            // Unterminated class reads as a literal `[`.
            None => !n.is_empty() && n[0] == b'[' && match_class(&n[1..], &p[1..]),
            Some((hit, used)) => hit && !n.is_empty() && match_class(&n[1..], &p[used..]),
        },
        Some((c, rest)) => !n.is_empty() && n[0] == *c && match_class(&n[1..], rest),
    }
}

/// `[seq]` / `[!seq]` matcher; returns (matched, pattern bytes consumed).
/// None on an unterminated class, which the caller reads as a literal `[`.
fn char_class(c: Option<u8>, p: &[u8]) -> Option<(bool, usize)> {
    let negate = matches!(p.get(1), Some(b'!'));
    let mut i = if negate { 2 } else { 1 };
    let start = i;
    let mut hit = false;
    loop {
        match p.get(i) {
            None => return None,
            Some(b']') if i > start => {
                return Some((if negate { !hit } else { hit }, i + 1));
            }
            Some(&lo)
                if p.get(i + 1) == Some(&b'-')
                    && matches!(p.get(i + 2), Some(&hi) if hi != b']') =>
            {
                let hi = p[i + 2];
                if let Some(c) = c {
                    if c >= lo && c <= hi {
                        hit = true;
                    }
                }
                i += 3;
            }
            Some(&ch) => {
                if c == Some(ch) {
                    hit = true;
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fnmatch;

    #[test]
    fn star_crosses_slashes_and_question_marks_one_char() {
        assert!(fnmatch("crates/fno/src/lib.rs", "crates/**/*.rs"));
        assert!(fnmatch("a/b/c", "a/*"));
        assert!(fnmatch("cli/src/fno/backlog/law.py", "cli/src/fno/*.py"));
        assert!(!fnmatch("abc", "a?"));
    }

    #[test]
    fn classes_range_negate_and_read_unterminated_as_literal() {
        assert!(fnmatch("d1", "d[0-9]"));
        assert!(!fnmatch("dx", "d[0-9]"));
        assert!(fnmatch("dx", "d[!0-9]"));
        assert!(fnmatch("a[b", "a["));
    }
}

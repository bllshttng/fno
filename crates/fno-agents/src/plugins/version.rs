//! PEP 440 versions, as `packaging.version.Version` parses and orders them:
//! an optional `N!` epoch, a dotted release, then optional pre (`a`, `b`,
//! `rc` and their spellings), post and dev segments, and a `+local` label.
//! Case and a leading `v` are ignored. Ordering follows packaging's
//! comparison key, so `1.0.dev1 < 1.0a1 < 1.0 < 1.0.post1`.

use std::cmp::Ordering;

#[derive(Clone, Debug)]
pub struct Version {
    epoch: u64,
    release: Vec<u64>,
    pre: Option<(u8, u64)>,
    post: Option<u64>,
    dev: Option<u64>,
    local: Vec<LocalPart>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LocalPart {
    // A string part sorts below a numeric one.
    Text(String),
    Number(u64),
}

/// packaging's `InvalidVersion` message.
pub fn invalid(text: &str) -> String {
    format!(
        "Invalid version: {}",
        crate::client_commands::py_repr_str(text)
    )
}

fn number(text: &str, i: &mut usize) -> Option<u64> {
    let start = *i;
    let bytes = text.as_bytes();
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        *i += 1;
    }
    (start != *i)
        .then(|| text[start..*i].parse().ok())
        .flatten()
}

fn eat_separator(text: &str, i: &mut usize) {
    if matches!(text.as_bytes().get(*i), Some(b'.' | b'-' | b'_')) {
        *i += 1;
    }
}

fn eat_word(text: &str, i: &mut usize, words: &[&str]) -> Option<usize> {
    // Longest spelling first, so `preview` wins over `pre`.
    let mut found: Option<(usize, usize)> = None;
    for (index, word) in words.iter().enumerate() {
        if text[*i..].starts_with(word) && found.is_none_or(|(_, len)| word.len() > len) {
            found = Some((index, word.len()));
        }
    }
    let (index, len) = found?;
    *i += len;
    Some(index)
}

impl Version {
    pub fn parse(raw: &str) -> Result<Version, String> {
        let lowered = raw.trim().to_ascii_lowercase();
        let text = lowered.strip_prefix('v').unwrap_or(&lowered);
        let bad = || invalid(raw);
        let mut i = 0;
        let mut epoch = 0;
        if let Some(bang) = text.find('!') {
            epoch = text[..bang].parse().map_err(|_| bad())?;
            i = bang + 1;
        }
        let mut release = vec![number(text, &mut i).ok_or_else(bad)?];
        while text.as_bytes().get(i) == Some(&b'.')
            && text.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit)
        {
            i += 1;
            release.push(number(text, &mut i).ok_or_else(bad)?);
        }
        let mut pre = None;
        let save = i;
        eat_separator(text, &mut i);
        let pre_words = ["a", "b", "c", "rc", "alpha", "beta", "pre", "preview"];
        if let Some(word) = eat_word(text, &mut i, &pre_words) {
            let rank = match pre_words[word] {
                "a" | "alpha" => 0,
                "b" | "beta" => 1,
                _ => 2,
            };
            eat_separator(text, &mut i);
            pre = Some((rank, number(text, &mut i).unwrap_or(0)));
        } else {
            i = save;
        }
        let mut post = None;
        let save = i;
        if text.as_bytes().get(i) == Some(&b'-')
            && text.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit)
        {
            i += 1;
            post = number(text, &mut i);
        } else {
            eat_separator(text, &mut i);
            if eat_word(text, &mut i, &["post", "rev", "r"]).is_some() {
                eat_separator(text, &mut i);
                post = Some(number(text, &mut i).unwrap_or(0));
            } else {
                i = save;
            }
        }
        let mut dev = None;
        let save = i;
        eat_separator(text, &mut i);
        if eat_word(text, &mut i, &["dev"]).is_some() {
            eat_separator(text, &mut i);
            dev = Some(number(text, &mut i).unwrap_or(0));
        } else {
            i = save;
        }
        let mut local = Vec::new();
        if text.as_bytes().get(i) == Some(&b'+') {
            let label = &text[i + 1..];
            if label.is_empty() {
                return Err(bad());
            }
            for part in label.split(['.', '-', '_']) {
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return Err(bad());
                }
                local.push(match part.parse::<u64>() {
                    Ok(n) if part.bytes().all(|b| b.is_ascii_digit()) => LocalPart::Number(n),
                    _ => LocalPart::Text(part.to_string()),
                });
            }
            i = text.len();
        }
        if i != text.len() {
            return Err(bad());
        }
        Ok(Version {
            epoch,
            release,
            pre,
            post,
            dev,
            local,
        })
    }

    #[allow(clippy::type_complexity)]
    fn key(
        &self,
    ) -> (
        u64,
        Vec<u64>,
        (u8, u64),
        (u8, u64),
        (u8, u64),
        (u8, Vec<LocalPart>),
    ) {
        let mut release = self.release.clone();
        while release.len() > 1 && release.last() == Some(&0) {
            release.pop();
        }
        // pre: a dev-only release sorts before every pre-release; a final
        // release sorts after them.
        let pre = match (self.pre, self.post, self.dev) {
            (None, None, Some(_)) => (0, 0),
            (None, _, _) => (4, 0),
            (Some((rank, n)), _, _) => (rank + 1, n),
        };
        let post = self.post.map_or((0, 0), |n| (1, n));
        let dev = self.dev.map_or((1, 0), |n| (0, n));
        let local = if self.local.is_empty() {
            (0, Vec::new())
        } else {
            (1, self.local.clone())
        };
        (self.epoch, release, pre, post, dev, local)
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key().cmp(&other.key())
    }
}

/// `resolved` within `[minimum, maximum]`; an unparseable version is an
/// error, never a pass.
pub fn in_range(resolved: &str, minimum: &str, maximum: Option<&str>) -> Result<bool, String> {
    let resolved = Version::parse(resolved)?;
    if resolved < Version::parse(minimum)? {
        return Ok(false);
    }
    if let Some(maximum) = maximum {
        if resolved > Version::parse(maximum)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_and_refuses_as_packaging_does() {
        let ordered = [
            "1.0.dev1",
            "1.0a1",
            "1.0b2",
            "1.0rc1",
            "1.0",
            "1.0.post1",
            "1.1",
            "2!0.1",
        ];
        for pair in ordered.windows(2) {
            assert!(
                Version::parse(pair[0]).unwrap() < Version::parse(pair[1]).unwrap(),
                "{pair:?}"
            );
        }
        assert_eq!(
            Version::parse("1.0.0").unwrap(),
            Version::parse("1.0").unwrap()
        );
        assert_eq!(
            Version::parse("1.0")
                .unwrap()
                .cmp(&Version::parse("v1.0.0").unwrap()),
            Ordering::Equal
        );
        assert!(Version::parse("0.2.0rc1").is_ok());
        for bad in ["", "abc", "1.0-", "1..0", "1.0+", "1.0 beta"] {
            assert_eq!(Version::parse(bad), Err(invalid(bad)), "{bad:?}");
        }
        assert_eq!(in_range("1.1.0", "1.0.0", Some("2.0.0")), Ok(true));
        assert_eq!(in_range("0.4.3", "0.0.1", Some("0.0.2")), Ok(false));
    }
}

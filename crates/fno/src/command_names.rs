//! The names the `fno` front door answers, read from `command-names.tsv`.
//!
//! The front door asks [`root_refusal`] before any classifier runs, and
//! `fno doctor lint menu-caps` counts [`live_paths`]. One table feeds both,
//! so a root name cannot answer without being counted.

const TABLE: &str = include_str!("../command-names.tsv");

/// Every row as (path, live, replacement, note). A malformed row panics: the
/// table is compiled in, so the test below fails before it ships.
fn rows() -> impl Iterator<Item = (&'static str, bool, &'static str, &'static str)> {
    TABLE
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| match line.split('\t').collect::<Vec<_>>()[..] {
            [path, state @ ("live" | "retired"), new, note] => (path, state == "live", new, note),
            _ => panic!("command-names.tsv: want path, live|retired, replacement, note: {line:?}"),
        })
}

/// The live paths, each as its words.
pub fn live_paths() -> Vec<Vec<&'static str>> {
    rows()
        .filter(|r| r.1)
        .map(|r| r.0.split(' ').collect())
        .collect()
}

/// The live root names, in file order.
pub fn live_roots() -> Vec<&'static str> {
    rows()
        .filter(|r| r.1 && !r.0.contains(' '))
        .map(|r| r.0)
        .collect()
}

/// The one stderr line for a root word the front door does not answer, or
/// None when the word is live. A flag is not a name, so it passes.
pub fn root_refusal(args: &[std::ffi::OsString]) -> Option<String> {
    let word = args.first()?.to_string_lossy();
    if word.starts_with('-') {
        return None;
    }
    Some(match rows().find(|r| r.0 == word) {
        Some((_, true, _, _)) => return None,
        Some((_, false, "-", note)) => format!("fno {word} was removed: {note}"),
        Some((_, false, new, _)) => format!("fno {word} is now fno {new}"),
        None => format!(
            "fno: no command `{word}`. Commands: {}. Run fno --help.",
            live_roots().join(", ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(words: &[&str]) -> Option<String> {
        root_refusal(
            &words
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn command_names_refuse_retired_and_unknown_root_words() {
        // The root holds twelve live names (d-aef0ed7b), and each passes.
        assert_eq!(live_roots().len(), 12, "{:?}", live_roots());
        assert!(live_roots().iter().all(|w| refusal(&[w, "x"]).is_none()));
        assert!(refusal(&[]).is_none() && refusal(&["--session", "work"]).is_none());
        // A retired word names its new spelling; a removed one says why;
        // an unlisted one names the live roots.
        let review = refusal(&["review", "lane"]);
        assert_eq!(review.as_deref(), Some("fno review is now fno do review"));
        assert!(refusal(&["codemap"]).unwrap().contains("was removed"));
        assert!(refusal(&["frobnicate"])
            .unwrap()
            .contains("Commands: agents"));
        // Every retired root points under a live root, or at a flag.
        for (path, _, new, _) in rows().filter(|r| !r.1 && !r.0.contains(' ') && r.2 != "-") {
            let head = new.split(' ').next().unwrap();
            assert!(
                head.starts_with('-') || live_roots().contains(&head),
                "{path} -> {new}"
            );
        }
    }
}

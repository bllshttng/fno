//! The names the `fno` front door answers, read from `command-names.tsv`.
//!
//! The front door asks [`root_refusal`] before any classifier runs, and
//! `fno doctor lint menu-caps` counts [`live_paths`]. One table feeds both,
//! so a root name cannot answer without being counted.

const TABLE: &str = include_str!("../command-names.tsv");

/// One table row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: Vec<&'static str>,
    pub live: bool,
    pub replacement: Option<&'static str>,
    pub note: &'static str,
}

/// Every row, in file order. A malformed row panics: the table is compiled
/// in, and `rows_parse` below fails `cargo test` before it ships.
pub fn rows() -> Vec<Row> {
    TABLE
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let cols: Vec<&'static str> = line.split('\t').collect();
            assert!(
                cols.len() == 4,
                "command-names.tsv: not 4 columns: {line:?}"
            );
            let live = match cols[1] {
                "live" => true,
                "retired" => false,
                other => panic!("command-names.tsv: bad state {other:?} in {line:?}"),
            };
            Row {
                path: cols[0].split(' ').collect(),
                live,
                replacement: (cols[2] != "-").then_some(cols[2]),
                note: cols[3],
            }
        })
        .collect()
}

/// The live paths, each as its words.
pub fn live_paths() -> Vec<Vec<&'static str>> {
    rows()
        .into_iter()
        .filter(|r| r.live)
        .map(|r| r.path)
        .collect()
}

/// The live root names, sorted.
pub fn live_roots() -> Vec<&'static str> {
    let mut roots: Vec<&str> = rows()
        .into_iter()
        .filter(|r| r.live && r.path.len() == 1)
        .map(|r| r.path[0])
        .collect();
    roots.sort_unstable();
    roots
}

/// The one stderr line for a root word the front door does not answer, or
/// None when the word is live. Flags pass: `--help`, `--version`, `-J` and
/// the mux attach flags are not names.
pub fn root_refusal(args: &[std::ffi::OsString]) -> Option<String> {
    let word = args.first()?.to_string_lossy();
    if word.starts_with('-') {
        return None;
    }
    let rows = rows();
    match rows.iter().find(|r| r.path.len() == 1 && r.path[0] == word) {
        Some(row) if row.live => None,
        Some(Row {
            replacement: Some(new),
            ..
        }) => Some(format!("fno {word} is now fno {new}")),
        Some(row) => Some(format!("fno {word} was removed: {}", row.note)),
        None => Some(format!(
            "fno: no command `{word}`. Commands: {}. Run fno --help.",
            live_roots().join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn command_names_refuse_retired_and_unknown_root_words() {
        // The root holds twelve live names (d-aef0ed7b).
        assert_eq!(live_roots().len(), 12, "{:?}", live_roots());
        for live in live_roots() {
            assert_eq!(root_refusal(&argv(&[live, "x"])), None, "{live}");
        }
        assert_eq!(root_refusal(&argv(&[])), None);
        assert_eq!(root_refusal(&argv(&["--help"])), None);
        assert_eq!(root_refusal(&argv(&["--session", "work"])), None);
        // A retired word names its new spelling; a removed one says why.
        assert_eq!(
            root_refusal(&argv(&["review", "lane"])).as_deref(),
            Some("fno review is now fno do review")
        );
        assert!(root_refusal(&argv(&["codemap"]))
            .unwrap()
            .contains("was removed"));
        // An unlisted word names the live roots.
        let unknown = root_refusal(&argv(&["frobnicate"])).unwrap();
        assert!(unknown.contains("no command `frobnicate`") && unknown.contains("agents"));
        // Every retired root names a live root as its new home.
        let roots = live_roots();
        for row in rows().iter().filter(|r| !r.live && r.path.len() == 1) {
            if let Some(new) = row.replacement.filter(|n| !n.starts_with('-')) {
                let head = new.split(' ').next().unwrap();
                assert!(roots.contains(&head), "{:?} -> {new}", row.path);
            }
        }
    }
}

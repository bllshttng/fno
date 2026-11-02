//! The refusal a removed `fno backlog` command answers with. `tombstones.toml`
//! keys removed command paths, longest prefix wins, so one entry covers a whole
//! removed subtree. A name nobody removed is a plain unknown command.

const TABLE: &str = include_str!("../tombstones.toml");

/// `annotate` retired leaf by leaf: each names its own replacement.
const ANNOTATE_RETIRED: [(&str, &str); 3] = [
    ("add", "fno backlog note <node> \"<text>\" --blocking"),
    ("list", "fno backlog notes findings [<node>]"),
    ("resolve", "fno backlog note --resolve <finding-id>"),
];

fn longest_prefix(table: &toml::Table, args: &[String]) -> Option<(String, String)> {
    (1..=args.len()).rev().find_map(|n| {
        let path = format!("backlog {}", args[..n].join(" "));
        let replacement = table.get(&path)?.as_str()?;
        Some((path, replacement.to_string()))
    })
}

fn refusal_text(args: &[String]) -> String {
    let table: toml::Table = TABLE.parse().unwrap_or_default();
    match longest_prefix(&table, args) {
        Some((removed, replacement)) => {
            // "Use nothing - ..." is not a sentence: a capability that simply
            // went away says so directly.
            let lead = if replacement.starts_with("nothing") {
                ""
            } else {
                "Use "
            };
            format!("Error: `fno {removed}` was removed. {lead}{replacement}.")
        }
        None => format!(
            "Error: No such command '{}'. Run `fno backlog --help`.",
            args.first().map(String::as_str).unwrap_or("")
        ),
    }
}

/// The exit-2 answer for a word no arm owns.
pub(super) fn refuse(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("annotate") {
        let leaf = args.get(1).map(String::as_str);
        if let Some((_, use_this)) = ANNOTATE_RETIRED.iter().find(|(l, _)| Some(*l) == leaf) {
            eprintln!("annotate is retired: use {use_this}");
            return 2;
        }
    }
    eprintln!("{}", refusal_text(args));
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_removed_name_teaches_its_replacement_and_an_unknown_name_does_not() {
        let release = refusal_text(&argv(&["release", "x-1"]));
        assert!(
            release.contains("`fno backlog release` was removed. Use `fno backlog unclaim`"),
            "{release}"
        );
        let gone = refusal_text(&argv(&["schedule"]));
        assert!(gone.contains("was removed. nothing - "), "{gone}");
        let unknown = refusal_text(&argv(&["frobnicate", "x-1"]));
        assert!(
            unknown.contains("No such command 'frobnicate'"),
            "{unknown}"
        );
    }
}

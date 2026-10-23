//! `fno doctor lint menu-caps`: the first three levels of every fno binary
//! hold at most twelve commands each, and a hidden command counts. Hiding is not
//! folding, so the guard never reads a visibility cell.
//!
//! It is a pure read over committed files, so it needs no Python and no
//! running binary:
//! - `scripts/ci/command-collapse-map.tsv`: every leaf of a collapsed Python
//!   group, hidden included (the command ratchet refuses drift from the code);
//! - `scripts/ci/command-baseline.txt`: every other Python leaf (`inbox` and
//!   `whoami` are in no collapsed group, so the map alone misses them);
//! - `scripts/ci/native-command-tree.txt`: the typed clap tree;
//! - `cli/src/fno/command_moves.py`: a moved root spelling is a shim, so its
//!   whole subtree is dropped;
//! - `crates/fno-agents/src/bin/client.rs`: the fno-agents binary's own
//!   top-level commands, which count the same as fno's;
//! - and, in-process, the `agents` table in `agents_menu`. Its group is
//!   counted from the table, and the flat Python registrations it forwards
//!   to are the `(python compat)` leg.
//!
//! `scripts/ci/menu-caps-debt.tsv` names the groups that are over today. It
//! is shrink-only: a group over the cap needs a row, may not pass its row's
//! ceiling, may not sit below it (lower the row in the same change), and a
//! row for a group back at twelve or under must go.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

/// The one cap, at the root, the second level and the third.
pub const MENU_CAP: usize = 12;

const MAP_REL: &str = "scripts/ci/command-collapse-map.tsv";
const BASELINE_REL: &str = "scripts/ci/command-baseline.txt";
const NATIVE_REL: &str = "scripts/ci/native-command-tree.txt";
const MOVES_REL: &str = "cli/src/fno/command_moves.py";
const AGENTS_SOURCE_REL: &str = "crates/fno-agents/src/bin/client.rs";
pub const DEBT_REL: &str = "scripts/ci/menu-caps-debt.tsv";
const COMPAT: &str = "(python compat)";

/// The committed files the guard reads, as text.
pub struct Inputs {
    pub collapse_map: String,
    pub baseline: String,
    pub native_tree: String,
    pub command_moves: String,
    pub agents_source: String,
    pub debt: String,
}

/// Read every input under `root`; the error names the first missing file.
pub fn read_inputs(root: &Path) -> Result<Inputs, String> {
    let read = |rel: &str| {
        std::fs::read_to_string(root.join(rel))
            .map_err(|e| format!("menu-caps: cannot read {rel} under {}: {e}", root.display()))
    };
    Ok(Inputs {
        collapse_map: read(MAP_REL)?,
        baseline: read(BASELINE_REL)?,
        native_tree: read(NATIVE_REL)?,
        command_moves: read(MOVES_REL)?,
        agents_source: read(AGENTS_SOURCE_REL)?,
        debt: read(DEBT_REL)?,
    })
}

/// The root names `COMMAND_MOVES` holds. Zero keys is an error: an empty set
/// would count every shim as a root.
fn moved_roots(command_moves: &str) -> Result<BTreeSet<String>, String> {
    let key = Regex::new(r#"(?m)^\s+"([a-z][a-z0-9-]*)": Move\("#).expect("static regex");
    let keys: BTreeSet<String> = key
        .captures_iter(command_moves)
        .map(|c| c[1].to_string())
        .collect();
    if keys.is_empty() {
        return Err(format!(
            "menu-caps: {MOVES_REL} yielded no COMMAND_MOVES keys"
        ));
    }
    Ok(keys)
}

fn words(path: &str) -> Vec<String> {
    path.split_whitespace().map(str::to_string).collect()
}

/// Every Python leaf path, hidden included, with moved roots dropped.
fn python_paths(inputs: &Inputs, moved: &BTreeSet<String>) -> Vec<Vec<String>> {
    let from_map = inputs
        .collapse_map
        .lines()
        .skip(1)
        .filter_map(|line| line.split('\t').next())
        .map(words);
    let from_baseline = inputs
        .baseline
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| {
            line.split_whitespace()
                .filter(|w| !w.starts_with('!'))
                .map(str::to_string)
                .collect::<Vec<_>>()
        });
    from_map
        .chain(from_baseline)
        .filter(|p| !p.is_empty())
        // The baseline names the sibling binary as one leaf; it is its own tree.
        .filter(|p| p.as_slice() != ["fno-agents"])
        .filter(|p| !moved.contains(&p[0]))
        .collect()
}

fn native_paths(native_tree: &str) -> Vec<Vec<String>> {
    native_tree
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split('\t').next())
        .filter(|path| *path != "(root)")
        .map(words)
        .collect()
}

/// The `agents` group as the menu table declares it.
fn menu_paths() -> Vec<Vec<String>> {
    crate::agents_menu::walk(&crate::agents_menu::ROOT)
        .into_iter()
        .map(|(path, _)| {
            std::iter::once("agents".to_string())
                .chain(path.iter().map(|w| w.to_string()))
                .collect()
        })
        .collect()
}

/// The source lines with `//` comments removed: doc comments narrate commands
/// that no arm dispatches.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn braced_region<'a>(text: &'a str, marker: &str) -> Result<&'a str, String> {
    let missing = || format!("menu-caps: {AGENTS_SOURCE_REL} has no dispatch marker {marker:?}");
    let start = text.find(marker).ok_or_else(missing)?;
    let opening = start + text[start..].find('{').ok_or_else(missing)?;
    let mut depth = 0usize;
    for (offset, ch) in text[opening..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&text[opening..=opening + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!(
        "menu-caps: unbalanced dispatch marker {marker:?} in {AGENTS_SOURCE_REL}"
    ))
}

/// The command literals the match opening at the first line dispatches
/// directly. Depth-scoped: a flag parser nested inside an arm spells words
/// that are not commands.
fn arms_at_top_level(lines: &[&str], arm: &Regex) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    let mut depth: i64 = 0;
    for (i, line) in lines.iter().enumerate() {
        if depth == 0 {
            commands.extend(arm.captures_iter(line).map(|c| c[1].to_string()));
        }
        depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
        if i == 0 {
            depth = (depth - 1).max(0);
        }
    }
    commands
}

/// The fno-agents binary's top-level commands, read from its dispatcher.
fn fno_agents_commands(source: &str) -> Result<BTreeSet<String>, String> {
    let text = strip_comments(source);
    let arm = Regex::new(r#"(?:Some\(\s*)?"([a-z][a-z0-9-]*)"\s*(?:\)\s*)?(?:\||=>)"#)
        .expect("static regex");
    let literal = Regex::new(r#""([a-z][a-z0-9-]*)""#).expect("static regex");
    let mut commands = BTreeSet::new();
    let run = braced_region(&text, "async fn run(args:")?;
    let if_command = Regex::new(r#"\bif command == "([a-z][a-z0-9-]*)""#).expect("static regex");
    commands.extend(if_command.captures_iter(run).map(|c| c[1].to_string()));
    let matches = Regex::new(r"matches!\(\s*command\s*,([^)]*)\)").expect("static regex");
    for alternatives in matches.captures_iter(run) {
        commands.extend(
            literal
                .captures_iter(&alternatives[1])
                .map(|c| c[1].to_string()),
        );
    }
    let retired = braced_region(&text, "fn retired_command_pointer(command:")?;
    commands.extend(arm.captures_iter(retired).map(|c| c[1].to_string()));
    let request = braced_region(&text, "let method = match command")?;
    let lines: Vec<&str> = request.lines().collect();
    commands.extend(arms_at_top_level(&lines, &arm));
    commands.insert("help".into());
    commands.insert("version".into());
    if !commands.contains("claim") || !commands.contains("detect") {
        return Err(format!(
            "menu-caps: the {AGENTS_SOURCE_REL} scan missed `claim` or `detect`; \
             a silent miss is the defect this scan exists to prevent"
        ));
    }
    Ok(commands)
}

/// How many distinct commands each group at the first three levels holds,
/// keyed by its printed path.
pub fn counts(inputs: &Inputs) -> Result<BTreeMap<String, usize>, String> {
    let moved = moved_roots(&inputs.command_moves)?;
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add = |path: &[String], from: usize, label: &dyn Fn(&[String]) -> String| {
        for i in from..path.len().min(3) {
            children
                .entry(label(&path[..i]))
                .or_default()
                .insert(path[i].clone());
        }
    };
    let plain = |parent: &[String]| {
        if parent.is_empty() {
            "(root)".to_string()
        } else {
            parent.join(" ")
        }
    };
    let compat = |parent: &[String]| format!("{} {COMPAT}", parent.join(" "));
    let menu = menu_paths();
    for path in &menu {
        add(path, 0, &plain);
    }
    for path in python_paths(inputs, &moved) {
        if path[0] == "agents" {
            add(&path, 1, &compat);
        } else {
            add(&path, 0, &plain);
        }
    }
    for path in native_paths(&inputs.native_tree) {
        add(&path, 0, &plain);
    }
    for command in fno_agents_commands(&inputs.agents_source)? {
        add(&[command], 0, &|_: &[String]| "fno-agents".to_string());
    }
    Ok(children.into_iter().map(|(k, v)| (k, v.len())).collect())
}

/// One debt row: the group's printed path and the count it may not pass.
fn debt_rows(text: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut rows = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let ceiling = cols.get(1).and_then(|c| c.trim().parse::<usize>().ok());
        match (cols.first(), ceiling) {
            (Some(path), Some(ceiling)) if !path.is_empty() => {
                rows.insert(path.to_string(), ceiling);
            }
            _ => {
                return Err(format!(
                    "menu-caps: {DEBT_REL}:{} is not `path<TAB>ceiling<TAB>note`",
                    n + 1
                ))
            }
        }
    }
    Ok(rows)
}

/// The verdict over the counts and the debt file: the ok line, or every
/// refusal, each naming the group and the fix.
pub fn check(counts: &BTreeMap<String, usize>, debt: &str) -> Result<String, Vec<String>> {
    let rows = debt_rows(debt).map_err(|e| vec![e])?;
    let mut failures = Vec::new();
    for (path, &count) in counts {
        if count <= MENU_CAP {
            continue;
        }
        match rows.get(path) {
            None => failures.push(format!(
                "`{path}` holds {count} commands (cap {MENU_CAP}) and has no debt row. Fold its \
                 commands into nested groups; hiding a command does not lower the count. Only a \
                 group over the cap today may enter {DEBT_REL}, as `{path}\\t{count}\\t<note>`."
            )),
            Some(&ceiling) if count > ceiling => failures.push(format!(
                "`{path}` holds {count} commands, above its debt ceiling {ceiling} (cap \
                 {MENU_CAP}). Nest the new command under a group, or remove it."
            )),
            Some(&ceiling) if count < ceiling => failures.push(format!(
                "`{path}` holds {count} commands, below its debt ceiling {ceiling}. Lower its \
                 row in {DEBT_REL} to {count} in this change, so the slack is not banked."
            )),
            Some(_) => {}
        }
    }
    for path in rows.keys() {
        let count = counts.get(path).copied().unwrap_or(0);
        if count <= MENU_CAP {
            failures.push(format!(
                "`{path}` holds {count} commands, inside the cap of {MENU_CAP}. Delete its row \
                 from {DEBT_REL}."
            ));
        }
    }
    if !failures.is_empty() {
        return Err(failures);
    }
    let debt: Vec<String> = rows
        .keys()
        .map(|path| format!("{path} {}", counts[path]))
        .collect();
    Ok(format!(
        "menu-caps: ok (cap {MENU_CAP} at levels 1-3; {} groups in debt: {})",
        debt.len(),
        if debt.is_empty() {
            "none".to_string()
        } else {
            debt.join(", ")
        }
    ))
}

const USAGE: &str = "usage: fno doctor lint menu-caps

Counts every command, hidden included, at the first three levels of fno and
fno-agents. Cap 12. Groups over the cap today are named in
scripts/ci/menu-caps-debt.tsv, which may only shrink.
Exit 0 ok, 1 a group over its cap or ceiling, 2 an input is missing.";

/// The command: exit 0 ok, 1 refused, 2 unreadable input.
pub fn run(args: &[std::ffi::OsString]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    if let Some(extra) = args.first() {
        eprintln!("menu-caps: unexpected argument {:?}\n{USAGE}", extra);
        return 2;
    }
    let root = crate::setup_run::repo_root();
    let inputs = match read_inputs(&root) {
        Ok(inputs) => inputs,
        Err(line) => {
            eprintln!("{line}");
            return 2;
        }
    };
    let counts = match counts(&inputs) {
        Ok(counts) => counts,
        Err(line) => {
            eprintln!("{line}");
            return 2;
        }
    };
    match check(&counts, &inputs.debt) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(failures) => {
            eprintln!("menu-caps: FAIL");
            for f in failures {
                eprintln!("  {f}");
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(groups: &[(&str, usize)]) -> BTreeMap<String, usize> {
        groups.iter().map(|(p, n)| (p.to_string(), *n)).collect()
    }

    #[test]
    fn menu_caps_refuses_every_debt_drift_and_counts_hidden_leaves() {
        // A group at the cap passes with no row; one more command, hidden or
        // not, refuses by name with the count and the cap (AC2).
        assert!(check(&tree(&[("do", 12)]), "").is_ok());
        let over = check(&tree(&[("do", 13)]), "").unwrap_err();
        assert!(
            over[0].contains("`do` holds 13") && over[0].contains("cap 12"),
            "{over:?}"
        );
        // Above, below, and inside a ceiling each refuse with the fix (AC3).
        let above = check(&tree(&[("mux", 22)]), "mux\t21\tfold\n").unwrap_err();
        assert!(above[0].contains("above its debt ceiling 21"), "{above:?}");
        let below = check(&tree(&[("mux", 20)]), "mux\t21\tfold\n").unwrap_err();
        assert!(
            below[0].contains("Lower its row") && below[0].contains("to 20"),
            "{below:?}"
        );
        let inside = check(&tree(&[("mux", 9)]), "# c\nmux\t21\tfold\n").unwrap_err();
        assert!(inside[0].contains("Delete its row"), "{inside:?}");
        assert!(check(&tree(&[]), "mux\tlots\n").is_err());
        // The ok line names the debt, so ok never reads as clean.
        let ok = check(&tree(&[("mux", 21), ("do", 3)]), "mux\t21\tfold\n").unwrap();
        assert!(ok.contains("1 groups in debt: mux 21"), "{ok}");

        // The guard never reads a visibility cell: a hidden native row
        // counts like a visible one, and a moved root's subtree is dropped.
        let mut native =
            String::from("# header\n(root)\troot\tvisible\t-\t-\nmux\tgroup\tvisible\t-\t-\n");
        for i in 0..13 {
            let vis = if i % 2 == 0 { "hidden" } else { "visible" };
            native.push_str(&format!("mux v{i}\tleaf\t{vis}\t-\t-\n"));
        }
        let inputs = Inputs {
            collapse_map: "current-leaf\ttier\nlead board\tT1\nwhoami x\tT1\n".into(),
            baseline: "# c\ninbox a !--hid\nfno-agents\n".into(),
            native_tree: native,
            command_moves: "COMMAND_MOVES = {\n    \"lead\": Move(\n".into(),
            agents_source: "async fn run(args: X) { if command == \"claim\" {} if matches!(command, \"detect\" | \"top\") {} }\n\
                 fn retired_command_pointer(command: &str) { match command { \"gone\" => 1, _ => 0 } }\n\
                 let method = match command {\n    \"rows\" => {\n        match x { \"inner\" => 1 }\n    }\n}\n"
                .into(),
            debt: String::new(),
        };
        let got = counts(&inputs).unwrap();
        assert_eq!(got["mux"], 13);
        assert_eq!(got.get("lead"), None);
        assert_eq!(got["inbox"], 1);
        assert_eq!(got["fno-agents"], 7, "{got:?}");
        assert!(got["(root)"] >= 4, "agents, inbox, mux, whoami: {got:?}");
        assert!(got.contains_key("agents fleet"));
    }

    #[test]
    fn menu_caps_holds_on_the_committed_tree() {
        // The CI gate: the real files, the real table, the real debt.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let inputs = read_inputs(&root).expect("inputs readable");
        let counts = counts(&inputs).expect("inputs parse");
        if let Err(failures) = check(&counts, &inputs.debt) {
            panic!("menu-caps: FAIL\n  {}", failures.join("\n  "));
        }
    }
}

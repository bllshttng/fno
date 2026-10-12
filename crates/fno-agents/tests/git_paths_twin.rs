//! `fno::git_paths` and `fno_agents::git_paths` are two copies of one reader
//! (this crate links `fno` as a dev-dependency only). Both must answer every
//! stdout shape git has produced, including the old-git echo of the flag.

use std::path::{Path, PathBuf};

/// (absolute-flag stdout, plain stdout, expected common dir)
type Case = (
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
);

const CASES: [Case; 7] = [
    (Some("/work/repo/.git\n"), None, Some("/work/repo/.git")),
    (
        Some("--path-format=absolute\n.git\n"),
        Some(".git\n"),
        Some("/work/repo/.git"),
    ),
    (
        Some("--path-format=absolute\n/work/repo/.git\n"),
        Some("/work/repo/.git\n"),
        Some("/work/repo/.git"),
    ),
    (
        Some("--path-format=absolute\n.\n"),
        Some(".\n"),
        Some("/work/repo"),
    ),
    (None, None, None),
    (Some(".git\n"), Some(".git\n"), Some("/work/repo/.git")),
    (Some(""), Some("\n"), None),
];

fn answer(abs: Option<&str>, plain: Option<&str>, args: &[&str]) -> Option<String> {
    let out = if args[0] == "--path-format=absolute" {
        abs
    } else {
        plain
    };
    out.map(str::to_string)
}

#[test]
fn both_readers_answer_every_git_shape_alike() {
    let cwd = Path::new("/work/repo");
    for (abs, plain, expected) in CASES {
        let expected = expected.map(PathBuf::from);
        let from_fno = fno::git_paths::common_dir_with(cwd, |a| answer(abs, plain, a));
        let from_agents = fno_agents::git_paths::common_dir_with(cwd, |a| answer(abs, plain, a));
        assert_eq!(from_fno, expected, "fno reader on {abs:?} / {plain:?}");
        assert_eq!(
            from_agents, expected,
            "fno-agents reader on {abs:?} / {plain:?}"
        );
    }
}

//! The folder a workspace owns. A new workspace has no folder of its own, so
//! the rows under it have nothing to join by. The create overlay offers the
//! project roots of the live rows instead, and a workspace section's Add
//! folder picks from the same list.

use super::*;

/// What the create overlay's folder field holds. `NoFolder` is the default:
/// the folder is optional, and a workspace without one is joined by an explicit
/// move only. `Viewed` copies the workspace being viewed now. `Live` indexes
/// the live-row folder list.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum FolderChoice {
    #[default]
    NoFolder,
    Viewed,
    Live(usize),
}

/// Which folder the create overlay will send. While `picked` is false the
/// folder follows the typed name; the arrow keys set it. `path` is the folder
/// field while the operator types a path, and it wins over the pick.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct FolderPick {
    pub(super) picked: bool,
    pub(super) choice: FolderChoice,
    pub(super) path: Option<String>,
}

/// The `origin` a create sends. `Viewed` is an absent origin, `NoFolder` is
/// the wire's empty-string marker, and `Folder` carries an absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CreateOrigin {
    Viewed,
    NoFolder,
    Folder(String),
}

impl CreateOrigin {
    pub(super) fn wire(self) -> Option<String> {
        match self {
            CreateOrigin::Viewed => None,
            CreateOrigin::NoFolder => Some(String::new()),
            CreateOrigin::Folder(p) => Some(p),
        }
    }
}

/// `~` and `~/x` to the home directory; every other path unchanged.
fn expand_tilde(raw: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match raw.strip_prefix('~') {
        Some("") if !home.is_empty() => home,
        Some(rest) if rest.starts_with('/') && !home.is_empty() => format!("{home}{rest}"),
        _ => raw.to_string(),
    }
}

/// A typed folder as an absolute directory path, or the reason it is not one.
pub(super) fn resolve_typed_folder(raw: &str) -> Result<String, String> {
    let expanded = expand_tilde(raw.trim());
    if !expanded.starts_with('/') {
        return Err(format!(
            "folder must be an absolute path or start with ~: {raw}"
        ));
    }
    let trimmed = expanded.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    if std::path::Path::new(path).is_dir() {
        Ok(path.to_string())
    } else {
        Err(format!("no such folder: {raw}"))
    }
}

/// Tab-complete the last segment of `typed` against the directories on disk:
/// a unique match completes with a trailing `/`, several extend to their common
/// prefix, none leaves the text alone. A leading `~` stays as typed.
pub(super) fn complete_dir(typed: &str) -> String {
    let (dir_part, stem) = match typed.rfind('/') {
        Some(i) => (&typed[..=i], &typed[i + 1..]),
        None => return typed.to_string(),
    };
    let Ok(entries) = std::fs::read_dir(expand_tilde(dir_part)) else {
        return typed.to_string();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(stem) && (stem.starts_with('.') || !n.starts_with('.')))
        .collect();
    names.sort();
    let Some(first) = names.first() else {
        return typed.to_string();
    };
    if names.len() == 1 {
        return format!("{dir_part}{first}/");
    }
    let mut common = first.clone();
    for n in &names[1..] {
        let keep = common
            .chars()
            .zip(n.chars())
            .take_while(|(a, b)| a == b)
            .count();
        common = common.chars().take(keep).collect();
    }
    format!("{dir_part}{common}")
}

/// The entry whose last path segment equals `name`, case-insensitively.
fn folder_for_name(folders: &[String], name: &str) -> Option<usize> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    folders.iter().position(|f| {
        f.trim_end_matches('/')
            .rsplit('/')
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case(name))
    })
}

/// The label the move picker draws for destination `idx` of a folder pick.
/// `None` for any other source, which labels by workspace name. A long path
/// keeps its tail, which is the part that tells two folders apart.
pub(super) fn folder_label(src: &MoveSrc, idx: u64) -> Option<String> {
    let MoveSrc::Folder { folders, .. } = src else {
        return None;
    };
    let path = folders.get(idx as usize).map_or("(gone)", String::as_str);
    let shown = abbrev_home(path);
    let n = shown.chars().count();
    Some(if n > 32 {
        let tail: String = shown.chars().skip(n - 31).collect();
        format!("…{tail}")
    } else {
        shown
    })
}

impl View {
    /// The distinct project roots of live rows, most rows first, then by path.
    pub(super) fn live_row_folders(&self) -> Vec<String> {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for a in self.layout.agents.iter().filter(|a| !a.exited) {
            if let Some(root) = a.project_root.as_deref().filter(|r| !r.is_empty()) {
                *counts.entry(root).or_default() += 1;
            }
        }
        let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        ranked
            .into_iter()
            .map(|(root, _)| root.to_string())
            .collect()
    }

    /// The viewed workspace's name and folder, for the default slot's label.
    fn viewed_workspace(&self) -> (String, String) {
        self.layout
            .squads
            .iter()
            .find(|s| s.id == self.layout.active_squad)
            .map(|s| (s.name.clone(), s.canonical_cwd.clone()))
            .unwrap_or_default()
    }

    fn create_choice(&self, name: &str, folders: &[String]) -> FolderChoice {
        if self.folder_pick.picked {
            self.folder_pick.choice
        } else {
            folder_for_name(folders, name).map_or(FolderChoice::NoFolder, FolderChoice::Live)
        }
    }

    /// The folder the create overlay sends for the typed `name`. A typed path
    /// wins over the arrow pick; a path that is not a directory is an `Err`
    /// the Enter handler shows as a notice.
    pub(super) fn create_folder(&self, name: &str) -> Result<CreateOrigin, String> {
        if let Some(path) = &self.folder_pick.path {
            return resolve_typed_folder(path).map(CreateOrigin::Folder);
        }
        let folders = self.live_row_folders();
        Ok(match self.create_choice(name, &folders) {
            FolderChoice::Viewed => CreateOrigin::Viewed,
            FolderChoice::NoFolder => CreateOrigin::NoFolder,
            FolderChoice::Live(i) => folders
                .get(i)
                .cloned()
                .map_or(CreateOrigin::Viewed, CreateOrigin::Folder),
        })
    }

    /// Up (`forward` false) or Down in the create overlay: step through no
    /// folder, the viewed workspace's folder, and each live-row folder, and stop following
    /// the name.
    pub(super) fn cycle_create_folder(&mut self, name: &str, forward: bool) {
        let folders = self.live_row_folders();
        let slots = folders.len() + 2;
        let slot = match self.create_choice(name, &folders) {
            FolderChoice::NoFolder => 0,
            FolderChoice::Viewed => 1,
            FolderChoice::Live(i) => i + 2,
        };
        let next = (slot + if forward { 1 } else { slots - 1 }) % slots;
        self.folder_pick = FolderPick {
            picked: true,
            choice: match next {
                0 => FolderChoice::NoFolder,
                1 => FolderChoice::Viewed,
                i => FolderChoice::Live(i - 2),
            },
            path: None,
        };
    }

    /// Tab in the create overlay: start typing a folder, seeded with the
    /// current pick (or `~/`). A second Tab completes the typed path.
    pub(super) fn tab_create_folder(&mut self, name: &str) {
        match self.folder_pick.path.take() {
            Some(path) => self.folder_pick.path = Some(complete_dir(&path)),
            None => {
                let seed = match self.create_folder(name) {
                    Ok(CreateOrigin::Folder(p)) => abbrev_home(&p),
                    _ => "~/".to_string(),
                };
                self.folder_pick.path = Some(seed);
            }
        }
    }

    /// The create overlay's footer: the folder it will send.
    pub(super) fn create_hint(&self, name: &str) -> String {
        if let Some(path) = &self.folder_pick.path {
            return format!("folder (optional): {path}▏ tab completes, esc back");
        }
        let shown = match self.create_folder(name) {
            Ok(CreateOrigin::Folder(p)) => abbrev_home(&p),
            Ok(CreateOrigin::NoFolder) => "none".to_string(),
            Ok(CreateOrigin::Viewed) | Err(_) => {
                let (ws, folder) = self.viewed_workspace();
                let folder = if folder.is_empty() {
                    "no folder".to_string()
                } else {
                    abbrev_home(&folder)
                };
                format!("same as {ws} ({folder})")
            }
        };
        format!("folder (optional): {shown} ↑↓ tab to type")
    }

    /// A workspace section's Add folder: the picker of live-row folders.
    pub(super) fn open_add_folder(&mut self, squad: u64) {
        let folders = self.live_row_folders();
        if folders.is_empty() {
            self.set_notice("no live row has a folder to add".into());
            return;
        }
        let picks: Vec<u64> = (0..folders.len() as u64).collect();
        self.open_move_pick(MoveSrc::Folder { squad, folders }, picks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> Vec<String> {
        vec!["/code/fnh".into(), "/code/fno-books".into()]
    }

    #[test]
    fn the_folder_follows_the_typed_name_until_the_arrows_pick_one() {
        let rows = folders();
        assert_eq!(folder_for_name(&rows, "fno-books"), Some(1));
        assert_eq!(folder_for_name(&rows, "  FNH "), Some(0));
        assert_eq!(folder_for_name(&rows, "books"), None);
        assert_eq!(folder_for_name(&rows, ""), None);

        let mut v = test_view_with_roots(&["/code/fnh", "/code/fno-books", "/code/fno-books"]);
        // Two rows under fno-books rank it first, so the list is reordered.
        assert_eq!(
            v.live_row_folders(),
            vec!["/code/fno-books".to_string(), "/code/fnh".to_string()]
        );
        let folder = |p: &str| Ok(CreateOrigin::Folder(p.to_string()));
        assert_eq!(v.create_folder("fno-books"), folder("/code/fno-books"));
        assert_eq!(v.create_folder("other"), Ok(CreateOrigin::NoFolder));
        assert_eq!(CreateOrigin::NoFolder.wire(), Some(String::new()));
        // Down from none lands on the viewed folder, then the first live
        // folder, and the pick sticks even when the typed name differs.
        v.cycle_create_folder("other", true);
        assert_eq!(v.create_folder("fnh"), Ok(CreateOrigin::Viewed));
        v.cycle_create_folder("fnh", true);
        assert_eq!(v.create_folder("fnh"), folder("/code/fno-books"));
        v.cycle_create_folder("fnh", true);
        v.cycle_create_folder("fnh", true);
        assert_eq!(v.create_folder("fnh"), Ok(CreateOrigin::NoFolder));
        v.cycle_create_folder("fnh", false);
        assert_eq!(v.create_folder("fnh"), folder("/code/fnh"));
        assert!(v.create_hint("fnh").starts_with("folder (optional): "));
    }

    #[test]
    fn a_typed_path_must_be_a_folder_and_tab_completes_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        std::fs::create_dir_all(dir.path().join("alpha-one")).unwrap();
        std::fs::create_dir_all(dir.path().join("alpha-two")).unwrap();
        std::fs::create_dir_all(dir.path().join("beta")).unwrap();
        std::fs::write(dir.path().join("betamax"), "").unwrap();

        assert_eq!(complete_dir(&format!("{root}/be")), format!("{root}/beta/"));
        assert_eq!(
            complete_dir(&format!("{root}/al")),
            format!("{root}/alpha-")
        );
        assert_eq!(complete_dir(&format!("{root}/zz")), format!("{root}/zz"));

        assert_eq!(
            resolve_typed_folder(&format!("{root}/beta/")),
            Ok(format!("{root}/beta"))
        );
        assert!(resolve_typed_folder(&format!("{root}/betamax")).is_err());
        assert!(resolve_typed_folder("relative/path").is_err());

        let mut v = test_view_with_roots(&[]);
        v.folder_pick.path = Some(format!("{root}/beta"));
        assert_eq!(
            v.create_folder("x"),
            Ok(CreateOrigin::Folder(format!("{root}/beta")))
        );
        v.folder_pick.path = Some(format!("{root}/missing"));
        assert!(v.create_folder("x").is_err());
    }

    fn test_view_with_roots(roots: &[&str]) -> View {
        let mut v = crate::client::tests::view_with_agents(vec![]);
        v.layout.agents = roots
            .iter()
            .enumerate()
            .map(|(i, root)| AgentRow {
                name: format!("w{i}"),
                project_root: Some((*root).to_string()),
                ..Default::default()
            })
            .collect();
        v
    }
}

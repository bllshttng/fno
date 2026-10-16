//! The folder a workspace owns. A new workspace has no folder of its own, so
//! the rows under it have nothing to join by. The create overlay offers the
//! project roots of the live rows instead, and a workspace section's Add
//! folder picks from the same list.

use super::*;

/// Which folder the create overlay will send. While `picked` is false the
/// folder follows the typed name; the arrow keys set it, and `index` then
/// holds (`None` is the viewed workspace's folder, today's default).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FolderPick {
    pub(super) picked: bool,
    pub(super) index: Option<usize>,
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

    /// The folder the create overlay sends for the typed `name`: a live-row
    /// folder, or `None` for the viewed workspace's folder.
    pub(super) fn create_folder(&self, name: &str) -> Option<String> {
        let folders = self.live_row_folders();
        let index = if self.folder_pick.picked {
            self.folder_pick.index
        } else {
            folder_for_name(&folders, name)
        };
        index.and_then(|i| folders.get(i).cloned())
    }

    /// Up (`forward` false) or Down in the create overlay: step through the
    /// viewed-workspace default and each folder, and stop following the name.
    pub(super) fn cycle_create_folder(&mut self, name: &str, forward: bool) {
        let folders = self.live_row_folders();
        let slots = folders.len() + 1;
        let current = if self.folder_pick.picked {
            self.folder_pick.index
        } else {
            folder_for_name(&folders, name)
        };
        let slot = current.map_or(0, |i| i + 1);
        let next = (slot + if forward { 1 } else { slots - 1 }) % slots;
        self.folder_pick = FolderPick {
            picked: true,
            index: next.checked_sub(1),
        };
    }

    /// The create overlay's footer: the folder it will send.
    pub(super) fn create_hint(&self, name: &str) -> String {
        let shown = self
            .create_folder(name)
            .map_or_else(|| "viewed".to_string(), |p| abbrev_home(&p));
        format!("folder: {shown} ↑↓")
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
        assert_eq!(
            v.create_folder("fno-books").as_deref(),
            Some("/code/fno-books")
        );
        assert_eq!(
            v.create_folder("other"),
            None,
            "no match: the viewed folder"
        );
        // Down from "viewed" lands on the first entry, and the pick sticks
        // even when the typed name points elsewhere.
        v.cycle_create_folder("other", true);
        assert_eq!(v.create_folder("fnh").as_deref(), Some("/code/fno-books"));
        v.cycle_create_folder("fnh", true);
        v.cycle_create_folder("fnh", true);
        assert_eq!(v.create_folder("fnh"), None, "past the last entry: viewed");
        v.cycle_create_folder("fnh", false);
        assert_eq!(v.create_folder("fnh").as_deref(), Some("/code/fnh"));
        assert!(v.create_hint("fnh").starts_with("folder: "));
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

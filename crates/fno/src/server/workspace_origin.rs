//! A named workspace's folder. A paneless row (thread, headless, daemon)
//! joins a workspace by `Session::find_by_cwd` alone, so a workspace with no
//! origin owns no row and its workers render under `~ elsewhere`. This module
//! gives a workspace its folder at birth, lets the operator edit it later, and
//! words the refusal that names what holds a taken name.

use super::*;

impl Core {
    /// What holds `name`, worded for the refusal notice, or `None` when free.
    /// A stored row with no open workspace is the case the bare "name taken"
    /// hid: the operator saw no such workspace and could not tell why.
    pub(super) fn name_holder(&self, name: &str) -> Option<String> {
        let folder = |origins: &[String]| match origins.first() {
            Some(origin) => format!("folder {origin}"),
            None => "no folder".to_string(),
        };
        if let Some(sq) = self
            .session
            .squads
            .iter()
            .find(|s| s.name.as_deref() == Some(name))
        {
            return Some(format!("open, {}", folder(&sq.origins)));
        }
        crate::squad_store::load()
            .squads
            .iter()
            .find(|s| s.name == name)
            .map(|s| format!("stored, {}", folder(&s.origins)))
    }

    /// `Command::NewSquad`: create a named workspace. A blank name or a taken
    /// name is refused fail-closed, and nothing is created. The shell opens in
    /// `origin`, else where the sender is, and that folder is the workspace's
    /// first origin: the workspace owns the rows under the folder it was made
    /// from. PTY-first ordering: a spawn failure mutates no model.
    pub(super) fn handle_new_squad(
        &mut self,
        client_id: u64,
        view: (u64, TabId),
        vp: Rect,
        name: &str,
        origin: Option<String>,
    ) -> Flow {
        let name = name.trim();
        if name.is_empty() {
            self.notice(client_id, "name required");
            return Flow::Continue;
        }
        if let Some(holder) = self.name_holder(name) {
            self.notice(client_id, format!("name taken: {name} is {holder}"));
            return Flow::Continue;
        }
        let cwd = origin.unwrap_or_else(|| {
            self.session
                .squad(view.0)
                .map(|s| s.canonical_cwd().to_string())
                .unwrap_or_default()
        });
        let origins: Vec<String> = Some(cwd.clone())
            .filter(|c| !c.is_empty())
            .into_iter()
            .collect();
        let (rows, cols) = self
            .clients
            .iter()
            .find(|c| c.id == client_id)
            .map(|c| c.dims)
            .unwrap_or((vp.rows, vp.cols));
        let pid = match self.spawn_pane(rows, cols, &cwd) {
            Ok(p) => p,
            Err(e) => {
                self.notice(client_id, format!("new workspace failed: {e}"));
                return Flow::Continue;
            }
        };
        let sid = self.next_squad_id;
        self.next_squad_id += 1;
        let tid = self.session.mint_tab_id();
        self.session.add_squad(
            sid,
            origins,
            Some(name.to_string()),
            Tab {
                name: None,
                id: tid,
                root: Node::Leaf(pid),
                focus: pid,
            },
        );
        // Track + persist the new (empty-membership) workspace so it restores
        // next session even before any recruit.
        self.squad_members.insert(sid, Vec::new());
        self.persist_squad(sid);
        self.set_view(client_id, sid, tid);
        self.push_layout(true);
        Flow::Continue
    }

    /// `ControlVerb::WorkspaceOrigin`: add or drop one folder on the open
    /// workspace `workspace`. Adding a folder the workspace holds, or dropping
    /// one it lacks, changes nothing and still answers `Ok`. The edit reaches
    /// the store before the reply. If the write fails, the live origins revert
    /// and the reply is an error: an unstored edit would vanish at restart.
    pub(super) fn handle_workspace_origin(
        &mut self,
        workspace: &str,
        path: &str,
        remove: bool,
        reply: ControlReply,
    ) {
        let _ = reply.send(match self.apply_origin_edit(workspace, path, remove) {
            Ok(()) => ServerMsg::Ok,
            Err((code, msg)) => ServerMsg::Err { code, msg },
        });
    }

    /// `Command::AddWorkspaceFolder`: the section menu's Add folder. The same
    /// edit as `fno mux workspace origin add`, so it shares the core below.
    /// A refusal reaches the operator as a notice.
    pub(super) fn handle_add_workspace_folder(
        &mut self,
        client_id: u64,
        squad: u64,
        path: &str,
    ) -> Flow {
        let named = self
            .session
            .squad(squad)
            .and_then(|s| s.name.clone())
            .filter(|n| !n.is_empty());
        match named {
            Some(workspace) => {
                if let Err((_, msg)) = self.apply_origin_edit(&workspace, path, false) {
                    self.notice(client_id, msg);
                }
            }
            None => self.notice(client_id, "add folder needs a named workspace"),
        }
        Flow::Continue
    }

    /// The origin a workspace created by recruiting `ids` takes: the one
    /// project root every recruited row shares. Rows from mixed roots, or a
    /// row whose root is not yet resolved, give no origin, as before.
    pub(super) fn recruit_origin(&self, ids: &[String]) -> Vec<String> {
        let mut roots = HashSet::new();
        for id in ids {
            let root = self
                .agents
                .iter()
                .find(|a| a.attach_id.as_deref() == Some(id.as_str()))
                .and_then(|a| self.project_by_cwd.get(&a.cwd))
                .map(|p| p.root.clone());
            match root {
                Some(root) => roots.insert(root),
                None => return Vec::new(),
            };
        }
        if roots.len() == 1 {
            roots.into_iter().collect()
        } else {
            Vec::new()
        }
    }

    fn apply_origin_edit(
        &mut self,
        workspace: &str,
        path: &str,
        remove: bool,
    ) -> Result<(), (u32, String)> {
        let Some(sid) = self
            .session
            .squads
            .iter()
            .find(|s| s.name.as_deref() == Some(workspace))
            .map(|s| s.id)
        else {
            return Err((
                err_code::NOT_FOUND,
                format!("no open workspace named {workspace}"),
            ));
        };
        if !path.starts_with('/') {
            return Err((
                err_code::BAD_REQUEST,
                format!("origin must be an absolute path: {path}"),
            ));
        }
        let previous = self
            .session
            .squad(sid)
            .map(|s| s.origins.clone())
            .unwrap_or_default();
        let changed = self.session.squad_mut(sid).is_some_and(|sq| {
            if remove {
                sq.origins.retain(|o| o != path);
            } else if !sq.origins.iter().any(|o| o == path) {
                sq.origins.push(path.to_string());
            }
            sq.origins != previous
        });
        if changed {
            let origins = self
                .session
                .squad(sid)
                .map(|s| s.origins.clone())
                .unwrap_or_default();
            // Only the origins reach the store. A full snapshot would carry
            // this server's members and layout over a second server's newer ones.
            let stored = match crate::squad_store::set_origins(workspace, &origins) {
                Ok((true, batch)) => self.persist_result(Ok(batch)),
                // Never stored: a full snapshot has nothing to overwrite.
                Ok((false, _)) => {
                    self.persist_squad(sid);
                    true
                }
                Err(error) => self.persist_result(Err(error)),
            };
            if !stored {
                if let Some(sq) = self.session.squad_mut(sid) {
                    sq.origins = previous;
                }
                return Err((
                    err_code::STORE_WRITE_FAILED,
                    format!("origin edit on {workspace} was not stored; nothing changed"),
                ));
            }
            self.push_layout(true);
        }
        Ok(())
    }
}

//! `fno mux workspace`: the command family's dispatch, and its `origin` command.
//! A child module of `mux_cli`: the file budget gate keeps the parent
//! shrink-only, and `use super::*` keeps every helper in one place.

use super::*;

/// `fno mux workspace <command> ...`: the workspace-store maintenance family.
/// `prune`, `restore` and `origin` exist; a bare or unknown command is usage.
/// Tokens after the family command carry verbatim, like `pane`/`block`.
///
/// The retired `fno mux squad` spelling is GONE: an unadvertised alias named
/// by nothing outside its own test. `--squad` stays a hidden-deprecated flag
/// alias beside the canonical `--workspace`. The user-facing/internal
/// vocabulary split that leaves behind - `workspace` where a person types,
/// `squad` in this crate's identifiers - is a decision, not an unfinished
/// rename: renaming ~2900 internal sites buys no user-visible change. The
/// remaining user-adjacent `squad` spellings, the key in the `--json`
/// placement receipt and `~/.fno/squads.json`, ride the next change that
/// bumps `PROTO_VERSION` or migrates the store for a real reason.
pub fn workspace(
    op: crate::cli_args::WorkspaceOp,
    args: &[OsString],
    env_session: Option<&str>,
) -> i32 {
    match op {
        crate::cli_args::WorkspaceOp::Prune(_) => squad_prune(args),
        crate::cli_args::WorkspaceOp::Restore(_) => {
            super::restore::workspace_restore(args, env_session)
        }
        crate::cli_args::WorkspaceOp::Origin(_) => workspace_origin(args),
    }
}

const ORIGIN_USAGE: &str =
    "usage: fno mux workspace origin add|remove <name> <path> [--server <name>] [--json]";

/// An absolute, normalized folder from what the operator typed: `~` and
/// `~/x` expand to HOME, a relative path joins the current directory, and
/// `.`, `..` and a trailing slash fold away. No symlink is resolved: the
/// registry rows carry the cwd as their harness recorded it, and an origin
/// matches a row by that string.
fn origin_path(raw: &str) -> Result<String, String> {
    let expanded = if raw == "~" || raw.starts_with("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| "HOME is unset; spell the path in full".to_string())?;
        format!("{home}{}", &raw[1..])
    } else if raw.starts_with('/') {
        raw.to_string()
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?;
        cwd.join(raw).to_string_lossy().into_owned()
    };
    let mut out = PathBuf::new();
    for part in Path::new(&expanded).components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out.to_string_lossy().into_owned())
}

/// `fno mux workspace origin add|remove <name> <path>`: add or drop one
/// folder on an open named workspace. The folder decides which paneless rows
/// (thread, headless, daemon) the workspace owns, and a workspace with none
/// owns none. Every answering mux server that holds the name applies the edit
/// and persists it; `--server` addresses one.
fn workspace_origin(args: &[OsString]) -> i32 {
    let (common, rest) = match MuxCommon::take(args) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("fno mux workspace origin: {e}");
            return EXIT_USAGE;
        }
    };
    let (action, name, raw) = match rest.as_slice() {
        [action, name, raw]
            if (action == "add" || action == "remove")
                && !name.trim().is_empty()
                && !raw.trim().is_empty() =>
        {
            (action.as_str(), name.trim(), raw.as_str())
        }
        _ => {
            eprintln!("fno mux workspace origin: {ORIGIN_USAGE}");
            return EXIT_USAGE;
        }
    };
    let path = match origin_path(raw) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("fno mux workspace origin: {e}");
            return EXIT_USAGE;
        }
    };
    let sessions = match common.server.or(common.session) {
        Some(one) => vec![one],
        None => match session_names() {
            Ok(names) => names,
            Err(e) => {
                eprintln!("fno mux workspace origin: {e}");
                return EXIT_ERROR;
            }
        },
    };
    let mut applied: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for session in sessions {
        let Ok(sock) = proto::socket_path(&session) else {
            continue;
        };
        let control = ControlCommand::WorkspaceOrigin {
            workspace: name.to_string(),
            path: path.clone(),
            remove: action == "remove",
        };
        match control_roundtrip(&sock, &session, control) {
            Ok(ServerMsg::Ok) => applied.push(session),
            // This server holds no such workspace; another may.
            Ok(ServerMsg::Err { code, .. }) if code == err_code::NOT_FOUND => {}
            Ok(ServerMsg::Err { msg, .. }) => refused.push(format!("{session}: {msg}")),
            Ok(_) => refused.push(format!("{session}: unexpected reply shape (build skew?)")),
            Err(e) => refused.push(format!("{session}: {e}")),
        }
    }
    for line in &refused {
        eprintln!("fno mux workspace origin: {line}");
    }
    if applied.is_empty() {
        if refused.is_empty() {
            eprintln!("fno mux workspace origin: no open workspace named {name} on any mux server");
        }
        return EXIT_ERROR;
    }
    let origins = crate::squad_store::load()
        .squads
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.origins)
        .unwrap_or_default();
    if common.json {
        println!(
            "{}",
            serde_json::json!({ "workspace": name, "origins": origins, "sessions": applied })
        );
    } else {
        let now = if origins.is_empty() {
            "none".to_string()
        } else {
            origins.join(", ")
        };
        println!("workspace {name}: origin {action} {path}; folders now: {now}");
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_path_makes_an_absolute_normalized_folder() {
        assert_eq!(origin_path("/a/b/").unwrap(), "/a/b");
        assert_eq!(origin_path("/a/./b/../c").unwrap(), "/a/c");
        assert_eq!(origin_path("/").unwrap(), "/");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            origin_path("x").unwrap(),
            cwd.join("x").to_string_lossy().to_string()
        );
    }
}

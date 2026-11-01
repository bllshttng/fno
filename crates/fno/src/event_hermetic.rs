use std::path::{Component, Path, PathBuf};

fn realpath(path: &Path) -> Result<PathBuf, String> {
    fn resolve(path: &Path, depth: usize) -> Result<PathBuf, String> {
        if depth > 40 {
            return Err("too many symbolic links".into());
        }
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("cannot resolve current directory: {e}"))?
                .join(path)
        };
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => out.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(part) => {
                    out.push(part);
                    match std::fs::symlink_metadata(&out) {
                        Ok(meta) if meta.file_type().is_symlink() => {
                            let target = std::fs::read_link(&out)
                                .map_err(|e| format!("cannot resolve {}: {e}", out.display()))?;
                            let parent = out.parent().unwrap_or(Path::new("/"));
                            out = resolve(&parent.join(target), depth + 1)?;
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!("cannot resolve {}: {error}", out.display()))
                        }
                    }
                }
            }
        }
        Ok(out)
    }
    resolve(path, 0)
}

fn judge(path: &Path, temp: &Path, pin: Option<&Path>) -> Result<(), String> {
    let resolved = realpath(path)?;
    let mut roots = vec![realpath(temp)?];
    if let Some(pin) = pin.filter(|value| value.is_absolute()) {
        if let Some(parent) = pin.parent() {
            roots.push(realpath(parent)?);
        }
    }
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(())
    } else {
        Err(format!("refused a state path outside the test sandbox: {}. Pass an explicit path under tmp_path, or resolve it with a fno.paths accessor so the sandbox pins apply.", path.display()))
    }
}

pub(super) fn check(path: &Path) -> Result<(), String> {
    let mode = std::env::var("FNO_TEST_HERMETIC").ok();
    if mode.as_deref() == Some("0") {
        return Ok(());
    }
    let pytest = std::env::var_os("PYTEST_CURRENT_TEST").is_some();
    if mode.as_deref() != Some("1") && !pytest {
        return Ok(());
    }
    let temp = std::env::var_os("TMPDIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let pin = std::env::var_os("FNO_EVENTS_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    judge(path, &temp, pin.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_pin_allows_its_parent_relative_pin_does_not_and_symlinks_cannot_escape() {
        let root = tempfile::tempdir().unwrap();
        let allowed = root.path().join("allowed");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        assert!(judge(&allowed.join("events.jsonl"), &allowed, None).is_ok());
        assert!(judge(
            &outside.join("events.jsonl"),
            &allowed,
            Some(Path::new("relative/events.jsonl"))
        )
        .is_err());
        assert!(judge(
            &outside.join("events.jsonl"),
            &allowed,
            Some(&outside.join("pin.jsonl"))
        )
        .is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, allowed.join("link")).unwrap();
            assert!(judge(&allowed.join("link/events.jsonl"), &allowed, None).is_err());
            std::os::unix::fs::symlink(outside.join("missing"), allowed.join("dangling")).unwrap();
            assert!(judge(&allowed.join("dangling/events.jsonl"), &allowed, None).is_err());
            assert!(judge(
                &allowed.join("missing/../../outside/events.jsonl"),
                &allowed,
                None
            )
            .is_err());
        }
    }
}

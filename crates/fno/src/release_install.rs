//! The release leg of `fno update`: a wheel install moves to the binary
//! install in `~/.fno/bin`, and a binary install updates from the newest
//! tagged GitHub release. The wheel stops publishing once Python is gone, so
//! `uv tool upgrade fno` would strand every wheel user on the last wheel.
//!
//! The move keeps the uv tool. The front door still forwards some argv to
//! `fno-py`, and the bootstrap reinstalls the wheel when it finds none, so
//! only the four Rust entrypoint links move. `uv tool uninstall fno` runs
//! once a probe shows the binary `fno --version` answers without Python.
//! Every later binary update runs the same probe.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::model_catalog::state_dir;
use crate::update_prebuilt::{self, BINARIES};

/// Where the running `fno` came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallChannel {
    /// A uv tool venv: `<tool_root>/bin/fno` beside `<tool_root>/uv-receipt.toml`.
    Wheel {
        tool_root: PathBuf,
    },
    /// `~/.fno/bin/fno`.
    Binary,
    Other,
}

/// The binary install's directory.
pub(crate) fn bin_dir() -> PathBuf {
    state_dir().join("bin")
}

fn marker_path() -> PathBuf {
    state_dir().join("install").join("channel")
}

/// The channel of the executable at `exe`, given the binary install dir.
pub(crate) fn channel_of(exe: &Path, bin: &Path) -> InstallChannel {
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let Some(parent) = exe.parent() else {
        return InstallChannel::Other;
    };
    if std::fs::canonicalize(bin).is_ok_and(|b| b == parent) {
        return InstallChannel::Binary;
    }
    let index = |root: &Path| {
        std::fs::read_to_string(root.join("uv-receipt.toml"))
            .ok()
            .and_then(|r| receipt_is_index(&r))
            == Some(true)
    };
    match parent.parent() {
        Some(root) if parent.ends_with("bin") && index(root) => InstallChannel::Wheel {
            tool_root: root.to_path_buf(),
        },
        _ => InstallChannel::Other,
    }
}

/// `Some(true)` when the uv receipt installed fno from an index (a release
/// wheel), `Some(false)` for a directory, path, git, url or editable source,
/// `None` when the receipt has no fno entry. Only a release wheel moves.
pub(crate) fn receipt_is_index(receipt: &str) -> Option<bool> {
    let value: toml::Value = toml::from_str(receipt).ok()?;
    let reqs = value.get("tool")?.get("requirements")?.as_array()?;
    let fno = reqs
        .iter()
        .find(|r| r.get("name").and_then(|n| n.as_str()) == Some("fno"))?;
    let local = ["directory", "path", "git", "url", "editable"]
        .iter()
        .any(|k| fno.get(k).is_some());
    Some(!local)
}

pub(crate) fn current_channel() -> InstallChannel {
    match std::env::current_exe() {
        Ok(exe) => channel_of(&exe, &bin_dir()),
        Err(_) => InstallChannel::Other,
    }
}

/// The release tag that ships `version` (the semver the binaries report).
/// release.yml tags PEP 440: `0.4.6-rc.1` ships as `v0.4.6rc1`, and a
/// `-dev.N` build ships on the rolling `nightly` release.
pub(crate) fn release_tag(version: &str) -> Option<String> {
    version_key(version)?;
    Some(match version.split_once('-') {
        None => format!("v{version}"),
        Some((core, pre)) => match pre.strip_prefix("rc.") {
            Some(n) => format!("v{core}rc{n}"),
            None => "nightly".to_string(),
        },
    })
}

/// The semver a release tag ships: the inverse of [`release_tag`].
pub(crate) fn tag_version(tag: &str) -> Option<String> {
    let bare = tag.strip_prefix('v')?;
    let version = match bare.split_once("rc") {
        Some((core, n)) => format!("{core}-rc.{n}"),
        None => bare.to_string(),
    };
    version_key(&version).map(|_| version)
}

/// An order key for `X.Y.Z`, `X.Y.Z-rc.N` and `X.Y.Z-dev.N`: dev before rc
/// before stable. None for any other shape.
pub(crate) fn version_key(version: &str) -> Option<(u64, u64, u64, u8, u64)> {
    let (core, pre) = match version.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (version, None),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let (x, y, z) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    let (rank, n) = match pre {
        None => (2, 0),
        Some(p) => match (p.strip_prefix("rc."), p.strip_prefix("dev.")) {
            (Some(n), _) => (1, n.parse().ok()?),
            (_, Some(n)) => (0, n.parse().ok()?),
            _ => return None,
        },
    };
    Some((x, y, z, rank, n))
}

fn tarball_url(tag: &str, platform: &str) -> String {
    format!(
        "https://github.com/{}/releases/download/{tag}/fno-agents-{platform}.tar.gz",
        update_prebuilt::release_repo()
    )
}

/// The newest stable release's tag, read from the `releases/latest` redirect.
/// That URL is not the REST API, so it spends no rate limit.
pub(crate) fn latest_release_tag() -> Result<String, String> {
    let url = format!(
        "https://github.com/{}/releases/latest",
        update_prebuilt::release_repo()
    );
    let out = crate::process_admission::std_command("curl")
        .args([
            "-fsSL",
            "--max-time",
            "20",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
        ])
        .arg(&url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    let effective = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        return Err(format!(
            "curl exited {} for {url}",
            out.status.code().unwrap_or(1)
        ));
    }
    effective
        .rsplit_once("/tag/")
        .map(|(_, tag)| tag.to_string())
        .ok_or_else(|| format!("{url} redirected to {effective}, which names no tag"))
}

/// The `(name, install-path)` of each uv receipt entrypoint.
pub(crate) fn receipt_links(receipt: &str) -> Vec<(String, PathBuf)> {
    let Ok(value) = toml::from_str::<toml::Value>(receipt) else {
        return Vec::new();
    };
    let Some(points) = value
        .get("tool")
        .and_then(|t| t.get("entrypoints"))
        .and_then(|e| e.as_array())
    else {
        return Vec::new();
    };
    points
        .iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?;
            let path = p.get("install-path")?.as_str()?;
            Some((name.to_string(), PathBuf::from(path)))
        })
        .collect()
}

/// Point each Rust entrypoint link at its binary in `bin`. `fno-py` keeps its
/// venv link. Each link lands as a temp symlink, then renames over the old one.
pub(crate) fn relink(links: &[(String, PathBuf)], bin: &Path) -> Result<(), String> {
    for (name, path) in links.iter().filter(|(n, _)| BINARIES.contains(&n.as_str())) {
        let tmp = path.with_file_name(format!(".{name}.fno-move-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(bin.join(name), &tmp)
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", path.display())
            })?;
    }
    Ok(())
}

/// The `package` version the unpacked `fno-agents version --json` reports.
fn unpacked_version(unpacked: &Path) -> Result<String, String> {
    let out = crate::process_admission::std_command(unpacked.join("fno-agents"))
        .args(["version", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("the downloaded fno-agents did not run: {e}"))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("the downloaded fno-agents version --json is not JSON: {e}"))?;
    v.get("package")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the downloaded fno-agents version --json names no package".to_string())
}

/// Download `tag`, prove it ships `want`, and swap it into `bin`. Nothing
/// is swapped on any Err.
fn install_tag(tag: &str, want: &str, platform: &str, bin: &Path) -> Result<(), String> {
    let url = tarball_url(tag, platform);
    let staging = state_dir()
        .join("install")
        .join(format!("release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let result = update_prebuilt::download_verified(&url, &staging)
        .map_err(|e| format!("fetch {url}: {e}"))
        .and_then(|unpacked| {
            let got = unpacked_version(&unpacked)?;
            if got != want {
                return Err(format!("version proof: {url} ships {got}, want {want}"));
            }
            update_prebuilt::swap_into(&unpacked, bin).map_err(|e| format!("swap: {e}"))
        });
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Remove the uv tool once the binary `fno` no longer forwards to Python:
/// `fno --version` must answer with `FNO_PY` on a stub that refuses. uv
/// deletes the entrypoint links on uninstall, so they are made again after.
fn retire_uv_tool(tool_root: &Path, bin: &Path) -> Result<String, String> {
    let receipt = tool_root.join("uv-receipt.toml");
    let Ok(text) = std::fs::read_to_string(&receipt) else {
        return Ok("no uv tool to retire".to_string());
    };
    let stub_dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let stub = stub_dir.path().join("fno-py");
    std::fs::write(&stub, "#!/bin/sh\nexit 97\n")
        .map_err(|e| format!("{}: {e}", stub.display()))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755));
    let answered = crate::process_admission::std_command(bin.join("fno"))
        .arg("--version")
        .env("FNO_PY", &stub)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !answered {
        return Ok("keeping the uv tool: `fno --version` still needs fno-py".to_string());
    }
    let uv = crate::bootstrap::find_uv().ok_or("uv is not on PATH")?;
    let links = receipt_links(&text);
    let out = crate::process_admission::std_command(uv)
        .args(["tool", "uninstall", "fno"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("uv tool uninstall fno: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "uv tool uninstall fno exited {}",
            out.status.code().unwrap_or(1)
        ));
    }
    relink(&links, bin)?;
    Ok("removed the uv tool; the links point into the binary install".to_string())
}

/// The uv tool root on a binary install, when the move kept one.
fn kept_tool_root() -> Option<PathBuf> {
    let uv = crate::bootstrap::find_uv()?;
    let out = crate::process_admission::std_command(uv)
        .args(["tool", "dir"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("fno");
    (out.status.success() && dir.join("uv-receipt.toml").is_file()).then_some(dir)
}

fn failed(step: &str, why: &str) -> i32 {
    eprintln!("fno update: FAILED: {step}: {why}");
    1
}

/// `fno update` on a Wheel or Binary channel. Returns the exit code.
pub(crate) fn update(channel: &InstallChannel, dry_run: bool) -> i32 {
    let Some(platform) = update_prebuilt::platform() else {
        return failed("platform", "no release tarball is built for this platform");
    };
    let bin = bin_dir();
    let running = env!("CARGO_PKG_VERSION");
    let (tag, want) = match channel {
        InstallChannel::Wheel { .. } => match release_tag(running) {
            Some(tag) => (tag, running.to_string()),
            None => return failed("version", &format!("{running} maps to no release tag")),
        },
        InstallChannel::Binary => {
            let tag = match latest_release_tag() {
                Ok(t) => t,
                Err(e) => return failed("latest release", &e),
            };
            let Some(latest) = tag_version(&tag) else {
                return failed("latest release", &format!("tag {tag} names no version"));
            };
            if version_key(&latest) <= version_key(running) {
                println!("fno update: current ({running}); the latest release is {latest}.");
                return 0;
            }
            (tag, latest)
        }
        InstallChannel::Other => return failed("channel", "not a wheel or binary install"),
    };
    let url = tarball_url(&tag, platform);
    if dry_run {
        println!(
            "Would fetch {url} and its .sha256, prove version {want}, and swap into {}",
            bin.display()
        );
        if let InstallChannel::Wheel { tool_root } = channel {
            println!(
                "Would relink the uv tool links from {} into {}, write {} = binary, refresh pr-watch, and keep the uv tool while fno-py is needed",
                tool_root.join("uv-receipt.toml").display(),
                bin.display(),
                marker_path().display()
            );
        }
        return 0;
    }
    println!("fno update: installing {want} from {url}");
    if let Err(e) = install_tag(&tag, &want, platform, &bin) {
        return failed("install", &e);
    }
    let tool_root = match channel {
        InstallChannel::Wheel { tool_root } => {
            let receipt = tool_root.join("uv-receipt.toml");
            let links = std::fs::read_to_string(&receipt)
                .map(|t| receipt_links(&t))
                .unwrap_or_default();
            if !links.iter().any(|(n, _)| n == "fno") {
                return failed(
                    "relink",
                    &format!("{} names no fno entrypoint", receipt.display()),
                );
            }
            if let Err(e) = relink(&links, &bin) {
                return failed("relink", &e);
            }
            let marker = marker_path();
            let wrote = marker
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::write(&marker, "binary\n"));
            if let Err(e) = wrote {
                return failed("marker", &format!("{}: {e}", marker.display()));
            }
            Some(tool_root.clone())
        }
        _ => kept_tool_root(),
    };
    let mut code = 0;
    let refresh = crate::process_admission::std_command(bin.join("fno-agents"))
        .args(["pr-watch", "refresh", "--caller", "update", "--fno-binary"])
        .arg(bin.join("fno"))
        .stdin(Stdio::null())
        .output();
    match refresh {
        Ok(o) if o.status.success() => print!("{}", String::from_utf8_lossy(&o.stdout)),
        Ok(o) => {
            code = failed(
                "pr-watch refresh",
                &format!("exit {}", o.status.code().unwrap_or(1)),
            )
        }
        Err(e) => code = failed("pr-watch refresh", &e.to_string()),
    }
    if let Some(root) = tool_root {
        match retire_uv_tool(&root, &bin) {
            Ok(line) => println!("fno update: {line}"),
            Err(e) => code = failed("uv tool retire", &e),
        }
    }
    if code == 0 {
        println!("fno update: done; {} runs {want}. Restart the daemon and mux to run the new code: fno agents session restart --mux. This ends live mux sessions.", bin.join("fno").display());
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_reads_the_exe_path_and_the_receipt_names_the_links() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(".fno/bin");
        let venv = home.path().join("tools/fno/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(bin.join("fno"), b"").unwrap();
        std::fs::write(venv.join("fno"), b"").unwrap();
        assert_eq!(channel_of(&bin.join("fno"), &bin), InstallChannel::Binary);
        // A venv without a receipt is a pip or source venv, never the uv tool,
        // and a uv tool built from a directory is a source install.
        assert_eq!(channel_of(&venv.join("fno"), &bin), InstallChannel::Other);
        let source = "[tool]\nrequirements = [{ name = \"fno\", directory = \"/x/cli\" }]\n";
        std::fs::write(home.path().join("tools/fno/uv-receipt.toml"), source).unwrap();
        assert_eq!(channel_of(&venv.join("fno"), &bin), InstallChannel::Other);
        let receipt = "[tool]\nrequirements = [{ name = \"fno\" }]\nentrypoints = [\n  { name = \"fno\", install-path = \"/u/bin/fno\", from = \"fno\" },\n  { name = \"fno-py\", install-path = \"/u/bin/fno-py\", from = \"fno\" },\n]\n";
        std::fs::write(home.path().join("tools/fno/uv-receipt.toml"), receipt).unwrap();
        let root = std::fs::canonicalize(home.path().join("tools/fno")).unwrap();
        assert_eq!(
            channel_of(&venv.join("fno"), &bin),
            InstallChannel::Wheel { tool_root: root }
        );
        assert_eq!(
            receipt_links(receipt),
            vec![
                ("fno".to_string(), PathBuf::from("/u/bin/fno")),
                ("fno-py".to_string(), PathBuf::from("/u/bin/fno-py")),
            ]
        );
        assert!(receipt_links("not toml [").is_empty());
    }

    #[test]
    fn versions_map_to_release_tags_and_order() {
        assert_eq!(release_tag("0.4.6").as_deref(), Some("v0.4.6"));
        assert_eq!(release_tag("0.4.6-rc.2").as_deref(), Some("v0.4.6rc2"));
        assert_eq!(release_tag("0.4.6-dev.7").as_deref(), Some("nightly"));
        assert_eq!(release_tag("0.4"), None);
        assert_eq!(tag_version("v0.4.6rc2").as_deref(), Some("0.4.6-rc.2"));
        assert_eq!(tag_version("v0.4.6").as_deref(), Some("0.4.6"));
        assert_eq!(tag_version("nightly"), None);
        assert!(version_key("0.4.6-dev.9") < version_key("0.4.6-rc.1"));
        assert!(version_key("0.4.6-rc.1") < version_key("0.4.6"));
        assert!(version_key("0.4.6") < version_key("0.4.10"));
        assert_eq!(version_key("0.4.6-beta.1"), None);
    }

    #[test]
    fn relink_moves_the_rust_links_and_leaves_fno_py() {
        let dir = tempfile::tempdir().unwrap();
        let tool_bin = dir.path().join("tool-bin");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&tool_bin).unwrap();
        let venv_py = dir.path().join("venv-fno-py");
        std::fs::write(&venv_py, b"").unwrap();
        std::os::unix::fs::symlink(&venv_py, tool_bin.join("fno-py")).unwrap();
        std::os::unix::fs::symlink(&venv_py, tool_bin.join("fno")).unwrap();
        let links: Vec<(String, PathBuf)> = ["fno", "fno-agents", "fno-py"]
            .iter()
            .map(|n| (n.to_string(), tool_bin.join(n)))
            .collect();
        relink(&links, &bin).unwrap();
        assert_eq!(
            std::fs::read_link(tool_bin.join("fno")).unwrap(),
            bin.join("fno")
        );
        assert_eq!(
            std::fs::read_link(tool_bin.join("fno-agents")).unwrap(),
            bin.join("fno-agents")
        );
        assert_eq!(
            std::fs::read_link(tool_bin.join("fno-py")).unwrap(),
            venv_py
        );
        let leftovers = std::fs::read_dir(&tool_bin).unwrap().count();
        assert_eq!(leftovers, 3, "no temp link stays behind");
    }
}

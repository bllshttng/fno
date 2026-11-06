//! The release leg of `fno update`: a wheel install moves to the binary
//! install in `~/.fno/install/bin`, and a binary install updates from the newest
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

/// The front door's member name in the tarball and in `bin_dir()`.
const FRONT_DOOR: &str = BINARIES[3];
/// The uv tool's package name: its venv is `<uv tool dir>/fno`.
const UV_TOOL: &str = "fno";

/// Where the running `fno` came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallChannel {
    /// A uv tool venv: `<tool_root>/bin/fno` beside `<tool_root>/uv-receipt.toml`.
    Wheel {
        tool_root: PathBuf,
    },
    /// `~/.fno/install/bin/fno`.
    Binary,
    Other,
}

/// The binary install's directory: the state layout's `bin` row, a
/// subfolder, so the state root grows no top-level entry.
pub(crate) fn bin_dir() -> PathBuf {
    let root = state_dir();
    crate::state_layout::place_dir(&root, "bin").unwrap_or_else(|| root.join("install/bin"))
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

/// Download `tag` and prove it ships `want`; return `(staging, unpacked)`.
/// The caller removes `staging` when it is done with `unpacked`. Nothing is
/// swapped anywhere on any Err.
fn fetch_and_prove(tag: &str, want: &str, platform: &str) -> Result<(PathBuf, PathBuf), String> {
    let url = tarball_url(tag, platform);
    let staging = state_dir()
        .join("install")
        .join(format!("release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let unpacked = update_prebuilt::download_verified(&url, &staging)
        .map_err(|e| format!("fetch {url}: {e}"))?;
    let got = unpacked_version(&unpacked)?;
    if got != want {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("version proof: {url} ships {got}, want {want}"));
    }
    Ok((staging, unpacked))
}

/// Adopt an already-proven unpacked release: swap it into `bin`, relink the
/// receipt links, write the channel marker, refresh pr-watch, and retire the
/// uv tool when nothing still runs from it. `tool_root` is Some on the
/// wheel's own move; None falls back to a kept tool root. Returns the tool
/// root the retire ran for, so a fresh install (None) knows it must link
/// `~/.local/bin` itself.
fn adopt_unpacked(
    unpacked: &Path,
    tool_root: Option<&Path>,
    bin: &Path,
) -> Result<Option<PathBuf>, String> {
    update_prebuilt::swap_into(unpacked, bin).map_err(|e| format!("swap: {e}"))?;
    let tool_root = match tool_root {
        Some(root) => {
            let receipt = root.join("uv-receipt.toml");
            let links = std::fs::read_to_string(&receipt)
                .map(|t| receipt_links(&t))
                .unwrap_or_default();
            if !links.iter().any(|(n, _)| n == "fno") {
                return Err(format!("{} names no fno entrypoint", receipt.display()));
            }
            relink(&links, bin)?;
            Some(root.to_path_buf())
        }
        None => kept_tool_root(),
    };
    let marker = marker_path();
    marker
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&marker, "binary\n"))
        .map_err(|e| format!("{}: {e}", marker.display()))?;
    // Auxiliary: a failed refresh costs its FAILED line, never the move -
    // stranding the uv tool over it would undo the swap's progress.
    let refresh = crate::process_admission::std_command(bin.join("fno-agents"))
        .args(["pr-watch", "refresh", "--caller", "update", "--fno-binary"])
        .arg(bin.join(FRONT_DOOR))
        .stdin(Stdio::null())
        .output();
    if let Ok(o) = &refresh {
        if !o.status.success() {
            failed(
                "pr-watch refresh",
                &format!("exit {}", o.status.code().unwrap_or(1)),
            );
        }
    }
    if let Some(root) = tool_root.as_deref() {
        match retire_uv_tool(&root, bin) {
            Ok(line) => println!("fno update: {line}"),
            Err(e) => {
                failed("uv tool retire", &e);
            }
        }
    }
    Ok(tool_root)
}

/// The argv the front door still forwarded to `fno-py` when the move
/// shipped. Each must answer with `FNO_PY` on a refusing stub before the uv
/// tool goes: a forward that finds no `fno-py` reinstalls the wheel over the
/// move.
const FORWARD_PROBES: [&[&str]; 3] = [&["--version"], &["--help"], &["inbox", "outstanding"]];

/// One line per live process whose argv names a path under `root`. The same
/// over-catching read as doctor_update's `live_tool_env_processes` and
/// postinstall.sh's: a suspicious line costs a kept tool, never a broken one.
fn live_processes_under(root: &Path) -> Vec<String> {
    let needle = root.to_string_lossy().to_string();
    if needle.is_empty() {
        return Vec::new();
    }
    let Ok(out) = crate::process_admission::std_command("ps")
        .args(["-axo", "pid=,args="])
        .stdin(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(&needle) && !l.contains("ps -axo"))
        .map(str::to_string)
        .collect()
}

/// Remove the uv tool once the binary `fno` no longer forwards to Python. uv
/// deletes the entrypoint links on uninstall, so they are made again after.
fn retire_uv_tool(tool_root: &Path, bin: &Path) -> Result<String, String> {
    let live = live_processes_under(tool_root);
    if !live.is_empty() {
        return Ok(format!(
            "keeping the uv tool: {} process(es) still run from it; the next fno update retires it",
            live.len()
        ));
    }
    let receipt = tool_root.join("uv-receipt.toml");
    let Ok(text) = std::fs::read_to_string(&receipt) else {
        return Ok("no uv tool to retire".to_string());
    };
    let stub_dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let stub = stub_dir.path().join("fno-py");
    let reached = stub_dir.path().join("reached");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\n: > '{}'\nexit 97\n", reached.display()),
    )
    .map_err(|e| format!("{}: {e}", stub.display()))?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755));
    let forwards = FORWARD_PROBES.iter().find(|argv| {
        crate::process_admission::std_command(bin.join(FRONT_DOOR))
            .args(argv.iter())
            .env("FNO_PY", &stub)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
            || reached.exists()
    });
    if let Some(argv) = forwards {
        return Ok(format!(
            "keeping the uv tool: `fno {}` still needs fno-py",
            argv.join(" ")
        ));
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
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join(UV_TOOL);
    (out.status.success() && dir.join("uv-receipt.toml").is_file()).then_some(dir)
}

fn failed(step: &str, why: &str) -> i32 {
    eprintln!("fno update: FAILED: {step}: {why}");
    1
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The first-run move's one-hour backoff stamp: a failed move never retries
/// from a hook more often than hourly, and never fails the user's command.
const BACKOFF_SECS: u64 = 3_600;

fn backoff_path() -> PathBuf {
    state_dir().join("install").join("first-run-backoff")
}

fn backoff_active() -> bool {
    std::fs::read_to_string(backoff_path())
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|t| now_secs() < t.saturating_add(BACKOFF_SECS))
}

fn move_lock_path() -> PathBuf {
    state_dir().join("install").join("first-run-move.lock")
}

/// Take the move lock, or None when another live process holds it. The file
/// carries the holder's pid, so a crashed mover's stale lock is stolen.
/// `create_new` closes the exists-then-create race: losing it reads as held.
fn acquire_move_lock() -> Option<PathBuf> {
    let path = move_lock_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let alive = text
            .trim()
            .parse::<u32>()
            .map(|pid| {
                crate::process_admission::std_command("ps")
                    .args(["-p", &pid.to_string()])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .unwrap_or(true);
        if alive {
            return None;
        }
        let _ = std::fs::remove_file(&path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    f.write_all(format!("{}\n", std::process::id()).as_bytes())
        .ok()?;
    Some(path)
}

/// Link each binary into `dir` (the PATH dir of a fresh install that has no
/// uv receipt to relink). Each link lands as a temp symlink, then renames
/// over the old one, so a running command never sees a missing name.
fn link_binaries(bin: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for name in update_prebuilt::BINARIES {
        let link = dir.join(name);
        let tmp = link.with_file_name(format!(".{name}.fno-adopt-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(bin.join(name), &tmp)
            .and_then(|_| std::fs::rename(&tmp, &link))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", link.display())
            })?;
    }
    Ok(())
}

/// The `fno update --adopt <dir>` door: prove the unpacked release ships the
/// running version, swap it in, and adopt the binary install. The staged
/// front door that postinstall.sh unpacks runs this on a machine that may
/// have no uv and no receipt: adopt must not require either.
pub(crate) fn adopt_release(unpacked: &Path) -> i32 {
    let running = env!("CARGO_PKG_VERSION");
    if let Err(e) = unpacked_version(unpacked).and_then(|got| {
        (got == running).then_some(()).ok_or_else(|| {
            format!(
                "version proof: {} ships {got}, want {running}",
                unpacked.display()
            )
        })
    }) {
        return failed("adopt", &e);
    }
    let Some(lock) = acquire_move_lock() else {
        return failed(
            "lock",
            "another fno call is moving this install; let it finish, then retry",
        );
    };
    let bin = bin_dir();
    let result = adopt_unpacked(unpacked, None, &bin);
    let _ = std::fs::remove_file(&lock);
    let tool_root = match result {
        Ok(r) => r,
        Err(e) => return failed("adopt", &e),
    };
    if tool_root.is_none() {
        // A fresh install has no receipt: the PATH links are this door's own
        // job. With no HOME there is no PATH dir to own; postinstall's PATH
        // note covers it.
        match std::env::var_os("FNO_INSTALL_DIR")
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/bin")))
        {
            Some(dir) => {
                if let Err(e) = link_binaries(&bin, &dir) {
                    return failed("link", &e);
                }
            }
            None => eprintln!(
                "fno update: no HOME; link {} into your PATH yourself",
                bin.display()
            ),
        }
    }
    println!(
        "fno update: done; {} runs {running}.",
        bin.join(FRONT_DOOR).display()
    );
    0
}

/// The bridge's whole job (law d-df1f42c1): a wheel install that has not yet
/// moved runs the Wheel install once at this version's release tag, adopts
/// the binary install, and execs the binary front door with the original
/// argv. Skips silently unless this exe is a wheel install awaiting the move;
/// never fails the caller's command.
pub(crate) fn first_run_move(args: &[std::ffi::OsString]) {
    if backoff_active() {
        return;
    }
    let InstallChannel::Wheel { tool_root } = current_channel() else {
        return;
    };
    if std::fs::read_to_string(marker_path()).is_ok_and(|m| m.trim() == "binary") {
        return;
    }
    let Some(platform) = update_prebuilt::platform() else {
        return;
    };
    let running = env!("CARGO_PKG_VERSION");
    let Some(tag) = release_tag(running) else {
        return;
    };
    let Some(lock) = acquire_move_lock() else {
        return; // another call is moving; this one runs on the wheel
    };
    let bin = bin_dir();
    eprintln!(
        "fno: moving this wheel install to the binary install at {}",
        bin.join(FRONT_DOOR).display()
    );
    let result = fetch_and_prove(&tag, running, platform).and_then(|(staging, unpacked)| {
        let adopted = adopt_unpacked(&unpacked, Some(&tool_root), &bin);
        let _ = std::fs::remove_dir_all(&staging);
        adopted
    });
    let _ = std::fs::remove_file(&lock);
    if let Err(e) = result {
        let _ = std::fs::write(backoff_path(), now_secs().to_string());
        eprintln!("fno: the move did not finish ({e}); run: fno update");
        return;
    }
    // Exec replaces this process: the caller's own door never runs on the
    // wheel, and no stdio is touched so an interactive command keeps its tty.
    use std::os::unix::process::CommandExt as _;
    let err = std::process::Command::new(bin.join(FRONT_DOOR))
        .args(args.iter())
        .exec();
    eprintln!("fno: the moved front door did not start: {err}");
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
    let tool_root = match channel {
        InstallChannel::Wheel { tool_root } => Some(tool_root.clone()),
        _ => None,
    };
    let url = tarball_url(&tag, platform);
    if dry_run {
        println!(
            "Would fetch {url} and its .sha256, prove version {want}, and swap into {}",
            bin.display()
        );
        if let Some(root) = &tool_root {
            println!(
                "Would relink the uv tool links from {} into {}, write {} = binary, refresh pr-watch, and keep the uv tool while fno-py is needed",
                root.join("uv-receipt.toml").display(),
                bin.display(),
                marker_path().display()
            );
        }
        return 0;
    }
    // `fno update` and the first-run move share one lock: a move already in
    // flight owns the swap.
    let Some(lock) = acquire_move_lock() else {
        return failed(
            "lock",
            "another fno call is moving this install; let it finish, then retry",
        );
    };
    println!("fno update: installing {want} from {url}");
    let result = fetch_and_prove(&tag, &want, platform).and_then(|(staging, unpacked)| {
        let adopted = adopt_unpacked(&unpacked, tool_root.as_deref(), &bin);
        let _ = std::fs::remove_dir_all(&staging);
        adopted
    });
    let _ = std::fs::remove_file(&lock);
    if let Err(e) = result {
        return failed("install", &e);
    }
    println!("fno update: done; {} runs {want}. Restart the daemon and mux to run the new code: fno agents session restart --mux. This ends live mux sessions.", bin.join(FRONT_DOOR).display());
    0
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

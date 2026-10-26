//! `fno config setup shim-check`: fno shims must never dangle into a cleaned
//! mktemp dir, or go missing.
//!
//! Scans the tool bin for `fno*` symlinks that dangle or resolve under the
//! temp root, and for required shims that are absent, and points them at the
//! durable copy. The installers (`scripts/install/fno.sh`,
//! `.claude-plugin/postinstall.sh`) run it with `--repair` after every
//! install and fail loud when a defect survives.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Every gh read and every fno command dies when one of these is missing.
const REQUIRED_SHIMS: &[&str] = &["fno", "fno-gh-proxy"];

const USAGE: &str = "usage: fno config setup shim-check [--repair] [--bin-dir DIR]";

/// The directories a scan reads. The command builds it from the environment;
/// tests build it from a scratch dir.
pub struct Roots {
    pub bin_dir: PathBuf,
    /// The directory this front door runs from. The installers invoke it
    /// from the tool venv, so its bin is the durable copy a repair relinks
    /// to, and a shim resolving there is healthy even under the temp root.
    pub self_bin: Option<PathBuf>,
    pub temp_root: PathBuf,
    /// The uv tools copy of the fno venv bin.
    pub uv_bin: PathBuf,
    /// `fno doctor update` lands the Rust binaries here, fno-gh-proxy among
    /// them. The wheel does not ship that one.
    pub cargo_bin: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Defect {
    pub name: String,
    pub link: PathBuf,
    pub target: PathBuf,
    pub problem: &'static str,
    pub repair: Option<PathBuf>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Roots {
    fn from_env(bin_dir: Option<PathBuf>) -> Roots {
        let home = home();
        let cargo_home = std::env::var_os("CARGO_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"));
        Roots {
            bin_dir: bin_dir.unwrap_or_else(|| home.join(".local/bin")),
            self_bin: std::env::current_exe()
                .ok()
                .and_then(|exe| canonical(&exe).parent().map(Path::to_path_buf)),
            // The temp root may carry symlinked ancestors (macOS /var ->
            // /private/var) a lexical comparison would miss.
            temp_root: canonical(&std::env::temp_dir()),
            uv_bin: home.join(".local/share/uv/tools/fno/bin"),
            cargo_bin: cargo_home.join("bin"),
        }
    }
}

/// A file this user may run. access(2), not the mode bits: an owned file
/// with only the group or other execute bit set is not runnable by its owner.
fn executable(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated path that outlives the call.
    p.is_file() && unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0
}

impl Roots {
    fn defect(&self, link: &Path, target: PathBuf, problem: &'static str) -> Defect {
        let name = link
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let repair = self
            .self_bin
            .iter()
            .chain([&self.uv_bin, &self.cargo_bin])
            .map(|dir| dir.join(&name))
            .find(|durable| executable(durable));
        Defect {
            name,
            link: link.to_path_buf(),
            target,
            problem,
            repair,
        }
    }

    fn under_self_bin(&self, p: &Path) -> bool {
        self.self_bin.as_ref().is_some_and(|dir| p.starts_with(dir))
    }
}

/// Every fno* symlink defect (dangling, temp-resolving, absent). Scope is the
/// fno prefix, so a broken third-party link is not our finding. An absent
/// required shim counts only when a durable copy exists to link: a
/// pure-Python install ships no fno binary, and that is not a link defect.
pub fn scan(roots: &Roots) -> Vec<Defect> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&roots.bin_dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("fno"))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    let mut defects = Vec::new();
    for entry in entries {
        let Ok(target) = std::fs::read_link(&entry) else {
            continue;
        };
        let target = if target.is_absolute() {
            target
        } else {
            entry.parent().unwrap_or(Path::new("")).join(target)
        };
        match std::fs::canonicalize(&target) {
            Err(_) => defects.push(roots.defect(&entry, target, "dangling")),
            Ok(resolved) if roots.under_self_bin(&resolved) => {}
            Ok(resolved) if resolved.starts_with(&roots.temp_root) => {
                defects.push(roots.defect(&entry, resolved, "temp-resolving"))
            }
            Ok(_) => {}
        }
    }
    if roots.bin_dir.is_dir() {
        for name in REQUIRED_SHIMS {
            let entry = roots.bin_dir.join(name);
            if entry.exists() || entry.symlink_metadata().is_ok() {
                continue;
            }
            let defect = roots.defect(&entry, entry.clone(), "absent");
            // A durable copy under the temp root would relink into the very
            // defect the re-scan condemns, failing an install that passed.
            let linkable = defect
                .repair
                .as_deref()
                .map(canonical)
                .is_some_and(|d| roots.under_self_bin(&d) || !d.starts_with(&roots.temp_root));
            if linkable {
                defects.push(defect);
            }
        }
    }
    defects
}

/// Repoint repairable defects to the durable copy; return what still fails.
pub fn repair(defects: &[Defect]) -> Vec<String> {
    let mut remaining = Vec::new();
    for defect in defects {
        let Some(durable) = &defect.repair else {
            remaining.push(format!("{}: no durable copy", defect.name));
            continue;
        };
        let tmp =
            defect
                .link
                .with_file_name(format!(".{}.relink.{}", defect.name, std::process::id()));
        // Atomic: a reader never sees the link absent.
        let relinked = std::os::unix::fs::symlink(durable, &tmp)
            .and_then(|()| std::fs::rename(&tmp, &defect.link));
        if let Err(e) = relinked {
            remaining.push(format!("{}: relink failed: {e}", defect.name));
        }
        let _ = std::fs::remove_file(&tmp);
    }
    remaining
}

/// The scan, the optional repair and the re-scan, printed. Exit 1 while any
/// defect survives.
pub fn run_roots(roots: &Roots, do_repair: bool) -> i32 {
    let mut defects = scan(roots);
    for d in &defects {
        println!(
            "shim defect: {} -> {} ({})",
            d.name,
            d.target.display(),
            d.problem
        );
    }
    if do_repair && !defects.is_empty() {
        for line in repair(&defects) {
            println!("shim unrepairable: {line}");
        }
        defects = scan(roots);
    }
    if defects.is_empty() {
        return 0;
    }
    eprintln!(
        "shim scan: {} defect(s) in {}; re-run with --repair, or relink to {}",
        defects.len(),
        roots.bin_dir.display(),
        roots.uv_bin.display()
    );
    1
}

pub fn run(tail: &[OsString]) -> i32 {
    let (mut do_repair, mut bin_dir) = (false, None);
    let mut args = tail.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--repair") => do_repair = true,
            Some("--bin-dir") => match args.next() {
                Some(v) => bin_dir = Some(PathBuf::from(v)),
                None => {
                    eprintln!("--bin-dir needs a value\n{USAGE}");
                    return 2;
                }
            },
            Some("-h" | "--help") => {
                println!(
                    "{USAGE}\n\nFind fno* shims in the tool bin that dangle, resolve into the temp \
                     dir, or are missing, and with --repair relink them to the durable copy."
                );
                return 0;
            }
            _ => {
                eprintln!("unknown option: {}\n{USAGE}", arg.to_string_lossy());
                return 2;
            }
        }
    }
    run_roots(&Roots::from_env(bin_dir), do_repair)
}

#[cfg(test)]
#[path = "shim_check_tests.rs"]
mod tests;

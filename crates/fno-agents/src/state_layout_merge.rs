//! The merge lane of the dir mover. A message log must never park: an old
//! binary or a hook script that writes the legacy spelling after a fold
//! recreates the legacy folder, and the per-child park rule would hide those
//! lines from every reader. `bus`, `chats` and `history` fold through here
//! instead: a child the destination lacks renames in, a line log merges by
//! line set, anything else keeps the newer file and parks the older.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::Path;

/// Merge every child of `legacy` into `new`, then drop `legacy` once empty.
/// `Ok(false)` means a destination lock was held: nothing in that child
/// moved, `legacy` stays, and the next pass retries.
pub(crate) fn merge_tree(
    root: &Path,
    stamp: &str,
    legacy: &Path,
    new: &Path,
) -> Result<bool, String> {
    std::fs::create_dir_all(new).map_err(|e| format!("refused: {e}"))?;
    let mut complete = true;
    let children = std::fs::read_dir(legacy).map_err(|e| format!("refused: {e}"))?;
    for child in children.flatten() {
        let from = child.path();
        let to = new.join(child.file_name());
        let name = child.file_name().to_string_lossy().into_owned();
        if !to.exists() {
            std::fs::rename(&from, &to).map_err(|e| format!("refused: {e}"))?;
        } else if from.is_dir() && to.is_dir() {
            complete &= merge_tree(root, stamp, &from, &to)?;
        } else if from.is_file() && to.is_file() {
            if name.ends_with(".lock") {
                // A holder on the old binary keeps its lock until it lets go;
                // unlinking a held lock would let a second writer in.
                let free = std::fs::File::open(&from).is_ok_and(|f| f.try_lock().is_ok());
                if free {
                    std::fs::remove_file(&from).map_err(|e| format!("refused: {e}"))?;
                } else {
                    complete = false;
                }
            } else if is_line_log(&name) {
                complete &= merge_lines(&from, &to)?;
            } else {
                keep_newer(root, stamp, &from, &to)?;
            }
        } else {
            crate::state_layout::park_file_at(root, stamp, &from)?;
        }
    }
    let emptied = std::fs::read_dir(legacy)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false);
    if emptied {
        std::fs::remove_dir(legacy).ok();
    }
    Ok(complete)
}

/// A line log: `x.jsonl` or a rotated segment `x.jsonl.<n>`.
fn is_line_log(name: &str) -> bool {
    name.ends_with(".jsonl")
        || name
            .rsplit_once(".jsonl.")
            .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Append the lines of `from` that `to` lacks, in place under the writer's
/// own `<to>.lock` when one exists, then remove `from`. In place, never a
/// tmp-and-rename: a rename over `to` would drop a concurrent append.
fn merge_lines(from: &Path, to: &Path) -> Result<bool, String> {
    let lock_path = format!("{}.lock", to.display());
    let _guard = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => {
            if f.try_lock().is_err() {
                return Ok(false);
            }
            Some(f)
        }
        Err(_) => None,
    };
    let data = std::fs::read(from).map_err(|e| format!("merge read: {e}"))?;
    let wanted: Vec<&[u8]> = data
        .split(|b| *b == b'\n')
        .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
        .collect();
    let wanted_set: HashSet<&[u8]> = wanted.iter().copied().collect();
    let mut held: HashSet<Vec<u8>> = HashSet::new();
    let dest = std::fs::File::open(to).map_err(|e| format!("merge read: {e}"))?;
    for line in std::io::BufReader::new(dest).split(b'\n') {
        let line = line.map_err(|e| format!("merge read: {e}"))?;
        if wanted_set.contains(line.as_slice()) {
            held.insert(line);
        }
    }
    let mut out: Vec<u8> = Vec::new();
    let mut emitted: HashSet<&[u8]> = HashSet::new();
    for line in wanted {
        if !held.contains(line) && emitted.insert(line) {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    if !out.is_empty() {
        let ends_open = std::fs::read(to)
            .map(|b| b.last().is_some_and(|c| *c != b'\n'))
            .unwrap_or(false);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(to)
            .map_err(|e| format!("merge append: {e}"))?;
        if ends_open {
            f.write_all(b"\n")
                .map_err(|e| format!("merge append: {e}"))?;
        }
        f.write_all(&out)
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("merge append: {e}"))?;
    }
    std::fs::remove_file(from).map_err(|e| format!("refused: {e}"))?;
    Ok(true)
}

/// Two copies of a non-log file: the newer one stays at `to`, the older
/// parks.
fn keep_newer(root: &Path, stamp: &str, from: &Path, to: &Path) -> Result<(), String> {
    let mtime = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    };
    if mtime(from) > mtime(to) {
        crate::state_layout::park_file_at(root, stamp, to)?;
        std::fs::rename(from, to).map_err(|e| format!("refused: {e}"))
    } else {
        crate::state_layout::park_file_at(root, stamp, from).map(|_| ())
    }
}

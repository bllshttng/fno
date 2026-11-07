//! macOS Developer ID signing for the binaries an update places.
//!
//! Cargo ad-hoc signs on Apple Silicon (flags `0x20002`), which macOS Login
//! Items reports as an item from an unidentified developer, and every
//! rebuild wipes a hand-applied signature. After any deploy that places
//! fresh binaries, `fno doctor update` re-signs every binary it finds in
//! the target bin dir with the machine's `Developer ID Application`
//! identity, so a valid signature survives the update instead of dying
//! with the next one.
//!
//! Best effort by design. No identity in the keychain (a fresh machine, a
//! CI runner) skips the step with one advisory line, and a failed sign
//! warns and continues rather than failing an install that already
//! succeeded. Notarization stays a separate, still-unbuilt step; it is not
//! needed for a locally built binary, only the day binaries ship as
//! downloads. The Python `fno` console script cannot carry this signature
//! at all: it is a script, not a Mach-O, so it stays unsigned by nature.

use std::path::Path;
use std::process::Command;

/// The distribution identity class. An `Apple Development` cert also
/// produces a valid signature that Gatekeeper still refuses, so only this
/// class counts.
const IDENTITY_CLASS: &str = "Developer ID Application";

/// Every fno Mach-O in `bin_dir`: the front door, the fno-agents triad, and
/// the helpers other builds drop beside them (`fno-books`, `fno-gh-proxy`,
/// `fnob`). Found by scanning, not by a fixed list, so a bin is signed the
/// day it appears, not the day someone notices it ad hoc. Symlinks and
/// scripts are skipped: a script is not a Mach-O and cannot carry a
/// signature.
fn placed_stems(bin_dir: &Path, suffix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(bin_dir) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name.strip_suffix(suffix)?;
            let ours = stem == "fno" || stem == "fnob" || stem.starts_with("fno-");
            (ours && is_mach_o(&e.path())).then(|| stem.to_string())
        })
        .collect();
    stems.sort();
    stems
}

/// True when the file opens with a Mach-O or fat-binary magic number, in
/// either byte order.
fn is_mach_o(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    if f.read_exact(&mut magic).is_err() {
        return false;
    }
    matches!(
        u32::from_be_bytes(magic),
        0xfeed_face | 0xfeed_facf | 0xcefa_edfe | 0xcffa_edfe | 0xcafe_babe | 0xbeba_feca
    )
}

/// The codesign identifier for a binary stem, in the fixed `dev.fno`
/// namespace. macOS ties permission grants to identifier plus team, so a
/// stable identifier keeps a user's grants across every version; an ad-hoc
/// build ties to the hash, which is why each rebuild re-prompts today.
fn identifier(stem: &str) -> String {
    format!("dev.fno.{stem}")
}

/// The first `Developer ID Application` identity line in
/// `security find-identity -v -p codesigning` output; None when the
/// keychain holds only other identity classes. An `Apple Development`
/// line never matches.
fn parse_first_developer_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let quoted = line.split('"').nth(1)?;
        quoted
            .starts_with(IDENTITY_CLASS)
            .then(|| quoted.to_string())
    })
}

fn discover_identity() -> Option<String> {
    let out = Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_first_developer_id(&String::from_utf8_lossy(&out.stdout))
}

fn sign_bin(identity: &str, bin: &Path, stem: &str) -> Result<(), String> {
    let out = Command::new("codesign")
        .args([
            "--force",
            "--sign",
            identity,
            "--identifier",
            &identifier(stem),
            "--options",
            "runtime",
            "--timestamp",
        ])
        .arg(bin)
        .output()
        .map_err(|e| format!("codesign: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Sign every placed binary present in `bin_dir`. macOS only, silent
/// elsewhere; one advisory line when no Developer ID identity answers, one
/// line per signed binary, one warning per failed binary. Never fails the
/// caller: the deploy already succeeded, and the next update retries.
pub(crate) fn sign_placed(bin_dir: &Path, dry_run: bool) {
    if !cfg!(target_os = "macos") || !bin_dir.is_dir() {
        return;
    }
    let suffix = crate::doctor_update::exe_suffix();
    let present = placed_stems(bin_dir, suffix);
    if present.is_empty() {
        return;
    }
    let Some(identity) = discover_identity() else {
        println!(
            "fno doctor update: no Developer ID identity in the keychain; the bins stay ad-hoc signed"
        );
        return;
    };
    for stem in present {
        let bin = bin_dir.join(format!("{stem}{suffix}"));
        if dry_run {
            println!("Would codesign: {} -> {}", bin.display(), identifier(&stem));
            continue;
        }
        match sign_bin(&identity, &bin, &stem) {
            Ok(()) => println!("fno doctor update: signed {}", bin.display()),
            Err(e) => eprintln!(
                "fno doctor update: WARNING: codesign failed on {}: {e}",
                bin.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_is_namespaced_and_stable() {
        assert_eq!(identifier("fno"), "dev.fno.fno");
        assert_eq!(identifier("fno-agents"), "dev.fno.fno-agents");
    }

    #[test]
    fn placed_stems_take_every_fno_mach_o_and_skip_scripts_and_strangers() {
        let dir = std::env::temp_dir().join(format!("fno-codesign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mach_o = [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0];
        for name in [
            "fno",
            "fno-agents",
            "fno-books",
            "fno-gh-proxy",
            "fnob",
            "other",
        ] {
            std::fs::write(dir.join(name), mach_o).unwrap();
        }
        std::fs::write(dir.join("fno-autocontinue"), b"#!/bin/bash\n").unwrap();
        assert_eq!(
            placed_stems(&dir, ""),
            vec!["fno", "fno-agents", "fno-books", "fno-gh-proxy", "fnob"],
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_picks_the_developer_id_over_apple_development() {
        let out = concat!(
            "  1) AAA111 \"Developer ID Application: Example Dev (TEAM1234)\"\n",
            "  2) BBB222 \"Apple Development: Example Dev (TEAM5678)\"\n",
        );
        assert_eq!(
            parse_first_developer_id(out).as_deref(),
            Some("Developer ID Application: Example Dev (TEAM1234)"),
        );
    }

    #[test]
    fn parse_answers_none_without_a_developer_id_identity() {
        let out = "  1) BBB222 \"Apple Development: Example Dev (TEAM5678)\"\n   2 valid identities found\n";
        assert_eq!(parse_first_developer_id(out), None);
    }
}

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

/// Every binary stem an install may place: the mux front door plus the
/// fno-agents triad. Derived from the triad's own list so a new bin is
/// signed the day it appears, not the day someone notices it missing.
fn stems() -> Vec<&'static str> {
    std::iter::once("fno")
        .chain(crate::doctor_update::TRIAD_STEMS)
        .collect()
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
    let present: Vec<&str> = stems()
        .into_iter()
        .filter(|s| bin_dir.join(format!("{s}{suffix}")).is_file())
        .collect();
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
            println!("Would codesign: {} -> {}", bin.display(), identifier(stem));
            continue;
        }
        match sign_bin(&identity, &bin, stem) {
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
    fn stems_carry_the_front_door_and_the_whole_triad() {
        assert_eq!(
            stems(),
            vec![
                "fno",
                "fno-agents",
                "fno-agents-daemon",
                "fno-agents-worker"
            ],
        );
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

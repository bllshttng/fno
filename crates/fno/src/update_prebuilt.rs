//! The download leg of `fno update`: install the release tarball CI built
//! for the source's crates/ rev, so an update never compiles when CI already
//! did. `main-binaries.yml` builds one tarball per platform on each main merge
//! that touches crates/ and uploads it to the rolling `bin-cache` pre-release
//! as `fno-bin-<crates_rev>-<platform>.tar.gz`, beside a `.sha256` file.
//!
//! Keyed by the crates/ subtree rev, not HEAD: that is the rev the binaries
//! bake in and the rev the freshness verdict compares, so a Python-only merge
//! reuses the last tarball instead of waiting on a build.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest, Sha256};

pub(crate) const BIN_CACHE_TAG: &str = "bin-cache";
const DEFAULT_REPO: &str = "bllshttng/fno";

/// The GitHub `owner/repo` releases publish to; the env override keeps a
/// fork's install testable.
pub(crate) fn release_repo() -> String {
    std::env::var("FNO_RELEASE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}
/// One curl bound per file. The tarball is about 30 MB; a link that cannot
/// move it in this window loses to the compile fallback anyway.
const FETCH_SECS: &str = "45";

/// The four binaries every tarball must carry: the fno-agents triad and the
/// `fno` front door. The standalone `footnote` harness left the tarball when
/// crates/footnote was retired; requiring it here refused every update. This
/// is the required core, not the inventory: swap_into lands every binary the
/// tarball carries, so a bin added to crates/fno-agents reaches machines
/// without this list learning about it.
pub(crate) const BINARIES: [&str; 4] = [
    "fno-agents",
    "fno-agents-daemon",
    "fno-agents-worker",
    "fno",
];

/// The CI matrix name for this host, or None where CI builds no tarball.
pub(crate) fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("macos", "x86_64") => Some("macos-x64"),
        ("linux", "x86_64") => Some("linux-x64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        _ => None,
    }
}

pub(crate) fn asset_name(crates_rev: &str, platform: &str) -> String {
    format!("fno-bin-{crates_rev}-{platform}.tar.gz")
}

pub(crate) fn asset_url(crates_rev: &str, platform: &str) -> String {
    let repo = release_repo();
    format!(
        "https://github.com/{repo}/releases/download/{BIN_CACHE_TAG}/{}",
        asset_name(crates_rev, platform)
    )
}

/// `fno-bin-<rev>-<platform>.tar.gz` -> Some(rev), else None.
pub(crate) fn asset_rev(name: &str, platform: &str) -> Option<String> {
    let rest = name.strip_prefix("fno-bin-")?;
    let suffix = format!("-{platform}.tar.gz");
    let rev = rest.strip_suffix(&suffix)?;
    (!rev.is_empty() && rev.chars().all(|c| c.is_ascii_hexdigit())).then(|| rev.to_string())
}

/// The crates rev of the newest published tarball for `platform` among a
/// bin-cache release's assets, or None when none matches. Asset names carry
/// no order, so the pick reads each asset's upload timestamp.
pub(crate) fn newest_published_rev(assets: &[serde_json::Value], platform: &str) -> Option<String> {
    let mut best: Option<(&str, String)> = None;
    for asset in assets {
        let (Some(name), Some(created)) = (
            asset.get("name").and_then(|v| v.as_str()),
            asset.get("created_at").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Some(rev) = asset_rev(name, platform) else {
            continue;
        };
        if best.as_ref().map_or(true, |(c, _)| created > *c) {
            best = Some((created, rev));
        }
    }
    best.map(|(_, rev)| rev)
}

/// The bin-cache asset naming the newest published rev, one `<rev> <platform>`
/// line per tarball. main-binaries.yml uploads it after the tarballs, so a
/// rev it names is downloadable.
pub(crate) const LATEST_MANIFEST: &str = "fno-bin-latest.txt";

/// The rev the latest manifest names for `platform`, or None.
pub(crate) fn manifest_rev(text: &str, platform: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let rev = words.next()?;
        (words.next() == Some(platform)
            && !rev.is_empty()
            && rev.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| rev.to_ascii_lowercase())
    })
}

/// The latest manifest through the download URL. That URL is not rate
/// limited, so machines behind one IP never spend the 60-an-hour REST
/// allowance on it.
fn fetch_latest_manifest(platform: &str) -> Result<String, String> {
    let url = format!(
        "https://github.com/{}/releases/download/{BIN_CACHE_TAG}/{LATEST_MANIFEST}",
        release_repo()
    );
    let dest = std::env::temp_dir().join(format!("fno-prebuilt-latest-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&dest);
    let fetched = curl_to(&url, &dest);
    let text = std::fs::read_to_string(&dest).unwrap_or_default();
    let _ = std::fs::remove_file(&dest);
    fetched?;
    manifest_rev(&text, platform)
        .ok_or_else(|| format!("{LATEST_MANIFEST} names no build for {platform}"))
}

/// A GitHub token for the REST fallback: GH_TOKEN, GITHUB_TOKEN, then
/// `gh auth token`. None reads unauthenticated.
fn github_token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    let out = crate::process_admission::std_command("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

/// Why a REST listing read failed, from its status code and response
/// headers. A 403 or 429 with no reads left names the rate limit and when it
/// resets, so the operator sees the cause instead of a bare curl exit.
pub(crate) fn listing_refusal(code: &str, headers: &str, authed: bool) -> String {
    let header = |name: &str| {
        headers.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };
    let exhausted = header("x-ratelimit-remaining").as_deref() == Some("0");
    if (code == "403" || code == "429") && exhausted {
        let reset = header("x-ratelimit-reset")
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|epoch| chrono::DateTime::from_timestamp(epoch, 0))
            .map(|t| format!(", resets {}", t.format("%H:%M:%SZ")))
            .unwrap_or_default();
        return if authed {
            format!("HTTP {code}: the token's GitHub API rate limit is spent{reset}")
        } else {
            format!("HTTP {code}: the unauthenticated GitHub API limit (60 reads an hour per IP) is spent{reset}; set GH_TOKEN or run `gh auth login`")
        };
    }
    format!("HTTP {code} for the bin-cache release listing")
}

/// The bin-cache release asset list from the GitHub REST API, the fallback
/// when the latest manifest cannot answer. Sends a token when one exists:
/// the unauthenticated allowance is per IP, so machines behind one router
/// spend it together. The token goes through curl's stdin config, never
/// argv, so `ps` cannot read it.
pub(crate) fn fetch_release_assets() -> Result<Vec<serde_json::Value>, String> {
    use std::io::Write;
    let url = format!(
        "https://api.github.com/repos/{}/releases/tags/{}",
        release_repo(),
        BIN_CACHE_TAG
    );
    let tmp = std::env::temp_dir();
    let pid = std::process::id();
    let dest = tmp.join(format!("fno-prebuilt-assets-{pid}.json"));
    let head = tmp.join(format!("fno-prebuilt-assets-{pid}.headers"));
    let token = github_token();
    let mut child = crate::process_admission::std_command("curl")
        .args([
            "-sSL",
            "--max-time",
            FETCH_SECS,
            "-K",
            "-",
            "-w",
            "%{http_code}",
            "-D",
        ])
        .arg(&head)
        .arg("-o")
        .arg(&dest)
        .arg(&url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let mut config = String::from("header = \"Accept: application/vnd.github+json\"\n");
        if let Some(t) = &token {
            config.push_str(&format!("header = \"Authorization: Bearer {t}\"\n"));
        }
        let _ = stdin.write_all(config.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"));
    let body = std::fs::read_to_string(&dest).unwrap_or_default();
    let headers = std::fs::read_to_string(&head).unwrap_or_default();
    let _ = std::fs::remove_file(&dest);
    let _ = std::fs::remove_file(&head);
    let out = out?;
    let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        return Err(format!(
            "curl exited {} for {url}: {}",
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if code != "200" {
        return Err(listing_refusal(&code, &headers, token.is_some()));
    }
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("the bin-cache release listing is not JSON: {e}"))?;
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or("the bin-cache release listing carries no assets array")?;
    Ok(assets.clone())
}

/// The newest published tarball rev for `platform`, or None when the
/// release lists none for it. The latest manifest answers first; the REST
/// listing answers only when the manifest cannot.
pub(crate) fn newest_published(platform: &str) -> Result<Option<String>, String> {
    let manifest_why = match fetch_latest_manifest(platform) {
        Ok(rev) => return Ok(Some(rev)),
        Err(why) => why,
    };
    match fetch_release_assets() {
        Ok(assets) => Ok(newest_published_rev(&assets, platform)),
        Err(why) => Err(format!("{LATEST_MANIFEST}: {manifest_why}; listing: {why}")),
    }
}

/// The hex digest from a `sha256sum`-format line (`<hex>  <name>`).
pub(crate) fn parse_sha_line(text: &str) -> Option<String> {
    let hex = text.split_whitespace().next()?.to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

fn curl_to(url: &str, dest: &Path) -> Result<(), String> {
    let out = crate::process_admission::std_command("curl")
        .args(["-fsSL", "--max-time", FETCH_SECS, "-o"])
        .arg(dest)
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "curl exited {} for {url}: {}",
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Ok when CI published a tarball for `crates_rev`: its `.sha256` answers.
pub(crate) fn probe_published(crates_rev: &str, platform: &str) -> Result<(), String> {
    let dest = std::env::temp_dir().join(format!("fno-probe-{}.sha256", std::process::id()));
    let got = curl_to(
        &format!("{}.sha256", asset_url(crates_rev, platform)),
        &dest,
    );
    let _ = std::fs::remove_file(&dest);
    got
}

/// Download, verify and unpack the tarball for `crates_rev` into a fresh dir
/// under `staging_parent`. Returns the dir holding the four binaries. Every
/// Err names why, so the caller's compile fallback says what it replaced. A
/// failed fetch removes its staging dir.
pub(crate) fn fetch(crates_rev: &str, staging_parent: &Path) -> Result<PathBuf, String> {
    let platform = platform().ok_or("CI builds no binary for this platform")?;
    let staging = staging_parent.join(format!("prebuilt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let fetched = fetch_into(crates_rev, platform, &staging);
    if fetched.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    fetched
}

fn fetch_into(crates_rev: &str, platform: &str, staging: &Path) -> Result<PathBuf, String> {
    let url = asset_url(crates_rev, platform);
    let tarball = staging.join(asset_name(crates_rev, platform));
    let sha_file = staging.join("tarball.sha256");
    curl_to(&format!("{url}.sha256"), &sha_file)?;
    curl_to(&url, &tarball)?;
    let want = std::fs::read_to_string(&sha_file)
        .ok()
        .as_deref()
        .and_then(parse_sha_line)
        .ok_or("the .sha256 file holds no digest")?;
    let bytes = std::fs::read(&tarball).map_err(|e| format!("{}: {e}", tarball.display()))?;
    let got = format!("{:x}", Sha256::digest(&bytes));
    if got != want {
        return Err(format!("sha256 mismatch: want {want}, got {got}"));
    }
    let unpacked = staging.join("bin");
    std::fs::create_dir_all(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
    let out = crate::process_admission::std_command("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&unpacked)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tar exited {}: {}",
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    proves_rev(&unpacked, crates_rev)?;
    Ok(unpacked)
}

/// The unpacked client must report `crates_rev` from a clean build before
/// anything is swapped. A tarball that would fail the post-deploy verify is
/// refused here instead, so the caller compiles rather than wedging on it.
fn proves_rev(unpacked: &Path, crates_rev: &str) -> Result<(), String> {
    let out = crate::process_admission::std_command(unpacked.join("fno-agents"))
        .args(["version", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("the downloaded fno-agents did not run: {e}"))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("the downloaded fno-agents version --json is not JSON: {e}"))?;
    let baked = v.get("crates_rev").and_then(serde_json::Value::as_str);
    let dirty = v.get("dirty").and_then(serde_json::Value::as_bool);
    if baked == Some(crates_rev) && dirty == Some(false) {
        Ok(())
    } else {
        Err(format!(
            "the downloaded fno-agents reports crates_rev {} dirty {}, want {crates_rev} clean",
            baked.unwrap_or("none"),
            dirty.map_or("unknown".to_string(), |d| d.to_string())
        ))
    }
}

/// Move every unpacked binary into `bin_dir`. The four core names must be
/// present (a truncated or foreign tarball refuses here and swaps nothing);
/// anything else the tarball carries is swapped too, so a bin added to
/// crates/fno-agents reaches machines whose install predates it. On
/// 2026-10-22 a four-name swap list dropped the new fno-gh-proxy and left
/// the `gh` shim execing a binary no install ever landed. Every copy lands
/// first as a temp file beside its target, and only then do the renames run,
/// so a failed copy leaves the old set whole. A rename over a running binary
/// is safe on unix: the live process keeps its old inode.
pub(crate) fn swap_into(unpacked: &Path, bin_dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let missing: Vec<&str> = BINARIES
        .iter()
        .copied()
        .filter(|n| !unpacked.join(n).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!("the tarball lacks {}", missing.join(", ")));
    }
    let mut names: Vec<String> = match std::fs::read_dir(unpacked) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(e) => return Err(format!("{}: {e}", unpacked.display())),
    };
    names.sort();
    std::fs::create_dir_all(bin_dir).map_err(|e| format!("{}: {e}", bin_dir.display()))?;
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in &names {
        let tmp = bin_dir.join(format!(".{name}.prebuilt-{}", std::process::id()));
        let copied = std::fs::copy(unpacked.join(name), &tmp)
            .and_then(|_| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)));
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&tmp);
            for (t, _) in &staged {
                let _ = std::fs::remove_file(t);
            }
            return Err(format!("{}: {e}", tmp.display()));
        }
        staged.push((tmp, bin_dir.join(name)));
    }
    for (tmp, dest) in &staged {
        std::fs::rename(tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_key_on_crates_rev_and_platform() {
        assert_eq!(
            asset_name("abc123", "macos-arm64"),
            "fno-bin-abc123-macos-arm64.tar.gz"
        );
        assert!(asset_url("abc123", "linux-x64")
            .ends_with("/releases/download/bin-cache/fno-bin-abc123-linux-x64.tar.gz"));
        let hex = "a".repeat(64);
        assert_eq!(parse_sha_line(&format!("{hex}  x.tar.gz\n")), Some(hex));
        assert_eq!(parse_sha_line("not-a-digest  x.tar.gz"), None);
    }

    #[test]
    fn swap_moves_every_tarball_binary_or_none() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        for name in &BINARIES[..3] {
            std::fs::write(src.path().join(name), b"new").unwrap();
        }
        std::fs::write(dest.path().join("fno-agents"), b"old").unwrap();
        let err = swap_into(src.path(), dest.path()).unwrap_err();
        assert_eq!(err, "the tarball lacks fno", "{err}");
        assert_eq!(
            std::fs::read(dest.path().join("fno-agents")).unwrap(),
            b"old"
        );

        // The core four swap; so does anything else the tarball carries (a
        // bin added to crates/fno-agents after this machine's first install).
        std::fs::write(src.path().join(BINARIES[3]), b"new").unwrap();
        std::fs::write(src.path().join("fno-gh-proxy"), b"proxy").unwrap();
        std::fs::write(src.path().join("footnote"), b"stub").unwrap();
        swap_into(src.path(), dest.path()).unwrap();
        for name in BINARIES {
            assert_eq!(std::fs::read(dest.path().join(name)).unwrap(), b"new");
        }
        assert_eq!(
            std::fs::read(dest.path().join("fno-gh-proxy")).unwrap(),
            b"proxy"
        );
        assert_eq!(
            std::fs::read(dest.path().join("footnote")).unwrap(),
            b"stub"
        );
        let leftovers = std::fs::read_dir(dest.path()).unwrap().count();
        assert_eq!(leftovers, BINARIES.len() + 2);
    }

    #[test]
    fn manifest_rev_reads_the_line_for_this_platform() {
        let text = "aaaa1111 macos-arm64\nAAAA1111 linux-x64\nnot-hex linux-arm64\n";
        assert_eq!(
            manifest_rev(text, "macos-arm64"),
            Some("aaaa1111".to_string())
        );
        assert_eq!(
            manifest_rev(text, "linux-x64"),
            Some("aaaa1111".to_string())
        );
        assert_eq!(manifest_rev(text, "linux-arm64"), None);
        assert_eq!(manifest_rev(text, "macos-x64"), None);
        assert_eq!(manifest_rev("", "macos-arm64"), None);
    }

    #[test]
    fn listing_refusal_names_the_ip_limit_and_its_reset() {
        let spent = "HTTP/2 403\r\nX-RateLimit-Remaining: 0\r\nx-ratelimit-reset: 1792620000\r\n";
        let anon = listing_refusal("403", spent, false);
        assert!(anon.contains("60 reads an hour per IP"), "{anon}");
        assert!(anon.contains("resets "), "{anon}");
        assert!(anon.contains("GH_TOKEN"), "{anon}");
        let authed = listing_refusal("403", spent, true);
        assert!(authed.contains("token's GitHub API rate limit"), "{authed}");
        // A 403 with reads left is some other refusal, never named the limit.
        let other = listing_refusal("403", "x-ratelimit-remaining: 12\n", false);
        assert_eq!(other, "HTTP 403 for the bin-cache release listing");
    }

    #[test]
    fn newest_published_rev_reads_names_and_timestamps() {
        let assets: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
            {"name": "fno-bin-aaaa1111-macos-arm64.tar.gz", "created_at": "2026-10-09T10:00:00Z"},
            {"name": "fno-bin-bbbb2222-macos-arm64.tar.gz", "created_at": "2026-10-10T03:08:00Z"},
            {"name": "fno-bin-cccc3333-linux-x64.tar.gz", "created_at": "2026-10-10T09:00:00Z"},
            {"name": "release-notes.md", "created_at": "2026-10-10T09:30:00Z"},
            {"name": "fno-bin-not-hex-macos-arm64.tar.gz", "created_at": "2026-10-10T09:40:00Z"}
        ]"#,
        )
        .unwrap();
        let rev = newest_published_rev(&assets, "macos-arm64").expect("a macos-arm64 build exists");
        assert_eq!(rev, "bbbb2222");
        // Another platform never wins, junk names never win.
        assert_eq!(
            newest_published_rev(&assets, "linux-x64"),
            Some("cccc3333".to_string())
        );
        assert_eq!(newest_published_rev(&assets, "linux-arm64"), None);
        // The parser behind the pick: platform must match exactly, rev must
        // be hex (both already exercised through the picks above).
        assert_eq!(
            asset_rev("fno-bin-bbbb2222-macos-arm64.tar.gz", "macos-arm64"),
            Some("bbbb2222".to_string())
        );
    }
}

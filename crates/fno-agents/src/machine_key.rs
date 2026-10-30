//! This machine's Ed25519 signing key, and the peer keys it has pinned.
//!
//! Every cross-host mail row is signed with the sending machine's key on
//! insert and checked on pull. Trust is first use: the first key a receiver
//! sees for a host is pinned in its own agents home, and a later different
//! key for that host is refused until the user re-pins it. A key changes
//! only when the user rotates it (`fno-agents mail-remote key rotate`).
//!
//! The same module mints the per-machine store token: an EdDSA JWT signed by
//! one key the user holds, which sqld checks with `--auth-jwt-key-file`.
//! sqld ignores `sub` and grants full access to a token with no access
//! claim, so the token does not limit what a machine may write.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn key_path(root: &Path) -> PathBuf {
    root.join("machine_key.pk8")
}

pub fn pins_path(root: &Path) -> PathBuf {
    root.join("machine_pins.json")
}

pub struct MachineKey {
    pair: Ed25519KeyPair,
}

impl MachineKey {
    /// Read the key at `path`, or create it when the file is absent. Two
    /// processes racing the first read both end with the one key that won
    /// the `create_new`.
    pub fn load_or_create(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => Self::from_pkcs8(path, &bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let bytes = generate()?;
                match write_private(path, &bytes, false) {
                    Ok(()) => Self::from_pkcs8(path, &bytes),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        Self::from_pkcs8(path, &std::fs::read(path).map_err(|e| e.to_string())?)
                    }
                    Err(e) => Err(format!("{}: {e}", path.display())),
                }
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Replace the key at `path` with a new one. Peers that pinned the old
    /// key refuse rows signed by this one until the user re-pins there.
    pub fn rotate(path: &Path) -> Result<Self, String> {
        let bytes = generate()?;
        write_private(path, &bytes, true).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_pkcs8(path, &bytes)
    }

    fn from_pkcs8(path: &Path, bytes: &[u8]) -> Result<Self, String> {
        Ed25519KeyPair::from_pkcs8(bytes)
            .map(|pair| Self { pair })
            .map_err(|e| format!("{}: not an Ed25519 PKCS#8 key: {e}", path.display()))
    }

    /// The public key, base64url without padding.
    pub fn public_key(&self) -> String {
        B64.encode(self.pair.public_key().as_ref())
    }

    /// The signature over `bytes`, base64url without padding.
    pub fn sign(&self, bytes: &[u8]) -> String {
        B64.encode(self.pair.sign(bytes).as_ref())
    }
}

/// This machine's own key, from the agents home.
#[cfg(not(test))]
pub fn own() -> Result<MachineKey, String> {
    let home = crate::paths::AgentsHome::from_env();
    home.ensure_root().map_err(|e| e.to_string())?;
    MachineKey::load_or_create(&key_path(home.root()))
}

/// Tests sign with one key per process, never the real home's.
#[cfg(test)]
pub fn own() -> Result<MachineKey, String> {
    static TEST_KEY: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let bytes = TEST_KEY.get_or_init(|| generate().unwrap());
    MachineKey::from_pkcs8(Path::new("test key"), bytes)
}

fn generate() -> Result<Vec<u8>, String> {
    Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map(|doc| doc.as_ref().to_vec())
        .map_err(|_| "could not generate an Ed25519 key".to_string())
}

/// Write `bytes` owner-only. `replace` writes a sibling and renames it over
/// the target, so a reader never sees half a key.
fn write_private(path: &Path, bytes: &[u8], replace: bool) -> std::io::Result<()> {
    let target = if replace {
        path.with_extension("pk8.new")
    } else {
        path.to_path_buf()
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if replace {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&target)?.write_all(bytes)?;
    if replace {
        std::fs::rename(&target, path)?;
    }
    Ok(())
}

pub fn verify(public_key: &str, bytes: &[u8], signature: &str) -> bool {
    match (B64.decode(public_key), B64.decode(signature)) {
        (Ok(key), Ok(sig)) => UnparsedPublicKey::new(&ED25519, key)
            .verify(bytes, &sig)
            .is_ok(),
        _ => false,
    }
}

/// A short stable name for a key: 16 hex of its SHA-256.
pub fn fingerprint(public_key: &str) -> String {
    let digest = Sha256::digest(public_key.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// How a pulled row's sender was trusted.
#[derive(Debug, PartialEq, Eq)]
pub enum Trust {
    /// Signed by the key already pinned for its host.
    Pinned,
    /// Signed, and the first key seen for its host: now pinned.
    FirstUse,
    /// Unsigned, from a host with no pin: a sender not yet upgraded. A
    /// pinned host always signs, so its unsigned rows never land here.
    Unsigned,
}

/// Why a row was not trusted. `Untrusted` is the row's fault and final:
/// a bad signature, a key that disagrees with the pin, an unsigned row from
/// a pinned host. `Store` is this machine's pin file failing to read or
/// write; the row itself may be genuine, so it waits for a later beat.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Untrusted(String),
    Store(String),
}

type Pins = BTreeMap<String, String>;

fn load_pins(path: &Path) -> Result<Pins, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Pins::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save_pins(path: &Path, pins: &Pins) -> Result<(), String> {
    let tmp = path.with_extension("json.new");
    let body = serde_json::to_string_pretty(pins).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Pin `public_key` for `host`, replacing any earlier pin. The user's
/// re-pin after a rotation, and the first-use write.
pub fn pin(pins_file: &Path, host: &str, public_key: &str) -> Result<(), String> {
    if B64.decode(public_key).map(|k| k.len()) != Ok(32) {
        return Err(format!(
            "{public_key:?} is not a base64url Ed25519 public key"
        ));
    }
    let mut pins = load_pins(pins_file)?;
    pins.insert(host.to_string(), public_key.to_string());
    save_pins(pins_file, &pins)
}

/// Decide whether a row from `host` is trusted. `signed` is the row's
/// public key and signature over `bytes`.
pub fn check(
    pins_file: &Path,
    host: &str,
    signed: Option<(&str, &str)>,
    bytes: &[u8],
) -> Result<Trust, Refusal> {
    let pins = load_pins(pins_file).map_err(Refusal::Store)?;
    let pinned = pins.get(host);
    let Some((public_key, signature)) = signed else {
        return match pinned {
            Some(key) => Err(Refusal::Untrusted(format!(
                "unsigned row from {host}, which is pinned to key {}",
                fingerprint(key)
            ))),
            None => Ok(Trust::Unsigned),
        };
    };
    if !verify(public_key, bytes, signature) {
        return Err(Refusal::Untrusted(format!(
            "the signature from {host} (key {}) does not verify",
            fingerprint(public_key)
        )));
    }
    match pinned {
        Some(key) if key == public_key => Ok(Trust::Pinned),
        Some(key) => Err(Refusal::Untrusted(format!(
            "{host} is pinned to key {} but this row carries key {}. If {host} rotated its \
             key, re-pin it here: fno-agents mail-remote key pin {host} {public_key}",
            fingerprint(key),
            fingerprint(public_key)
        ))),
        None => {
            pin(pins_file, host, public_key).map_err(Refusal::Store)?;
            Ok(Trust::FirstUse)
        }
    }
}

/// The public key in the SubjectPublicKeyInfo PEM sqld's
/// `--auth-jwt-key-file` reads.
fn public_pem(public_key: &[u8]) -> String {
    // The fixed DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410).
    let mut der = vec![
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    der.extend_from_slice(public_key);
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n")
}

/// An EdDSA JWT for `machine`, signed by `key`: one sqld access token per
/// machine, all from the one key the user holds. `sub` names the machine
/// for the reader only; sqld does not scope the token by it.
pub fn mint_token(key: &MachineKey, machine: &str, issued_at: u64) -> String {
    let header = B64.encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
    let claims = B64.encode(serde_json::json!({"sub": machine, "iat": issued_at}).to_string());
    let signing_input = format!("{header}.{claims}");
    let signature = key.sign(signing_input.as_bytes());
    format!("{signing_input}.{signature}")
}

const USAGE: &str = "usage: fno-agents mail-remote key <show|rotate|pin <host> <public-key>|token <user-key-file> <machine>>";

/// `fno-agents mail-remote key ...`: read, rotate and pin machine keys, and
/// mint a machine's store token.
pub fn run(args: &[String]) -> i32 {
    let home = crate::paths::AgentsHome::from_env();
    let root = home.root().to_path_buf();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["show"] => own().map(|key| {
            let pins = load_pins(&pins_path(&root)).unwrap_or_default();
            let mut out = format!(
                "this machine: {} (key {})\n",
                key.public_key(),
                fingerprint(&key.public_key())
            );
            for (host, key) in &pins {
                out.push_str(&format!(
                    "pinned {host}: {key} (key {})\n",
                    fingerprint(key)
                ));
            }
            out
        }),
        ["rotate"] => home
            .ensure_root()
            .map_err(|e| e.to_string())
            .and_then(|()| MachineKey::rotate(&key_path(&root)))
            .map(|key| {
                format!(
                    "new key {} (key {}). Each peer refuses it until the user runs there: \
                     fno-agents mail-remote key pin {} {}\n",
                    key.public_key(),
                    fingerprint(&key.public_key()),
                    crate::mail_bus::normalize_host(&crate::claims::hostname()),
                    key.public_key()
                )
            }),
        ["pin", host, public_key] => {
            let host = crate::mail_bus::normalize_host(host);
            pin(&pins_path(&root), &host, public_key)
                .map(|()| format!("pinned {host} to key {}\n", fingerprint(public_key)))
        }
        ["token", user_key, machine] => {
            let path = Path::new(user_key);
            let created = !path.exists();
            MachineKey::load_or_create(path).and_then(|key| {
                let pem_path = path.with_extension("pub.pem");
                let raw = B64.decode(key.public_key()).map_err(|e| e.to_string())?;
                std::fs::write(&pem_path, public_pem(&raw))
                    .map_err(|e| format!("{}: {e}", pem_path.display()))?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                eprintln!(
                    "{} the user key {}; sqld checks tokens with --auth-jwt-key-file {}. \
                     Set the token below as store.remote_token on {machine} only.",
                    if created { "created" } else { "read" },
                    path.display(),
                    pem_path.display()
                );
                Ok(format!("{}\n", mint_token(&key, machine, now)))
            })
        }
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("mail-remote key: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_key_is_made_once_owner_only_and_a_different_key_for_a_pinned_host_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(dir.path());
        let first = MachineKey::load_or_create(&path).unwrap();
        let again = MachineKey::load_or_create(&path).unwrap();
        assert_eq!(first.public_key(), again.public_key());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let pins = pins_path(dir.path());
        let bytes = b"row bytes";
        let signed = |key: &MachineKey| (key.public_key(), key.sign(bytes));
        let (pk, sig) = signed(&first);
        assert_eq!(
            check(&pins, "imac", Some((&pk, &sig)), bytes),
            Ok(Trust::FirstUse)
        );
        assert_eq!(
            check(&pins, "imac", Some((&pk, &sig)), bytes),
            Ok(Trust::Pinned)
        );
        // Edited bytes after signing never verify.
        let untrusted = |r| matches!(r, Err(Refusal::Untrusted(_)));
        assert!(untrusted(check(
            &pins,
            "imac",
            Some((&pk, &sig)),
            b"edited"
        )));
        // A pinned host's unsigned row refuses; an unpinned host's passes.
        assert!(untrusted(check(&pins, "imac", None, bytes)));
        assert_eq!(check(&pins, "r2d2", None, bytes), Ok(Trust::Unsigned));

        // A rotated key refuses, naming the host and both fingerprints,
        // until the user re-pins.
        let rotated = MachineKey::rotate(&path).unwrap();
        let (pk2, sig2) = signed(&rotated);
        let Err(Refusal::Untrusted(error)) = check(&pins, "imac", Some((&pk2, &sig2)), bytes)
        else {
            panic!("a rotated key must refuse");
        };
        assert!(
            error.contains("imac")
                && error.contains(&fingerprint(&pk))
                && error.contains(&fingerprint(&pk2)),
            "{error}"
        );
        pin(&pins, "imac", &pk2).unwrap();
        assert_eq!(
            check(&pins, "imac", Some((&pk2, &sig2)), bytes),
            Ok(Trust::Pinned)
        );
        // An unreadable pin file is this machine's fault: the row waits.
        std::fs::write(&pins, "not json").unwrap();
        assert!(matches!(
            check(&pins, "imac", Some((&pk2, &sig2)), bytes),
            Err(Refusal::Store(_))
        ));

        // The store token verifies against the key that minted it.
        let token = mint_token(&rotated, "imac", 1);
        let (input, sig) = token.rsplit_once('.').unwrap();
        assert!(verify(&pk2, input.as_bytes(), sig));
    }
}

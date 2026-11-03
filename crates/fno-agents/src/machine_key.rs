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
//!
//! The same user key signs a user certificate: one user name bound to one
//! machine key. A machine that holds its own certificate signs the wide law
//! it records, and binds peer wide law only when that user key proves it.

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

pub fn cert_path(root: &Path) -> PathBuf {
    root.join("user_cert.txt")
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
        let mut sibling = path.as_os_str().to_owned();
        sibling.push(".new");
        PathBuf::from(sibling)
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

/// The tag a certificate signature covers, so a certificate never reads as
/// a store token the same user key signed.
const CERT_TAG: &[u8] = b"fno-user-cert-v1\n";

/// What a user certificate binds: a user name and user key to one machine key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCert {
    pub user: String,
    pub user_key: String,
    pub machine_key: String,
}

/// A certificate line, `<payload>.<signature>`, both base64url: the user
/// key's signature over the user name and `machine_key`.
pub fn mint_cert(user_key: &MachineKey, user: &str, machine_key: &str, issued_at: u64) -> String {
    let payload = serde_json::json!({
        "v": 1,
        "user": user,
        "user_key": user_key.public_key(),
        "machine_key": machine_key,
        "iat": issued_at,
    })
    .to_string();
    let signed = [CERT_TAG, payload.as_bytes()].concat();
    format!("{}.{}", B64.encode(&payload), user_key.sign(&signed))
}

/// The certificate a line carries, when the user key it names signed it.
/// The user key is self-asserted: a reader decides whether to trust it.
pub fn read_cert(line: &str) -> Option<UserCert> {
    let (payload, signature) = line.trim().split_once('.')?;
    let bytes = B64.decode(payload).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("v") != Some(&serde_json::json!(1)) {
        return None;
    }
    let field = |key: &str| value.get(key)?.as_str().map(str::to_string);
    let cert = UserCert {
        user: field("user").filter(|u| crate::identity::is_sender_token(u))?,
        user_key: field("user_key")?,
        machine_key: field("machine_key")?,
    };
    verify(&cert.user_key, &[CERT_TAG, &bytes].concat(), signature).then_some(cert)
}

/// The certificate installed under `root`, with its line, when its user key
/// signed it. It may name an older machine key: `rotate` leaves it in place.
pub fn installed_cert(root: &Path) -> Option<(String, UserCert)> {
    let line = std::fs::read_to_string(cert_path(root)).ok()?;
    let line = line.trim().to_string();
    read_cert(&line).map(|cert| (line, cert))
}

/// Install `line` at `path` when it verifies and names `own_key`. A refusal
/// writes nothing.
fn install_cert(path: &Path, line: &str, own_key: &str) -> Result<String, String> {
    let cert = read_cert(line).ok_or_else(|| {
        "the certificate does not verify: the user key it names did not sign it".to_string()
    })?;
    if cert.machine_key != own_key {
        return Err(format!(
            "the certificate is for machine key {}, but this machine's key is {}. Mint one \
             for this machine where the user key lives: fno-agents mail-remote key certify \
             <user-key-file> {} {own_key}",
            fingerprint(&cert.machine_key),
            fingerprint(own_key),
            cert.user
        ));
    }
    write_private(path, format!("{}\n", line.trim()).as_bytes(), true)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!(
        "installed: this machine is {} (user key {})\n",
        cert.user,
        fingerprint(&cert.user_key)
    ))
}

/// The `show` line for the installed certificate.
fn cert_line(root: &Path, own_key: &str) -> String {
    match installed_cert(root) {
        Some((_, cert)) if cert.machine_key == own_key => format!(
            "user: {} (user key {})\n",
            cert.user,
            fingerprint(&cert.user_key)
        ),
        Some((_, cert)) => format!(
            "user: {} (user key {}), but the certificate names machine key {}; it signs \
             nothing until a new one is installed\n",
            cert.user,
            fingerprint(&cert.user_key),
            fingerprint(&cert.machine_key)
        ),
        None if cert_path(root).exists() => format!(
            "no user certificate: {} does not verify\n",
            cert_path(root).display()
        ),
        None => "no user certificate\n".to_string(),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

const USAGE: &str = "usage: fno-agents mail-remote key <show|rotate|pin <host> <public-key>|token <user-key-file> <machine>|certify <user-key-file> <user> <machine-public-key>|cert <certificate>>";

/// `fno-agents mail-remote key ...`: read, rotate and pin machine keys, mint
/// a machine's store token, and mint and install user certificates.
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
            out.push_str(&cert_line(&root, &key.public_key()));
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
                let mut out = format!(
                    "new key {} (key {}). Each peer refuses it until the user runs there: \
                     fno-agents mail-remote key pin {} {}\n",
                    key.public_key(),
                    fingerprint(&key.public_key()),
                    crate::mail::address::normalize_host(&crate::claims::hostname()),
                    key.public_key()
                );
                if let Some((_, cert)) = installed_cert(&root) {
                    out.push_str(&format!(
                        "The user certificate names the old key, so wide law refuses here \
                         until a new one is installed: fno-agents mail-remote key certify \
                         <user-key-file> {} {}\n",
                        cert.user,
                        key.public_key()
                    ));
                }
                out
            }),
        ["pin", host, public_key] => {
            let host = crate::mail::address::normalize_host(host);
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
                let now = now_secs();
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
        ["certify", user_key, user, machine_key] => {
            if !crate::identity::is_sender_token(user) {
                Err(format!(
                    "{user:?} is not a plain user name (letters, digits, '-', '_', '.')"
                ))
            } else if B64.decode(machine_key).map(|k| k.len()) != Ok(32) {
                Err(format!(
                    "{machine_key:?} is not a base64url Ed25519 public key; \
                     `fno-agents mail-remote key show` prints it on that machine"
                ))
            } else {
                let path = Path::new(user_key);
                let created = !path.exists();
                MachineKey::load_or_create(path).map(|key| {
                    eprintln!(
                        "{} the user key {} (user key {}). Keep it off the fleet. On the \
                         machine with key {}, run: fno-agents mail-remote key cert <line>",
                        if created { "created" } else { "read" },
                        path.display(),
                        fingerprint(&key.public_key()),
                        fingerprint(machine_key)
                    );
                    format!("{}\n", mint_cert(&key, user, machine_key, now_secs()))
                })
            }
        }
        ["cert", line] => home
            .ensure_root()
            .map_err(|e| e.to_string())
            .and_then(|()| own())
            .and_then(|key| install_cert(&cert_path(&root), line, &key.public_key())),
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

    #[test]
    fn a_user_certificate_installs_only_for_this_machine_key_and_proves_the_user() {
        use crate::claims::EnvVarGuard;
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set(crate::paths::HOME_ENV, dir.path().to_str().unwrap());
        let user_key = MachineKey::load_or_create(&dir.path().join("user.pk8")).unwrap();
        let own_key = own().unwrap().public_key();
        let other = MachineKey::load_or_create(&dir.path().join("other.pk8")).unwrap();
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();

        // A certificate for another machine key refuses, naming both keys,
        // and writes nothing.
        let foreign = mint_cert(&user_key, "jason", &other.public_key(), 1);
        assert_eq!(run(&argv(&["cert", foreign.as_str()])), 1);
        assert!(!cert_path(dir.path()).exists());
        let error = install_cert(&cert_path(dir.path()), &foreign, &own_key).unwrap_err();
        assert!(
            error.contains(&fingerprint(&other.public_key()))
                && error.contains(&fingerprint(&own_key)),
            "{error}"
        );
        // A whole payload with one field edited keeps every key, so only the
        // signature check can refuse it.
        let (payload, sig) = foreign.split_once('.').unwrap();
        let text = String::from_utf8(B64.decode(payload).unwrap()).unwrap();
        let edited = text.replace("\"jason\"", "\"mallory\"");
        assert_ne!(text, edited);
        assert!(read_cert(&format!("{}.{sig}", B64.encode(&edited))).is_none());
        assert!(read_cert(&foreign).is_some());
        assert!(crate::identity::proven_user().is_none());

        // This machine's certificate installs, proves the user, and shows.
        let mine = mint_cert(&user_key, "jason", &own_key, 1);
        assert_eq!(run(&argv(&["cert", mine.as_str()])), 0);
        let proven = crate::identity::proven_user().expect("the installed certificate proves");
        assert_eq!(proven.user, "jason");
        assert_eq!(
            crate::identity::trusted_user_key(),
            Some(user_key.public_key())
        );
        assert_eq!(crate::identity::person(), "jason");
        let shown = cert_line(dir.path(), &own_key);
        assert!(
            shown.contains("jason") && shown.contains(&fingerprint(&user_key.public_key())),
            "{shown}"
        );
    }
}

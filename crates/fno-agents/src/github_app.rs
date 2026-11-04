//! The fleet's GitHub App identity: an installation token carries its own
//! REST quota (15,000 per hour on an Enterprise Cloud install), so the
//! fleet's poll surfaces never drain the operator's user token. The app is
//! machine-global config in `~/.fno/config.toml`:
//! `github.app_id`, `github.app_installation_id`, and `github.app_key_path`
//! (default `~/.fno/secrets/github-app.pem`, mode 0600). With no app
//! configured nothing changes: every caller keeps the user token.
//!
//! The key and the token are never logged and never reach argv: the mint
//! POST rides curl's stdin config (`-K -`), and the minted token lives in a
//! 0600 cache file under the machine locks dir. `inject` is called at the
//! poll-surface spawn points only (pr status, pr-watch, CI reads, merge);
//! identity surfaces (pr create, review posts, comments) and `git push`
//! never see the token, so authorship stays with the user.

use serde_json::Value;
use std::ffi::OsStr;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Reuse a minted token until this close to GitHub's one-hour expiry.
const EXPIRY_SLOP_S: i64 = 300;
/// GitHub grants an installation token one hour.
const TOKEN_TTL_S: i64 = 3600;
/// A JWT lives at most ten minutes per GitHub's App auth docs.
const JWT_TTL_S: i64 = 540;
/// JWT `iat` sits a minute back, for clock drift toward GitHub.
const JWT_BACKDATE_S: i64 = 60;
/// The mint POST is one small call; hanging a poll tick on it is worse than
/// falling back to the user token for one tick.
const MINT_TIMEOUT_SECS: &str = "15";

pub(crate) struct AppConfig {
    app_id: String,
    installation_id: String,
    key_path: PathBuf,
}

struct Minted {
    token: String,
    expires_at_s: i64,
}

/// Put the installation token on a gh child's env. A caller that already
/// set or removed GH_TOKEN decided; an operator-exported GH_TOKEN or
/// GITHUB_TOKEN outranks the app; `FNO_GITHUB_APP=0` turns the whole thing
/// off for one run. Every failure path keeps the child on whatever auth it
/// would have had, so a mint outage degrades to today's behavior.
pub(crate) fn inject(cmd: &mut Command, program: &OsStr) {
    if program != OsStr::new("gh") {
        return;
    }
    inject_with(cmd, installation_token().as_deref());
}

fn inject_with(cmd: &mut Command, token: Option<&str>) {
    let Some(token) = token else {
        return;
    };
    if cmd.get_envs().any(|(key, _)| key == OsStr::new("GH_TOKEN")) {
        return;
    }
    if std::env::var_os("GH_TOKEN").is_some() || std::env::var_os("GITHUB_TOKEN").is_some() {
        return;
    }
    if std::env::var("FNO_GITHUB_APP").as_deref() == Ok("0") {
        return;
    }
    cmd.env("GH_TOKEN", token);
}

fn installation_token() -> Option<String> {
    let cfg = app_config()?;
    let notice = |reason: &str| notice_fallback_once(reason);
    token_from(
        Some(cfg),
        &cache_path(),
        crate::gh_budget::now_ms(),
        |cfg, now| {
            mint(cfg, now)
                .map_err(|e| {
                    notice(&e);
                    e
                })
                .ok()
        },
    )
}

fn app_config() -> Option<AppConfig> {
    let app_id = config_string(&["github", "app_id"])?;
    let installation_id = config_string(&["github", "app_installation_id"])?;
    let key_path = config_string(&["github", "app_key_path"])
        .map(|p| expand_tilde(Path::new(&p)))
        .unwrap_or_else(default_key_path);
    Some(AppConfig {
        app_id,
        installation_id,
        key_path,
    })
}

/// Rust path handling never expands `~`, so a configured `~/...` key path
/// resolves against HOME here.
pub(crate) fn expand_tilde(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

fn config_string(keys: &[&str]) -> Option<String> {
    let value = crate::agents_config::config_lookup_global(keys)?;
    match &value {
        toml::Value::String(s) => {
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        toml::Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

fn default_key_path() -> PathBuf {
    let fallback = std::env::temp_dir().join("github-app.pem");
    std::env::var_os("HOME")
        .map(|home| {
            PathBuf::from(home)
                .join(".fno")
                .join("secrets")
                .join("github-app.pem")
        })
        .unwrap_or(fallback)
}

fn cache_path() -> PathBuf {
    let fallback = std::env::temp_dir().join("fno-locks");
    crate::agents_config::machine_locks_dir()
        .unwrap_or(fallback)
        .join("github-app-token.json")
}

/// The one decision point every caller shares: a valid cache answer wins,
/// else the mint runs and its answer is cached. `mint` is a seam so tests
/// prove the whole path without a live POST.
fn token_from(
    cfg: Option<AppConfig>,
    cache: &Path,
    now_ms: i64,
    mint: impl Fn(&AppConfig, i64) -> Option<String>,
) -> Option<String> {
    let cfg = cfg?;
    let now_s = now_ms / 1000;
    if let Some(cached) = read_cache(cache) {
        if now_s < cached.expires_at_s - EXPIRY_SLOP_S {
            return Some(cached.token);
        }
    }
    let minted_json = mint(&cfg, now_ms)?;
    let minted: Minted = match serde_json::from_str::<Value>(&minted_json) {
        Ok(v) => Minted {
            token: v.get("token")?.as_str()?.to_string(),
            expires_at_s: now_s + TOKEN_TTL_S,
        },
        Err(_) => return None,
    };
    if minted.token.is_empty() {
        return None;
    }
    write_cache(cache, &minted);
    Some(minted.token)
}

fn read_cache(path: &Path) -> Option<Minted> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some(Minted {
        token: v.get("token")?.as_str()?.to_string(),
        expires_at_s: v.get("expires_at")?.as_i64()?,
    })
}

fn write_cache(path: &Path, minted: &Minted) {
    let body = serde_json::json!({
        "token": minted.token,
        "expires_at": minted.expires_at_s,
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(f, "{body}");
    }
}

/// The production mint: read the PEM, sign a 10-minute JWT, POST it to the
/// installation access-tokens endpoint. The JWT never reaches argv.
fn mint(cfg: &AppConfig, now_ms: i64) -> Result<String, String> {
    let der = read_key_der(&cfg.key_path)?;
    let jwt = sign_jwt(&der, &cfg.app_id, now_ms)?;
    let (code, body) = post_via_curl(&jwt, &cfg.installation_id)?;
    if !(200..300).contains(&code) {
        return Err(format!("mint POST answered http {code}"));
    }
    Ok(body)
}

fn read_key_der(path: &Path) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let pem = std::fs::read_to_string(path).map_err(|e| format!("key unreadable: {e}"))?;
    let body = pem_body(&pem)?;
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|e| format!("key pem does not decode: {e}"))?;
    if pem.contains("BEGIN RSA PRIVATE KEY") {
        // GitHub hands out PKCS#1; ring signs from PKCS#8 only.
        Ok(pkcs1_to_pkcs8(&der))
    } else if pem.contains("BEGIN PRIVATE KEY") {
        Ok(der)
    } else {
        Err("key pem is neither PKCS#1 nor PKCS#8".to_string())
    }
}

fn pem_body(pem: &str) -> Result<String, String> {
    // Only the armor delimiter lines go; a base64 body line can carry the
    // substring BEGIN or END and would corrupt the key if dropped by a
    // substring match.
    let inner = pem
        .lines()
        .filter(|line| !line.trim_start().starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    (!inner.is_empty())
        .then_some(inner)
        .ok_or_else(|| "key pem has no body".to_string())
}

/// Wrap PKCS#1 RSAPrivateKey DER into the PrivateKeyInfo (PKCS#8) frame:
/// `SEQUENCE { version 0, rsaEncryption AlgId, OCTET STRING { pkcs1 } }`.
/// The private key rides an OCTET STRING (RFC 5208); the BIT STRING form
/// belongs to SubjectPublicKeyInfo, and ring's unwrap refuses it.
fn pkcs1_to_pkcs8(pkcs1: &[u8]) -> Vec<u8> {
    const RSA_ENCRYPTION_ALG_ID: [u8; 15] = [
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let mut inner = vec![0x02, 0x01, 0x00];
    inner.extend_from_slice(&RSA_ENCRYPTION_ALG_ID);
    inner.extend_from_slice(&tlv(0x04, pkcs1));
    tlv(0x30, &inner)
}

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&der_len(body.len()));
    out.extend_from_slice(body);
    out
}

fn der_len(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else if len <= 0xFF {
        vec![0x81, len as u8]
    } else {
        vec![0x82, (len >> 8) as u8, (len & 0xFF) as u8]
    }
}

fn sign_jwt(pkcs8_der: &[u8], app_id: &str, now_ms: i64) -> Result<String, String> {
    let key = ring::signature::RsaKeyPair::from_pkcs8(pkcs8_der)
        .map_err(|e| format!("key rejected: {e}"))?;
    let now_s = now_ms / 1000;
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = b64url(
        serde_json::json!({
            "iss": app_id,
            "iat": now_s - JWT_BACKDATE_S,
            "exp": now_s - JWT_BACKDATE_S + JWT_TTL_S,
        })
        .to_string()
        .as_bytes(),
    );
    let signing_input = format!("{header}.{claims}");
    let mut sig = vec![0u8; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signing_input.as_bytes(),
        &mut sig,
    )
    .map_err(|e| format!("jwt sign failed: {e}"))?;
    Ok(format!("{signing_input}.{}", b64url(&sig)))
}

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// POST the mint request with the JWT in curl's stdin config, so no secret
/// is visible in `ps` output or in the environment.
fn post_via_curl(jwt: &str, installation_id: &str) -> Result<(u16, String), String> {
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
    let auth = format!("Authorization: Bearer {jwt}");
    let config = format!(
        "silent\nshow-error\nmax-time = {MINT_TIMEOUT_SECS}\nheader = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nheader = \"{}\"\nwrite-out = \"\\n%{{http_code}}\"\n",
        auth.replace('"', "\\\"")
    );
    let mut child = Command::new("curl")
        .arg("-K")
        .arg("-")
        .arg("-X")
        .arg("POST")
        .arg(&url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl spawn failed: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("curl stdin unavailable")?
        .write_all(config.as_bytes())
        .map_err(|e| format!("curl config write failed: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl wait failed: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = match text.rsplit_once('\n') {
        Some((body, code)) => (body.to_string(), code.trim().parse::<u16>().unwrap_or(0)),
        None => (text.clone(), 0),
    };
    Ok((code, body))
}

fn notice_fallback_once(reason: &str) {
    static NOTICED: OnceLock<()> = OnceLock::new();
    if NOTICED.set(()).is_ok() {
        eprintln!("github-app: falling back to the user token ({reason})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        &LOCK
    }

    /// A throwaway 2048-bit key generated per run: nothing secret is
    /// committed, and the wrap/sign/verify chain is exercised end to end.
    fn generated_pem() -> String {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let mut rng = rand::rngs::OsRng;
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("keygen");
        key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .expect("encode pem")
            .to_string()
    }

    fn temp_cfg(dir: &Path) -> (AppConfig, PathBuf) {
        let pem_path = dir.join("app.pem");
        std::fs::write(&pem_path, generated_pem()).expect("write pem");
        (
            AppConfig {
                app_id: "1111111".to_string(),
                installation_id: "2222222".to_string(),
                key_path: pem_path.clone(),
            },
            dir.join("token-cache.json"),
        )
    }

    #[test]
    fn pkcs1_pem_signs_a_jwt_that_verifies_with_the_public_key() {
        let dir = std::env::temp_dir().join("fno-github-app-test-sign");
        std::fs::create_dir_all(&dir).unwrap();
        let pem = generated_pem();
        std::fs::write(dir.join("k.pem"), &pem).unwrap();
        let der = read_key_der(&dir.join("k.pem")).expect("key read");
        let jwt = sign_jwt(&der, "1111111", 1_700_000_000_000).expect("sign");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "compact JWT");
        let claims = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap(),
        )
        .unwrap();
        assert!(claims.contains("\"iss\":\"1111111\""), "{claims}");
        assert!(claims.contains("\"iat\":1699999940"), "{claims}");
        assert!(claims.contains("\"exp\":1700000480"), "{claims}");
        // The signature verifies under the public half of the same key.
        use rsa::pkcs1::DecodeRsaPrivateKey;
        use rsa::traits::PublicKeyParts;
        let key = rsa::RsaPrivateKey::from_pkcs1_pem(&pem).expect("parse");
        let (n, e) = (key.n().to_bytes_be(), key.e().to_bytes_be());
        let msg = format!("{}.{}", parts[0], parts[1]);
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
            .verify(
                &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                msg.as_bytes(),
                &sig,
            )
            .expect("signature verifies");
    }

    #[test]
    fn token_path_is_chosen_when_the_key_exists_without_a_live_call() {
        let dir = std::env::temp_dir().join("fno-github-app-test-mint");
        std::fs::create_dir_all(&dir).unwrap();
        let (cfg, cache) = temp_cfg(&dir);
        let posts = Mutex::new(0usize);
        let seen_jwt = Mutex::new(String::new());
        let token = token_from(Some(cfg), &cache, 1_700_000_000_000, |cfg, _now| {
            *posts.lock().unwrap() += 1;
            assert_eq!(cfg.installation_id, "2222222");
            let jwt = sign_jwt(&read_key_der(&cfg.key_path).unwrap(), &cfg.app_id, _now)
                .expect("test sign");
            *seen_jwt.lock().unwrap() = jwt.clone();
            assert_eq!(jwt.split('.').count(), 3);
            Some(
                serde_json::json!({"token": "ghs_testtoken", "expires_at": "irrelevant"})
                    .to_string(),
            )
        });
        assert_eq!(token.as_deref(), Some("ghs_testtoken"));
        assert_eq!(*posts.lock().unwrap(), 1, "one mint, no live POST");
        assert!(*seen_jwt.lock().unwrap() != "", "the mint carried a JWT");
        let mode = std::fs::metadata(&cache).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "cache file is 0600");
    }

    #[test]
    fn cache_answers_until_five_minutes_before_expiry_then_re_mints() {
        let dir = std::env::temp_dir().join("fno-github-app-test-cache");
        std::fs::create_dir_all(&dir).unwrap();
        let (cfg, cache) = temp_cfg(&dir);
        let posts = Mutex::new(0usize);
        let mint = |cfg: &AppConfig, _now: i64| -> Option<String> {
            *posts.lock().unwrap() += 1;
            let _ = cfg;
            Some(serde_json::json!({"token": "ghs_cached"}).to_string())
        };
        let now = 1_700_000_000_000;
        let first = token_from(Some(clone_cfg(&cfg)), &cache, now, mint);
        assert_eq!(first.as_deref(), Some("ghs_cached"));
        // 50 minutes in: still inside the slop window, served from cache.
        let second = token_from(Some(clone_cfg(&cfg)), &cache, now + 50 * 60 * 1000, mint);
        assert_eq!(second.as_deref(), Some("ghs_cached"));
        assert_eq!(*posts.lock().unwrap(), 1, "cache served the second read");
        // 56 minutes in: inside the 5-minute slop, so the token re-mints.
        let third = token_from(Some(clone_cfg(&cfg)), &cache, now + 56 * 60 * 1000, mint);
        assert_eq!(third.as_deref(), Some("ghs_cached"));
        assert_eq!(*posts.lock().unwrap(), 2, "expired cache re-minted");
    }

    fn clone_cfg(cfg: &AppConfig) -> AppConfig {
        AppConfig {
            app_id: cfg.app_id.clone(),
            installation_id: cfg.installation_id.clone(),
            key_path: cfg.key_path.clone(),
        }
    }

    #[test]
    fn unconfigured_or_keyless_falls_back_without_writing_cache() {
        let dir = std::env::temp_dir().join("fno-github-app-test-fallback");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("token-cache.json");
        let none = token_from(None, &cache, 0, |_cfg, _now| {
            panic!("no mint may run without config")
        });
        assert!(none.is_none());
        let missing = AppConfig {
            app_id: "1".to_string(),
            installation_id: "2".to_string(),
            key_path: dir.join("absent.pem"),
        };
        let failed = token_from(Some(missing), &cache, 0, |cfg, _now| {
            // The production mint reads the key before any POST; the seam
            // mirrors that order so the missing-key refusal proves no POST.
            match read_key_der(&cfg.key_path) {
                Ok(_) => panic!("absent key must not read"),
                Err(_) => None,
            }
        });
        assert!(failed.is_none());
        assert!(!cache.exists(), "no cache row without a mint");
    }

    #[test]
    fn inject_respects_a_caller_decision_an_operator_token_and_the_kill_switch() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        // A caller that set or removed GH_TOKEN decided; the app never overrides.
        let mut decided = Command::new("gh");
        decided.env("GH_TOKEN", "callers-own");
        inject_with(&mut decided, Some("app-token"));
        assert_eq!(
            decided
                .get_envs()
                .find(|(k, _)| **k == *OsStr::new("GH_TOKEN"))
                .map(|(_, v)| v.unwrap().to_string_lossy().into_owned())
                .as_deref(),
            Some("callers-own")
        );
        // An operator-exported token outranks the app.
        std::env::set_var("GH_TOKEN", "operator-token");
        let mut operator = Command::new("gh");
        inject_with(&mut operator, Some("app-token"));
        assert!(operator
            .get_envs()
            .all(|(k, _)| k != OsStr::new("GH_TOKEN")));
        std::env::remove_var("GH_TOKEN");
        // The kill switch leaves the child untouched.
        std::env::set_var("FNO_GITHUB_APP", "0");
        let mut killed = Command::new("gh");
        inject_with(&mut killed, Some("app-token"));
        std::env::remove_var("FNO_GITHUB_APP");
        assert!(killed.get_envs().all(|(k, _)| k != OsStr::new("GH_TOKEN")));
        // No app token means no change, which is today's behavior.
        let mut plain = Command::new("gh");
        inject_with(&mut plain, None);
        assert!(plain.get_envs().all(|(k, _)| k != OsStr::new("GH_TOKEN")));
    }
}

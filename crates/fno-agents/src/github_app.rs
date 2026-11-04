//! The fleet's GitHub App identity, scoped per repository: an installation
//! token carries its own REST quota (15,000 per hour on an Enterprise Cloud
//! install), so the fleet's poll surfaces never drain the operator's user
//! token. The app is machine-global config in `~/.fno/config.toml`:
//! `github.app_id` and `github.app_key_path` (default
//! `~/.fno/secrets/github-app.pem`, mode 0600); `github.app_installation_id`
//! is an optional pin. With no app configured nothing changes: every caller
//! keeps the user token.
//!
//! Injection is scoped before it happens. `inject` reads the gh command's
//! target repo (`-R`/`--repo`, an `api repos/OWNER/REPO` endpoint, or the
//! cwd's git remote) and resolves the installation that owns it: the pinned
//! installation id when config pins one, else the app's installations list
//! (`GET /app/installations`, JWT auth) matched on the repo owner. The
//! token is placed only when that installation's own repository list
//! (`GET /installation/repositories`) contains the repo; a repo whose owner
//! has no installation, or that sits outside the list, keeps the user
//! token. One app installed on several owners then covers every repo with
//! no new config, and each installation spends its own quota.
//!
//! The caches live with the tokens under the machine locks dir: one
//! installations list (owner -> installation id) and one token+repo-list
//! file per installation, so a cache answer and its scope agree by
//! construction. A 404-shaped failure from a child that carried an
//! installation token retries once with the caller's own auth
//! (`should_retry_without_app`), covering the window where a cached
//! installation went stale.
//!
//! The key and the tokens are never logged and never reach argv: the mint
//! POST rides curl's stdin config (`-K -`), and minted tokens live in 0600
//! cache files. `inject` is called at the poll-surface spawn points only
//! (pr status, pr-watch, CI reads, merge); identity surfaces (pr create,
//! review posts, comments) and `git push` never see the token, so
//! authorship stays with the user.

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
/// The installations list and the repo lists change rarely; an hour of
/// staleness costs a user-token fallback, never a wrong answer.
const INSTALLS_TTL_S: i64 = 3600;
/// Page bound for GitHub's paged list endpoints: 1000 installations or
/// repos, past which a surprise pagination cannot loop a poll tick forever.
const LIST_PAGE_CAP: u32 = 10;
/// The cwd remote read rides the poll spawn path, so a wedged `git config`
/// costs this bound and falls back to the user token, never a hung fire.
const CWD_READ_BOUND: std::time::Duration = std::time::Duration::from_secs(2);

pub(crate) struct AppConfig {
    app_id: String,
    key_path: PathBuf,
    /// An operator pin: use this installation for every repo it can see,
    /// skipping the per-owner resolution.
    installation_pin: Option<String>,
}

/// One row of the app's installations list: the installation id and the
/// account (user or org) it is installed on.
#[derive(Clone)]
struct InstallRef {
    id: String,
    login: String,
}

/// An installation's token and the repo list it was minted for, cached
/// together so a cache answer and its scope agree by construction.
struct InstallAuth {
    token: String,
    repos: Vec<String>,
}

impl InstallAuth {
    /// Repo slugs are normalized at every entry point, so the comparison
    /// is exact and lowercase on both sides.
    fn sees(&self, repo: &str) -> bool {
        self.repos.iter().any(|listed| listed == repo)
    }
}

/// Put an installation token on a gh child's env when the command's target
/// repo is visible to its owner's installation. Returns whether the token
/// was placed, so a caller can retry a 404-shaped failure once without it.
/// A caller that already set or removed GH_TOKEN decided; an
/// operator-exported GH_TOKEN or GITHUB_TOKEN outranks the app;
/// `FNO_GITHUB_APP=0` turns the whole thing off for one run. Every refusal
/// path keeps the child on whatever auth it would have had, so a mint
/// outage or an unknown repo degrades to today's behavior.
pub(crate) fn inject(cmd: &mut Command, program: &OsStr) -> bool {
    if program != OsStr::new("gh") {
        return false;
    }
    inject_with(cmd, injection_decision(cmd).as_deref())
}

fn inject_with(cmd: &mut Command, token: Option<&str>) -> bool {
    let Some(token) = token else {
        return false;
    };
    if cmd.get_envs().any(|(key, _)| key == OsStr::new("GH_TOKEN")) {
        return false;
    }
    if std::env::var_os("GH_TOKEN").is_some() || std::env::var_os("GITHUB_TOKEN").is_some() {
        return false;
    }
    if std::env::var("FNO_GITHUB_APP").as_deref() == Ok("0") {
        return false;
    }
    cmd.env("GH_TOKEN", token);
    true
}

/// The injection decision for a built gh command: the installation token
/// when the target repo is visible to its owner's installation, None keeps
/// the child's own auth. Every cheap refusal runs before any subprocess:
/// the guard reads, then config, then the repo resolution (a bounded git
/// read), then the installation resolution (network).
fn injection_decision(cmd: &Command) -> Option<String> {
    decision_with(cmd, resolve_installation_auth)
}

fn decision_with(
    cmd: &Command,
    resolve: impl Fn(&AppConfig, &str) -> Option<InstallAuth>,
) -> Option<String> {
    if cmd.get_envs().any(|(key, _)| key == OsStr::new("GH_TOKEN")) {
        return None;
    }
    if std::env::var_os("GH_TOKEN").is_some() || std::env::var_os("GITHUB_TOKEN").is_some() {
        return None;
    }
    if std::env::var("FNO_GITHUB_APP").as_deref() == Ok("0") {
        return None;
    }
    let cfg = app_config()?;
    let repo = target_repo(cmd)?;
    let owner = repo.split('/').next()?;
    let auth = resolve(&cfg, owner)?;
    auth.sees(&repo).then_some(auth.token)
}

/// The production resolver: the pinned installation when config pins one,
/// else the repo owner's installation from the app's installations list.
fn resolve_installation_auth(cfg: &AppConfig, owner: &str) -> Option<InstallAuth> {
    let id = match &cfg.installation_pin {
        Some(id) => id.clone(),
        None => installation_id_for_owner(cfg, owner)?,
    };
    install_auth(cfg, &id)
}

/// The target repo of a gh command, from `-R`/`--repo` or an `api
/// repos/OWNER/REPO` endpoint, else the cwd's origin/upstream remote - the
/// same order gh itself resolves. None keeps the child's own auth.
fn target_repo(cmd: &Command) -> Option<String> {
    repo_from_args(cmd.get_args()).or_else(|| cmd.get_current_dir().and_then(repo_from_cwd))
}

fn repo_from_args<'a>(args: impl Iterator<Item = &'a OsStr>) -> Option<String> {
    let args: Vec<String> = args.map(|a| a.to_string_lossy().into_owned()).collect();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "-R" || arg == "--repo" {
            return args.get(i + 1).and_then(|v| normalize_repo(v));
        }
        if let Some(value) = arg.strip_prefix("--repo=") {
            return normalize_repo(value);
        }
        if arg.len() > 2 && arg.starts_with("-R") {
            return normalize_repo(&arg[2..]);
        }
        if arg == "api" {
            // The endpoint is the first positional after `api`; method and
            // header flags may sit between, so try every candidate until
            // one carries a repos path.
            return args[i + 1..]
                .iter()
                .filter(|v| !v.starts_with('-'))
                .find_map(|v| repo_from_api_path(v));
        }
        i += 1;
    }
    None
}

/// `repos/OWNER/REPO` out of a `gh api` endpoint: a bare path, a
/// leading-slash path, or a full URL all carry the segment. Template
/// placeholders (`:owner`) never resolve.
fn repo_from_api_path(endpoint: &str) -> Option<String> {
    let segs: Vec<&str> = endpoint.split('/').filter(|s| !s.is_empty()).collect();
    let i = segs.iter().position(|s| *s == "repos")?;
    let owner = segs.get(i + 1)?;
    let repo = segs.get(i + 2)?;
    if owner.contains(':') || repo.contains(':') {
        return None;
    }
    normalize_repo(&format!("{owner}/{repo}"))
}

/// One canonical repo slug: lowercase `owner/repo` with `.git` gone. A
/// `-R` value may carry a host or full URL (`github.com/owner/repo`), a
/// remote URL may be scp-form (`git@github.com:owner/repo.git`); anything
/// without a github.com anchor or a bare two-segment slug is not a
/// github.com repo, and an anchor not followed by `/` or `:` is a
/// lookalike host, not GitHub.
fn normalize_repo(value: &str) -> Option<String> {
    let text = value.trim();
    let anchored = match text.split_once("github.com") {
        Some((_, rest)) if rest.starts_with(':') || rest.starts_with('/') => {
            Some(rest.trim_start_matches([':', '/']).to_string())
        }
        _ => None,
    };
    let body = match anchored {
        Some(rest) => rest,
        None => {
            let segs: Vec<&str> = text.split('/').filter(|s| !s.is_empty()).collect();
            if segs.len() != 2 {
                return None;
            }
            segs.join("/")
        }
    };
    let segs: Vec<&str> = body.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() != 2 {
        return None;
    }
    let (owner, repo) = (segs[0], segs[1]);
    let plain = |s: &str| !s.is_empty() && !s.contains(':') && !s.contains('%');
    if !plain(owner) || !plain(repo) {
        return None;
    }
    Some(format!(
        "{}/{}",
        owner.to_lowercase(),
        repo.trim_end_matches(".git").to_lowercase()
    ))
}

/// The repo behind a working tree, from its origin or upstream remote -
/// upstream loses to origin so a fork checkout names its fork.
fn repo_from_cwd(cwd: &Path) -> Option<String> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new("git")
        .args(["-C"])
        .arg(cwd)
        .args([
            "config",
            "--get-regexp",
            r"^remote\.(origin|upstream)\.url$",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + CWD_READ_BOUND;
    let done = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    let Some(_status) = done else {
        crate::bounded_spawn::killpg(child.id() as i32);
        let _ = child.wait();
        return None;
    };
    // The read's output is two config lines at most, far under one pipe
    // buffer, so reading after the exit cannot deadlock.
    let mut stdout = child.stdout.take();
    let _ = child.wait();
    let mut buf = Vec::new();
    if let Some(pipe) = stdout.as_mut() {
        let _ = std::io::Read::read_to_end(pipe, &mut buf);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut origin = None;
    let mut upstream = None;
    for line in text.lines() {
        let Some((key, url)) = line.split_once(' ') else {
            continue;
        };
        let Some(slug) = normalize_repo(url) else {
            continue;
        };
        if key == "remote.origin.url" {
            origin = Some(slug);
        } else {
            upstream = Some(slug);
        }
    }
    origin.or(upstream)
}

fn app_config() -> Option<AppConfig> {
    let app_id = config_string(&["github", "app_id"])?;
    let key_path = config_string(&["github", "app_key_path"])
        .map(|p| expand_tilde(Path::new(&p)))
        .unwrap_or_else(default_key_path);
    let installation_pin = config_string(&["github", "app_installation_id"]);
    Some(AppConfig {
        app_id,
        key_path,
        installation_pin,
    })
}

/// The installation id of the app on `owner`, from the cached installations
/// list; a stale or missing list refetches it. No installation on that
/// owner means None - the child keeps the user token.
fn installation_id_for_owner(cfg: &AppConfig, owner: &str) -> Option<String> {
    let path = installs_cache_path();
    let now = crate::gh_budget::now_ms();
    let refs = installations_from(&path, now, |now_ms| fetch_installations(cfg, now_ms).ok());
    installation_for_owner(&refs, owner)
}

fn installation_for_owner(refs: &[InstallRef], owner: &str) -> Option<String> {
    refs.iter()
        .find(|inst| inst.login.eq_ignore_ascii_case(owner))
        .map(|inst| inst.id.clone())
}

/// The cached installations answer when fresh, else a fetch that lands in
/// the cache. A fetch failure serves the stale list when one exists (an
/// install changes rarely; a removed install fails its own mint later,
/// which degrades to the user token).
fn installations_from(
    path: &Path,
    now_ms: i64,
    fetch: impl Fn(i64) -> Option<Vec<InstallRef>>,
) -> Vec<InstallRef> {
    let cached = read_installs_cache(path);
    if let Some((refs, at_ms)) = &cached {
        if now_ms / 1000 < at_ms / 1000 + INSTALLS_TTL_S {
            return refs.clone();
        }
    }
    let Some(refs) = fetch(now_ms) else {
        return cached.map(|(refs, _)| refs).unwrap_or_default();
    };
    write_installs_cache(path, &refs, now_ms);
    refs
}

/// An installation's token and visible repo list. `mint_and_list` is a seam
/// so tests prove the whole path without a live POST.
fn install_auth(cfg: &AppConfig, installation_id: &str) -> Option<InstallAuth> {
    let notice = |reason: &str| notice_fallback_once(reason);
    auth_from(
        cfg,
        installation_id,
        &token_cache_path(installation_id),
        crate::gh_budget::now_ms(),
        |cfg, id, now| {
            mint_and_list(cfg, id, now)
                .map_err(|e| {
                    notice(&e);
                    e
                })
                .ok()
        },
    )
}

fn auth_from(
    cfg: &AppConfig,
    installation_id: &str,
    cache: &Path,
    now_ms: i64,
    mint_and_list: impl Fn(&AppConfig, &str, i64) -> Option<(String, Vec<String>)>,
) -> Option<InstallAuth> {
    let now_s = now_ms / 1000;
    if let Some(cached) = read_token_cache(cache) {
        if now_s < cached.expires_at_s - EXPIRY_SLOP_S {
            if let Some(repos) = cached.repos {
                return Some(InstallAuth {
                    token: cached.token,
                    repos,
                });
            }
            // A pre-scoping cache row carries no repo list: re-mint so the
            // token and its scope land together.
        }
    }
    let (minted_json, repos) = mint_and_list(cfg, installation_id, now_ms)?;
    let token = match serde_json::from_str::<Value>(&minted_json) {
        Ok(v) => v.get("token")?.as_str()?.to_string(),
        Err(_) => return None,
    };
    if token.is_empty() {
        return None;
    }
    write_token_cache(cache, &token, now_s, &repos);
    Some(InstallAuth { token, repos })
}

fn read_token_cache(path: &Path) -> Option<CachedToken> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some(CachedToken {
        token: v.get("token")?.as_str()?.to_string(),
        expires_at_s: v.get("expires_at")?.as_i64()?,
        repos: v.get("repos").and_then(Value::as_array).map(|rows| {
            rows.iter()
                .filter_map(|r| r.as_str())
                .map(str::to_string)
                .collect()
        }),
    })
}

struct CachedToken {
    token: String,
    expires_at_s: i64,
    repos: Option<Vec<String>>,
}

fn write_token_cache(path: &Path, token: &str, now_s: i64, repos: &[String]) {
    let body = serde_json::json!({
        "token": token,
        "expires_at": now_s + TOKEN_TTL_S,
        "repos": repos,
    });
    write_private(path, &format!("{body}\n"));
}

fn read_installs_cache(path: &Path) -> Option<(Vec<InstallRef>, i64)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let at_ms = v.get("fetched_at")?.as_i64()?;
    let mut refs = Vec::new();
    for row in v.get("installs")?.as_array()? {
        let id = row.get("id")?.as_str()?.to_string();
        let login = row.get("login")?.as_str()?.to_string();
        refs.push(InstallRef { id, login });
    }
    Some((refs, at_ms))
}

fn write_installs_cache(path: &Path, refs: &[InstallRef], now_ms: i64) {
    let rows: Vec<Value> = refs
        .iter()
        .map(|r| serde_json::json!({"id": r.id, "login": r.login}))
        .collect();
    let body = serde_json::json!({"fetched_at": now_ms, "installs": rows});
    write_private(path, &format!("{body}\n"));
}

fn write_private(path: &Path, body: &str) {
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
        let _ = f.write_all(body.as_bytes());
    }
}

fn installs_cache_path() -> PathBuf {
    locks_dir().join("github-app-installations.json")
}

/// One cache file per installation. Punctuation is stripped from the id so
/// a stray config value cannot walk the path out of the locks dir; distinct
/// numeric ids stay distinct.
fn token_cache_path(installation_id: &str) -> PathBuf {
    let safe: String = installation_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    locks_dir().join(format!("github-app-token-{safe}.json"))
}

fn locks_dir() -> PathBuf {
    let fallback = std::env::temp_dir().join("fno-locks");
    crate::agents_config::machine_locks_dir().unwrap_or(fallback)
}

/// The production installations list: one JWT, paged GET
/// /app/installations, capped so a pagination surprise cannot loop a poll
/// tick forever.
fn fetch_installations(cfg: &AppConfig, now_ms: i64) -> Result<Vec<InstallRef>, String> {
    let der = read_key_der(&cfg.key_path)?;
    let jwt = sign_jwt(&der, &cfg.app_id, now_ms)?;
    let mut refs = Vec::new();
    for page in 1..=LIST_PAGE_CAP {
        let (code, body) = curl_json(
            "GET",
            &format!("https://api.github.com/app/installations?per_page=100&page={page}"),
            &jwt,
        )?;
        if code != 200 {
            return Err(format!("installations list answered http {code}"));
        }
        let v: Value = serde_json::from_str(&body)
            .map_err(|e| format!("installations list unparsable: {e}"))?;
        let total = v.get("total_count").and_then(Value::as_u64).unwrap_or(0);
        let rows = v
            .get("installations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in rows.iter() {
            let id = row.get("id").and_then(Value::as_u64).map(|n| n.to_string());
            let login = row
                .get("account")
                .and_then(|a| a.get("login"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let (Some(id), Some(login)) = (id, login) {
                refs.push(InstallRef { id, login });
            }
        }
        if (refs.len() as u64) >= total || rows.is_empty() {
            break;
        }
    }
    Ok(refs)
}

/// The production mint + scope fetch: read the PEM, sign a 10-minute JWT,
/// POST it to the installation access-tokens endpoint, then read the
/// installation's repository list with the new token. The JWT and the
/// token never reach argv.
fn mint_and_list(
    cfg: &AppConfig,
    installation_id: &str,
    now_ms: i64,
) -> Result<(String, Vec<String>), String> {
    let der = read_key_der(&cfg.key_path)?;
    let jwt = sign_jwt(&der, &cfg.app_id, now_ms)?;
    let (code, body) = curl_json(
        "POST",
        &format!("https://api.github.com/app/installations/{installation_id}/access_tokens"),
        &jwt,
    )?;
    if !(200..300).contains(&code) {
        return Err(format!("mint POST answered http {code}"));
    }
    let token = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("token").and_then(Value::as_str).map(str::to_string))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "mint body carried no token".to_string())?;
    let repos = list_installation_repos(&token)?;
    Ok((body, repos))
}

/// The installation's visible repos, `full_name` slugs normalized
/// lowercase, paged and capped.
fn list_installation_repos(token: &str) -> Result<Vec<String>, String> {
    let mut repos = Vec::new();
    for page in 1..=LIST_PAGE_CAP {
        let (code, body) = curl_json(
            "GET",
            &format!("https://api.github.com/installation/repositories?per_page=100&page={page}"),
            token,
        )?;
        if code != 200 {
            return Err(format!("installation repo list answered http {code}"));
        }
        let v: Value = serde_json::from_str(&body)
            .map_err(|e| format!("installation repo list unparsable: {e}"))?;
        let total = v.get("total_count").and_then(Value::as_u64).unwrap_or(0);
        let rows = v
            .get("repositories")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in rows.iter() {
            if let Some(name) = row.get("full_name").and_then(Value::as_str) {
                if let Some(slug) = normalize_repo(name) {
                    repos.push(slug);
                }
            }
        }
        if (repos.len() as u64) >= total || rows.is_empty() {
            break;
        }
    }
    Ok(repos)
}

fn expand_tilde(path: &Path) -> PathBuf {
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

/// GET/POST with the bearer in curl's stdin config, so no secret is
/// visible in `ps` output or in the environment.
fn curl_json(method: &str, url: &str, bearer: &str) -> Result<(u16, String), String> {
    let auth = format!("Authorization: Bearer {bearer}");
    let config = format!(
        "silent\nshow-error\nmax-time = {MINT_TIMEOUT_SECS}\nheader = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nheader = \"{}\"\nwrite-out = \"\\n%{{http_code}}\"\n",
        auth.replace('"', "\\\"")
    );
    let mut child = Command::new("curl")
        .arg("-K")
        .arg("-")
        .args(["-X", method])
        .arg(url)
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

/// Does a failed gh read name the not-found shape an installation token
/// produces on a repo it cannot see? REST reads answer `HTTP 404`;
/// GraphQL reads answer `Could not resolve to a Repository/PullRequest`.
/// A genuinely missing object under the user token matches the same
/// markers, so the retry's only cost when the first answer was right is
/// one duplicate call.
pub(crate) fn names_missing_visibility(stderr: &str) -> bool {
    stderr.contains("HTTP 404")
        || stderr.contains("Could not resolve to a Repository")
        || stderr.contains("Could not resolve to a PullRequest")
}

/// A 404-shaped failure from a child that carried an installation token:
/// retry once with the caller's own auth. This covers the window where the
/// cached installation state went stale (a repo moved out of an
/// installation, an installation removed); the scope check makes it a
/// narrow window, not a missing fix.
pub(crate) fn should_retry_without_app(injected: bool, failed: bool, stderr: &str) -> bool {
    injected && failed && names_missing_visibility(stderr)
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
                key_path: pem_path.clone(),
                installation_pin: None,
            },
            dir.join("token-cache.json"),
        )
    }

    fn clone_auth(auth: &InstallAuth) -> InstallAuth {
        InstallAuth {
            token: auth.token.clone(),
            repos: auth.repos.clone(),
        }
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
    fn normalize_repo_canonicalizes_every_remote_and_flag_form() {
        let n = normalize_repo;
        assert_eq!(n("Owner/Repo").as_deref(), Some("owner/repo"));
        assert_eq!(n("github.com/o/r").as_deref(), Some("o/r"));
        assert_eq!(n("https://github.com/o/r.git").as_deref(), Some("o/r"));
        assert_eq!(n("git@github.com:o/r.git").as_deref(), Some("o/r"));
        assert_eq!(n("ssh://git@github.com/o/r").as_deref(), Some("o/r"));
        // A lookalike host is not GitHub, and non-github multi-segment
        // hosts and templates are not repo slugs.
        assert_eq!(n("https://github.com.evil/o/r"), None);
        assert_eq!(n("gitlab.com/group/sub/repo"), None);
        assert_eq!(n(":owner/:repo"), None);
        assert_eq!(n("../etc/passwd"), None);
    }

    #[test]
    fn args_carry_the_repo_through_every_spelling() {
        let case = |args: &[&str]| repo_from_args(args.iter().map(OsStr::new));
        assert_eq!(
            case(&["-R", "Owner/Repo.git"]).as_deref(),
            Some("owner/repo")
        );
        assert_eq!(case(&["--repo", "o/r"]).as_deref(), Some("o/r"));
        assert_eq!(case(&["--repo=o/r"]).as_deref(), Some("o/r"));
        assert_eq!(case(&["-Ro/r"]).as_deref(), Some("o/r"));
        assert_eq!(case(&["api", "repos/o/r/pulls"]).as_deref(), Some("o/r"));
        assert_eq!(case(&["api", "/repos/o/r"]).as_deref(), Some("o/r"));
        assert_eq!(
            case(&["api", "https://api.github.com/repos/o/r/pulls?x=1"]).as_deref(),
            Some("o/r")
        );
        // Method and header flags may sit between `api` and the endpoint.
        assert_eq!(
            case(&["api", "--method", "POST", "-H", "x: y", "repos/o/r/x"]).as_deref(),
            Some("o/r")
        );
        // Templates never resolve, and a cwd-resolved command carries no
        // repo in its args at all.
        assert_eq!(case(&["api", "repos/:owner/:repo"]), None);
        assert_eq!(case(&["pr", "view", "377"]), None);
    }

    #[test]
    fn a_cwd_remote_names_the_repo_with_origin_outranking_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:BlLsHtTnG/Fno-Books.git",
        ]);
        assert_eq!(
            repo_from_cwd(dir.path()).as_deref(),
            Some("bllshttng/fno-books")
        );
        git(&[
            "remote",
            "add",
            "upstream",
            "https://github.com/fnodotdev/fno.git",
        ]);
        assert_eq!(
            repo_from_cwd(dir.path()).as_deref(),
            Some("bllshttng/fno-books"),
            "origin outranks upstream"
        );
    }

    #[test]
    fn injection_decision_scopes_to_the_visible_repo() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("FNO_GITHUB_APP");
        let visible = InstallAuth {
            token: "ghs_visible".to_string(),
            repos: vec!["fnodotdev/fno".to_string()],
        };
        let resolve_visible = move |_cfg: &AppConfig, owner: &str| -> Option<InstallAuth> {
            assert_eq!(owner, "fnodotdev");
            Some(clone_auth(&visible))
        };
        let mut api = Command::new("gh");
        api.args(["api", "repos/fnodotdev/fno/pulls"]);
        assert_eq!(
            decision_with(&api, resolve_visible).as_deref(),
            Some("ghs_visible")
        );
        // A repo outside the installation's list keeps the child's own auth.
        let resolve_foreign = |_cfg: &AppConfig, _owner: &str| -> Option<InstallAuth> {
            Some(InstallAuth {
                token: "ghs_foreign".to_string(),
                repos: Vec::new(),
            })
        };
        let mut foreign = Command::new("gh");
        foreign.args(["api", "repos/bllshttng/fno-books/pulls"]);
        assert_eq!(decision_with(&foreign, resolve_foreign), None);
        // A command with no resolvable repo never asks the resolver.
        let mut unknown = Command::new("gh");
        unknown.args(["pr", "view", "377"]);
        assert_eq!(
            decision_with(&unknown, |_: &AppConfig, _: &str| -> Option<InstallAuth> {
                panic!("no repo must not resolve")
            }),
            None
        );
        // An operator-exported token outranks the app before any resolution.
        std::env::set_var("GH_TOKEN", "operator-token");
        let resolve_panic =
            |_: &AppConfig, _: &str| -> Option<InstallAuth> { panic!("guards precede resolution") };
        assert_eq!(decision_with(&api, resolve_panic), None);
        std::env::remove_var("GH_TOKEN");
    }

    #[test]
    fn inject_respects_a_caller_decision_an_operator_token_and_the_kill_switch() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        // A caller that set or removed GH_TOKEN decided; the app never overrides.
        let mut decided = Command::new("gh");
        decided.env("GH_TOKEN", "callers-own");
        assert!(!inject_with(&mut decided, Some("app-token")));
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
        assert!(!inject_with(&mut operator, Some("app-token")));
        assert!(operator
            .get_envs()
            .all(|(k, _)| k != OsStr::new("GH_TOKEN")));
        std::env::remove_var("GH_TOKEN");
        // The kill switch leaves the child untouched.
        std::env::set_var("FNO_GITHUB_APP", "0");
        let mut killed = Command::new("gh");
        assert!(!inject_with(&mut killed, Some("app-token")));
        std::env::remove_var("FNO_GITHUB_APP");
        assert!(killed.get_envs().all(|(k, _)| k != OsStr::new("GH_TOKEN")));
        // No app token means no change, which is today's behavior.
        let mut plain = Command::new("gh");
        assert!(!inject_with(&mut plain, None));
        assert!(plain.get_envs().all(|(k, _)| k != OsStr::new("GH_TOKEN")));
        // A placed token reports true, so the caller can arm the 404 retry.
        let mut placed = Command::new("gh");
        assert!(inject_with(&mut placed, Some("app-token")));
    }

    #[test]
    fn visibility_compares_normalized_slugs() {
        let auth = InstallAuth {
            token: "t".to_string(),
            repos: vec!["fnodotdev/fno".to_string()],
        };
        assert!(auth.sees("fnodotdev/fno"));
        assert!(!auth.sees("bllshttng/fno-books"));
    }

    #[test]
    fn token_path_is_chosen_when_the_key_exists_without_a_live_call() {
        let dir = std::env::temp_dir().join("fno-github-app-test-mint");
        std::fs::create_dir_all(&dir).unwrap();
        let (cfg, cache) = temp_cfg(&dir);
        let posts = Mutex::new(0usize);
        let seen_jwt = Mutex::new(String::new());
        let token = auth_from(
            &cfg,
            "2222222",
            &cache,
            1_700_000_000_000,
            |cfg, id, _now| {
                *posts.lock().unwrap() += 1;
                assert_eq!(id, "2222222");
                let jwt = sign_jwt(&read_key_der(&cfg.key_path).unwrap(), &cfg.app_id, _now)
                    .expect("test sign");
                *seen_jwt.lock().unwrap() = jwt.clone();
                assert_eq!(jwt.split('.').count(), 3);
                Some((
                    serde_json::json!({"token": "ghs_testtoken", "expires_at": "irrelevant"})
                        .to_string(),
                    vec!["fnodotdev/fno".to_string()],
                ))
            },
        );
        assert_eq!(
            token.as_ref().map(|a| a.token.as_str()),
            Some("ghs_testtoken")
        );
        assert_eq!(
            token.as_ref().map(|a| a.repos.as_slice()),
            Some(["fnodotdev/fno".to_string()].as_slice())
        );
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
        let mint = |_cfg: &AppConfig, _id: &str, _now: i64| -> Option<(String, Vec<String>)> {
            *posts.lock().unwrap() += 1;
            Some((
                serde_json::json!({"token": "ghs_cached"}).to_string(),
                vec!["fnodotdev/fno".to_string()],
            ))
        };
        let now = 1_700_000_000_000;
        let first = auth_from(&cfg, "2222222", &cache, now, mint);
        assert_eq!(first.as_ref().map(|a| a.token.as_str()), Some("ghs_cached"));
        // 50 minutes in: still inside the slop window, served from cache.
        let second = auth_from(&cfg, "2222222", &cache, now + 50 * 60 * 1000, mint);
        assert_eq!(
            second.as_ref().map(|a| a.token.as_str()),
            Some("ghs_cached")
        );
        assert_eq!(*posts.lock().unwrap(), 1, "cache served the second read");
        // 56 minutes in: inside the 5-minute slop, so the token re-mints.
        let third = auth_from(&cfg, "2222222", &cache, now + 56 * 60 * 1000, mint);
        assert_eq!(third.as_ref().map(|a| a.token.as_str()), Some("ghs_cached"));
        assert_eq!(*posts.lock().unwrap(), 2, "expired cache re-minted");
    }

    #[test]
    fn a_fresh_token_without_a_scoped_list_re_mints_so_scope_and_token_agree() {
        let dir = std::env::temp_dir().join("fno-github-app-test-unscoped");
        std::fs::create_dir_all(&dir).unwrap();
        let (cfg, cache) = temp_cfg(&dir);
        // A pre-scoping cache row: token and expiry, no repo list.
        let now = 1_700_000_000_000;
        write_token_cache(&cache, "ghs_legacy", now / 1000, &[]);
        let raw = std::fs::read_to_string(&cache).unwrap();
        let mut v: Value = serde_json::from_str(&raw).unwrap();
        v.as_object_mut().unwrap().remove("repos");
        std::fs::write(&cache, v.to_string()).unwrap();
        let posts = Mutex::new(0usize);
        let auth = auth_from(&cfg, "2222222", &cache, now + 60_000, |_c, _i, _n| {
            *posts.lock().unwrap() += 1;
            Some((
                serde_json::json!({"token": "ghs_fresh"}).to_string(),
                vec!["fnodotdev/fno".to_string()],
            ))
        });
        assert_eq!(auth.as_ref().map(|a| a.token.as_str()), Some("ghs_fresh"));
        assert_eq!(*posts.lock().unwrap(), 1, "a listless row re-mints");
    }

    #[test]
    fn unconfigured_or_keyless_falls_back_without_writing_cache() {
        let dir = std::env::temp_dir().join("fno-github-app-test-fallback");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("token-cache.json");
        let missing = AppConfig {
            app_id: "1".to_string(),
            key_path: dir.join("absent.pem"),
            installation_pin: None,
        };
        let failed = auth_from(&missing, "2", &cache, 0, |cfg, _id, _now| {
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
    fn a_fresh_installations_cache_answers_without_a_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installs.json");
        let now = 1_700_000_000_000i64;
        write_installs_cache(
            &path,
            &[InstallRef {
                id: "1".to_string(),
                login: "FnodotDev".to_string(),
            }],
            now,
        );
        let refs = installations_from(&path, now + 60_000, |_| {
            panic!("fresh cache must not fetch")
        });
        assert_eq!(
            installation_for_owner(&refs, "fnodotdev").as_deref(),
            Some("1")
        );
        assert_eq!(installation_for_owner(&refs, "nobody"), None);
    }

    #[test]
    fn a_stale_list_refetches_and_a_fetch_failure_serves_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installs.json");
        let now = 1_700_000_000_000i64;
        write_installs_cache(
            &path,
            &[InstallRef {
                id: "1".to_string(),
                login: "fnodotdev".to_string(),
            }],
            now,
        );
        let stale_at = now + (INSTALLS_TTL_S + 60) * 1000;
        let fetched = Mutex::new(0usize);
        let refs = installations_from(&path, stale_at, |_| {
            *fetched.lock().unwrap() += 1;
            Some(vec![InstallRef {
                id: "9".to_string(),
                login: "bllshttng".to_string(),
            }])
        });
        assert_eq!(*fetched.lock().unwrap(), 1, "stale list refetched");
        assert_eq!(
            installation_for_owner(&refs, "bllshttng").as_deref(),
            Some("9")
        );
        // A failing fetch past freshness serves the stale list; with no
        // cache row at all it serves an empty answer.
        let refs = installations_from(&path, stale_at, |_| None);
        assert_eq!(
            installation_for_owner(&refs, "bllshttng").as_deref(),
            Some("9"),
            "stale list survives a fetch failure"
        );
        let empty = installations_from(&dir.path().join("absent.json"), now, |_| None);
        assert!(empty.is_empty());
    }

    #[test]
    fn the_retry_fires_only_for_a_404_under_the_app_token() {
        assert!(names_missing_visibility("gh: Not Found (HTTP 404)"));
        assert!(names_missing_visibility(
            "GraphQL: Could not resolve to a PullRequest object for the specified input."
        ));
        assert!(names_missing_visibility(
            "GraphQL: Could not resolve to a Repository within the 'bllshttng' organization."
        ));
        assert!(!names_missing_visibility("HTTP 403: rate limit exceeded"));
        assert!(should_retry_without_app(
            true,
            true,
            "gh: Not Found (HTTP 404)"
        ));
        assert!(
            !should_retry_without_app(false, true, "gh: Not Found (HTTP 404)"),
            "a user-token read never retries"
        );
        assert!(
            !should_retry_without_app(true, false, "gh: Not Found (HTTP 404)"),
            "a green read never retries"
        );
        assert!(
            !should_retry_without_app(true, true, "HTTP 429"),
            "rate limits are not missing visibility"
        );
    }
}

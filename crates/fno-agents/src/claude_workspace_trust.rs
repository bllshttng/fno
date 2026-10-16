//! Workspace-trust preflight for claude spawns.
//!
//! Claude refuses an untrusted cwd with `Workspace not trusted. Run claude in
//! <dir> once and accept the trust prompt, then retry.` - but only AFTER the
//! spawn has paid the `max_live` queue and the routing, and the bg wrapper
//! then reports exit 0 over the harness's EXIT=1, so the refusal is easy to
//! miss twice. This preflight reads
//! `projects[<cwd>].hasTrustDialogAccepted` from the claude config the spawn
//! resolves and refuses BEFORE the gate queue.
//!
//! Read-only by contract: fno never flips the flag. Trust is the user's to
//! accept.
//!
//! Leg boundary: the preflight runs in the fno-agents client, which every
//! `fno agents spawn` passes through - including an `--account` run, whose
//! Python re-exec happens after this check. Only a manually pinned
//! `FNO_AGENTS_RUNTIME=python` invocation skips it.

use std::path::{Path, PathBuf};

/// The config root whose `.claude.json` the launched claude reads: the
/// account record's own dir when it pins one, else the ambient
/// `CLAUDE_CONFIG_DIR`, else `$HOME`.
///
/// Known limit: a managed account with no `config_dir` rides the shared slot
/// and Python pins `CLAUDE_CONFIG_DIR=$HOME/.claude` for it; this resolution
/// reads `$HOME` there, so a machine that keeps those two trust stores in
/// disagreement can pass or refuse one rung off. The refusal names the file
/// it read, so the remedy stays actionable.
pub fn resolved_config_root(account: Option<&str>) -> PathBuf {
    if let Some(dir) = account
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(account_record_config_dir)
    {
        return dir;
    }
    if let Some(cfg) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(cfg);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The `config_dir` of one account record, read from the GLOBAL config only:
/// Python's `--account` resolver calls `load_providers(repo_root=None)`, so a
/// project-local accounts block never reaches it, and the preflight that
/// predicts the pinned dir must not read one either.
fn account_record_config_dir(id: &str) -> Option<PathBuf> {
    let content = std::fs::read_to_string(crate::agents_config::global_config_path()?).ok()?;
    let table = crate::agents_config::parse_config(&content)?;
    let records = table
        .get("accounts")
        .or_else(|| table.get("providers"))?
        .get("records")?
        .as_array()?;
    for record in records.iter().rev() {
        // Last wins, matching Python's by_id dict on a duplicate id.
        if record.get("id").and_then(|v| v.as_str()) == Some(id) {
            return record
                .get("config_dir")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(expand_home);
        }
    }
    None
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Why the preflight stays silent: a non-claude harness has no trust dialog,
/// a pane can answer it interactively (that IS the remedy), and a cwd that
/// never resolves names a dir the spawn would refuse on its own.
#[derive(Debug)]
pub enum Skip {
    NotClaude,
    InteractivePane,
    UnresolvableCwd,
}

#[derive(Debug)]
pub enum Verdict {
    Trusted,
    Untrusted { config_json: String },
}

/// Read the trust flag for `cwd` out of `root/.claude.json`. Both the path as
/// given and its canonicalized form are accepted: claude stores the resolved
/// absolute path, and the caller may have passed a symlinked spelling.
pub fn check(root: &Path, cwd: &Path) -> Verdict {
    let config_json = root.join(".claude.json");
    // One read, one parse: the candidates below retest keys, not the file.
    let parsed: Option<serde_json::Value> = std::fs::read_to_string(&config_json)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok());
    let trusted = |key: &str| -> Option<bool> {
        parsed
            .as_ref()?
            .get("projects")?
            .get(key)?
            .get("hasTrustDialogAccepted")
            .and_then(|v| v.as_bool())
    };
    let mut candidates: Vec<String> = vec![cwd.to_string_lossy().to_string()];
    if let Ok(canon) = std::fs::canonicalize(cwd) {
        candidates.push(canon.to_string_lossy().to_string());
    }
    for key in &candidates {
        if trusted(key) == Some(true) {
            return Verdict::Trusted;
        }
    }
    Verdict::Untrusted {
        config_json: config_json.display().to_string(),
    }
}

/// One call for the spawn seam: `None` skips the check, a verdict decides it.
pub fn preflight(
    provider: &str,
    substrate: &str,
    account: Option<&str>,
    cwd: &str,
) -> Result<Verdict, Skip> {
    if provider != "claude" {
        return Err(Skip::NotClaude);
    }
    if substrate == "pane" {
        return Err(Skip::InteractivePane);
    }
    let dir = PathBuf::from(cwd);
    let dir = if dir.is_absolute() {
        dir
    } else {
        // A relative `--cwd` resolves against the caller at exec time; mirror
        // that so the key matches what claude will store.
        match std::env::current_dir() {
            Ok(base) => base.join(dir),
            Err(_) => return Err(Skip::UnresolvableCwd),
        }
    };
    let root = resolved_config_root(account);
    Ok(check(&root, &dir))
}

/// The spawn seam's one-line entry: `true` means the refusal is already
/// printed and the caller must exit 2. Sits before the gate queue so an
/// untrusted workspace costs no queue wait and no routing, and the refusal
/// is one loud stderr line instead of a paid-then-silent harness death.
/// Takes the params OBJECT: a non-object value reads as fieldless, and every
/// skip below fires the same way it would for a missing provider.
pub fn spawn_preflight_refuses(params: &serde_json::Value, substrate: &str) -> bool {
    // FNO_SPAWN_GATE=0 is the operator bypass both gate implementations
    // honor. This preflight is gate-adjacent: the bypass excuses it the same
    // way, so a lane exercised under the bypass keeps its own refusal order.
    if std::env::var_os("FNO_SPAWN_GATE").is_some_and(|v| v == "0") {
        return false;
    }
    let provider = params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("codex");
    let Ok(Verdict::Untrusted { config_json }) = preflight(
        provider,
        substrate,
        params.get("account").and_then(|v| v.as_str()),
        params
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
    ) else {
        return false;
    };
    eprintln!(
        "Workspace not trusted. Run claude in {} once and accept the trust prompt, then retry. \
         (trust read from {config_json})",
        params
            .get("cwd")
            .and_then(|v| v.as_str())
            .filter(|c| !c.is_empty())
            .unwrap_or(".")
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn check_verdicts_by_config_shape() {
        let root = std::env::temp_dir().join("claude-trust-check-cases");
        let workdir = root.join("w");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&workdir).unwrap();
        // claude stores the canonicalized absolute path as the projects key.
        let canon = fs::canonicalize(&workdir).unwrap();
        let canon_str = canon.to_string_lossy().replace('\\', "\\\\");
        let entry = |flag: &str| {
            format!(r#"{{"projects":{{"{canon_str}":{{"hasTrustDialogAccepted":{flag}}}}}}}"#)
        };
        let cases: Vec<(&str, String, bool)> = vec![
            ("trusted flag", entry("true"), true),
            ("flag false", entry("false"), false),
            ("no entry", r#"{"projects":{}}"#.into(), false),
            ("no config file", String::new(), false),
            ("unparsable config", "{not json".into(), false),
        ];
        for (label, body, expect_trusted) in cases {
            if body.is_empty() {
                let _ = fs::remove_file(root.join(".claude.json"));
            } else {
                fs::write(root.join(".claude.json"), &body).unwrap();
            }
            let verdict = check(&root, &workdir);
            let trusted = matches!(verdict, Verdict::Trusted);
            assert_eq!(trusted, expect_trusted, "{label}: verdict {verdict:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn preflight_skip_matrix() {
        // Non-claude and pane skip; a relative cwd is legal input (it joins
        // the caller dir) and reaches the check.
        assert!(matches!(
            preflight("codex", "bg", None, "/tmp/w"),
            Err(Skip::NotClaude)
        ));
        assert!(matches!(
            preflight("claude", "pane", None, "/tmp/w"),
            Err(Skip::InteractivePane)
        ));
        assert!(
            preflight("claude", "bg", None, "relative/dir").is_ok(),
            "relative cwd reaches the check"
        );
    }

    #[test]
    fn resolved_config_root_ordering() {
        // The account record's own config_dir wins; an account without one
        // and a bare spawn fall through to the ambient root.
        let root = std::env::temp_dir().join("claude-trust-root-ordering");
        let _ = fs::remove_dir_all(&root);
        let cfg_dir = root.join("claude-alt");
        fs::create_dir_all(&cfg_dir).unwrap();
        // FNO_GLOBAL_SETTINGS_PATH relocates the GLOBAL config the same way
        // the Python loader honors: the sibling config.toml of the named file.
        let fixture = root.join("settings.yaml");
        let config = root.join("config.toml");
        fs::write(
            &config,
            format!(
                "[[accounts.records]]\nid = \"alt\"\nconfig_dir = \"{}\"\n",
                cfg_dir.display()
            ),
        )
        .unwrap();
        let prev = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH");
        std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", &fixture);
        let got = resolved_config_root(Some("alt"));
        let _ = std::env::remove_var("FNO_GLOBAL_SETTINGS_PATH");
        if let Some(v) = prev {
            std::env::set_var("FNO_GLOBAL_SETTINGS_PATH", v);
        }
        assert_eq!(got, PathBuf::from(cfg_dir), "record config_dir wins");
        let _ = fs::remove_dir_all(&root);
    }
}

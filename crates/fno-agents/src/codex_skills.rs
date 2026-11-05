//! `fno-agents plugin-install --codex-skills probe|inventory|curate`: the one
//! Codex skill-discovery engine `scripts/setup.sh --provider codex` and
//! `scripts/doctor.sh` render from, so setup and doctor never disagree about
//! what is exposed. The plugin cache is read-only here: detection, never
//! repair (the reinstall flow in docs/HARNESSES.md repairs).

use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const ALIAS_PREFIXES: [&str; 2] = ["fno--", "plugin--fno--"];
const PLACEHOLDER_MARKERS: [&str; 3] = ["{{", "placeholder", "<+"];
const PLACEHOLDER_WORDS: [&str; 2] = ["todo", "tbd"];

#[derive(Debug, Clone)]
pub(crate) struct Plugin {
    pub path: PathBuf,
    pub version: String,
    pub skills_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub name: String,
    pub alias: Option<String>,
    pub source: Option<String>,
    pub digest: Option<String>,
    pub owner: String,
    pub status: String,
    pub note: String,
}

fn digest(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes))[..16].to_string())
}

/// Codex installs under CODEX_HOME when set, ~/.codex otherwise.
fn default_cache() -> PathBuf {
    let home = std::env::var("CODEX_HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "~/.codex".into());
    expand(&home).join("plugins/cache")
}

fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

/// Digit parts compare as numbers, anything else as text below them, so a
/// prerelease suffix never outranks a release.
fn version_key(version: &str) -> Vec<(u8, u64, String)> {
    version
        .split('.')
        .map(|p| match p.parse::<u64>() {
            Ok(n) if p.chars().all(|c| c.is_ascii_digit()) => (1, n, String::new()),
            _ => (0, 0, p.to_string()),
        })
        .collect()
}

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// The newest usable footnote/fno plugin under the codex cache, else None.
pub(crate) fn find_plugin(cache: &Path) -> Option<Plugin> {
    let mut best: Option<Plugin> = None;
    for child in sorted_children(&cache.join("footnote/fno")) {
        let Some(data) = std::fs::read_to_string(child.join("plugin.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let Some(rel) = data
            .get("skills")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        else {
            continue;
        };
        let Ok(skills_dir) = child
            .join(rel.trim_start_matches(['.', '/']))
            .canonicalize()
        else {
            continue;
        };
        if !skills_dir.is_dir() {
            continue;
        }
        let candidate = Plugin {
            version: name_of(&child),
            path: child,
            skills_dir,
        };
        if best
            .as_ref()
            .is_none_or(|b| version_key(&candidate.version) > version_key(&b.version))
        {
            best = Some(candidate);
        }
    }
    best
}

fn source_skills(repo: &Path) -> BTreeSet<String> {
    sorted_children(&repo.join("skills"))
        .into_iter()
        .filter(|d| d.join("SKILL.md").is_file())
        .map(|d| name_of(&d))
        .collect()
}

/// The exact reasons a SKILL.md is not a usable discovery entry.
pub(crate) fn metadata_problems(md: &Path, canonical: Option<&str>) -> Vec<String> {
    let text = match std::fs::read(md) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => return vec![format!("unreadable: {e}")],
    };
    if !text.starts_with("---") {
        return vec!["no frontmatter block".into()];
    }
    let fence = Regex::new(r"(?s)^---\n(.*?)\n---\s*(?:\n|$)").unwrap();
    let Some(caps) = fence.captures(&text) else {
        return vec!["frontmatter never closes".into()];
    };
    let data: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&caps[1]) {
        Ok(v) => v,
        Err(e) => {
            return vec![format!(
                "invalid YAML: {}",
                e.to_string().lines().next().unwrap_or("")
            )]
        }
    };
    let Some(map) = data.as_mapping() else {
        return vec![if data.is_null() {
            "frontmatter is empty"
        } else {
            "frontmatter is not a mapping"
        }
        .into()];
    };
    let text_of = |v: &serde_yaml_ng::Value| match v {
        serde_yaml_ng::Value::String(s) => s.clone(),
        other => serde_yaml_ng::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let mut problems = Vec::new();
    match map.get("description") {
        None => problems.push("description missing".into()),
        Some(desc) => {
            let d = text_of(desc);
            let d = d.trim();
            let lower = d.to_lowercase();
            let words = Regex::new(&format!(r"\b(?:{})\b", PLACEHOLDER_WORDS.join("|"))).unwrap();
            if d.is_empty() {
                problems.push("description empty".into());
            } else if d.chars().all(|c| ">-*_".contains(c) || c.is_whitespace()) {
                problems.push(format!("description is punctuation ('{d}')"));
            } else if PLACEHOLDER_MARKERS.iter().any(|m| lower.contains(m))
                || words.is_match(&lower)
            {
                let head: String = d.chars().take(40).collect();
                problems.push(format!("description carries placeholder text ('{head}')"));
            }
        }
    }
    if let (Some(canonical), Some(serde_yaml_ng::Value::String(name))) =
        (canonical, map.get("name"))
    {
        let name = name.trim();
        if !name.is_empty() && name != canonical {
            problems.push(format!(
                "name '{name}' does not match canonical '{canonical}'"
            ));
        }
    }
    problems
}

fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn link_owner(target: &Path, repo: &Path) -> String {
    let resolved = real(target);
    let root = real(repo);
    if resolved == real(&root.join("skills")) || resolved.starts_with(&root) && resolved != root {
        "footnote".into()
    } else {
        format!("external:{}", resolved.display())
    }
}

fn alias_health(alias: Option<&Path>, name: &str) -> (&'static str, String) {
    let Some(alias) = alias else {
        return ("absent", String::new());
    };
    if !alias.exists() {
        return ("broken", "dangling symlink".into());
    }
    let target = real(alias);
    let md = target.join("SKILL.md");
    if !md.is_file() {
        return (
            "stale",
            format!("linked dir has no SKILL.md ({})", target.display()),
        );
    }
    let problems = metadata_problems(&md, Some(name));
    if problems.is_empty() {
        ("ok", String::new())
    } else {
        ("unavailable", problems.join("; "))
    }
}

pub(crate) fn inventory(repo: &Path, root: &Path, plugin: Option<&Plugin>) -> Vec<Entry> {
    let sources = source_skills(repo);
    let plugin_names: BTreeSet<String> = plugin
        .map(|p| {
            sorted_children(&p.skills_dir)
                .into_iter()
                .filter(|d| d.join("SKILL.md").is_file())
                .map(|d| name_of(&d))
                .collect()
        })
        .unwrap_or_default();
    let links: Vec<(String, PathBuf)> = sorted_children(root)
        .into_iter()
        .filter(|c| {
            c.symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        })
        .map(|c| {
            let raw = name_of(&c);
            let name = ALIAS_PREFIXES
                .iter()
                .find_map(|p| raw.strip_prefix(p))
                .unwrap_or(&raw)
                .to_string();
            (name, c)
        })
        .collect();
    let names: BTreeSet<String> = sources
        .iter()
        .cloned()
        .chain(plugin_names.iter().cloned())
        .chain(links.iter().map(|(n, _)| n.clone()))
        .collect();
    let mut rows = Vec::new();
    for name in names {
        let mut linked: Vec<Option<&PathBuf>> = links
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, p)| Some(p))
            .collect();
        if linked.is_empty() {
            linked.push(None);
        }
        for alias in linked {
            let (health, health_note) = alias_health(alias.map(PathBuf::as_path), &name);
            let mut statuses: Vec<&str> = [health]
                .into_iter()
                .filter(|s| *s != "ok" && *s != "absent")
                .collect();
            let live = alias.is_some() && health != "broken";
            if plugin_names.contains(&name) && live {
                statuses.push("duplicate");
            }
            let source = alias
                .filter(|_| live)
                .map(|a| real(a).display().to_string());
            let digest = source
                .as_ref()
                .and_then(|s| digest(&Path::new(s).join("SKILL.md")));
            let owner = alias
                .filter(|_| live)
                .map(|a| link_owner(a, repo))
                .unwrap_or_else(|| "footnote".into());
            if statuses.is_empty() {
                statuses.push(
                    if plugin.is_some() && !plugin_names.contains(&name) && alias.is_none() {
                        "absent-from-plugin"
                    } else {
                        "ok"
                    },
                );
            }
            let note: Vec<String> = std::iter::once(health_note)
                .chain(statuses[1..].iter().map(|s| s.to_string()))
                .filter(|s| !s.is_empty())
                .collect();
            rows.push(Entry {
                name: name.clone(),
                alias: alias.map(|a| a.display().to_string()),
                source,
                digest,
                owner,
                status: statuses[0].to_string(),
                note: note.join("; "),
            });
        }
    }
    rows
}

/// Remove unrelated FOREIGN symlink exposure; real dirs and our own links stay.
pub(crate) fn curate(root: &Path, repo: &Path, apply: bool) -> Vec<Value> {
    let ours = real(&repo.join("skills"));
    let mut removals = Vec::new();
    for child in sorted_children(root) {
        let raw = name_of(&child);
        let Ok(target) = std::fs::read_link(&child) else {
            continue;
        };
        if ALIAS_PREFIXES.iter().any(|p| raw.starts_with(p)) {
            continue;
        }
        let resolved = real(&if target.is_absolute() {
            target
        } else {
            child.parent().unwrap_or(root).join(target)
        });
        if resolved == ours || resolved.starts_with(&ours) {
            continue;
        }
        removals.push(json!({
            "alias": child.display().to_string(),
            "source": resolved.display().to_string(),
            "restore": format!("ln -sfn '{}' '{}'", resolved.display(), child.display()),
        }));
        if apply {
            let _ = std::fs::remove_file(&child);
        }
    }
    removals
}

fn render_table(rows: &[Entry]) -> String {
    let mut lines = vec![format!(
        "{:<22} {:<17} {:<10} {:<17} source",
        "name", "status", "owner", "digest"
    )];
    for r in rows {
        let src = r.source.clone().unwrap_or_else(|| {
            if r.status == "absent-from-plugin" || r.status == "unavailable" {
                r.note.clone()
            } else {
                String::new()
            }
        });
        lines.push(format!(
            "{:<22} {:<17} {:<10} {:<17} {src}",
            r.name,
            r.status,
            r.owner,
            r.digest.as_deref().unwrap_or("-")
        ));
    }
    lines.join("\n")
}

const USAGE: &str = "usage: fno-agents plugin-install --codex-skills probe --repo R [--cache C]
       fno-agents plugin-install --codex-skills inventory --repo R --root S [--cache C] [--json|--tsv]
       fno-agents plugin-install --codex-skills curate --repo R --root S [--cache C] [--apply] [--json]";

pub fn run(args: &[String]) -> i32 {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(|v| expand(v))
    };
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let (Some(op), Some(repo)) = (args.first().map(String::as_str), value("--repo")) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let cache = value("--cache").unwrap_or_else(default_cache);
    let plugin = find_plugin(&cache);
    match (op, value("--root")) {
        ("probe", _) => {
            // An absent plugin prints an EMPTY value: consumers pick dev mode on -z.
            println!(
                "plugin={}",
                plugin
                    .as_ref()
                    .map(|p| p.path.display().to_string())
                    .unwrap_or_default()
            );
            if let Some(p) = &plugin {
                println!("version={}", p.version);
            }
            0
        }
        ("inventory", Some(root)) => {
            let rows = inventory(&repo, &root, plugin.as_ref());
            if has("--json") {
                let out: Vec<Value> = rows.iter().map(|r| json!({"name": r.name, "alias": r.alias, "source": r.source, "digest": r.digest, "owner": r.owner, "status": r.status, "note": r.note})).collect();
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            } else if has("--tsv") {
                for r in &rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        r.name,
                        r.status,
                        r.owner,
                        r.digest.as_deref().unwrap_or("-"),
                        r.note
                    );
                }
            } else {
                println!("{}", render_table(&rows));
            }
            0
        }
        ("curate", Some(root)) => {
            let removals = curate(&root, &repo, has("--apply"));
            if has("--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&removals).unwrap_or_default()
                );
            } else if removals.is_empty() {
                println!("nothing to remove");
            } else {
                for r in &removals {
                    println!(
                        "removed {} (foreign source {})",
                        r["alias"].as_str().unwrap_or(""),
                        r["source"].as_str().unwrap_or("")
                    );
                    println!("  restore: {}", r["restore"].as_str().unwrap_or(""));
                }
            }
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn skill(dir: &Path, name: &str, front: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("SKILL.md"), front).unwrap();
    }

    #[test]
    fn metadata_names_every_unusable_shape() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("a", "no fence", "no frontmatter block"),
            ("b", "---\nname: b\n", "frontmatter never closes"),
            ("c", "---\nname: [b\n---\n", "invalid YAML"),
            ("d", "---\nname: d\n---\n", "description missing"),
            (
                "e",
                "---\ndescription: TODO later\n---\n",
                "placeholder text",
            ),
            (
                "f",
                "---\nname: other\ndescription: real words\n---\n",
                "does not match canonical 'f'",
            ),
        ];
        for (name, text, want) in cases {
            skill(dir.path(), name, text);
            let got =
                metadata_problems(&dir.path().join(name).join("SKILL.md"), Some(name)).join("; ");
            assert!(got.contains(want), "{name}: {got}");
        }
        skill(
            dir.path(),
            "g",
            "---\nname: g\ndescription: Ship it.\n---\n",
        );
        assert!(metadata_problems(&dir.path().join("g/SKILL.md"), Some("g")).is_empty());
    }

    #[test]
    fn inventory_flags_duplicates_and_curate_spares_our_links() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let ok = "---\nname: x\ndescription: Real words.\n---\n";
        skill(&repo.join("skills"), "x", ok);
        skill(&repo.join("skills"), "y", &ok.replace("name: x", "name: y"));
        let cache = dir.path().join("cache");
        for v in ["0.9.0", "0.10.0"] {
            let p = cache.join("footnote/fno").join(v);
            skill(&p.join("skills"), "x", ok);
            std::fs::write(p.join("plugin.json"), r#"{"skills": "./skills/"}"#).unwrap();
        }
        let plugin = find_plugin(&cache).unwrap();
        assert_eq!(plugin.version, "0.10.0");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        symlink(repo.join("skills/x"), root.join("plugin--fno--x")).unwrap();
        symlink(dir.path().join("elsewhere"), root.join("foreign")).unwrap();
        let rows = inventory(&repo, &root, Some(&plugin));
        let status: Vec<(String, String)> = rows
            .iter()
            .map(|r| (r.name.clone(), r.status.clone()))
            .collect();
        assert!(
            status.contains(&("x".into(), "duplicate".into())),
            "{status:?}"
        );
        assert!(
            status.contains(&("y".into(), "absent-from-plugin".into())),
            "{status:?}"
        );
        assert!(
            status.contains(&("foreign".into(), "broken".into())),
            "{status:?}"
        );
        let removed = curate(&root, &repo, true);
        assert_eq!(removed.len(), 1);
        assert!(
            !root.join("foreign").symlink_metadata().is_ok()
                && root.join("plugin--fno--x").exists()
        );
    }
}

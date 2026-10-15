//! CI-only marketplace tree builder. Compile with rustc; no installed binary.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn files(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let path = rel.join(entry.file_name());
        let meta = entry.file_type()?;
        if meta.is_symlink() {
            return Err(format!("symlink refused: {}", path.display()).into());
        }
        if meta.is_dir() {
            files(root, &path, out)?;
        } else if meta.is_file() {
            out.push(path);
        } else {
            return Err(format!("non-file refused: {}", path.display()).into());
        }
    }
    Ok(())
}

fn jq(root: &Path, path: &str, filter: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("jq")
        .args(args)
        .arg(filter)
        .arg(root.join(path))
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "invalid {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}

fn check(root: &Path) -> Result<usize> {
    let mut listed = Vec::new();
    files(root, Path::new(""), &mut listed)?;
    if listed.len() > 5000 {
        return Err(format!("plugin has {} files; maximum is 5000", listed.len()).into());
    }
    for path in &listed {
        if path.starts_with("cli") || path.starts_with("crates") {
            return Err(format!("forbidden plugin path: {}", path.display()).into());
        }
        if path.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(".venv" | "__pycache__" | ".git")
            )
        }) {
            return Err(format!("runtime debris: {}", path.display()).into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if fs::metadata(root.join(path))?.nlink() != 1 {
                return Err(format!("hard link refused: {}", path.display()).into());
            }
        }
        if path.starts_with("hooks") && path.extension().is_some_and(|e| e == "sh") {
            let text = fs::read_to_string(root.join(path))?;
            for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
                if line.contains("uv run") || line.contains("uv sync") {
                    return Err(format!(
                        "hook may create a plugin environment: {}",
                        path.display()
                    )
                    .into());
                }
            }
        }
    }
    for manifest in [".claude-plugin/plugin.json", ".codex-plugin/plugin.json"] {
        jq(
            root,
            manifest,
            ".name == \"fno\" and (.version | type == \"string\")",
            &["-e"],
        )?;
    }
    jq(
        root,
        ".agents/plugins/marketplace.json",
        ".plugins | length > 0",
        &["-e"],
    )?;
    let source = fs::read_to_string(root.join("SOURCE_COMMIT"))?;
    if source.trim().len() != 40 || !source.trim().bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("SOURCE_COMMIT must name the full source commit".into());
    }
    if !fs::read_to_string(root.join("README.md"))?.contains("generated from main") {
        return Err("generated README is missing its provenance".into());
    }
    println!(
        "plugin-tree: {} files <= 5000; no cli/, crates/, symlinks, hard links or runtime debris",
        listed.len()
    );
    Ok(listed.len())
}

fn build(source: &Path, dest: &Path, sha: &str, version: &str) -> Result<()> {
    if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("build requires a full source commit".into());
    }
    if dest.exists() {
        return Err("destination must not exist".into());
    }
    let mut selected = BTreeSet::new();
    for dir in [
        "skills",
        "agents",
        "commands",
        "hooks",
        "assets",
        ".claude-plugin",
        ".codex-plugin",
        ".agents/plugins",
    ] {
        let mut entries = Vec::new();
        files(source, Path::new(dir), &mut entries)?;
        selected.extend(entries);
    }
    let mut scripts = Vec::new();
    files(source, Path::new("scripts"), &mut scripts)?;
    // Follow literal repository paths and sibling filenames transitively.
    // The source is a git archive, so ignored build debris cannot enter it.
    let mut pending: Vec<PathBuf> = selected.iter().cloned().collect();
    while let Some(path) = pending.pop() {
        let bytes = fs::read(source.join(path))?;
        let text = String::from_utf8_lossy(&bytes);
        for script in &scripts {
            let rel = script.to_string_lossy();
            let name = script.file_name().unwrap().to_string_lossy();
            if (text.contains(rel.as_ref()) || text.contains(name.as_ref()))
                && selected.insert(script.clone())
            {
                pending.push(script.clone());
            }
        }
    }
    for path in &selected {
        let target = dest.join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::copy(source.join(path), target)?;
    }
    fs::copy(source.join("LICENSE"), dest.join("LICENSE"))?;
    let codex = ".agents/plugins/marketplace.json";
    let bytes = jq(
        dest,
        codex,
        ".plugins |= map(if .name == \"fno\" then .source = {source: \"local\", path: \".\"} else . end)",
        &[],
    )?;
    fs::write(dest.join(codex), bytes)?;
    for manifest in [".claude-plugin/plugin.json", ".codex-plugin/plugin.json"] {
        let bytes = jq(
            dest,
            manifest,
            ".version = $version",
            &["--arg", "version", version],
        )?;
        fs::write(dest.join(manifest), bytes)?;
    }
    fs::write(dest.join("SOURCE_COMMIT"), format!("{sha}\n"))?;
    fs::write(dest.join("README.md"), format!(
        "# Footnote marketplace plugin\n\nThis tree is generated from main at source commit `{sha}`. Do not edit this branch.\n\nSource and contributions: https://github.com/bllshttng/footnote/tree/main\n\nRuntime: install `fno` and `fno-agents` on PATH with https://fno.sh before using the plugin.\n\nClaude and Codex install this same tree. Stable uses `stable`; nightly plugins use `plugin-nightly`. The rolling `nightly` release tag is separate and force-moved by design. Plugin branches advance by fast-forward commits.\n"
    ))?;
    check(dest)?;
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [verb, root] if verb == "check" => check(Path::new(root)).map(|_| ()),
        [verb, source, dest, sha, version] if verb == "build" => {
            build(Path::new(source), Path::new(dest), sha, version)
        }
        _ => Err("usage: plugin-tree build SOURCE DEST SHA VERSION | check TREE".into()),
    };
    if let Err(error) = result {
        eprintln!("plugin-tree: {error}");
        std::process::exit(1);
    }
}

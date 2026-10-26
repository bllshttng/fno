//! Opt-in regression against the real Codex plugin parser: a failed legacy
//! migration restores the config bytes, the legacy cache and the marker.
//! Runs only with FNO_REAL_CODEX_PLUGIN_TEST=1 and a `codex` on PATH; the
//! rust-ci real-codex job installs the pinned Codex and sets it.

use std::fs;
use std::path::{Path, PathBuf};

use fno_agents::codex_converge::{codex_runner, converge, Ran, Request};
use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn codex(home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("codex")
        .args(args)
        .env("CODEX_HOME", home)
        .output()
        .unwrap()
}

/// The repo's loadable payload with a marketplace descriptor pointing at
/// itself, the shape the filtered stage gives Codex.
fn staged_source(tmp: &Path) -> PathBuf {
    let stage = tmp.join("staged-source");
    let root = repo_root();
    for rel in [
        "skills",
        "agents",
        "commands",
        "hooks",
        "assets",
        ".claude-plugin",
        ".codex-plugin",
    ] {
        copy_tree(&root.join(rel), &stage.join(rel));
    }
    let raw = fs::read_to_string(root.join(".agents/plugins/marketplace.json")).unwrap();
    let mut marketplace: Value = serde_json::from_str(&raw).unwrap();
    marketplace["plugins"][0]["source"] = json!({"source": "local", "path": "."});
    let descriptor = stage.join(".agents/plugins/marketplace.json");
    fs::create_dir_all(descriptor.parent().unwrap()).unwrap();
    fs::write(descriptor, marketplace.to_string()).unwrap();
    stage
}

#[test]
fn a_failed_legacy_migration_restores_config_cache_and_marker() {
    if std::env::var("FNO_REAL_CODEX_PLUGIN_TEST").as_deref() != Ok("1") {
        eprintln!("skipped: set FNO_REAL_CODEX_PLUGIN_TEST=1 to run against a real codex");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let stage = staged_source(tmp.path());
    let home = tmp.path().join("codex-home");
    fs::create_dir_all(&home).unwrap();

    let legacy = tmp
        .path()
        .join("legacy-source/.agents/marketplaces/footnote-dev");
    let descriptor = legacy.join(".agents/plugins/marketplace.json");
    fs::create_dir_all(descriptor.parent().unwrap()).unwrap();
    let plugins = json!([{"name": "fno", "source": {"source": "local", "path": "../../.."}}]);
    fs::write(
        &descriptor,
        json!({"name": "footnote-dev", "plugins": plugins}).to_string(),
    )
    .unwrap();
    let config = home.join("config.toml");
    let config_bytes = format!(
        "# an unrelated user comment survives the rollback\n\
         [marketplaces.footnote-dev]\nsource_type = \"local\"\nsource = \"{}\"\n\n\
         [plugins.\"fno@footnote-dev\"]\nenabled = true\n",
        legacy.display()
    );
    fs::write(&config, &config_bytes).unwrap();
    let legacy_cache = home.join("plugins/cache/footnote-dev/fno/0.3.0");
    fs::create_dir_all(&legacy_cache).unwrap();
    fs::write(legacy_cache.join("payload"), "legacy working payload\n").unwrap();
    let marker = home.join("footnote/plugin-channel.json");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let marker_bytes =
        "{\"channel\":\"dev\",\"marketplace\":\"footnote-dev\",\"source\":\"legacy\"}\n";
    fs::write(&marker, marker_bytes).unwrap();

    // The live `plugin add` succeeds in codex, then reports a failure once,
    // so the rollback has real state to undo.
    let mut failed_once = false;
    let mut runner = |argv: &[String], h: &Path| -> Ran {
        let out = codex_runner(argv, h);
        let live_add = h == home && argv.join(" ") == "codex plugin add fno@fno --json";
        if live_add && out.code == 0 && !failed_once {
            failed_once = true;
            return Ran {
                code: 17,
                stdout: out.stdout,
                stderr: "injected post-mutation plugin add failure".into(),
            };
        }
        out
    };
    let req = Request {
        channel: "dev",
        refresh: false,
        source: Some(&stage),
        validate_candidate: true,
    };
    let err = converge(&mut runner, &home, &req).unwrap_err();

    assert_eq!(err.stage, "plugin-add", "{err}");
    assert_eq!(fs::read_to_string(&config).unwrap(), config_bytes);
    assert_eq!(
        fs::read_to_string(legacy_cache.join("payload")).unwrap(),
        "legacy working payload\n"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), marker_bytes);
    assert!(!home.join("plugins/cache/fno").exists());
    let listed = codex(&home, &["plugin", "marketplace", "list", "--json"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let doc: Value = serde_json::from_slice(&listed.stdout).unwrap();
    let names: Vec<&str> = doc["marketplaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert_eq!(names, ["footnote-dev"]);
}

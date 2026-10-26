//! Contracts for the codex converge engine, against a fake Codex that keeps
//! one registry per CODEX_HOME and copies real payloads into its cache, so
//! candidate validation and the digest checks run for real.

use super::*;
use std::collections::HashMap;

#[derive(Default, Clone)]
struct Registry {
    marketplaces: Vec<Value>,
    plugins: Vec<Value>,
}

struct Fake {
    /// What `marketplace add bllshttng/fno` checks out.
    release_root: PathBuf,
    homes: HashMap<PathBuf, Registry>,
    calls: Vec<(PathBuf, Vec<String>)>,
    /// Fail this argv (joined by spaces) in a live home, this many times.
    fail: Option<(String, usize)>,
    /// Answer this argv (joined by spaces) with unparseable stdout, any home.
    garbage: Option<String>,
    live: PathBuf,
}

fn ran(code: i32, stdout: Value, stderr: &str) -> Ran {
    Ran {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.into(),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    let mut files = Vec::new();
    walk_files(from, &mut files);
    for file in files {
        let dest = to.join(file.strip_prefix(from).unwrap());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

impl Fake {
    fn new(tmp: &Path) -> Fake {
        Fake {
            release_root: plugin_source(&tmp.join("release-checkout"), "0.4.0"),
            homes: HashMap::new(),
            calls: Vec::new(),
            fail: None,
            garbage: None,
            live: tmp.join("codex-home"),
        }
    }

    fn call(&mut self, argv: &[String], home: &Path) -> Ran {
        self.calls.push((home.to_path_buf(), argv.to_vec()));
        let joined = argv.join(" ");
        if let Some((target, left)) = &mut self.fail {
            if *left > 0 && *target == joined && home == self.live {
                *left -= 1;
                return ran(17, json!({}), "injected failure");
            }
        }
        if self.garbage.as_deref() == Some(joined.as_str()) {
            return Ran {
                code: 0,
                stdout: "not json".into(),
                stderr: String::new(),
            };
        }
        let a: Vec<&str> = argv.iter().map(String::as_str).collect();
        let release_root = self.release_root.clone();
        let reg = self.homes.entry(home.to_path_buf()).or_default();
        match a.as_slice() {
            ["codex", "plugin", "marketplace", "list", "--json"] => {
                ran(0, json!({"marketplaces": reg.marketplaces}), "")
            }
            ["codex", "plugin", "list", "--json"] => ran(0, json!({"installed": reg.plugins}), ""),
            ["codex", "plugin", "list", "--available", "--marketplace", m, "--json"] => {
                let root = reg
                    .marketplaces
                    .iter()
                    .find(|row| row["name"] == *m)
                    .map(|row| PathBuf::from(row["root"].as_str().unwrap()));
                let version = root.map(|r| manifest_version(&r).unwrap());
                ran(
                    0,
                    json!({"available": [{"pluginId": PLUGIN_ID, "version": version}]}),
                    "",
                )
            }
            ["codex", "plugin", "marketplace", "add", src, ..] => {
                let (kind, root) = if *src == RELEASE_SOURCE {
                    ("git", release_root)
                } else {
                    ("local", PathBuf::from(src))
                };
                let name = if root
                    .file_name()
                    .is_some_and(|n| n == LEGACY_DEV_MARKETPLACE)
                {
                    LEGACY_DEV_MARKETPLACE
                } else {
                    MARKETPLACE
                };
                reg.marketplaces.push(json!({
                    "name": name,
                    "root": root.display().to_string(),
                    "marketplaceSource": {"sourceType": kind, "source": src},
                }));
                ran(0, json!({}), "")
            }
            ["codex", "plugin", "marketplace", "remove", name, "--json"] => {
                reg.marketplaces.retain(|row| row["name"] != *name);
                ran(0, json!({}), "")
            }
            ["codex", "plugin", "marketplace", "upgrade", ..] => ran(0, json!({}), ""),
            ["codex", "plugin", "remove", id, "--json"] => {
                reg.plugins.retain(|row| row["pluginId"] != *id);
                ran(0, json!({}), "")
            }
            ["codex", "plugin", "add", id, "--json"] => {
                let market_name = id.split_once('@').unwrap().1;
                let Some(market) = reg
                    .marketplaces
                    .iter()
                    .find(|row| row["name"] == market_name)
                    .cloned()
                else {
                    return ran(1, json!({}), "no such marketplace");
                };
                let root = PathBuf::from(market["root"].as_str().unwrap());
                let version = manifest_version(&root).unwrap();
                copy_tree(
                    &root,
                    &cache_dir(home, market_name).join(PLUGIN_DIR).join(&version),
                );
                reg.plugins.push(json!({
                    "pluginId": id,
                    "marketplaceName": market_name,
                    "version": version,
                    "installed": true,
                    "enabled": true,
                    "marketplaceSource": market["marketplaceSource"],
                }));
                ran(0, json!({}), "")
            }
            _ => ran(1, json!({}), &format!("unexpected {joined}")),
        }
    }

    fn live_calls(&self) -> Vec<String> {
        self.calls
            .iter()
            .filter(|(home, _)| *home == self.live)
            .map(|(_, argv)| argv.join(" "))
            .collect()
    }
}

fn plugin_source(root: &Path, version: &str) -> PathBuf {
    fs::create_dir_all(root.join(".codex-plugin")).unwrap();
    fs::write(
        root.join(".codex-plugin/plugin.json"),
        json!({"name": "fno", "version": version}).to_string(),
    )
    .unwrap();
    fs::create_dir_all(root.join("skills/target")).unwrap();
    fs::write(root.join("skills/target/SKILL.md"), "# target\n").unwrap();
    root.to_path_buf()
}

fn converge_with(
    fake: &mut Fake,
    channel: &str,
    source: Option<&Path>,
    refresh: bool,
) -> Res<Converged> {
    let home = fake.live.clone();
    let req = Request {
        channel,
        refresh,
        source,
        validate_candidate: true,
    };
    converge(
        &mut |argv: &[String], h: &Path| fake.call(argv, h),
        &home,
        &req,
    )
}

fn inspect_with(fake: &mut Fake) -> Value {
    let home = fake.live.clone();
    inspect(&mut |argv: &[String], h: &Path| fake.call(argv, h), &home)
}

/// A machine on the pre-rename dev identity: `fno@footnote-dev` from a
/// `footnote-dev` marketplace, with its cache.
fn legacy_dev_machine(fake: &mut Fake, tmp: &Path) {
    let legacy = plugin_source(&tmp.join("old").join(LEGACY_DEV_MARKETPLACE), "0.2.0");
    let live = fake.live.clone();
    for argv in [
        vec![
            "codex",
            "plugin",
            "marketplace",
            "add",
            legacy.to_str().unwrap(),
            "--json",
        ],
        vec!["codex", "plugin", "add", "fno@footnote-dev", "--json"],
    ] {
        let argv: Vec<String> = argv.into_iter().map(String::from).collect();
        assert_eq!(fake.call(&argv, &live).code, 0);
    }
    fake.calls.clear();
}

#[test]
fn release_replaces_the_legacy_dev_identity_and_reads_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(tmp.path());
    legacy_dev_machine(&mut fake, tmp.path());

    let done = converge_with(&mut fake, "release", None, false).unwrap();
    assert_eq!(
        (done.action, done.plugin_id.as_str(), done.version.as_str()),
        ("installed", PLUGIN_ID, "0.4.0")
    );
    assert_eq!(
        fake.live_calls(),
        [
            "codex plugin marketplace list --json",
            "codex plugin list --json",
            "codex plugin remove fno@footnote-dev --json",
            "codex plugin marketplace remove footnote-dev --json",
            "codex plugin marketplace add bllshttng/fno --ref stable --json",
            "codex plugin marketplace upgrade fno --json",
            "codex plugin add fno@fno --json",
            "codex plugin marketplace list --json",
            "codex plugin list --json",
        ]
    );
    let marker: Value =
        serde_json::from_slice(&fs::read(marker_path(&fake.live)).unwrap()).unwrap();
    assert_eq!(
        marker,
        json!({"channel": "release", "marketplace": "fno", "ref": "stable", "source": RELEASE_SOURCE})
    );
    assert!(
        !cache_dir(&fake.live, LEGACY_DEV_MARKETPLACE).exists(),
        "legacy cache removed"
    );
    assert_eq!(inspect_with(&mut fake)["status"], "fresh");

    // A converged machine is a no-op that only reads the live state, so an
    // offline host never fails candidate validation.
    fake.calls.clear();
    assert_eq!(
        converge_with(&mut fake, "release", None, false)
            .unwrap()
            .action,
        "no-op"
    );
    assert_eq!(fake.calls.len(), 2);
}

#[test]
fn a_marker_without_the_stable_ref_is_repaired_not_trusted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(tmp.path());
    converge_with(&mut fake, "release", None, false).unwrap();
    // A machine converged before `--ref stable` existed has no ref key.
    let old = json!({"channel": "release", "marketplace": "fno", "source": RELEASE_SOURCE});
    fs::write(marker_path(&fake.live), format!("{old}\n")).unwrap();
    assert_eq!(inspect_with(&mut fake)["issue"], "wrong-marketplace-source");

    assert_eq!(
        converge_with(&mut fake, "release", None, false)
            .unwrap()
            .action,
        "repaired"
    );
    assert_eq!(inspect_with(&mut fake)["status"], "fresh");
}

#[test]
fn dev_reads_payload_drift_until_a_forced_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(tmp.path());
    let stage = plugin_source(&tmp.path().join("stage"), "0.3.0");
    let done = converge_with(&mut fake, "dev", Some(&stage), false).unwrap();
    assert_eq!(done.action, "installed");
    assert_eq!(inspect_with(&mut fake)["status"], "fresh");

    // Same version, new payload: codex keeps its cache until a refresh.
    fs::write(stage.join("skills/target/SKILL.md"), "# target v2\n").unwrap();
    let verdict = inspect_with(&mut fake);
    assert_eq!(
        (verdict["status"].as_str(), verdict["issue"].as_str()),
        (Some("stale"), Some("payload-drift"))
    );
    assert_eq!(verdict["remedy"], DEV_REMEDY);
    assert_eq!(
        converge_with(&mut fake, "dev", Some(&stage), false)
            .unwrap()
            .action,
        "no-op"
    );

    assert_eq!(
        converge_with(&mut fake, "dev", Some(&stage), true)
            .unwrap()
            .action,
        "refreshed"
    );
    assert_eq!(inspect_with(&mut fake)["status"], "fresh");
}

#[test]
fn a_failed_switch_restores_the_working_install_and_its_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(tmp.path());
    let stage = plugin_source(&tmp.path().join("stage"), "0.3.0");
    converge_with(&mut fake, "dev", Some(&stage), false).unwrap();
    let config = "# a user comment survives\n[plugins.\"fno@fno\"]\nenabled = true\n";
    fs::write(fake.live.join("config.toml"), config).unwrap();
    let marker = fs::read(marker_path(&fake.live)).unwrap();
    let before = fingerprint(&fake.homes[&fake.live].clone().into_state());

    fake.fail = Some(("codex plugin add fno@fno --json".into(), 1));
    let err = converge_with(&mut fake, "release", None, false).unwrap_err();
    assert_eq!(err.stage, "plugin-add");
    assert_eq!(fs::read(marker_path(&fake.live)).unwrap(), marker);
    assert_eq!(
        fs::read_to_string(fake.live.join("config.toml")).unwrap(),
        config
    );
    assert_eq!(
        fingerprint(&fake.homes[&fake.live].clone().into_state()),
        before
    );
    assert!(
        version_cache(&fake.live, "0.3.0").is_dir(),
        "the quarantined cache is back"
    );
    assert_eq!(inspect_with(&mut fake)["status"], "fresh");
}

#[test]
fn a_failed_rollback_is_persisted_and_named_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(tmp.path());
    let stage = plugin_source(&tmp.path().join("stage"), "0.3.0");
    converge_with(&mut fake, "dev", Some(&stage), false).unwrap();

    // The switch fails, then the rollback's re-add fails too.
    fake.fail = Some(("codex plugin add fno@fno --json".into(), 2));
    let err = converge_with(&mut fake, "release", None, false).unwrap_err();
    assert_eq!(err.stage, "rollback-failure");
    assert!(err
        .detail
        .starts_with("plugin-add: injected failure; rollback failed:"));
    let verdict = inspect_with(&mut fake);
    assert_eq!(
        (verdict["status"].as_str(), verdict["issue"].as_str()),
        (Some("error"), Some("rollback-failure"))
    );
    assert_eq!(verdict["remedy"], remedy("release"));
    // The stranded quarantine carries the 32-hex suffix reclaim sweeps.
    let stranded: Vec<String> = fs::read_dir(owned_dir(&fake.live))
        .unwrap()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_prefix(".fno.")
                .map(str::to_string)
        })
        .collect();
    assert_eq!(stranded.len(), 1, "{stranded:?}");
    assert!(
        stranded[0].len() == 32
            && stranded[0]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );

    // The failed rollback left no plugin. The next converge clears the
    // receipt and installs the release channel.
    assert_eq!(
        converge_with(&mut fake, "release", None, false)
            .unwrap()
            .action,
        "installed"
    );
    assert!(!receipt_path(&fake.live).exists());
}

#[test]
fn every_refusal_lands_before_the_live_install_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fake = Fake::new(tmp.path());
    legacy_dev_machine(&mut fake, tmp.path());
    let untouched = |fake: &Fake| {
        let moved = fake
            .live_calls()
            .iter()
            .any(|c| c.contains(" remove ") || c.contains(" add "));
        !moved && !marker_path(&fake.live).exists()
    };

    // Unreadable live state.
    fake.garbage = Some("codex plugin list --json".into());
    let err = converge_with(&mut fake, "release", None, false).unwrap_err();
    assert_eq!(err.stage, "plugin-list");
    assert!(untouched(&fake));

    // A candidate Codex cannot parse never replaces the working channel.
    fake.garbage = Some("codex plugin list --available --marketplace fno --json".into());
    let err = converge_with(&mut fake, "release", None, false).unwrap_err();
    assert_eq!(err.stage, "candidate-plugin-list");
    assert!(untouched(&fake));

    // A symlinked legacy cache is refused, not followed.
    fake.garbage = None;
    let cache = cache_dir(&fake.live, LEGACY_DEV_MARKETPLACE);
    fs::remove_dir_all(&cache).unwrap();
    std::os::unix::fs::symlink(tmp.path(), &cache).unwrap();
    let err = converge_with(&mut fake, "release", None, false).unwrap_err();
    assert_eq!(err.stage, "cache-quarantine");
    assert!(untouched(&fake));
}

#[test]
fn owned_config_restores_exact_bytes_but_never_over_a_foreign_change() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let original = "# keep me\nmodel = \"o3\"\n\n[marketplaces.footnote-dev]\nsource = \"/old\"\n";
    fs::write(home.join("config.toml"), original).unwrap();
    let snapshot = read_owned_config(home).unwrap();

    fs::write(home.join("config.toml"), "model = \"o3\"\n").unwrap();
    restore_owned_config(home, &snapshot).unwrap();
    assert_eq!(
        fs::read_to_string(home.join("config.toml")).unwrap(),
        original
    );

    fs::write(home.join("config.toml"), "model = \"o4\"\n").unwrap();
    let err = restore_owned_config(home, &snapshot).unwrap_err();
    assert_eq!(err.detail, "non-Footnote config changed during convergence");
}

#[test]
fn the_digest_hashes_only_the_loadable_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let root = plugin_source(&tmp.path().join("p"), "0.3.0");
    let first = payload_digest(&root).unwrap();
    fs::write(root.join("README.md"), "not loadable").unwrap();
    fs::create_dir_all(root.join("skills/__pycache__")).unwrap();
    fs::write(root.join("skills/__pycache__/x.pyc"), "bytecode").unwrap();
    fs::write(root.join("skills/.DS_Store"), "finder").unwrap();
    assert_eq!(payload_digest(&root).unwrap(), first);
    fs::write(root.join("skills/target/SKILL.md"), "changed").unwrap();
    assert_ne!(payload_digest(&root).unwrap(), first);

    let row = json!({"marketplaces": []}).to_string();
    let enabled_not_installed =
        json!({"installed": [{"pluginId": PLUGIN_ID, "installed": false, "enabled": true}]});
    let err = parse_state(&row, &enabled_not_installed.to_string()).unwrap_err();
    assert_eq!(err.detail, "enabled plugin is not installed");
}

impl Registry {
    fn into_state(self) -> State {
        parse_state(
            &json!({"marketplaces": self.marketplaces}).to_string(),
            &json!({"installed": self.plugins}).to_string(),
        )
        .unwrap()
    }
}

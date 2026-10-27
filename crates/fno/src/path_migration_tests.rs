//! Contracts for the first-run path migration, replacing the deleted Python
//! tests: the managed keys come from the schema table, a hand-edited file's
//! foreign keys survive behind a backup, and a vault link turns obsidian on.

use crate::path_migration::*;
use serde_json::{json, Value};

#[test]
fn overlay_spells_every_managed_key_at_its_default() {
    let overlay = managed_overlay(None);
    let config = &overlay["config"];
    assert_eq!(overlay["schema_version"], json!(1));
    assert_eq!(config["state_dir"], json!("~/.fno/"));
    assert_eq!(config["plans_dir"], json!(".fno/plans/"));
    assert_eq!(config["obsidian"], json!({"enabled": false, "vault": null}));
    assert_eq!(config["project"], json!({"id": null, "vision": null}));
    assert_eq!(config["paths"]["worktrees_base"], Value::Null);
}

#[test]
fn migration_backs_up_a_hand_edited_file_and_keeps_its_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("settings.yaml"),
        "config:\n  providers:\n    claude: enabled\n  state_dir: /custom/\n",
    )
    .unwrap();

    let summary = migrate(&root, dir.path()).unwrap().unwrap();
    assert!(
        summary.contains("backed up prior settings.yaml"),
        "{summary}"
    );
    assert!(summary.contains("fresh install"), "{summary}");
    let written: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(root.join("settings.yaml")).unwrap())
            .unwrap();
    let written = crate::config_cli::yaml_to_json(written);
    assert_eq!(written["config"]["providers"], json!({"claude": "enabled"}));
    assert_eq!(written["config"]["state_dir"], json!("~/.fno/"));
    assert!(root.join(".path-migration-done").exists());
    assert_eq!(std::fs::read_dir(root.join("backups")).unwrap().count(), 1);

    // The sentinel makes a second run a no-op.
    assert_eq!(migrate(&root, dir.path()).unwrap(), None);
}

#[test]
fn an_internal_link_into_a_vault_enables_obsidian() {
    let dir = tempfile::tempdir().unwrap();
    let vault = std::fs::canonicalize(dir.path()).unwrap().join("vault");
    std::fs::create_dir_all(vault.join(".obsidian")).unwrap();
    std::fs::create_dir_all(vault.join("internal")).unwrap();
    let cwd = dir.path().join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    std::os::unix::fs::symlink(vault.join("internal"), cwd.join("internal")).unwrap();

    assert_eq!(detect_vault(&cwd), Some(vault.clone()));
    let overlay = managed_overlay(Some(&vault));
    assert_eq!(overlay["config"]["obsidian"]["enabled"], json!(true));
    assert_eq!(
        overlay["config"]["obsidian"]["vault"],
        json!(vault.display().to_string())
    );
}

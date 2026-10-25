//! Contracts for the cli-hooks installers, replacing the deleted Python
//! installer tests: merge without clobbering, backup before write, the Codex
//! layer refusals and legacy migration, and Claude's repair rules.

use super::*;

const CMD: &str = "/opt/footnote/hooks/session-start.sh";

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn codex_toml(command: &str) -> String {
    format!(
        "[[hooks.SessionStart]]\n\n[[hooks.SessionStart.hooks]]\ntype = \"command\"\ncommand = {}\n",
        Value::String(command.into())
    )
}

fn session_hooks(command: &str) -> Value {
    json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": command}]}]}})
}

#[test]
fn codex_appends_refuses_malformed_layers_and_migrates_only_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let legacy = dir.path().join("hooks.json");

    // An append keeps the user's comments and hooks, and backs up first.
    let original = "# my codex config\nmodel = \"gpt-5\"\n\n[[hooks.Stop]]\n\n\
                    [[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"my-stop.sh\"\n";
    std::fs::write(&config, original).unwrap();
    let res = install_codex(CMD, &config, &legacy, false);
    assert!(
        res.changed && res.needs_trust && res.error.is_none(),
        "{res:?}"
    );
    assert_eq!(
        std::fs::read_to_string(res.backup.unwrap()).unwrap(),
        original
    );
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.starts_with(original) && text.contains(crate::uninstall::CODEX_BLOCK_MARK));
    let parsed = parse_toml(&text).unwrap();
    assert_eq!(
        parsed["hooks"]["Stop"][0]["hooks"][0]["command"],
        "my-stop.sh"
    );
    let commands = session_start_commands(&parsed, "config.toml").unwrap();
    assert_eq!(commands, vec![format!("env FNO_PLATFORM=codex {CMD}")]);
    // A re-run is a no-op.
    let again = install_codex(CMD, &config, &legacy, false);
    assert!(!again.changed && again.already_present);

    // Both layers holding our hook: warn, and leave the JSON byte-identical.
    std::fs::write(&config, codex_toml(CMD)).unwrap();
    std::fs::write(&legacy, session_hooks(CMD).to_string()).unwrap();
    let before = std::fs::read(&legacy).unwrap();
    let res = install_codex(CMD, &config, &legacy, false);
    let note = res.note.unwrap();
    assert!(note.contains("both Codex hook layers") && note.contains("TOML is preferred"));
    assert_eq!(std::fs::read(&legacy).unwrap(), before);

    // Migration drops only our SessionStart hook, keeps foreign events and
    // the description, and backs up the exact original bytes.
    let mixed = json!({
        "description": "Mixed legacy hooks",
        "hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": CMD}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "foreign-stop.sh"}]}],
        }
    });
    let mixed_text = serde_json::to_string_pretty(&mixed).unwrap() + "\n";
    std::fs::write(&legacy, &mixed_text).unwrap();
    let res = install_codex(CMD, &config, &legacy, true);
    let remaining = read_json(&legacy);
    assert!(remaining["hooks"].get("SessionStart").is_none());
    assert_eq!(
        remaining["hooks"]["Stop"][0]["hooks"][0]["command"],
        "foreign-stop.sh"
    );
    assert_eq!(remaining["description"], "Mixed legacy hooks");
    assert_eq!(
        std::fs::read_to_string(res.legacy_backup.unwrap()).unwrap(),
        mixed_text
    );
    assert!(res.note.unwrap().contains("foreign hooks were preserved"));

    // A wholly footnote-owned legacy file is removed outright.
    std::fs::write(&legacy, session_hooks(CMD).to_string()).unwrap();
    install_codex(CMD, &config, &legacy, true);
    assert!(!legacy.exists());

    // A malformed layer refuses before any write.
    let fresh = dir.path().join("fresh.toml");
    std::fs::write(&legacy, "{not json").unwrap();
    let res = install_codex(CMD, &fresh, &legacy, true);
    assert!(res.error.unwrap().contains("malformed") && !res.changed && !res.needs_trust);
    assert!(!fresh.exists());
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "{not json");
}

#[test]
fn claude_merges_quotes_repairs_and_never_ping_pongs() {
    let dir = tempfile::tempdir().unwrap();
    let script = |name: &str, live: bool| {
        let p = dir.path().join(name).join("hooks/worktree-remove.sh");
        if live {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "#!/usr/bin/env bash\n").unwrap();
        }
        p.display().to_string()
    };
    let commands = |path: &Path| -> Vec<String> {
        read_json(path)["hooks"]["WorktreeRemove"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().clone())
            .map(|h| h["command"].as_str().unwrap().to_string())
            .collect()
    };

    // A merge keeps other settings and hooks, and quotes a spacey path.
    let settings = dir.path().join("settings.json");
    std::fs::write(
        &settings,
        json!({"model": "opus", "hooks": {"WorktreeRemove": [{"hooks": [{"command": "/other/plugin.sh"}]}]}})
            .to_string(),
    )
    .unwrap();
    let spacey = script("My Plugins", true);
    let res = install_claude_worktree_remove(&spacey, &settings, false);
    assert!(res.changed && res.backup.is_some());
    assert_eq!(read_json(&settings)["model"], "opus");
    let wired = commands(&settings);
    assert_eq!(wired[0], "/other/plugin.sh");
    assert_eq!(
        shlex::split(&wired[1]).unwrap(),
        vec!["bash".to_string(), spacey.clone()]
    );
    assert!(install_claude_worktree_remove(&spacey, &settings, false).already_present);

    // A malformed file is a refusal and stays untouched.
    let broken = dir.path().join("broken.json");
    std::fs::write(&broken, "{not json").unwrap();
    let res = install_claude_worktree_remove(&spacey, &broken, false);
    assert!(res.error.unwrap().contains("malformed") && !res.changed);
    assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{not json");

    // A dead path is repaired in place, even by the automatic repair.
    let fixed = dir.path().join("fixed.json");
    install_claude_worktree_remove(&script("archived", false), &fixed, false);
    let live = script("canonical", true);
    let res = install_claude_worktree_remove(&live, &fixed, true);
    assert!(res.changed && res.note.unwrap().starts_with("re-pointed from"));
    assert_eq!(commands(&fixed), vec![format!("bash {}", sh_quote(&live))]);

    // A live other root: an explicit install converges, the repair holds.
    let (a, b) = (script("a", true), script("b", true));
    let pinned = dir.path().join("pinned.json");
    install_claude_worktree_remove(&a, &pinned, false);
    let before = std::fs::read_to_string(&pinned).unwrap();
    let res = install_claude_worktree_remove(&b, &pinned, true);
    assert!(res.already_present && !res.changed);
    assert_eq!(std::fs::read_to_string(&pinned).unwrap(), before);
    assert!(install_claude_worktree_remove(&b, &pinned, false).changed);
    assert_eq!(commands(&pinned), vec![format!("bash {}", sh_quote(&b))]);

    // Repair never wires a hook that was not there.
    let bare = dir.path().join("bare.json");
    std::fs::write(&bare, "{\"model\": \"opus\"}").unwrap();
    let res = install_claude_worktree_remove(&b, &bare, true);
    assert!(!res.changed && !res.already_present);
    assert_eq!(read_json(&bare), json!({"model": "opus"}));
}

#[test]
fn the_flags_parse_and_gemini_merges_once() {
    let oss = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
    let o = parse_opts(
        &oss(&["--no-claude", "--gemini-settings=/g.json", "--repair-only"]),
        false,
    )
    .unwrap()
    .unwrap();
    assert!(o.codex && o.gemini && !o.claude && o.repair_only);
    assert_eq!(o.gemini_settings, Some(PathBuf::from("/g.json")));
    let o = parse_opts(&oss(&["--migrate-legacy-hooks-json"]), true)
        .unwrap()
        .unwrap();
    assert!(o.codex && !o.gemini && !o.claude && o.migrate_legacy_hooks_json);
    assert!(parse_opts(&oss(&["--repair-only"]), true).is_err());
    assert!(parse_opts(&oss(&["--codex-config"]), false).is_err());
    assert!(parse_opts(&oss(&["--codex=false"]), false).is_err());
    assert_eq!(parse_opts(&oss(&["--help"]), false).unwrap(), None);

    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("g/settings.json");
    let res = install_gemini(CMD, &settings);
    assert!(res.changed && res.backup.is_none());
    let hook = &read_json(&settings)["hooks"]["SessionStart"][0]["hooks"][0];
    assert_eq!(hook["command"], format!("env FNO_PLATFORM=gemini {CMD}"));
    assert_eq!(hook["name"], GEMINI_HOOK_NAME);
    assert!(install_gemini(CMD, &settings).already_present);
    std::fs::write(&settings, "[1]").unwrap();
    assert!(install_gemini(CMD, &settings)
        .note
        .unwrap()
        .contains("root is list"));
}

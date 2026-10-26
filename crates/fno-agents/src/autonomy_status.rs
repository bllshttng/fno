//! `fno agents autonomy status`: one table listing every path that can start
//! a session without an operator asking - its trigger, its gate key, its
//! resolved value, and the precedence rank that supplied it. The Rust owner,
//! ported from the deleted `cli/src/fno/autonomy_cli.py`.
//!
//! The deliverable an operator actually asked for: "what can spawn without
//! me" answerable in one command. Each spawner is resolved THROUGH ITS REAL
//! RESOLVER, never by reading config directly - a status command that read
//! config.auto_continue.enabled directly would have printed "false"
//! throughout the six-and-a-half-week incident that motivated this command,
//! because a since-removed marker file silently outranked it. A spawner with
//! no gate at all renders as `ungated` rather than being omitted from the
//! table - omission is exactly what hid them.
//!
//! Exit code is always 0 - this is introspection, never a gate (AC9-ERR).

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::backlog::advance::auto_continue_resolve;
use crate::backlog::advance_settings::{autonomy_master_enabled, load_merged};
use crate::naming;

/// One row of the autonomy table.
struct SpawnerStatus {
    name: &'static str,
    trigger: &'static str,
    gate_key: &'static str,
    /// `None` = ungated (no gate exists yet).
    armed: Option<bool>,
    /// "env" | "config" | "default" | "autonomy" | "ungated".
    rank: &'static str,
    /// dispatch source code; `None` = n/a.
    source: Option<String>,
    /// dispatch command code.
    command: Option<String>,
}

/// A direct-config-read row's shared shape: the master panic switch is
/// checked FIRST (rank "autonomy" when it vetoes), matching the precedence
/// every resolver-backed spawner with a gate applies to itself.
fn gate_with_master(
    root: Option<&Path>,
    read: impl FnOnce(&Value) -> bool,
) -> (Option<bool>, &'static str) {
    if !autonomy_master_enabled(root) {
        return (Some(false), "autonomy");
    }
    let settings = load_merged(root);
    (Some(read(&settings)), "config")
}

/// A typed block's `enabled` with the model's default for an absent or
/// malformed value - the same fail-safe direction `_coerce_bool_default_true`
/// (true-default gates) and the strict-affirmative validators (false-default
/// gates) land on: garbage answers the default.
fn block_enabled(settings: &Value, block: &str, default: bool) -> bool {
    settings
        .get(block)
        .and_then(|b| b.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

/// The env override's truthy read, byte-parity with the Python legs:
/// `env.strip().lower() in {"1", "true", "yes", "on"}`. Takes the raw value,
/// not the name: the env-registry scanner only sees literal `env::var*` call
/// shapes, so each override reads its name at its own call site.
fn env_truth(raw: &str) -> bool {
    let value = raw.trim().to_ascii_lowercase();
    ["1", "true", "yes", "on"].contains(&value.as_str())
}

/// spawn_think's precedence: master switch, then `FNO_THINK_SPAWN`, then
/// `config.think_spawn.enabled` (default False).
fn spawn_think_resolve(root: Option<&Path>) -> (Option<bool>, &'static str) {
    if !autonomy_master_enabled(root) {
        return (Some(false), "autonomy");
    }
    if let Ok(raw) = std::env::var("FNO_THINK_SPAWN") {
        return (Some(env_truth(&raw)), "env");
    }
    let settings = load_merged(root);
    (
        Some(block_enabled(&settings, "think_spawn", false)),
        "config",
    )
}

/// keep_going's precedence: master switch, then `FNO_KEEP_GOING`, then
/// `config.keep_going.enabled` (default False; the opt-in engine). Resolved
/// inline rather than through a shared helper that swallows its own settings
/// exception and returns a plain False with no rank exposed.
fn keep_going_resolve(root: Option<&Path>) -> (Option<bool>, &'static str) {
    if !autonomy_master_enabled(root) {
        return (Some(false), "autonomy");
    }
    if let Ok(raw) = std::env::var("FNO_KEEP_GOING") {
        return (Some(env_truth(&raw)), "env");
    }
    let settings = load_merged(root);
    (
        Some(block_enabled(&settings, "keep_going", false)),
        "config",
    )
}

/// Resolve every known spawner's armed state through its real resolver.
fn collect_status(root: Option<&Path>) -> Vec<SpawnerStatus> {
    let (advance_armed, advance_rank) = auto_continue_resolve(root);
    let master_on = autonomy_master_enabled(root);
    let (post_merge_armed, post_merge_rank) =
        gate_with_master(root, |s| block_enabled(s, "post_merge", true));
    let (pr_watch_armed, pr_watch_rank) =
        gate_with_master(root, |s| block_enabled(s, "pr_watch", false));
    let (recovery_armed, recovery_rank) =
        gate_with_master(root, |s| block_enabled(s, "recovery", true));
    let (keep_going_armed, keep_going_rank) = keep_going_resolve(root);
    let (groom_armed, groom_rank) = gate_with_master(root, |s| block_enabled(s, "groom", true));
    let (evals_armed, evals_rank) = gate_with_master(root, |s| block_enabled(s, "evals", true));
    let (lead_armed, lead_rank) = gate_with_master(root, |s| block_enabled(s, "lead", false));
    let (think_armed, think_rank) = spawn_think_resolve(root);
    vec![
        row(
            "autonomy (master switch)",
            "every row below",
            "config.autonomy.enabled",
            Some(master_on),
            "config",
        ),
        row(
            "advance (node-walk)",
            "PR merge (reconcile / /fno:ship pr merged)",
            "config.auto_continue.enabled",
            Some(advance_armed),
            advance_rank,
        ),
        row(
            "dispatch_lanes (parallel fill)",
            "active-backlog daemon tick",
            "config.auto_continue.enabled",
            Some(advance_armed),
            advance_rank,
        ),
        row(
            "epic converge (mission drain)",
            "fno backlog advance --epic",
            "config.auto_continue.enabled",
            Some(advance_armed),
            advance_rank,
        ),
        row(
            "reconcile_dispatch (G4 de-stub)",
            "PR merge (contract dependent's blocker closes)",
            "config.auto_continue.enabled",
            Some(advance_armed),
            advance_rank,
        ),
        row(
            "spawn_think (context /think)",
            "node birth / work-start / retro",
            "config.think_spawn.enabled",
            think_armed,
            think_rank,
        ),
        row(
            "post-merge ritual",
            "PR merge",
            "config.post_merge.enabled",
            post_merge_armed,
            post_merge_rank,
        ),
        row(
            "pr_watch (headless PR poll)",
            "launchd interval tick",
            "config.pr_watch.enabled",
            pr_watch_armed,
            pr_watch_rank,
        ),
        row(
            "recovery sweep (crash respawn)",
            "pr_watch launchd interval tick",
            "config.recovery.enabled",
            recovery_armed,
            recovery_rank,
        ),
        row(
            "keep_going (autonomous follow-up)",
            "post-merge ritual carve-out drain",
            "config.keep_going.enabled",
            keep_going_armed,
            keep_going_rank,
        ),
        row(
            "groom (_spawn_groom_worker)",
            "fno backlog groom",
            "config.groom.enabled",
            groom_armed,
            groom_rank,
        ),
        row(
            "evals runner",
            "fno doctor evals run",
            "config.evals.enabled",
            evals_armed,
            evals_rank,
        ),
        row(
            "lead loop",
            "board non-empty (stop hook / loop run)",
            "config.lead.enabled",
            lead_armed,
            lead_rank,
        ),
    ]
}

fn row(
    name: &'static str,
    trigger: &'static str,
    gate_key: &'static str,
    armed: Option<bool>,
    rank: &'static str,
) -> SpawnerStatus {
    SpawnerStatus {
        name,
        trigger,
        gate_key,
        armed,
        rank,
        source: None,
        command: None,
    }
}

/// Stamp each row with its dispatch provenance codes, matching by the row's
/// spawner name (the provenance table's site column).
fn stamp_provenance(rows: &mut [SpawnerStatus]) {
    for row in rows.iter_mut() {
        let codes = naming::provenance_rows()
            .iter()
            .find(|(site, _, _)| site == row.name)
            .map(|(_, s, c)| (s.to_string(), c.to_string()));
        row.source = codes.as_ref().map(|(s, _)| s.clone());
        row.command = codes.map(|(_, c)| c);
    }
}

/// The rendered table, byte-parity with the Python leg's `format_table`:
/// two-space column separator, every cell ljust-padded (including the last
/// column, so rows carry trailing spaces), dashes under the header.
fn format_table(rows: &[SpawnerStatus]) -> String {
    let headers = [
        "SPAWNER", "TRIGGER", "GATE KEY", "ARMED", "RANK", "SOURCE", "COMMAND",
    ];
    let table_rows: Vec<[String; 7]> = rows
        .iter()
        .map(|r| {
            let armed_cell = match r.armed {
                None => "ungated".to_string(),
                Some(true) => "true".to_string(),
                Some(false) => "false".to_string(),
            };
            [
                r.name.to_string(),
                r.trigger.to_string(),
                r.gate_key.to_string(),
                armed_cell,
                r.rank.to_string(),
                r.source.clone().unwrap_or_else(|| "-".to_string()),
                r.command.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in &table_rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut lines = vec![
        headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  "),
        headers
            .iter()
            .enumerate()
            .map(|(i, _)| "-".repeat(widths[i]))
            .collect::<Vec<_>>()
            .join("  "),
    ];
    for row in &table_rows {
        lines.push(
            row.iter()
                .enumerate()
                .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
                .collect::<Vec<_>>()
                .join("  "),
        );
    }
    lines.join("\n")
}

/// The command body: `fno-agents autonomy status [--project-root P]`. Always
/// exits 0 (AC9-ERR): a read, never a gate. Even a repo with no fno config at
/// all prints every spawner's fail-safe default rather than raising.
pub fn run(args: &[String]) -> i32 {
    if args.is_empty() || args[0] != "status" {
        eprintln!("usage: fno-agents autonomy status [--project-root P]");
        return 2;
    }
    let mut root: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--project-root" if i + 1 < args.len() => {
                root = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            other => {
                eprintln!("unknown autonomy flag {other:?}");
                return 2;
            }
        }
    }
    let mut rows = collect_status(root.as_deref());
    stamp_provenance(&mut rows);
    println!("{}", format_table(&rows));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process-env pin with restore-on-drop: the table's env ranks read
    /// real env vars, and a parallel test's unpinned read must not race it.
    struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var_os(name);
            std::env::set_var(name, value);
            EnvGuard(vec![(name, prior)])
        }

        fn clear(name: &'static str) -> Self {
            let prior = std::env::var_os(name);
            std::env::remove_var(name);
            EnvGuard(vec![(name, prior)])
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, prior) in self.0.drain(..) {
                match prior {
                    Some(v) => std::env::set_var(name, v),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    fn row_by_name<'a>(rows: &'a [SpawnerStatus], name: &str) -> &'a SpawnerStatus {
        rows.iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("spawner row {name:?} missing"))
    }

    #[test]
    fn format_table_matches_the_captured_python_bytes() {
        // Golden captured from cli/src/fno/autonomy_cli.py format_table
        // before deletion (port protocol). Trailing spaces included: every
        // cell ljust-pads, the last column too.
        let mut rows = vec![
            row(
                "autonomy (master switch)",
                "every row below",
                "config.autonomy.enabled",
                Some(true),
                "config",
            ),
            row(
                "advance (node-walk)",
                "PR merge (reconcile / /fno:ship pr merged)",
                "config.auto_continue.enabled",
                Some(false),
                "default",
            ),
            row(
                "groom (_spawn_groom_worker)",
                "fno backlog groom",
                "config.groom.enabled",
                None,
                "ungated",
            ),
            row(
                "evals runner",
                "fno doctor evals run",
                "config.evals.enabled",
                Some(false),
                "config",
            ),
        ];
        rows[1].source = Some("AG".into());
        rows[1].command = Some("TG".into());
        let rendered = format_table(&rows);
        let expected = [
            "SPAWNER                      TRIGGER                                     GATE KEY                      ARMED    RANK     SOURCE  COMMAND",
            "---------------------------  ------------------------------------------  ----------------------------  -------  -------  ------  -------",
            "autonomy (master switch)     every row below                             config.autonomy.enabled       true     config   -       -      ",
        ];
        for (i, line) in rendered.lines().enumerate() {
            if i < expected.len() {
                assert_eq!(line, expected[i], "line {i}");
            }
        }
        assert!(rendered.contains("AG"), "provenance stamps render");
        assert!(rendered.contains("TG"));
    }

    #[test]
    fn every_known_spawner_appears_in_table_order() {
        let rows = collect_status(None);
        let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            vec![
                "autonomy (master switch)",
                "advance (node-walk)",
                "dispatch_lanes (parallel fill)",
                "epic converge (mission drain)",
                "reconcile_dispatch (G4 de-stub)",
                "spawn_think (context /think)",
                "post-merge ritual",
                "pr_watch (headless PR poll)",
                "recovery sweep (crash respawn)",
                "keep_going (autonomous follow-up)",
                "groom (_spawn_groom_worker)",
                "evals runner",
                "lead loop",
            ]
        );
    }

    #[test]
    fn env_override_rank_is_visible() {
        let _guard = EnvGuard::set("FNO_KEEP_GOING", "1");
        let rows = collect_status(None);
        let keep = row_by_name(&rows, "keep_going (autonomous follow-up)");
        assert_eq!(keep.armed, Some(true));
        assert_eq!(keep.rank, "env");
    }

    #[test]
    fn master_switch_off_vetoes_every_other_row() {
        // The master switch reads through the settings stack; a root whose
        // .fno config.toml carries autonomy.enabled=false vetoes every gated
        // row to rank "autonomy".
        let dir = std::env::temp_dir().join(format!("fno-aut-veto-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".fno")).unwrap();
        std::fs::write(
            dir.join(".fno").join("config.toml"),
            "[autonomy]\nenabled = false\n",
        )
        .unwrap();
        let rows = collect_status(Some(&dir));
        for name in [
            "post-merge ritual",
            "pr_watch (headless PR poll)",
            "groom (_spawn_groom_worker)",
            "evals runner",
            "lead loop",
        ] {
            let r = row_by_name(&rows, name);
            assert_eq!(r.armed, Some(false), "{name}");
            assert_eq!(r.rank, "autonomy", "{name}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ungated_cells_render_ungated() {
        let rows = vec![row("x", "t", "k", None, "ungated")];
        let rendered = format_table(&rows);
        assert!(rendered.contains("ungated"));
    }
}

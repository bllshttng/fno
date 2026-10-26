//! The per-harness surface report (doctor.py:3731-3847).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{call_binary_json, codex_home, command_call, resolve_binary, which, Probes, Report};

pub(crate) fn harness_surface_report() -> Value {
    let mut report = json!({});
    let oc = opencode_doctor_lines();
    if let Some(lines) = oc {
        report["opencode_doctor_lines"] = lines;
    }
    let codex_gate = which("codex").is_some() || codex_plugin_state_exists(&codex_home());
    if codex_gate {
        report["codex_plugin"] = super::codex::inspect_codex_plugin_freshness();
    }
    let dual = super::codex::codex_hooks_dual_and_foreign();
    if dual.0 {
        report["codex_hooks_dual"] = Value::Bool(true);
    }
    if !dual.1.is_empty() {
        report["codex_hooks_foreign_json"] = dual.1;
    }
    report["codex_app_server"] = super::codex_app_server_report();
    if which("codex").is_some() {
        let present = report["codex_app_server"]["present"] == Value::Bool(true);
        report["codex_context_window"] = super::codex_context_window_report(Some(present));
    }
    report
}

fn opencode_doctor_lines() -> Option<Value> {
    let oc_home = std::env::var_os("OPENCODE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir_x().join(".config/opencode"));
    if !oc_home.join("plugins").is_dir() {
        return None;
    }
    let (err, receipt) =
        call_binary_json("plugin-install", &["--status", "--json", "opencode"], 60);
    if err.is_some() {
        return None;
    }
    let lines = receipt
        .get("doctor_lines")
        .and_then(Value::as_array)?
        .clone();
    let keep = |line: &Value| -> Option<String> {
        line.get("text")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let filtered: Vec<Value> = lines.iter().filter_map(keep).map(Value::String).collect();
    Some(Value::Array(filtered))
}

fn home_dir_x() -> PathBuf {
    super::home_dir()
}

fn codex_plugin_state_exists(home: &std::path::Path) -> bool {
    home.join("footnote/plugin-channel.json").is_file()
        || home.join("footnote/rollback-failure.json").is_file()
        || home.join("plugins/cache/footnote").exists()
        || home.join("plugins/cache/footnote-dev").exists()
}

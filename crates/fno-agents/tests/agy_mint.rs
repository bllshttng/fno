//! The agy mint runtime's first contract test: a PATH fake `agy` prints the
//! stream-json init line then holds (the measured 1.3.3 shape); `run` must
//! answer the id and kill the holder. The env mutation is serialized because
//! integration tests in one file share a process.

use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn mint_reads_the_init_line_of_a_holder_and_answers_the_id() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agy");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"c5661b28-bcba-4690-8b2e-4a4a88541e8c\"}'\nsleep 30\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", dir.path().display()));
    let ask = serde_json::json!({
        "cwd": dir.path(),
        "model": "",
        "effort": "",
        "permission_mode": "",
        "yolo": false,
        "timeout_s": 10,
    });
    let answer = fno_agents::agy_mint::run(&ask);
    std::env::set_var("PATH", old_path);
    assert_eq!(
        answer.get("conversation_id").and_then(|v| v.as_str()),
        Some("c5661b28-bcba-4690-8b2e-4a4a88541e8c")
    );
}

//! The front-door join: the first `fno` verb a session runs joins it to fno.
//!
//! The joined path costs one stat of the marker that
//! `crates/fno-agents/src/join_marker.rs` writes at
//! `<agents home>/sessions/<harness>/<sid>/joined`; the layout is a wire
//! format between the two crates. An unjoined session shells
//! `fno-agents report --kind join` once, bounded and fail-open. Hook commands
//! carry `FNO_HOOK_CONTEXT=1`, so a hook's own fno call never joins.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Highest precedence first; mirrors `HARNESS_SESSION_MARKERS` in
/// crates/fno-agents/src/claims.rs.
const MARKERS: &[(&str, &str)] = &[
    ("CODEX_THREAD_ID", "codex"),
    ("CLAUDE_CODE_SESSION_ID", "claude"),
    ("CODEX_SESSION_ID", "codex"),
    ("GEMINI_SESSION_ID", "gemini"),
    ("OPENCODE_SESSION_ID", "opencode"),
];

const JOIN_BOUND: Duration = Duration::from_secs(2);

/// The session this call would join, or `None` when it must not join: a hook
/// call, a spawned worker, no ambient identity, two disagreeing codex ids, or
/// an id that is not one path segment.
pub fn join_candidate(env: &dyn Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    if set("FNO_HOOK_CONTEXT").is_some() || set("FNO_AGENT_SELF").is_some() {
        return None;
    }
    if let (Some(a), Some(b)) = (set("CODEX_THREAD_ID"), set("CODEX_SESSION_ID")) {
        if a != b {
            return None;
        }
    }
    let (harness, sid) = MARKERS
        .iter()
        .find_map(|(key, harness)| set(key).map(|sid| (*harness, sid)))?;
    let segment = !sid.contains(['/', '\\', '\0']) && sid != "." && sid != ".." && sid.len() <= 128;
    segment.then_some((harness, sid))
}

pub fn marker_path(harness: &str, sid: &str) -> PathBuf {
    let home = crate::agents_view::registry_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("sessions").join(harness).join(sid).join("joined")
}

/// Join this session when it has not joined yet. Never fails the verb.
pub fn join_if_needed() {
    let Some((harness, sid)) = join_candidate(&|k| std::env::var(k).ok()) else {
        return;
    };
    if marker_path(harness, &sid).exists() {
        return;
    }
    let spawned = std::process::Command::new(crate::digest_overlay::fno_agents_bin())
        .args([
            "report",
            "--kind",
            "join",
            "--harness",
            harness,
            "--session-id",
            &sid,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return;
    };
    let start = Instant::now();
    while start.elapsed() < JOIN_BOUND {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn front_door_join_candidate() {
        let claude = env_of(&[("CLAUDE_CODE_SESSION_ID", "abc")]);
        assert_eq!(join_candidate(&claude), Some(("claude", "abc".to_string())));
        // AC8: a hook's fno call never joins its session.
        let hook = env_of(&[("CLAUDE_CODE_SESSION_ID", "abc"), ("FNO_HOOK_CONTEXT", "1")]);
        assert_eq!(join_candidate(&hook), None);
        let worker = env_of(&[("CLAUDE_CODE_SESSION_ID", "abc"), ("FNO_AGENT_SELF", "w1")]);
        assert_eq!(join_candidate(&worker), None);
        let split = env_of(&[("CODEX_THREAD_ID", "a"), ("CODEX_SESSION_ID", "b")]);
        assert_eq!(join_candidate(&split), None);
        assert_eq!(join_candidate(&env_of(&[])), None);
        let walk = env_of(&[("CLAUDE_CODE_SESSION_ID", "../x")]);
        assert_eq!(join_candidate(&walk), None);
    }
}

//! Keys the supervisor strips from every child it launches.

const SUPERVISOR_KEYS: [&str; 3] = [
    "FNO_AGENTS_BIN",
    "FNO_AGENTS_DAEMON_BIN",
    "FNO_AGENTS_FRONT",
];

pub fn scrub(env: &mut Vec<(String, String)>) {
    env.retain(|(k, _)| !SUPERVISOR_KEYS.contains(&k.as_str()));
}

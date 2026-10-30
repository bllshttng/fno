//! The trace page's read: one bounded shell-out to the fold.

use std::time::Duration;

const TRACE_TIMEOUT: Duration = Duration::from_secs(30);

/// A node id that cannot reach the argv as a flag or a path.
pub(crate) fn valid_node(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The fold's JSON for one node, or the reason it was not read.
pub(crate) async fn read_trace(node: &str) -> Result<Vec<u8>, String> {
    let mut command =
        crate::process_admission::tokio_command(crate::digest_overlay::fno_agents_bin());
    command
        .args(["backlog", "get", node, "--trace", "--json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let output = match tokio::time::timeout(TRACE_TIMEOUT, fut).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err(format!("timed out after {}ms", TRACE_TIMEOUT.as_millis())),
    };
    if !output.status.success() {
        let said = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_default();
        return Err(if said.is_empty() {
            match output.status.code() {
                Some(code) => format!("backlog get --trace exited {code}"),
                None => "backlog get --trace was killed by a signal".to_string(),
            }
        } else {
            said.chars().take(200).collect()
        });
    }
    if serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err() {
        return Err("unreadable output".to_string());
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::valid_node;

    #[test]
    fn node_ids_never_reach_argv_as_flags() {
        assert!(valid_node("n-1"));
        assert!(valid_node("fno-a3f9"));
        assert!(!valid_node("--help"));
        assert!(!valid_node("-x"));
        assert!(!valid_node(""));
        assert!(!valid_node("a b"));
        assert!(!valid_node("a/../b"));
    }
}

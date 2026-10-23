//! Self-heal for a dead server's leftover session files. A SIGKILLed server
//! runs no guard, so its `.sock`, `.pid` and `.ver` stay behind and every
//! reader would otherwise report the corpse forever. Every reader enumerates
//! sessions through [`session_names`], so the heal runs there once and no
//! reader ever tells the user to kill a session that is already dead.

use super::*;

/// True only when nothing can be serving `sock`: the pid sidecar is absent
/// or names a dead or reused pid, no starter holds the startup marker, and
/// every connect is refused or finds no file. A live pid, an unparseable
/// sidecar, a timeout or a wire error all read false, so a reader stays
/// fail-closed on everything but a proven corpse. The pid read comes first:
/// it is a file read and a `kill(0)`, so a live session never pays a connect.
pub(super) fn proven_listenerless(sock: &Path) -> bool {
    let pid_dead = match std::fs::read_to_string(proto::pid_sidecar_path(sock)) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(raw) => match proto::parse_pid_sidecar(&raw) {
            None => false,
            Some((pid, start)) => proto::pid_confirmed_dead(pid) || identity_mismatch(pid, start),
        },
    };
    pid_dead
        && !proto::startup_sidecar_path(sock).exists()
        && proto::probe_status(sock) == proto::ProbeOutcome::Dead
}

/// Remove a proven-dead session's files and journal one
/// `mux_session_healed` row. True when the session is gone, so the caller
/// drops it from the listing.
pub(super) fn heal_if_dead(name: &str, sock: &Path) -> bool {
    if !proven_listenerless(sock) {
        return false;
    }
    if let Err(e) = proto::remove_session_files(sock) {
        eprintln!(
            "fno mux: warn: dead session {name:?} files remain at {}: {e}",
            sock.display()
        );
        return false;
    }
    eprintln!("fno mux: removed dead session {name:?} (connect refused, no live pid)");
    let row = serde_json::json!({
        "ts": crate::review_invocation::review_invocation_timestamp(),
        "type": "mux_session_healed",
        "source": "daemon",
        "data": { "mux_session": name, "path": sock.display().to_string() },
    });
    let path = crate::pane_send_audit::pane_send_audit_events_path();
    let _ = crate::pane_send_audit::append_agents_event(&path, &row);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_socket_with_no_live_pid_heals_and_nothing_else_does() {
        // A dead server's leftover socket: the file stays, every connect is
        // refused. Only a dead or absent pid proves no listener; a live pid,
        // a garbage sidecar or a startup marker leaves the files in place.
        let sock = std::env::temp_dir().join(format!("fno-pl-{}.sock", std::process::id()));
        let _ = proto::remove_session_files(&sock);
        let corpse = || drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        corpse();
        let pid_file = proto::pid_sidecar_path(&sock);

        std::fs::write(&pid_file, std::process::id().to_string()).unwrap();
        assert!(!heal_if_dead("t", &sock), "a live pid stays fail-closed");
        std::fs::write(&pid_file, "junk").unwrap();
        assert!(
            !heal_if_dead("t", &sock),
            "a garbage sidecar stays fail-closed"
        );
        std::fs::remove_file(&pid_file).unwrap();
        let marker = proto::startup_sidecar_path(&sock);
        std::fs::write(&marker, "x").unwrap();
        assert!(
            !heal_if_dead("t", &sock),
            "a starter mid-bind stays fail-closed"
        );
        std::fs::remove_file(&marker).unwrap();
        assert!(sock.exists(), "every refusal leaves the socket in place");

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();
        std::fs::write(&pid_file, dead_pid.to_string()).unwrap();
        assert!(heal_if_dead("t", &sock), "a dead pid heals");
        assert!(
            !sock.exists() && !pid_file.exists(),
            "socket and pid sidecar are gone"
        );

        corpse();
        assert!(heal_if_dead("t", &sock), "no pid sidecar heals");
        assert!(!sock.exists());
    }
}

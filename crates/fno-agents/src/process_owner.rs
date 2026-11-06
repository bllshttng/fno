//! Read the narrow owner lease carried by fno-launched sandbox processes.

use std::io::Read as _;
use std::path::{Path, PathBuf};

const MAX_OWNER_SIDECAR_BYTES: usize = 4096;
#[cfg(target_os = "linux")]
const MAX_PROCESS_ENVIRONMENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerLease {
    pub pid: u32,
    pub birth: u64,
    pub session: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerStatus {
    Alive,
    Dead,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerRead {
    Owner(OwnerLease),
    Ownerless,
    Unknown,
}

#[derive(serde::Deserialize)]
struct OwnerSidecar {
    server_pid: u32,
    server_birth: u64,
    owner_pid: u32,
    owner_birth: u64,
    owner_session: String,
}

pub fn owner_sidecar_path(socket: &Path) -> PathBuf {
    socket.with_extension("owner")
}

pub fn mux_server_socket(command: &str) -> Option<String> {
    let first_end = command.find(char::is_whitespace)?;
    let executable = &command[..first_end];
    if Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("fno")
    {
        return None;
    }
    let after_executable = command[first_end..].trim_start();
    let flag_end = after_executable.find(char::is_whitespace)?;
    if &after_executable[..flag_end] != "--server" {
        return None;
    }
    let raw_socket = after_executable[flag_end..].trim();
    let socket = raw_socket
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            raw_socket
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(raw_socket);
    (!socket.is_empty()).then(|| socket.to_string())
}

pub fn owner_lease_for_server(pid: u32, socket: &Path) -> OwnerRead {
    let path = owner_sidecar_path(socket);
    match std::fs::File::open(&path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            if file
                .take((MAX_OWNER_SIDECAR_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .is_err()
                || bytes.len() > MAX_OWNER_SIDECAR_BYTES
            {
                return OwnerRead::Unknown;
            }
            let Some(lease) = crate::daemon::process_start_time(pid)
                .and_then(|birth| lease_from_sidecar(&bytes, pid, birth))
            else {
                return OwnerRead::Unknown;
            };
            OwnerRead::Owner(lease)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(target_os = "linux")]
            {
                match process_environment(pid) {
                    Some(bytes) => match lease_from_environment(&bytes) {
                        Some(lease) => OwnerRead::Owner(lease),
                        None if bytes.windows(10).any(|window| window == b"FNO_OWNER_") => {
                            OwnerRead::Unknown
                        }
                        None => OwnerRead::Ownerless,
                    },
                    None => OwnerRead::Unknown,
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = error;
                OwnerRead::Ownerless
            }
        }
        Err(_) => OwnerRead::Unknown,
    }
}

pub fn lease_from_sidecar(
    bytes: &[u8],
    expected_server_pid: u32,
    expected_birth: u64,
) -> Option<OwnerLease> {
    let sidecar = serde_json::from_slice::<OwnerSidecar>(bytes).ok()?;
    if sidecar.server_pid != expected_server_pid
        || sidecar.server_birth != expected_birth
        || sidecar.owner_pid <= 1
        || sidecar.owner_birth == 0
        || !valid_owner_session(&sidecar.owner_session)
    {
        return None;
    }
    Some(OwnerLease {
        pid: sidecar.owner_pid,
        birth: sidecar.owner_birth,
        session: sidecar.owner_session,
    })
}

#[cfg(target_os = "linux")]
fn process_environment(pid: u32) -> Option<Vec<u8>> {
    let file = std::fs::File::open(format!("/proc/{pid}/environ")).ok()?;
    let mut bytes = Vec::new();
    file.take((MAX_PROCESS_ENVIRONMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_PROCESS_ENVIRONMENT_BYTES).then_some(bytes)
}

/// The environment of `pid` on macOS: the tail of `KERN_PROCARGS2` past argv.
/// Same-user processes read fine; a sysctl failure (SIP-shielded, gone) reads
/// `None`, and the caller treats an unreadable env as no lease evidence.
#[cfg(target_os = "macos")]
fn process_environment(pid: u32) -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size < 4
    {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    procargs2_env_region(&buf[..size])
}

/// The environment strings of a `KERN_PROCARGS2` buffer: an `i32` argc, the
/// exec path, NUL padding, argc argv strings, then the environment to the end
/// of the buffer. Mirrors `census::parse_procargs2`, which stops at argv.
#[cfg(target_os = "macos")]
fn procargs2_env_region(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 4 {
        return None;
    }
    let argc = i32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
    if argc == 0 {
        return None;
    }
    let mut i = 4;
    while i < buf.len() && buf[i] != 0 {
        i += 1;
    }
    if i >= buf.len() {
        return None;
    }
    while i < buf.len() && buf[i] == 0 {
        i += 1;
    }
    let mut consumed = 0;
    while consumed < argc && i < buf.len() {
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        i += 1;
        consumed += 1;
    }
    (consumed == argc).then(|| buf[i..].to_vec())
}

/// The full owner lease carried in `pid`'s own environment, `None` when the
/// env names no lease. The shape-agnostic orphan sweep's read leg: any tagged
/// process, any argv shape, both platforms.
pub fn owner_lease_for_pid(pid: u32) -> OwnerRead {
    let Some(bytes) = process_environment(pid) else {
        return OwnerRead::Ownerless;
    };
    match lease_from_environment(&bytes) {
        Some(lease) => OwnerRead::Owner(lease),
        None if bytes.windows(10).any(|window| window == b"FNO_OWNER_") => OwnerRead::Unknown,
        None => OwnerRead::Ownerless,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The macOS env read is a sysctl away, so the parser is the unit: a
    /// synthetic KERN_PROCARGS2 buffer must yield an env region the existing
    /// lease parser accepts, argv and exec path excluded.
    #[cfg(target_os = "macos")]
    #[test]
    fn procargs2_env_region_yields_the_lease_past_argv() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&2i32.to_ne_bytes());
        buf.extend_from_slice(b"/usr/local/bin/fno\0");
        buf.extend_from_slice(&[0, 0]); // alignment padding
        buf.extend_from_slice(b"fno\0-do\0"); // argv, argc = 2
        buf.extend_from_slice(
            b"PATH=/bin\0FNO_OWNER_PID=42\0FNO_OWNER_BIRTH=9001\0FNO_OWNER_SESSION=session-a\0",
        );
        let region = procargs2_env_region(&buf).expect("env region parses");
        let lease = lease_from_environment(&region).expect("lease parses");
        assert_eq!(lease.pid, 42);
        assert_eq!(lease.birth, 9001);
        assert_eq!(lease.session, "session-a");
        // A partial lease is visible, never kill authority.
        let mut partial = Vec::new();
        partial.extend_from_slice(&2i32.to_ne_bytes());
        partial.extend_from_slice(b"/bin/x\0\0x\0-y\0FNO_OWNER_PID=7\0");
        let region = procargs2_env_region(&partial).expect("env region parses");
        assert_eq!(
            owner_read_of_bytes(&region),
            OwnerRead::Unknown,
            "partial lease reads Unknown"
        );
        // No lease tokens at all reads Ownerless.
        let mut plain = Vec::new();
        plain.extend_from_slice(&1i32.to_ne_bytes());
        plain.extend_from_slice(b"/bin/sh\0\0sh\0-c\0");
        plain.extend_from_slice(b"PATH=/bin\0HOME=/tmp\0");
        let region = procargs2_env_region(&plain).expect("env region parses");
        assert_eq!(
            owner_read_of_bytes(&region),
            OwnerRead::Ownerless,
            "untagged env reads Ownerless"
        );
    }

    #[cfg(target_os = "macos")]
    fn owner_read_of_bytes(bytes: &[u8]) -> OwnerRead {
        match lease_from_environment(bytes) {
            Some(lease) => OwnerRead::Owner(lease),
            None if bytes.windows(10).any(|w| w == b"FNO_OWNER_") => OwnerRead::Unknown,
            None => OwnerRead::Ownerless,
        }
    }
}

pub fn lease_from_environment(bytes: &[u8]) -> Option<OwnerLease> {
    let mut pid = None;
    let mut birth = None;
    let mut session = None;
    for item in bytes.split(|byte| *byte == 0 || byte.is_ascii_whitespace()) {
        let Ok(item) = std::str::from_utf8(item) else {
            continue;
        };
        if let Some(value) = item.strip_prefix("FNO_OWNER_PID=") {
            pid = value.parse::<u32>().ok().filter(|value| *value > 1);
        } else if let Some(value) = item.strip_prefix("FNO_OWNER_BIRTH=") {
            birth = value.parse::<u64>().ok().filter(|value| *value > 0);
        } else if let Some(value) = item.strip_prefix("FNO_OWNER_SESSION=") {
            if valid_owner_session(value) {
                session = Some(value.to_string());
            }
        }
    }
    Some(OwnerLease {
        pid: pid?,
        birth: birth?,
        session: session?,
    })
}

fn valid_owner_session(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 128
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub fn owner_status(lease: &OwnerLease) -> OwnerStatus {
    match crate::daemon::process_start_time(lease.pid) {
        Some(birth) if birth == lease.birth => OwnerStatus::Alive,
        Some(_) => OwnerStatus::Dead,
        None => {
            let result = unsafe { libc::kill(lease.pid as libc::pid_t, 0) };
            if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
                OwnerStatus::Unknown
            } else {
                OwnerStatus::Dead
            }
        }
    }
}

/// The owner-lease env entries for a live owner pid and session name, `None`
/// when the owner's birth is unreadable or the session name is not a valid
/// owner session. Writers stamp this on every fleet worker child, so the
/// shape-agnostic orphan sweep can name and reap a process whose owner died.
/// Clears any inherited lease first: the stamp names THIS owner, never an
/// ancestor's.
pub fn owner_lease_env(owner_pid: u32, session: &str) -> Option<Vec<(String, String)>> {
    if !valid_owner_session(session) {
        return None;
    }
    let birth = crate::daemon::process_start_time(owner_pid)?;
    Some(vec![
        ("FNO_OWNER_PID".to_string(), owner_pid.to_string()),
        ("FNO_OWNER_BIRTH".to_string(), birth.to_string()),
        ("FNO_OWNER_SESSION".to_string(), session.to_string()),
    ])
}

/// Apply [`owner_lease_env`] to a std `Command`, removing any inherited
/// FNO_OWNER_* keys first.
pub fn stamp_command_owner_lease(cmd: &mut std::process::Command, session: &str) {
    for key in ["FNO_OWNER_PID", "FNO_OWNER_BIRTH", "FNO_OWNER_SESSION"] {
        cmd.env_remove(key);
    }
    if let Some(env) = owner_lease_env(std::process::id(), session) {
        for (key, value) in env {
            cmd.env(key, value);
        }
    }
}

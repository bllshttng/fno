//! Arg parsers for the `logs` and `trace` verbs. Extracted beside
//! `resume_args` for the same reason: client_verbs is over the line budget
//! and shrink-only, and a flag's grammar belongs beside the parser that
//! produced it.
use crate::client_verbs::{echo_extra, expand_eq};

/// Parsed `trace` flags.
pub(crate) struct TraceArgs {
    pub(crate) name: Option<String>,
    pub(crate) request_id: Option<String>,
    pub(crate) all_agents: bool,
    pub(crate) json_out: bool,
    pub(crate) limit: i64,
    pub(crate) since: Option<String>,
    pub(crate) cross_project: bool,
}

pub(crate) fn parse_trace_args(rest: &[String]) -> Result<TraceArgs, String> {
    let mut a = TraceArgs {
        name: None,
        request_id: None,
        all_agents: false,
        json_out: false,
        limit: 200,
        since: None,
        // The store heal's project-confinement refusal prescribes
        // --cross-project; accept it so the taught remedy is a form this
        // verb takes.
        cross_project: false,
    };
    let rest = expand_eq(rest);
    let mut it = rest.iter().cloned().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            // -A/-J are the global-register shorts; mirror the
            // Python typer.Option aliases so Rust-routed `trace` honors them.
            "--all" | "-A" => a.all_agents = true,
            "--json" | "-J" => a.json_out = true,
            "--cross-project" => a.cross_project = true,
            "--request-id" => {
                a.request_id = Some(it.next().ok_or("--request-id needs a value")?);
            }
            "--since" => {
                a.since = Some(it.next().ok_or("--since needs a value")?);
            }
            "--limit" => {
                let v = it.next().ok_or("--limit needs a value")?;
                a.limit = v
                    .parse::<i64>()
                    .map_err(|_| format!("--limit needs an integer (got: {v})"))?;
            }
            other if other.starts_with("--") => {
                return Err(format!("fno-agents: unknown trace flag: {other}"));
            }
            positional => {
                if a.name.is_some() {
                    return Err(format!(
                        "fno-agents: trace takes one NAME (got extra: {}).",
                        echo_extra(positional)
                    ));
                }
                a.name = Some(positional.to_string());
            }
        }
    }
    Ok(a)
}

/// Parsed `logs` flags.
pub(crate) struct LogsArgs {
    pub(crate) name: String,
    pub(crate) tail: i64,
    pub(crate) follow: bool,
    pub(crate) json_out: bool,
    pub(crate) cross_project: bool,
}

pub(crate) fn parse_logs_args(rest: &[String]) -> Result<LogsArgs, (i32, String)> {
    let mut name: Option<String> = None;
    let mut tail: i64 = 100;
    let mut follow = false;
    let mut json_out = false;
    // The store heal's project-confinement refusal prescribes --cross-project;
    // accept it so the taught remedy is a form this verb takes.
    let mut cross_project = false;
    let rest = expand_eq(rest);
    let mut it = rest.iter().cloned().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--follow" | "-f" => follow = true,
            "--json" | "-J" => json_out = true, // global-register short
            "--cross-project" => cross_project = true,
            "--tail" | "-n" => {
                let v = it.next().ok_or((2, "--tail needs a value".to_string()))?;
                tail = v
                    .parse::<i64>()
                    .map_err(|_| (2, format!("--tail needs an integer (got: {v})")))?;
            }
            // Attached short-option form `-n5` (Click/Typer accept it; codex P2).
            s if s.starts_with("-n") && s.len() > 2 => {
                let v = &s[2..];
                tail = v
                    .parse::<i64>()
                    .map_err(|_| (2, format!("--tail needs an integer (got: {v})")))?;
            }
            other
                if other.starts_with('-')
                    && other.len() > 1
                    && !other[1..].chars().next().unwrap().is_ascii_digit() =>
            {
                return Err((2, format!("fno-agents: unknown logs flag: {other}")));
            }
            positional => {
                if name.is_some() {
                    return Err((
                        2,
                        format!(
                            "fno-agents: logs takes one NAME (got extra: {}).",
                            echo_extra(positional)
                        ),
                    ));
                }
                name = Some(positional.to_string());
            }
        }
    }
    let name = name.ok_or((2, "logs needs a <name>".to_string()))?;
    // cmd_logs: `--tail must be >= 0`.
    if tail < 0 {
        return Err((2, format!("--tail must be >= 0 (got {tail})")));
    }
    Ok(LogsArgs {
        name,
        tail,
        follow,
        json_out,
        cross_project,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_rows() {
        let a = parse_logs_args(&["worker-A".to_string()]).unwrap();
        assert_eq!(a.name, "worker-A");
        assert_eq!(a.tail, 100);
        assert!(!a.follow);
        let a = parse_logs_args(&[
            "w".to_string(),
            "-n".to_string(),
            "5".to_string(),
            "-f".to_string(),
        ])
        .unwrap();
        assert_eq!(a.tail, 5);
        assert!(a.follow);
        // Attached short form `-n5` (codex P2) and the `--tail=N` equals form.
        assert_eq!(
            parse_logs_args(&["w".to_string(), "-n5".to_string()])
                .unwrap()
                .tail,
            5
        );
        assert_eq!(
            parse_logs_args(&["w".to_string(), "--tail=7".to_string()])
                .unwrap()
                .tail,
            7
        );
        let err = parse_logs_args(&["w".to_string(), "--tail".to_string(), "-3".to_string()]);
        assert!(matches!(err, Err((2, _))));

        // (codex P2, PR #431): -J must parse like --json on the
        // Rust-routed `logs` path, not fall through to "unknown flag".
        let a = parse_logs_args(&["w".to_string(), "-J".to_string()]).unwrap();
        assert!(a.json_out);

        // The store heal's project-confinement refusal prescribes
        // --cross-project on logs too; the old surface died with "unknown
        // logs flag" before any resolution ran.
        let a = parse_logs_args(&["w".to_string(), "--cross-project".to_string()]).unwrap();
        assert!(a.cross_project);
        let plain = parse_logs_args(&["w".to_string()]).unwrap();
        assert!(!plain.cross_project);

        // (codex P2, PR #431): -A/-J must parse identically to
        // --all/--json on the Rust-routed `trace` path.
        let short = parse_trace_args(&["-A".to_string(), "-J".to_string()]).unwrap();
        let long = parse_trace_args(&["--all".to_string(), "--json".to_string()]).unwrap();
        assert!(short.all_agents && short.json_out);
        assert_eq!(short.all_agents, long.all_agents);
        assert_eq!(short.json_out, long.json_out);

        // The refusal prescribes --cross-project on trace as well;
        // the flag rides to the scoped resolver with the resolution, not to
        // the daemon (trace is client-local).
        let a = parse_trace_args(&["w".to_string(), "--cross-project".to_string()]).unwrap();
        assert!(a.cross_project);
        let plain = parse_trace_args(&["w".to_string()]).unwrap();
        assert!(!plain.cross_project);
    }
}

//! `--help` for the seven operator leaves, and the refusal a removed claim
//! leaf answers with. The front door execs `fno agents claim` straight into
//! this binary, so both answers live here.

const TABLE: &str = include_str!("../tombstones.toml");

const ACQUIRE: &str = "\
Usage: fno agents claim acquire [OPTIONS] [KEY]

  Acquire a claim on KEY for HOLDER. Idempotent re-acquire if HOLDER matches.
  With --lane <id> --max-lanes N it acquires a lane slot instead.

Arguments:
  KEY                  Lock key, e.g. node:ab-1234abcd. Omit with --lane.

Options:
  --holder TEXT        Symbolic owner string.
  --lane TEXT          Acquire a lane slot for this lane id. Needs --max-lanes;
                       takes no KEY and no --holder.
  --max-lanes INTEGER  With --lane: the concurrency cap (>=1; 1 is sequential).
  -R, --reason TEXT    Optional rationale recorded in audit.
  --ttl TEXT           TTL like 30m, 1h, 3600s (omit for PID-liveness).
  --metadata TEXT      JSON object passed verbatim. [default: {}]
  --pid INTEGER        PID-liveness anchor (omit for the nearest agent session,
                       else this process). Pin a long-lived owner.
  --pid-unavailable    Record explicit PID absence. Needs --ttl.
  -J, --json           Emit JSON to stdout.
  --verbose            More detail on stderr.
  --harness TEXT       Pin the owning harness instead of reading ambient
                       markers. The init hook passes the proven harness.
  --handover-from TEXT Take over a claim held by this exact prior holder. A
                       normal acquire when no such claim is on disk.
  --help               Show this message and exit.
";

const RELEASE: &str = "\
Usage: fno agents claim release [OPTIONS] [KEY]

  Release a claim we own. Silent success if already released. --lane <id>
  releases a lane slot; --force drops a claim regardless of owner.

Arguments:
  KEY                  Claim key. Omit with --lane.

Options:
  --holder TEXT
  --lane TEXT          Release the lane slot this lane id holds. Takes no KEY.
  -F, --force          Drop the claim regardless of owner, archived to
                       .expired/. Needs --reason; takes no --holder.
  -R, --reason TEXT    With --force: the required audit rationale.
  --strict             Fail if the holder does not match.
  --stamp-do           Stamp an execute provenance row for this claim's window.
                       A released node: claim stamps by default.
  --rollback-do        Remove the open execute row this claim's acquire opened.
                       Mutually exclusive with --stamp-do.
  -J, --json
  --help               Show this message and exit.
";

const REFRESH: &str = "\
Usage: fno agents claim refresh [OPTIONS] KEY

  Extend a TTL claim's expires_at. No-op for PID-liveness claims.

Arguments:
  KEY                  [required]

Options:
  --holder TEXT        [required]
  --ttl TEXT
  -J, --json
  --help               Show this message and exit.
";

const STATUS: &str = "\
Usage: fno agents claim status [OPTIONS] KEY

  Inspect a single claim. On a node: key nobody holds, the fleet roster is
  read before the answer, so free never hides a live worker. JSON callers
  read state; the roster fields are additive.

Arguments:
  KEY                  [required]

Options:
  -J, --json
  --roster / --no-roster
                       Read the fleet roster before calling a node: key free.
                       [default: roster] --no-roster is for a hot path that
                       reads only the claim record.
  --help               Show this message and exit.
";

const LIST: &str = "\
Usage: fno agents claim list [OPTIONS]

  Enumerate claims. Both claims roots (global ~/.fno/claims and the cwd-local
  root) are read and merged; only an explicit --root narrows.

Options:
  --prefix TEXT        Filter keys starting with this prefix.
  --include-stale
  -J, --json
  --root PATH          Explicit claims root (repo root).
  --silent-after TEXT  Report LIVE claims silent this long ('15m').
  --help               Show this message and exit.
";

const REAP: &str = "\
Usage: fno agents claim reap [OPTIONS]

  Archive every provably-dead claim, naming the instrument that proved it.
  Dry-run is the default; --apply archives and verifies every move.

Options:
  --apply              Archive dead claims.
  --root PATH          Repeatable. Overrides the both-roots sweep.
  -J, --json
  --help               Show this message and exit.
";

const SESSION_PID: &str = "\
Usage: fno agents claim session-pid [OPTIONS]

  Resolve the durable session pid: the nearest harness ancestor that is not
  Claude Code pool machinery. Prints nothing when uncapturable. Always exit 0.

Options:
  --from-pid INTEGER   Start the ancestor walk here (default: the parent).
  -J, --json
  --help               Show this message and exit.
";

fn page(op: &str) -> Option<&'static str> {
    Some(match op {
        "acquire" => ACQUIRE,
        "release" => RELEASE,
        "refresh" => REFRESH,
        "status" => STATUS,
        "list" => LIST,
        "reap" => REAP,
        "session-pid" => SESSION_PID,
        _ => return None,
    })
}

/// The answer for `claim <op> --help`, or a removed leaf's refusal. None
/// when the op runs.
pub fn answer(op: &str, rest: &[String]) -> Option<i32> {
    if let Some(text) = page(op) {
        let asks = rest
            .iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a == "--help");
        if !asks {
            return None;
        }
        print!("{text}");
        return Some(0);
    }
    let table: toml::Table = TABLE.parse().unwrap_or_default();
    let removed = format!("claim {op}");
    let replacement = table.get(&removed)?.as_str()?;
    // Lane doors share the `claim lane-` names with retired leaves and
    // still run.
    if op.starts_with("lane-") {
        return None;
    }
    eprintln!("Error: `fno {removed}` was removed. Use {replacement}.");
    Some(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_leaf_answers_help_and_a_removed_leaf_names_its_replacement() {
        for op in [
            "acquire",
            "release",
            "refresh",
            "status",
            "list",
            "reap",
            "session-pid",
        ] {
            assert_eq!(answer(op, &argv(&["--help"])), Some(0), "{op}");
            assert_eq!(answer(op, &argv(&["node:x"])), None, "{op}");
        }
        assert_eq!(answer("status", &argv(&["--", "--help"])), None);
        assert_eq!(answer("force-release", &argv(&["node:x"])), Some(2));
        assert_eq!(answer("lane-acquire", &argv(&[])), None);
        assert_eq!(answer("sweep", &argv(&[])), None);
    }
}

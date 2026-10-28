//! The fleet-law seed block: the machine's active freeze/hold constraints,
//! rendered once and appended to a spawn's delivered payload. A worker seated
//! while a fleet stop is armed reads the line in its seed. Law-stage keyword
//! matching cannot be that selector: the live law list carries lifts
//! (`merge-freeze:<repo>@<pin>`: "lift") and PR-scoped holds that read as
//! constraints to it. What makes a constraint active is the STATE record, and
//! the records are plain local files (machine breaker, targeted holds, merge
//! freeze) read from the agents home - no daemon, no lock, no network.

use serde_json::json;

use crate::fleet_incident::{self, IncidentRecord, Verdict};
use crate::paths::AgentsHome;

pub const MARKER: &str = "<fno_fleet_laws>";
pub const END_MARKER: &str = "</fno_fleet_laws>";

const HEADER: &str =
    "ACTIVE FLEET LAWS - these bind this worker now; doing the barred work is a breach:";

/// One rendered block plus the subjects it names, for the spawn receipt.
pub struct Delivered {
    pub block: String,
    pub subjects: Vec<String>,
}

/// The active fleet constraints of one home, or None when the machine is
/// clear (or resolves no home: a test that declared no state root has no
/// incident to observe, the same degrade `fleet_incident::verdict` takes).
/// `repo` is the spawn cwd's `owner/repo` slug; a None slug sees only a
/// version-1 freeze record, which freezes every repo. `subject` is the
/// worker being seated: a targeted hold binds the seed only when it names
/// this subject (its territory, its node's territory membership, or its
/// session ids - empty for an unborn worker), the same match
/// `fleet_incident::verdict_for_subject` runs for the loop.
pub fn active_laws_for(
    home: &AgentsHome,
    subject: &fleet_incident::Subject<'_>,
    repo: Option<&str>,
) -> Option<Delivered> {
    let mut lines: Vec<String> = Vec::new();
    machine_wide_lines(home, &mut lines);
    targeted_lines(home, subject, &mut lines);
    if let Some(line) = crate::merge_freeze::active_freeze_line(home, repo) {
        lines.push(line);
    }
    if lines.is_empty() {
        return None;
    }
    let mut block = String::from(MARKER);
    block.push('\n');
    block.push_str(HEADER);
    for line in &lines {
        block.push('\n');
        block.push_str(line);
    }
    block.push('\n');
    block.push_str(END_MARKER);
    let subjects = lines
        .iter()
        .map(
            |line| match line.strip_prefix("- ").unwrap_or(line).split_once("): ") {
                Some((head, _)) => format!("{head})"),
                None => line.strip_prefix("- ").unwrap_or(line).to_string(),
            },
        )
        .collect();
    Some(Delivered { block, subjects })
}

/// Append the block to a spawn payload for a worker seated at `cwd`: brevity
/// guidance first (the caller's `enrich_spawn_payload`), the laws last,
/// nearest the worker's eye. Empty messages stay empty and an already-marked
/// payload is never double-marked. The subjects name what was appended, for
/// the receipt; an empty vec means nothing was.
pub fn append_fleet_laws_at(message: &str, cwd: &std::path::Path) -> (String, Vec<String>) {
    append_fleet_laws_with(message, active_laws_at(cwd).as_ref())
}

/// The active fleet constraints of this machine, read for a worker seated at
/// `cwd` (its git remote names the repo; its node and territory are unknown
/// at the payload seam, so territory targets match only by explicit scope).
pub fn active_laws_at(cwd: &std::path::Path) -> Option<Delivered> {
    let home = AgentsHome::from_env_opt()?;
    let repo = crate::finalize::slug_from_git_remote(cwd);
    let subject = fleet_incident::Subject {
        session_ids: Vec::new(),
        node: None,
        territory: None,
        cwd,
    };
    active_laws_for(&home, &subject, repo.as_deref())
}

/// `append_fleet_laws_at` over injected laws (tests; the cwd wrapper above
/// is the production entry).
pub fn append_fleet_laws_with(
    message: &str,
    delivered: Option<&Delivered>,
) -> (String, Vec<String>) {
    match (message.is_empty(), message.contains(MARKER), delivered) {
        (false, false, Some(delivered)) => {
            let payload = format!("{message}\n\n{}", delivered.block);
            (payload, delivered.subjects.clone())
        }
        _ => (message.to_owned(), Vec::new()),
    }
}

/// `fleet-law-payload`: the payload-side render for the Python compat shell,
/// the `command_call` twin of `task-context-payload`. `block: null` means no
/// active constraint - absent, never fabricated.
pub fn run_payload(args: &[String]) -> i32 {
    crate::task_context::run_stdin_command(
        args,
        "fleet-law-payload  (one JSON request on stdin: cwd? - the seated worker's dir)",
        |req| {
            let cwd = req
                .get("cwd")
                .and_then(Value::as_str)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let delivered = active_laws_at(&cwd);
            let answer = json!({
                "ok": true,
                "block": delivered.as_ref().map(|d| d.block.clone()),
                "subjects": delivered.map(|d| d.subjects),
            });
            println!("{answer}");
        },
    )
}

fn machine_wide_lines(home: &AgentsHome, lines: &mut Vec<String>) {
    let verdict = fleet_incident::read_at(&fleet_incident::fleet_stop_path(home));
    let record = match &verdict {
        Verdict::Stopped(record) => record.clone(),
        Verdict::Unavailable(detail) => {
            // Fail closed in prose: a hold may be armed behind the unreadable
            // record, so the seed says so instead of rendering nothing.
            lines.push(format!(
                "- fleet-stop (machine-wide): the breaker record is unreadable ({detail}); treat every hold as armed"
            ));
            return;
        }
        Verdict::Clear(_) => return,
    };
    incident_line("fleet-stop (machine-wide)", &record, lines);
}

fn targeted_lines(
    home: &AgentsHome,
    subject: &fleet_incident::Subject<'_>,
    lines: &mut Vec<String>,
) {
    for (path, verdict) in fleet_incident::targeted_records(home) {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let filename_target = fleet_incident::target_from_path(&path);
        let record = match &verdict {
            Verdict::Stopped(record) => record.clone(),
            Verdict::Unavailable(detail) => {
                // A hold may sit behind the unreadable record and name this
                // subject, so the fail-closed line renders only when the
                // target itself matches; another subject's hold stays theirs.
                if filename_target
                    .as_deref()
                    .is_some_and(|t| fleet_incident::target_matches(t, subject).unwrap_or(false))
                {
                    lines.push(format!(
                        "- fleet-stop:{name} (hold): the record is unreadable ({detail}); treat the hold as armed"
                    ));
                }
                continue;
            }
            Verdict::Clear(_) => continue,
        };
        let target = record
            .target
            .clone()
            .or(filename_target)
            .unwrap_or_else(|| "unknown".into());
        // The hold binds this seed only when it names this subject; another
        // session's or territory's hold never rides a foreign worker's seed.
        if !fleet_incident::target_matches(&target, subject).unwrap_or(false) {
            continue;
        }
        incident_line(&format!("fleet-stop:{target} (hold)"), &record, lines);
    }
}

/// One `- <subject>)[: <reason>]` line. `subject` carries its label with an
/// OPEN paren (`fleet-stop (machine-wide`), so the scope clause closes the
/// label's paren and the line reads `- fleet-stop (machine-wide, scope:
/// tests): <reason>`.
fn incident_line(subject: &str, record: &IncidentRecord, lines: &mut Vec<String>) {
    let scopes = record.held_scopes().join(", ");
    let head = if scopes.is_empty() {
        format!("{subject})")
    } else {
        format!("{subject}, scope: {scopes})")
    };
    let reason = crate::law_match::first_sentence(&record.reason);
    if reason.is_empty() {
        lines.push(format!("- {head}"));
    } else {
        lines.push(format!(
            "- {head}: {}",
            crate::law_match::one_line(reason, 160)
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{active_laws_for, append_fleet_laws_with, Delivered, END_MARKER, MARKER};
    use crate::fleet_incident::{self, IncidentRecord};
    use crate::paths::AgentsHome;

    fn stopped(scopes: &[&str], reason: &str, target: Option<&str>) -> IncidentRecord {
        IncidentRecord {
            version: fleet_incident::STATE_VERSION,
            state: "stopped".to_string(),
            generation: 1,
            changed_at: "2026-10-28T00:00:00Z".to_string(),
            changed_by: "gavin".to_string(),
            reason: reason.to_string(),
            holds: scopes.iter().map(|s| s.to_string()).collect(),
            source: Some("file".to_string()),
            target: target.map(str::to_string),
            expires_at: None,
            origin: None,
            mail: None,
            mail_session_id: None,
        }
    }

    fn write_record(home: &AgentsHome, name: &str, record: &IncidentRecord) {
        let path = if name == "machine" {
            fleet_incident::fleet_stop_path(home)
        } else {
            fleet_incident::targets_dir(home).join(format!("{name}.json"))
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(record).unwrap()).unwrap();
    }

    fn laws(home: &AgentsHome) -> Option<Delivered> {
        let subject = crate::fleet_incident::Subject {
            session_ids: Vec::new(),
            node: None,
            territory: None,
            cwd: std::path::Path::new("/tmp/x"),
        };
        active_laws_for(home, &subject, Some("o/r"))
    }

    #[test]
    fn armed_machine_wide_hold_renders_the_line() {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let home = AgentsHome::at(dir.path());
        write_record(
            &home,
            "machine",
            &stopped(
                &["tests"],
                "machine-wide all-stop armed by the operator.",
                None,
            ),
        );
        let delivered = laws(&home).expect("an armed hold renders a block");
        assert!(delivered.block.contains(MARKER) && delivered.block.contains(END_MARKER));
        let line = delivered
            .block
            .lines()
            .find(|l| l.starts_with("- fleet-stop"))
            .expect("the hold line");
        assert!(line.contains("machine-wide, scope: tests"));
        assert!(line.contains("machine-wide all-stop armed by the operator."));
        assert_eq!(
            delivered.subjects,
            vec!["fleet-stop (machine-wide, scope: tests)".to_string()]
        );
    }

    #[test]
    fn clear_machine_renders_no_block() {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let home = AgentsHome::at(dir.path());
        assert!(laws(&home).is_none());
        let (payload, subjects) = append_fleet_laws_with("do the work", None);
        assert_eq!(payload, "do the work");
        assert!(subjects.is_empty());
    }

    #[test]
    fn targeted_hold_binds_only_its_named_subject() {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let home = AgentsHome::at(dir.path());
        write_record(
            &home,
            "territory-fnh",
            &stopped(&["builds"], "territory build stop.", Some("territory:fnh")),
        );
        let subject_in = crate::fleet_incident::Subject {
            session_ids: Vec::new(),
            node: None,
            territory: Some("fnh".to_string()),
            cwd: std::path::Path::new("/tmp/x"),
        };
        let subject_out = crate::fleet_incident::Subject {
            session_ids: Vec::new(),
            node: None,
            territory: Some("elsewhere".to_string()),
            cwd: std::path::Path::new("/tmp/x"),
        };
        let delivered =
            active_laws_for(&home, &subject_in, Some("o/r")).expect("the named seat renders");
        let line = delivered
            .block
            .lines()
            .find(|l| l.starts_with("- fleet-stop:territory:fnh"))
            .expect("the targeted line");
        assert!(line.contains("hold, scope: builds"));
        assert!(line.contains("territory build stop."));
        assert!(active_laws_for(&home, &subject_out, Some("o/r")).is_none());
    }

    #[test]
    fn append_is_idempotent_and_keeps_empty_messages_empty() {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let home = AgentsHome::at(dir.path());
        write_record(&home, "machine", &stopped(&["tests"], "stop.", None));
        let delivered = laws(&home).expect("the armed home renders");
        let (payload, subjects) = append_fleet_laws_with("do the work", Some(&delivered));
        assert!(payload.contains(MARKER));
        assert_eq!(subjects.len(), 1);
        let (again, subjects_again) = append_fleet_laws_with(&payload, Some(&delivered));
        assert_eq!(again.matches(MARKER).count(), 1);
        assert!(subjects_again.is_empty());
        let (empty, empty_subjects) = append_fleet_laws_with("", Some(&delivered));
        assert_eq!(empty, "");
        assert!(empty_subjects.is_empty());
    }

    #[test]
    fn unreadable_breaker_fails_closed_in_prose() {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let home = AgentsHome::at(dir.path());
        std::fs::create_dir_all(fleet_incident::fleet_stop_path(&home).parent().unwrap()).unwrap();
        std::fs::write(fleet_incident::fleet_stop_path(&home), "not json").unwrap();
        let delivered = laws(&home).expect("an unreadable breaker still warns");
        assert!(delivered.block.contains("unreadable"));
        assert!(delivered.block.contains("treat every hold as armed"));
    }

    #[test]
    fn markers_match_the_shared_cross_language_fixture() {
        const FIXTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/fleet-law.json"
        ));
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("valid fixture");

        assert_eq!(fixture["marker"], MARKER);
        assert_eq!(fixture["end_marker"], END_MARKER);
        // The Python compat shell hardcodes the same marker (its gate for the
        // command_call render); the fixture pins the cross-language spell.
    }
}

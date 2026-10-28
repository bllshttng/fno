//! The fleet-law seed block: the machine's active freeze/hold constraints,
//! rendered once and appended to a spawn's delivered payload. A worker seated
//! while a fleet stop is armed reads the line in its seed, so an unseeded
//! worker cannot breach by not knowing (the measured specimen: nine workers
//! seated with no freeze line in any seed ran local cargo into load 418).
//!
//! The selector reads machine STATE records, never law rows: the live law
//! list carries lifts (`merge-freeze:<repo>@<pin>`: "lift") and PR-scoped
//! holds that a keyword matcher cannot tell from fleet-wide constraints.
//! What makes a constraint active is the record, and the records live in
//! three plain local files: the machine breaker (`fleet-stop.json`), the
//! targeted holds (`fleet-stop.d/`), and the merge freeze
//! (`merge-freeze.json`, lift-checked against the decision index). All
//! reads are local files resolved from the agents home - no daemon, no lock,
//! no network - so the spawn path never waits on background work.

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
/// version-1 freeze record, which freezes every repo.
pub fn active_laws_for(home: &AgentsHome, repo: Option<&str>) -> Option<Delivered> {
    let mut lines: Vec<String> = Vec::new();
    machine_wide_lines(home, &mut lines);
    targeted_lines(home, &mut lines);
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

/// The active fleet constraints of this machine, from the resolved home and
/// the spawn cwd's git remote.
pub fn active_laws() -> Option<Delivered> {
    let home = AgentsHome::from_env_opt()?;
    let repo = crate::finalize::slug_from_git_remote(&std::env::current_dir().unwrap_or_default());
    active_laws_for(&home, repo.as_deref())
}

/// Append the block to a spawn payload: brevity guidance first (the caller's
/// `enrich_spawn_payload`), the laws last, nearest the worker's eye. Empty
/// messages stay empty and an already-marked payload is never double-marked.
/// The subjects name what was appended, for the receipt; an empty vec means
/// nothing was.
pub fn append_fleet_laws(message: &str) -> (String, Vec<String>) {
    append_fleet_laws_with(message, active_laws().as_ref())
}

/// `append_fleet_laws` over injected laws (tests; the env wrapper above is
/// the production entry).
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
        "fleet-law-payload  (one JSON request on stdin; the fields are the machine's own state)",
        |_req| {
            let delivered = active_laws();
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

fn targeted_lines(home: &AgentsHome, lines: &mut Vec<String>) {
    for (path, verdict) in fleet_incident::targeted_records(home) {
        let record = match &verdict {
            Verdict::Stopped(record) => record.clone(),
            Verdict::Unavailable(detail) => {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                lines.push(format!(
                    "- fleet-stop:{name} (hold): the record is unreadable ({detail}); treat the hold as armed"
                ));
                continue;
            }
            Verdict::Clear(_) => continue,
        };
        let target = record
            .target
            .clone()
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());
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
        active_laws_for(home, Some("o/r"))
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
    fn targeted_hold_names_its_target() {
        let dir = tempfile::TempDir::new_in(std::env::temp_dir()).unwrap();
        let home = AgentsHome::at(dir.path());
        write_record(
            &home,
            "t-mbp3",
            &stopped(&["builds"], "territory build stop.", Some("mbp3")),
        );
        let delivered = laws(&home).expect("a targeted hold renders a block");
        let line = delivered
            .block
            .lines()
            .find(|l| l.starts_with("- fleet-stop:mbp3"))
            .expect("the targeted line");
        assert!(line.contains("hold, scope: builds"));
        assert!(line.contains("territory build stop."));
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

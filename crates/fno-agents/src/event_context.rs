use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

fn model_samples(agent: &str, lines: impl Iterator<Item = String>) -> (Option<String>, usize) {
    let mut last = None;
    let mut samples = 0;
    for line in lines {
        let Some(model) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|record| crate::session_truth::model_of(agent, &record))
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        last = Some(model);
        samples += 1;
    }
    (last, samples)
}

fn read_error(error: &std::io::Error, path: &Path) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let detail = error.to_string();
            let suffix = format!(" (os error {code})");
            let detail = detail.strip_suffix(&suffix).unwrap_or(&detail);
            format!(
                "[Errno {code}] {detail}: {}",
                crate::client_commands::py_repr_str(&path.to_string_lossy())
            )
        }
        None => error.to_string(),
    }
}

fn observed_model(agent: &str, path: Option<&Path>) -> Value {
    if agent != "claude" && agent != "codex" {
        return json!({"kind":"not-file-backed"});
    }
    let Some(path) = path else {
        return json!({"kind":"no-transcript"});
    };
    let read_tail = || -> std::io::Result<(Vec<u8>, bool)> {
        let mut file = std::fs::File::open(path)?;
        let size = file.seek(SeekFrom::End(0))?;
        let start = size.saturating_sub(256 * 1024);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok((bytes, start > 0))
    };
    let (bytes, windowed) = match read_tail() {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return json!({"kind":"no-transcript"})
        }
        Err(error) => return json!({"kind":"unreadable", "reason": read_error(&error, path)}),
    };
    if bytes.is_empty() {
        return json!({"kind":"no-model-yet"});
    }
    if !bytes.ends_with(b"\n") {
        return json!({"kind":"unreadable", "reason":"torn final line (mid-write)"});
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.split('\n');
    if windowed {
        lines.next();
    }
    let (mut last, mut samples) = model_samples(agent, lines.map(str::to_string));
    if last.is_none() && windowed {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) => return json!({"kind":"unreadable", "reason": read_error(&error, path)}),
        };
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Some(model) = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|record| crate::session_truth::model_of(agent, &record))
                        .filter(|model| !model.is_empty())
                    {
                        last = Some(model);
                        samples += 1;
                    }
                }
                Err(error) => {
                    return json!({"kind":"unreadable", "reason": read_error(&error, path)})
                }
            }
        }
    }
    match last {
        Some(model) => json!({"kind":"observed", "model": model, "samples": samples}),
        None => json!({"kind":"no-model-yet"}),
    }
}

fn parent_for(rows: &[crate::state::RegistryEntry], sid: &str, harness: &str) -> Option<String> {
    let handle = crate::identity::canonical_handle(sid);
    for row in rows {
        let stored = if harness == "claude" {
            Some(row.short_id.as_str())
        } else {
            row.harness_session_id.as_deref()
        };
        let matches = row.name == handle
            || (row.harness.as_deref() == Some(harness)
                && stored.is_some_and(|id| id == sid || id == handle));
        if matches {
            let parent = row
                .spawned_by_session
                .as_deref()
                .filter(|v| !v.is_empty())?;
            row.spawned_by_harness
                .as_deref()
                .filter(|v| !v.is_empty())?;
            return Some(crate::identity::canonical_handle(parent));
        }
    }
    None
}

fn event_context() -> Value {
    let get = |key: &str| std::env::var(key).ok();
    let canonical = crate::spawn_context::resolve_canonical_resolution(&get);
    let (harness, session) = match canonical.class {
        crate::spawn_context::CanonicalClass::Absent => {
            crate::spawn_context::vendor_identity(&canonical.markers)
        }
        crate::spawn_context::CanonicalClass::Canonical => {
            (canonical.harness, canonical.session_id)
        }
        _ => (None, None),
    };
    let home = crate::paths::AgentsHome::from_env();
    let rows = crate::state::load_registry(&home.registry_json())
        .ok()
        .map(|registry| registry.entries)
        .unwrap_or_default();
    let parent = match (&session, &harness) {
        (Some(sid), Some(harness)) => parent_for(&rows, sid, harness),
        _ => None,
    };
    let (attester, witness) = crate::spawn_context::resolve_attester_identity(&get);
    let (witness_name, conflict) = match witness {
        crate::spawn_context::AttesterWitness::Process => ("process", None),
        crate::spawn_context::AttesterWitness::EnvOnly => ("env_only", None),
        crate::spawn_context::AttesterWitness::Conflict(marker, ancestor, environment) => ("conflict", Some(format!("attester identity conflict on {marker}: process ancestry carries {}, the emitting environment carries {}", crate::client_commands::py_repr_str(&ancestor), crate::client_commands::py_repr_str(&environment)))),
    };
    let observed = match (
        std::env::var("FNO_AGENT_HARNESS")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var("FNO_AGENT_PROVIDER").ok()),
        attester.as_deref(),
    ) {
        (Some(agent), Some(sid)) if !agent.is_empty() && !sid.is_empty() => {
            let stores = crate::session_truth::Stores::ambient();
            let session = crate::session_truth::TruthSession {
                agent: agent.clone(),
                session_id: sid.to_string(),
                transcript_path: None,
                row: None,
            };
            let path = crate::session_truth::session_transcript(&session, &stores);
            observed_model(&agent, path.as_deref())
        }
        _ => json!({"kind":"no-transcript"}),
    };
    let model = if session.is_some() && harness.is_some() {
        Some(crate::mail_cli::send::resolve_self_model())
    } else {
        None
    };
    let handle = session.as_deref().map(crate::identity::canonical_handle);
    json!({"session_id":session,"handle":handle,"harness":harness,"model":model,"parent":parent,"attester_session_id":attester.unwrap_or_default(),"attester_witness":witness_name,"attester_conflict":conflict,"observed_model":observed})
}

pub(super) fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("event-context") => {
            println!("{}", event_context());
            0
        }
        Some("observed-model") => {
            let mut agent = String::new();
            let mut path: Option<PathBuf> = None;
            let mut it = args[1..].iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "--agent" => agent = it.next().cloned().unwrap_or_default(),
                    "--path" => path = it.next().map(PathBuf::from),
                    _ => {
                        eprintln!("error: unknown observed-model argument {arg}");
                        return 2;
                    }
                }
            }
            println!("{}", observed_model(&agent, path.as_deref()));
            0
        }
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_lineage_matches_stored_identity_with_a_display_name() {
        let row = crate::state::RegistryEntry {
            name: "target-display-name".into(),
            harness: Some("claude".into()),
            short_id: "03401fb3".into(),
            spawned_by_session: Some("parentsess123".into()),
            spawned_by_harness: Some("claude".into()),
            ..Default::default()
        };
        assert_eq!(
            parent_for(&[row], "03401fb3-92b2-cafe", "claude").as_deref(),
            Some("parentse")
        );
    }

    #[test]
    fn observed_model_distinguishes_valid_torn_and_unobserved_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"assistant\",\"message\":{\"model\":\"model-a\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"model\":\"<synthetic>\"}}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            observed_model("claude", Some(&path)),
            json!({"kind":"observed", "model":"model-a", "samples":1})
        );
        let old_model = "{\"type\":\"turn_context\",\"payload\":{\"model\":\"old-model\"}}\n";
        let long_tail = "{\"type\":\"other\"}\n".repeat(20_000);
        std::fs::write(&path, format!("{old_model}{long_tail}")).unwrap();
        assert_eq!(
            observed_model("codex", Some(&path)),
            json!({"kind":"observed", "model":"old-model", "samples":1})
        );
        std::fs::write(&path, "{\"type\":\"turn_context\"").unwrap();
        assert_eq!(
            observed_model("codex", Some(&path)),
            json!({"kind":"unreadable", "reason":"torn final line (mid-write)"})
        );
        assert_eq!(
            observed_model("codex", None),
            json!({"kind":"no-transcript"})
        );
        assert_eq!(
            observed_model("opencode", None),
            json!({"kind":"not-file-backed"})
        );
    }
}

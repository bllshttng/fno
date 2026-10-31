use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Map, Value};

#[derive(Default)]
struct Options {
    kind: Option<String>,
    data: Option<String>,
    payload: Option<String>,
    source: Option<String>,
    state: Option<PathBuf>,
    events: Option<PathBuf>,
    global: bool,
    node: Option<String>,
    task: Option<String>,
    run: Option<String>,
    parent: Option<String>,
    outcome: Option<String>,
    project: Option<String>,
}

fn parse(args: &[OsString]) -> Result<Options, String> {
    let mut result = Options::default();
    let mut i = 0;
    while i < args.len() {
        let word = args[i].to_string_lossy();
        let (key, inline) = word
            .split_once('=')
            .map_or((word.as_ref(), None), |(a, b)| (a, Some(b)));
        if key == "--global" {
            result.global = true;
            i += 1;
            continue;
        }
        let value = match inline {
            Some(v) => v.to_string(),
            None => {
                i += 1;
                args.get(i)
                    .ok_or_else(|| format!("Option '{key}' requires an argument."))?
                    .to_string_lossy()
                    .into_owned()
            }
        };
        match key {
            "--type" | "-t" => result.kind = Some(value),
            "--data" | "-d" => result.data = Some(value),
            "--payload" => result.payload = Some(value),
            "--source" | "-s" => result.source = Some(value),
            "--state" => result.state = Some(value.into()),
            "--events" => result.events = Some(value.into()),
            "--node" => result.node = Some(value),
            "--task" => result.task = Some(value),
            "--run" => result.run = Some(value),
            "--parent" => result.parent = Some(value),
            "--outcome" => result.outcome = Some(value),
            "--project" => result.project = Some(value),
            _ => return Err(format!("No such option: {key}")),
        }
        i += 1;
    }
    if result.kind.is_none() {
        return Err("Missing option '--type' / '-t'.".into());
    }
    Ok(result)
}

fn agents(args: &[&str], input: Option<Vec<u8>>, timeout: u64) -> Result<Value, String> {
    let mut command = Command::new(crate::digest_overlay::fno_agents_bin());
    command.args(args);
    let out =
        crate::config_doctor::bounded_output(&mut command, input, Duration::from_secs(timeout))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

fn supplied_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(Value::Array(items)) if items.is_empty() => String::new(),
        Some(Value::Object(items)) if items.is_empty() => String::new(),
        Some(Value::String(text)) => text.trim().to_string(),
        Some(value) => crate::config_doctor::py_repr(value).trim().to_string(),
    }
}

fn repo_identity() -> Option<String> {
    let mut command = Command::new("git");
    command.current_dir(crate::config_cli::repo_root()).args([
        "config",
        "--get",
        "remote.origin.url",
    ]);
    let output =
        crate::config_doctor::bounded_output(&mut command, None, Duration::from_secs(5)).ok()?;
    if !output.status.success() {
        return None;
    }
    crate::repo_identity::repo_identity_from_remote_url(&String::from_utf8_lossy(&output.stdout))
}

fn context() -> Result<Value, String> {
    agents(&["attest", "event-context"], None, 60)
}

fn manifest(path: &Path) -> Map<String, Value> {
    let mut result = Map::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            for key in ["session_id", "graph_node_id"] {
                if let Some(value) = line.trim().strip_prefix(&format!("{key}:")) {
                    let value = value.trim().trim_matches('"').trim();
                    if !value.is_empty() && value != "null" {
                        result.entry(key).or_insert(json!(value));
                    }
                }
            }
        }
    }
    result
}

fn default_state() -> PathBuf {
    let settings = crate::config_doctor::Settings::load();
    let state = crate::config_doctor::worktree_space_dir(&settings).join("target-state.md");
    if state.exists() {
        state
    } else {
        crate::config_cli::repo_root().join(".fno/target-state.md")
    }
}

fn project_log() -> Result<PathBuf, String> {
    let settings = crate::config_doctor::Settings::load();
    let path = crate::config_doctor::project_space_dir(&settings).join("events.jsonl");
    let old = crate::config_cli::repo_root().join(".fno/events.jsonl");
    if old.exists() && !old.is_symlink() && !path.exists() {
        super::hermetic::check(&old)?;
        super::hermetic::check(&path)?;
        let mut command = Command::new(crate::digest_overlay::fno_agents_bin());
        command.args(["state", "path", "events"]);
        command.env_remove("FNO_EVENTS_PATH");
        command.current_dir(crate::config_cli::repo_root());
        let _ = crate::config_doctor::bounded_output(&mut command, None, Duration::from_secs(30));
    }
    Ok(path)
}

fn append(path: &Path, event: &Value) -> Result<(), String> {
    super::hermetic::check(path)?;
    if path.is_dir() {
        return Err(format!(
            "events path is a directory, not a journal: {}",
            path.display()
        ));
    }
    let line = event.to_string();
    let receipt = crate::event_store::append_envelope(path, &line, None)?;
    crate::first_check::record(path, &receipt.event_id, &line)?;
    Ok(())
}

fn push(
    parent: &str,
    kind: &str,
    run: Option<&str>,
    node: Option<&str>,
    reason: Option<&str>,
) -> bool {
    let mut msg = format!(
        "[fno:{kind}] run={}",
        run.filter(|v| !v.is_empty()).unwrap_or("?")
    );
    if let Some(node) = node.filter(|v| !v.is_empty()) {
        msg.push_str(&format!(" node={node}"));
    }
    if let Some(reason) = reason.filter(|v| !v.is_empty()) {
        msg.push_str(&format!(": {reason}"));
    }
    let mut command = Command::new(crate::digest_overlay::fno_agents_bin());
    command.args([
        "machine-mail-send",
        "--arm",
        "events-push",
        "--timeout-secs",
        "20",
        "--to",
        parent,
        "--",
        &msg,
    ]);
    match crate::config_doctor::bounded_output(&mut command, None, Duration::from_secs(20)) {
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() {
                eprintln!("push: note: parent push failed (non-fatal): {}", err.trim());
                return false;
            }
            if !err.is_empty() {
                eprintln!("push: note: {}", err.trim());
            }
            true
        }
        Err(error) => {
            eprintln!("push: note: parent push failed (non-fatal): {error}");
            false
        }
    }
}

pub(super) fn run(args: &[OsString], json_mode: bool) -> i32 {
    if args.iter().any(|arg| arg == "--help") {
        print!("{}", include_str!("event_emit_help.txt"));
        return 0;
    }
    let options = match parse(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    let kind = options.kind.as_deref().unwrap_or_default();
    if kind == "verification_receipt" {
        eprintln!("error: verification_receipt is preflight-owned; use the preflight runner");
        return 1;
    }
    if options.data.is_some() && options.payload.is_some() {
        eprintln!("error: pass either --data or --payload, not both");
        return 1;
    }
    if options.global && options.events.is_some() {
        eprintln!("error: pass either --global or --events, not both");
        return 1;
    }
    if options.payload.is_some() {
        eprintln!("warning: --payload is deprecated; use --data instead. The alias will be removed in a future release.");
    }
    let raw = options
        .payload
        .as_deref()
        .or(options.data.as_deref())
        .unwrap_or("{}");
    let mut data = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(v)) => v,
        Ok(_) => {
            eprintln!("error: --data must be a JSON object");
            return 1;
        }
        Err(e) => {
            eprintln!(
                "error: invalid JSON in --data: {}",
                crate::json_compat::py_parse_error(raw, &e)
            );
            return 1;
        }
    };
    let schema: Value = match serde_json::from_str(include_str!("event_store/events_schema.json")) {
        Ok(schema) => schema,
        Err(error) => {
            eprintln!("error: invalid compiled event schema: {error}");
            return 1;
        }
    };
    let family = schema["protocol_family"]["types"]
        .as_array()
        .is_some_and(|types| types.iter().any(|v| v == kind));
    let identity = if family || kind == "review_attestation" {
        context().ok()
    } else {
        None
    };
    if kind == "review_attestation" {
        let Some(ctx) = identity.as_ref() else {
            eprintln!("error: cannot resolve attester identity. No event emitted.");
            return 1;
        };
        if let Some(conflict) = ctx["attester_conflict"].as_str() {
            eprintln!("error: {conflict}. No event emitted under an identity the caller overrode.");
            return 1;
        }
        let resolved = ctx["attester_session_id"].as_str().unwrap_or("");
        let supplied = supplied_string(data.get("attester_session_id"));
        if !supplied.is_empty() && supplied != resolved {
            eprintln!("error: attester_session_id supplied as '{supplied}' but this process resolves to '{resolved}'. Drop the field; the emitter stamps it. No event emitted.");
            return 1;
        }
        data.insert("attester_session_id".into(), json!(resolved));
        data.insert("attester_witness".into(), ctx["attester_witness"].clone());
        let observed = &ctx["observed_model"];
        if let Some(model) = observed
            .get("model")
            .and_then(Value::as_str)
            .filter(|_| observed["kind"] == "observed")
        {
            let supplied = supplied_string(data.get("model"));
            if !supplied.is_empty() && supplied != model {
                eprintln!("error: model supplied as '{supplied}' but this session's transcript reads '{model}'. Drop the field; the emitter stamps it. No event emitted.");
                return 1;
            }
            data.insert("model".into(), json!(model));
        } else {
            data.remove("model");
        }
    }
    let state = options.state.unwrap_or_else(default_state);
    let default_events = match project_log() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let source = options.source.or_else(|| {
        std::fs::read_to_string(&state)
            .ok()
            .filter(|s| s.contains("session_id:"))
            .map(|_| "target".into())
    });
    let Some(source) = source else {
        let mut declared: Vec<_> = schema["event_types"][kind]["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        declared.sort();
        eprintln!("error: no target session detected and --source was not given, so this emit cannot name its producer. {kind} declares: {}.", if declared.is_empty() { "(none declared)".into() } else { declared.join(", ") });
        return 1;
    };
    if family {
        for key in ["title", "reason", "evidence", "termination_reason"] {
            if let Some(Value::String(v)) = data.get_mut(key) {
                if v.chars().count() > 500 {
                    *v = v.chars().take(500).collect();
                }
            }
        }
    }
    let mut event = Map::new();
    event.insert(
        "ts".into(),
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
    );
    event.insert("type".into(), json!(kind));
    event.insert("source".into(), json!(source));
    event.insert("data".into(), Value::Object(data));
    if family {
        let fields = manifest(&state);
        event.insert("v".into(), schema["protocol_family"]["version"].clone());
        let entries = [
            (
                "run",
                options.run.filter(|value| !value.is_empty()).or_else(|| {
                    fields
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            ),
            (
                "node",
                options.node.filter(|value| !value.is_empty()).or_else(|| {
                    fields
                        .get("graph_node_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }),
            ),
            ("task", options.task),
            (
                "parent",
                options
                    .parent
                    .filter(|value| !value.is_empty())
                    .or_else(|| {
                        identity
                            .as_ref()?
                            .get("parent")?
                            .as_str()
                            .map(str::to_string)
                    }),
            ),
            ("outcome", options.outcome),
            ("project", options.project),
        ];
        for (key, value) in entries {
            if let Some(value) = value {
                event.insert(key.into(), json!(value));
            }
        }
        if let Some(ctx) = &identity {
            if let (Some(_), Some(_)) = (ctx["session_id"].as_str(), ctx["harness"].as_str()) {
                event.insert("from".into(), ctx["handle"].clone());
                if let Some(model) = ctx["model"].as_str() {
                    event.insert("model".into(), json!(model));
                }
            }
        }
        let host = crate::agents_view::hostname();
        if !host.is_empty() {
            event.insert("host".into(), json!(host));
        }
    }
    if let Err(error) = crate::event_store::validate::validate_envelope(&event) {
        eprintln!("error: {error}");
        return 1;
    }
    let event = Value::Object(event);
    let journal = if options.global {
        crate::config_cli::history::global_journal()
    } else {
        options.events.unwrap_or(default_events)
    };
    if let Err(error) = append(&journal, &event) {
        eprintln!("error: failed to append event: {error}");
        return 1;
    }
    if matches!(
        kind,
        "review_attestation" | "review_coverage" | "worktree_removed"
    ) {
        let global = crate::config_cli::history::global_journal();
        if crate::config_cli::resolve_path(&global) != crate::config_cli::resolve_path(&journal) {
            let mut mirror = event.clone();
            if let Some(repo) = repo_identity() {
                mirror["data"]["repo"] = json!(repo);
            }
            if let Err(error) = append(&global, &mirror) {
                eprintln!("warning: {kind} not mirrored to the global log: {error}");
            }
        }
    }
    if kind == "review_attestation" {
        let payload = json!({"pr_number":null,"head_sha":event["data"]["head_sha"].as_str().unwrap_or(""),"verdict":event["data"]["verdict"].as_str().unwrap_or(""),"reviewer":event["data"]["reviewer"].as_str().unwrap_or(""),"cwd":crate::config_cli::repo_root().to_string_lossy(),"dry_run":false});
        match agents(
            &["publish-review"],
            Some(payload.to_string().into_bytes()),
            45,
        ) {
            Ok(receipt) => eprintln!(
                "{}",
                receipt["receipt"]
                    .as_str()
                    .unwrap_or("bot-review: skipped (no receipt)")
            ),
            Err(error) => eprintln!("bot-review: skipped (mirror error: {error})"),
        }
    }
    if kind == "blocked" {
        if let Some(parent) = event["parent"].as_str() {
            push(
                parent,
                kind,
                event["run"].as_str(),
                event["node"].as_str(),
                event["data"]["reason"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .or_else(|| event["data"]["termination_reason"].as_str()),
            );
        }
    }
    if json_mode {
        println!("{}", crate::doctor::python_json(&event));
    } else {
        println!(
            "{}",
            event["data"]["nonce"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| event["ts"].as_str().unwrap_or(""))
        );
    }
    0
}

pub(super) fn run_push(args: &[OsString]) -> i32 {
    if args.iter().any(|arg| arg == "--help") {
        print!("{}", include_str!("event_push_help.txt"));
        return 0;
    }
    let mut kind = None;
    let mut run = None;
    let mut node = None;
    let mut reason = None;
    let mut parent = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let word = arg.to_string_lossy();
        let (key, inline) = word
            .split_once('=')
            .map_or((word.as_ref(), None), |(a, b)| (a, Some(b)));
        let value = match inline {
            Some(value) => value.to_string(),
            None => match iter.next() {
                Some(value) => value.to_string_lossy().into_owned(),
                None => {
                    eprintln!("Error: Option '{key}' requires an argument.");
                    return 2;
                }
            },
        };
        match key {
            "--type" | "-t" => kind = Some(value),
            "--run" => run = Some(value),
            "--node" => node = Some(value),
            "--reason" | "-R" => reason = Some(value),
            "--parent" => parent = Some(value),
            _ => {
                eprintln!("Error: No such option: {key}");
                return 2;
            }
        }
    }
    let Some(kind) = kind else {
        eprintln!("Error: Missing option '--type' / '-t'.");
        return 2;
    };
    let parent = parent
        .filter(|s| !s.is_empty())
        .or_else(|| context().ok()?.get("parent")?.as_str().map(str::to_string));
    let Some(parent) = parent else {
        println!("push: no parent lineage; skipped");
        return 0;
    };
    let delivered = push(
        &parent,
        &kind,
        run.as_deref(),
        node.as_deref(),
        reason.as_deref(),
    );
    println!("{}", if delivered { "pushed" } else { "push-skipped" });
    0
}

pub(super) fn build() -> i32 {
    use std::io::Read;
    let mut raw = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("error: could not read event request: {error}");
        return 2;
    }
    let request: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("error: invalid event request: {error}");
            return 2;
        }
    };
    let Some(kind) = request.get("type").and_then(Value::as_str) else {
        eprintln!("error: event type is required");
        return 2;
    };
    let Some(source) = request.get("source").and_then(Value::as_str) else {
        eprintln!("error: event source is required");
        return 2;
    };
    let Some(mut data) = request.get("data").and_then(Value::as_object).cloned() else {
        eprintln!("error: event data must be an object");
        return 2;
    };
    match request.get("builder").and_then(Value::as_str) {
        Some("operator_question") => {
            if let Some(Value::String(question)) = data.get_mut("question") {
                *question = question.chars().take(2000).collect();
            }
            data.retain(|_, value| !value.is_null());
        }
        Some("integrity_warning") => {
            let kind = data.get("kind").and_then(Value::as_str).unwrap_or("");
            if kind != "missing_nonce_legacy_accepted" {
                eprintln!("error: unknown integrity_warning kind: '{kind}' (allowed: ['missing_nonce_legacy_accepted'])");
                return 3;
            }
        }
        Some("session_satisfied") => {
            let trigger = data.get("source").and_then(Value::as_str).unwrap_or("");
            let allowed = [
                "check_pr",
                "ci_watcher",
                "delegated",
                "fno_gate_manual",
                "pr_merge",
            ];
            if !allowed.contains(&trigger) {
                eprintln!("error: unknown session_satisfied trigger: '{trigger}' (allowed: ['check_pr', 'ci_watcher', 'delegated', 'fno_gate_manual', 'pr_merge'])");
                return 3;
            }
            for key in ["reason", "session_id", "gate_state_hash"] {
                if data
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(|value| value.trim().is_empty())
                {
                    eprintln!("error: session_satisfied {key} cannot be empty");
                    return 3;
                }
            }
            data.retain(|_, value| !value.is_null());
        }
        Some(other) => {
            eprintln!("error: unknown event builder: {other}");
            return 2;
        }
        None => {}
    }
    let mut event = Map::new();
    event.insert(
        "ts".into(),
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)),
    );
    event.insert("type".into(), json!(kind));
    event.insert("source".into(), json!(source));
    event.insert("data".into(), Value::Object(data));
    if let Err(error) = crate::event_store::validate::validate_envelope(&event) {
        eprintln!("error: {error}");
        return 3;
    }
    println!("{}", Value::Object(event));
    0
}

//! The resume door for a headless-only harness (`ResumeRoute::HeadlessAsk`):
//! footnote and zcode have no interactive resume form, so `--message`
//! resumes them as one ask turn through the lane that owns their rows.

use serde_json::Value;

use crate::paths::AgentsHome;

/// One ask turn on a headless-only row. The text is required: a resume with
/// no message has nothing to run (fnh's UserPromptSubmit hook only posts a
/// mail notice), and `--message-already-queued` still sends the text for
/// the same reason.
pub(crate) fn run(home: &AgentsHome, entry: &Value, name: &str, message: Option<&str>) -> i32 {
    let Some(message) = message.filter(|m| !m.is_empty()) else {
        eprintln!(
            "fno agents resume {name}: a headless-only session resumes one turn at a time; pass --message <text>"
        );
        return 2;
    };
    let cwd = entry.get("cwd").and_then(Value::as_str).unwrap_or("");
    let harness = entry
        .get("harness")
        .and_then(Value::as_str)
        .or_else(|| entry.get("provider").and_then(Value::as_str))
        .unwrap_or("");
    let session_id = crate::client_verbs::resume_session_id(entry, harness);
    if !std::path::Path::new(cwd).is_dir() {
        return crate::resume_gate::gone_cwd_refusal(cwd, name);
    }
    if let Some(code) =
        crate::resume_gate::pause_refusal_for_entry(home, name, session_id, entry, cwd)
    {
        return code;
    }
    // The zcode lane runs its child in params.cwd, and a wake's process cwd
    // is not the row's, so hand the recorded one over. The footnote lane
    // reads the row's cwd from the entry and ignores the param.
    let params = serde_json::json!({ "message": message, "cwd": cwd });
    if let Some(code) = crate::footnote_harness::maybe_run_ask(home, &params, name) {
        return code;
    }
    if let Some(code) = crate::zcode_ask::maybe_run_zcode_ask(home, &params, name) {
        return code;
    }
    eprintln!("fno agents resume: no headless resume lane for {name}");
    13
}

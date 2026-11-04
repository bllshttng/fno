//! Where `chokepoint_wait_exceeded` rows land for the `fno` binary.

use crate::chokepoint::Refused;

fn write(refused: &Refused) {
    let settings = crate::config_doctor::Settings::load();
    let path = crate::config_doctor::project_space_dir(&settings).join("events.jsonl");
    let envelope = serde_json::json!({
        "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "type": "chokepoint_wait_exceeded",
        "source": "loop",
        "data": refused.event_data(),
    });
    let _ = crate::event_store::append_envelope(&path, &envelope.to_string(), None);
}

/// Register the journal sink. `main` calls this first.
pub fn install() {
    crate::chokepoint::set_event_sink(write);
}

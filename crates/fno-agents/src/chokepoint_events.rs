//! Where `chokepoint_wait_exceeded` rows land for the fno-agents binaries.

use crate::chokepoint::Refused;

fn write(refused: &Refused) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let Some(path) = crate::state_path::resolve("events", &cwd) else {
        return;
    };
    let journal = crate::loop_runtime::Journal::new_raw(path.clone(), path);
    let _ = journal.append("chokepoint_wait_exceeded", refused.event_data());
}

/// Register the journal sink. Each binary calls this first thing in `main`.
pub fn install() {
    crate::chokepoint::set_event_sink(write);
}

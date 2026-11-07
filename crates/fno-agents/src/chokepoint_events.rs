//! Where `chokepoint_wait_exceeded` rows land for the fno-agents binaries.

use crate::chokepoint::Refused;

fn write(refused: &Refused) {
    append_row("chokepoint_wait_exceeded", refused.event_data());
}

/// Append one journal row from library code that holds no `Refused`: the
/// graph-lock holder's `graph_lock_held` event and the busy-write refusal
/// row. Same resolution as [`write`]; a resolution miss skips silently.
pub fn append_row(kind: &str, data: serde_json::Value) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let Some(path) = crate::state_path::resolve("events", &cwd) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let journal = crate::loop_runtime::Journal::new_raw(path.clone(), path);
    if let Err(error) = journal.append(kind, data) {
        // A lost row is a lost measurement; the judge or the store refused
        // it, so name both instead of vanishing.
        eprintln!("chokepoint_events: {kind} row lost: {error}");
    }
}

/// Register the journal sink. Each binary calls this first thing in `main`.
pub fn install() {
    crate::chokepoint::set_event_sink(write);
}

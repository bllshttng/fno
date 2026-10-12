//! The user's time-boxed lift of the learned spawn cap. One small file beside
//! the capacity state: `<agents home>/capacity/<machine-key>.grant.json`. The
//! decide door gives the authority and the journal row (an agent session is
//! refused there); this file is the hot read the spawn gate takes on every
//! queue pass, so the gate never shells out to read a decision. It is plain
//! state at the same trust level as the capacity file itself.

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::capacity::Effective;
use crate::paths::AgentsHome;

/// The longest window one grant may cover.
pub const MAX_WINDOW_SECS: i64 = 8 * 3600;
/// How long after its end a grant still reads as ended, so the user sees it
/// ended and was not lost. The tick deletes the file after this.
pub const ENDED_NOTE_SECS: i64 = 900;

/// The persisted grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub cap: u32,
    pub granted_epoch: i64,
    pub expires_epoch: i64,
    pub decision_id: String,
    pub learned_at_grant: u32,
}

/// What a cap reader sees of a grant: `active` inside the window, inactive for
/// [`ENDED_NOTE_SECS`] after it. `learned` is the cap before the lift.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantView {
    pub cap: usize,
    pub learned: usize,
    pub expires_epoch: i64,
    pub active: bool,
    pub decision_id: String,
}

pub fn path(home: &AgentsHome) -> PathBuf {
    home.root()
        .join("capacity")
        .join(format!("{}.grant.json", crate::capacity::machine_key()))
}

/// The grant on disk, `None` when missing or unparseable. A reader never
/// repairs a broken file.
pub fn read(home: &AgentsHome) -> Option<Grant> {
    serde_json::from_str(&std::fs::read_to_string(path(home)).ok()?).ok()
}

/// Tempfile and rename in the same directory, so a reader sees the old file
/// or the new one and never half of either.
pub fn write(home: &AgentsHome, grant: &Grant) -> io::Result<()> {
    let path = path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(grant)?)?;
    std::fs::rename(&tmp, &path)
}

/// Remove the grant file. A missing file is already clear.
pub fn clear(home: &AgentsHome) -> io::Result<()> {
    match std::fs::remove_file(path(home)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The tick's cleaner: drop a grant whose end is past the ended note window.
pub fn sweep(home: &AgentsHome, now: i64) {
    if read(home).is_some_and(|g| now - g.expires_epoch > ENDED_NOTE_SECS) {
        let _ = clear(home);
    }
}

/// Fold a grant into a cap answer. Inside the window a grant above the
/// learned cap lifts the cap to `grant.cap`, never past the ceiling. A grant
/// at or under the learned cap lifts nothing and leaves no trace. For
/// [`ENDED_NOTE_SECS`] after its end the answer keeps the learned cap and
/// carries an inactive view, so the clause says the grant ended.
pub(crate) fn apply(mut eff: Effective, grant: Option<Grant>, now: i64) -> Effective {
    let Some(grant) = grant else { return eff };
    let learned = eff.cap;
    let lifted = (grant.cap as usize).min(eff.ceiling);
    let active = now < grant.expires_epoch;
    if active && lifted > learned {
        eff.cap = lifted;
        eff.known = true;
    } else if active || now - grant.expires_epoch >= ENDED_NOTE_SECS {
        return eff;
    }
    eff.grant = Some(GrantView {
        cap: lifted,
        learned,
        expires_epoch: grant.expires_epoch,
        active,
        decision_id: grant.decision_id,
    });
    eff
}

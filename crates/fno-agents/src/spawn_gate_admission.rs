//! The machine admission boundary of the spawn gate: the runaway brake, and
//! the one caller key every arm here shares. The census and the process
//! ceiling live in the fno crate's own agent-spawn door (this crate never
//! links fno; it shells the binary at runtime), so a `fno`-front invocation
//! carries all three arms and a door this crate answers carries the brake.

use crate::spawn_gate::{Refusal, EXIT_FLEET_STOP};

/// True when this process carries a worker identity: the agent-spawn doors'
/// one caller key. FNO_AGENT_SELF names a spawned worker. A claude thread
/// worker carries none (the supervisor strips it), so CLAUDECODE=1 with
/// CLAUDE_CODE_SESSION_ATTENDED=0, the harness's own unattended marker,
/// counts too. An attended session, or one without both markers, is the
/// user's own typed verb, and no gate may refuse or hold it.
pub(crate) fn gate_agent_origin() -> bool {
    std::env::var_os("FNO_AGENT_SELF").is_some_and(|name| !name.is_empty())
        || (std::env::var_os("CLAUDECODE").is_some_and(|v| v == "1")
            && std::env::var_os("CLAUDE_CODE_SESSION_ATTENDED").is_some_and(|v| v == "0"))
}

/// The machine brake as a gate arm. The default is admit, and only an
/// agent-origin caller is held; the arm attributes load before it arms the
/// brake, so a hold here means fno's own fan-out. The user's pane taps admit
/// through the default door.
pub(crate) fn process_admission_gate() -> Result<(), Refusal> {
    if !gate_agent_origin() {
        return Ok(());
    }
    // The fleet map's takes-work toggle: the user said this machine takes
    // no new agent work, so no load figure needs to agree.
    let home = crate::paths::AgentsHome::from_env();
    if !crate::fleet_beacon::takes_work(&home) {
        return Err(Refusal::code(EXIT_FLEET_STOP)
            .ev("reason", serde_json::json!("machine-takes-no-work"))
            .ev(
                "detail",
                serde_json::json!(format!(
                    "this machine takes no new agent work; `fno agents fleet top --machines --takes-work on` clears {}",
                    crate::fleet_beacon::takes_work_path(&home).display()
                )),
            ));
    }
    match crate::machine_watch::brake_holds() {
        None => Ok(()),
        Some(hold) => {
            // One line: the verdict line carries the hold (reason, seconds
            // left) as its detail figure; the exit code carries the class.
            Err(Refusal::code(EXIT_FLEET_STOP)
                .ev("reason", serde_json::json!("machine-runaway"))
                .ev("detail", serde_json::json!(hold)))
        }
    }
}

/// Fixtures that exercise the agent-fan-out refusal arms carry the worker
/// identity those arms key on; removed on drop, before the env lock.
#[cfg(test)]
pub(crate) struct AgentSelfFixture;
#[cfg(test)]
impl AgentSelfFixture {
    pub(crate) fn set() -> Self {
        std::env::set_var("FNO_AGENT_SELF", "gate-fixture-worker");
        AgentSelfFixture
    }
}
#[cfg(test)]
impl Drop for AgentSelfFixture {
    fn drop(&mut self) {
        std::env::remove_var("FNO_AGENT_SELF");
    }
}

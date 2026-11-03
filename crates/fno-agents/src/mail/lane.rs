//! How a live message reaches a running session: one lane per transport, one
//! lane chosen per send. The module knows lane kinds and which target shape
//! each one serves. The transports themselves (pane inject, codex app-server,
//! daemon RPC, control socket) stay with the caller that owns the registry.

use super::state::DeliveryState;

/// What the engine knows about the recipient, flattened from its registry row
/// so this module never sees the registry's JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The harness's own session id, when the row carries one.
    pub session_id: Option<String>,
    pub harness: String,
    /// The row has a mux pane object.
    pub has_mux: bool,
    /// The row is a codex app-server thread.
    pub codex_thread: bool,
    /// The row is a keeper-backed thread: its messaging socket sits under
    /// `mux/threads/` and the harness contract names the keeper lane.
    pub keeper_thread: bool,
    pub messaging_socket: Option<String>,
}

/// The lanes, in the order a send tries them. The first whose `applies` is
/// true carries the send alone; a lane's own fallbacks stay inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneKind {
    /// A mux pane: inject through the pane, healing a dead codex pane to its thread.
    Pane,
    /// A codex app-server thread: switchboard first, then the codex inject.
    CodexThread,
    /// A keeper-backed thread session.
    Keeper,
    /// Any harness the daemon delivers for (ACP, the footnote-harness thread).
    DaemonDeliver,
    /// Claude and opencode: switchboard, then the control socket.
    ClaudeSwitchboard,
}

/// The order of the chain. The order is the contract.
pub const LADDER: [LaneKind; 5] = [
    LaneKind::Pane,
    LaneKind::CodexThread,
    LaneKind::Keeper,
    LaneKind::DaemonDeliver,
    LaneKind::ClaudeSwitchboard,
];

impl LaneKind {
    /// Whether this lane serves the target, given that no earlier lane in
    /// `LADDER` did.
    pub fn applies(self, t: &Target) -> bool {
        match self {
            LaneKind::Pane => t.has_mux,
            LaneKind::CodexThread => t.codex_thread,
            LaneKind::Keeper => t.keeper_thread && t.session_id.is_some(),
            LaneKind::DaemonDeliver => t.harness != "claude" && t.harness != "opencode",
            LaneKind::ClaudeSwitchboard => true,
        }
    }
}

/// One transport. `deliver` returns what the lane reached; a lane that cannot
/// reach the recipient says `Failed` with a reason and pushes it on `reasons`.
pub trait Lane {
    fn kind(&self) -> LaneKind;

    fn applies(&self, t: &Target) -> bool {
        self.kind().applies(t)
    }

    fn deliver(&self, wrapped: &str, t: &Target, reasons: &mut Vec<String>) -> DeliveryState;
}

/// The first lane whose `applies` is true, trying `lanes` in the order given.
pub fn first_applicable<'a>(lanes: &'a [Box<dyn Lane + 'a>], t: &Target) -> Option<&'a dyn Lane> {
    lanes.iter().map(|l| l.as_ref()).find(|l| l.applies(t))
}

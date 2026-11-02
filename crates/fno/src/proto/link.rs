use serde::{Deserialize, Serialize};

/// The first server generation that understands [`LinkHello`]. An older server
/// closes the connection on the unknown message, so the far door reads this
/// off the first `Layout` before it sends the hello.
pub const LINK_MIN_PROTO: u32 = 121;

/// Where a row on another machine lives: the host ssh dials, and the key the far
/// door resolves there (its session id, else its name).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRow {
    pub host: String,
    pub key: String,
    /// The `name@host` this row paints as, which its link pane also carries.
    pub name: String,
    /// What the card says the far session is doing, as of the last fleet
    /// read: `working`, `idle 3m`, or `offline 2m` once its machine's beat is stale.
    pub state: String,
}

/// Sent by a passive client right after its `Attach`: drive this one pane.
/// `identity` is the far door's reading of the pane (`pid:<child pid>`); a pane
/// id reused between that read and this hello carries a different child, and
/// the server refuses the tag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkHello {
    pub name: String,
    pub pane: u64,
    pub identity: String,
}

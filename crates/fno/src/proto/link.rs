use serde::{Deserialize, Serialize};

/// The first server generation that understands [`LinkHello`]. An older server
/// closes the connection on the unknown message, so the far door reads this
/// off the first `Layout` before it sends the hello.
pub const LINK_MIN_PROTO: u32 = 121;

/// Where a row on another machine lives: the host ssh dials, and the key the far
/// door resolves there (its session id, else its name).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRow {
    pub host: String,
    pub key: String,
    /// The `name@host` this row paints as, which its link pane also carries.
    pub name: String,
}

/// Painted beside the `name@host` title of a pane that shows a remote session.
pub const REMOTE_GLYPH: char = '\u{21c4}';

/// Whether a name is a remote row's `name@host` address, the form peek and mail
/// route by. A local worker's registered name carries no host.
pub fn is_remote_address(name: &str) -> bool {
    name.rsplit_once('@')
        .is_some_and(|(session, host)| !session.is_empty() && !host.is_empty())
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

//! What content area a tab is tiled into when more than one client views it.
//!
//! One pty has one winsize, so co-viewers of a tab always share one area.
//! `FNO_MUX_WINDOW_SIZE` (latched from `mux.window_size` by the client at
//! server spawn) picks which viewer decides it: the `smallest` clamp, the
//! viewer that last acted (`latest`, the default), or the `largest`.

use super::*;

/// How a tab's area is chosen from its viewers' dims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum WindowSize {
    Smallest,
    #[default]
    Latest,
    Largest,
}

impl WindowSize {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "smallest" => Some(Self::Smallest),
            "latest" => Some(Self::Latest),
            "largest" => Some(Self::Largest),
            _ => None,
        }
    }

    /// Unset reads the default; an unknown value reads the default too, with
    /// one log line so a typo does not pass silently.
    pub(super) fn from_env() -> Self {
        match std::env::var("FNO_MUX_WINDOW_SIZE") {
            Ok(raw) => Self::parse(&raw).unwrap_or_else(|| {
                eprintln!("fno mux: unknown FNO_MUX_WINDOW_SIZE {raw:?}; using latest");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }
}

/// Per-tab sizing state: the last-applied areas, the policy, and which
/// client acted last.
pub(super) struct TabSizing {
    /// Each tab's last-applied content area (Locked 1's "no viewers -> keep
    /// last size"). Written by the geometry pass for every viewed tab; read
    /// as the fallback when a tab loses its last viewer. Purged when a tab
    /// dies (ids are never reused, so stale entries would only accumulate).
    pub(super) areas: HashMap<TabId, (u16, u16)>,
    policy: WindowSize,
    /// Client id -> activity stamp; a higher stamp acted more recently.
    activity: HashMap<u64, u64>,
    seq: u64,
}

impl TabSizing {
    pub(super) fn with_policy(policy: WindowSize) -> Self {
        Self {
            areas: HashMap::new(),
            policy,
            activity: HashMap::new(),
            seq: 0,
        }
    }

    pub(super) fn from_env() -> Self {
        Self::with_policy(WindowSize::from_env())
    }

    /// Stamp `id` as the client that acted last. True when that changes who
    /// is latest, so the caller re-tiles before the message applies.
    pub(super) fn note_activity(&mut self, id: u64) -> bool {
        if self.policy != WindowSize::Latest {
            return false;
        }
        let already_latest = self.activity.get(&id).copied()
            == self.activity.values().max().copied()
            && self.activity.contains_key(&id);
        self.seq += 1;
        self.activity.insert(id, self.seq);
        !already_latest
    }

    pub(super) fn forget(&mut self, id: u64) {
        self.activity.remove(&id);
    }

    /// The area the policy picks from `viewers` (client id, dims), in attach
    /// order. Ties on activity go to the later attach.
    fn pick(&self, viewers: &[(u64, (u16, u16))]) -> Option<(u16, u16)> {
        match self.policy {
            WindowSize::Smallest => viewers
                .iter()
                .map(|v| v.1)
                .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1))),
            WindowSize::Largest => viewers
                .iter()
                .map(|v| v.1)
                .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1))),
            WindowSize::Latest => viewers
                .iter()
                .max_by_key(|v| self.activity.get(&v.0).copied().unwrap_or(0))
                .map(|v| v.1),
        }
    }
}

impl Core {
    /// A tab's content area: the policy's pick over every client currently
    /// viewing it; with no viewers it keeps its last-applied size, and a tab
    /// that has never been sized falls back to the VT defaults.
    pub(super) fn tab_area(&self, tid: TabId) -> (u16, u16) {
        // An observer (passive) client never enters the pick, so a
        // phone-sized viewer can never shrink a real client's PTY
        // (Locked Decision 4 / AC1-EDGE).
        let viewers: Vec<(u64, (u16, u16))> = self
            .clients
            .iter()
            .filter(|c| c.view.1 == tid && !c.passive)
            .map(|c| (c.id, c.dims))
            .collect();
        self.sizing
            .pick(&viewers)
            .or_else(|| self.sizing.areas.get(&tid).copied())
            .unwrap_or((crate::vt::DEFAULT_ROWS, crate::vt::DEFAULT_COLS))
    }

    pub(super) fn tab_rect(&self, tid: TabId) -> Rect {
        let (rows, cols) = self.tab_area(tid);
        Rect {
            x: 0,
            y: 0,
            rows,
            cols,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_size_policy_picks_per_row() {
        let viewers = [(1, (40, 120)), (2, (20, 80)), (3, (30, 200))];
        let mut sizing = TabSizing::with_policy(WindowSize::Smallest);
        assert_eq!(sizing.pick(&viewers), Some((20, 80)));
        sizing.policy = WindowSize::Largest;
        assert_eq!(sizing.pick(&viewers), Some((40, 200)));
        sizing.policy = WindowSize::Latest;
        for id in [1, 2, 3] {
            assert!(sizing.note_activity(id), "client {id} takes over");
        }
        assert_eq!(sizing.pick(&viewers), Some((30, 200)));
        assert!(!sizing.note_activity(3), "already latest changes nothing");
        assert!(sizing.note_activity(1));
        assert_eq!(sizing.pick(&viewers), Some((40, 120)));
        sizing.forget(1);
        assert_eq!(sizing.pick(&viewers[1..]), Some((30, 200)));
        assert_eq!(sizing.pick(&[]), None);
        assert_eq!(WindowSize::parse(" smallest "), Some(WindowSize::Smallest));
        assert_eq!(WindowSize::parse("tmux"), None);
    }
}

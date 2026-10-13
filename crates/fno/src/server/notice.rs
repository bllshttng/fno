//! The desktop-notice broadcast (wire v110): one `(title, body)` pair to
//! every attached client's terminal, rendered client-side as OSC 777 by
//! [`crate::notify`]. The pane-capture leg titles the alert with the pane
//! name; `fno notify` passes its title verbatim. Notification codes only, no
//! raw passthrough: the wire carries sanitized strings, never raw OSC bytes.
impl super::Core {
    /// Re-emit one desktop notification to EVERY attached client's tty,
    /// `title` verbatim (the pane-capture path resolves the pane name first;
    /// see [`Self::broadcast_pane_notice`]). Returns how many clients the
    /// notice was queued to. A wedged reliable channel is a dead client (the
    /// copy policy): torn down, and its notice does not count as delivered.
    pub(super) fn broadcast_desktop_notice(&mut self, title: &str, body: &str) -> usize {
        let mut queued = 0usize;
        let mut wedged: Vec<u64> = Vec::new();
        for c in &self.clients {
            if c.reliable_tx
                .try_send(super::ServerMsg::DesktopNotice {
                    title: title.to_string(),
                    body: body.to_string(),
                })
                .is_ok()
            {
                queued += 1;
            } else {
                wedged.push(c.id);
            }
        }
        if !wedged.is_empty() {
            // One retain + one layout push for the whole wedge batch: N
            // teardowns never pay N layout pushes.
            self.clients.retain(|c| !wedged.contains(&c.id));
            for id in wedged {
                eprintln!(
                    "fno mux: client {id} reliable channel wedged on DesktopNotice; dropping it"
                );
            }
            self.push_layout(true);
        }
        queued
    }

    /// The pane-capture leg: title the pane's own notification with the pane
    /// name (its mux-assigned name, else `pane <id>`), so the desktop alert
    /// names the pane it came from.
    pub(super) fn broadcast_pane_notice(&mut self, pane: u64, body: &str) {
        let title = self
            .panes
            .get(&pane)
            .and_then(|e| e.name.clone())
            .unwrap_or_else(|| format!("pane {pane}"));
        self.broadcast_desktop_notice(&title, body);
    }
}

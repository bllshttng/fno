//! Attach and input deadlines, armed only while a frame is missing.
use std::time::{Duration, Instant};

pub(super) struct Health {
    attached: Instant,
    first_frame: bool,
    attach_warned: bool,
    input: Option<Instant>,
    stall_warned: bool,
}

impl Health {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            attached: now,
            first_frame: false,
            attach_warned: false,
            input: None,
            stall_warned: false,
        }
    }
    pub(super) fn frame(&mut self, _now: Instant) {
        self.first_frame = true;
        self.input = None;
        self.stall_warned = false;
    }
    pub(super) fn input(&mut self, now: Instant) {
        self.input.get_or_insert(now);
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        let attach = (!self.first_frame && !self.attach_warned)
            .then_some(self.attached + Duration::from_secs(5));
        let paint = self
            .input
            .filter(|_| !self.stall_warned)
            .map(|t| t + Duration::from_secs(3));
        attach.into_iter().chain(paint).min()
    }
    pub(super) fn poll(&mut self, now: Instant) -> Vec<&'static str> {
        let mut events = Vec::new();
        if !self.first_frame
            && !self.attach_warned
            && now.duration_since(self.attached) >= Duration::from_secs(5)
        {
            self.attach_warned = true;
            events.push("attach-no-paint");
        }
        if !self.stall_warned
            && self
                .input
                .is_some_and(|t| now.duration_since(t) >= Duration::from_secs(3))
        {
            self.stall_warned = true;
            events.push("paint-stall");
        }
        events
    }
    pub(super) fn record(&mut self) {
        for event in self.poll(Instant::now()) {
            crate::mux_log::client(
                if event == "attach-no-paint" {
                    "error"
                } else {
                    "warn"
                },
                event,
            );
        }
    }
}

pub(super) async fn wait(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mux_log_client_reports_each_attach_and_paint_stall_once() {
        let t = Instant::now();
        let mut health = Health::new(t);
        assert!(health.poll(t + Duration::from_secs(4)).is_empty());
        assert_eq!(health.poll(t + Duration::from_secs(5)), ["attach-no-paint"]);
        assert!(health.poll(t + Duration::from_secs(30)).is_empty());
        health.frame(t + Duration::from_secs(31));
        health.input(t + Duration::from_secs(32));
        assert!(health.poll(t + Duration::from_secs(34)).is_empty());
        assert_eq!(health.poll(t + Duration::from_secs(35)), ["paint-stall"]);
        assert!(health.poll(t + Duration::from_secs(60)).is_empty());
        health.frame(t + Duration::from_secs(61));
        health.input(t + Duration::from_secs(62));
        health.frame(t + Duration::from_secs(63));
        assert!(health.poll(t + Duration::from_secs(90)).is_empty());
        assert_eq!(health.deadline(), None);
        health.input(t + Duration::from_secs(91));
        assert_eq!(health.poll(t + Duration::from_secs(94)), ["paint-stall"]);
    }
}

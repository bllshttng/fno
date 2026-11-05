//! A pane's input deadline, reset by output and limited to one warning per minute.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct EchoWatch {
    pending: Option<(Instant, Instant)>,
    last_warning: Option<Instant>,
}

impl EchoWatch {
    pub(super) fn sent(&mut self, now: Instant, last_output: Instant) {
        self.pending.get_or_insert((now, last_output));
    }
    pub(super) fn poll(&mut self, now: Instant, last_output: Instant) -> bool {
        let Some((sent, output)) = self.pending else {
            return false;
        };
        if output != last_output {
            self.pending = None;
            return false;
        }
        if now.duration_since(sent) < Duration::from_secs(3) {
            return false;
        }
        self.pending = None;
        if self
            .last_warning
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(60))
        {
            return false;
        }
        self.last_warning = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mux_log_input_warns_without_echo_and_limits_repeated_keys() {
        let t = Instant::now();
        let mut watch = EchoWatch::default();
        watch.sent(t, t);
        watch.sent(t + Duration::from_secs(1), t);
        assert!(!watch.poll(t + Duration::from_secs(2), t));
        assert!(watch.poll(t + Duration::from_secs(3), t));
        assert!(!watch.poll(t + Duration::from_secs(30), t));
        watch.sent(t + Duration::from_secs(31), t);
        assert!(!watch.poll(t + Duration::from_secs(34), t));
        watch.sent(t + Duration::from_secs(61), t);
        assert!(watch.poll(t + Duration::from_secs(64), t));
        watch.sent(t + Duration::from_secs(65), t);
        assert!(!watch.poll(t + Duration::from_secs(68), t + Duration::from_secs(66)));
        assert!(!watch.poll(t + Duration::from_secs(130), t + Duration::from_secs(66)));
    }
}

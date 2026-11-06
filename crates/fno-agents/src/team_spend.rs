use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SessionMeter {
    Exact(f64),
    Estimated(f64),
    Unmetered,
}

impl SessionMeter {
    pub(crate) fn usd(self) -> Option<f64> {
        match self {
            Self::Exact(usd) | Self::Estimated(usd) => Some(usd),
            Self::Unmetered => None,
        }
    }
}

pub(crate) fn session_meter(
    otel_db: &Path,
    ledger: &Path,
    fno_id: Option<&str>,
    harness_session_id: Option<&str>,
) -> SessionMeter {
    let harness_session_id = harness_session_id.filter(|id| !id.is_empty());
    if let Some(session_id) = harness_session_id {
        if let Some(usd) = crate::otel_ingest::session_cost_usd(otel_db, session_id) {
            return SessionMeter::Exact(usd);
        }
        if let Some(cents) = crate::model_price::served_session_cost(Some(session_id)).0 {
            return SessionMeter::Estimated(cents as f64 / 100.0);
        }
    }

    if let Some(fno_id) = fno_id.filter(|id| !id.is_empty()) {
        if let Some(usd) = crate::loopcheck::session_cost_from_ledger(ledger, fno_id) {
            return SessionMeter::Estimated(usd);
        }
    }
    if let Some(session_id) = harness_session_id.filter(|id| Some(*id) != fno_id) {
        if let Some(usd) = crate::loopcheck::session_cost_from_ledger(ledger, session_id) {
            return SessionMeter::Estimated(usd);
        }
    }

    SessionMeter::Unmetered
}

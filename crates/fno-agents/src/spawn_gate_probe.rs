//! The daemon's `agent.gate-probe` command: the spawn-gate probe's answer
//! served from one daemon-side compute instead of one child per caller.
//!
//! The mux client's org block reads this answer every refresh TTL. The
//! client used to spawn `fno-agents spawn-gate` per read, which made the
//! probe a per-client child paying a whole-fleet read (~1.6s CPU measured)
//! every 15 seconds - the reading the slowness audit budgeted at 0.02
//! cores of client child CPU. Here the daemon computes the SAME probe
//! in process (`probe_answer_at`, probe mode only) once per cwd per TTL
//! and every caller reads the cached answer over the socket.
//!
//! The cache keys on the caller's cwd because the probe's config reads
//! (`agents.max_live`, thresholds, provider lane caps) resolve per cwd: one
//! machine serving two spaces keeps two answers. One compute mutex caps
//! concurrent computes at one across every cwd; a caller that overtakes
//! re-checks the cache after the wait and serves what landed.

use crate::protocol::{ErrorCode, Request, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long one cwd's answer stays fresh. Matches the truth probe's 60s
/// cadence: both read the whole fleet, both amortize one compute across
/// every client of the daemon.
pub(crate) const GATE_PROBE_CACHE_TTL: Duration = Duration::from_secs(60);

/// The payload the mux client sends, as the daemon-side default. The gate
/// mode is unreachable through this command: [`serve`] refuses any other.
pub(crate) const PROBE_PAYLOAD_DEFAULT: &str = r#"{"mode":"probe","only":["lanes"],"tests":true}"#;

/// The daemon's `agent.gate-probe` handler. Params: `cwd` (required - the
/// probe's config root), `payload` (optional, defaults to the probe
/// payload). The answer carries `cached_s`: 0 for a fresh compute, else the
/// served entry's age, so a client can render honesty about freshness.
pub(crate) fn gate_probe_rpc<C>(_ctx: &C, req: &Request) -> Response {
    let Some(cwd) = req.params.get("cwd").and_then(Value::as_str) else {
        return Response::err(
            req.id,
            ErrorCode::InvalidParams,
            "missing `cwd`: the probe's config root is per caller",
        );
    };
    if cwd.is_empty() {
        return Response::err(req.id, ErrorCode::InvalidParams, "`cwd` is empty");
    }
    let payload = match req.params.get("payload") {
        Some(p) if p.is_object() => p.clone(),
        Some(_) => {
            return Response::err(
                req.id,
                ErrorCode::InvalidParams,
                "`payload` must be the probe's JSON object",
            )
        }
        None => serde_json::from_str(PROBE_PAYLOAD_DEFAULT).expect("the default payload parses"),
    };
    if payload.get("mode").and_then(Value::as_str) != Some("probe") {
        // The gate mode holds the spawn mutex and mints reservations; this
        // command serves readings only.
        return Response::err(
            req.id,
            ErrorCode::InvalidParams,
            "only `mode: \"probe\"` answers over the socket",
        );
    }
    let (answer, cached_s) = cache().serve(cwd, &payload);
    Response::ok(req.id, json!({"answer": answer, "cached_s": cached_s}))
}

/// One cwd-keyed answer cache with a single compute. `serve` returns the
/// answer and its age in seconds (0 for a fresh compute).
pub(crate) struct GateProbeCache {
    entries: Mutex<HashMap<String, (Instant, Value)>>,
    /// Caps concurrent computes at one across every cwd: the probe reads
    /// the whole registry and process table, so two computes racing would
    /// both pay the whole cost for answers one would serve.
    compute: Mutex<()>,
}

impl GateProbeCache {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            compute: Mutex::new(()),
        }
    }

    /// A fresh entry's `(answer, age_s)`, else `None`. Poison recovery
    /// takes the inner map: a panicked reader must not wedge the glance.
    fn fresh(&self, key: &str) -> Option<(Value, u64)> {
        let map = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (at, answer) = map.get(key)?;
        let age = at.elapsed();
        (age < GATE_PROBE_CACHE_TTL).then(|| (answer.clone(), age.as_secs()))
    }

    pub(crate) fn serve(&self, cwd: &str, payload: &Value) -> (Value, u64) {
        if let Some(hit) = self.fresh(cwd) {
            return hit;
        }
        let _gate = self
            .compute
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A caller that waited behind another cwd's compute re-checks: the
        // entry that landed while it waited is fresher than its own would be.
        if let Some(hit) = self.fresh(cwd) {
            return hit;
        }
        let answer = crate::spawn_gate_command::probe_answer_at(Some(Path::new(cwd)), payload);
        let map = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.insert(cwd.to_string(), (Instant::now(), answer.clone()));
        (answer, 0)
    }
}

fn cache() -> &'static GateProbeCache {
    static CACHE: OnceLock<GateProbeCache> = OnceLock::new();
    CACHE.get_or_init(GateProbeCache::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured probe payload, fixture-shaped so the tests never read
    /// the live registry.
    fn fixture_payload() -> Value {
        serde_json::json!({"mode": "probe", "only": ["lanes"], "tests": true})
    }

    #[test]
    fn a_fresh_entry_serves_cached_and_a_compute_is_one_per_ttl() {
        let cache = GateProbeCache::new();
        let (first, age) = cache.serve("/tmp/cwd-a", &fixture_payload());
        assert_eq!(age, 0, "the first serve computes");
        let (second, age) = cache.serve("/tmp/cwd-a", &fixture_payload());
        assert_eq!(second, first, "the second serve reads the cache");
        assert!(age > 0, "a served entry carries its age");
    }

    #[test]
    fn a_second_cwd_keeps_its_own_entry() {
        let cache = GateProbeCache::new();
        let (first_a, _) = cache.serve("/tmp/cwd-a", &fixture_payload());
        let (_, age_b) = cache.serve("/tmp/cwd-b", &fixture_payload());
        assert_eq!(age_b, 0, "the other cwd computes its own");
        let (second_a, age_a) = cache.serve("/tmp/cwd-a", &fixture_payload());
        assert_eq!(
            second_a, first_a,
            "the first cwd's entry survived the other cwd's compute"
        );
        assert!(age_a > 0, "the first cwd serves cached, not recomputed");
    }

    #[test]
    fn an_expired_entry_recomputes() {
        let cache = GateProbeCache::new();
        let (first, _) = cache.serve("/tmp/cwd-a", &fixture_payload());
        // Expire by hand: the TTL is 60s and the test must not wait it out.
        let map = cache
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let old = Instant::now() - GATE_PROBE_CACHE_TTL - Duration::from_secs(1);
        map.insert("/tmp/cwd-a".into(), (old, first.clone()));
        drop(map);
        let (_, age) = cache.serve("/tmp/cwd-a", &fixture_payload());
        assert_eq!(age, 0, "an entry past the TTL computes again");
    }

    #[test]
    fn the_handler_refuses_gate_mode_and_missing_cwd() {
        let req = |params: Value| crate::protocol::Request::new(1, "agent.gate-probe", params);
        let gate = gate_probe_rpc(
            &(),
            &req(serde_json::json!({"cwd": "/tmp/x", "payload": {"mode": "gate"}})),
        );
        assert!(gate.is_err(), "gate mode is refused over the socket");
        let no_cwd = gate_probe_rpc(&(), &req(serde_json::json!({})));
        assert!(no_cwd.is_err(), "a missing cwd is refused");
        let empty = gate_probe_rpc(&(), &req(serde_json::json!({"cwd": ""})));
        assert!(empty.is_err(), "an empty cwd is refused");
        let probe = gate_probe_rpc(&(), &req(serde_json::json!({"cwd": "/tmp/cwd-tests"})));
        assert!(!probe.is_err(), "the default payload serves");
    }
}

//! A law row's `when`: a start, a state and a daily window.
//!
//! ```json
//! "when": {
//!   "from":   {"kind": "date", "at": "<RFC 3339>"} | {"kind": "event", "node_id": "x-1"},
//!   "while":  [{"cmd": "<shell>"}],
//!   "window": {"start": "22:00", "end": "08:00"}
//! }
//! ```
//!
//! `from` keeps the law off until it starts, and a started law stays started.
//! The law is on only while every `while` command exits 0. `window` is
//! machine-local and can wrap midnight. All three must pass. An event start
//! reads the graph and a state guard runs a command, so the rule set build
//! reads a cache and the match and stage doors refresh it. A reading that
//! cannot run keeps the last answer, and with none the law stays on.

use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const STATE_TTL_SECS: i64 = 60;
const READ_TIMEOUT_SECS: u64 = 10;
const CACHE_FILE: &str = "law-when.json";

/// `--from date:<RFC 3339 | YYYY-MM-DD>` or `--from event:<node>`. A bare
/// date reads as midnight in this machine's time zone.
pub fn parse_from(spec: &str) -> Result<Value, String> {
    let bad = || format!("--from wants date:<when> or event:<node>, got {spec:?}");
    match spec.split_once(':').ok_or_else(bad)? {
        ("date", t) => {
            let at = DateTime::parse_from_rfc3339(t)
                .map(|t| t.with_timezone(&Utc))
                .ok()
                .or_else(|| {
                    let midnight = NaiveDate::parse_from_str(t, "%Y-%m-%d")
                        .ok()?
                        .and_hms_opt(0, 0, 0)?;
                    let local = Local.from_local_datetime(&midnight).earliest()?;
                    Some(local.with_timezone(&Utc))
                })
                .ok_or_else(bad)?;
            Ok(json!({"kind": "date", "at": at.to_rfc3339()}))
        }
        ("event", n) if !n.trim().is_empty() => Ok(json!({"kind": "event", "node_id": n.trim()})),
        _ => Err(bad()),
    }
}

/// `--between HH:MM-HH:MM`. Equal ends are refused: that window is empty or
/// always open, and neither is a window.
pub fn parse_window(spec: &str) -> Result<Value, String> {
    let bad = || format!("--between wants HH:MM-HH:MM with two different times, got {spec:?}");
    let (a, b) = spec.split_once('-').ok_or_else(bad)?;
    let time = |t: &str| NaiveTime::parse_from_str(t.trim(), "%H:%M").map_err(|_| bad());
    let (start, end) = (time(a)?, time(b)?);
    if start == end {
        return Err(bad());
    }
    Ok(json!({"start": start.format("%H:%M").to_string(), "end": end.format("%H:%M").to_string()}))
}

/// The row's `when` object from the door flags, `None` when no flag was given.
pub fn build_when(
    from: Option<&str>,
    whiles: &[String],
    between: Option<&str>,
) -> Result<Option<Value>, String> {
    let mut when = Map::new();
    if let Some(f) = from {
        when.insert("from".into(), parse_from(f)?);
    }
    let cmds: Vec<Value> = whiles
        .iter()
        .filter(|c| !c.trim().is_empty())
        .map(|c| json!({"cmd": c.trim()}))
        .collect();
    if !cmds.is_empty() {
        when.insert("while".into(), Value::Array(cmds));
    }
    if let Some(b) = between {
        when.insert("window".into(), parse_window(b)?);
    }
    Ok((!when.is_empty()).then(|| Value::Object(when)))
}

/// Whether `now` is inside the window. A start after the end wraps midnight.
pub fn window_open(window: &Value, now: NaiveTime) -> bool {
    let at = |k: &str| {
        let t = window.get(k)?.as_str()?;
        NaiveTime::parse_from_str(t, "%H:%M").ok()
    };
    let (Some(start), Some(end)) = (at("start"), at("end")) else {
        return true;
    };
    let now = now.with_second(0).unwrap_or(now);
    if start < end {
        start <= now && now < end
    } else {
        now >= start || now < end
    }
}

/// What one reading left behind for one law.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    started: Option<bool>,
    on: Option<bool>,
    at: Option<String>,
}

#[derive(Default, Clone, Debug)]
pub struct Cache(BTreeMap<String, Entry>);

impl Cache {
    pub fn load() -> Cache {
        let path = crate::decision_index::default_state_path(CACHE_FILE);
        let map = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        Cache(map.unwrap_or_default())
    }

    fn save(&self) {
        let path = crate::decision_index::default_state_path(CACHE_FILE);
        let Ok(text) = serde_json::to_string(&self.0) else {
            return;
        };
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn law_id(row: &Value) -> &str {
    row.get("decision_id").and_then(Value::as_str).unwrap_or("")
}

/// Why a law is off, `None` when every part of its `when` passes.
pub fn off_reason(
    row: &Value,
    now: DateTime<Utc>,
    local: NaiveTime,
    cache: &Cache,
) -> Option<String> {
    let when = row.get("when").filter(|w| w.is_object())?;
    let seen = cache.0.get(law_id(row));
    match when.pointer("/from/kind").and_then(Value::as_str) {
        Some("date") => {
            let at = when.pointer("/from/at").and_then(Value::as_str)?;
            if DateTime::parse_from_rfc3339(at).is_ok_and(|t| t > now) {
                return Some(format!("starts {at}"));
            }
        }
        Some("event") if seen.and_then(|e| e.started) == Some(false) => {
            let node = when.pointer("/from/node_id").and_then(Value::as_str);
            return Some(format!("starts when {} closes", node.unwrap_or("its node")));
        }
        _ => {}
    }
    let has_state = when
        .get("while")
        .and_then(Value::as_array)
        .is_some_and(|w| !w.is_empty());
    if has_state && seen.and_then(|e| e.on) == Some(false) {
        return Some("its state guard reads off".into());
    }
    let window = when.get("window")?;
    (!window_open(window, local)).then(|| format!("outside its window {window}"))
}

/// Re-read the graph and the state commands for one law and record the
/// answers. A reading that cannot run leaves the last answer in place.
fn refresh_row(row: &Value, cache: &mut Cache) {
    let Some(when) = row.get("when").filter(|w| w.is_object()) else {
        return;
    };
    let entry = cache.0.entry(law_id(row).to_string()).or_default();
    if let Some(node) = when.pointer("/from/node_id").and_then(Value::as_str) {
        if entry.started != Some(true) {
            let graph = crate::backlog::decisions_cli::read_graph_entries().unwrap_or_default();
            let found = graph
                .iter()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(node));
            if let Some(n) = found {
                entry.started = Some(!crate::backlog_ready::node_is_open(n));
            }
        }
    }
    let fresh = entry
        .at
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| (Utc::now() - t.with_timezone(&Utc)).num_seconds() < STATE_TTL_SECS);
    let cmds = when
        .get("while")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let cmds: Vec<&str> = cmds.filter_map(|w| w.get("cmd")?.as_str()).collect();
    if cmds.is_empty() || fresh {
        return;
    }
    let root = crate::law_match::evidence_repo_root();
    let mut on = true;
    for cmd in cmds {
        match crate::evidence::shell_run(cmd, &root, READ_TIMEOUT_SECS) {
            Ok(out) => on &= out.exit == 0,
            Err(_) => return,
        }
    }
    entry.on = Some(on);
    entry.at = Some(Utc::now().to_rfc3339());
}

/// Whether the law applies now. `refresh` runs the graph read and the state
/// commands first; the rule set build passes false and reads the cache only.
pub fn row_active(row: &Value, refresh: bool) -> bool {
    if row.get("when").is_none_or(|w| !w.is_object()) {
        return true;
    }
    let mut cache = Cache::load();
    if refresh {
        refresh_row(row, &mut cache);
        cache.save();
    }
    off_reason(row, Utc::now(), Local::now().time(), &cache).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn cached(started: Option<bool>, on: Option<bool>) -> Cache {
        Cache(BTreeMap::from([(
            "d-1".into(),
            Entry {
                started,
                on,
                at: None,
            },
        )]))
    }

    #[test]
    fn the_flags_parse_and_bad_ones_are_refused() {
        assert_eq!(build_when(None, &[], None).unwrap(), None);
        let w = build_when(Some("event:x-1"), &["true".into()], Some("22:00-08:00")).unwrap();
        let w = w.unwrap();
        assert_eq!(w["from"]["node_id"], "x-1");
        assert_eq!(w["while"][0]["cmd"], "true");
        assert!(parse_from("date:2026-11-04").unwrap()["at"].is_string());
        for bad in ["0.4.5", "event:", "date:soon"] {
            assert!(parse_from(bad).is_err(), "{bad}");
        }
        for bad in ["08:00-08:00", "8am-5pm"] {
            assert!(parse_window(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_part_must_pass_and_an_unread_part_fails_open() {
        let now = Utc::now();
        let dated = json!({"decision_id": "d-1", "when": {"from": {"kind": "date", "at": "2999-01-01T00:00:00+00:00"}}});
        assert!(off_reason(&dated, now, t(1, 0), &Cache::default())
            .unwrap()
            .starts_with("starts "));
        let event =
            json!({"decision_id": "d-1", "when": {"from": {"kind": "event", "node_id": "x-1"}}});
        assert_eq!(off_reason(&event, now, t(1, 0), &Cache::default()), None);
        assert!(off_reason(&event, now, t(1, 0), &cached(Some(false), None)).is_some());
        assert_eq!(
            off_reason(&event, now, t(1, 0), &cached(Some(true), None)),
            None
        );
        let both = json!({"decision_id": "d-1", "when": {"while": [{"cmd": "true"}], "window": {"start": "22:00", "end": "08:00"}}});
        assert_eq!(
            off_reason(&both, now, t(23, 0), &cached(None, Some(true))),
            None
        );
        assert!(off_reason(&both, now, t(23, 0), &cached(None, Some(false))).is_some());
        assert!(off_reason(&both, now, t(12, 0), &cached(None, Some(true))).is_some());
        assert!(row_active(&json!({"decision_id": "d-9"}), false));
    }
}

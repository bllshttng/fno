//! The "when is this law on" half of a law row: a start condition, an on/off
//! state and a recurring window. A law row carries them under `when`:
//!
//! ```json
//! "when": {
//!   "from":   {"kind": "date", "at": "<RFC 3339>"} | {"kind": "event", "node_id": "x-1"},
//!   "while":  [{"cmd": "<shell>"}],
//!   "window": {"start": "22:00", "end": "08:00"}
//! }
//! ```
//!
//! `from` keeps the law off until it starts, and once started it stays
//! started. Each `while` command must exit 0 for the law to be on. `window`
//! is machine-local wall time and may wrap midnight. All three must pass.
//!
//! The date and window parts are pure. An event start reads the graph and a
//! state guard runs a command, so the hot readers (the rule set build) read
//! the cache and never shell out; the match and stage doors refresh it. An
//! unreadable reading keeps the last cached answer, and with no answer the law
//! stays on, the posture every law reader keeps when it cannot decide.

use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// A state reading younger than this is trusted without re-running.
const STATE_TTL_SECS: i64 = 60;
const READ_TIMEOUT_SECS: u64 = 10;
const CACHE_FILE: &str = "law-when.json";

pub const USAGE: &str =
    "[--from date:<when>|event:<node>] [--while <cmd>]... [--between HH:MM-HH:MM]";

/// `--from`: `date:<RFC 3339 | YYYY-MM-DD | YYYY-MM-DDTHH:MM>` or `event:<node>`.
/// A bare date or local datetime reads in this machine's time zone.
pub fn parse_from(spec: &str) -> Result<Value, String> {
    let (kind, rest) = spec
        .split_once(':')
        .ok_or_else(|| format!("--from wants date:<when> or event:<node>, got {spec:?}"))?;
    match kind {
        "date" => parse_instant(rest)
            .map(|at| json!({"kind": "date", "at": at.to_rfc3339()}))
            .ok_or_else(|| format!("--from date:{rest} is not an RFC 3339 time or a date")),
        "event" if !rest.trim().is_empty() => Ok(json!({"kind": "event", "node_id": rest.trim()})),
        _ => Err(format!(
            "--from wants date:<when> or event:<node>, got {spec:?}"
        )),
    }
}

fn parse_instant(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.with_timezone(&Utc));
    }
    let naive = if let Ok(d) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        d.and_hms_opt(0, 0, 0)?
    } else {
        chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M").ok()?
    };
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// `--between HH:MM-HH:MM`, machine-local. Equal ends are refused: that
/// window is either empty or always open, and neither is a window.
pub fn parse_window(spec: &str) -> Result<Value, String> {
    let bad = || format!("--between wants HH:MM-HH:MM, got {spec:?}");
    let (a, b) = spec.split_once('-').ok_or_else(bad)?;
    let time = |t: &str| NaiveTime::parse_from_str(t.trim(), "%H:%M").map_err(|_| bad());
    let (start, end) = (time(a)?, time(b)?);
    if start == end {
        return Err(format!(
            "--between {spec:?} opens and closes at the same minute"
        ));
    }
    Ok(json!({
        "start": start.format("%H:%M").to_string(),
        "end": end.format("%H:%M").to_string(),
    }))
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
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(|c| json!({"cmd": c}))
        .collect();
    if !cmds.is_empty() {
        when.insert("while".into(), Value::Array(cmds));
    }
    if let Some(b) = between {
        when.insert("window".into(), parse_window(b)?);
    }
    Ok((!when.is_empty()).then(|| Value::Object(when)))
}

/// Whether `now` falls inside the window. A start after the end wraps midnight.
pub fn window_open(window: &Value, now: NaiveTime) -> bool {
    let at = |k: &str| {
        window
            .get(k)
            .and_then(Value::as_str)
            .and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok())
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

/// What the cache remembers about one law.
#[derive(Default, Clone, Debug)]
pub struct Cache(BTreeMap<String, Value>);

impl Cache {
    pub fn load() -> Cache {
        let path = crate::decision_index::default_state_path(CACHE_FILE);
        let map = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<BTreeMap<String, Value>>(&t).ok())
            .unwrap_or_default();
        Cache(map)
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

    fn started(&self, id: &str) -> Option<bool> {
        self.0.get(id)?.get("started")?.as_bool()
    }

    fn on(&self, id: &str) -> Option<bool> {
        self.0.get(id)?.get("on")?.as_bool()
    }

    fn read_at(&self, id: &str) -> Option<DateTime<Utc>> {
        let at = self.0.get(id)?.get("at")?.as_str()?;
        DateTime::parse_from_rfc3339(at)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    fn entry(&mut self, id: &str) -> &mut Map<String, Value> {
        let slot = self
            .0
            .entry(id.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        slot.as_object_mut().expect("object just ensured")
    }
}

/// Why a law is off, `None` when every part of its `when` passes.
pub fn off_reason(
    row: &Value,
    now: DateTime<Utc>,
    local: NaiveTime,
    cache: &Cache,
) -> Option<String> {
    let when = row.get("when").filter(|w| w.is_object())?;
    let id = row
        .get("decision_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(from) = when.get("from") {
        match from.get("kind").and_then(Value::as_str) {
            Some("date") => {
                let at = from.get("at").and_then(Value::as_str).unwrap_or_default();
                let started = DateTime::parse_from_rfc3339(at).map_or(true, |t| t <= now);
                if !started {
                    return Some(format!("starts {at}"));
                }
            }
            Some("event") if cache.started(id) == Some(false) => {
                let node = from.get("node_id").and_then(Value::as_str).unwrap_or("");
                return Some(format!("starts when {node} closes"));
            }
            _ => {}
        }
    }
    if when
        .get("while")
        .and_then(Value::as_array)
        .is_some_and(|w| !w.is_empty())
        && cache.on(id) == Some(false)
    {
        return Some("its state guard reads off".into());
    }
    if let Some(window) = when.get("window") {
        if !window_open(window, local) {
            let (s, e) = (
                window["start"].as_str().unwrap_or(""),
                window["end"].as_str().unwrap_or(""),
            );
            return Some(format!("outside its window {s}-{e}"));
        }
    }
    None
}

/// Re-read the graph and the state commands for one law and record the
/// answers. An unreadable reading leaves the last answer in place.
pub fn refresh_row(row: &Value, cache: &mut Cache, entries: &mut Option<Option<Vec<Value>>>) {
    let Some(when) = row.get("when").filter(|w| w.is_object()) else {
        return;
    };
    let id = row
        .get("decision_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if id.is_empty() {
        return;
    }
    let now = Utc::now();
    if let Some(node) = when
        .pointer("/from/node_id")
        .and_then(Value::as_str)
        .filter(|_| cache.started(&id) != Some(true))
    {
        let graph =
            entries.get_or_insert_with(|| crate::backlog::decisions_cli::read_graph_entries().ok());
        let closed = graph.as_deref().and_then(|g| {
            g.iter()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(node))
                .map(|n| !crate::backlog_ready::node_is_open(n))
        });
        if let Some(closed) = closed {
            cache.entry(&id).insert("started".into(), json!(closed));
        }
    }
    let cmds: Vec<&str> = when
        .get("while")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| w.get("cmd").and_then(Value::as_str))
        .collect();
    let fresh = cache
        .read_at(&id)
        .is_some_and(|t| (now - t).num_seconds() < STATE_TTL_SECS);
    if cmds.is_empty() || fresh {
        return;
    }
    let root = crate::law_match::evidence_repo_root();
    let mut on = true;
    for cmd in cmds {
        match crate::evidence::shell_run(cmd, &root, READ_TIMEOUT_SECS) {
            Ok(out) if out.exit == 0 => {}
            Ok(_) => on = false,
            Err(_) => return,
        }
    }
    let e = cache.entry(&id);
    e.insert("on".into(), json!(on));
    e.insert("at".into(), json!(now.to_rfc3339()));
}

/// Whether the law applies right now. `refresh` lets the caller run the
/// graph read and the state commands first; the rule set build passes false
/// and reads only what the cache holds.
pub fn row_active(row: &Value, refresh: bool) -> bool {
    if row.get("when").is_none_or(|w| !w.is_object()) {
        return true;
    }
    let mut cache = Cache::load();
    if refresh {
        refresh_row(row, &mut cache, &mut None);
        cache.save();
    }
    off_reason(row, Utc::now(), Local::now().time(), &cache).is_none()
}

/// Refresh every law row once, sharing one graph read, for the record door.
pub fn warm(row: &Value) {
    let mut cache = Cache::load();
    refresh_row(row, &mut cache, &mut None);
    cache.save();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn law(when: Value) -> Value {
        json!({"decision_id": "d-1", "when": when})
    }

    #[test]
    fn a_window_wraps_midnight() {
        let w = parse_window("22:00-08:00").unwrap();
        assert!(window_open(&w, t(23, 30)));
        assert!(window_open(&w, t(7, 59)));
        assert!(!window_open(&w, t(8, 0)));
        assert!(!window_open(&w, t(12, 0)));
        let day = parse_window("09:00-17:00").unwrap();
        assert!(window_open(&day, t(9, 0)));
        assert!(!window_open(&day, t(17, 0)));
    }

    #[test]
    fn a_bad_window_or_start_is_refused() {
        assert!(parse_window("08:00-08:00").is_err());
        assert!(parse_window("8am-5pm").is_err());
        assert!(parse_from("0.4.5").is_err());
        assert!(parse_from("event:").is_err());
        assert_eq!(parse_from("event:x-1").unwrap()["node_id"], "x-1");
        assert!(parse_from("date:2026-11-04").unwrap()["at"].is_string());
    }

    #[test]
    fn no_flag_builds_no_when() {
        assert_eq!(build_when(None, &[], None).unwrap(), None);
        let w = build_when(
            Some("event:x-1"),
            &["gh run list".into()],
            Some("22:00-08:00"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(w["while"][0]["cmd"], "gh run list");
        assert_eq!(w["window"]["start"], "22:00");
    }

    #[test]
    fn a_date_start_holds_the_law_off_until_it_passes() {
        let row = law(json!({"from": {"kind": "date", "at": "2026-11-10T00:00:00+00:00"}}));
        let cache = Cache::default();
        let before = DateTime::parse_from_rfc3339("2026-11-09T00:00:00Z")
            .unwrap()
            .into();
        let after = DateTime::parse_from_rfc3339("2026-11-10T00:00:01Z")
            .unwrap()
            .into();
        assert!(off_reason(&row, before, t(12, 0), &cache)
            .unwrap()
            .starts_with("starts "));
        assert_eq!(off_reason(&row, after, t(12, 0), &cache), None);
    }

    #[test]
    fn an_event_start_stays_off_until_the_cache_reads_started() {
        let row = law(json!({"from": {"kind": "event", "node_id": "x-1"}}));
        let now = Utc::now();
        let mut cache = Cache::default();
        assert_eq!(
            off_reason(&row, now, t(12, 0), &cache),
            None,
            "no reading fails open"
        );
        cache.entry("d-1").insert("started".into(), json!(false));
        assert_eq!(
            off_reason(&row, now, t(12, 0), &cache).as_deref(),
            Some("starts when x-1 closes")
        );
        cache.entry("d-1").insert("started".into(), json!(true));
        assert_eq!(off_reason(&row, now, t(12, 0), &cache), None);
    }

    #[test]
    fn a_state_guard_follows_the_cached_reading() {
        let row = law(json!({"while": [{"cmd": "true"}]}));
        let now = Utc::now();
        let mut cache = Cache::default();
        assert_eq!(off_reason(&row, now, t(1, 0), &cache), None);
        cache.entry("d-1").insert("on".into(), json!(false));
        assert!(off_reason(&row, now, t(1, 0), &cache).is_some());
        cache.entry("d-1").insert("on".into(), json!(true));
        assert_eq!(off_reason(&row, now, t(1, 0), &cache), None);
    }

    #[test]
    fn every_part_must_pass() {
        let row = law(json!({
            "window": {"start": "22:00", "end": "08:00"},
            "while": [{"cmd": "true"}],
        }));
        let now = Utc::now();
        let mut cache = Cache::default();
        cache.entry("d-1").insert("on".into(), json!(true));
        assert_eq!(off_reason(&row, now, t(23, 0), &cache), None);
        assert!(off_reason(&row, now, t(12, 0), &cache)
            .unwrap()
            .contains("window"));
    }

    #[test]
    fn a_row_with_no_when_is_always_on() {
        assert!(row_active(&json!({"decision_id": "d-9"}), false));
    }
}

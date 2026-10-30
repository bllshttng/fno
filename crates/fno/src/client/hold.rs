//! The machine-wide fleet hold, shown where the user looks and lifted with
//! one key. fno-agents writes the hold to `<agents home>/fleet-stop.json`
//! (reserve mode, the machine watch, an operator stop). The status row reads
//! that file and names what holds, why, and until when. prefix+U or the
//! sideline menu lifts a hold the machine armed. A stop a person armed shows
//! the command instead, because lifting it is that person's call.

use super::*;

/// Origins the mux may lift: a reserve (named or the daemon's) and the
/// machine watch's overload hold.
const LIFTABLE: &[&str] = &["reserve", "reserve_auto", "machine_watch"];

/// How long one read of the record serves the status row's repaints.
const READ_TTL: Duration = Duration::from_secs(2);

/// The last read and when it was taken; a lift clears it so the row drops
/// the segment on the next paint.
static CACHE: std::sync::Mutex<Option<(Instant, Option<Hold>)>> = std::sync::Mutex::new(None);

/// An armed, unexpired machine-wide hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hold {
    pub holds: Vec<String>,
    pub origin: String,
    pub reason: String,
    /// The expiry in local `HH:MM`, or `None` for a hold with no time box.
    pub until: Option<String>,
}

impl Hold {
    pub(crate) fn liftable(&self) -> bool {
        LIFTABLE.contains(&self.origin.as_str())
    }

    /// The status-row segment: what holds, why, until when, and the way out.
    pub(crate) fn segment(&self) -> String {
        let why = self
            .reason
            .split([',', ':', '('])
            .next()
            .unwrap_or_default()
            .trim();
        let mut text = format!("⏸ held: {}", self.holds.join(","));
        if !why.is_empty() {
            text.push_str(&format!(" · {why}"));
        }
        if let Some(until) = &self.until {
            text.push_str(&format!(" · until {until}"));
        }
        if self.liftable() {
            text.push_str(&format!(" · {}+U lifts", crate::keys::prefix_display()));
        } else {
            text.push_str(" · fno agents audit incident clear");
        }
        text
    }
}

/// Parse the record. `None` when it is clear, holds nothing, or expired.
pub(crate) fn parse(raw: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Hold> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v["state"].as_str() != Some("stopped") {
        return None;
    }
    let holds: Vec<String> = v["holds"]
        .as_array()?
        .iter()
        .filter_map(|h| h.as_str().map(str::to_string))
        .collect();
    if holds.is_empty() {
        return None;
    }
    let expires = v["expires_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    if expires.is_some_and(|at| at <= now) {
        return None;
    }
    Some(Hold {
        holds,
        origin: v["origin"].as_str().unwrap_or_default().to_string(),
        reason: v["reason"].as_str().unwrap_or_default().to_string(),
        until: expires.map(|at| at.with_timezone(&chrono::Local).format("%H:%M").to_string()),
    })
}

/// The hold in force now, read at most once per [`READ_TTL`]. The agents
/// home resolves the way the registry does. Under test with no declared home
/// there is no hold: the ambient one is the real machine's.
pub(crate) fn current() -> Option<Hold> {
    if cfg!(test) && std::env::var_os("FNO_AGENTS_HOME").is_none() {
        return None;
    }
    let mut cache = CACHE.lock().ok()?;
    if let Some((at, hold)) = cache.as_ref() {
        if at.elapsed() < READ_TTL {
            return hold.clone();
        }
    }
    let path = crate::agents_view::runtime_home().join("fleet-stop.json");
    let hold = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| parse(&raw, chrono::Utc::now()));
    *cache = Some((Instant::now(), hold.clone()));
    hold
}

/// prefix+U and the menu row: clear a hold the machine armed through the one
/// clear command, and say what happened. A person's stop is refused with the
/// command, never cleared from a keypress.
pub(super) async fn lift(view: &mut View) {
    view.aux = None;
    let Some(hold) = current() else {
        view.set_notice("no fleet hold to lift".into());
        return;
    };
    if !hold.liftable() {
        view.set_notice(format!(
            "this hold was armed by {}; lift it with fno agents audit incident clear",
            hold.origin
        ));
        return;
    }
    let mut command =
        crate::process_admission::tokio_command(crate::digest_overlay::fno_agents_bin());
    command
        .args([
            "fleet-incident",
            "clear",
            "--reason",
            "lifted from the mux",
            "--by",
            "mux",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let notice = match tokio::time::timeout(Duration::from_secs(10), fut).await {
        Ok(Ok(out)) if out.status.success() => {
            if let Ok(mut cache) = CACHE.lock() {
                *cache = None;
            }
            format!("lifted the fleet hold on {}", hold.holds.join(","))
        }
        Ok(Ok(out)) => format!(
            "lift refused: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Ok(Err(e)) => format!("lift failed: {e}"),
        Err(_) => "lift failed: fno-agents gave no answer within 10s".into(),
    };
    view.set_notice(notice);
}

/// Whether the daemon arms a reserve while OBS or an editor runs
/// (`agents.reserve_auto`, default on), read from the mux's cwd ladder.
pub(super) fn reserve_auto_on() -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::digest_overlay::section_bool(&cwd, "agents", "reserve_auto", true)
}

/// The settings toggle: flip `agents.reserve_auto` in the config fno-agents
/// reads, then rebuild the modal so the glyph tracks the file.
pub(super) fn toggle_reserve_auto(view: &mut View) {
    let on = !reserve_auto_on();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let notice = match crate::spawn_floor::set_agents_bool("reserve_auto", on, &cwd) {
        Ok(path) => format!("agents.reserve_auto = {on} in {}", path.display()),
        Err(e) => format!("reserve toggle not saved: {}", e.message),
    };
    view.set_notice(notice);
    view.reopen_settings_keeping_sel();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        "2026-10-22T22:00:00Z".parse().unwrap()
    }

    fn record(origin: &str, expires_at: &str) -> String {
        serde_json::json!({
            "state": "stopped",
            "holds": ["spawns", "tests"],
            "origin": origin,
            "reason": "OBS is running, so the fleet keeps the machine free for stream",
            "expires_at": expires_at,
        })
        .to_string()
    }

    /// hold_banner: an armed reserve names what holds, why, until when, and
    /// the key that lifts it.
    #[test]
    fn hold_banner_names_the_hold_and_its_lift() {
        let hold = parse(&record("reserve_auto", "2026-10-22T23:00:00Z"), now()).unwrap();
        let until = chrono::DateTime::parse_from_rfc3339("2026-10-22T23:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        assert!(hold.liftable());
        assert_eq!(
            hold.segment(),
            format!(
                "⏸ held: spawns,tests · OBS is running · until {until} · {}+U lifts",
                crate::keys::prefix_display()
            )
        );
    }

    /// reserve_lift: a stop a person armed is shown with the command, never
    /// offered to the key; an expired or clear record shows nothing.
    #[test]
    fn reserve_lift_refuses_a_person_stop_and_hides_a_lapsed_one() {
        let operator = parse(&record("operator", "2026-10-22T23:00:00Z"), now()).unwrap();
        assert!(!operator.liftable());
        assert!(operator
            .segment()
            .ends_with("fno agents audit incident clear"));
        assert_eq!(
            parse(&record("reserve", "2026-10-22T21:00:00Z"), now()),
            None
        );
        let clear = r#"{"state":"clear","holds":[],"origin":"reserve"}"#;
        assert_eq!(parse(clear, now()), None);
    }
}

//! The thread-prompt watch: a native `claude --bg` worker parked on a
//! permission prompt. It has no mux pane, so the check-in's pane scrape never
//! sees it. `claude agents --json` names the wait but not when it began, so
//! each read records the first sighting under `prompt-wait/<short id>.json`.
//! Contract: docs/architecture/spawn-watch.md.

use crate::claude_roster::{ClaudeAgentRow, ClaudeAgentsSnapshot};
use crate::paths::AgentsHome;
use crate::state::{self, Registry};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// A person answering at the keyboard clears a prompt well inside this.
pub(crate) const PARK_AFTER_MS: i64 = 5 * 60_000;
const SENDER: &str = "fno/prompt-wait";
/// The spawner walk stops here, so a lineage loop cannot hang a pass.
const MAX_HOPS: usize = 8;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Seen {
    first_seen_ms: i64,
    notified: bool,
    /// The recipients a send reached, so a retry mails only the rest.
    told: Vec<String>,
}

/// One worker waiting on a prompt now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Waiting {
    pub short_id: String,
    pub session_id: Option<String>,
    pub name: String,
    pub cwd: String,
    pub waiting_for: String,
    pub since_ms: i64,
    notified: bool,
    told: Vec<String>,
}

impl Waiting {
    pub(crate) fn overdue(&self, now: i64) -> bool {
        now.saturating_sub(self.since_ms) >= PARK_AFTER_MS
    }

    fn minutes(&self, now: i64) -> i64 {
        now.saturating_sub(self.since_ms) / 60_000
    }

    /// The check-in line: the finding and the remedy in one sentence.
    pub(crate) fn line(&self, now: i64) -> String {
        format!(
            "WAITING ON APPROVAL: {} (claude thread {}) {} for {}m - answer with: claude attach {}",
            self.name,
            self.short_id,
            self.waiting_for,
            self.minutes(now),
            self.short_id
        )
    }
}

fn dir(home: &AgentsHome) -> PathBuf {
    home.root().join("prompt-wait")
}

/// The short id is the file name, so only a plain hex id is admitted.
fn waiting_on_permission(row: &ClaudeAgentRow, carries_pids: bool) -> bool {
    let id = &row.short_id;
    let waits = row.waiting_for.as_deref().unwrap_or_default();
    row.has_live_process(carries_pids)
        && waits.to_ascii_lowercase().contains("permission")
        && !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Record the first sighting of every waiting row and return them all. A
/// clean listing also drops the file of every row that stopped waiting. A
/// partial or failed listing drops nothing: absence from it is no evidence.
pub(crate) fn observe(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
) -> Result<Vec<Waiting>, String> {
    let dir = dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let carries_pids = snapshot.carries_pids();
    let rows = match snapshot {
        ClaudeAgentsSnapshot::Known { rows, .. } | ClaudeAgentsSnapshot::Unknown { rows, .. } => {
            rows
        }
    };
    let mut out = Vec::new();
    let mut keep = HashSet::new();
    for row in rows {
        if !waiting_on_permission(row, carries_pids) {
            continue;
        }
        keep.insert(row.short_id.clone());
        let path = dir.join(format!("{}.json", row.short_id));
        let mut seen: Seen = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if seen.first_seen_ms <= 0 {
            seen.first_seen_ms = now;
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
        out.push(Waiting {
            short_id: row.short_id.clone(),
            session_id: row.session_id.clone(),
            name: row.name.clone().unwrap_or_else(|| row.short_id.clone()),
            cwd: row.cwd.clone().unwrap_or_default(),
            waiting_for: row.waiting_for.clone().unwrap_or_default(),
            since_ms: seen.first_seen_ms,
            notified: seen.notified,
            told: seen.told,
        });
    }
    if snapshot.is_known() && snapshot.warning_text().is_empty() {
        for path in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = path.path();
            let json = path.extension().is_some_and(|e| e == "json");
            let stem = path.file_stem().and_then(|s| s.to_str());
            if json && stem.is_some_and(|id| !keep.contains(id)) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A row names its session in either id field, the way burn-watch reads it.
fn is_session(r: &crate::state::RegistryEntry, sid: &str) -> bool {
    r.harness_session_id.as_deref() == Some(sid) || r.session_id.as_deref() == Some(sid)
}

fn parent_of(home: &AgentsHome, registry: &Registry, sid: &str) -> Option<String> {
    registry
        .entries
        .iter()
        .find(|r| is_session(r, sid))
        .and_then(|r| r.spawned_by_session.clone())
        .or_else(|| crate::spawn_watch::caller_of(home, sid))
        .filter(|s| !s.is_empty() && s != sid)
}

/// Who hears about this wait: the spawner, then the nearest ancestor that
/// holds a lead role. One session that is both hears it once.
fn recipients(home: &AgentsHome, registry: &Registry, sid: &str) -> Vec<String> {
    let lead = |s: &str| {
        let mut rows = registry.entries.iter();
        rows.any(|r| is_session(r, s) && r.role_level.is_some())
    };
    let mut out: Vec<String> = parent_of(home, registry, sid).into_iter().collect();
    let mut cur = out.first().cloned();
    for _ in 0..MAX_HOPS {
        let Some(s) = cur.take() else { break };
        if lead(s.as_str()) {
            if !out.contains(&s) {
                out.push(s);
            }
            break;
        }
        cur = parent_of(home, registry, &s);
    }
    out
}

fn notice(w: &Waiting, now: i64) -> String {
    format!(
        "Worker {} has waited {}m on a {} on {}. Nothing answers it on its own. Run `claude attach {}` on that host and pick an option.",
        w.name,
        w.minutes(now),
        w.waiting_for,
        crate::session_origin::this_machine(),
        w.short_id
    )
}

pub(crate) fn run_pass(
    home: &AgentsHome,
    now: i64,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    run_pass_with(
        home,
        now,
        &crate::claude_roster::read_all_agents_union(),
        runner,
    )
}

fn run_pass_with(
    home: &AgentsHome,
    now: i64,
    snapshot: &ClaudeAgentsSnapshot,
    runner: crate::burn_watch::Runner<'_>,
) -> Result<(), String> {
    let waiting = observe(home, now, snapshot)?;
    if !waiting.iter().any(|w| w.overdue(now) && !w.notified) {
        return Ok(());
    }
    let registry = state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?;
    let mut first_error = None;
    for w in waiting.iter().filter(|w| w.overdue(now) && !w.notified) {
        let text = notice(w, now);
        let to = w
            .session_id
            .as_deref()
            .map(|sid| recipients(home, &registry, sid))
            .unwrap_or_default();
        let mut told = w.told.clone();
        // With no spawner on record, one fleet task reaches the operator.
        let notified = if to.is_empty() {
            let run = format!("claude attach {}", w.short_id);
            let store = crate::provider_cap::questions_path(home);
            let key = format!("prompt wait {}", w.short_id);
            crate::fleet_task::file_once(&store, SENDER, &key, &w.cwd, &text, Some(&run), None)
                .map_err(|e| first_error.get_or_insert(e).clone())
                .is_ok()
        } else {
            let pending: Vec<String> = to.iter().filter(|s| !told.contains(*s)).cloned().collect();
            for sid in pending {
                match crate::first_check::send_mail(&mut *runner, SENDER, &sid, &text) {
                    Ok(()) => told.push(sid),
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            to.iter().all(|sid| told.contains(sid))
        };
        if notified || told != w.told {
            let seen = Seen {
                first_seen_ms: w.since_ms,
                notified,
                told,
            };
            let path = dir(home).join(format!("{}.json", w.short_id));
            state::write_json_atomic(&path, &seen).map_err(|e| e.to_string())?;
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Lineage, RegistryEntry};

    fn waiting_row(short: &str, sid: &str, name: &str) -> ClaudeAgentRow {
        let mut row = ClaudeAgentRow::new(short, Some("blocked")).with_pid(Some(12949));
        row.session_id = Some(sid.into());
        row.name = Some(name.into());
        row.cwd = Some("/work".into());
        row.waiting_for = Some("permission prompt".into());
        row
    }

    #[test]
    fn a_thread_prompt_mails_the_spawner_and_its_lead_once_after_five_minutes() {
        let temp = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(temp.path().join("agents"));
        let (lead, mid, child) = ("aaaa-lead", "bbbb-mid", "30d6ff6a-e15b");
        let row = |sid: &str, parent: Option<&str>, level: Option<u32>| {
            let mut e = RegistryEntry::new(Some(sid.into()), Lineage::unproven("test"));
            e.harness_session_id = Some(sid.into());
            e.spawned_by_session = parent.map(str::to_string);
            e.role_level = level;
            e
        };
        state::update_registry(&home.registry_json(), |r| {
            r.entries.push(row(lead, None, Some(1)));
            r.entries.push(row(mid, Some(lead), None));
            r.entries.push(row(child, Some(mid), None));
        })
        .unwrap();
        let busy = ClaudeAgentRow::new("ed2495bd", Some("working")).with_pid(Some(1));
        let parked = waiting_row("30d6ff6a", child, "t-recovery-prune");
        let snap = ClaudeAgentsSnapshot::known(vec![busy, parked.clone()]);
        let sent: std::cell::RefCell<Vec<(String, String)>> = Default::default();
        let mut deliver = |argv: &[String], _: &str| {
            sent.borrow_mut().push((argv[8].clone(), argv[9].clone()));
            (0, "delivered (hosted)".into(), String::new())
        };
        let t0 = 1_000_000;
        run_pass_with(&home, t0, &snap, &mut deliver).unwrap();
        run_pass_with(&home, t0 + PARK_AFTER_MS - 1, &snap, &mut deliver).unwrap();
        assert!(sent.borrow().is_empty(), "a fresh prompt is no news");
        let mut lead_down = |argv: &[String], _: &str| {
            if argv[8] == lead {
                (1, String::new(), "transport unavailable".to_string())
            } else {
                deliver(argv, "")
            }
        };
        assert!(run_pass_with(&home, t0 + PARK_AFTER_MS, &snap, &mut lead_down).is_err());
        run_pass_with(&home, t0 + PARK_AFTER_MS, &snap, &mut deliver).unwrap();
        {
            let sent = sent.borrow();
            let to: Vec<&str> = sent.iter().map(|(to, _)| to.as_str()).collect();
            assert_eq!(to, vec![mid, lead], "spawner, then the lead, each once");
            assert!(
                sent[0].1.contains("claude attach 30d6ff6a"),
                "{}",
                sent[0].1
            );
        }
        run_pass_with(&home, t0 + 3 * PARK_AFTER_MS, &snap, &mut deliver).unwrap();
        assert_eq!(sent.borrow().len(), 2, "one notice per wait");
        // The check-in line reads the same first sighting.
        let seen = observe(&home, t0 + PARK_AFTER_MS, &snap).unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0]
            .line(t0 + PARK_AFTER_MS)
            .starts_with("WAITING ON APPROVAL: t-recovery-prune (claude thread 30d6ff6a)"));
        // A partial listing keeps the file; a clean one without the row drops
        // it, so the next prompt counts from its own start.
        let partial = ClaudeAgentsSnapshot::Unknown {
            rows: vec![],
            warnings: vec!["timed out".into()],
        };
        observe(&home, t0, &partial).unwrap();
        assert!(dir(&home).join("30d6ff6a.json").exists());
        observe(&home, t0, &ClaudeAgentsSnapshot::known(vec![])).unwrap();
        assert!(!dir(&home).join("30d6ff6a.json").exists());
        let again = observe(&home, t0 + 9 * PARK_AFTER_MS, &snap).unwrap();
        assert_eq!(again[0].since_ms, t0 + 9 * PARK_AFTER_MS);
        // A dead process keeps its last waitingFor; it is not waiting.
        let mut dead = parked;
        dead.pid = None;
        let live = ClaudeAgentRow::new("ed2495bd", Some("working")).with_pid(Some(1));
        let snap = ClaudeAgentsSnapshot::known(vec![live, dead]);
        assert!(observe(&home, t0, &snap).unwrap().is_empty());
    }
}

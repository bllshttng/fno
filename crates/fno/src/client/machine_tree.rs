//! The sideline's machine layer, behind the `other machines` experimental
//! toggle: a band per machine above whatever the agent group (prefix+W) paints.
//!
//! This machine's band holds the ordinary tree from the server `Layout`. Every
//! other machine's rows come from the fleet map, read off the UI loop while the
//! toggle is on, and paint as ordinary `AgentRow` values with `remote` set,
//! grouped on the same axis. No connection to the other machine exists until
//! Enter opens one row in a portal, as a local paneless row opens: the seat's
//! child is `fno mux serve --link <key> --host <host>`. With the toggle off,
//! or fewer than two machines, the tree paints with no machine layer.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::org_board::{OrgMsg, OrgTx};
use super::*;
use crate::proto::RemoteRow;

const READ_EVERY: Duration = Duration::from_secs(15);

/// Another machine's session: its row and the band each axis files it under.
#[derive(Debug)]
struct Seat {
    row: AgentRow,
    project: String,
    lead: String,
    cwd: String,
    /// The fleet map says `writing`. Its status words carry no needs-input
    /// state, so a far row never lands in that band.
    working: bool,
    /// Its session id, and its parent seat's, for nesting under workspace.
    id: Option<String>,
    parent: Option<String>,
}

/// One machine from the newest fleet read.
#[derive(Debug, Default)]
pub(super) struct Band {
    host: String,
    local: bool,
    fresh: bool,
    age_s: i64,
    /// Another machine's sessions. Empty for this machine.
    seats: Vec<Seat>,
}

#[derive(Debug, Default)]
pub(super) struct MachineTree {
    /// The `other machines` toggle.
    pub(super) on: bool,
    bands: Vec<Band>,
    inflight: bool,
    last_read: Option<Instant>,
}

impl MachineTree {
    pub(super) fn load() -> Self {
        let name = crate::view_store::EXPERIMENTAL_OTHER_MACHINES;
        Self {
            on: crate::view_store::load_experimental(name),
            ..Self::default()
        }
    }

    /// Far rows reading blocked. Zero while the toggle is off.
    pub(super) fn blocked_remote(&self) -> usize {
        if !self.on {
            return 0;
        }
        self.rows()
            .filter(|r| r.badge == Some(AgentBadge::Blocked))
            .count()
    }

    /// The rows this tree painted, for a menu action to resolve at execution time.
    pub(super) fn rows(&self) -> impl Iterator<Item = &AgentRow> {
        self.bands
            .iter()
            .flat_map(|band| &band.seats)
            .map(|seat| &seat.row)
    }
}

/// Turning the toggle off drops the bands at once, so no far row outlives it.
pub(super) fn set_on(view: &mut View, on: bool) {
    view.machine_tree = MachineTree {
        on,
        ..MachineTree::default()
    };
}

/// The off-loop read, single-flight, only while the toggle is on.
pub(super) fn maybe_kick(view: &mut View, tx: &OrgTx) {
    let tree = &mut view.machine_tree;
    let due = tree.last_read.is_none_or(|at| at.elapsed() >= READ_EVERY);
    if !tree.on || tree.inflight || !due {
        return;
    }
    tree.inflight = true;
    tree.last_read = Some(Instant::now());
    let tx = tx.clone();
    tokio::spawn(async move {
        let read = super::org_seats::read_fleet().await;
        let _ = tx.send((0, OrgMsg::Machines(read)));
    });
}

/// A failed read keeps the last good bands: a row must not vanish because the
/// primary blinked. An aged band already reads offline. A read that lands after
/// the toggle went off is dropped.
pub(super) fn apply(view: &mut View, read: Result<Value, String>) {
    view.machine_tree.inflight = false;
    match read {
        Ok(map) if view.machine_tree.on && map.get("error").is_none() => {
            view.machine_tree.bands = fold(&map);
        }
        Ok(_) | Err(_) => {}
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Fleet top's `machines` as bands, this machine first.
fn fold(map: &Value) -> Vec<Band> {
    let machines = map["machines"].as_array().into_iter().flatten();
    let mut bands: Vec<Band> = machines.map(fold_machine).collect();
    // With no beat from this machine yet, its rows still paint from the Layout.
    if !bands.iter().any(|band| band.local) {
        bands.push(Band {
            host: crate::agents_view::hostname(),
            local: true,
            ..Band::default()
        });
    }
    bands.sort_by(|a, b| (!a.local, &a.host).cmp(&(!b.local, &b.host)));
    bands
}

fn fold_machine(machine: &Value) -> Band {
    let host = text(machine, "host").unwrap_or_else(|| "unknown host".into());
    let local = machine["local"].as_bool().unwrap_or(false);
    let sessions: Vec<&Value> = machine["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|seat| text(seat, "name").is_some())
        .filter(|seat| !super::org_seats::is_shell(text(seat, "harness").as_deref()))
        .collect();
    let mut band = Band {
        local,
        fresh: machine["fresh"].as_bool().unwrap_or(false),
        age_s: machine["age_s"].as_i64().unwrap_or(0),
        ..Band::default()
    };
    if !local {
        let seats = sessions
            .iter()
            .map(|seat| fold_seat(seat, &sessions, &host, &band));
        band.seats = seats.collect();
    }
    band.host = host;
    band
}

fn fold_seat(seat: &Value, all: &[&Value], host: &str, band: &Band) -> Seat {
    let cwd = text(seat, "cwd").map(|cwd| {
        let base = cwd
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        base.to_string()
    });
    Seat {
        row: remote_row(seat, host, seat_state(seat, band)),
        project: text(seat, "project").unwrap_or_else(|| "(no project)".into()),
        lead: lead_of(seat, all),
        cwd: cwd
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "(no cwd)".into()),
        working: text(seat, "status").as_deref() == Some("writing"),
        id: text(seat, "session_id"),
        parent: parent_seat(seat, all).and_then(|p| text(p, "session_id")),
    }
}

/// The seat a seat's `parent` names. The fleet map writes the head of the
/// parent's session id.
fn parent_seat<'a>(seat: &Value, all: &[&'a Value]) -> Option<&'a Value> {
    let head: String = text(seat, "parent")?.chars().take(8).collect();
    let mut seats = all.iter().copied();
    seats.find(|s| text(s, "session_id").is_some_and(|id| id.starts_with(&head)))
}

/// The far twin of `team_lead_name`: the seat itself when it holds a role,
/// else the first ancestor that does.
fn lead_of<'a>(seat: &'a Value, all: &[&'a Value]) -> String {
    let mut at = seat;
    for _ in 0..=all.len() {
        if text(at, "role").is_some() {
            return text(at, "name").unwrap_or_default();
        }
        match parent_seat(at, all) {
            Some(next) => at = next,
            None => break,
        }
    }
    "~ unteamed".into()
}

/// What a far card says the session is doing. A stale beat outranks the
/// seat's own word: the session may be fine, but this mux cannot see it.
fn seat_state(seat: &Value, band: &Band) -> String {
    let age = |s: i64| super::overlay_lines::humanize_age(Some(s.max(0) as u64));
    if !band.fresh {
        return format!("offline {}", age(band.age_s).trim());
    }
    if text(seat, "status").as_deref() == Some("writing") {
        return "working".into();
    }
    match seat["age_s"].as_i64() {
        Some(s) => format!("idle {}", age(s).trim()),
        None => "idle".into(),
    }
}

/// A fleet seat as a row. `name@host` is the address peek and mail already
/// route, so those two work from the name alone.
fn remote_row(seat: &Value, host: &str, state: String) -> AgentRow {
    let name = text(seat, "name").unwrap_or_default();
    let address = format!("{name}@{host}");
    AgentRow {
        name: address.clone(),
        harness: text(seat, "harness"),
        node: text(seat, "node"),
        // No `cwd_base`: the band names it, and a card would repeat it after
        // the name.
        model: text(seat, "model"),
        context_used_pct: seat["context_pct"].as_u64().map(|pct| pct.min(100) as u8),
        badge: match text(seat, "status").as_deref() {
            Some("writing") => Some(AgentBadge::Working),
            Some("blocked") => Some(AgentBadge::Blocked),
            _ => None,
        },
        // The Created and Modified sorts read these, so a far row orders by
        // its own times inside a band.
        started_at: stamp(seat, "started_at"),
        updated_at: stamp(seat, "last_message_at"),
        remote: Some(RemoteRow {
            host: host.to_string(),
            key: text(seat, "session_id").unwrap_or(name),
            name: address,
            state,
        }),
        ..AgentRow::default()
    }
}

/// An RFC 3339 field as epoch seconds.
fn stamp(seat: &Value, key: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(&text(seat, key)?).ok()?;
    Some(at.timestamp().max(0) as u64)
}

/// The band a far seat joins on the active axis, the local axis's own words.
fn seat_label(group: AgentGroup, seat: &Seat) -> String {
    match group {
        AgentGroup::Workspace => seat.project.clone(),
        AgentGroup::Team => seat.lead.clone(),
        AgentGroup::Cwd => seat.cwd.clone(),
        AgentGroup::Status if seat.working => "Working".into(),
        AgentGroup::Status => "Completed".into(),
        AgentGroup::Harness => (seat.row.harness.clone()).unwrap_or_else(|| "(no harness)".into()),
        AgentGroup::Flat => String::new(),
    }
}

fn band_label(band: &Band) -> String {
    let state = if band.fresh { "ready" } else { "offline" };
    format!("{} · {state} · {}s", band.host, band.age_s)
}

/// Another machine's rows on the active axis: a header per band, keyed by
/// host so folding one machine's band leaves the same band elsewhere open.
/// Under workspace, workers nest under their parent as this machine's
/// workspace tree nests them. Flat paints no header at all.
fn remote_rows<'a>(view: &'a View, band: &'a Band) -> (Vec<DisplayRow<'a>>, Vec<usize>) {
    let group = view.agent_group;
    let mut buckets: Vec<(String, Vec<&Seat>)> = Vec::new();
    for seat in &band.seats {
        let label = seat_label(group, seat);
        match buckets.iter_mut().find(|(k, _)| *k == label) {
            Some((_, seats)) => seats.push(seat),
            None => buckets.push((label, vec![seat])),
        }
    }
    // Status keeps its fixed order, Working before Completed; the rest read
    // alphabetically.
    buckets.sort_by(|a, b| match group {
        AgentGroup::Status => b.0.cmp(&a.0),
        _ => a.0.cmp(&b.0),
    });
    let (mut out, mut depths) = (Vec::new(), Vec::new());
    for (label, seats) in buckets {
        if group != AgentGroup::Flat {
            let key = SectionKey::Group(format!("@{}/{label}", band.host));
            let sview = view.section_view(&key);
            out.push(DisplayRow::Header {
                label: format!("  {label}"),
                rollup: section_rollup(seats.iter().map(|s| agent_lattice_state(&s.row))),
                key,
                view: sview,
            });
            depths.push(0);
            if sview == SectionView::Collapsed {
                continue;
            }
        }
        let (order, nest) = if group == AgentGroup::Workspace {
            lineage_layout(&seats, |s| s.id.as_deref(), |s| s.parent.as_deref())
        } else {
            ((0..seats.len()).collect(), vec![0; seats.len()])
        };
        for i in order {
            out.push(DisplayRow::Agent(&seats[i].row));
            depths.push(nest[i]);
        }
    }
    (out, depths)
}

impl View {
    /// The sideline tree: this machine's rows on the active axis, under a band
    /// per machine while the toggle is on and a second machine reports.
    pub(super) fn tree_rows_with_depths(&self) -> (Vec<DisplayRow<'_>>, Vec<usize>) {
        let tree = &self.machine_tree;
        if !tree.on || tree.bands.len() < 2 {
            return self.local_rows_with_depths();
        }
        let (mut out, mut depths) = (Vec::new(), Vec::new());
        for band in &tree.bands {
            let (rows, band_depths, states): (Vec<_>, Vec<_>, Vec<_>) = if band.local {
                let (rows, d) = self.local_rows_with_depths();
                // One indent under the machine, the same step the far bands take.
                let rows = rows.into_iter().map(|row| match row {
                    DisplayRow::Header {
                        label,
                        rollup,
                        key,
                        view,
                    } => {
                        let label = format!("  {label}");
                        DisplayRow::Header {
                            label,
                            rollup,
                            key,
                            view,
                        }
                    }
                    row => row,
                });
                let states = self.layout.agents.iter().map(agent_lattice_state).collect();
                (rows.collect(), d, states)
            } else {
                let (rows, d) = remote_rows(self, band);
                let states = band
                    .seats
                    .iter()
                    .map(|s| agent_lattice_state(&s.row))
                    .collect();
                (rows, d, states)
            };
            if !out.is_empty() {
                out.push(DisplayRow::Blank);
                depths.push(0);
            }
            let key = SectionKey::Group(format!("@{}", band.host));
            let sview = self.section_view(&key);
            out.push(DisplayRow::Header {
                label: band_label(band),
                rollup: section_rollup(states.into_iter()),
                key,
                view: sview,
            });
            depths.push(0);
            if sview != SectionView::Collapsed {
                out.extend(rows);
                depths.extend(band_depths);
            }
        }
        (out, depths)
    }
}

/// Show a far row in a portal, through the reach a local paneless row takes.
/// The server runs the link in the seat. A refusal lands as a notice here.
pub(super) async fn reach(
    view: &mut View,
    remote: RemoteRow,
    placement: PanePlacement,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if !super::wire_version::server_has_remote_reach(view.server_proto) {
        view.set_notice("update the mux server to open a row from another machine".into());
        return Ok(());
    }
    let command = Command::ReachRemote { remote, placement };
    view.note_command_sent(&command);
    write_msg(sock_w, &ClientMsg::Command(command))
        .await
        .map_err(|e| format!("open send failed: {e}"))
}

/// Enter on a far row: the portal a local row's Enter names.
pub(super) async fn open(
    view: &mut View,
    remote: RemoteRow,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    reach(view, remote, super::agent_group::enter_placement(), sock_w).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fleet() -> Value {
        json!({"machines": [
            {"host": "bb16", "local": false, "fresh": true, "age_s": 45, "sessions": [
                {"name": "muriel", "harness": "claude", "project": "fno",
                 "session_id": "133a419b", "status": "writing"},
                {"name": "ines", "harness": "claude", "project": "fnh", "age_s": 180},
                {"name": "shell", "harness": "zsh"},
            ]},
            {"host": "iMac2D2", "local": true, "fresh": true, "age_s": 3, "sessions": [
                {"name": "gavin", "harness": "claude", "project": "fno"},
            ]},
        ]})
    }

    #[test]
    fn machine_tree_folds_this_machine_first_with_remote_rows_addressed() {
        let bands = fold(&fleet());
        assert_eq!(bands[0].host, "iMac2D2");
        assert!(
            bands[0].seats.is_empty(),
            "this machine paints from the Layout"
        );
        assert_eq!(bands[1].seats.len(), 2, "a shell is not a seat");
        let row = &bands[1].seats[0].row;
        assert_eq!(row.name, "muriel@bb16");
        assert_eq!(row.badge, Some(AgentBadge::Working));
        let remote = row.remote.as_ref().expect("a remote row");
        assert_eq!(
            (remote.host.as_str(), remote.key.as_str()),
            ("bb16", "133a419b")
        );
        assert_eq!(remote.state, "working");
        let ines = bands[1].seats[1].row.remote.as_ref().unwrap();
        assert_eq!(
            (ines.key.as_str(), ines.state.as_str()),
            ("ines", "idle 3m")
        );

        let stale =
            json!({"host": "bb16", "age_s": 120, "sessions": [{"name": "x", "harness": "codex"}]});
        let others = json!({"machines": [stale, {"host": "mbp3"}]});
        let bands = fold(&others);
        assert!(bands[0].local, "this machine keeps a band with no beat");
        assert!(!bands[0].fresh);
        assert_eq!(bands.len(), 3);
        let x = bands[1].seats[0].row.remote.as_ref().unwrap();
        assert_eq!(
            x.state, "offline 2m",
            "a stale beat outranks the seat's word"
        );
    }
}

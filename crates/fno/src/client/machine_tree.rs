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

/// The status band for a session waiting on the user, in the local band's words.
const NEEDS_INPUT: &str = "Needs input";

/// Another machine's session: its row and the band each axis files it under.
#[derive(Debug)]
struct Seat {
    row: AgentRow,
    project: String,
    lead: String,
    cwd: String,
    /// The fleet map says `writing`.
    working: bool,
    /// The seat is blocked on the user, as its own machine's badge says, and
    /// that machine's beat is fresh enough to act on.
    needs_input: bool,
    /// The workspace its own machine's mux files it under. Absent when no mux
    /// there names one, and the project stands in.
    workspace: Option<String>,
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

    pub(super) fn destinations(&self) -> Vec<String> {
        self.bands
            .iter()
            .filter(|b| !b.local)
            .map(|b| b.host.clone())
            .collect()
    }

    /// Remote rows reading blocked. Zero while the toggle is off.
    pub(super) fn blocked_remote(&self) -> usize {
        if !self.on {
            return 0;
        }
        self.rows()
            .filter(|r| r.badge == Some(AgentBadge::Blocked))
            .count()
    }

    /// The layer paints only with the toggle on and a second machine reporting.
    pub(super) fn layered(&self) -> bool {
        self.on && self.bands.len() >= 2
    }

    /// The rows this tree painted, for a menu action to resolve at execution time.
    pub(super) fn rows(&self) -> impl Iterator<Item = &AgentRow> {
        self.bands
            .iter()
            .flat_map(|band| &band.seats)
            .map(|seat| &seat.row)
    }
}

/// The glyph form of a remote row's address: the label its portal seat pane
/// carries (`pane_label` leads a `name@host` registered name with the glyph).
pub(super) fn twin_label(remote_name: &str) -> String {
    format!("{} {remote_name}", crate::proto::REMOTE_GLYPH)
}

/// The local twin of a remote row: the pane whose registered name is the
/// glyph form of the row's address (`pane_label` leads a `name@host` name
/// with the glyph, so any seat of that session carries the label).
/// `None` when no such pane exists: the portal never opened, or closed.
pub(super) fn portal_twin<'a>(view: &'a View, remote: &RemoteRow) -> Option<&'a AgentRow> {
    let label = twin_label(&remote.name);
    view.layout
        .agents
        .iter()
        .find(|a| a.remote.is_none() && a.name == label)
}

/// Turning the toggle off drops the bands at once, so no remote row outlives it.
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
            // Captured against the old bands: the selector follows the row's
            // identity through the swap (any density), and a hover whose row
            // moved away clears instead of drifting onto the neighbor.
            let held = view.selected_agent_name();
            let hover_held = view.hover_row.and_then(|i| {
                view.display_rows().get(i).and_then(|r| match r {
                    DisplayRow::Agent(a) => Some(a.name.clone()),
                    _ => None,
                })
            });
            view.machine_tree.bands = fold(&map);
            if let Some(l) = view.launcher.as_mut() {
                l.machines = view.machine_tree.destinations();
            }
            view.reanchor_selector(held);
            if let Some(hr) = view.hover_row {
                let now_at = view.display_rows().get(hr).and_then(|r| match r {
                    DisplayRow::Agent(a) => Some(a.name.clone()),
                    _ => None,
                });
                if now_at != hover_held {
                    view.hover_row = None;
                }
            }
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
    let needs_input = band.fresh && seat["needs_input"].as_bool() == Some(true);
    Seat {
        row: remote_row(seat, host, needs_input),
        project: text(seat, "project").unwrap_or_else(|| "(no project)".into()),
        lead: lead_of(seat, all),
        cwd: cwd
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "(no cwd)".into()),
        working: text(seat, "status").as_deref() == Some("writing"),
        needs_input,
        workspace: text(seat, "workspace"),
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

/// The remote twin of `team_lead_name`: the seat itself when it holds a role,
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

/// A fleet seat as a row that paints like a local one: the same model, cost,
/// context, token and compaction fields, and the last line the session wrote.
/// A count its own machine has no reading for stays `None` and the card hides
/// it. `name@host` is the address peek and mail already route, so those two
/// work from the name alone. A seat waiting on the user badges blocked, so it
/// sorts and rolls up as a local blocked row does.
fn remote_row(seat: &Value, host: &str, needs_input: bool) -> AgentRow {
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
        session_cost_cents: seat["cost_cents"].as_u64(),
        session_tokens: seat["tokens"].as_u64(),
        compaction_count: seat["compactions"].as_u64(),
        tail: text(seat, "last_message"),
        badge: if needs_input {
            Some(AgentBadge::Blocked)
        } else {
            (text(seat, "status").as_deref() == Some("writing")).then_some(AgentBadge::Working)
        },
        pinned: seat["pinned"].as_bool() == Some(true),
        // The Created and Modified sorts read these, so a remote row orders by
        // its own times inside a band.
        started_at: stamp(seat, "started_at"),
        updated_at: stamp(seat, "last_message_at"),
        remote: Some(RemoteRow {
            host: host.to_string(),
            key: text(seat, "session_id").unwrap_or(name),
            name: address,
        }),
        ..AgentRow::default()
    }
}

/// An RFC 3339 field as epoch seconds.
fn stamp(seat: &Value, key: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(&text(seat, key)?).ok()?;
    Some(at.timestamp().max(0) as u64)
}

/// The band a remote seat joins on the active axis, the local axis's own words.
fn seat_label(group: AgentGroup, seat: &Seat) -> String {
    match group {
        AgentGroup::Workspace => (seat.workspace.clone()).unwrap_or_else(|| seat.project.clone()),
        AgentGroup::Team => seat.lead.clone(),
        AgentGroup::Cwd => seat.cwd.clone(),
        AgentGroup::Status if seat.needs_input => NEEDS_INPUT.into(),
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
    // Status keeps its fixed order, Needs input then Working then Completed,
    // the order a local status band takes; the rest read alphabetically.
    let status_rank = |label: &str| match label {
        NEEDS_INPUT => 0,
        "Working" => 1,
        _ => 2,
    };
    buckets.sort_by(|a, b| match group {
        AgentGroup::Status => status_rank(&a.0).cmp(&status_rank(&b.0)),
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
        if !tree.layered() {
            return self.local_rows_with_depths();
        }
        // One session, one row: a seat twin folds into its remote row, so
        // an open portal never shifts the rows a held highlight points at.
        let twins: std::collections::HashSet<String> = tree
            .bands
            .iter()
            .filter(|b| !b.local)
            .flat_map(|b| &b.seats)
            .map(|seat| twin_label(&seat.row.name))
            .collect();
        let (mut out, mut depths) = (Vec::new(), Vec::new());
        for band in &tree.bands {
            let (rows, band_depths, states): (Vec<_>, Vec<_>, Vec<_>) = if band.local {
                let (rows, d) = self.local_rows_with_depths();
                let (rows, d): (Vec<_>, Vec<_>) = rows
                    .into_iter()
                    .zip(d)
                    .filter(|(row, _)| {
                        !matches!(row, DisplayRow::Agent(a) if a.remote.is_none() && twins.contains(&a.name))
                    })
                    .unzip();
                // One indent under the machine, the same step the remote bands take.
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

/// Show a remote row in a portal, through the reach a local paneless row takes.
/// The server runs the link in the seat. A refusal lands as a notice here.
pub(super) async fn reach(
    view: &mut View,
    remote: RemoteRow,
    placement: PanePlacement,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if !super::wire_version::server_has_remote_reach(view.server_proto) {
        super::server_skew::offer_server_restart(view, "open a row from another machine");
        return Ok(());
    }
    let command = Command::ReachRemote { remote, placement };
    view.note_command_sent(&command);
    write_msg(sock_w, &ClientMsg::Command(command))
        .await
        .map_err(|e| format!("open send failed: {e}"))
}

/// Enter on a remote row: the portal a local row's Enter names.
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
        let ines = bands[1].seats[1].row.remote.as_ref().unwrap();
        assert_eq!(ines.key, "ines");

        let stale =
            json!({"host": "bb16", "age_s": 120, "sessions": [{"name": "x", "harness": "codex"}]});
        let others = json!({"machines": [stale, {"host": "mbp3"}]});
        let bands = fold(&others);
        assert!(bands[0].local, "this machine keeps a band with no beat");
        assert!(!bands[0].fresh);
        assert_eq!(bands.len(), 3);
        assert!(!bands[1].fresh, "a stale beat reads offline on its band");
    }
}

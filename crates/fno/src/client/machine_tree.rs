//! The sideline's Machine axis: machine band, project band, then rows.
//!
//! This machine's rows come from the server `Layout`. Every other machine's
//! rows come from the fleet map, read off the UI loop, and paint as ordinary
//! `AgentRow` values with `remote` set. No connection to the other machine
//! exists until Enter opens one row: a local pane whose child is
//! `fno mux serve --link <key> --host <host>`. With fewer than two machines the
//! axis paints the workspace tree unchanged.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::org_board::{OrgMsg, OrgTx};
use super::*;
use crate::mux_cli::control_roundtrip_with_timeouts;
use crate::proto::{ControlCommand, RemoteRow};

const READ_EVERY: Duration = Duration::from_secs(15);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// `fno`, when this process cannot name its own binary.
const FNO: &str = "fno";
const NO_PROJECT: &str = "(no project)";

/// One machine from the newest fleet read.
#[derive(Debug, Default)]
pub(super) struct Band {
    host: String,
    local: bool,
    fresh: bool,
    age_s: i64,
    /// Row name -> published project. For this machine, the join onto the
    /// `Layout` rows; for another, its rows' projects.
    projects: HashMap<String, String>,
    /// Another machine's rows. Empty for this machine.
    rows: Vec<AgentRow>,
}

#[derive(Debug, Default)]
pub(super) struct MachineTree {
    bands: Vec<Band>,
    inflight: bool,
    last_read: Option<Instant>,
}

impl MachineTree {
    /// The rows this tree painted, for a menu action to resolve at execution time.
    pub(super) fn rows(&self) -> impl Iterator<Item = &AgentRow> {
        self.bands.iter().flat_map(|band| &band.rows)
    }
}

/// True when the axis has only one machine to show. The fleet map answers one
/// machine with `store.remote_url` unset, and no read has landed at first paint.
pub(super) fn paints_workspace_tree(view: &View) -> bool {
    view.agent_group == AgentGroup::Machine && view.machine_tree.bands.len() < 2
}

/// The off-loop read, single-flight, only while the axis is on.
pub(super) fn maybe_kick(view: &mut View, tx: &OrgTx) {
    let tree = &mut view.machine_tree;
    let due = tree.last_read.is_none_or(|at| at.elapsed() >= READ_EVERY);
    if view.agent_group != AgentGroup::Machine || tree.inflight || !due {
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
/// primary blinked. An aged band already reads offline.
pub(super) fn apply(view: &mut View, read: Result<Value, String>) {
    view.machine_tree.inflight = false;
    match read {
        Ok(map) if map.get("error").is_none() => view.machine_tree.bands = fold(&map),
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
    bands.sort_by(|a, b| (!a.local, &a.host).cmp(&(!b.local, &b.host)));
    bands
}

fn fold_machine(machine: &Value) -> Band {
    let host = text(machine, "host").unwrap_or_else(|| "unknown host".into());
    let local = machine["local"].as_bool().unwrap_or(false);
    let mut band = Band {
        local,
        fresh: machine["fresh"].as_bool().unwrap_or(false),
        age_s: machine["age_s"].as_i64().unwrap_or(0),
        ..Band::default()
    };
    for seat in machine["sessions"].as_array().into_iter().flatten() {
        let Some(name) = text(seat, "name") else {
            continue;
        };
        if super::org_seats::is_shell(text(seat, "harness").as_deref()) {
            continue;
        }
        let project = text(seat, "project").unwrap_or_else(|| NO_PROJECT.into());
        if local {
            band.projects.insert(name, project);
        } else {
            let row = remote_row(seat, &name, &host);
            band.projects.insert(row.name.clone(), project);
            band.rows.push(row);
        }
    }
    band.host = host;
    band
}

/// A fleet seat as a row. `name@host` is the address peek and mail already
/// route, so those two work from the name alone.
fn remote_row(seat: &Value, name: &str, host: &str) -> AgentRow {
    let address = format!("{name}@{host}");
    AgentRow {
        name: address.clone(),
        harness: text(seat, "harness"),
        node: text(seat, "node"),
        cwd_base: text(seat, "project"),
        model: text(seat, "model"),
        context_used_pct: seat["context_pct"].as_u64().map(|pct| pct.min(100) as u8),
        badge: (text(seat, "status").as_deref() == Some("writing")).then_some(AgentBadge::Working),
        remote: Some(RemoteRow {
            host: host.to_string(),
            key: text(seat, "session_id").unwrap_or_else(|| name.to_string()),
            name: address,
        }),
        ..AgentRow::default()
    }
}

fn band_label(band: &Band) -> String {
    let state = if band.fresh { "ready" } else { "offline" };
    format!("{} · {state} · {}s", band.host, band.age_s)
}

/// A row's published project. A `Layout` row the fleet map has not seen yet
/// falls back to its directory.
fn local_project<'a>(band: &'a Band, row: &'a AgentRow) -> &'a str {
    band.projects.get(&row.name).map_or_else(
        || row.cwd_base.as_deref().unwrap_or(NO_PROJECT),
        String::as_str,
    )
}

/// The Machine axis's rows: a band per machine, a band per project inside it.
pub(super) fn rows_with_depths(view: &View) -> (Vec<DisplayRow<'_>>, Vec<usize>) {
    let mut out = Vec::new();
    for band in &view.machine_tree.bands {
        let members: Vec<&AgentRow> = if band.local {
            view.layout.agents.iter().collect()
        } else {
            band.rows.iter().collect()
        };
        if !out.is_empty() {
            out.push(DisplayRow::Blank);
        }
        let key = SectionKey::Group(band.host.clone());
        let sview = view.section_view(&key);
        out.push(DisplayRow::Header {
            label: band_label(band),
            rollup: section_rollup(members.iter().map(|a| agent_lattice_state(a))),
            key,
            view: sview,
        });
        if sview == SectionView::Collapsed {
            continue;
        }
        let mut projects: Vec<&str> = members.iter().map(|a| local_project(band, a)).collect();
        projects.sort_unstable();
        projects.dedup();
        for project in projects {
            let rows: Vec<&AgentRow> = (members.iter().copied())
                .filter(|a| local_project(band, a) == project)
                .collect();
            let key = SectionKey::Group(format!("{}/{project}", band.host));
            let pview = view.section_view(&key);
            out.push(DisplayRow::Header {
                label: format!("  {project}"),
                rollup: section_rollup(rows.iter().map(|a| agent_lattice_state(a))),
                key,
                view: pview,
            });
            if pview == SectionView::Collapsed {
                continue;
            }
            let shown = rows.into_iter();
            out.extend(
                shown
                    .filter(|a| pview == SectionView::Expanded || !a.exited)
                    .map(DisplayRow::Agent),
            );
        }
    }
    let depths = vec![0; out.len()];
    (out, depths)
}

/// The pane child that opens this row, under the `env` wrapper that names a
/// pane. Its argv is also how a later open finds the pane: see
/// [`find_link_pane`]. The name stays on this machine: ssh does not forward it,
/// and a pane with no registry row is not published to the fleet map.
fn link_argv(remote: &RemoteRow) -> Vec<String> {
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| FNO.into());
    let words = [
        "mux",
        "serve",
        "--link",
        &remote.key,
        "--host",
        &remote.host,
    ];
    ["env".into(), format!("FNO_AGENT_SELF={}", remote.name), exe]
        .into_iter()
        .chain(words.into_iter().map(String::from))
        .collect()
}

/// The pane already running this link, from each pane's child argv. Reading the
/// argv keeps the answer true across a client restart with no stored state.
fn find_link_pane(panes: &[(u64, String)], remote: &RemoteRow) -> Option<u64> {
    let tail = format!("--link {} --host {}", remote.key, remote.host);
    let running = |args: &String| args.ends_with(&tail);
    panes
        .iter()
        .find(|(_, args)| running(args))
        .map(|(id, _)| *id)
}

/// Child pid -> full argv from one `ps`, joined onto the pane list.
fn panes_with_args(panes: &[crate::proto::PaneInfo]) -> Vec<(u64, String)> {
    let pids: Vec<String> = panes
        .iter()
        .filter_map(|p| p.child_pid.map(|pid| pid.to_string()))
        .collect();
    if pids.is_empty() {
        return Vec::new();
    }
    let listing = std::process::Command::new("ps")
        .args(["-o", "pid=,args=", "-p", &pids.join(",")])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let mut by_pid = HashMap::new();
    for line in listing.lines() {
        if let Some((pid, args)) = line.trim().split_once(char::is_whitespace) {
            by_pid.insert(pid.to_string(), args.trim().to_string());
        }
    }
    panes
        .iter()
        .filter_map(|p| {
            let args = by_pid.get(&p.child_pid?.to_string())?;
            Some((p.pane_id, args.clone()))
        })
        .collect()
}

/// What Enter on a remote row found or opened, off the UI loop.
fn open_blocking(
    session: &str,
    sock: &Path,
    remote: &RemoteRow,
    cwd: String,
    placement: PanePlacement,
) -> Result<u64, String> {
    let control = |command| {
        control_roundtrip_with_timeouts(sock, session, command, CONTROL_TIMEOUT, CONTROL_TIMEOUT)
            .map_err(|e| e.to_string())
    };
    if let ServerMsg::PaneList { panes } = control(ControlCommand::PaneLs)? {
        if let Some(pane) = find_link_pane(&panes_with_args(&panes), remote) {
            return Ok(pane);
        }
    }
    let run = ControlCommand::PaneRun {
        cwd,
        argv: link_argv(remote),
        cols: None,
        rows: None,
        claim: false,
        placement,
        worker: None,
        fno_id: None,
    };
    match control(run)? {
        ServerMsg::PaneSpawned { pane_id, .. } => Ok(pane_id),
        ServerMsg::Err { msg, .. } => Err(msg),
        other => Err(format!("unexpected reply to pane run: {other:?}")),
    }
}

/// Open a remote row: focus its link pane, opening one first if needed. A
/// refusal lands as a notice on the row, in this mux.
pub(super) async fn open(
    view: &mut View,
    remote: RemoteRow,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let session = view.session.clone();
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = format!("{}/{}", remote.host, remote.key);
    let placement = PanePlacement {
        tab: Some(crate::proto::TabSel::New),
        target: PaneTarget::SquadId(view.layout.active_squad),
        ..PanePlacement::default()
    };
    let opened = tokio::task::spawn_blocking(move || {
        let sock = crate::proto::socket_path(&session)?;
        open_blocking(&session, &sock, &remote, cwd, placement)
    })
    .await
    .map_err(|e| format!("link open task failed: {e}"))?;
    match opened {
        Ok(pane) => {
            view.note_command_sent(&Command::FocusPane(pane));
            write_msg(sock_w, &ClientMsg::Command(Command::FocusPane(pane)))
                .await
                .map_err(|e| format!("focus send failed: {e}"))?;
        }
        Err(e) => view.set_notice(format!("{label}: {e}")),
    }
    Ok(())
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
                {"name": "ines", "harness": "claude", "project": "fnh"},
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
            bands[0].rows.is_empty(),
            "this machine paints from the Layout"
        );
        assert_eq!(bands[1].rows.len(), 2, "a shell is not a seat");
        let row = &bands[1].rows[0];
        assert_eq!(row.name, "muriel@bb16");
        assert_eq!(row.badge, Some(AgentBadge::Working));
        let remote = row.remote.as_ref().expect("a remote row");
        assert_eq!(
            (remote.host.as_str(), remote.key.as_str()),
            ("bb16", "133a419b")
        );
        assert_eq!(bands[1].rows[1].remote.as_ref().unwrap().key, "ines");
    }

    #[test]
    fn machine_tree_finds_an_open_link_by_its_child_argv() {
        let remote = RemoteRow {
            host: "bb16".into(),
            key: "133a419b".into(),
            name: "muriel@bb16".into(),
        };
        let panes = [
            (1, "/bin/zsh -l".to_string()),
            (2, "fno mux serve --link other --host bb16".to_string()),
            (3, "fno mux serve --link 133a419b --host bb16".to_string()),
        ];
        assert_eq!(find_link_pane(&panes, &remote), Some(3));
        assert_eq!(find_link_pane(&panes[..2], &remote), None);
        let argv = link_argv(&remote).join(" ");
        assert!(argv.starts_with("env FNO_AGENT_SELF=muriel@bb16 "));
        assert!(argv.ends_with("--link 133a419b --host bb16"));
    }
}

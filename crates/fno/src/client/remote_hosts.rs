//! Remote hosts: the ssh names the user told the mux about (`mux.remote_hosts`),
//! the one probe that says whether each answers, and the settings page that
//! lists them beside the machines the fleet map already knows. Sessions still
//! come only from the fleet map; a host here that never beats the shared store
//! reads "not on the shared store" instead of vanishing.

use std::time::Duration;

use super::input_field::{back_row, InputField};
use super::machine_tree::MachineTree;
use super::org_board::{OrgMsg, OrgTx};
use super::*;

const PROBE_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_INPUT: usize = 256;
const CONFIG_KEY: &str = "mux.remote_hosts";
const REFUSED_PATH: &str =
    "a socket path is not a host here; type a tailnet name, a host or user@host";

/// What one probe learned about a configured host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HostState {
    Checking,
    /// ssh answered and `fno --version` ran; carries its first output line.
    Reachable(String),
    Unreachable(String),
}

#[derive(Debug, Clone)]
struct Host {
    name: String,
    state: HostState,
    started: bool,
}

/// A row action on the Remote hosts page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    Add,
    /// Enter on a host row: say why it reads as it does. On a configured
    /// host a second Enter removes it.
    Pick(String),
}

#[derive(Debug, Default)]
pub(super) struct RemoteHosts {
    hosts: Vec<Host>,
    /// The Add field, open while the user types a host.
    pub(super) add: Option<InputField>,
    /// The configured host the last Enter named, awaiting a second to remove.
    armed: Option<String>,
}

/// One line of the page: a configured host or a machine only the map knows.
struct Line {
    name: String,
    word: String,
    detail: String,
    configured: bool,
}

/// A host the field accepts: `name`, `user@name`, or an `https://name[:port]/`
/// URL, which gives its host. Anything ssh would read as an option, and any
/// path, is refused with the forms that work.
pub(super) fn parse_host(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("type a tailnet name, a host or user@host".into());
    }
    if input.starts_with(['/', '~', '.']) {
        return Err(REFUSED_PATH.into());
    }
    if input.starts_with('-') {
        return Err("a host cannot start with '-'".into());
    }
    let bare = ["https://", "http://"]
        .iter()
        .find_map(|scheme| input.strip_prefix(scheme))
        .unwrap_or(input);
    let authority = bare.split(['/', '?', '#']).next().unwrap_or_default();
    let (user, host) = match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, authority),
    };
    let host = host.split(':').next().unwrap_or_default();
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if !ok(host) || user.is_some_and(|u| !ok(u)) || host.starts_with('-') {
        return Err("type a tailnet name, a host or user@host".into());
    }
    Ok(match user {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    })
}

/// Whether a configured name and a fleet-map host name one machine: the
/// `user@` and the domain suffix do not count.
fn same_host(configured: &str, band: &str) -> bool {
    let head = |s: &str| {
        s.rsplit('@')
            .next()
            .unwrap_or_default()
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    !band.is_empty() && head(configured) == head(band)
}

impl RemoteHosts {
    /// The hosts in the merged config, each waiting for its first probe.
    pub(super) fn load() -> Self {
        let config = crate::config_cli::merged_config(&crate::config_cli::repo_root());
        let names = crate::config_cli::raw_get(&config, CONFIG_KEY)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        Self::from_names(names)
    }

    fn from_names(names: Vec<String>) -> Self {
        let hosts = names
            .into_iter()
            .map(|name| Host {
                name,
                state: HostState::Checking,
                started: false,
            })
            .collect();
        Self {
            hosts,
            ..Self::default()
        }
    }

    fn names(&self) -> Vec<String> {
        self.hosts.iter().map(|h| h.name.clone()).collect()
    }

    /// The fleet map's destinations plus every configured host whose probe
    /// answered, each machine once.
    pub(super) fn destinations(&self, mut map: Vec<String>) -> Vec<String> {
        for host in &self.hosts {
            let known = map.iter().any(|m| same_host(&host.name, m));
            if !known && matches!(host.state, HostState::Reachable(_)) {
                map.push(host.name.clone());
            }
        }
        map
    }

    fn lines(&self, tree: &MachineTree) -> Vec<Line> {
        let far: Vec<_> = tree.far_bands().collect();
        let mut lines: Vec<Line> = self
            .hosts
            .iter()
            .map(|host| {
                let band = far.iter().find(|(h, ..)| same_host(&host.name, h));
                let (word, detail) = match (band, &host.state) {
                    (Some(&(_, fresh, age_s)), _) => beat(fresh, age_s),
                    (None, HostState::Checking) => ("checking".into(), "ssh has not answered yet".into()),
                    (None, HostState::Reachable(v)) => (
                        "not on the shared store".into(),
                        format!(
                            "ssh answers ({v}), but {} publishes no beat to the shared store; set store.remote_url there",
                            host.name
                        ),
                    ),
                    (None, HostState::Unreachable(why)) => {
                        ("couldn't connect".into(), why.clone())
                    }
                };
                Line {
                    name: host.name.clone(),
                    word,
                    detail,
                    configured: true,
                }
            })
            .collect();
        for (host, fresh, age_s) in far {
            if !self.hosts.iter().any(|h| same_host(&h.name, host)) {
                let (word, _) = beat(fresh, age_s);
                lines.push(Line {
                    name: host.to_string(),
                    word,
                    detail: "on the shared store; not in mux.remote_hosts".into(),
                    configured: false,
                });
            }
        }
        lines
    }
}

fn beat(fresh: bool, age_s: i64) -> (String, String) {
    if fresh {
        ("live".into(), "live on the shared store".into())
    } else {
        let word = format!("beat {}m ago", (age_s / 60).max(1));
        (word.clone(), format!("last {word}"))
    }
}

/// The settings tab's rows: the add field when it is open, else one row per
/// host and the add row.
pub(super) fn rows(view: &View) -> (Vec<PopupRow>, Vec<AuxAction>) {
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    let inert = |label: &str| PopupRow::Entry {
        glyph: " ".into(),
        label: label.into(),
        hint: String::new(),
        enabled: false,
    };
    if let Some(field) = &view.remote_hosts.add {
        rows.push(back_row());
        actions.push(AuxAction::SettingsBack);
        rows.push(field.row("host"));
        rows.push(inert("a tailnet name, a host or user@host"));
        rows.push(inert(
            "https://name.tailnet.ts.net works too; enter connects",
        ));
        return (rows, actions);
    }
    rows.push(PopupRow::Header("remote hosts".into()));
    for line in view.remote_hosts.lines(&view.machine_tree) {
        rows.push(PopupRow::Entry {
            glyph: if line.word == "live" { "●" } else { "○" }.into(),
            label: format!("{} · {}", line.name, line.word),
            hint: String::new(),
            enabled: true,
        });
        actions.push(AuxAction::RemoteHost(Act::Pick(line.name)));
    }
    rows.push(PopupRow::Entry {
        glyph: "+".into(),
        label: "add remote host".into(),
        hint: String::new(),
        enabled: true,
    });
    actions.push(AuxAction::RemoteHost(Act::Add));
    (rows, actions)
}

/// Open settings on the Remote hosts tab with the add field armed: the
/// footer button and its key.
pub(super) fn open_add(view: &mut View) {
    view.lane.reset();
    theme_import_ui::reset(view);
    view.settings_tab = SettingsTab::RemoteHosts;
    view.remote_hosts.add = Some(InputField::new(
        "tailnet name, host or user@host",
        MAX_INPUT,
    ));
    view.aux = Some(view.build_settings_modal());
    view.aux_esc.clear();
}

/// Close the add field. True when one was open.
pub(super) fn cancel_add(view: &mut View) -> bool {
    view.remote_hosts.add.take().is_some()
}

/// Enter in the add field: parse, save, probe, and turn the machine layer on.
/// True ends the chunk.
pub(super) async fn submit(view: &mut View, input: String) -> bool {
    let name = match parse_host(&input) {
        Ok(name) => name,
        Err(reason) => {
            view.set_notice(reason);
            return true;
        }
    };
    view.remote_hosts.add = None;
    let hosts = &mut view.remote_hosts.hosts;
    if !hosts.iter().any(|h| h.name == name) {
        hosts.push(Host {
            name: name.clone(),
            state: HostState::Checking,
            started: false,
        });
    }
    let saved = save(view).await;
    if !view.machine_tree.on {
        super::machine_tree::set_on(view, true);
        crate::view_store::save_experimental(crate::view_store::EXPERIMENTAL_OTHER_MACHINES, true);
    }
    view.set_notice(match saved {
        Ok(()) => format!("{name}: checking"),
        Err(_) => format!("{name}: added this session; save failed"),
    });
    true
}

async fn save(view: &View) -> Result<(), String> {
    let json = serde_json::to_string(&view.remote_hosts.names()).map_err(|e| e.to_string())?;
    config_set::spawn_config_set_global(CONFIG_KEY, &json).await
}

/// Run one row action.
pub(super) async fn act(view: &mut View, act: Act) {
    match act {
        Act::Add => {
            view.remote_hosts.add = Some(InputField::new(
                "tailnet name, host or user@host",
                MAX_INPUT,
            ));
        }
        Act::Pick(name) => {
            let lines = view.remote_hosts.lines(&view.machine_tree);
            let Some(line) = lines.iter().find(|l| l.name == name) else {
                return;
            };
            if !line.configured {
                view.set_notice(format!("{name}: {}", line.detail));
            } else if view.remote_hosts.armed.as_deref() == Some(name.as_str()) {
                view.remote_hosts.armed = None;
                view.remote_hosts.hosts.retain(|h| h.name != name);
                let saved = save(view).await;
                view.set_notice(match saved {
                    Ok(()) => format!("{name}: removed"),
                    Err(_) => format!("{name}: removed this session; save failed"),
                });
            } else {
                view.set_notice(format!("{name}: {}. Enter again to remove", line.detail));
                if let Some(h) = view.remote_hosts.hosts.iter_mut().find(|h| h.name == name) {
                    if matches!(h.state, HostState::Unreachable(_)) {
                        h.state = HostState::Checking;
                        h.started = false;
                    }
                }
                view.remote_hosts.armed = Some(name);
            }
        }
    }
    view.reopen_settings_keeping_sel();
}

/// The snapshot seam: two configured hosts in opposite states, so the shot
/// shows both words. The demo root configures no hosts of its own.
#[cfg(test)]
pub(super) fn snapshot_seam(view: &mut View) {
    view.remote_hosts = Self::from_names(vec!["r2d2".into(), "nohost".into()]);
    view.remote_hosts.hosts[0].state = HostState::Reachable("fno 0.4.7".into());
    view.remote_hosts.hosts[1].state =
        HostState::Unreachable("could not reach nohost: Connection refused".into());
    for host in &mut view.remote_hosts.hosts {
        host.started = true;
    }
    view.settings_tab = SettingsTab::RemoteHosts;
}

/// Start one probe per host that has none in flight, off the UI loop.
pub(super) fn kick(view: &mut View, tx: &OrgTx) {
    for host in view.remote_hosts.hosts.iter_mut().filter(|h| !h.started) {
        host.started = true;
        let (name, tx) = (host.name.clone(), tx.clone());
        tokio::spawn(async move {
            let state = probe(&name).await;
            let _ = tx.send((0, OrgMsg::HostProbe(name, state)));
        });
    }
}

/// A probe landed.
pub(super) fn apply(view: &mut View, name: String, state: HostState) {
    if let Some(host) = view.remote_hosts.hosts.iter_mut().find(|h| h.name == name) {
        host.state = state;
    }
    if let Some(l) = view.launcher.as_mut() {
        l.machines = view
            .remote_hosts
            .destinations(view.machine_tree.destinations());
    }
    if view.aux.is_some() && view.settings_tab == SettingsTab::RemoteHosts {
        view.reopen_settings_keeping_sel();
    }
}

/// The ssh argv for one probe: the same fast options as the plan carrier, and
/// the host after `--` so a name can never read as an option.
fn probe_argv(host: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "--"]
        .map(String::from)
        .into();
    argv.push(host.to_string());
    argv.push("sh -lc 'exec fno --version'".into());
    argv
}

/// One bounded ssh run: `fno --version` on the far side, in the words
/// `fno-agents attach --dial` uses for a failed hop.
async fn probe(host: &str) -> HostState {
    let mut command = crate::process_admission::tokio_command("ssh");
    command
        .args(probe_argv(host))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let run = crate::process_admission::tokio_output(&mut command);
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            HostState::Reachable(text.lines().next().unwrap_or("fno").trim().to_string())
        }
        Ok(Ok(out)) => {
            let err = String::from_utf8_lossy(&out.stderr);
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            HostState::Unreachable(match out.status.code() {
                Some(255) => format!("could not reach {host}: {}", last.trim()),
                _ => format!(
                    "{host} answers ssh, but fno did not run there: {}",
                    last.trim()
                ),
            })
        }
        Ok(Err(e)) => HostState::Unreachable(format!("ssh did not start: {e}")),
        Err(_) => HostState::Unreachable(format!("could not reach {host}: timed out")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_takes_names_users_and_urls() {
        assert_eq!(parse_host("r2d2").as_deref(), Ok("r2d2"));
        assert_eq!(parse_host(" bb16@imac2d2 ").as_deref(), Ok("bb16@imac2d2"));
        assert_eq!(
            parse_host("https://r2d2.tail1234.ts.net:8443/x").as_deref(),
            Ok("r2d2.tail1234.ts.net")
        );
    }

    #[test]
    fn parse_refuses_paths_options_and_junk() {
        assert_eq!(parse_host("/tmp/sock"), Err(REFUSED_PATH.to_string()));
        assert_eq!(parse_host("~/sock"), Err(REFUSED_PATH.to_string()));
        assert!(parse_host("-oProxyCommand=x").is_err());
        assert!(parse_host("a b").is_err());
        assert!(parse_host("").is_err());
        assert!(parse_host("user@-host").is_err());
        // What parse admits, the probe fences: the host rides after `--`, so
        // an ssh option can never read from it.
        let argv = probe_argv("r2d2");
        let fence = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[fence + 1], "r2d2");
        assert!(argv.iter().any(|a| a == "BatchMode=yes"));
    }

    #[test]
    fn same_host_ignores_user_and_domain() {
        assert!(same_host("bb16@R2D2.tail1.ts.net", "r2d2"));
        assert!(!same_host("r2d2", "imac2d2"));
        assert!(!same_host("r2d2", ""));
    }

    #[test]
    fn destinations_add_only_reachable_hosts_not_on_the_map() {
        let mut hosts = RemoteHosts::from_names(vec!["r2d2".into(), "nohost".into(), "mac".into()]);
        hosts.hosts[0].state = HostState::Reachable("fno 1".into());
        hosts.hosts[1].state = HostState::Unreachable("down".into());
        hosts.hosts[2].state = HostState::Reachable("fno 1".into());
        let got = hosts.destinations(vec!["mac.local".into()]);
        assert_eq!(got, vec!["mac.local".to_string(), "r2d2".to_string()]);
    }

    #[test]
    fn page_lines_name_each_state() {
        let mut hosts = RemoteHosts::from_names(vec!["r2d2".into(), "nohost".into()]);
        hosts.hosts[0].state = HostState::Reachable("fno 1".into());
        hosts.hosts[1].state = HostState::Unreachable("could not reach nohost: no route".into());
        let lines = hosts.lines(&MachineTree::default());
        assert_eq!(lines[0].word, "not on the shared store");
        assert_eq!(lines[1].word, "couldn't connect");
        assert!(lines[1].detail.contains("no route"));
        // The beat words are the page's words: live, or the band's age.
        assert_eq!(beat(true, 0).0, "live");
        assert_eq!(beat(false, 360).0, "beat 6m ago");
    }
}

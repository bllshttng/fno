//! The org panel: what the machine is holding, and the cap that decides
//! whether another lane fits.
//!
//! The operator asked for this and said the part that matters: "I just don't
//! know what our cap even is." The cap is the fleet's share of CPU capacity
//! (`agents.max_fleet_cpu_share`), and `fno doctor lanes` already
//! knows it. This module is the SURFACE for a shipped advisor, never a second
//! capacity model: it folds one payload and renders it, and no arithmetic
//! here decides anything.
//!
//! The module owns the whole panel - the bounded fold, the cached state, and
//! the render - so the client keeps only the wiring: one field, one channel,
//! one compose branch. The fold runs off the UI loop on a spawned task and
//! reports back over that channel, so a slow `fno` never blocks the overlay
//! from opening.

use crate::proto::AgentRow;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Five seconds against a probe that measured 0.58s on the real fleet: the
/// budget holds with room, and a hung probe must never hold a paint.
const GATE_TIMEOUT: Duration = Duration::from_secs(5);

/// The org half rides the org board's own cadence: one `fno-agents org-fold`
/// child a minute while the block is expanded, never on the paint path.
pub const ORG_TTL: Duration = Duration::from_secs(60);

/// Ten seconds, and the number is MEASURED rather than reasoned.
///
/// `time fno doctor lanes --json` on this machine at 1-minute load 107:
/// **8.07s**. The verb's own `macmon` sample is bounded at 5.0s, so a read
/// over five seconds is its designed worst case, not a fault. A budget under
/// that guarantees a degrade exactly when the machine is busy, which is the
/// only time a person opens this panel. The plan for this change proposed
/// 2500ms on reasoning, and it failed against the live verb on the first
/// run. Setting a budget against a read nobody timed is the defect this
/// whole change was filed about.
///
/// A long budget costs nothing here. The fold runs off the UI loop, one at a
/// time, and the overlay opens on the keypress whether or not it has landed.
/// `kill_on_drop` reaps the child on the timeout, so an overrun cannot leak
/// a Python process.
const SHELLOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// Fifteen seconds, not the yard's sixty. The yard renders identity, which
/// does not move; this renders load, which does.
///
/// The floor is the same measurement: a TTL under the read's own 8.07s cost
/// means every open refetches and every reading is already stale on arrival,
/// which is a busy loop wearing a cache's clothes. A reading older than this
/// still RENDERS, with its age and the word `stale`, never as `unknown`. An
/// operator watching `unknown` every second learns nothing and reaches for
/// `--force`, which is how a guard becomes a formality.
pub const CACHE_TTL: Duration = Duration::from_secs(15);

/// The minimized block is exactly five glance lines: cpu, limit, memory,
/// team, lanes. The client reserves this many sideline rows for it, so the
/// height the layout subtracts and the height the painter draws cannot
/// drift.
pub const MINIMIZED_ROWS: usize = 5;

/// Readings each sparkline keeps: twelve folds at the TTL is three minutes.
const HISTORY: usize = 12;

/// The simple glance's second line: where the agents stand against the
/// limit that decides whether another worker starts.
fn simple_limit_line(org: &Org) -> String {
    match (
        org.arm_num("cpu admission", "share_low"),
        org.arm_num("cpu admission", "ceiling"),
    ) {
        (Some(low), Some(ceiling)) if low > ceiling => format!(
            "  agents    over their {:.0}% limit, so new workers wait",
            ceiling * 100.0
        ),
        (Some(_), Some(ceiling)) => {
            format!("  agents    under their {:.0}% limit", ceiling * 100.0)
        }
        _ => "  agents    limit unknown".to_string(),
    }
}

/// A history as one bar per fold, on a fixed 0-100% scale so a flat low
/// line never stretches into a full-height graph.
fn spark(history: &[f64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    history
        .iter()
        .map(|b| BARS[(b.clamp(0.0, 1.0) * 7.0).round() as usize])
        .collect()
}

/// The glance's team line: leads first, then the workers by how recently
/// each one did something. A lead count that failed to read is left out,
/// never shown as zero.
fn census_line(org: &Org, ages: &[Option<u64>]) -> String {
    let mut parts = Vec::new();
    if let Some(leads) = org.census.leads {
        parts.push(format!("{leads} leads"));
    }
    if !ages.is_empty() {
        parts.push(census_split(ages).render());
    }
    if parts.is_empty() {
        "  team      no workers held".to_string()
    } else {
        format!("  team      {}", parts.join(" · "))
    }
}

/// The census block: leads are a ROW count, tests is a PROCESS count. `None`
/// is a read that failed, and it renders as `unknown` rather than as a zero:
/// an operator who reads a fabricated zero as headroom is exactly the
/// failure the panel exists to prevent. The worker total is NOT here - the
/// panel derives it from the row ages it already holds, so one source
/// answers "how many are working".
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Census {
    #[serde(default)]
    pub leads: Option<u32>,
    #[serde(default)]
    pub lead_conflicts: Option<u32>,
    #[serde(default)]
    pub workers: Option<u32>,
    #[serde(default)]
    pub tests: Option<u32>,
    #[serde(default)]
    pub roster_rows: Option<u32>,
    /// Set when live rows could not be attributed to processes. The fleet CPU
    /// share is then an UNDERCOUNT, never headroom. It qualifies the
    /// CPU reading and is never folded into the counts above.
    #[serde(default)]
    pub attribution_gap: Option<String>,
    /// The caller's own share reading, from the one function the spawn gate
    /// refuses on (AC3). `None` when the census could not read it.
    #[serde(default)]
    pub share: Option<ShareReading>,
    /// Top fleet consumers by program name, aggregated from the ps read the
    /// lanes fold already performs. `None` when the footprint went dark.
    #[serde(default)]
    pub top_consumers: Option<Vec<TopConsumer>>,
    #[serde(default)]
    pub read_ms: Option<u64>,
}

/// The caller's own share reading, produced by the same Python
/// function the spawn gate refuses on. `None` counts are a failed read and
/// render `unknown`, never zero.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ShareReading {
    #[serde(default)]
    pub leads: Option<u32>,
    #[serde(default)]
    pub share: Option<u32>,
    #[serde(default)]
    pub held: Option<u32>,
    #[serde(default)]
    pub unattributed: Option<Unattributed>,
}

/// The live rows that name nobody (LD4): one named bucket, count plus
/// row names. They divide nothing and pay no lead's tax.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Unattributed {
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub rows: Vec<String>,
}

/// One program in the expanded panel's `top` block: name, process count,
/// summed ps `%cpu`, and the worktree the most of its rows run from.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TopConsumer {
    pub name: String,
    #[serde(default)]
    pub procs: u32,
    #[serde(default)]
    pub cpu_pct: f64,
    #[serde(default)]
    pub worktree: Option<String>,
    #[serde(default)]
    pub worktree_procs: u32,
}

/// The activity census, bucketed from the transcript-derived age the daemon
/// already stamps on every row it hands the client. Under five minutes is
/// working; five minutes to two hours idle; two to eight hours stale; past
/// eight hours dead, the reap backlog. A row with no age is its own bucket
/// and is never folded into a live one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CensusSplit {
    pub working: u32,
    pub idle: u32,
    pub stale: u32,
    pub dead: u32,
    pub unknown_age: u32,
}

const WORKING_MAX_S: u64 = 5 * 60;
const IDLE_MAX_S: u64 = 2 * 3600;
const STALE_MAX_S: u64 = 8 * 3600;

/// The working cutoff, shared with the team census so "working" keeps one
/// meaning on every line that says it.
pub fn working_max_s() -> u64 {
    WORKING_MAX_S
}

pub fn census_split(ages: &[Option<u64>]) -> CensusSplit {
    let mut split = CensusSplit::default();
    for age in ages {
        match age {
            None => split.unknown_age += 1,
            Some(s) if *s < WORKING_MAX_S => split.working += 1,
            Some(s) if *s < IDLE_MAX_S => split.idle += 1,
            Some(s) if *s < STALE_MAX_S => split.stale += 1,
            Some(_) => split.dead += 1,
        }
    }
    split
}

impl CensusSplit {
    /// The buckets that hold someone, joined for render. A zero bucket is
    /// omitted: on an all-unknown roster nothing may read as live headroom.
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (count, label) in [
            (self.working, "working"),
            (self.idle, "idle"),
            (self.stale, "stale"),
            (self.dead, "dead"),
            (self.unknown_age, "unknown age"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {label}"));
            }
        }
        parts.join(" · ")
    }
}

/// One arm of the lane answer, as emitted by `fno doctor lanes --json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Arm {
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default)]
    pub reason: String,
}

/// One whole reading. `lane_count` is `None` on a refusal, and then
/// `refused_reason` carries the advisor's own words - which the panel
/// repeats verbatim rather than summarizing, because a dark sensor is not
/// headroom and only the advisor knows which sensor went dark.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Org {
    #[serde(default)]
    pub lane_count: Option<u32>,
    #[serde(default)]
    pub per_lane_cpu_cores: Option<f64>,
    #[serde(default)]
    pub per_lane_mem_gb: Option<f64>,
    #[serde(default)]
    pub cost_source: String,
    #[serde(default)]
    pub refused_reason: String,
    #[serde(default)]
    pub census: Census,
    #[serde(default)]
    pub arms: Vec<Arm>,
}

impl Org {
    /// The named arm, if the payload carried it.
    pub fn arm(&self, name: &str) -> Option<&Arm> {
        self.arms.iter().find(|a| a.name == name)
    }

    /// A numeric field of a measured arm's value object.
    pub fn arm_num(&self, name: &str, field: &str) -> Option<f64> {
        self.arm(name)?.value.get(field)?.as_f64()
    }

    /// A string field of a measured arm's value object.
    pub fn arm_str(&self, name: &str, field: &str) -> Option<&str> {
        self.arm(name)?.value.get(field)?.as_str()
    }
}

/// One registry row the spawn-gate probe names, enough for the census's
/// managed-membership read.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct SlotRow {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// The spawn-gate probe's answer, read for the team census. Every field is
/// optional: an older or refusing answer degrades to what it carries, and
/// `slots` absent means the reading never named its occupancy, so the
/// render says unknown rather than a fabricated zero.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct GateReading {
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub slots: Option<u32>,
    #[serde(default)]
    pub max_live: Option<u32>,
    #[serde(default)]
    pub slot_rows: Option<Vec<SlotRow>>,
    #[serde(default)]
    pub lanes: Option<Value>,
    #[serde(default)]
    pub tests: Option<Value>,
}

impl GateReading {
    /// True when the answer named its occupancy at all: a refusal or an
    /// unread probe answers the team line as unknown, never as zero live.
    pub fn usable(&self) -> bool {
        self.slots.is_some()
    }
}

/// One landed fold half. Each half keeps its own stamp, so a slow `fno
/// doctor lanes` never holds the probe's team answer, and a failed org read
/// never blanks the fleet line.
#[derive(Debug, Clone)]
pub enum FoldPart {
    Lanes(Option<Org>),
    Gate(Result<GateReading, String>),
    Org(crate::org_model::OrgInputs),
}

/// Which halves are due, from one `take_want` call. `agents` rides only
/// when the org half is due: it is the roster the org fold needs, and the
/// other halves read nothing from it.
#[derive(Debug, Default)]
pub struct Want {
    pub lanes: bool,
    pub gate: bool,
    pub org: bool,
    pub agents: Vec<AgentRow>,
}

/// One count, rendered so an unread number says so. `unknown` is printed
/// ONLY where the read genuinely failed; a fabricated zero read as headroom
/// is the failure this panel exists to prevent.
fn count_or_unknown(value: Option<u32>) -> String {
    value.map_or_else(|| "unknown".to_string(), |v| v.to_string())
}

/// The panel's whole state. The block is painted on every frame now, so
/// there is no open/close: `expanded` is the only operator-facing state.
/// The fold runs ONLY while expanded (a 41s `fno doctor
/// lanes` child every TTL for a block nobody opened is the mux client's
/// largest child-CPU line), one in flight; a minimized block renders its
/// last reading stale-honest and an expand arms the refresh from the
/// deadline, never a fold of its own.
#[derive(Debug, Default)]
pub struct Panel {
    expanded: bool,
    fold: Option<Org>,
    fold_at: Option<Instant>,
    /// A failed fold retries one TTL out, not on the next loop pass: with
    /// the block painted every frame, an instantly-failing fold (a missing
    /// `fno` on PATH) would otherwise become a hot refetch loop.
    retry_at: Option<Instant>,
    degraded: bool,
    inflight: bool,
    /// The spawn-gate half: the team census's occupancy, lanes and tests.
    /// Unlike the lanes fold it runs while minimized too: the glance shows
    /// team and lanes, and the probe is a 0.6s child, not an 8s one.
    gate: Option<GateReading>,
    gate_at: Option<Instant>,
    gate_retry_at: Option<Instant>,
    gate_inflight: bool,
    gate_error: Option<String>,
    /// The org half: the Lead tree the per-Lead lines render from.
    org_tree: Option<crate::org_model::OrgTree>,
    org_at: Option<Instant>,
    org_retry_at: Option<Instant>,
    org_inflight: bool,
    org_error: Option<String>,
    /// `config.mux.load_readout = "detailed"`: the raw numbers. Off (the
    /// default) is plain words.
    detailed: bool,
    /// Whole-machine busy fraction per landed fold, oldest first.
    busy_history: Vec<f64>,
    /// Memory used fraction per landed fold, oldest first.
    mem_history: Vec<f64>,
}

impl Panel {
    pub fn with_detail(detailed: bool) -> Self {
        Self {
            detailed,
            ..Self::default()
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Expand in place, or collapse back to the four-line glance. A pure
    /// render toggle: the cached reading survives, so no expand pays the
    /// slow fold again.
    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    /// The timer wake for the next refresh, or `None` while a fold is
    /// already running or the panel is minimized: a past-due deadline would
    /// re-fire every pass and the single-flight refusal would just burn
    /// loop passes until it lands. `now` when no reading has ever landed
    /// (the first fold waits for the first expand, then must not wait for
    /// a further event), the retry backoff after a failure, else the last
    /// landing plus the TTL.
    pub fn refresh_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut due: Option<Instant> = None;
        // The retry backoff WINS over a stale reading's due time: otherwise a
        // failed refresh over an already-stale reading leaves a past-due
        // deadline, and the timer branch busy-spins until the backoff ends.
        if self.expanded && !self.inflight {
            let lanes_due = match self.fold_at {
                Some(t) => t + CACHE_TTL,
                None => now,
            };
            due = Some(self.retry_at.map_or(lanes_due, |r| r.max(lanes_due)));
        }
        if !self.gate_inflight {
            let gate_due = match self.gate_at {
                Some(t) => t + CACHE_TTL,
                None => now,
            };
            let gate_due = self.gate_retry_at.map_or(gate_due, |r| r.max(gate_due));
            due = Some(match due {
                Some(d) => d.min(gate_due),
                None => gate_due,
            });
        }
        if self.expanded && !self.org_inflight {
            let org_due = match self.org_at {
                Some(t) => t + ORG_TTL,
                None => now,
            };
            let org_due = self.org_retry_at.map_or(org_due, |r| r.max(org_due));
            due = Some(match due {
                Some(d) => d.min(org_due),
                None => org_due,
            });
        }
        due
    }

    /// Arm and consume the refresh want: `Some` exactly when at least one
    /// fold half should spawn now, each half single-flight on its own stamp
    /// and its own retry backoff. The lanes and org halves are the expanded
    /// block's cost; the gate half always runs, because the minimized
    /// glance renders team and lanes from it and the probe is a 0.6s
    /// child, not an 8s one.
    pub fn take_want(&mut self, agents: &[AgentRow]) -> Option<Want> {
        let now = Instant::now();
        let mut want = Want::default();
        if self.expanded
            && !self.inflight
            && !self
                .fold_at
                .is_some_and(|t| now.duration_since(t) < CACHE_TTL)
            && !self.retry_at.is_some_and(|t| now < t)
        {
            want.lanes = true;
            self.inflight = true;
        }
        if !self.gate_inflight
            && !self
                .gate_at
                .is_some_and(|t| now.duration_since(t) < CACHE_TTL)
            && !self.gate_retry_at.is_some_and(|t| now < t)
        {
            want.gate = true;
            self.gate_inflight = true;
        }
        if self.expanded
            && !self.org_inflight
            && !self.org_at.is_some_and(|t| now.duration_since(t) < ORG_TTL)
            && !self.org_retry_at.is_some_and(|t| now < t)
        {
            want.org = true;
            want.agents = agents.to_vec();
            self.org_inflight = true;
        }
        (want.lanes || want.gate || want.org).then_some(want)
    }

    /// Merge one landed fold half. A FAILED half never stamps its landing,
    /// so the age words keep describing the last real reading, and the
    /// failure arms that half's retry one TTL out instead of letting the
    /// want re-arm instantly into a hot loop.
    pub fn apply(&mut self, part: FoldPart) {
        match part {
            FoldPart::Lanes(result) => {
                self.inflight = false;
                match result {
                    Some(org) => {
                        let busy = org.arm_num("whole-machine cpu", "busy_fraction");
                        let used = org.arm_num("memory", "free_fraction").map(|f| 1.0 - f);
                        for (history, value) in [
                            (&mut self.busy_history, busy),
                            (&mut self.mem_history, used),
                        ] {
                            if let Some(v) = value {
                                history.push(v);
                                if history.len() > HISTORY {
                                    history.remove(0);
                                }
                            }
                        }
                        self.fold = Some(org);
                        self.degraded = false;
                        self.retry_at = None;
                        self.fold_at = Some(Instant::now());
                    }
                    None => {
                        self.degraded = true;
                        self.retry_at = Some(Instant::now() + CACHE_TTL);
                    }
                }
            }
            FoldPart::Gate(result) => {
                self.gate_inflight = false;
                match result {
                    Ok(gate) => {
                        self.gate = Some(gate);
                        self.gate_error = None;
                        self.gate_retry_at = None;
                        self.gate_at = Some(Instant::now());
                    }
                    Err(error) => {
                        self.gate_error = Some(error);
                        self.gate_retry_at = Some(Instant::now() + CACHE_TTL);
                    }
                }
            }
            FoldPart::Org(inputs) => {
                self.org_inflight = false;
                let now_epoch = chrono::Utc::now().timestamp().max(0) as u64;
                match crate::org_model::derive(&inputs, now_epoch) {
                    Ok(tree) => {
                        self.org_tree = Some(tree);
                        self.org_error = None;
                        self.org_retry_at = None;
                        self.org_at = Some(Instant::now());
                    }
                    Err(error) => {
                        self.org_error = Some(error);
                        self.org_retry_at = Some(Instant::now() + ORG_TTL);
                    }
                }
            }
        }
    }

    /// The minimized block: exactly four lines, painted at the bottom of
    /// the sideline on every frame. Cpu, limit, memory, census.
    /// Every number carries its unit or its comparand here too; the expanded
    /// view adds the detail.
    pub fn minimized_lines(&self, agents: &[AgentRow], text_w: usize) -> Vec<String> {
        let ages: Vec<Option<u64>> = agents.iter().map(|a| a.last_activity_age_s).collect();
        let mut lines: Vec<String> = match self.fold.as_ref() {
            // No machine reading in hand and minimized: the fold is
            // expanded-only, so nothing is reading. Say where the reading
            // lives instead of promising activity that is not running.
            None => vec![
                "  org     expand for a machine reading".to_string(),
                String::new(),
                String::new(),
            ],
            Some(org) if !self.detailed => vec![
                self.simple_cpu_line(org),
                simple_limit_line(org),
                self.memory_line(org),
            ],
            Some(org) => {
                let load_line = match (
                    org.arm_num("cpu admission", "share_low"),
                    org.arm_num("cpu admission", "capacity_cores"),
                    org.arm_num("cpu admission", "ceiling"),
                ) {
                    (Some(low), Some(cores), Some(ceiling)) => {
                        // the fleet's attributed share is what decides, so
                        // that is what renders - never a load average.
                        let mut l = format!(
                            "  fleet     {:.0}% of {cores:.0} cores against {:.0}%",
                            low * 100.0,
                            ceiling * 100.0
                        );
                        if let Some(high) = org.arm_num("cpu admission", "share_high") {
                            if high > low {
                                l.push_str(&format!(" up to {:.0}%", high * 100.0));
                            }
                        }
                        if low > ceiling {
                            // The over verdict names the axis it read, so a busy box
                            // is never mistaken for the fleet being over.
                            l.push_str(&format!(
                                " · {:.1}x over on fleet_cpu_share",
                                low / ceiling
                            ));
                        }
                        l
                    }
                    _ => format!(
                        "  fleet     unknown - {}",
                        org.arm("cpu admission")
                            .map_or("arm absent", |a| a.reason.as_str())
                    ),
                };
                let cpu_line = match (
                    org.arm_num("whole-machine cpu", "busy_fraction"),
                    org.arm_num("whole-machine cpu", "capacity_cores"),
                ) {
                    (Some(busy), Some(cores)) => {
                        format!(
                            "  cpu       {} {:.0}% busy of {cores:.0} cores",
                            self.spark(),
                            busy * 100.0
                        )
                    }
                    _ => "  cpu       unknown".to_string(),
                };
                vec![load_line, cpu_line, self.memory_line(org)]
            }
        };
        let team = self
            .gate
            .as_ref()
            .filter(|g| g.usable())
            .map(|gate| crate::team_census::census(agents, gate, self.org_tree.as_ref()));
        lines.push(self.minimized_team_line(team.as_ref(), &ages, text_w));
        lines.push(self.minimized_lanes_line(team.as_ref(), text_w));
        lines
    }

    /// The glance's team line: from the gate reading when one has landed,
    /// else today's lanes-payload census, else the honest unknown.
    fn minimized_team_line(
        &self,
        team: Option<&crate::team_census::Team>,
        ages: &[Option<u64>],
        text_w: usize,
    ) -> String {
        if let (Some(gate), Some(team)) = (self.gate.as_ref().filter(|g| g.usable()), team) {
            let mut segments = crate::team_census::fleet_segments(team, gate);
            if let Some(word) = self.gate_age_word() {
                segments.push(word);
            }
            return crate::team_census::fit("team", &segments, text_w);
        }
        if let Some(error) = self.gate_error.as_ref() {
            return crate::team_census::fit("team", &[format!("unknown - {error}")], text_w);
        }
        match self.fold.as_ref() {
            Some(org) => census_line(org, ages),
            None => crate::team_census::fit("team", &["unknown - reading".to_string()], text_w),
        }
    }

    /// The glance's lanes line: one segment per provider, `zai 13/16`, with
    /// the lane's working count beside it.
    fn minimized_lanes_line(
        &self,
        team: Option<&crate::team_census::Team>,
        text_w: usize,
    ) -> String {
        if let Some(team) = team {
            let mut segments = Vec::new();
            for lane in &team.providers {
                match lane.limit {
                    Some(cap) => segments.push(format!("{} {}/{}", lane.name, lane.occupied, cap)),
                    None => segments.push(format!("{} {}", lane.name, lane.occupied)),
                }
                if lane.working > 0 {
                    segments.push(format!("{} working", lane.working));
                }
            }
            if !segments.is_empty() {
                return crate::team_census::fit("lanes", &segments, text_w);
            }
        }
        let word = self
            .gate_error
            .as_ref()
            .map(|e| format!("unknown - {e}"))
            .unwrap_or_else(|| "unknown - reading".to_string());
        crate::team_census::fit("lanes", &[word], text_w)
    }

    /// `gate 4.2s` while fresh, `(gate 43s, stale)` past the TTL, `None`
    /// when no landing has a stamp to read.
    fn gate_age_word(&self) -> Option<String> {
        let landed = self.gate_at?;
        let secs = landed.elapsed().as_secs_f32();
        Some(if landed.elapsed() > CACHE_TTL {
            format!("gate {secs:.1}s stale")
        } else {
            format!("gate {secs:.1}s")
        })
    }

    /// The simple glance's first line: the whole machine, then how much of
    /// it is ours, so both numbers read on one base. The agents' share is a
    /// slice of the busy total, never a second total beside it.
    fn simple_cpu_line(&self, org: &Org) -> String {
        let busy = org.arm_num("whole-machine cpu", "busy_fraction");
        let agents = org.arm_num("cpu admission", "share_low");
        match (busy, agents) {
            (Some(busy), Some(agents)) => {
                // Split the ROUNDED numbers so the printed parts add up.
                let (busy, agents) = ((busy * 100.0).round(), (agents * 100.0).round());
                format!(
                    "  CPU       {} {busy:.0}% busy: agents {agents:.0}%, everything else {:.0}%",
                    self.spark(),
                    (busy - agents).max(0.0)
                )
            }
            (Some(busy), None) => format!("  CPU       {} {:.0}% busy", self.spark(), busy * 100.0),
            (None, Some(agents)) => format!("  CPU       agents {:.0}%", agents * 100.0),
            (None, None) => "  CPU       unknown".to_string(),
        }
    }

    /// Memory in use with its graph: one line in both views, since a full
    /// machine swaps long before its CPU line looks alarming. The figure is
    /// the capacity state's own (one reader with the meter); the fold's
    /// arms stay the fallback when no state exists yet.
    fn memory_line(&self, org: &Org) -> String {
        if let Some(state) = crate::client::meter::capacity_state() {
            if let Some(mem) = state.get("memory") {
                if let (Some(fraction), Some(total)) = (
                    mem.get("used_fraction").and_then(|v| v.as_f64()),
                    mem.get("total_gb").and_then(|v| v.as_f64()),
                ) {
                    return format!(
                        "  memory    {} {:.0}% used, {:.1} GB free",
                        spark(&self.mem_history),
                        fraction * 100.0,
                        ((1.0 - fraction) * total).max(0.0)
                    );
                }
            }
        }
        match (
            org.arm_num("memory", "free_fraction"),
            org.arm_num("memory", "available_gb"),
        ) {
            (Some(free), Some(gb)) => format!(
                "  memory    {} {:.0}% used, {gb:.1} GB free",
                spark(&self.mem_history),
                (1.0 - free) * 100.0
            ),
            (None, Some(gb)) => format!("  memory    {gb:.1} GB free"),
            _ => "  memory    unknown".to_string(),
        }
    }

    fn spark(&self) -> String {
        spark(&self.busy_history)
    }

    /// The expanded block: the full reading, in place. Three rules this
    /// render keeps, each closing a way a monitor can lie. The `read` line
    /// always shows the fold's age and says `stale` rather than `unknown`.
    /// The attribution gap gets its own line and is never folded into a
    /// count. A refusal prints the advisor's own words verbatim with no lane
    /// number beside them.
    pub fn expanded_lines(
        &self,
        agents: &[AgentRow],
        text_w: usize,
        max_lines: usize,
    ) -> Vec<String> {
        let ages: Vec<Option<u64>> = agents.iter().map(|a| a.last_activity_age_s).collect();
        let mut lines = Vec::new();
        let org = match self.fold.as_ref() {
            None => {
                lines.push(if self.degraded {
                    format!(
                        "  fold failed - `fno doctor lanes` did not answer inside {}s",
                        SHELLOUT_TIMEOUT.as_secs()
                    )
                } else {
                    "  reading the machine...".to_string()
                });
                None
            }
            Some(org) => Some(org),
        };

        let team_budget = max_lines.saturating_sub(1); // the read line keeps its row
        if let Some(org) = org {
            // the fleet's CPU admission: the operator's own question, first.
            // The share is what decides; the 1m/5m/15m trio is trend
            // only, and the 15-minute figure sits beside its backstop.
            match (
                org.arm_num("cpu admission", "share_low"),
                org.arm_num("cpu admission", "capacity_cores"),
                org.arm_num("cpu admission", "ceiling"),
            ) {
                (Some(low), Some(cores), Some(ceiling)) => {
                    let mut first = format!(
                        "  {label:<8} {:.0}% of {cores:.0} cores against {:.0}%",
                        low * 100.0,
                        ceiling * 100.0,
                        label = "fleet"
                    );
                    if let Some(high) = org.arm_num("cpu admission", "share_high") {
                        if high > low {
                            first.push_str(&format!(" up to {:.0}% (gap)", high * 100.0));
                        }
                    }
                    lines.push(first);
                    let mut second = String::from("            ");
                    if let (Some(l1), Some(l5), Some(l15)) = (
                        org.arm_num("cpu admission", "load_1m"),
                        org.arm_num("cpu admission", "load_5m"),
                        org.arm_num("cpu admission", "load_15m"),
                    ) {
                        second.push_str(&format!("· {l1:.1} 1m · {l5:.1} 5m · {l15:.1} 15m "));
                    }
                    if let Some(backstop) = org.arm_num("cpu admission", "backstop") {
                        second.push_str(&format!("· backstop {backstop:.1}"));
                    }
                    if !second.trim().is_empty() {
                        lines.push(second.trim_end().to_string());
                    }
                    if low > ceiling {
                        lines.push(format!(
                            "            · {:.1}x over on fleet_cpu_share",
                            low / ceiling
                        ));
                    }
                }
                _ => {
                    let reason = org
                        .arm("cpu admission")
                        .map_or("arm absent", |a| a.reason.as_str());
                    lines.push(format!("  {label:<8} unknown - {reason}", label = "fleet"));
                }
            }

            // the machine the lanes compete for, not the fleet alone. The 100%
            // reading was TRUE and unbelievable, so a saturated box names the
            // place the explanation lives.
            match (
                org.arm_num("whole-machine cpu", "busy_fraction"),
                org.arm_num("whole-machine cpu", "capacity_cores"),
            ) {
                (Some(busy), Some(cores)) => {
                    let saturated = if busy >= 0.995 {
                        format!(" (all {cores:.0} saturated; see top)")
                    } else {
                        String::new()
                    };
                    lines.push(format!(
                        "  {label:<8} {} {:.0}% busy of {cores:.0} cores{saturated}",
                        self.spark(),
                        busy * 100.0,
                        label = "cpu"
                    ));
                }
                _ => lines.push(format!("  {label:<8} unknown", label = "cpu")),
            }
            if let Some(free) = org.arm_num("memory", "available_gb") {
                lines.push(format!(
                    "  {label:<8} {free:.1} GB available",
                    label = "memory"
                ));
            }

            // The census: split by what the operator actually asked (how many
            // are working) while no gate reading has landed, and replaced by the
            // team section once one has - the all-rows line and the Python
            // census's lead count answer the same question the team line does,
            // and two answers on one block is the drift this node exists to
            // close.
            if !self.gate_ready() {
                if ages.is_empty() {
                    lines.push(format!(
                        "  {label:<8} no roster rows held",
                        label = "workers"
                    ));
                } else {
                    let split = census_split(&ages);
                    lines.push(format!(
                        "  {label:<8} {} ({} rows)",
                        split.render(),
                        ages.len(),
                        label = "workers"
                    ));
                }
                let census = &org.census;
                lines.push(format!(
                    "  {label:<8} {} leads · {} test processes",
                    count_or_unknown(census.leads),
                    count_or_unknown(census.tests),
                    label = "org"
                ));
                if let Some(conflicts) = census.lead_conflicts.filter(|c| *c > 0) {
                    lines.push(format!(
                        "  {label:<8} {conflicts} scope(s) held by more than one team",
                        label = "warn"
                    ));
                }
            }
        }
        // The team section: the census fold over the gate reading, the org
        // tree and the rows already in hand. No I/O; the halves landed.
        lines.extend(self.team_section(agents, text_w, team_budget.saturating_sub(lines.len())));
        // The reap hint survives the all-rows line that used to carry it:
        // dead rows are a backlog, whatever line answered the census.
        let all_split = census_split(&ages);
        if all_split.dead > 0 {
            lines.push(format!(
                "            {} dead: run `fno agents reap --apply`",
                all_split.dead
            ));
        }

        // the lanes payload's own readings, when it landed: the caller's
        // share, what saturates the box, the advisor's answer, and the
        // attribution gap. The unattributed bucket renders only when it
        // holds someone - a count that sees nobody says so by being absent.
        if let Some(org) = self.fold.as_ref() {
            let census = &org.census;
            if let Some(share) = &census.share {
                let mut line = format!(
                    "  {label:<8} held {} of share {} across {} leads",
                    count_or_unknown(share.held),
                    count_or_unknown(share.share),
                    count_or_unknown(share.leads),
                    label = "share"
                );
                if let Some(unattributed) = share.unattributed.as_ref().filter(|u| u.count > 0) {
                    let shown = unattributed.rows.join(", ");
                    line.push_str(&format!(" · {} unattributed ({shown})", unattributed.count));
                }
                lines.push(line);
            }

            // what is saturating the box, from the ps read the fold already
            // performed - never a second instrument.
            match census.top_consumers.as_ref().map(Vec::as_slice) {
                Some([]) => lines.push(format!(
                    "  {label:<8} no fleet processes measured",
                    label = "top"
                )),
                Some(consumers) => {
                    let items = consumers
                        .iter()
                        .map(|c| format!("{} {} procs {:.0}%", c.name, c.procs, c.cpu_pct))
                        .collect::<Vec<_>>()
                        .join(" · ");
                    lines.push(format!("  {label:<8} {items}", label = "top"));
                    for c in consumers
                        .iter()
                        .filter(|c| c.worktree_procs >= 2 && c.worktree.is_some())
                    {
                        lines.push(format!(
                            "            {} of {} in {}",
                            c.worktree_procs,
                            c.name,
                            c.worktree.as_deref().unwrap_or_default()
                        ));
                    }
                }
                None => lines.push(format!(
                    "  {label:<8} unavailable - the fleet footprint reading is dark",
                    label = "top"
                )),
            }

            // the advisor's answer, or the advisor's refusal in its own words.
            match org.lane_count {
                Some(count) => {
                    let cost = match (org.per_lane_cpu_cores, org.per_lane_mem_gb) {
                        (Some(c), Some(g)) => format!(" · {c:.3} cores, {g:.2} GB per lane"),
                        _ => String::new(),
                    };
                    lines.push(format!(
                        "  {label:<8} {count} more fit{cost}",
                        label = "lanes"
                    ));
                    if !org.cost_source.is_empty() {
                        lines.push(format!("          {}", org.cost_source));
                    }
                }
                None => {
                    let reason = if org.refused_reason.is_empty() {
                        "no lane number: the advisor refused without naming a reason"
                    } else {
                        org.refused_reason.as_str()
                    };
                    lines.push(format!("  {label:<8} REFUSED - {reason}", label = "lanes"));
                }
            }

            if let Some(gap) = census.attribution_gap.as_ref() {
                lines.push(format!("  {label:<8} {gap}", label = "gap"));
                lines.push("          fleet share is an undercount, not headroom".to_string());
            }
        }

        let age = self.fold_at.map_or_else(
            || "age unknown".to_string(),
            |t| {
                let secs = t.elapsed().as_secs_f32();
                if t.elapsed() > CACHE_TTL {
                    format!("{secs:.1}s ago (stale, refreshing)")
                } else {
                    format!("{secs:.1}s ago")
                }
            },
        );
        let mut read = format!("{age}");
        if let Some(word) = self.gate_age_word() {
            read.push_str(&format!(" · {word}"));
        }
        if let Some(landed) = self.org_at {
            let secs = landed.elapsed().as_secs_f32();
            read.push_str(&format!(" · teams {secs:.0}s"));
        }
        let degraded = if self.degraded {
            " · last refresh failed"
        } else {
            ""
        };
        lines.push(format!("  {label:<8} {read}{degraded}", label = "read"));
        lines
    }

    /// True when a usable gate reading has landed: the team section then
    /// answers the census, and the all-rows workers line stands down.
    fn gate_ready(&self) -> bool {
        self.gate.as_ref().filter(|g| g.usable()).is_some()
    }

    /// The expanded team section: fleet line, Lead lines under the line
    /// budget, the unowned line, and one provider line per lane. A gate
    /// reading past the TTL keeps its numbers and names its age; a failed
    /// or unread one says unknown and names the reason.
    fn team_section(&self, agents: &[AgentRow], text_w: usize, budget: usize) -> Vec<String> {
        let mut lines = Vec::new();
        match self.gate.as_ref() {
            Some(gate) if gate.usable() => {
                let team = crate::team_census::census(agents, gate, self.org_tree.as_ref());
                let mut segments = crate::team_census::fleet_segments(&team, gate);
                if let Some(word) = self.gate_age_word() {
                    segments.push(word);
                }
                lines.push(crate::team_census::fit("team", &segments, text_w));
                let lead_budget = budget.saturating_sub(lines.len());
                if self.org_tree.is_some() {
                    lines.extend(crate::team_census::lead_lines(
                        &team.leads,
                        text_w,
                        lead_budget,
                    ));
                } else if let Some(error) = self.org_error.as_ref() {
                    lines.push(crate::team_census::fit(
                        "leads",
                        &[format!("unknown - {error}")],
                        text_w,
                    ));
                }
                if let Some(line) = crate::team_census::unowned_line(&team.unowned, text_w) {
                    lines.push(line);
                }
                lines.extend(crate::team_census::provider_lines(&team.providers, text_w));
            }
            Some(gate) => {
                let reason = gate
                    .reason
                    .as_deref()
                    .or(gate.message.as_deref())
                    .unwrap_or("the probe named no occupancy");
                lines.push(crate::team_census::fit(
                    "team",
                    &[format!("unknown - {reason}")],
                    text_w,
                ));
            }
            None => {
                let reason = self.gate_error.as_deref().unwrap_or("reading").to_string();
                lines.push(crate::team_census::fit(
                    "team",
                    &[format!("unknown - {reason}")],
                    text_w,
                ));
            }
        }
        lines
    }
}

/// Resolve the `fno` binary through the server's one resolver, exactly as
/// the yard leg does. A second resolver with different semantics would let
/// this panel degrade on a checkout where every sibling leg still works.
fn fno_bin() -> PathBuf {
    crate::server::fno_bin()
}

/// Fold the org now. `None` on any failure (timeout, unparseable JSON) -
/// the caller shows a named degrade line.
///
/// A NONZERO exit is not a failure here: `fno doctor lanes` exits 3 on a
/// refusal and still prints the whole reading, refusal reason and census
/// included. Treating exit 3 as a dead fold would blank the panel at exactly
/// the moment its refusal is the answer.
pub async fn fold_now() -> Option<Org> {
    let mut command = crate::process_admission::tokio_command(fno_bin());
    command
        .args(["doctor", "lanes", "--json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // Dropped on timeout; kill_on_drop reaps the child so a slow fold
        // can't orphan a Python process on each overlay open.
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let output = tokio::time::timeout(SHELLOUT_TIMEOUT, fut)
        .await
        .ok()?
        .ok()?;
    parse(&output.stdout)
}

/// Parse the verb's JSON payload. Fails quiet (returns `None`) on
/// unparseable output so a torn stdout degrades the overlay, not crashes it.
fn parse(stdout: &[u8]) -> Option<Org> {
    serde_json::from_slice(stdout).ok()
}

/// Fold the spawn-gate probe now: one payload on stdin, the whole answer
/// parsed as the gate reading. A nonzero exit is not a failure - the probe
/// answers refusals on stdout too - but an empty one is.
pub async fn gate_now() -> Result<GateReading, String> {
    let mut command =
        crate::process_admission::tokio_command(crate::digest_overlay::fno_agents_bin());
    command
        .args(["spawn-gate"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // Dropped on timeout; kill_on_drop reaps the child so a hung probe
        // cannot leak a process per TTL. stdout MUST be piped here: unlike
        // `tokio_output`, `wait_with_output` reads the stdio this command
        // already carries, and an inherited stdout paints the answer on the
        // pane instead of parsing it.
        .kill_on_drop(true);
    let mut child = crate::process_admission::tokio_spawn(&mut command)
        .map_err(|e| format!("fno-agents spawn-gate could not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let payload = br#"{"mode":"probe","only":["lanes"],"tests":true}"#;
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(payload).await;
        let _ = stdin.shutdown().await;
        // `stdin` drops here and closes the pipe: the probe reads to EOF.
    }
    let output = tokio::time::timeout(GATE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "fno-agents spawn-gate timed out after {}s",
                GATE_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("fno-agents spawn-gate failed: {e}"))?;
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("fno-agents spawn-gate sent invalid JSON: {e}"))
}

/// Start one task per due half. Each task sends its own part over the one
/// channel, so a slow `fno doctor lanes` never holds the probe's team
/// answer and a failed org read never blanks the fleet line. Paint and key
/// paths run no I/O; this is the only place folds are born.
pub fn spawn_folds(want: Want, tx: tokio::sync::mpsc::UnboundedSender<FoldPart>) {
    if want.lanes {
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(FoldPart::Lanes(fold_now().await));
        });
    }
    if want.gate {
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(FoldPart::Gate(gate_now().await));
        });
    }
    if want.org {
        let tx = tx.clone();
        let agents = want.agents;
        tokio::spawn(async move {
            let inputs = crate::org_model::gather(&crate::backlog_view::graph_path(), agents).await;
            let _ = tx.send(FoldPart::Org(inputs));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload measured on one machine, 2026-09-04, trimmed to the
    /// fields the panel reads. The `workers` count stays in the fixture to
    /// prove the render never reads it: the split is the worker answer now.
    const LIVE: &[u8] = br#"{
      "lane_count": 0,
      "per_lane_cpu_cores": 0.032,
      "per_lane_mem_gb": 0.396,
      "cost_source": "measured from the live roster's attributed footprint (50 live row(s))",
      "refused_reason": "",
      "census": {"leads": 5, "lead_conflicts": 0, "workers": 45, "tests": 2,
                 "roster_rows": 50, "read_ms": 597, "attribution_gap": null,
                 "share": {"leads": 4, "share": 7, "held": 2,
                           "unattributed": {"count": 2, "rows": ["ghost-a", "ghost-b"]}},
                 "top_consumers": [
                   {"name": "fno-py", "procs": 23, "cpu_pct": 41.2,
                    "worktree": ".fno/worktrees/x-aaaa", "worktree_procs": 22},
                   {"name": "fno-agents-worker", "procs": 18, "cpu_pct": 27.9,
                    "worktree": ".fno/worktrees/x-aaaa", "worktree_procs": 18}
                 ]},
      "arms": [
        {"name": "cpu admission", "state": "measured",
         "value": {"fleet_cores": 7.0, "capacity_cores": 12.0,
                   "share_low": 0.5833, "share_high": 0.5833, "bound": "exact",
                   "ceiling": 0.5, "verdict": "hold",
                   "load_15m": 88.2, "backstop": 480.0,
                   "load_1m": 107.3, "load_5m": 99.5}, "reason": ""},
        {"name": "whole-machine cpu", "state": "measured",
         "value": {"busy_fraction": 0.797, "capacity_cores": 12}, "reason": ""},
        {"name": "memory", "state": "measured",
         "value": {"free_fraction": 0.63, "available_gb": 64.9}, "reason": ""}
      ]
    }"#;

    fn live() -> Org {
        parse(LIVE).expect("the measured payload parses")
    }

    /// The AC6 roster as rows, so the census folds over the same ages the
    /// old all-rows helpers passed.
    fn age_rows(ages: &[Option<u64>]) -> Vec<crate::proto::AgentRow> {
        ages.iter()
            .enumerate()
            .map(|(i, age)| crate::proto::AgentRow {
                name: format!("row-{i}"),
                last_activity_age_s: *age,
                ..Default::default()
            })
            .collect()
    }

    /// The AC6 roster: one row per bucket plus one with no age.
    const AC6_AGES: [Option<u64>; 7] = [
        Some(10),
        Some(200),
        Some(900),
        Some(4000),
        Some(20_000),
        Some(40_000),
        None,
    ];

    /// A panel holding one landed reading, expanded (the fold runs only
    /// while expanded, so a reading can only land there).
    fn opened(org: Org) -> Panel {
        let mut panel = Panel::default();
        panel.toggle();
        assert!(
            panel.take_want(&[]).is_some(),
            "a fresh expanded panel wants a fold"
        );
        panel.apply(FoldPart::Lanes(Some(org)));
        panel
    }

    /// The lanes and org folds are the expanded panel's cost: minimized
    /// never wants them. The gate half is the one exception: the glance
    /// renders team and lanes from it, so a minimized panel wants the gate
    /// alone, and a fresh gate reading leaves nothing due.
    #[test]
    fn minimized_panel_wants_only_the_gate_half() {
        let mut panel = Panel::default();
        assert!(
            panel.refresh_deadline().is_some(),
            "the due gate half arms the timer"
        );
        let want = panel.take_want(&[]).expect("minimized wants the gate");
        assert!(want.gate, "the gate half is always due when unread");
        assert!(!want.lanes, "minimized never wants the lanes fold");
        assert!(!want.org, "minimized never wants the org fold");
        assert!(
            panel.refresh_deadline().is_none(),
            "the deadline is None while the gate half runs"
        );

        panel.apply(FoldPart::Gate(Ok(GateReading {
            slots: Some(2),
            ..GateReading::default()
        })));
        assert!(
            panel.take_want(&[]).is_none(),
            "a fresh gate reading leaves nothing due while minimized"
        );
        assert!(
            panel.refresh_deadline().is_some(),
            "the next gate refresh schedules at the TTL"
        );

        panel.toggle();
        assert!(
            panel.refresh_deadline().is_some(),
            "expanding arms the timer"
        );
        let want = panel.take_want(&[]).expect("expanding arms the first fold");
        assert!(want.lanes, "expanded wants the lanes fold");
        assert!(want.org, "expanded wants the org fold");
        assert!(!want.gate, "the fresh gate reading is inside its TTL");

        panel.apply(FoldPart::Lanes(Some(live())));
        panel.fold_at = Some(Instant::now() - CACHE_TTL - Duration::from_secs(1));
        panel.toggle();
        assert!(
            panel.refresh_deadline().is_some(),
            "the minimized glance still schedules its gate refresh"
        );
        panel.gate_at = Some(Instant::now() - CACHE_TTL - Duration::from_secs(1));
        let want = panel.take_want(&[]).expect("the stale gate re-arms");
        assert!(want.gate, "{want:?}");
        assert!(
            !want.lanes,
            "a stale lanes reading still waits while minimized"
        );
        panel.toggle();
        let want = panel.take_want(&[]).expect("re-expanding refreshes both");
        assert!(want.lanes && want.org, "{want:?}");
    }

    #[test]
    fn parse_rows() {
        let org = live();
        assert_eq!(org.lane_count, Some(0));
        assert_eq!(org.census.leads, Some(5));
        assert_eq!(org.census.workers, Some(45));
        assert_eq!(org.census.tests, Some(2));
        assert_eq!(org.census.read_ms, Some(597));
        assert_eq!(org.arm_num("cpu admission", "fleet_cores"), Some(7.0));
        assert_eq!(org.arm_num("cpu admission", "share_low"), Some(0.5833));
        assert_eq!(org.arm_num("cpu admission", "ceiling"), Some(0.5));
        assert_eq!(org.arm_num("cpu admission", "load_1m"), Some(107.3));
        assert_eq!(org.arm_num("cpu admission", "load_5m"), Some(99.5));
        assert_eq!(org.arm_num("cpu admission", "load_15m"), Some(88.2));
        assert_eq!(org.arm_num("cpu admission", "backstop"), Some(480.0));
        assert_eq!(org.arm_str("cpu admission", "verdict"), Some("hold"));
        assert_eq!(org.arm_str("cpu admission", "bound"), Some("exact"));
        assert_eq!(
            org.arm_num("whole-machine cpu", "capacity_cores"),
            Some(12.0)
        );
        let top = org
            .census
            .top_consumers
            .as_ref()
            .expect("top consumers parse");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].name, "fno-py");
        assert_eq!(top[0].worktree.as_deref(), Some(".fno/worktrees/x-aaaa"));

        // A binary newer than the fno on PATH: the census defaults to all
        // unknown rather than failing the whole fold.
        let org = parse(br#"{"lane_count": 7, "arms": []}"#).expect("parses");
        assert_eq!(org.lane_count, Some(7));
        assert_eq!(org.census, Census::default());

        assert!(parse(b"{not json").is_none());
        assert!(parse(b"").is_none());
    }

    #[test]
    fn leads_plus_workers_is_the_roster() {
        // Not arithmetic the panel performs - the identity the Python census
        // guarantees by reading ONE rows list. Asserted here so a payload
        // that stops holding it is caught at the surface that renders it.
        let c = live().census;
        assert_eq!(
            c.leads.unwrap() + c.workers.unwrap(),
            c.roster_rows.unwrap()
        );
    }

    #[test]
    fn refusal_rows() {
        let mut org = live();
        org.lane_count = None;
        org.refused_reason = String::new();
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("REFUSED"), "{text}");
        assert!(text.contains("without naming a reason"), "{text}");

        let org = parse(
            br#"{"lane_count": null, "refused_reason": "arms dark",
                 "census": {}, "arms": [{"name": "cpu admission", "state": "dark",
                 "value": null, "reason": "the footprint reading is dark"}]}"#,
        )
        .expect("parses");

        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(
            text.contains("fleet    unknown - the footprint reading is dark"),
            "{text}"
        );
        assert!(!text.contains("fleet 58%"), "{text}");

        let mut org = live();
        org.lane_count = None;
        org.refused_reason =
            "the machine arms cannot answer the lane question: memory dark (macmon not on PATH)"
                .to_string();
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("lanes    REFUSED"), "{text}");
        assert!(text.contains("memory dark (macmon not on PATH)"), "{text}");
        assert!(!text.contains("more fit"), "{text}");
    }

    #[test]
    fn expanded_fleet_rows() {
        let text = opened(live())
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        // the share renders as the deciding number, the trend trio is
        // display-only, and the over-line names the fleet_cpu_share axis.
        assert!(
            text.contains("fleet    58% of 12 cores against 50%"),
            "{text}"
        );
        assert!(
            text.contains("· 107.3 1m · 99.5 5m · 88.2 15m · backstop 480.0"),
            "{text}"
        );
        assert!(text.contains("1.2x over on fleet_cpu_share"), "{text}");
        assert!(!text.contains("load avg"), "{text}");
        assert!(text.contains("80% busy of 12 cores"), "{text}");
        assert!(text.contains("64.9 GB available"), "{text}");
        assert!(text.contains("0 more fit"), "{text}");
        assert!(text.contains("0.0s ago"), "{text}");

        // The 100%-busy reading was TRUE and read as a bug: the saturated
        // line must name where the explanation lives.
        let mut org = live();
        let arm = org
            .arms
            .iter_mut()
            .find(|a| a.name == "whole-machine cpu")
            .expect("the cpu arm exists");
        arm.value["busy_fraction"] = serde_json::json!(1.0);
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("(all 12 saturated; see top)"), "{text}");

        let text = opened(live())
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("fno-py 23 procs 41%"), "{text}");
        assert!(
            text.contains("22 of fno-py in .fno/worktrees/x-aaaa"),
            "{text}"
        );
        assert!(
            text.contains("18 of fno-agents-worker in .fno/worktrees/x-aaaa"),
            "{text}"
        );

        let mut org = live();
        org.census.top_consumers = None;
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(
            text.contains("unavailable - the fleet footprint reading is dark"),
            "{text}"
        );
        assert!(
            text.contains("fleet    58% of 12 cores against 50%"),
            "{text}"
        );
        assert!(text.contains("80% busy of 12 cores"), "{text}");
        assert!(text.contains("2 working"), "{text}");
    }

    #[test]
    fn glance_rows() {
        let lines = opened(live()).minimized_lines(&age_rows(&AC6_AGES), 200);

        assert_eq!(lines.len(), MINIMIZED_ROWS, "{lines:?}");
        // The default glance is plain words, with the CPU graph.
        assert_eq!(
            lines[0],
            "  CPU       ▇ 80% busy: agents 58%, everything else 22%"
        );
        assert_eq!(
            lines[1],
            "  agents    over their 50% limit, so new workers wait"
        );
        assert_eq!(lines[2], "  memory    ▄ 37% used, 64.9 GB free");

        let mut panel = Panel::with_detail(true);
        panel.toggle();
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(Some(live())));
        panel.apply(FoldPart::Lanes(Some(live())));
        let detail = panel.minimized_lines(&age_rows(&AC6_AGES), 200);
        assert!(
            detail[0].contains("58% of 12 cores against 50%"),
            "{detail:?}"
        );
        assert!(detail[0].contains("1.2x"), "{detail:?}");
        assert!(detail[1].contains("▇▇ 80% busy of 12 cores"), "{detail:?}");
        assert_eq!(
            lines[3],
            "  team      5 leads · 2 working · 2 idle · 1 stale · 1 dead · 1 unknown age"
        );

        let panel = Panel::default();

        let lines = panel.minimized_lines(&[], 200);

        assert_eq!(lines.len(), MINIMIZED_ROWS, "{lines:?}");
        assert!(
            lines[0].contains("expand for a machine reading"),
            "{lines:?}"
        );
        assert!(
            !lines[0].contains('0'),
            "a pending fold must show no counts"
        );
        assert!(lines[1].is_empty() && lines[2].is_empty(), "{lines:?}");

        let mut panel = Panel::default();
        panel.toggle();
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(None));

        // Minimized, a failure reads as the same honest pointer (nothing is
        // retrying while minimized); the fold-failed words live in the
        // expanded view, where the retry actually runs.
        let lines = panel.minimized_lines(&[], 200);

        assert!(
            lines[0].contains("expand for a machine reading"),
            "{lines:?}"
        );
        let expanded = panel.expanded_lines(&[], 200, 40).join("\n");

        assert!(expanded.contains("fold failed"), "{expanded:?}");
    }

    #[test]
    fn panel_lifecycle_rows() {
        let mut panel = opened(live());
        assert!(panel.is_expanded(), "a landed reading implies expanded");
        panel.toggle();
        assert!(!panel.is_expanded());
        panel.toggle();
        assert!(panel.is_expanded());
        // Expanding never spawns a fold and never drops the cached reading.
        assert!(panel.take_want(&[]).is_none(), "inside the TTL no refetch");
        assert!(panel
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n")
            .contains("5 leads"));

        let mut panel = Panel::default();
        panel.toggle();

        assert!(panel.take_want(&[]).is_some());
        assert!(
            panel.take_want(&[]).is_none(),
            "holding the key down must not queue Python processes"
        );
    }

    #[test]
    fn census_rows() {
        let text = opened(live())
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(
            text.contains("2 working · 2 idle · 1 stale · 1 dead · 1 unknown age (7 rows)"),
            "{text}"
        );
        assert!(
            text.contains("1 dead: run `fno agents reap --apply`"),
            "{text}"
        );
        // The lanes workers count is never rendered: the split replaced it.
        assert!(!text.contains("45 workers"), "{text}");

        let split = census_split(&AC6_AGES);
        assert_eq!(split.working, 2); // 10s, 200s: under 5m
        assert_eq!(split.idle, 2); // 900s, 4000s: 15m, 66m
        assert_eq!(split.stale, 1); // 20000s: 5.6h
        assert_eq!(split.dead, 1); // 40000s: 11h, the reap backlog
        assert_eq!(split.unknown_age, 1);

        let ages = [None::<u64>, None, None];
        let text = opened(live())
            .expanded_lines(&age_rows(&ages), 200, 40)
            .join("\n");

        assert!(text.contains("3 unknown age (3 rows)"), "{text}");
        assert!(!text.contains("0 working"), "{text}");
        assert!(!text.contains("0 idle"), "{text}");
        assert!(!text.contains("0 stale"), "{text}");
        assert!(!text.contains("0 dead"), "{text}");
        assert!(!text.contains("reap"), "{text}");

        let text = opened(live()).expanded_lines(&[], 200, 40).join("\n");

        assert!(text.contains("no roster rows held"), "{text}");

        let text = opened(live())
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(
            text.contains("share    held 2 of share 7 across 4 leads"),
            "{text}"
        );
        assert!(text.contains("2 unattributed (ghost-a, ghost-b)"), "{text}");

        // Nobody in the bucket: the line stays clean rather than printing a
        // zero that reads as a fact about liveness.
        let mut org = live();
        if let Some(share) = org.census.share.as_mut() {
            share.unattributed = None;
        }
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("share    held 2 of share 7"), "{text}");
        assert!(!text.contains("unattributed"), "{text}");
    }

    #[test]
    fn fold_failure_rows() {
        let mut panel = Panel::default();
        panel.toggle();
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(None));

        let lines = panel.expanded_lines(&[], 200, 40);

        // Three lines: the machine fold's degrade, the team section's honest
        // unknown - the probe failed with it - and the read line, which
        // always shows the fold's age and names the failed refresh.
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("fold failed"), "{lines:?}");
        assert!(lines[0].contains("10s"), "{lines:?}");
        assert!(
            lines[1].contains("team") && lines[1].contains("unknown"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains("read")
                && lines[2].contains("age unknown")
                && lines[2].contains("last refresh failed"),
            "{lines:?}"
        );

        let mut panel = Panel::default();
        panel.toggle();
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(None));

        // The failure armed a retry delay, so the always-visible block does
        // not turn an instantly-failing fold into a hot refetch loop.
        assert!(panel.take_want(&[]).is_none());
        assert!(panel.refresh_deadline().is_some_and(|t| t > Instant::now()));

        // The spin: a stale reading arms a refresh, the refresh fails, and
        // the retry backoff starts from NOW while the stale reading's due
        // time is already long past. The timer wake must wait for the
        // backoff, not re-fire every pass until it ends.
        let mut panel = opened(live());
        panel.fold_at = Some(Instant::now() - CACHE_TTL - Duration::from_secs(120));
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(None));

        assert!(
            panel.take_want(&[]).is_none(),
            "still inside the retry backoff"
        );
        assert!(panel.refresh_deadline().is_some_and(|t| t > Instant::now()));

        let mut panel = opened(live());
        panel.fold_at = Some(Instant::now() - CACHE_TTL - Duration::from_secs(1));
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(None));

        let text = panel
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("last refresh failed"), "{text}");
        assert!(text.contains("5 leads"), "{text}");
        // The failed refresh did NOT restamp the age: the line still
        // describes the last real reading, which is already stale.
        assert!(
            text.contains("stale, refreshing) · last refresh failed"),
            "{text}"
        );

        let mut panel = opened(live());
        panel.fold_at = Some(Instant::now() - CACHE_TTL - Duration::from_secs(1));
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(None));
        // The failure backed off one TTL; simulate it elapsing.
        panel.retry_at = Some(Instant::now() - Duration::from_secs(1));
        assert!(panel.take_want(&[]).is_some());
        panel.apply(FoldPart::Lanes(Some(live())));

        assert!(!panel
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n")
            .contains("last refresh failed"));
    }

    #[test]
    fn ttl_rows() {
        let mut panel = opened(live());

        assert!(panel.take_want(&[]).is_none(), "no refetch inside the TTL");
        assert!(panel.refresh_deadline().is_some_and(|t| t > Instant::now()));

        let mut panel = opened(live());
        panel.fold_at = Some(Instant::now() - CACHE_TTL - Duration::from_secs(1));

        assert!(panel.take_want(&[]).is_some(), "a stale reading refetches");
        let text = panel
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");
        assert!(text.contains("stale, refreshing"), "{text}");
        assert!(text.contains("5 leads"), "{text}");
        // While the refetch runs, no timer wake is armed: a past-due
        // deadline would re-fire every pass for nothing.
        assert!(panel.refresh_deadline().is_none());
    }

    #[test]
    fn census_degrade_rows() {
        // made the gap honest. A panel that folded it into a count
        // would re-open the hole: the gap is a process-to-row failure and
        // cannot change how many rows exist.
        let mut org = live();
        org.census.attribution_gap =
            Some("11 pidless row(s) with no identity route (codex)".to_string());
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("11 pidless row(s)"), "{text}");
        assert!(text.contains("undercount, not headroom"), "{text}");
        assert!(text.contains("5 leads"), "{text}");

        let mut org = live();
        org.census = Census {
            tests: Some(3),
            ..Census::default()
        };
        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(text.contains("unknown leads · 3 test processes"), "{text}");

        let mut org = live();
        org.census.lead_conflicts = Some(2);

        let text = opened(org)
            .expanded_lines(&age_rows(&AC6_AGES), 200, 40)
            .join("\n");

        assert!(
            text.contains("2 scope(s) held by more than one team"),
            "{text}"
        );
    }

    #[test]
    fn the_org_binding_is_c_in_global_and_dispatches_open_org() {
        let binding = crate::keys::key_bindings()
            .into_iter()
            .find(|b| b.action == "org")
            .expect("the org action is bound");

        assert_eq!(binding.key, b'C');
        assert_eq!(binding.event, crate::keys::Event::OpenTeam);
        assert!(matches!(binding.section, crate::keys::KeySection::Global));
    }
}

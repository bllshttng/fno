//! The onboarding conversation as a pure module: fifteen screens in a
//! fixed order, three presets as per-screen stop flags, and a proposal
//! list the terminal host applies. No terminal, no subprocess and no
//! clock live here - a scripted prompter drives the same flow in tests,
//! and the terminal host in `wizard_term` supplies the real one.

use std::collections::{BTreeMap, VecDeque};

use crate::setup_autowire::HARNESS_LIST;
use crate::setup_run::Proposal;

/// One of the three onboarding postures. `slot` indexes the per-screen
/// stop table: the wizard only asks where the posture says to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Aggressive,
    Balanced,
    UserReviewed,
}

impl Preset {
    pub fn slot(self) -> usize {
        match self {
            Preset::Aggressive => 0,
            Preset::Balanced => 1,
            Preset::UserReviewed => 2,
        }
    }

    fn from_slot(slot: usize) -> Preset {
        match slot {
            0 => Preset::Aggressive,
            2 => Preset::UserReviewed,
            _ => Preset::Balanced,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Preset::Aggressive => "aggressive",
            Preset::Balanced => "balanced",
            Preset::UserReviewed => "user-reviewed",
        }
    }
}

/// One harness found on this machine, and how.
#[derive(Debug, Clone)]
pub struct Found {
    pub name: &'static str,
    pub by: &'static str,
}

/// What the host measured before the flow starts. `env_keys` pairs a
/// harness name with the env var name that carries its key - never the
/// key itself.
#[derive(Debug, Clone)]
pub struct Detect {
    pub markers_present: bool,
    pub band: Option<i64>,
    pub harnesses: Vec<Found>,
    pub env_keys: Vec<(&'static str, &'static str)>,
    pub models: Vec<String>,
}

impl Detect {
    /// The empty fallback for a host that could not measure anything.
    pub fn fresh() -> Detect {
        Detect {
            markers_present: false,
            band: None,
            harnesses: Vec::new(),
            env_keys: Vec::new(),
            models: Vec::new(),
        }
    }
}

/// Whether a screen runs for the machine at hand.
#[derive(Debug, Clone, Copy)]
pub enum When {
    Always,
    FreshAndUnpreset,
    Rerun,
    ModelsPending,
    NeedsKey,
    JoinGiven,
}

impl When {
    fn holds(&self, state: &State) -> bool {
        match self {
            When::Always => true,
            When::FreshAndUnpreset => !state.detect.markers_present && state.preset.is_none(),
            When::Rerun => state.detect.markers_present,
            When::ModelsPending => {
                !state.picked_harnesses().is_empty() && !state.answers.contains_key("models")
            }
            When::NeedsKey => {
                !state.answers.contains_key("keys")
                    && state
                        .picked_harnesses()
                        .iter()
                        .any(|h| env_for(&state.detect, h).is_none())
            }
            When::JoinGiven => state.join,
        }
    }
}

/// One screen: its id, when it runs, and whether each preset stops to
/// ask on it (aggressive, balanced, user-reviewed).
#[derive(Debug, Clone, Copy)]
pub struct Screen {
    pub id: &'static str,
    pub when: When,
    pub stops: [bool; 3],
}

/// The fixed screen order. The wizard never reorders; a false `when`
/// records a skip and moves on.
pub const SCREENS: [Screen; 15] = [
    Screen {
        id: "preflight",
        when: When::Always,
        stops: [false, false, false],
    },
    Screen {
        id: "rerun",
        when: When::Rerun,
        stops: [false, true, true],
    },
    Screen {
        id: "preset",
        when: When::FreshAndUnpreset,
        stops: [false, true, true],
    },
    Screen {
        id: "look",
        when: When::Always,
        stops: [false, false, true],
    },
    Screen {
        id: "harnesses",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "models",
        when: When::ModelsPending,
        stops: [false, true, true],
    },
    Screen {
        id: "keys",
        when: When::NeedsKey,
        stops: [false, true, true],
    },
    Screen {
        id: "budget",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "review-merge",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "services",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "projects",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "join",
        when: When::JoinGiven,
        stops: [false, true, true],
    },
    Screen {
        id: "review",
        when: When::Always,
        stops: [false, true, true],
    },
    Screen {
        id: "apply",
        when: When::Always,
        stops: [false, false, false],
    },
    Screen {
        id: "done",
        when: When::Always,
        stops: [false, false, false],
    },
];

/// An answer the flow keeps. A secret never lands here - the keys
/// screen stores an env var name or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Picked(Vec<String>),
    Text(String),
}

/// Everything one run carries. A host restores a mid-run session by
/// building this and driving `step` again.
#[derive(Debug, Clone)]
pub struct State {
    pub preset: Option<Preset>,
    pub join: bool,
    pub detect: Detect,
    pub answers: BTreeMap<&'static str, Answer>,
    pub index: usize,
    pub writes_started: bool,
    pub skip_all: bool,
    /// (screen id, "shown" | "skipped") in walk order, for the report.
    pub screens: Vec<(&'static str, &'static str)>,
    /// The ids of the screens shown so far; back walks this in reverse.
    pub trail: Vec<&'static str>,
    /// (harness, "pass" | "fail:<reason>", milliseconds).
    pub checks: Vec<(&'static str, String, u64)>,
}

impl State {
    pub fn new(preset: Option<Preset>, detect: Detect, join: bool) -> State {
        State {
            preset,
            join,
            detect,
            answers: BTreeMap::new(),
            index: 0,
            writes_started: false,
            skip_all: false,
            screens: Vec::new(),
            trail: Vec::new(),
            checks: Vec::new(),
        }
    }

    /// The harness names the harnesses screen committed.
    pub fn picked_harnesses(&self) -> Vec<String> {
        match self.answers.get("harnesses") {
            Some(Answer::Picked(picked)) => picked.clone(),
            _ => Vec::new(),
        }
    }

    /// Back is live until the review screen says yes to a write.
    pub fn back_allowed(&self) -> bool {
        !self.writes_started
    }
}

/// What one `step` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Forward,
    Back,
    Cancel,
    SkipToDone,
    Done,
}

/// The prompter's reply to a yes/no ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conf {
    Back,
    Cancel,
    Yes,
    No,
}

/// The prompter's reply to a single pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Back,
    Cancel,
    Ok(usize),
}

/// The prompter's reply to a checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ticks {
    Back,
    Cancel,
    Ok(Vec<usize>),
}

/// The prompter's reply to a secret ask. The pure flow never asks for
/// one; the terminal host owns the masked read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hidden {
    Back,
    Cancel,
    Skipped,
    Value(String),
}

/// One probe outcome: how long a check took, or why it failed.
#[derive(Debug, Clone, Copy)]
pub enum Probe {
    Pass { ms: u64 },
    Fail(&'static str),
}

/// The flow's view of the terminal. Every ask names its screen so a
/// script and the report can key on it.
pub trait Prompter {
    fn confirm(&mut self, screen: &'static str, prompt: &str, default: bool) -> Conf;
    fn select(
        &mut self,
        screen: &'static str,
        prompt: &str,
        options: &[&str],
        default: usize,
    ) -> Pick;
    fn checklist(
        &mut self,
        screen: &'static str,
        prompt: &str,
        options: &[&str],
        defaults: &[usize],
    ) -> Ticks;
    fn secret(&mut self, screen: &'static str, prompt: &str) -> Hidden;
    fn note(&mut self, screen: &'static str, text: &str);
    fn progress(&mut self, screen: &'static str, label: &str);
}

/// The flow's view of the real checks. The terminal host runs them;
/// tests answer with a fixed queue.
pub trait Probes {
    fn harness_check(&mut self, harness: &str) -> Probe;
    fn model_check(&mut self, harness: &str, model: &str) -> Probe;
}

enum Flow<T> {
    Answered(T),
    Back,
    Cancel,
}

enum Outcome {
    Forward,
    Back,
    Cancel,
    SkipToDone,
}

/// The shared confirm loop. A Back honours the latch: once writes have
/// started it says so and asks again.
fn confirm_flow(
    state: &State,
    pr: &mut dyn Prompter,
    screen: &'static str,
    prompt: &str,
    default: bool,
) -> Flow<bool> {
    loop {
        match pr.confirm(screen, prompt, default) {
            Conf::Yes => return Flow::Answered(true),
            Conf::No => return Flow::Answered(false),
            Conf::Cancel => return Flow::Cancel,
            Conf::Back => {
                if state.back_allowed() {
                    return Flow::Back;
                }
                pr.note(screen, "writes have started; back is off");
            }
        }
    }
}

fn pick_flow(
    state: &State,
    pr: &mut dyn Prompter,
    screen: &'static str,
    prompt: &str,
    options: &[&str],
    default: usize,
) -> Flow<usize> {
    loop {
        match pr.select(screen, prompt, options, default) {
            Pick::Ok(i) => return Flow::Answered(i),
            Pick::Cancel => return Flow::Cancel,
            Pick::Back => {
                if state.back_allowed() {
                    return Flow::Back;
                }
                pr.note(screen, "writes have started; back is off");
            }
        }
    }
}

fn ticks_flow(
    state: &State,
    pr: &mut dyn Prompter,
    screen: &'static str,
    prompt: &str,
    options: &[&str],
    defaults: &[usize],
) -> Flow<Vec<usize>> {
    loop {
        match pr.checklist(screen, prompt, options, defaults) {
            Ticks::Ok(v) => return Flow::Answered(v),
            Ticks::Cancel => return Flow::Cancel,
            Ticks::Back => {
                if state.back_allowed() {
                    return Flow::Back;
                }
                pr.note(screen, "writes have started; back is off");
            }
        }
    }
}

fn slot(state: &State) -> usize {
    state.preset.map(Preset::slot).unwrap_or(1)
}

/// Walk one screen. Skipped screens record and fall through in the same
/// call; a shown screen dispatches to its handler.
pub fn step(state: &mut State, pr: &mut dyn Prompter, pb: &mut dyn Probes) -> Next {
    while state.index < SCREENS.len() {
        let screen = SCREENS[state.index];
        if screen.when.holds(state) {
            break;
        }
        state.screens.push((screen.id, "skipped"));
        state.index += 1;
    }
    if state.index >= SCREENS.len() {
        return Next::Done;
    }
    let screen = SCREENS[state.index];
    state.screens.push((screen.id, "shown"));
    state.trail.push(screen.id);
    let stops = screen.stops[slot(state)];
    let outcome = match screen.id {
        "preflight" => screen_preflight(state, pr),
        "rerun" => screen_rerun(state, pr, stops),
        "preset" => screen_preset(state, pr),
        "look" => screen_look(state, pr, stops),
        "harnesses" => screen_harnesses(state, pr, pb, stops),
        "models" => screen_models(state, pr, pb, stops),
        "keys" => screen_keys(state, pr, stops),
        "budget" => screen_budget(state, pr, stops),
        "review-merge" => screen_review_merge(state, pr, stops),
        "services" => screen_services(state, pr, stops),
        "projects" => screen_projects(state, pr, stops),
        "join" => screen_join(state, pr, stops),
        "review" => screen_review(state, pr, stops),
        "apply" => screen_apply(state, pr),
        _ => screen_done(state, pr),
    };
    match outcome {
        Outcome::Forward => {
            state.index += 1;
            Next::Forward
        }
        Outcome::Back => {
            state.trail.pop();
            if let Some(last) = state.trail.last() {
                if let Some(pos) = SCREENS.iter().position(|s| s.id == *last) {
                    state.index = pos;
                }
            }
            Next::Back
        }
        Outcome::Cancel => Next::Cancel,
        Outcome::SkipToDone => Next::SkipToDone,
    }
}

fn screen_preflight(state: &State, pr: &mut dyn Prompter) -> Outcome {
    if state.detect.markers_present {
        pr.note(
            "preflight",
            "this machine already carries a setup marker; the run below only fills gaps",
        );
    } else {
        pr.note(
            "preflight",
            "clear: a terminal, a parseable config and a free run lock are the host's to check",
        );
    }
    Outcome::Forward
}

fn screen_rerun(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let options = [
        "Verify and keep",
        "Fill missing only",
        "Review every screen",
        "One section",
    ];
    if !stops {
        state
            .answers
            .insert("rerun", Answer::Text(options[0].into()));
        pr.note(
            "rerun",
            "verify and keep: every check reruns and nothing that passes is rewritten",
        );
        return Outcome::Forward;
    }
    match pick_flow(
        state,
        pr,
        "rerun",
        "How much should this pass redo?",
        &options,
        0,
    ) {
        Flow::Answered(i) => {
            let label = options.get(i).copied().unwrap_or(options[0]);
            state.answers.insert("rerun", Answer::Text(label.into()));
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

fn screen_preset(state: &mut State, pr: &mut dyn Prompter) -> Outcome {
    // This screen always asks: an auto-run cannot choose a posture
    // before the posture exists. The `when` gate keeps it off a rerun.
    let shown = state.preset.unwrap_or(Preset::Balanced);
    for (k, v) in expansion(shown) {
        pr.note("preset", &format!("{k}: {v}"));
    }
    let options = ["aggressive", "balanced", "user-reviewed"];
    match pick_flow(
        state,
        pr,
        "preset",
        "Which posture should this run follow?",
        &options,
        1,
    ) {
        Flow::Answered(i) => {
            let chosen = Preset::from_slot(i);
            state.preset = Some(chosen);
            state
                .answers
                .insert("preset", Answer::Text(chosen.name().into()));
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

fn screen_look(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    if !stops {
        state.answers.insert("look", Answer::Yes);
        pr.note(
            "look",
            &format!(
                "setup already looked: {} harness(es) on this machine",
                state.detect.harnesses.len()
            ),
        );
        return Outcome::Forward;
    }
    match confirm_flow(state, pr, "look", "May setup look at this machine?", true) {
        Flow::Answered(true) => {
            state.answers.insert("look", Answer::Yes);
            Outcome::Forward
        }
        Flow::Answered(false) => {
            state.detect.band = None;
            state.detect.env_keys.clear();
            state.detect.harnesses.clear();
            state.answers.insert("look", Answer::No);
            pr.note("look", "measurements dropped; nothing further is read here");
            Outcome::Forward
        }
        Flow::Back => Outcome::Back,
        Flow::Cancel => Outcome::Cancel,
    }
}

struct Opt {
    name: &'static str,
    label: String,
}

fn env_for(detect: &Detect, harness: &str) -> Option<&'static str> {
    detect
        .env_keys
        .iter()
        .find(|(h, _)| *h == harness)
        .map(|(_, e)| *e)
}

fn harness_options(detect: &Detect) -> Vec<Opt> {
    HARNESS_LIST
        .iter()
        .map(|h| {
            let by = detect
                .harnesses
                .iter()
                .find(|f| f.name == h.name)
                .map(|f| f.by)
                .unwrap_or("not looked for");
            Opt {
                name: h.name,
                label: format!("{} (found {})", h.label, by),
            }
        })
        .collect()
}

/// First pass: what the posture would tick. Re-show after a commit:
/// the committed ticks themselves.
fn harness_defaults(state: &State, options: &[Opt]) -> Vec<usize> {
    if let Some(Answer::Picked(picked)) = state.answers.get("harnesses") {
        return options
            .iter()
            .enumerate()
            .filter(|(_, o)| picked.iter().any(|p| p.as_str() == o.name))
            .map(|(i, _)| i)
            .collect();
    }
    options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            let found = state.detect.harnesses.iter().any(|f| f.name == o.name);
            let keyed = env_for(&state.detect, o.name).is_some();
            match state.preset {
                Some(Preset::Aggressive) => found,
                Some(Preset::UserReviewed) => false,
                Some(Preset::Balanced) | None => found && keyed,
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn screen_harnesses(
    state: &mut State,
    pr: &mut dyn Prompter,
    pb: &mut dyn Probes,
    stops: bool,
) -> Outcome {
    loop {
        let options = harness_options(&state.detect);
        let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
        let defaults = harness_defaults(state, &options);
        let idxs = if !stops {
            defaults
        } else {
            match ticks_flow(
                state,
                pr,
                "harnesses",
                "Which harnesses should footnote use?",
                &labels,
                &defaults,
            ) {
                Flow::Answered(v) => v,
                Flow::Back => return Outcome::Back,
                Flow::Cancel => return Outcome::Cancel,
            }
        };
        let names: Vec<&'static str> = idxs
            .iter()
            .filter_map(|i| options.get(*i).map(|o| o.name))
            .collect();
        // A zero-tick screen is a valid answer: footnote stays read-only
        // and the flow advances.
        if names.is_empty() {
            state
                .answers
                .insert("harnesses", Answer::Picked(Vec::new()));
            pr.note(
                "harnesses",
                "no harness picked; footnote stays read-only until one is wired",
            );
            return Outcome::Forward;
        }
        let mut passing: Vec<&'static str> = Vec::new();
        let mut queue: VecDeque<&'static str> = names.iter().copied().collect();
        let mut picked_another = false;
        while let Some(h) = queue.front().copied() {
            match pb.harness_check(h) {
                Probe::Pass { ms } => {
                    state.checks.push((h, "pass".into(), ms));
                    passing.push(h);
                    queue.pop_front();
                }
                Probe::Fail(reason) => {
                    state.checks.push((h, format!("fail:{reason}"), 0));
                    let mut opts: Vec<&str> = vec!["Retry", "Pick another", "Skip this one"];
                    if passing.is_empty() {
                        // With nothing passing yet, the person can save
                        // the run and finish later instead.
                        opts.push("Skip for now: save and finish later");
                    }
                    let pick = match pick_flow(
                        state,
                        pr,
                        "harnesses",
                        &format!("{h} check failed ({reason}). What now?"),
                        &opts,
                        0,
                    ) {
                        Flow::Answered(i) => i,
                        Flow::Back => return Outcome::Back,
                        Flow::Cancel => return Outcome::Cancel,
                    };
                    match pick {
                        0 => {} // probe the same harness again
                        1 => {
                            picked_another = true;
                            break;
                        }
                        2 => {
                            queue.pop_front();
                        }
                        _ => {
                            state.skip_all = true;
                            return Outcome::SkipToDone;
                        }
                    }
                }
            }
        }
        state.answers.insert(
            "harnesses",
            Answer::Picked(passing.iter().map(|h| h.to_string()).collect()),
        );
        if !picked_another {
            return Outcome::Forward;
        }
    }
}

fn screen_models(
    state: &mut State,
    pr: &mut dyn Prompter,
    pb: &mut dyn Probes,
    stops: bool,
) -> Outcome {
    if state.detect.models.is_empty() {
        state.answers.insert("models", Answer::Picked(Vec::new()));
        pr.note(
            "models",
            "no model catalog loaded; every harness keeps its current model",
        );
        return Outcome::Forward;
    }
    let mut entries: Vec<String> = Vec::new();
    for h in state.picked_harnesses() {
        let mut opts = state.detect.models.clone();
        opts.push("Leave unchanged".to_string());
        let committed = match state.answers.get("models") {
            Some(Answer::Picked(picked)) => picked
                .iter()
                .filter_map(|e| e.split_once('='))
                .find(|(name, _)| *name == h.as_str())
                .map(|(_, model)| model.to_string()),
            _ => None,
        };
        let default = committed
            .and_then(|m| opts.iter().position(|o| *o == m))
            .unwrap_or(opts.len() - 1);
        let choice = if !stops {
            Some(state.detect.models[0].clone())
        } else {
            let refs: Vec<&str> = opts.iter().map(|o| o.as_str()).collect();
            match pick_flow(
                state,
                pr,
                "models",
                &format!("Default model for {h}"),
                &refs,
                default,
            ) {
                Flow::Answered(i) if i + 1 == opts.len() => None,
                Flow::Answered(i) => opts.get(i).cloned(),
                Flow::Back => return Outcome::Back,
                Flow::Cancel => return Outcome::Cancel,
            }
        };
        let Some(model) = choice else { continue };
        match pb.model_check(&h, &model) {
            Probe::Pass { ms: _ } => entries.push(format!("{h}={model}")),
            Probe::Fail(reason) => pr.note(
                "models",
                &format!("{h} keeps its current model; {model} failed its check ({reason})"),
            ),
        }
    }
    state.answers.insert("models", Answer::Picked(entries));
    Outcome::Forward
}

fn screen_keys(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let committed: Vec<String> = match state.answers.get("keys") {
        Some(Answer::Picked(picked)) => picked.clone(),
        _ => Vec::new(),
    };
    let mut stored: Vec<String> = Vec::new();
    for h in state.picked_harnesses() {
        let Some(env) = env_for(&state.detect, &h) else {
            pr.note(
                "keys",
                &format!("no key env found for {h}; add it before apply"),
            );
            continue;
        };
        let keep = if !stops {
            true
        } else {
            let default = if committed.is_empty() {
                true
            } else {
                committed.iter().any(|e| e == env)
            };
            match confirm_flow(
                state,
                pr,
                "keys",
                &format!("Use existing {env} for {h}?"),
                default,
            ) {
                Flow::Answered(true) => true,
                Flow::Answered(false) => false,
                Flow::Back => return Outcome::Back,
                Flow::Cancel => return Outcome::Cancel,
            }
        };
        if keep {
            stored.push(env.to_string());
        }
    }
    state.answers.insert("keys", Answer::Picked(stored));
    Outcome::Forward
}

fn screen_budget(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let Some(band) = state.detect.band else {
        state.answers.insert("budget", Answer::No);
        pr.note(
            "budget",
            "no worker band was measured; the current cap stays",
        );
        return Outcome::Forward;
    };
    let default = match state.answers.get("budget") {
        Some(Answer::No) => false,
        _ => true,
    };
    let apply = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "budget",
            &format!("Live workers on this machine: apply the measured band of {band}?"),
            default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if apply {
        state
            .answers
            .insert("budget", Answer::Text(band.to_string()));
    } else {
        state.answers.insert("budget", Answer::No);
    }
    Outcome::Forward
}

fn screen_review_merge(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let posture_default = match state.answers.get("review") {
        Some(Answer::Text(t)) => t == "self_review",
        _ => true,
    };
    let posture = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "review-merge",
            "Keep the self-review posture before a PR merges?",
            posture_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let merge_default = match state.answers.get("merge") {
        Some(Answer::Yes) => true,
        Some(Answer::No) => false,
        _ => matches!(state.preset, Some(Preset::Aggressive)),
    };
    let merge = if !stops {
        merge_default
    } else {
        match confirm_flow(
            state,
            pr,
            "review-merge",
            "Merge PRs on their own when review and CI pass?",
            merge_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if posture {
        state
            .answers
            .insert("review", Answer::Text("self_review".into()));
    } else {
        // A prior pass may have seeded self_review; leaving it would keep
        // proposing the posture the user just declined.
        state.answers.remove("review");
        pr.note("review-merge", "the review posture stays as configured");
    }
    state
        .answers
        .insert("merge", if merge { Answer::Yes } else { Answer::No });
    Outcome::Forward
}

const SERVICE_IDS: [&str; 3] = ["pr-watcher", "auto-heal", "groom-agent"];
const SERVICE_LABELS: [&str; 3] = ["PR watcher", "Auto-heal", "Backlog groom"];

/// First pass: what the posture would tick. Re-show after a commit:
/// the committed ticks themselves.
fn service_defaults(state: &State) -> Vec<usize> {
    if let Some(Answer::Picked(picked)) = state.answers.get("services") {
        return SERVICE_IDS
            .iter()
            .enumerate()
            .filter(|(i, _)| picked.iter().any(|p| p.as_str() == SERVICE_IDS[*i]))
            .map(|(i, _)| i)
            .collect();
    }
    match state.preset {
        Some(Preset::Aggressive) => vec![0, 1, 2],
        Some(Preset::UserReviewed) => Vec::new(),
        Some(Preset::Balanced) | None => vec![0, 2],
    }
}

fn screen_services(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let defaults = service_defaults(state);
    let idxs = if !stops {
        defaults
    } else {
        match ticks_flow(
            state,
            pr,
            "services",
            "Run these in the background?",
            &SERVICE_LABELS,
            &defaults,
        ) {
            Flow::Answered(v) => v,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let picked: Vec<String> = idxs
        .iter()
        .filter_map(|i| SERVICE_IDS.get(*i))
        .map(|id| id.to_string())
        .collect();
    state.answers.insert("services", Answer::Picked(picked));
    Outcome::Forward
}

fn screen_projects(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let register_default = match state.answers.get("projects") {
        Some(Answer::No) => false,
        _ => true,
    };
    let register = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "projects",
            "Register this repo?",
            register_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    let scan_default = match state.answers.get("scan") {
        Some(Answer::Yes) => true,
        Some(Answer::No) => false,
        _ => matches!(state.preset, Some(Preset::Aggressive)),
    };
    let scan = if !register {
        false
    } else if !stops {
        scan_default
    } else {
        match confirm_flow(
            state,
            pr,
            "projects",
            "Scan your code folders for more repos?",
            scan_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if !register {
        pr.note(
            "projects",
            "this checkout stays unregistered; nothing else is scanned",
        );
    }
    state
        .answers
        .insert("projects", if register { Answer::Yes } else { Answer::No });
    state
        .answers
        .insert("scan", if scan { Answer::Yes } else { Answer::No });
    Outcome::Forward
}

fn screen_join(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    pr.note("join", "Nothing has changed yet.");
    for row in pending(state) {
        pr.note("join", &format!("{}: {} = {}", row.id, row.key, row.value));
    }
    let yes_default = !matches!(state.preset, Some(Preset::UserReviewed));
    let yes = if !stops {
        true
    } else {
        match confirm_flow(
            state,
            pr,
            "join",
            "Proceed with these changes?",
            yes_default,
        ) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if yes {
        Outcome::Forward
    } else {
        Outcome::Cancel
    }
}

fn screen_review(state: &mut State, pr: &mut dyn Prompter, stops: bool) -> Outcome {
    let rows = pending(state);
    if rows.is_empty() {
        pr.note("review", "nothing to write; the config stays as it is");
        return Outcome::Forward;
    }
    pr.note(
        "review",
        "one grouped write: every row below lands together",
    );
    for row in &rows {
        pr.note(
            "review",
            &format!("{}: {} = {}", row.id, row.key, row.value),
        );
    }
    let yes_default = !matches!(state.preset, Some(Preset::UserReviewed));
    let yes = if !stops {
        true
    } else {
        match confirm_flow(state, pr, "review", "Write these changes?", yes_default) {
            Flow::Answered(true) => true,
            Flow::Answered(false) => false,
            Flow::Back => return Outcome::Back,
            Flow::Cancel => return Outcome::Cancel,
        }
    };
    if yes {
        state.writes_started = true;
        Outcome::Forward
    } else {
        Outcome::Cancel
    }
}

fn screen_apply(state: &State, pr: &mut dyn Prompter) -> Outcome {
    for row in pending(state) {
        pr.progress("apply", &format!("applying {}", row.id));
    }
    pr.note(
        "apply",
        "the terminal host runs each step here, one result line per row",
    );
    Outcome::Forward
}

fn screen_done(state: &State, pr: &mut dyn Prompter) -> Outcome {
    if state.skip_all {
        pr.note(
            "done",
            "saved for later: the harness gate never passed, so the done marker is withheld",
        );
    } else {
        pr.note(
            "done",
            "the terminal host prints the report, the backup path and one next action here",
        );
    }
    Outcome::Forward
}

/// The rows a finished run would write, in the same shape setup run
/// applies. `skip_all` withholds everything - the harness gate never
/// passed. The keys row stores env var names only; a secret value never
/// appears here or anywhere in state.
pub fn pending(state: &State) -> Vec<Proposal> {
    let mut rows = Vec::new();
    if state.skip_all {
        return rows;
    }
    if let Some(Answer::Text(n)) = state.answers.get("budget") {
        rows.push(Proposal {
            id: "concurrency-band",
            key: "agents.max_live".into(),
            value: n.clone(),
            scope: "global",
            reason: "the measured live-worker band".into(),
            source: "wizard",
        });
    }
    if let Some(Answer::Text(t)) = state.answers.get("review") {
        if t == "self_review" {
            rows.push(Proposal {
                id: "review-merge",
                key: "review.posture".into(),
                value: "self_review".into(),
                scope: "global",
                reason: "the review posture this run chose".into(),
                source: "wizard",
            });
        }
    }
    if let Some(Answer::Yes) = state.answers.get("merge") {
        rows.push(Proposal {
            id: "review-merge",
            key: "auto_merge.enabled".into(),
            value: "true".into(),
            scope: "global",
            reason: "merge on green review and CI".into(),
            source: "wizard",
        });
        rows.push(Proposal {
            id: "review-merge",
            key: "auto_merge.grant".into(),
            value: "dispatch".into(),
            scope: "global",
            reason: "the dispatch grant behind auto-merge".into(),
            source: "wizard",
        });
    }
    if let Some(Answer::Picked(picked)) = state.answers.get("services") {
        for id in picked {
            let (id, key) = match id.as_str() {
                "pr-watcher" => ("pr-watcher", "pr_watch.enabled"),
                "auto-heal" => ("auto-heal", "auto_heal.enabled"),
                "groom-agent" => ("groom-agent", ""),
                _ => continue,
            };
            rows.push(Proposal {
                id,
                key: key.into(),
                value: "true".into(),
                scope: "global",
                reason: "the background service this run picked".into(),
                source: "wizard",
            });
        }
    }
    if let Some(Answer::Yes) = state.answers.get("projects") {
        rows.push(Proposal {
            id: "projects",
            key: String::new(),
            value: String::new(),
            scope: "project",
            reason: "register this checkout".into(),
            source: "wizard",
        });
    }
    rows
}

/// What each preset decides, shown on the preset screen before the pick.
pub fn expansion(preset: Preset) -> Vec<(&'static str, &'static str)> {
    match preset {
        Preset::Aggressive => vec![
            ("decides", "everything with safe answers"),
            ("harnesses", "every detected one"),
            ("models", "the first catalog lane per harness"),
            ("services", "all three"),
            ("auto-merge", "on after review and CI"),
        ],
        Preset::Balanced => vec![
            ("decides", "the safe middle"),
            ("harnesses", "the detected ones with credentials"),
            ("models", "each harness keeps its current model"),
            ("services", "the PR watcher and the groom"),
            ("auto-merge", "off"),
        ],
        Preset::UserReviewed => vec![
            ("decides", "nothing without you"),
            ("harnesses", "none ticked"),
            ("models", "each harness keeps its current model"),
            ("services", "none"),
            ("auto-merge", "off"),
        ],
    }
}

#[cfg(test)]
#[path = "wizard_tests.rs"]
mod wizard_tests;

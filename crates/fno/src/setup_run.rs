//! `fno config setup run`: the agent host of setup, over the one step table.
//!
//! The single step list both surfaces ask: the terminal prompt path prints
//! `Install X? It will do Y. [Y/n]` per action step, and the chat skill asks
//! the same rows through `--list --json` and replays the accepted ids with
//! `--only`. The no-prompt path (`--yes`, a non-TTY stdin, or an agent env)
//! takes every recommended default and never reads stdin, so an agent can
//! never block on a prompt.
//!
//! `--dry-run` is the read-only preview of the same table: it prints one
//! JSON object through `redact` and writes nothing, no marker included. A
//! `--json` caller gets a refused object with `status: refused` on bad
//! argv, never prose on stderr.
//!
//! Layers: `global` once per machine (harness wiring, gh login, the
//! concurrency band), `project` once per repo, `contributor` only in a
//! footnote source checkout. Setup writes only keys whose value differs
//! from the default (`config_defaults::differs`). Steps that would edit a
//! shell rc or a provider account print their command instead (`report`
//! kind), and a browser login is `human` kind: under `--yes` those land in
//! the report, never acted silently. On success each layer writes a done
//! marker; `--once` refuses to redo a fully marked run.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

/// The step rows the chat skill asks through `--list --json` and the
/// terminal asks one by one. `act` steps may write config through
/// `fno config set`; `report` steps only ever print a command; `human`
/// steps need a person at a browser or terminal.
#[derive(Debug)]
pub struct Step {
    pub id: &'static str,
    pub layer: Layer,
    pub kind: Kind,
    pub question: &'static str,
    pub effect: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Global,
    Project,
    Contributor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Act,
    /// Proposes, never runs bare: under `--yes` the step lands in the
    /// report's `proposed` list with its exact command, and it executes
    /// only when `--only` names its id. An installer tail that passes
    /// `--yes` therefore never loads a service.
    Approve,
    Report,
    Human,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
    #[default]
    Both,
}

/// One proposed write: what a step would set if accepted. `key` empty
/// means a command proposal - the exact command rides in `reason`.
#[derive(Debug)]
pub struct Proposal {
    pub id: &'static str,
    pub key: &'static str,
    pub value: String,
    pub scope: &'static str,
    pub reason: String,
    pub source: &'static str,
}

impl Proposal {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "id": self.id,
            "key": self.key,
            "value": self.value,
            "scope": self.scope,
            "reason": self.reason,
            "source": self.source,
        })
    }
}

/// Every row code and its fix line - the `next_action` the report names.
/// Append-only: a code lands here when its step lands, and [`row`] refuses
/// to build a code this table does not carry.
const CODES: &[(&str, &str)] = &[
    ("gh_not_signed_in", "gh auth login"),
    ("gh_scope_missing", "gh auth refresh -s repo -s workflow"),
    (
        "gh_unreadable",
        "install the GitHub CLI, then rerun `fno config setup run`",
    ),
    (
        "wiring_failed",
        "read the wiring line above, fix it, rerun `fno config setup run`",
    ),
    ("wiring_manual", "finish the named integration by hand"),
    (
        "config_set_refused",
        "run the `fno config set` line named in the message by hand",
    ),
];

/// One problem a run or a dry-run hit: which list it sat on, a stable
/// code, the human message, and the fix that resolves it.
#[derive(Debug)]
pub struct Row {
    list: &'static str,
    code: &'static str,
    message: String,
    fix: &'static str,
}

impl Row {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "list": self.list,
            "code": self.code,
            "message": self.message,
            "fix": self.fix,
        })
    }
}

/// Build a row, pulling its fix from [`CODES`]. A code missing from the
/// table is a programming error, not a runtime state.
fn row(list: &'static str, code: &'static str, message: String) -> Row {
    let fix = CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, f)| *f)
        .expect("row code missing from CODES");
    Row {
        list,
        code,
        message,
        fix,
    }
}

/// One run's outcome entries. The JSON report carries exactly these
/// thirteen fields, so an agent reads the same facts the human lines
/// print; `proposed`, `verified`, `rows` and `next_action` carry the
/// dry-run contract.
pub struct Report {
    done: Vec<String>,
    skipped: Vec<String>,
    needs_human: Vec<String>,
    paths: Vec<String>,
    restart_needed: bool,
    proposed: Vec<Proposal>,
    verified: Vec<String>,
    rows: Vec<Row>,
    next_action: String,
    schema: u8,
    run_id: String,
    host: &'static str,
    status: &'static str,
}

impl Default for Report {
    fn default() -> Self {
        Report {
            done: Vec::new(),
            skipped: Vec::new(),
            needs_human: Vec::new(),
            paths: Vec::new(),
            restart_needed: false,
            proposed: Vec::new(),
            verified: Vec::new(),
            rows: Vec::new(),
            next_action: String::new(),
            schema: 1,
            run_id: String::new(),
            host: "agent",
            status: "finished",
        }
    }
}

impl Report {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "schema": self.schema,
            "run_id": self.run_id,
            "host": self.host,
            "status": self.status,
            "done": self.done,
            "skipped": self.skipped,
            "needs_human": self.needs_human,
            "paths": self.paths,
            "restart_needed": self.restart_needed,
            "proposed": self.proposed.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
            "verified": self.verified,
            "rows": self.rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
            "next_action": self.next_action,
        })
    }
}

/// A short unique id for one run or refusal: nanos plus the pid's low
/// bits, hex. Not a security token - a correlate for logs and mails.
fn run_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}{:04x}", std::process::id() & 0xffff)
}

/// The first red row's fix when one exists, else the fallback (the
/// /fno:target line on a healthy machine).
fn next_action(rows: &[Row], fallback: &str) -> String {
    rows.iter()
        .find(|r| r.list == "needs_human" && !r.fix.is_empty())
        .map(|r| r.fix.to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// The one JSON object a `--json` caller reads on bad argv, in place of
/// the prose refusal. Same envelope, `status: refused`.
fn refused_json(message: &str) -> serde_json::Value {
    json!({
        "schema": 1,
        "run_id": run_id(),
        "host": "agent",
        "status": "refused",
        "message": message,
        "next_action": message,
    })
}

/// The step table. Order is the order both surfaces ask in.
pub const STEPS: &[Step] = &[
    Step {
        id: "harness-wiring",
        layer: Layer::Global,
        kind: Kind::Act,
        question: "Wire the fno plugin into every agent CLI on PATH?",
        effect: "installs the fno plugin into each detected agent CLI (claude, codex, gemini, opencode, pi, agy)",
    },
    Step {
        id: "gh-auth",
        layer: Layer::Global,
        kind: Kind::Human,
        question: "Sign in to the GitHub CLI?",
        effect: "run `gh auth login`; without it fno stays local-only (no PR, no review, no merge)",
    },
    Step {
        id: "concurrency-band",
        layer: Layer::Global,
        kind: Kind::Act,
        question: "Write this machine's measured concurrency band as agents.max_live?",
        effect: "measures the machine and writes agents.max_live only when it differs from the default",
    },
    Step {
        id: "accounts",
        layer: Layer::Global,
        kind: Kind::Report,
        question: "Provider accounts and rotation?",
        effect: "review later with `fno config accounts`; login needs a browser, so setup only names the verb",
    },
    Step {
        id: "guardrails",
        layer: Layer::Global,
        kind: Kind::Act,
        question: "Keep the standard guardrail preset?",
        effect: "writes guards.preset = standard only when the file carries a different value",
    },
    Step {
        id: "worktree-policy",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Pin a worktree policy for this repo?",
        effect: "writes worktree.policy; the recommended default is unset (harness-native behavior)",
    },
    Step {
        id: "plans-dir",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Keep .fno/plans/ as this repo's plans dir?",
        effect: "writes plans_dir only when this repo should keep plans somewhere else",
    },
    Step {
        id: "review-bots",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Name external review bots for this repo?",
        effect: "writes review.github_apps only when this repo wants bot reviews",
    },
    Step {
        id: "backlog-prefix",
        layer: Layer::Project,
        kind: Kind::Act,
        question: "Mint backlog node IDs as fn-<4 hex> for this repo?",
        effect: "writes backlog.id_prefix = fn and backlog.id_hex_width = 4 together when no prefix is configured",
    },
    Step {
        id: "cargo-toolchain",
        layer: Layer::Contributor,
        kind: Kind::Report,
        question: "Rust toolchain?",
        effect: "contributors need rustup's stable toolchain: `curl https://sh.rustup.rs -sSf | sh`",
    },
    Step {
        id: "crates-build",
        layer: Layer::Contributor,
        kind: Kind::Report,
        question: "Build the Rust crates?",
        effect: "run `cargo build --locked` at the checkout root; a release install never needs this",
    },
    Step {
        id: "cargo-bin-on-path",
        layer: Layer::Contributor,
        kind: Kind::Report,
        question: "Put ~/.cargo/bin on PATH?",
        effect: "add ~/.cargo/bin to PATH so the built binaries resolve; setup names the line, never edits the rc",
    },
];

/// This verb's argv, claimed lexically before clap: exactly
/// `config setup run` plus the flags `parse_opts` accepts. The sibling
/// `auto-wire` and `plan` spellings stay with their owners.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
    if words.len() != args.len() {
        return None;
    }
    match words.as_slice() {
        ["config", "setup", "run", ..] => Some(args[3..].to_vec()),
        _ => None,
    }
}

/// `--yes` / `--once` / `--scope` / `--only` / `--list` / `--json` /
/// `--dry-run` / `--scan-repos` / `--scan-dirs`, hand parsed: the verb is
/// claimed before clap, so the flags are ours. An unknown flag or value
/// refuses with the usage line and exit 2.
pub fn parse_opts(tail: &[OsString]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < tail.len() {
        let flag = tail[i].to_str().unwrap_or("");
        match flag {
            "--yes" => o.yes = true,
            "--once" => o.once = true,
            "--list" => o.list = true,
            "--json" | "-J" => o.json = true,
            "--dry-run" | "-N" => o.dry_run = true,
            "--scan-repos" => o.scan = true,
            "--scan-dirs" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--scan-dirs needs a comma list of directories")?;
                o.scan_dirs = v.split(',').map(|s| s.trim().to_string()).collect();
                o.scan = true;
            }
            "--scope" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--scope needs global|project|both")?;
                o.scope = match v {
                    "global" => Scope::Global,
                    "project" => Scope::Project,
                    "both" => Scope::Both,
                    other => return Err(format!("--scope: unknown value {other}")),
                };
            }
            "--only" => {
                i += 1;
                let v = tail
                    .get(i)
                    .and_then(|a| a.to_str())
                    .ok_or("--only needs a comma list of step ids")?;
                o.only = v.split(',').map(|s| s.trim().to_string()).collect();
            }
            _ => return Err(format!("unknown flag {flag}; {USAGE}")),
        }
        i += 1;
    }
    Ok(o)
}

/// The no-prompt decision, factored so a test passes each input: an
/// explicit `--yes`, a non-TTY stdin, or an agent session env takes the
/// recommended default for every step and never reads stdin.
fn no_prompt(yes: bool, stdin_is_tty: bool, agent_env: bool) -> bool {
    yes || !stdin_is_tty || agent_env
}

/// `CLAUDECODE` / `CLAUDE_CODE_CHILD_SESSION`: either names an agent
/// session, where a stdout prompt would hang the run forever.
fn agent_env() -> bool {
    ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION"]
        .iter()
        .any(|k| std::env::var_os(k).map(|v| !v.is_empty()).unwrap_or(false))
}

/// UTC now as `YYYY-MM-DDTHH:MM:SSZ`, no external time dep: days-from-civil
/// inverted (Hinnant), exact for any date a marker will carry.
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// This run's flags.
#[derive(Default)]
pub struct Opts {
    yes: bool,
    once: bool,
    scope: Scope,
    only: Vec<String>,
    list: bool,
    json: bool,
    dry_run: bool,
    scan: bool,
    scan_dirs: Vec<String>,
}

/// The argv tail `parse_opts` accepts, repeated for refusals.
const USAGE: &str = "usage: fno config setup run [--yes] [--once] [--scope global|project|both] [--only <id,...>] [--list] [--json] [--dry-run] [--scan-repos] [--scan-dirs <dir,...>]";

/// Every non-usage refusal exit: the same code the Python CLI uses for a
/// bad flag or value.
const EXIT_USAGE: i32 = 2;

/// The global layer's done marker: inside the state root's `sidecar/`
/// subfolder, the one owned per-item area the inventory already names, so
/// the root itself grows no new row.
fn global_marker() -> PathBuf {
    crate::model_catalog::state_dir()
        .join("sidecar")
        .join("setup-done")
}

/// The project layer's done marker, beside the repo's `.fno/config.toml`.
fn project_marker(root: &Path) -> PathBuf {
    root.join(".fno").join("setup.done")
}

/// Marker content: the fno version and the UTC stamp, so a reader can tell
/// a stale marker from a live one.
fn marker_line() -> String {
    format!("fno {} {}", env!("CARGO_PKG_VERSION"), utc_now())
}

/// The nearest repo root: `$FNO_REPO_ROOT` when pinned, else the nearest
/// ancestor holding a `.git` dir, else the working directory. The same
/// resolution `config_defaults` applies to the project config file.
fn repo_root() -> PathBuf {
    if let Some(root) = std::env::var_os("FNO_REPO_ROOT").filter(|r| !r.is_empty()) {
        return PathBuf::from(root);
    }
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if cwd.join(".git").exists() {
            return cwd;
        }
        if !cwd.pop() {
            return PathBuf::from(".");
        }
    }
}

/// A footnote source checkout: the fno crate manifest under `crates/`.
/// Contributors get the three report steps; a release install never sees
/// them. One joined path, not chained `join("fno")` literals: the
/// seam-crossings ratchet reads that literal shape as a porcelain resolver.
fn in_source_checkout(cwd: &Path) -> bool {
    cwd.join("crates/fno/Cargo.toml").is_file()
}

/// Write one or more `key=value` pairs through `fno config set` (the
/// validated writer: coercion, schema, reserved families) as ONE call, so
/// a multi-key pair lands atomically: any refused value leaves the file
/// unchanged. Global scope by default, `--local` for the project layer.
/// Returns the config file written, so the report's paths stay absolute.
fn config_set(exe: &Path, pairs: &[(&str, &str)], local: bool) -> Result<PathBuf, String> {
    let mut argv: Vec<OsString> = vec![exe.to_path_buf().into(), "config".into(), "set".into()];
    for (key, value) in pairs {
        argv.push(format!("{key}={value}").into());
    }
    if local {
        argv.push("--local".into());
    }
    let keys = pairs.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(" ");
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = cmd
        .spawn()
        .map_err(|e| format!("fno config set {keys}: {e}"))?
        .wait_with_output()
        .map_err(|e| format!("fno config set {keys}: {e}"))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "`fno config set {keys}` exited {}: {detail}",
            out.status.code().unwrap_or(-1)
        ));
    }
    if local {
        Ok(repo_root().join(".fno").join("config.toml"))
    } else {
        Ok(crate::model_catalog::state_dir().join("config.toml"))
    }
}

/// Fold one auto-wire outcome line into the report. The verb's lines are
/// stable human text pinned by its own tests: `Label: installed`,
/// `Label: already installed`, `needs a manual finish`, `FAILED`.
fn fold_wire_line(line: &str, rep: &mut Report) {
    let t = line.trim();
    if t.is_empty() || t.starts_with("wired the fno plugin") {
        return;
    }
    if t.contains("FAILED") || t.contains("needs a manual finish") {
        let message = format!("harness-wiring: {t}");
        let code = if t.contains("FAILED") {
            "wiring_failed"
        } else {
            "wiring_manual"
        };
        rep.rows.push(row("needs_human", code, message.clone()));
        rep.needs_human.push(message);
    } else if t.contains(": installed") || t.contains(": already installed") {
        rep.done.push(format!("harness-wiring: {t}"));
        // A fresh install asks for a restart; an "already installed" line
        // changed nothing, so it must not.
        if !t.contains("already") {
            rep.restart_needed = true;
        }
    } else {
        rep.skipped.push(format!("harness-wiring: {t}"));
    }
}

/// The gh login probe: authenticated is skipped, otherwise the step is the
/// report's one needs-human row, because login opens a browser.
fn step_gh_auth(rep: &mut Report) {
    let out = std::process::Command::new("gh")
        .arg("auth")
        .arg("status")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            rep.skipped
                .push("gh-auth: gh is already authenticated".into());
        }
        Ok(o) => {
            let detail = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let message = format!(
                "gh-auth: run `gh auth login` - login opens a browser; without it fno stays local-only (no PR, no review, no merge){}",
                if detail.is_empty() { String::new() } else { format!(" ({detail})") }
            );
            rep.rows
                .push(row("needs_human", "gh_not_signed_in", message.clone()));
            rep.needs_human.push(message);
        }
        Err(e) => {
            let message = format!(
                "gh-auth: run `gh auth login` - login opens a browser; gh itself is missing ({e})"
            );
            rep.rows
                .push(row("needs_human", "gh_unreadable", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The proposal half of the harness-wiring step: what the auto-wire verb
/// would do, without running it. The run path runs the verb; `--dry-run`
/// prints this row. Empty `key` marks a command proposal.
fn propose_harness_wiring() -> Proposal {
    Proposal {
        id: "harness-wiring",
        key: "",
        value: String::new(),
        scope: "global",
        reason:
            "fno config setup auto-wire installs the fno plugin into every agent CLI found on PATH"
                .to_string(),
        source: "default",
    }
}

/// The skip line a no-sample machine reads: the daemon files the band
/// question on its own schedule, so setup says so instead of guessing.
const NO_BAND_SAMPLE: &str =
    "concurrency-band: no machine sample yet; the daemon files the band question within five minutes";

/// The proposal half of the concurrency-band step, pure of writes: the
/// band number from the machine sample's clause via the one shared
/// parser, or `None` with the skip reason pushed. The run path and
/// `--dry-run` both call it, so the proposal cannot drift from the write.
fn propose_concurrency_band(skips: &mut Vec<String>) -> Option<i64> {
    let band = crate::setup_detect::budget_clause()
        .and_then(|t| crate::setup_detect::parse_budget_clause(&t));
    match band {
        None => {
            skips.push(NO_BAND_SAMPLE.to_string());
            None
        }
        Some(n) => {
            let value = toml::Value::Integer(n);
            if !crate::config_defaults::differs("agents.max_live", &value) {
                skips.push(format!("concurrency-band: agents.max_live already {n}"));
                return None;
            }
            Some(n)
        }
    }
}

/// The concurrency band: the band number the machine sample printed,
/// written only when it differs from the configured value.
fn step_concurrency_band(exe: &Path, rep: &mut Report) {
    let mut skips = Vec::new();
    let Some(n) = propose_concurrency_band(&mut skips) else {
        rep.skipped.extend(skips);
        return;
    };
    match config_set(exe, &[("agents.max_live", &n.to_string())], false) {
        Ok(path) => {
            rep.done
                .push(format!("concurrency-band: agents.max_live = {n}"));
            rep.paths.push(path.display().to_string());
            rep.restart_needed = true;
        }
        Err(e) => {
            let message = format!("concurrency-band: {e}");
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The proposal half of the guardrails step: the recommended preset when
/// the global file carries a different value, else `None` with the skip
/// reason pushed. Shared by the run path and `--dry-run`.
fn propose_guardrails(skips: &mut Vec<String>) -> Option<Proposal> {
    let value = toml::Value::String("standard".into());
    if !crate::config_defaults::differs("guards.preset", &value) {
        skips.push("guardrails: guards.preset already standard".into());
        return None;
    }
    Some(Proposal {
        id: "guardrails",
        key: "guards.preset",
        value: "standard".to_string(),
        scope: "global",
        reason: "the standard guard preset turns on the refusal gates a fresh machine wants"
            .to_string(),
        source: "default",
    })
}

/// The guardrail preset step: recommended `standard`, written only when the
/// global file carries a different value.
fn step_guardrails(exe: &Path, rep: &mut Report) {
    let mut skips = Vec::new();
    let Some(p) = propose_guardrails(&mut skips) else {
        rep.skipped.extend(skips);
        return;
    };
    match config_set(exe, &[(p.key, &p.value)], false) {
        Ok(path) => {
            rep.done
                .push(format!("guardrails: {} = {}", p.key, p.value));
            rep.paths.push(path.display().to_string());
        }
        Err(e) => {
            let message = format!("guardrails: {e}");
            rep.rows
                .push(row("needs_human", "config_set_refused", message.clone()));
            rep.needs_human.push(message);
        }
    }
}

/// The proposal half of the backlog-prefix step: the fixed scheme (the
/// ruling: prefix `fn`, 4 hex digits, every repo) when no config file sets
/// the key yet, else `None` with the skip reason pushed. A configured
/// prefix is the project's own choice; setup never overwrites one.
fn propose_backlog_prefix(root: &Path, skips: &mut Vec<String>) -> Option<Proposal> {
    let global = crate::model_catalog::state_dir().join("config.toml");
    let project = root.join(".fno").join("config.toml");
    if crate::config_defaults::key_set(Some(&global), Some(&project), "backlog.id_prefix") {
        skips.push(
            "backlog-prefix: backlog.id_prefix is configured; setup never overwrites it".into(),
        );
        return None;
    }
    Some(Proposal {
        id: "backlog-prefix",
        key: "backlog.id_prefix",
        value: "fn".into(),
        scope: "project",
        reason: "the fixed scheme: nodes mint as fn-<4 hex>; the width rides the same write".into(),
        source: "default",
    })
}

/// Whether a config file carries any `backlog.` key, i.e. a `[backlog]`
/// table. An unreadable file counts as having one, the `key_set` rule:
/// setup never writes a config it cannot read.
fn backlog_table_exists(path: &Path) -> bool {
    match crate::config_defaults::read_flat(path) {
        Ok(Some(map)) => map.keys().any(|k| k.starts_with("backlog.")),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// The backlog-prefix step: the pair written so it is never half set, one
/// `config set` call per layer, each carrying both keys as `key=value`
/// tokens. Project-scoped always; the global file too when it has no
/// `[backlog]` table, so a session in a fresh worktree (its project file
/// is untracked and absent there) still mints the scheme. A configured
/// prefix in either file is the project's own choice; setup never
/// overwrites it.
fn step_backlog_prefix(exe: &Path, root: &Path, rep: &mut Report) {
    let mut skips = Vec::new();
    let Some(p) = propose_backlog_prefix(root, &mut skips) else {
        rep.skipped.extend(skips);
        return;
    };
    let pairs = [
        ("backlog.id_prefix", p.value.as_str()),
        ("backlog.id_hex_width", "4"),
    ];
    let global = crate::model_catalog::state_dir().join("config.toml");
    let mut targets: Vec<(bool, &str)> = vec![(true, "project")];
    if !backlog_table_exists(&global) {
        targets.push((false, "global"));
    }
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for (local, label) in targets {
        match config_set(exe, &pairs, local) {
            Ok(path) => paths.push(path),
            Err(e) => errors.push(format!("{label}: {e}")),
        }
    }
    if !paths.is_empty() {
        rep.done.push(format!(
            "backlog-prefix: backlog.id_prefix = {}, backlog.id_hex_width = 4",
            p.value
        ));
        rep.paths.extend(paths);
    }
    for e in errors {
        let message = format!("backlog-prefix: {e}");
        rep.rows
            .push(row("needs_human", "config_set_refused", message.clone()));
        rep.needs_human.push(message);
    }
}

/// The steps this run executes: the scope's layers, plus the contributor
/// rows when the repo root is a footnote source checkout (the root, not
/// the cwd: setup launched from a subdirectory still sees the crates),
/// filtered by `--only`. An `--only` id that names no selected step
/// refuses with the valid ids.
fn select_steps<'a>(opts: &'a Opts, root: &Path) -> Result<Vec<&'a Step>, String> {
    let layers = |l: Layer| match opts.scope {
        Scope::Global => l == Layer::Global,
        Scope::Project => l == Layer::Project,
        Scope::Both => l != Layer::Contributor,
    };
    let mut steps: Vec<&Step> = STEPS
        .iter()
        .filter(|s| {
            layers(s.layer)
                || (opts.scope != Scope::Global
                    && s.layer == Layer::Contributor
                    && in_source_checkout(root))
        })
        .collect();
    if !opts.only.is_empty() {
        for id in &opts.only {
            if !steps.iter().any(|s| s.id == id) {
                let valid: Vec<&str> = STEPS.iter().map(|s| s.id).collect();
                return Err(format!(
                    "--only: unknown step id {id} for this scope; valid ids: {}",
                    valid.join(", ")
                ));
            }
        }
        steps.retain(|s| opts.only.iter().any(|id| id == s.id));
    }
    Ok(steps)
}

/// The dry-run envelope: the seven keys the plan pins, assembled pure of
/// writes. `services` is lifted out before `detected` moves into the
/// literal, because `json!` evaluates its fields in order and a moved
/// binding cannot be read by a later field.
fn dry_run_json(
    detected: serde_json::Value,
    proposed: &[Proposal],
    rows: &[Row],
    needs_human: &[String],
    fallback: &str,
) -> serde_json::Value {
    let services = detected
        .get("services")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let key_gap = proposed.iter().filter(|p| !p.key.is_empty()).count();
    json!({
        "detected": detected,
        "proposed": proposed.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
        "services": services,
        "needs_human": needs_human,
        "key_gap": key_gap,
        "rows": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        "next_action": next_action(rows, fallback),
    })
}

/// `--dry-run`: the read-only preview of the same step table. Detection
/// runs once; the gh rows fold off what detection already found, so a
/// dry-run never re-probes. It prints one JSON object through `redact`
/// whether or not `--json` passed, writes nothing, takes no marker, and
/// has no in-repo gate, so a fresh HOME answers too.
fn dry_run(opts: &Opts, steps: &[&Step], root: &Path) -> i32 {
    let scan: Option<Vec<PathBuf>> = if !opts.scan {
        None
    } else {
        Some(opts.scan_dirs.iter().map(PathBuf::from).collect())
    };
    let detected = crate::setup_detect::detect(root, scan.as_deref());
    let mut proposed: Vec<Proposal> = Vec::new();
    let mut skips: Vec<String> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut needs_human: Vec<String> = Vec::new();
    for step in steps {
        match step.id {
            "harness-wiring" => proposed.push(propose_harness_wiring()),
            "concurrency-band" => {
                if let Some(n) = propose_concurrency_band(&mut skips) {
                    proposed.push(Proposal {
                        id: "concurrency-band",
                        key: "agents.max_live",
                        value: n.to_string(),
                        scope: "global",
                        reason: "the machine sample's measured band, so live agents stay inside it"
                            .to_string(),
                        source: "sample",
                    });
                }
            }
            "guardrails" => {
                if let Some(p) = propose_guardrails(&mut skips) {
                    proposed.push(p);
                }
            }
            "backlog-prefix" => {
                if let Some(p) = propose_backlog_prefix(root, &mut skips) {
                    proposed.push(p);
                }
            }
            _ => {}
        }
    }
    match detected
        .get("gh")
        .and_then(|g| g.get("status"))
        .and_then(serde_json::Value::as_str)
    {
        Some("unreadable") => {
            let error = detected["gh"]["error"].as_str().unwrap_or_default();
            let message = format!("gh-auth: gh itself is missing or unreadable ({error})");
            rows.push(row("needs_human", "gh_unreadable", message.clone()));
            needs_human.push(message);
        }
        Some("ok") => {
            let missing: Vec<String> = detected["gh"]["missing"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !missing.is_empty() {
                let message = format!(
                    "gh-auth: missing scopes {} - run `gh auth refresh -s repo -s workflow`",
                    missing.join(", ")
                );
                rows.push(row("needs_human", "gh_scope_missing", message.clone()));
                needs_human.push(message);
            }
        }
        _ => {}
    }
    let fallback = "fno config setup run --yes";
    let body = dry_run_json(detected, &proposed, &rows, &needs_human, fallback);
    println!(
        "{}",
        serde_json::to_string_pretty(&crate::setup_profile::redact(&body)).unwrap_or_default()
    );
    0
}

/// `--list`: the step table the chat skill asks from. `--list --json` is
/// the machine shape; plain `--list` prints one row per step.
fn print_list(opts: &Opts) {
    if opts.json {
        let arr: Vec<serde_json::Value> = STEPS
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "layer": match s.layer {
                        Layer::Global => "global",
                        Layer::Project => "project",
                        Layer::Contributor => "contributor",
                    },
                    "kind": match s.kind {
                        Kind::Act => "act",
                        Kind::Approve => "approve",
                        Kind::Report => "report",
                        Kind::Human => "human",
                    },
                    "question": s.question,
                    "effect": s.effect,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
        return;
    }
    for s in STEPS {
        println!(
            "{:<18} {:<12} {:<8} {}",
            s.id,
            layer_name(s.layer),
            kind_name(s.kind),
            s.question
        );
    }
}

fn layer_name(l: Layer) -> &'static str {
    match l {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Contributor => "contributor",
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Act => "act",
        Kind::Approve => "approve",
        Kind::Report => "report",
        Kind::Human => "human",
    }
}

/// Whether every layer this scope covers already carries its done marker.
fn all_markers_present(scope: Scope, root: &Path) -> bool {
    all_markers_present_in(
        scope,
        global_marker().exists(),
        project_marker(root).exists(),
    )
}

/// The pure core of [`all_markers_present`], so the per-layer conditions are
/// unit-testable without touching a real state dir: a layer outside this
/// scope never blocks, a covered layer needs its own marker.
fn all_markers_present_in(scope: Scope, global_done: bool, project_done: bool) -> bool {
    let global_ok = scope == Scope::Project || global_done;
    let project_ok = scope == Scope::Global || project_done;
    global_ok && project_ok
}

/// Write a layer's done marker (version + UTC stamp), parent dirs included.
fn write_marker(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, format!("{}\n", marker_line()));
}

/// Ask one yes/no question on the terminal, recommended default yes. EOF
/// reads as a decline so a closing pipe never wedges the run.
fn ask(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => false,
        Ok(_) => {
            let a = line.trim().to_lowercase();
            a.is_empty() || a == "y" || a == "yes"
        }
        Err(_) => false,
    }
}

/// The verb. `--list` prints the table; `--once` stops at its markers; the
/// no-prompt path takes every recommended default. A layer's marker is
/// written only when that layer left nothing needs-human, so `--once` never
/// papers over a blocked step. Exit 0 unless the argv was wrong.
pub fn run(tail: &[OsString]) -> i32 {
    // parse_opts has not run yet, so --json is read straight off the tail:
    // a --json caller gets every refusal as one JSON object on stdout,
    // never prose on stderr.
    let json_wanted = tail
        .iter()
        .filter_map(|a| a.to_str())
        .any(|a| a == "--json" || a == "-J");
    let refuse = |e: String| {
        if json_wanted {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::setup_profile::redact(&refused_json(
                    &format!("fno config setup run: {e}")
                )))
                .unwrap_or_default()
            );
        } else {
            eprintln!("fno config setup run: {e}");
        }
        EXIT_USAGE
    };
    let opts = match parse_opts(tail) {
        Ok(o) => o,
        Err(e) => return refuse(e),
    };
    if opts.list {
        print_list(&opts);
        return 0;
    }
    let root = repo_root();
    // Validate the argv (including --only) before --once can return early.
    let steps = match select_steps(&opts, &root) {
        Ok(s) => s,
        Err(e) => return refuse(e),
    };
    // The read-only preview runs before any --once gate: a dry run reports
    // even on an already-set-up machine, and writes nothing, marker included.
    if opts.dry_run {
        return dry_run(&opts, &steps, &root);
    }
    if opts.once && all_markers_present(opts.scope, &root) {
        let msg = format!("setup: already done ({})", global_marker().display());
        if opts.json {
            let mut rep = Report::default();
            rep.run_id = run_id();
            rep.next_action = "/fno:target \"<your first feature>\"".to_string();
            rep.skipped.push(msg);
            println!("{}", rep.to_json());
        } else {
            println!("{msg}");
        }
        return 0;
    }
    // The project and contributor layers answer only inside a git repo:
    // repo_root() falls back to the cwd, and setup must not litter an
    // arbitrary directory with .fno/.
    let in_repo = root.join(".git").exists();
    // A JSON stdout must stay pure: an interactive TTY never prompts into it.
    let prompting = !opts.json && !no_prompt(opts.yes, std::io::stdin().is_terminal(), agent_env());
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno"));
    let mut rep = Report::default();
    rep.run_id = run_id();
    if !in_repo && opts.scope != Scope::Global {
        rep.skipped.push(
            "project layer: not a git repo; the project and contributor steps need one".into(),
        );
    }
    if prompting && !opts.json {
        println!("fno config setup run - Enter accepts each recommended default.");
    }
    let mut declined = 0usize;
    for step in &steps {
        if !in_repo && step.layer != Layer::Global {
            continue;
        }
        // An approve step never runs bare: --yes leaves it in the report's
        // proposed list with its exact command, and it executes only when
        // --only names its id. It does not block the marker - the step did
        // not fail, it waits for a caller that asks for it.
        if step.kind == Kind::Approve && !opts.only.iter().any(|id| id == step.id) {
            rep.proposed.push(Proposal {
                id: step.id,
                key: "",
                value: String::new(),
                scope: layer_name(step.layer),
                reason: step.effect.to_string(),
                source: "default",
            });
            continue;
        }
        let go = match step.kind {
            Kind::Act if prompting => {
                let q = format!("Install {}? It will {}. [Y/n]", step.id, step.effect);
                ask(&q)
            }
            Kind::Act => true,
            _ => true,
        };
        if !go {
            rep.skipped.push(format!("{}: declined", step.id));
            declined += 1;
            continue;
        }
        match (step.layer, step.id) {
            (Layer::Global, "harness-wiring") => {
                let out = std::process::Command::new(&exe)
                    .args(["config", "setup", "auto-wire"])
                    .stdin(std::process::Stdio::null())
                    .output();
                match out {
                    Ok(o) => {
                        for line in String::from_utf8_lossy(&o.stdout).lines() {
                            fold_wire_line(line, &mut rep);
                        }
                    }
                    Err(e) => rep
                        .needs_human
                        .push(format!("harness-wiring: cannot run the wiring verb ({e})")),
                }
            }
            (Layer::Global, "gh-auth") => step_gh_auth(&mut rep),
            (Layer::Global, "concurrency-band") => step_concurrency_band(&exe, &mut rep),
            (Layer::Global, "accounts") => rep
                .skipped
                .push("accounts: review with `fno config accounts`; login needs a browser".into()),
            (Layer::Global, "guardrails") => step_guardrails(&exe, &mut rep),
            (Layer::Project, "worktree-policy") => rep
                .skipped
                .push("worktree-policy: left unset (harness-native default)".into()),
            (Layer::Project, "plans-dir") => rep
                .skipped
                .push("plans-dir: plans_dir stays at its default (.fno/plans/)".into()),
            (Layer::Project, "review-bots") => rep.skipped.push(
                "review-bots: review.github_apps left empty (no bot reviews requested)".into(),
            ),
            (Layer::Project, "backlog-prefix") => step_backlog_prefix(&exe, &root, &mut rep),
            (Layer::Contributor, "cargo-toolchain") => {
                let v = std::process::Command::new("cargo")
                    .arg("--version")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                rep.skipped.push(format!(
                    "cargo-toolchain: {}",
                    v.unwrap_or_else(
                        || "install rustup: curl https://sh.rustup.rs -sSf | sh".into()
                    )
                ));
            }
            (Layer::Contributor, "crates-build") => rep.skipped.push(format!(
                "crates-build: run `cargo build --locked` at {}",
                root.display()
            )),
            (Layer::Contributor, "cargo-bin-on-path") => {
                let on = std::env::var_os("PATH").unwrap_or_default();
                let hit = std::env::split_paths(&on).any(|d| d.ends_with(".cargo/bin"));
                rep.skipped.push(if hit {
                    "cargo-bin-on-path: ~/.cargo/bin is on PATH".into()
                } else {
                    "cargo-bin-on-path: add `export PATH=\"$HOME/.cargo/bin:$PATH\"` to your rc"
                        .into()
                });
            }
            _ => rep.skipped.push(format!("{}: no runner", step.id)),
        }
    }

    let global_ran = steps.iter().any(|s| s.layer == Layer::Global);
    let project_ran = steps.iter().any(|s| s.layer != Layer::Global && in_repo);
    // A layer's marker waits until nothing needs a human and nothing was
    // declined: `--once` never papers over a blocked or refused run. A
    // partial `--only` pass never writes one either - the steps it skipped
    // have not run, so a later full pass must still offer them. Step
    // runners are idempotent and differ-guarded, so the rerun is cheap.
    if opts.only.is_empty() && rep.needs_human.is_empty() && declined == 0 {
        if global_ran {
            write_marker(&global_marker());
        }
        if project_ran {
            write_marker(&project_marker(&root));
        }
    }
    if opts.json {
        rep.next_action = next_action(&rep.rows, "/fno:target \"<your first feature>\"");
        println!("{}", rep.to_json());
        return 0;
    }
    if !rep.done.is_empty() {
        println!("done:");
        for e in &rep.done {
            println!("  {e}");
        }
    }
    if !rep.skipped.is_empty() {
        println!("skipped:");
        for e in &rep.skipped {
            println!("  {e}");
        }
    }
    if !rep.needs_human.is_empty() {
        println!("needs a human:");
        for e in &rep.needs_human {
            println!("  {e}");
        }
    }
    if !rep.paths.is_empty() {
        println!("wrote:");
        for p in &rep.paths {
            println!("  {p}");
        }
    }
    let markers = global_ran
        .then(|| global_marker().display().to_string())
        .into_iter()
        .chain(project_ran.then(|| project_marker(&root).display().to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    println!("markers: {markers}");
    if rep.restart_needed {
        println!("a restart of any running agent session picks up the new wiring.");
    }
    0
}

#[cfg(test)]
#[path = "setup_run_tests.rs"]
mod tests;

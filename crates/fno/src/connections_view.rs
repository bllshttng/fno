//! The mux Connections modal : a stateful overlay listing managed
//! provider accounts + combos, driving register/use/remove/add and combo-order
//! edits through the `fno config accounts` CLI. The UI is a thin wrapper over that CLI
//! - it never writes provider records, combos, or runtime state directly (one
//! writer). Opened from the sideline MENU (`AuxAction::OpenConnections`).
//!
//! Structure mirrors the client's other stateful overlays (peek/nav): all pure
//! logic (JSON parse, state machine, render, key reducer) lives here and is unit
//! tested; `client.rs` owns only the thin async wiring (shell-out spawn, key
//! routing, compose branch). Reads are async fail-open shell-outs
//! (`needs_overlay::fold_now` idiom); a failed read degrades the modal, never
//! crashes it.

use crate::theme::type_scale;
use serde::Deserialize;
use std::time::Duration;

// Reuse the server's `fno` binary resolver ($FNO_BIN, else the running exe) so
// the modal's shell-outs and the server never drift (PR #421 review).
use crate::server::fno_bin;

/// Fail-open budget for a phase-1 read shell-out: a read slower than this
/// degrades the modal with a visible banner (AC2-ERR), never blocks the UI
/// loop. Phase 1 starts three `fno` Python processes at once; on 2026-10-22
/// at load 63 they took 1.1 to 1.5 s each warm, so 1.5 s failed the first open.
const READ_TIMEOUT: Duration = Duration::from_secs(8);

/// How often an idle, open Accounts tab re-measures usage: the usage cache's
/// own 300 s freshness window. Each re-measure probes every claude account's
/// usage endpoint, so a faster tick multiplies those calls while nobody
/// presses a key. Opening the tab and R still re-measure at once, and the AGE
/// column says how old each reading is.
const USAGE_TICK: Duration = Duration::from_secs(300);

/// Budget for the phase-2 identity read (plain `config accounts list`, whose
/// last column is the identity cell). Wider than phase 1: the binding fold
/// costs one binding read per claude record (a profile call on a cold cache),
/// and identity is enrichment that lands after the rows are visible.
const IDENTITY_TIMEOUT: Duration = Duration::from_millis(5000);

/// One account record, as emitted by `fno config accounts list -J` (task 1.1).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// The harness (CLI binary) this account is an instance of. `-J` emits
    /// `harness`; the alias keeps a pre-rename `fno` on PATH deserializable,
    /// since this struct is a wire contract with a separately-deployed binary
    /// and the two are not upgraded atomically.
    #[serde(rename = "harness", alias = "cli")]
    pub harness: String,
    pub auth: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub active: bool,
    /// Headroom state name (ok/low/exhausted/unknown). `unknown` is a
    /// first-class value rendered distinctly, never as healthy.
    #[serde(default = "unknown_headroom")]
    pub headroom: String,
    /// Snapshot age label for managed accounts; None for oauth/api-key records.
    /// Today's `-J` names it `cred-snapshot`; the alias is what fills LOGIN.
    #[serde(default, alias = "cred-snapshot")]
    pub snapshot: Option<String>,
    /// The record has its own config dir, so `use` refuses it. An older `fno`
    /// omits the field, and that CLI refusal still blocks the swap.
    #[serde(default)]
    pub spawn_only: bool,
    /// The vendor the record bills, when it is not its harness's own (a
    /// route or a base URL). None reads as the harness default.
    #[serde(default)]
    pub provider: Option<String>,
    /// The email the saved login proved at sign-in. The row masks it.
    #[serde(default)]
    pub email: Option<String>,
    /// The plan the account is on (`Max 20x`, `Pro`, a record's own `plan`).
    #[serde(default)]
    pub plan: Option<String>,
    /// The identity cell parsed from plain `list`'s own last column (the
    /// phase-2 read), taken verbatim, problems and all. None until it lands.
    #[serde(skip)]
    pub identity_cell: Option<String>,
    /// The usage cell from `config accounts usage -J`, cached read first and
    /// the live `--refresh` read after it. None until either lands.
    #[serde(skip)]
    pub usage: Option<UsageCell>,
}

fn unknown_headroom() -> String {
    "unknown".to_string()
}

/// One account's usage, rendered: each window's used percent, or the reason
/// there is none in plain words. `relogin` marks a reason only a fresh login
/// repairs, which turns `r` into the login flow for that row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCell {
    pub text: String,
    pub relogin: bool,
    /// When the reading was taken (epoch secs), for the read age.
    pub probed_at: Option<u64>,
    /// Each measured window, for the usage bars. Empty on an unknown.
    pub windows: Vec<UsageWindow>,
}

/// One measured usage window: its label (`5h`, `weekly`, a model), the
/// rounded used percent, and when it resets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    pub label: String,
    pub used_pct: u32,
    pub resets_at: Option<u64>,
}

/// Budget for the live usage read. Each claude account costs a profile
/// call and a usage call, so it gets more room than the cached reads.
const USAGE_REFRESH_TIMEOUT: Duration = Duration::from_secs(20);

/// One combo, as emitted by `fno config accounts combos list -J` (task 1.1 added the
/// `active` field).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ComboRow {
    pub name: String,
    #[serde(default)]
    pub strategy: String,
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub active: bool,
}

/// The two tabs of the modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Accounts,
    Order,
}

/// Overall modal lifecycle. `Degraded` carries the read failure to show in the
/// banner; while degraded, mutation keys are disabled (R retries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalState {
    Loading,
    Ready,
    Degraded(String),
}

/// The result of a read fold. `Ok`/`Degraded` are the phase-1 fold (both
/// lists); `Identity`/`IdentityFailed` are the slower phase-2 read that lands
/// after the rows are already on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Ok {
        accounts: Vec<Account>,
        combos: Vec<ComboRow>,
    },
    Degraded(String),
    /// Phase 2 landed: `(id, identity cell)` pairs parsed from plain `list`,
    /// merged into the rows by id.
    Identity(Vec<(String, String)>),
    /// Phase 2 failed or blew its budget: every identity cell reads `?` and
    /// the modal stays Ready, never Degraded (identity is enrichment, not the
    /// list itself).
    IdentityFailed(String),
    /// A usage read landed: `(id, cell)` pairs merged into the rows by id.
    Usage(Vec<(String, UsageCell)>),
    /// The live usage read failed: cells keep the cached reading and the
    /// footer names the failure.
    UsageFailed(String),
}

/// What a key press asks the async wrapper to do. The reducer stays pure by
/// returning an intent instead of touching the socket; `client.rs` executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnIntent {
    /// State changed in place - repaint only.
    Redraw,
    /// Key had no effect (ring the bell).
    Bell,
    /// Esc: close the modal.
    Close,
    /// R: re-run the reads (spawn a fresh fold).
    Refresh,
    /// Run a single-flight `fno <argv>` mutation, then re-read. The reducer only
    /// yields this when not already acting (single-flight, Locked Decision 5).
    Run(Vec<String>),
    /// Run a single-flight `fno <argv>` mutation with extra child env (register
    /// needs `CLAUDE_CONFIG_DIR`), then re-read.
    RunEnv {
        argv: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// Spawn the interactive login pane via `fno mux pane run` (the documented
    /// PaneRun front door - zero proto bump). Not single-flight/re-read: it opens
    /// a pane and returns; the modal marks the id pending-login locally.
    SpawnLogin(Vec<String>),
    /// Set the client's session-local active account to the carried
    /// value (the post-toggle state: `Some(id)`, or `None` when toggled off).
    /// Shells NOTHING - the client mirrors this into its own `active_account`
    /// so later spawns append `--account`. Distinct from the `use` command (the
    /// global credential slot-swap); this only routes NEW spawns (Locked
    /// Decision 1).
    SetActiveAccount(Option<String>),
}

/// A session-local pending login: a wizard spawned its login pane but register
/// has not finalized yet. Rendered as a synthetic Accounts row; never persisted
/// (Locked Decision 6). `dir` is the claude config dir (empty for codex).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLogin {
    pub id: String,
    pub cli: String,
    pub dir: String,
}

/// The `a`dd wizard: collect an id, pick a cli/kind, and (claude) a config dir,
/// then spawn a login pane or run the api-key add. One wizard at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wizard {
    pub step: WizardStep,
    pub id: String,
    pub kind: AddKind,
    pub dir: String,
    /// api-key kind: the key value typed (base_url is the GLM preset).
    pub api_key: String,
    /// Provider step: the search text and the highlighted row.
    pub query: String,
    pub pick: usize,
    /// A catalog provider picked for a key (`AddKind::Provider`).
    pub provider: Option<String>,
    /// A local server picked for a base URL (`AddKind::Provider`).
    pub local: Option<crate::provider_picker::LocalServer>,
    pub base_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Provider,
    BaseUrl,
    Id,
    Kind,
    Dir,
    ApiKey,
}

/// The `n` new-combo form (Order tab): a name then a comma-separated member list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboInput {
    pub step: ComboInputStep,
    pub name: String,
    pub members: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComboInputStep {
    Name,
    Members,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddKind {
    /// Managed claude account: shares the ONE `~/.claude` slot (managed.py). The
    /// login runs in `~/.claude` and register snapshots that login into the
    /// managed store; `use` swaps the Keychain token back into `~/.claude`. All
    /// accounts reuse your one config (skills/hooks/plugins) - the default.
    Claude,
    /// Isolated claude account: its OWN `CLAUDE_CONFIG_DIR` (a separate config
    /// dir + Keychain item). Opt-in - most users want the shared slot above.
    ClaudeIsolated,
    Codex,
    ApiKeyGlm,
    /// A catalog provider or local server picked in the Provider step.
    /// Reached only through the picker, never the Kind cycle.
    Provider,
}

impl AddKind {
    fn cli(self) -> &'static str {
        match self {
            AddKind::Claude | AddKind::ClaudeIsolated => "claude",
            AddKind::Codex => "codex",
            AddKind::Provider => "opencode",
            // GLM is a claude-cli provider fronted by a base_url + api key.
            AddKind::ApiKeyGlm => "claude",
        }
    }
    fn label(self) -> &'static str {
        match self {
            AddKind::Claude => "claude (shared ~/.claude)",
            AddKind::ClaudeIsolated => "claude (isolated config dir)",
            AddKind::Codex => "codex (oauth login)",
            AddKind::ApiKeyGlm => "api-key: GLM/z.ai",
            AddKind::Provider => "api key or local server",
        }
    }
    fn next(self) -> AddKind {
        match self {
            AddKind::Claude => AddKind::ClaudeIsolated,
            AddKind::ClaudeIsolated => AddKind::Codex,
            AddKind::Codex => AddKind::ApiKeyGlm,
            AddKind::ApiKeyGlm | AddKind::Provider => AddKind::Claude,
        }
    }
}

/// The GLM/z.ai base_url preset (Claude's Discretion 4: preset-first is
/// fine), read from the compiled-in reach map.
fn glm_base_url() -> String {
    crate::model_catalog::vendor_base_url("zai", "anthropic")
        .expect("model_reach.toml carries the zai anthropic endpoint")
}

impl ConnectionsView {
    /// `accounts add` for a catalog provider's key: the opencode harness,
    /// the catalog's first key env, and a route that names the provider so
    /// the card groups the row. The route model is the catalog's first id.
    fn catalog_key_argv(&self, w: &Wizard, provider: &str) -> Vec<String> {
        let cp = self.catalog.as_ref().and_then(|c| c.get(provider));
        let env = cp
            .and_then(|cp| cp.env.first().cloned())
            .unwrap_or_default();
        let model = cp
            .and_then(|cp| cp.model_ids().into_iter().next())
            .unwrap_or_default();
        vec![
            "config".into(),
            "accounts".into(),
            "add".into(),
            w.id.clone(),
            "--harness".into(),
            "opencode".into(),
            "--auth".into(),
            "api_key".into(),
            "--route".into(),
            format!("{provider}/{model}"),
            "--env".into(),
            format!("{env}={}", w.api_key),
        ]
    }
}

/// The model ids a local server reports, read with a short-bound curl. An
/// unreachable server reads as no models.
fn probe_local_models(origin: &str, path: &str) -> Vec<String> {
    let url = format!("{}{path}", origin.trim().trim_end_matches('/'));
    std::process::Command::new("curl")
        .args(["-s", "-m", "2", &url])
        .output()
        .map(|o| crate::provider_picker::parse_local_models(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Validate a new-account id BEFORE any subprocess: lowercase alphanumeric +
/// hyphens, leading letter (the register command's `_ID_PATTERN` contract).
pub fn valid_account_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Expand a leading `~/` to the home dir. `env`/exec do NOT expand `~`, so a
/// literal `~/.claude-<id>` in `CLAUDE_CONFIG_DIR` would resolve against the
/// spawned process's cwd, not `$HOME`. Pure (home injected) for testability;
/// falls back to the input verbatim when there is no `~/` prefix or no home.
pub fn expand_tilde(path: &str, home: Option<&std::ffi::OsStr>) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(h) = home {
            let mut p = std::path::PathBuf::from(h);
            p.push(rest);
            return p.to_string_lossy().into_owned();
        }
    }
    if path == "~" {
        if let Some(h) = home {
            return std::path::PathBuf::from(h).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

/// Closes a `use` refusal that only a fresh login repairs. `r` reads it back.
const LOGIN_HINT: &str = "press r to log in";

/// A pending destructive action awaiting the operator's one-key confirm. `label`
/// is shown in the footer prompt; `argv` is what Enter runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConfirm {
    pub label: String,
    pub argv: Vec<String>,
    /// Enter opens `argv` in a pane, for a login that needs a terminal.
    pub pane: bool,
}

/// The Connections modal state. Owned by `View::connections` while open.
#[derive(Debug, Clone)]
pub struct ConnectionsView {
    pub tab: Tab,
    pub state: ModalState,
    pub accounts: Vec<Account>,
    pub combos: Vec<ComboRow>,
    /// Selection cursor in the Accounts tab.
    pub acct_sel: usize,
    /// Selection cursor over combos in the Order tab.
    pub combo_sel: usize,
    /// A single in-flight mutation guard (Locked Decision 5): a second action
    /// key while `acting` is a no-op with a footer notice.
    pub acting: bool,
    /// Footer notice (error tail / transient message), cleared on next action.
    pub notice: Option<String>,
    /// A pending remove confirm (Enter runs, any other key cancels). While
    /// `Some`, keys route to the confirm (AC1-ERR / destructive-guard).
    pub confirm: Option<PendingConfirm>,
    /// Member cursor within the selected combo's list (Order tab).
    pub member_sel: usize,
    /// Buffered member order for the selected combo while reordering (Order tab).
    /// `None` = viewing committed order; `Some` = dirty, committed on Enter,
    /// reverted on Esc / combo switch (AC2-FR: never a command per keystroke).
    pub dirty_order: Option<Vec<String>>,
    /// The `n` new-combo input form (name -> providers csv), Order tab.
    pub combo_input: Option<ComboInput>,
    /// The `a`dd wizard, `Some` while collecting inputs. Keys divert to it.
    pub wizard: Option<Wizard>,
    /// The models.dev catalog the Provider step searches, read when the
    /// wizard opens. None when no cache exists yet.
    pub catalog: Option<crate::model_catalog::Catalog>,
    /// Session-local pending logins, rendered as synthetic Accounts rows after
    /// the real records. Never persisted; register is the durable step.
    pub pending: Vec<PendingLogin>,
    /// Generation token, bumped per open/refresh so a read landing after a newer
    /// refresh (or a close) is discarded.
    pub gen: u64,
    /// The client's session-local active account, mirrored here for the
    /// "active for spawns" marker. Seeded from the client on open and re-set by
    /// the set-active toggle (`act_set_active`); the client reads the yielded
    /// `SetActiveAccount` intent back into its own authoritative copy. Never a
    /// credential mutation (Locked Decisions 1-2).
    pub active_account: Option<String>,
    /// Where the phase-2 identity read stands. Pending shows `…` in every
    /// identity cell; Ready shows each row's verdict; Failed shows `?` on
    /// every row while the modal stays Ready.
    pub identity_phase: IdentityPhase,
    /// Real records whose login pane `r` opened; the next `r` on that row
    /// saves the new login with `register`.
    pub relogin: std::collections::BTreeSet<String>,
    /// When this modal last armed a read, for the [`USAGE_TICK`] refresh.
    pub last_read: std::time::Instant,
    /// The rendered line the cursor fill covers, set while rendering. A row
    /// carries no cursor glyph: the fill is the cursor.
    cursor_row: std::cell::Cell<Option<usize>>,
    /// Per rendered line, the type-level spans it was drawn with, set while
    /// rendering. A line with no entry paints as plain body text.
    line_segs: std::cell::RefCell<std::collections::BTreeMap<usize, Vec<Seg>>>,
    /// Live workers per harness slot, set while rendering: the key reducer has
    /// no agent rows, and the `u` prompt names this count.
    slot_workers: std::cell::RefCell<std::collections::BTreeMap<String, usize>>,
}

/// The phase-2 identity read's lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityPhase {
    /// Rows are on screen; the slower identity read (plain `list`) is running.
    Pending,
    /// Phase 2 landed and merged into the rows.
    Ready,
    /// Phase 2 failed or blew its budget: cells read `?`, modal stays Ready.
    Failed,
}

impl ConnectionsView {
    /// Fresh modal in the loading state (reads are kicked by the caller).
    pub fn new() -> Self {
        Self {
            tab: Tab::Accounts,
            state: ModalState::Loading,
            accounts: Vec::new(),
            combos: Vec::new(),
            acct_sel: 0,
            combo_sel: 0,
            acting: false,
            notice: None,
            confirm: None,
            member_sel: 0,
            dirty_order: None,
            combo_input: None,
            wizard: None,
            catalog: None,
            pending: Vec::new(),
            gen: 0,
            active_account: None,
            identity_phase: IdentityPhase::Pending,
            relogin: Default::default(),
            last_read: std::time::Instant::now(),
            cursor_row: Default::default(),
            line_segs: Default::default(),
            slot_workers: Default::default(),
        }
    }

    /// Whether an idle Accounts tab is due its [`USAGE_TICK`] re-read. A
    /// true answer restarts the clock, so the caller arms one read per tick.
    /// A tab mid-action, mid-form or degraded waits for the operator.
    pub fn usage_due(&mut self) -> bool {
        let idle = self.tab == Tab::Accounts
            && self.state == ModalState::Ready
            && !self.acting
            && self.wizard.is_none()
            && self.confirm.is_none();
        if !idle || self.last_read.elapsed() < USAGE_TICK {
            return false;
        }
        self.last_read = std::time::Instant::now();
        true
    }

    /// Seed the "active for spawns" marker from the client's current
    /// session-local active account when the modal opens, so the marker is
    /// correct on first paint (the client owns the authoritative value across
    /// modal open/close).
    pub fn with_active_account(mut self, account: Option<String>) -> Self {
        self.active_account = account;
        self
    }

    /// The combo the Order-tab cursor is on, if any.
    fn selected_combo(&self) -> Option<&ComboRow> {
        self.combos.get(self.combo_sel)
    }

    /// The member order currently shown for the selected combo: the dirty buffer
    /// if reordering, else the committed order.
    fn shown_members(&self) -> Vec<String> {
        if let Some(dirty) = &self.dirty_order {
            return dirty.clone();
        }
        self.selected_combo()
            .map(|c| c.members.clone())
            .unwrap_or_default()
    }

    /// Total selectable Accounts rows: real records + session-local pending logins.
    fn accounts_len(&self) -> usize {
        self.accounts.len() + self.pending.len()
    }

    /// The pending login at Accounts-tab cursor `acct_sel`, if the cursor sits on
    /// a synthetic pending row (past the real records).
    pub fn selected_pending(&self) -> Option<&PendingLogin> {
        self.acct_sel
            .checked_sub(self.accounts.len())
            .and_then(|i| self.pending.get(i))
    }

    /// The account the Accounts-tab cursor is on, if any.
    pub fn selected_account(&self) -> Option<&Account> {
        self.accounts.get(self.acct_sel)
    }

    /// Apply a read fold under the caller's gen guard already checked. Clears the
    /// notice and clamps selections to the new list lengths.
    pub fn apply_read(&mut self, outcome: ReadOutcome) {
        match outcome {
            ReadOutcome::Ok {
                mut accounts,
                combos,
            } => {
                // A pending login that now has a real record has been registered;
                // drop its synthetic row (register is the durable step).
                let real: std::collections::HashSet<&str> =
                    accounts.iter().map(|a| a.id.as_str()).collect();
                self.pending.retain(|p| !real.contains(p.id.as_str()));
                // Keep the last usage reading on screen until the next one
                // lands, so a re-read never blanks the percents.
                for a in &mut accounts {
                    a.usage = self
                        .accounts
                        .iter()
                        .find(|old| old.id == a.id)
                        .and_then(|old| old.usage.clone());
                }
                // The tab groups rows by provider, and the cursor walks rows
                // in screen order, so the list takes that order.
                accounts.sort_by_key(|a| {
                    let key = provider_key(a);
                    (provider_rank(&key), key)
                });
                self.accounts = accounts;
                self.combos = combos;
                self.state = ModalState::Ready;
                // Fresh data is the committed truth; drop any stale reorder buffer.
                self.dirty_order = None;
                // Fresh rows carry no identity yet: the slower phase-2 read
                // follows and flips this to Ready/Failed when it lands.
                self.identity_phase = IdentityPhase::Pending;
                self.clamp_selection();
            }
            ReadOutcome::Degraded(reason) => {
                self.state = ModalState::Degraded(reason);
            }
            ReadOutcome::Identity(cells) => {
                // Merge by id; never touches selection, dirty_order, or view
                // state (enrichment, not a re-read).
                for (id, cell) in cells {
                    if let Some(a) = self.accounts.iter_mut().find(|a| a.id == id) {
                        a.identity_cell = Some(cell);
                    }
                }
                self.identity_phase = IdentityPhase::Ready;
            }
            ReadOutcome::IdentityFailed(_) => {
                self.identity_phase = IdentityPhase::Failed;
            }
            ReadOutcome::Usage(cells) => {
                for (id, cell) in cells {
                    if let Some(a) = self.accounts.iter_mut().find(|a| a.id == id) {
                        a.usage = Some(cell);
                    }
                }
            }
            ReadOutcome::UsageFailed(reason) => {
                self.notice = Some(format!("usage refresh failed: {reason}"));
            }
        }
    }

    fn clamp_selection(&mut self) {
        let alen = self.accounts_len();
        if self.acct_sel >= alen {
            self.acct_sel = alen.saturating_sub(1);
        }
        if self.combo_sel >= self.combos.len() {
            self.combo_sel = self.combos.len().saturating_sub(1);
        }
        let mlen = self.shown_members().len();
        if self.member_sel >= mlen {
            self.member_sel = mlen.saturating_sub(1);
        }
    }

    /// True while a typed field is open: the add wizard or the new-combo form.
    /// Arrows must not fold to `hjkl` there, or they type into the field.
    pub fn captures_text(&self) -> bool {
        self.wizard.is_some() || self.combo_input.is_some()
    }

    /// Pure key reducer. Returns the intent for the async wrapper to execute.
    /// Task 1.2 handles navigation + refresh + close; later tasks extend the
    /// intent set (actions, wizard, order commit).
    pub fn on_key(&mut self, key: u8) -> ConnIntent {
        // The add wizard, while open, captures all keys (typed input + toggles).
        if self.wizard.is_some() {
            return self.wizard_key(key);
        }
        // The new-combo form captures all keys while open.
        if self.combo_input.is_some() {
            return self.combo_input_key(key);
        }
        // A pending confirm captures all keys: Enter commits, anything else
        // cancels (no destructive action without an explicit confirm).
        if let Some(pending) = self.confirm.take() {
            if key == b'\r' || key == b'\n' {
                self.notice = None;
                if pending.pane {
                    return ConnIntent::SpawnLogin(pending.argv);
                }
                self.acting = true;
                return ConnIntent::Run(pending.argv);
            }
            self.notice = Some("cancelled".to_string());
            return ConnIntent::Redraw;
        }

        // A degraded modal disables everything but refresh + close (AC2-ERR).
        let degraded = matches!(self.state, ModalState::Degraded(_));
        // Esc discards an uncommitted reorder before it closes the modal (AC2-FR).
        if key == 0x1b {
            if self.dirty_order.take().is_some() {
                self.notice = Some("reorder discarded".to_string());
                return ConnIntent::Redraw;
            }
            return ConnIntent::Close;
        }
        match key {
            b'\t' => {
                self.tab = match self.tab {
                    Tab::Accounts => Tab::Order,
                    Tab::Order => Tab::Accounts,
                };
                // Switching tabs abandons an uncommitted reorder (AC2-FR).
                self.dirty_order = None;
                self.notice = None;
                ConnIntent::Redraw
            }
            b'R' => ConnIntent::Refresh,
            _ if degraded => ConnIntent::Bell,
            _ if self.tab == Tab::Accounts => self.accounts_key(key),
            _ => self.order_key(key),
        }
    }

    /// Accounts-tab keys (not degraded).
    fn accounts_key(&mut self, key: u8) -> ConnIntent {
        match key {
            b'j' => self.move_sel(1),
            b'k' => self.move_sel(-1),
            b'u' | b'\r' | b'\n' => self.act_use(),
            b's' => self.act_set_active(),
            b'd' => self.act_remove(),
            b'a' => {
                let key = self.selected_account().map(provider_key);
                self.catalog = crate::model_catalog::load(&crate::model_catalog::state_dir()).ok();
                let pick = key
                    .as_deref()
                    .and_then(|k| {
                        crate::provider_picker::entries(self.catalog.as_ref(), "")
                            .iter()
                            .position(|e| matches!(e, crate::provider_picker::Entry::Catalog { id, .. } if id == k))
                    })
                    .unwrap_or(0);
                self.wizard = Some(Wizard {
                    step: WizardStep::Provider,
                    id: String::new(),
                    kind: key.as_deref().map_or(AddKind::Claude, add_kind_for),
                    dir: String::new(),
                    api_key: String::new(),
                    query: String::new(),
                    pick,
                    provider: None,
                    local: None,
                    base_url: String::new(),
                });
                self.notice = None;
                ConnIntent::Redraw
            }
            b'r' => self.act_register(),
            _ => ConnIntent::Bell,
        }
    }

    /// Order-tab keys (not degraded): j/k move the member cursor, h/l switch
    /// combos, J/K reorder (buffered), Enter commits, space activates, d removes,
    /// n creates.
    fn order_key(&mut self, key: u8) -> ConnIntent {
        match key {
            b'h' | b'l' => {
                // Switch the selected combo; abandon any uncommitted reorder.
                let n = self.combos.len();
                if n == 0 {
                    return ConnIntent::Bell;
                }
                let delta: isize = if key == b'l' { 1 } else { -1 };
                let next = (self.combo_sel as isize + delta).clamp(0, n as isize - 1) as usize;
                if next == self.combo_sel {
                    return ConnIntent::Bell;
                }
                self.combo_sel = next;
                self.member_sel = 0;
                self.dirty_order = None;
                ConnIntent::Redraw
            }
            b'j' | b'k' => {
                let len = self.shown_members().len();
                if len == 0 {
                    return ConnIntent::Bell;
                }
                let delta: isize = if key == b'j' { 1 } else { -1 };
                let next = (self.member_sel as isize + delta).clamp(0, len as isize - 1) as usize;
                if next == self.member_sel {
                    return ConnIntent::Bell;
                }
                self.member_sel = next;
                ConnIntent::Redraw
            }
            b'J' | b'K' => self.reorder_member(if key == b'J' { 1 } else { -1 }),
            b'\r' | b'\n' => self.commit_reorder(),
            b' ' => self.act_activate_combo(),
            b'd' => self.act_remove_combo(),
            b'n' => {
                self.combo_input = Some(ComboInput {
                    step: ComboInputStep::Name,
                    name: String::new(),
                    members: String::new(),
                });
                self.notice = None;
                ConnIntent::Redraw
            }
            _ => ConnIntent::Bell,
        }
    }

    /// Buffer a member move at `member_sel` by `delta` (down=+1/up=-1). Seeds the
    /// dirty buffer from the committed order on the first move; the member cursor
    /// follows the moved member.
    fn reorder_member(&mut self, delta: isize) -> ConnIntent {
        let mut members = self.shown_members();
        let len = members.len();
        if len < 2 {
            return ConnIntent::Bell;
        }
        let from = self.member_sel;
        let to = from as isize + delta;
        if to < 0 || to >= len as isize {
            return ConnIntent::Bell; // at the edge
        }
        let to = to as usize;
        members.swap(from, to);
        self.member_sel = to;
        self.dirty_order = Some(members);
        ConnIntent::Redraw
    }

    /// Enter: commit a dirty reorder as one atomic `combos update` (AC4-HP). No
    /// dirty buffer -> a bell (nothing to commit); this is not a zero-feedback
    /// no-op (AC1-UI).
    fn commit_reorder(&mut self) -> ConnIntent {
        let Some(order) = self.dirty_order.take() else {
            return ConnIntent::Bell;
        };
        let Some(name) = self.selected_combo().map(|c| c.name.clone()) else {
            return ConnIntent::Bell;
        };
        if self.acting {
            self.dirty_order = Some(order);
            self.notice = Some("busy - one action at a time".to_string());
            return ConnIntent::Redraw;
        }
        self.acting = true;
        self.notice = None;
        ConnIntent::Run(vec![
            "config".into(),
            "accounts".into(),
            "combos".into(),
            "update".into(),
            name,
            "--providers".into(),
            order.join(","),
        ])
    }

    /// space: set the selected combo active. Refused with a notice when it has a
    /// dangling member (AC2-EDGE) - activating a broken combo is a footgun.
    fn act_activate_combo(&mut self) -> ConnIntent {
        if self.acting {
            self.notice = Some("busy - one action at a time".to_string());
            return ConnIntent::Redraw;
        }
        let known: std::collections::HashSet<&str> =
            self.accounts.iter().map(|a| a.id.as_str()).collect();
        let Some(combo) = self.selected_combo() else {
            return ConnIntent::Bell;
        };
        if let Some(missing) = combo.members.iter().find(|m| !known.contains(m.as_str())) {
            self.notice = Some(format!(
                "combo {} has a dangling member {missing} - fix the order first",
                combo.name
            ));
            return ConnIntent::Redraw;
        }
        let name = combo.name.clone();
        self.acting = true;
        self.notice = None;
        ConnIntent::Run(vec![
            "config".into(),
            "accounts".into(),
            "combos".into(),
            "use".into(),
            name,
        ])
    }

    /// d: remove the selected combo behind an Enter-confirm.
    fn act_remove_combo(&mut self) -> ConnIntent {
        if self.acting {
            self.notice = Some("busy - one action at a time".to_string());
            return ConnIntent::Redraw;
        }
        let Some(combo) = self.selected_combo() else {
            return ConnIntent::Bell;
        };
        let name = combo.name.clone();
        self.confirm = Some(PendingConfirm {
            pane: false,
            label: format!("remove combo {name}? (Enter=yes, any key=no)"),
            argv: vec![
                "config".into(),
                "accounts".into(),
                "combos".into(),
                "remove".into(),
                name,
            ],
        });
        ConnIntent::Redraw
    }

    /// Handle a key while the new-combo form is open (name -> members csv).
    fn combo_input_key(&mut self, key: u8) -> ConnIntent {
        if key == 0x1b {
            self.combo_input = None;
            self.notice = Some("new combo cancelled".to_string());
            return ConnIntent::Redraw;
        }
        let Some(ci) = self.combo_input.as_mut() else {
            return ConnIntent::Bell;
        };
        let field = match ci.step {
            ComboInputStep::Name => &mut ci.name,
            ComboInputStep::Members => &mut ci.members,
        };
        match key {
            b'\r' | b'\n' => match ci.step {
                ComboInputStep::Name => {
                    if ci.name.is_empty() {
                        self.notice = Some("combo name cannot be empty".into());
                        return ConnIntent::Redraw;
                    }
                    ci.step = ComboInputStep::Members;
                    ConnIntent::Redraw
                }
                ComboInputStep::Members => {
                    if ci.members.trim().is_empty() {
                        self.notice = Some("members cannot be empty".into());
                        return ConnIntent::Redraw;
                    }
                    let ci = self.combo_input.take().expect("combo_input present");
                    // Accept comma- or space-separated ids; normalize to CSV.
                    let members: Vec<&str> = ci
                        .members
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .collect();
                    self.acting = true;
                    self.notice = None;
                    ConnIntent::Run(vec![
                        "config".into(),
                        "accounts".into(),
                        "combos".into(),
                        "add".into(),
                        ci.name,
                        "--providers".into(),
                        members.join(","),
                    ])
                }
            },
            0x7f | 0x08 => {
                field.pop();
                ConnIntent::Redraw
            }
            0x15 => {
                field.clear();
                ConnIntent::Redraw
            }
            c if c.is_ascii_graphic() => {
                field.push(c as char);
                ConnIntent::Redraw
            }
            _ => ConnIntent::Bell,
        }
    }

    /// `u`: stage the switch to the selected account. Enter runs it. The scope
    /// is global because the slot is machine-wide: the project default would
    /// write the pointer into whatever directory launched this client.
    fn act_use(&mut self) -> ConnIntent {
        if self.acting {
            self.notice = Some("busy - one action at a time".to_string());
            return ConnIntent::Redraw;
        }
        let Some(acct) = self.selected_account() else {
            return ConnIntent::Bell;
        };
        if acct.active {
            self.notice = Some(format!("{} is already active", acct.id));
            return ConnIntent::Redraw;
        }
        if acct.spawn_only {
            self.notice = Some(format!(
                "{} has its own login dir and never takes the shared slot; use s: spawn-acct for new spawns",
                acct.id
            ));
            return ConnIntent::Redraw;
        }
        let workers = self
            .slot_workers
            .borrow()
            .get(&acct.harness)
            .copied()
            .unwrap_or(0);
        let label = use_prompt(acct, workers);
        let id = acct.id.clone();
        self.notice = None;
        self.confirm = Some(PendingConfirm {
            pane: false,
            label,
            argv: vec![
                "config".into(),
                "accounts".into(),
                "use".into(),
                id,
                "--scope".into(),
                "global".into(),
            ],
        });
        ConnIntent::Redraw
    }

    /// `s`: set the selected account as the session-local active account for
    /// NEW spawns (distinct from `u`se, the global credential slot-swap - Locked
    /// Decision 1). Toggle: pressing it on the already-active account clears
    /// back to the default (`None`). NOT single-flight-guarded (it mutates no
    /// credential and shells nothing); a non-account row bells. Always repaints
    /// the marker (AC1-UI: no zero-feedback keypress). Yields the post-toggle
    /// value so the client mirrors it into its authoritative `active_account`.
    fn act_set_active(&mut self) -> ConnIntent {
        let Some(acct) = self.selected_account() else {
            return ConnIntent::Bell;
        };
        // Only claude accounts are spawn-routable: mux dispatch always launches
        // provider="claude", and `resolve_account_overlay` is claude-only, so a
        // codex/gemini selection would reject every spawn until cleared (codex
        // P2). Refuse it here with visible feedback instead.
        if acct.harness != "claude" {
            self.notice = Some(format!("{} is not a claude account", acct.id));
            return ConnIntent::Redraw;
        }
        let id = acct.id.clone();
        self.active_account = if self.active_account.as_deref() == Some(id.as_str()) {
            None // toggle off: back to the default account
        } else {
            Some(id)
        };
        self.notice = Some(match &self.active_account {
            Some(id) => format!("spawns now bill {id}"),
            None => "spawns back to default account".to_string(),
        });
        ConnIntent::SetActiveAccount(self.active_account.clone())
    }

    /// `d`: stage a remove confirm for the selected account.
    fn act_remove(&mut self) -> ConnIntent {
        if self.acting {
            self.notice = Some("busy - one action at a time".to_string());
            return ConnIntent::Redraw;
        }
        let Some(acct) = self.selected_account() else {
            return ConnIntent::Bell;
        };
        let id = acct.id.clone();
        self.confirm = Some(PendingConfirm {
            pane: false,
            label: format!("remove account {id}? (Enter=yes, any key=no)"),
            argv: vec!["config".into(), "accounts".into(), "remove".into(), id],
        });
        ConnIntent::Redraw
    }

    /// `r`: register (finalize a pending login, or refresh a managed record).
    /// Single-flight; claude passes CLAUDE_CONFIG_DIR in the child env.
    fn act_register(&mut self) -> ConnIntent {
        if self.acting {
            self.notice = Some("busy - one action at a time".to_string());
            return ConnIntent::Redraw;
        }
        // A pending row: finalize its login into a managed record.
        if let Some(p) = self.selected_pending().cloned() {
            let argv = vec![
                "config".into(),
                "accounts".into(),
                "register".into(),
                p.id.clone(),
                "--harness".into(),
                p.cli.clone(),
            ];
            self.acting = true;
            self.notice = None;
            return if p.cli == "claude" && !p.dir.is_empty() {
                ConnIntent::RunEnv {
                    argv,
                    env: vec![("CLAUDE_CONFIG_DIR".into(), p.dir.clone())],
                }
            } else {
                ConnIntent::Run(argv)
            };
        }
        // A real managed record: re-register (refresh the snapshot).
        let Some(acct) = self.selected_account() else {
            return ConnIntent::Bell;
        };
        if acct.auth != "managed" {
            self.notice = Some(format!("{} is not a managed account", acct.id));
            return ConnIntent::Redraw;
        }
        // A config-dir account keeps its own login, away from the shared slot
        // that `use --login` writes.
        if acct.spawn_only {
            self.notice = Some(format!(
                "{} keeps its own login: sign in under its config dir",
                acct.id
            ));
            return ConnIntent::Redraw;
        }
        // The stored login is someone else's, or dead: a re-snapshot would
        // save the wrong login again, so open a real login pane first. The
        // next `r` on the row saves whatever that pane signed in.
        let refused = self
            .notice
            .as_deref()
            .is_some_and(|n| n.contains(LOGIN_HINT) && n.contains(&format!("'{}'", acct.id)));
        // The usage probe can miss a spent login ("not measured yet"), so a
        // `use` refusal on this row also counts.
        let needs_login = refused || acct.usage.as_ref().is_some_and(|u| u.relogin);
        let (id, harness) = (acct.id.clone(), acct.harness.clone());
        // Re-authenticate a claude login: the two-step vault login, one browser
        // login per Keychain item, each proven before the store write. It puts
        // the account on the shared slot, so it asks first.
        if harness == "claude" {
            let argv: Vec<String> = [
                "mux", "pane", "run", "fno", "config", "accounts", "use", &id, "--scope", "global",
                "--login",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            self.confirm = Some(PendingConfirm {
                label: format!(
                    "re-authenticate {id}? two browser logins in a pane, and {id} takes the \
                     shared claude login (Enter=yes, any key=no)"
                ),
                argv,
                pane: true,
            });
            self.notice = None;
            return ConnIntent::Redraw;
        }
        if needs_login && !self.relogin.contains(&id) {
            let login: Vec<String> = match harness.as_str() {
                "codex" => vec!["codex".into(), "login".into()],
                _ => {
                    self.notice = Some(format!("{id}: no login flow for {harness}"));
                    return ConnIntent::Redraw;
                }
            };
            self.relogin.insert(id.clone());
            self.notice = Some(format!(
                "login pane opened: sign in as {id} there (it swaps the shared {harness} login), then press r"
            ));
            let mut argv: Vec<String> = vec!["mux".into(), "pane".into(), "run".into()];
            argv.extend(login);
            return ConnIntent::SpawnLogin(argv);
        }
        self.relogin.remove(&id);
        let argv = vec![
            "config".into(),
            "accounts".into(),
            "register".into(),
            id,
            "--harness".into(),
            harness,
        ];
        self.acting = true;
        self.notice = None;
        ConnIntent::Run(argv)
    }

    /// Handle a key while the add wizard is open. Text steps: printable appends,
    /// Backspace pops, Enter advances, Esc cancels. The Kind step toggles.
    fn wizard_key(&mut self, key: u8) -> ConnIntent {
        if key == 0x1b {
            self.wizard = None;
            self.notice = Some("add cancelled".to_string());
            return ConnIntent::Redraw;
        }
        let Some(w) = self.wizard.as_mut() else {
            return ConnIntent::Bell;
        };
        match w.step {
            WizardStep::Provider => self.provider_step_key(key),
            WizardStep::BaseUrl => self.base_url_key(key),
            WizardStep::Id => match key {
                b'\r' | b'\n' => {
                    if !valid_account_id(&w.id) {
                        self.notice =
                            Some("id must be lowercase letters/digits/-, leading letter".into());
                        return ConnIntent::Redraw;
                    }
                    w.step = if w.local.is_some() {
                        WizardStep::BaseUrl
                    } else if w.provider.is_some() {
                        WizardStep::ApiKey
                    } else {
                        WizardStep::Kind
                    };
                    self.notice = None;
                    ConnIntent::Redraw
                }
                0x7f | 0x08 => {
                    w.id.pop();
                    ConnIntent::Redraw
                }
                0x15 => {
                    w.id.clear();
                    ConnIntent::Redraw
                }
                c if c.is_ascii_graphic() => {
                    w.id.push(c as char);
                    ConnIntent::Redraw
                }
                _ => ConnIntent::Bell,
            },
            WizardStep::Kind => match key {
                b'\t' | b' ' => {
                    w.kind = w.kind.next();
                    ConnIntent::Redraw
                }
                b'\r' | b'\n' => match w.kind {
                    // Shared managed slot: no dir - login runs in ~/.claude and
                    // register snapshots it (the token-swap model). dir stays "".
                    AddKind::Claude | AddKind::Codex => self.finish_login(),
                    // Isolated: prefill a per-account config dir, then collect it.
                    AddKind::ClaudeIsolated => {
                        w.dir = format!("~/.claude-{}", w.id);
                        w.step = WizardStep::Dir;
                        ConnIntent::Redraw
                    }
                    AddKind::ApiKeyGlm | AddKind::Provider => {
                        w.step = WizardStep::ApiKey;
                        ConnIntent::Redraw
                    }
                },
                _ => ConnIntent::Bell,
            },
            WizardStep::Dir => match key {
                b'\r' | b'\n' => {
                    if w.dir.is_empty() {
                        self.notice = Some("config dir cannot be empty".into());
                        return ConnIntent::Redraw;
                    }
                    self.finish_login()
                }
                0x7f | 0x08 => {
                    w.dir.pop();
                    ConnIntent::Redraw
                }
                0x15 => {
                    w.dir.clear();
                    ConnIntent::Redraw
                }
                c if c.is_ascii_graphic() => {
                    w.dir.push(c as char);
                    ConnIntent::Redraw
                }
                _ => ConnIntent::Bell,
            },
            WizardStep::ApiKey => match key {
                b'\r' | b'\n' => {
                    if w.api_key.is_empty() {
                        self.notice = Some("api key cannot be empty".into());
                        return ConnIntent::Redraw;
                    }
                    self.finish_api_key()
                }
                0x7f | 0x08 => {
                    w.api_key.pop();
                    ConnIntent::Redraw
                }
                0x15 => {
                    w.api_key.clear();
                    ConnIntent::Redraw
                }
                c if c.is_ascii_graphic() => {
                    w.api_key.push(c as char);
                    ConnIntent::Redraw
                }
                _ => ConnIntent::Bell,
            },
        }
    }

    /// Close the wizard, add a session-local pending row, and spawn the login
    /// pane via the `fno mux pane run` PaneRun front door. An empty `dir` (the
    /// default managed claude + codex) logs into the SHARED slot (no
    /// CLAUDE_CONFIG_DIR override, so ~/.claude and its skills/hooks are reused
    /// and register snapshots that login); a non-empty `dir` (isolated claude)
    /// logs into its own CLAUDE_CONFIG_DIR.
    fn finish_login(&mut self) -> ConnIntent {
        let w = self.wizard.take().expect("wizard present");
        let cli = w.kind.cli().to_string();
        // Expand `~/` NOW so the spawn + later register use an absolute dir
        // (env/exec never expand `~`). Empty stays empty (shared slot).
        let dir = if w.dir.is_empty() {
            String::new()
        } else {
            expand_tilde(&w.dir, std::env::var_os("HOME").as_deref())
        };
        self.pending.push(PendingLogin {
            id: w.id.clone(),
            cli: cli.clone(),
            dir: dir.clone(),
        });
        self.notice = Some(format!(
            "login pane opened for {} - press r when done",
            w.id
        ));
        let inner: Vec<String> = if cli == "claude" {
            if dir.is_empty() {
                // Shared ~/.claude slot: no CLAUDE_CONFIG_DIR override.
                vec!["claude".into(), "/login".into()]
            } else {
                vec![
                    "env".into(),
                    format!("CLAUDE_CONFIG_DIR={dir}"),
                    "claude".into(),
                    "/login".into(),
                ]
            }
        } else {
            vec!["codex".into(), "login".into()]
        };
        let mut argv = vec!["mux".into(), "pane".into(), "run".into()];
        argv.extend(inner);
        ConnIntent::SpawnLogin(argv)
    }

    /// A key in the Provider step: type to search, Up/Down (Ctrl-N/Ctrl-P) or Tab to
    /// move, Enter to pick. A popular provider with a login flow keeps its
    /// old kind; the other catalog providers take a key, the local servers
    /// a base URL.
    fn provider_step_key(&mut self, key: u8) -> ConnIntent {
        use crate::provider_picker::{entries, Entry};
        let Some(w) = self.wizard.as_mut() else {
            return ConnIntent::Bell;
        };
        let rows = entries(self.catalog.as_ref(), &w.query);
        match key {
            b'\t' | 0x0e => w.pick = (w.pick + 1).min(rows.len().saturating_sub(1)),
            0x10 => w.pick = w.pick.saturating_sub(1),
            0x7f | 0x08 => {
                w.query.pop();
                w.pick = 0;
            }
            0x15 => {
                w.query.clear();
                w.pick = 0;
            }
            b'\r' | b'\n' => {
                let Some(row) = rows.get(w.pick) else {
                    return ConnIntent::Bell;
                };
                match row {
                    Entry::Local(s) => {
                        w.local = Some(*s);
                        w.base_url = s.origin.to_string();
                        w.id = s.key.to_string();
                        w.kind = AddKind::Provider;
                    }
                    Entry::Catalog { id, .. } => {
                        let keyed = self
                            .catalog
                            .as_ref()
                            .and_then(|c| c.get(id))
                            .is_some_and(|cp| !cp.env.is_empty());
                        match id.as_str() {
                            "anthropic" | "openai" | "zai" => w.kind = add_kind_for(id),
                            _ if keyed => {
                                w.provider = Some(id.clone());
                                w.id = id.clone();
                                w.kind = AddKind::Provider;
                            }
                            _ => w.kind = add_kind_for(id),
                        }
                    }
                }
                w.step = WizardStep::Id;
            }
            c if c.is_ascii_graphic() || c == b' ' => {
                w.query.push(c as char);
                w.pick = 0;
            }
            _ => return ConnIntent::Bell,
        }
        ConnIntent::Redraw
    }

    /// A key in the BaseUrl step (local servers): edit the URL, Enter asks
    /// the server what models it holds and adds the account only when it
    /// answers with at least one.
    fn base_url_key(&mut self, key: u8) -> ConnIntent {
        let Some(w) = self.wizard.as_mut() else {
            return ConnIntent::Bell;
        };
        match key {
            0x7f | 0x08 => {
                w.base_url.pop();
            }
            0x15 => w.base_url.clear(),
            b'\r' | b'\n' => {
                let server = w.local.expect("base url step follows a local pick");
                let models = probe_local_models(&w.base_url, server.models_path);
                let Some(first) = models.first().cloned() else {
                    self.notice = Some(format!(
                        "no models from {}{} - is {} running?",
                        w.base_url.trim_end_matches('/'),
                        server.models_path,
                        server.title
                    ));
                    return ConnIntent::Redraw;
                };
                let w = self.wizard.take().expect("wizard present");
                self.acting = true;
                self.notice = None;
                return ConnIntent::Run(vec![
                    "config".into(),
                    "accounts".into(),
                    "add".into(),
                    w.id,
                    "--harness".into(),
                    "opencode".into(),
                    "--auth".into(),
                    "api_key".into(),
                    "--route".into(),
                    format!("{}/{first}", server.key),
                    "--env".into(),
                    format!(
                        "OPENAI_BASE_URL={}",
                        crate::provider_picker::openai_base(&w.base_url)
                    ),
                ]);
            }
            c if c.is_ascii_graphic() => w.base_url.push(c as char),
            _ => return ConnIntent::Bell,
        }
        ConnIntent::Redraw
    }

    /// Close the wizard and add the GLM api-key provider in one command.
    fn finish_api_key(&mut self) -> ConnIntent {
        let w = self.wizard.take().expect("wizard present");
        self.acting = true;
        self.notice = None;
        if let Some(provider) = w.provider.as_deref() {
            return ConnIntent::Run(self.catalog_key_argv(&w, provider));
        }
        ConnIntent::Run(vec![
            "config".into(),
            "accounts".into(),
            "add".into(),
            w.id,
            "--harness".into(),
            "claude".into(),
            "--auth".into(),
            "api_key".into(),
            "--env".into(),
            format!("ANTHROPIC_BASE_URL={}", glm_base_url()),
            "--env".into(),
            format!("ANTHROPIC_API_KEY={}", w.api_key),
        ])
    }

    fn move_sel(&mut self, delta: isize) -> ConnIntent {
        let (sel, len) = match self.tab {
            Tab::Accounts => (&mut self.acct_sel, self.accounts.len() + self.pending.len()),
            Tab::Order => (&mut self.combo_sel, self.combos.len()),
        };
        if len == 0 {
            return ConnIntent::Bell;
        }
        let next = (*sel as isize + delta).clamp(0, len as isize - 1) as usize;
        if next == *sel {
            return ConnIntent::Bell; // at the edge
        }
        *sel = next;
        ConnIntent::Redraw
    }

    /// Render the modal to overlay lines, padded to a uniform width so the
    /// inverse-video block is a clean rectangle (draw_lines_overlay does no
    /// padding of its own).
    pub fn render(&self) -> Vec<String> {
        self.render_with(&[], usize::MAX)
    }

    /// [`Self::render`] with the live layout rows, so each account names how
    /// many running workers bill it, and the overlay's text width, which the
    /// accounts table fits.
    pub fn render_with(&self, agents: &[crate::proto::AgentRow], max_cols: usize) -> Vec<String> {
        self.render_cursor(agents, max_cols).0
    }

    /// [`Self::render_with`] and the line the cursor fill covers.
    pub fn render_cursor(
        &self,
        agents: &[crate::proto::AgentRow],
        max_cols: usize,
    ) -> (Vec<String>, Option<usize>) {
        self.cursor_row.set(None);
        self.line_segs.borrow_mut().clear();
        let mut out: Vec<String> = Vec::new();
        out.push(self.tab_bar());
        out.push(String::new());
        match &self.state {
            ModalState::Loading => out.push("loading…".to_string()),
            ModalState::Degraded(reason) => {
                out.push(format!("could not read accounts: {reason}"));
                out.push("actions disabled - press R to retry".to_string());
            }
            ModalState::Ready => {
                if let Some(w) = &self.wizard {
                    self.render_wizard(w, &mut out);
                } else if let Some(ci) = &self.combo_input {
                    self.render_combo_input(ci, &mut out);
                } else {
                    match self.tab {
                        Tab::Accounts => self.render_accounts(&mut out, agents, max_cols),
                        Tab::Order => self.render_order(&mut out),
                    }
                }
            }
        }
        out.push(String::new());
        if let Some(confirm) = &self.confirm {
            out.push(format!("? {}", confirm.label));
        } else if self.acting {
            out.push("running…".to_string());
        } else {
            let mut footer = Spans::default();
            footer.add(&self.footer(), type_scale::META);
            self.push_spans(&mut out, footer);
        }
        if let Some(notice) = &self.notice {
            out.push(format!("! {notice}"));
        }
        (pad_block(out), self.cursor_row.get())
    }

    /// The rendered lines as framed body lines: each carries the type-level
    /// spans it was drawn with, and the cursor line its fill.
    pub fn render_body(
        &self,
        agents: &[crate::proto::AgentRow],
        max_cols: usize,
    ) -> (Vec<crate::chrome::BodyLine>, Option<usize>) {
        let (lines, cursor) = self.render_cursor(agents, max_cols);
        let segs = self.line_segs.borrow();
        let body = lines
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                // The two side-pad columns the string overlay adds itself.
                let mut line = crate::chrome::BodyLine::plain(text + "  ");
                if let Some(spans) = segs.get(&i) {
                    line.segs = spans.clone();
                }
                if cursor == Some(i) {
                    line.sel_span = Some((0, line.text.chars().count()));
                }
                line
            })
            .collect();
        (body, cursor)
    }

    /// Push a styled line, keeping its spans for [`Self::render_body`].
    fn push_spans(&self, out: &mut Vec<String>, line: Spans) {
        self.line_segs.borrow_mut().insert(out.len(), line.segs);
        out.push(line.text);
    }

    fn tab_bar(&self) -> String {
        let (a, o) = match self.tab {
            Tab::Accounts => ("[Accounts]", " Order "),
            Tab::Order => (" Accounts ", "[Order]"),
        };
        format!("{a}{o}")
    }

    /// The Accounts tab: one card per provider, in [`PROVIDERS`] order, then
    /// any other provider a record names. Each card carries the plain harness
    /// login where one exists, one row per saved credential with its usage
    /// bars under it, or an add hint when it holds none.
    fn render_accounts(
        &self,
        out: &mut Vec<String>,
        agents: &[crate::proto::AgentRow],
        max_cols: usize,
    ) {
        *self.slot_workers.borrow_mut() = self
            .accounts
            .iter()
            .map(|a| {
                (
                    a.harness.clone(),
                    slot_workers(&a.harness, &self.accounts, agents),
                )
            })
            .collect();
        if self.accounts.is_empty() && self.pending.is_empty() {
            out.push("no accounts registered - press a to add".to_string());
            out.push(String::new());
        }
        let now = epoch_now();
        let mut keys: Vec<String> = PROVIDERS.iter().map(|(k, _)| k.to_string()).collect();
        for a in &self.accounts {
            let key = provider_key(a);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for (n, key) in keys.iter().enumerate() {
            if n > 0 {
                out.push(String::new());
            }
            let mut title = Spans::default();
            title
                .add(&provider_title(key), type_scale::HEADING)
                .add("   a: add account", type_scale::META);
            self.push_spans(out, title);
            let rows: Vec<(usize, &Account)> = self
                .accounts
                .iter()
                .enumerate()
                .filter(|(_, a)| provider_key(a) == *key)
                .collect();
            if let Some(harness) = system_default_harness(key) {
                // The plain login the harness has with no saved account named.
                let mut line = Spans::default();
                line.add("  ", type_scale::BODY);
                if rows.iter().any(|(_, a)| a.active) {
                    line.add("        ", type_scale::BODY);
                } else {
                    line.add("[ACTIVE]", type_scale::BADGE);
                }
                line.add("  ", type_scale::BODY)
                    .add("System default", type_scale::NAME)
                    .add(
                        &format!("   your own {harness} login on this machine"),
                        type_scale::META,
                    );
                self.push_spans(out, line);
            }
            if rows.is_empty() {
                let mut line = Spans::default();
                line.add("            no saved account", type_scale::META);
                self.push_spans(out, line);
            }
            for (i, a) in rows {
                if i == self.acct_sel {
                    self.cursor_row.set(Some(out.len()));
                }
                let line = self.account_line(a, agents, now, i == self.acct_sel, max_cols);
                self.push_spans(out, line);
                for line in self.usage_lines(a, now) {
                    self.push_spans(out, line);
                }
            }
        }
        // Session-local pending logins render after the real records; `r`
        // registers the selected one into a managed account.
        for (j, p) in self.pending.iter().enumerate() {
            if self.accounts.len() + j == self.acct_sel {
                self.cursor_row.set(Some(out.len()));
            }
            out.push(format!(
                "            {}  {}  …login pending  (r: register)",
                p.id, p.cli
            ));
        }
    }

    /// One credential's line: the ACTIVE badge (the one new spawns use) or
    /// room for it, the id, its kind, the harness that runs it, the masked
    /// identity, live workers and the read age, then its keys. Rows that do
    /// not fit drop their keys; the selected row always keeps them.
    fn account_line(
        &self,
        a: &Account,
        agents: &[crate::proto::AgentRow],
        now: u64,
        selected: bool,
        max_cols: usize,
    ) -> Spans {
        let badge = if a.active { "[ACTIVE]" } else { "        " };
        // The session-local spawn account keeps its own `$` marker.
        let spawn = if self.active_account.as_deref() == Some(a.id.as_str()) {
            "$"
        } else {
            " "
        };
        let kind = match a.auth.as_str() {
            "api_key" => "API key",
            "managed" | "oauth" | "oauth_dir" => "subscription login",
            other => other,
        };
        let mut parts = vec![kind.to_string(), a.harness.clone()];
        let mut warning = None;
        // Only a warning from the identity cell reaches the row: a login that
        // serves someone else or is shared. An unproved token says nothing the
        // user can act on here.
        let cell = self.identity_cell(a);
        let kept: Vec<&str> = cell
            .split_whitespace()
            .filter(|t| !t.starts_with('?'))
            .collect();
        if kept.iter().any(|t| t.starts_with('!')) {
            let text = mask_emails(&kept.join(" "));
            warning = Some(text.clone());
            parts.push(text);
        }
        match workers_on(a, agents) {
            0 => {}
            1 => parts.push("1 worker".to_string()),
            w => parts.push(format!("{w} workers")),
        }
        let age = usage_age(a, now);
        if !age.is_empty() {
            parts.push(format!("read {age} ago"));
        }
        if let Some(snap) = a.snapshot.as_deref().filter(|s| !s.is_empty()) {
            parts.push(format!("login saved {snap} ago"));
        }
        // The account's own name: its plan and masked email, as a provider's
        // account page names it.
        let plan = match (a.plan.as_deref(), a.email.as_deref()) {
            (Some(plan), Some(email)) => format!("  {plan} ({})", mask_emails(email)),
            (Some(plan), None) => format!("  {plan}"),
            (None, Some(email)) => format!("  ({})", mask_emails(email)),
            (None, None) => String::new(),
        };
        // Each key names only what it does on this row.
        let mut keys = Vec::new();
        if a.spawn_only {
            keys.push("s: spawn-acct");
        } else if !a.active {
            keys.push("u: make active");
        }
        if a.auth == "managed" && !a.spawn_only {
            keys.push("r: re-authenticate");
        }
        keys.push("d: remove");
        // The first key is the row's main action (`u: make active` on a row
        // that is not active), and a row never sheds it.
        let main_key = keys[0];
        let keys = keys.join("  ");
        // A row wider than the overlay wraps and pushes the footer off, so a
        // wide row sheds the fields read least: the login age, then the read
        // age, then the kind. Only then does a row the cursor is not on drop
        // its other keys.
        let shed = |p: &String| -> usize {
            if p.starts_with("login saved") {
                1
            } else if p.starts_with("read ") {
                2
            } else if *p == kind {
                3
            } else {
                usize::MAX
            }
        };
        let line_at = |level: usize, keys: &str| -> Spans {
            let mut line = Spans::default();
            line.add(" ", type_scale::BODY).add(spawn, type_scale::BODY);
            if a.active {
                line.add(badge, type_scale::BADGE);
            } else {
                line.add(badge, type_scale::BODY);
            }
            line.add("  ", type_scale::BODY)
                .add(&a.id, type_scale::NAME)
                .add(&plan, type_scale::BODY)
                .add("  ", type_scale::BODY);
            let kept: Vec<&String> = parts.iter().filter(|p| shed(p) > level).collect();
            for (n, part) in kept.iter().enumerate() {
                if n > 0 {
                    line.add(" · ", type_scale::META);
                }
                let role = if warning.as_ref() == Some(*part) {
                    type_scale::WARN
                } else {
                    type_scale::META
                };
                line.add(part, role);
            }
            line.add("   ", type_scale::BODY)
                .add(keys, type_scale::META);
            line
        };
        for level in 0..=3 {
            let full = line_at(level, &keys);
            if crate::chrome::str_cols(&full.text) <= max_cols {
                return full;
            }
        }
        if selected {
            line_at(3, &keys)
        } else {
            line_at(3, main_key)
        }
    }

    /// The lines under a credential: one bar per measured window, or the
    /// plain-words reason there is no reading.
    fn usage_lines(&self, a: &Account, now: u64) -> Vec<Spans> {
        let meta = |text: String| {
            let mut line = Spans::default();
            line.add(&text, type_scale::META);
            line
        };
        let mut lines: Vec<Spans> = match &a.usage {
            Some(u) if !u.windows.is_empty() => {
                u.windows.iter().map(|w| window_spans(w, now)).collect()
            }
            _ => vec![meta(format!("              {}", self.usage_text(a)))],
        };
        if self.relogin.contains(&a.id) && a.usage.as_ref().is_some_and(|u| !u.windows.is_empty()) {
            lines.push(meta("              login open, r saves it".to_string()));
        }
        lines
    }

    /// The usage cell for one row: the latest reading, or the list's headroom
    /// word until one lands. A row whose login pane is open says what `r` does.
    fn usage_text(&self, a: &Account) -> String {
        let headroom = format!("headroom {}", a.headroom);
        let text = a.usage.as_ref().map_or(&headroom, |u| &u.text);
        if self.relogin.contains(&a.id) {
            format!("{text}; login open, r saves it")
        } else {
            text.clone()
        }
    }

    /// The identity cell for one row, per phase. The modal renders the cell
    /// `fno config accounts list` printed for the id verbatim, so one
    /// renderer (Python) states every token; Pending shows `…` and Failed or
    /// a missing phase-2 row shows `?`.
    fn identity_cell(&self, a: &Account) -> String {
        match self.identity_phase {
            IdentityPhase::Pending => "…".to_string(),
            IdentityPhase::Failed => "?".to_string(),
            IdentityPhase::Ready => a.identity_cell.clone().unwrap_or_else(|| "?".to_string()),
        }
    }

    /// Render the add-wizard input lines (replaces the list body while open).
    fn render_wizard(&self, w: &Wizard, out: &mut Vec<String>) {
        out.push("add account".to_string());
        if w.step == WizardStep::Provider {
            self.render_provider_step(w, out);
            return;
        }
        let field = |active: bool, label: &str, val: &str| {
            let caret = if active { "_" } else { "" };
            format!("  {label}: {val}{caret}")
        };
        // "id" read as an opaque identifier; "name" + a format hint
        // makes the lowercase/hyphen constraint (valid_account_id) visible
        // while typing instead of only surfacing on a rejected submit.
        let id_hint = if w.step == WizardStep::Id {
            "  (lowercase, a-z0-9-)"
        } else {
            ""
        };
        if w.step == WizardStep::Id {
            self.cursor_row.set(Some(out.len()));
        }
        out.push(format!(
            "{}{id_hint}",
            field(w.step == WizardStep::Id, "name", &w.id)
        ));
        if w.step != WizardStep::Id && w.provider.is_none() && w.local.is_none() {
            if w.step == WizardStep::Kind {
                self.cursor_row.set(Some(out.len()));
            }
            let kind_line = format!("  kind: {}  (Tab/Space to change)", w.kind.label(),);
            out.push(kind_line);
        }
        if w.step == WizardStep::Dir {
            self.cursor_row.set(Some(out.len()));
            out.push(field(true, "config dir", &w.dir));
        }
        if w.step == WizardStep::ApiKey {
            self.cursor_row.set(Some(out.len()));
            out.push(field(true, "api key", &w.api_key));
        }
        if w.step == WizardStep::BaseUrl {
            self.cursor_row.set(Some(out.len()));
            out.push(field(true, "base url", &w.base_url));
        }
        out.push(String::new());
        out.push("Enter: next   Esc: cancel".to_string());
    }

    /// The Provider step: the search line, then Popular, Local and the
    /// catalog rows, with a mark on each provider that has an account.
    fn render_provider_step(&self, w: &Wizard, out: &mut Vec<String>) {
        use crate::provider_picker::Entry;
        let mut search = Spans::default();
        search
            .add("  search: ", type_scale::META)
            .add(&format!("{}_", w.query), type_scale::BODY);
        self.push_spans(out, search);
        out.push(String::new());
        let rows = crate::provider_picker::entries(self.catalog.as_ref(), &w.query);
        if rows.is_empty() {
            let mut none = Spans::default();
            none.add("  no provider matches", type_scale::META);
            self.push_spans(out, none);
        }
        let connected: std::collections::HashSet<String> =
            self.accounts.iter().map(provider_key).collect();
        // A window that keeps the highlighted row on screen.
        let first = w.pick.saturating_sub(PICKER_ROWS - 1);
        let mut section = "";
        for (i, row) in rows.iter().enumerate().skip(first).take(PICKER_ROWS) {
            let head = match row {
                Entry::Catalog { popular: true, .. } => "Popular",
                Entry::Catalog { .. } => "Other",
                Entry::Local(_) => "Local",
            };
            if head != section {
                section = head;
                let mut line = Spans::default();
                line.add(&format!("  {head}"), type_scale::HEADING);
                self.push_spans(out, line);
            }
            if i == w.pick {
                self.cursor_row.set(Some(out.len()));
            }
            let mark = if i == w.pick { ">" } else { " " };
            let key = match row {
                Entry::Catalog { id, .. } => id.as_str(),
                Entry::Local(s) => s.key,
            };
            let dot = if connected.contains(key) { "●" } else { " " };
            let mut line = Spans::default();
            line.add(&format!("  {mark} {dot} "), type_scale::BODY)
                .add(row.title(), type_scale::BODY);
            self.push_spans(out, line);
        }
        out.push(String::new());
        let mut legend = Spans::default();
        legend.add("  ● connected account", type_scale::META);
        self.push_spans(out, legend);
        let mut keys = Spans::default();
        keys.add(
            "type: search  Up/Down or Tab: move  Enter: pick  Esc: cancel",
            type_scale::META,
        );
        self.push_spans(out, keys);
    }

    fn render_order(&self, out: &mut Vec<String>) {
        if self.combos.is_empty() {
            out.push("no combos - press n to add".to_string());
            return;
        }
        let known: std::collections::HashSet<&str> =
            self.accounts.iter().map(|a| a.id.as_str()).collect();
        for (i, c) in self.combos.iter().enumerate() {
            let selected = i == self.combo_sel;
            if selected {
                self.cursor_row.set(Some(out.len()));
            }
            let badge = if c.active { "●" } else { " " };
            out.push(format!(
                " {badge} {name}  [{strategy}]",
                name = c.name,
                strategy = c.strategy,
            ));
            if selected {
                // The selected combo expands its members in (dirty-or-committed)
                // order, with the member cursor and dangling flags.
                let members = self.shown_members();
                for (m, member) in members.iter().enumerate() {
                    if m == self.member_sel {
                        self.cursor_row.set(Some(out.len()));
                    }
                    let dangling = if known.contains(member.as_str()) {
                        ""
                    } else {
                        "  (dangling)"
                    };
                    out.push(format!("      {n}. {member}{dangling}", n = m + 1));
                }
                if self.dirty_order.is_some() {
                    out.push("      Enter: commit   Esc: discard".to_string());
                }
            }
        }
    }

    /// Render the new-combo form (name -> members csv).
    fn render_combo_input(&self, ci: &ComboInput, out: &mut Vec<String>) {
        out.push("new combo".to_string());
        let field = |active: bool, label: &str, val: &str| {
            let caret = if active { "_" } else { "" };
            format!("  {label}: {val}{caret}")
        };
        if ci.step == ComboInputStep::Name {
            self.cursor_row.set(Some(out.len()));
        }
        out.push(field(ci.step == ComboInputStep::Name, "name", &ci.name));
        if ci.step == ComboInputStep::Members {
            self.cursor_row.set(Some(out.len()));
            out.push(field(true, "members (comma/space)", &ci.members));
        }
        out.push(String::new());
        out.push("Enter: next   Esc: cancel".to_string());
    }

    fn footer(&self) -> String {
        match self.tab {
            Tab::Accounts => {
                "Tab: order  j/k: move  u/Enter: make active  r: re-authenticate  d: remove  a: add  s: spawn-acct  R: refresh"
                    .to_string()
            }
            Tab::Order => {
                "Tab: acct  h/l: combo  j/k: member  J/K: reorder  space: use  d: rm  n: new  Enter: commit"
                    .to_string()
            }
        }
    }
}

impl Default for ConnectionsView {
    fn default() -> Self {
        Self::new()
    }
}

/// Pad every line to the max width so the inverse block is a rectangle.
fn pad_block(mut lines: Vec<String>) -> Vec<String> {
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    for line in &mut lines {
        let pad = width - line.chars().count();
        line.push_str(&" ".repeat(pad));
    }
    lines
}

/// The providers the Accounts tab always lists, in this order, as
/// `(key, title)`. A provider with no saved credential still shows.
const PROVIDERS: [(&str, &str); 7] = [
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("zai", "zai"),
    ("bedrock", "Amazon Bedrock"),
    ("deepseek", "DeepSeek"),
    ("opencode", "OpenCode"),
    ("llama", "Llama"),
];

/// The provider key a row groups under: the vendor `list -J` names, else
/// its harness's own. A URL host the list fell back to maps to its key.
fn provider_key(a: &Account) -> String {
    let raw = a
        .provider
        .clone()
        .unwrap_or_else(|| match a.harness.as_str() {
            "claude" => "anthropic".to_string(),
            "codex" => "openai".to_string(),
            other => other.to_string(),
        })
        .to_ascii_lowercase();
    [
        ("z.ai", "zai"),
        ("bigmodel", "zai"),
        ("deepseek", "deepseek"),
        ("bedrock", "bedrock"),
        ("amazonaws", "bedrock"),
        ("llama", "llama"),
        ("opencode", "opencode"),
        ("openai", "openai"),
        ("anthropic", "anthropic"),
    ]
    .iter()
    .find(|(needle, _)| raw.contains(needle))
    .map_or(raw.clone(), |(_, key)| key.to_string())
}

fn provider_rank(key: &str) -> usize {
    PROVIDERS
        .iter()
        .position(|(k, _)| *k == key)
        .unwrap_or(PROVIDERS.len())
}

fn provider_title(key: &str) -> String {
    PROVIDERS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(key.to_string(), |(_, title)| title.to_string())
}

/// The harness whose plain login serves `key` with no saved account named.
fn system_default_harness(key: &str) -> Option<&'static str> {
    match key {
        "anthropic" => Some("claude"),
        "openai" => Some("codex"),
        _ => None,
    }
}

/// The add wizard's first kind for a provider's card.
fn add_kind_for(key: &str) -> AddKind {
    match key {
        "openai" => AddKind::Codex,
        "zai" => AddKind::ApiKeyGlm,
        _ => AddKind::Claude,
    }
}

/// `jason@readyrule.com` reads `j•••@r•••.com`: the first letter of the
/// name and of the domain, and the top-level domain.
fn mask_emails(text: &str) -> String {
    let mask = |word: &str| -> String {
        let Some((local, domain)) = word.split_once('@') else {
            return word.to_string();
        };
        let Some((host, tld)) = domain.rsplit_once('.') else {
            return word.to_string();
        };
        // Keep a leading mark such as `(`: only the address masks.
        let lead: String = local.chars().take_while(|c| !c.is_alphanumeric()).collect();
        let (Some(first), Some(h)) = (local[lead.len()..].chars().next(), host.chars().next())
        else {
            return word.to_string();
        };
        format!("{lead}{first}•••@{h}•••.{tld}")
    };
    text.split(' ').map(mask).collect::<Vec<_>>().join(" ")
}

/// Rows the Provider step shows at once.
const PICKER_ROWS: usize = 17;

/// Cells in a usage bar.
const BAR_CELLS: usize = 20;

/// How long a window runs, for its pace: `5h`, `7d`, `weekly`.
fn window_seconds(label: &str) -> Option<u64> {
    if label == "weekly" {
        return Some(7 * 86_400);
    }
    if !label.is_ascii() {
        return None;
    }
    let (num, unit) = label.split_at(label.len().checked_sub(1)?);
    let n: u64 = num.parse().ok()?;
    match unit {
        "h" => Some(n * 3600),
        "d" => Some(n * 86_400),
        _ => None,
    }
}

/// One usage window as a bar: label, fill, percent, time to reset, and
/// pace (ahead of or behind an even spend across the window).
#[cfg(test)]
fn window_line(w: &UsageWindow, now: u64) -> String {
    window_spans(w, now).text
}

/// [`window_line`] with its type levels: the label and the times dim, the
/// fill colored by how full it is, the percent bold.
fn window_spans(w: &UsageWindow, now: u64) -> Spans {
    let fill = (w.used_pct.min(100) as usize * BAR_CELLS + 50) / 100;
    let mut line = Spans::default();
    line.add("              ", type_scale::BODY)
        .add(&format!("{:<7}", w.label), type_scale::META)
        .add(" ", type_scale::BODY)
        .add(&"━".repeat(fill), type_scale::level(w.used_pct))
        .add(&"─".repeat(BAR_CELLS - fill), type_scale::META)
        .add(" ", type_scale::BODY)
        .add(&format!("{:>3}%", w.used_pct), type_scale::STRONG);
    let Some(at) = w.resets_at.filter(|at| *at > now) else {
        return line;
    };
    line.add(
        &format!("  resets in {}", crate::client::humanize_ago(at - now)),
        type_scale::META,
    );
    if let Some(span) = window_seconds(&w.label).filter(|span| at - now <= *span) {
        let even = (span - (at - now)) * 100 / span;
        let pace = match u64::from(w.used_pct) {
            p if p + 5 < even => "pace slower",
            p if p > even + 5 => "pace faster",
            _ => "on pace",
        };
        line.add(&format!("  {pace}"), type_scale::META);
    }
    line
}

/// One `(offset, len, role)` type-level span, char-indexed.
type Seg = (usize, usize, crate::theme::Role);

/// A line built span by span, each span at one type level.
#[derive(Debug, Default)]
struct Spans {
    text: String,
    segs: Vec<Seg>,
    chars: usize,
}

impl Spans {
    fn add(&mut self, text: &str, role: crate::theme::Role) -> &mut Self {
        let n = text.chars().count();
        if n > 0 && role != type_scale::BODY {
            self.segs.push((self.chars, n, role));
        }
        self.text.push_str(text);
        self.chars += n;
        self
    }
}

// ── async reads (fail-open shell-outs, the needs_overlay idiom) ─────────────

/// Parse `fno config accounts list -J` stdout. `None` on unparseable output (torn
/// stdout degrades the read rather than crashing the modal).
pub fn parse_accounts(stdout: &[u8]) -> Option<Vec<Account>> {
    serde_json::from_slice(stdout).ok()
}

/// Parse `fno config accounts combos list -J` stdout.
pub fn parse_combos(stdout: &[u8]) -> Option<Vec<ComboRow>> {
    serde_json::from_slice(stdout).ok()
}

/// Parse the human `fno config accounts list` table into `(id, identity cell)`
/// pairs. A row line carries the cell in its own last column behind a
/// two-space `  identity=` separator; the left side is `<marker><id> ...`
/// (one active marker char, one space). Footer text has no separator and is
/// skipped. `None` on non-UTF-8 output or when zero cells parse, so a shape
/// the modal cannot read fails open as a named phase-2 miss instead of a
/// silent all-`-` render.
pub fn parse_identity_cells(stdout: &[u8]) -> Option<Vec<(String, String)>> {
    let text = std::str::from_utf8(stdout).ok()?;
    let mut cells = Vec::new();
    for line in text.lines() {
        let Some((left, cell)) = line.split_once("  identity=") else {
            continue;
        };
        let Some(id) = left
            .get(2..)
            .and_then(|rest| rest.split_whitespace().next())
        else {
            continue;
        };
        cells.push((id.to_string(), cell.trim_end().to_string()));
    }
    if cells.is_empty() {
        None
    } else {
        Some(cells)
    }
}

/// Parse `fno config accounts usage -J` into `(id, cell)` pairs. A reading
/// renders each window's used percent and, when the probe gave one, how long
/// until the window resets (`5h 10% ↻2h`); an unknown renders its reason in
/// plain words. `None` on output that is not a JSON object.
pub fn parse_usage(stdout: &[u8]) -> Option<Vec<(String, UsageCell)>> {
    parse_usage_at(stdout, epoch_now())
}

fn parse_usage_at(stdout: &[u8], now: u64) -> Option<Vec<(String, UsageCell)>> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(stdout).ok()?;
    let cells = map
        .into_iter()
        .map(|(id, row)| {
            let windows: Vec<UsageWindow> = row["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|w| {
                    Some(UsageWindow {
                        label: w["label"].as_str()?.to_string(),
                        used_pct: w["used_pct"].as_f64()?.round().clamp(0.0, 999.0) as u32,
                        resets_at: w["resets_at"].as_f64().map(|at| at as u64),
                    })
                })
                .collect();
            let text: Vec<String> = windows
                .iter()
                .map(|w| {
                    let mut cell = format!("{} {}%", w.label, w.used_pct);
                    if let Some(at) = w.resets_at.filter(|at| *at > now) {
                        cell.push_str(" ↻");
                        cell.push_str(&crate::client::humanize_ago(at - now));
                    }
                    cell
                })
                .collect();
            let mut cell = if windows.is_empty() {
                unknown_usage(row["reason"].as_str().unwrap_or("no reading"))
            } else {
                UsageCell {
                    text: text.join(" "),
                    relogin: false,
                    probed_at: None,
                    windows,
                }
            };
            cell.probed_at = row["probed_at"].as_f64().map(|at| at as u64);
            (id, cell)
        })
        .collect();
    Some(cells)
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How many live workers bill `a`: rows that name it, plus rows on the
/// default login of its harness when it is that harness's active account.
fn workers_on(a: &Account, agents: &[crate::proto::AgentRow]) -> usize {
    agents
        .iter()
        .filter(|r| !r.exited)
        .filter(|r| match &r.account {
            Some(id) => *id == a.id,
            None => a.active && r.harness.as_deref() == Some(a.harness.as_str()),
        })
        .count()
}

/// How many live workers share `harness`'s slot. A worker on a spawn-only
/// account has its own config dir, so a swap does not reach it.
fn slot_workers(harness: &str, accounts: &[Account], agents: &[crate::proto::AgentRow]) -> usize {
    let own_dir = |id: &String| accounts.iter().any(|a| a.id == *id && a.spawn_only);
    agents
        .iter()
        .filter(|r| !r.exited && r.harness.as_deref() == Some(harness))
        .filter(|r| !r.account.as_ref().is_some_and(own_dir))
        .count()
}

/// The `u` confirm: what Enter does to this account's login and live workers.
fn use_prompt(acct: &Account, workers: usize) -> String {
    let (id, harness) = (&acct.id, &acct.harness);
    let live = match workers {
        1 => "1 live worker".to_string(),
        n => format!("{n} live workers"),
    };
    let effect = if acct.auth != "managed" {
        format!("make {id} active? no login moves, live workers do not change")
    } else if harness == "codex" && workers > 0 {
        format!("switch codex to {id}? {live} hold the slot, so the switch will defer")
    } else if workers == 1 {
        format!("switch {harness} to {id}? {live} stays running")
    } else {
        format!("switch {harness} to {id}? {live} stay running")
    };
    format!("{effect} (Enter=yes, any key=no)")
}

/// The AGE cell: how old the usage reading is, blank before one lands.
fn usage_age(a: &Account, now: u64) -> String {
    a.usage
        .as_ref()
        .and_then(|u| u.probed_at)
        .map(|at| crate::client::humanize_ago(now.saturating_sub(at)))
        .unwrap_or_default()
}

/// The plain-words cell for a usage probe's unknown reason (the slugs
/// `probe_usage_detail` names). Only a fresh login repairs the first two.
fn unknown_usage(reason: &str) -> UsageCell {
    let (text, relogin) = match reason {
        // A stored login that is another account's, or dead, both land here.
        "unattributed" => ("no proven login for this account: r to log in", true),
        "credential-rejected" => ("login expired: r to log in", true),
        "not-probed" => ("not measured yet", false),
        "harness-unsupported" | "auth-unsupported" => ("no usage meter for this account", false),
        other => {
            return UsageCell {
                text: format!("no reading ({other})"),
                relogin: false,
                probed_at: None,
                windows: Vec::new(),
            }
        }
    };
    UsageCell {
        text: text.to_string(),
        relogin,
        probed_at: None,
        windows: Vec::new(),
    }
}

/// Run both reads in parallel and fold into a [`ReadOutcome`]. Any read failing
/// (missing `fno`, nonzero exit, unparseable JSON, timeout) degrades the whole
/// modal with a named reason (AC2-ERR) - the CLI is the single source, so a
/// partial render would be a silent lie.
///
/// Two-phase: phase 1 sends `Ok`/`Degraded` as soon as the two fast reads
/// fold; phase 2 then re-runs
/// `config accounts list` (plain; the human listing whose last column is the
/// identity cell) under its own budget and sends `Identity`/`IdentityFailed`.
/// A phase-2 result landing after a refresh or close is dropped by the
/// client's gen check, and the modal stays usable throughout (identity is
/// enrichment, never a gate).
pub async fn load_all(tx: tokio::sync::mpsc::UnboundedSender<(u64, ReadOutcome)>, gen: u64) {
    let (acc, com, cached) = tokio::join!(
        read_json(&["config", "accounts", "list", "-J"], READ_TIMEOUT),
        read_json(
            &["config", "accounts", "combos", "list", "-J"],
            READ_TIMEOUT
        ),
        read_json(&["config", "accounts", "usage", "-J"], READ_TIMEOUT),
    );
    let outcome = fold_phase1(acc, com);
    let degraded = matches!(outcome, ReadOutcome::Degraded(_));
    let _ = tx.send((gen, outcome));
    if degraded {
        return;
    }
    // The cached reading paints at once; a failed cached read is silent
    // because the live read below follows and names its own failure.
    if let Some(cells) = cached.ok().and_then(|b| parse_usage(&b)) {
        let _ = tx.send((gen, ReadOutcome::Usage(cells)));
    }
    let identity = async {
        let outcome = match read_json(&["config", "accounts", "list"], IDENTITY_TIMEOUT).await {
            Ok(bytes) => match parse_identity_cells(&bytes) {
                Some(cells) => ReadOutcome::Identity(cells),
                None => ReadOutcome::IdentityFailed(
                    "identity: no identity column in accounts list output".into(),
                ),
            },
            Err(e) => ReadOutcome::IdentityFailed(format!("identity: {e}")),
        };
        let _ = tx.send((gen, outcome));
    };
    // Opening the modal (and R) re-measures every account, so a row never
    // shows a number older than this open.
    let live = async {
        // One live refresh at a time: a repeated R waits out the one running,
        // then reads the cache it just wrote instead of probing again.
        static LAST_LIVE: std::sync::OnceLock<tokio::sync::Mutex<Option<std::time::Instant>>> =
            std::sync::OnceLock::new();
        let mut last = LAST_LIVE.get_or_init(Default::default).lock().await;
        let fresh = last.is_some_and(|at| at.elapsed() < Duration::from_secs(10));
        let args: &[&str] = if fresh {
            &["config", "accounts", "usage", "-J"]
        } else {
            &["config", "accounts", "usage", "--refresh", "-J"]
        };
        let read = read_json(args, USAGE_REFRESH_TIMEOUT).await;
        if !fresh && read.is_ok() {
            *last = Some(std::time::Instant::now());
        }
        let outcome = match read {
            Ok(bytes) => match parse_usage(&bytes) {
                Some(cells) => ReadOutcome::Usage(cells),
                None => ReadOutcome::UsageFailed("unparseable output".into()),
            },
            Err(e) => ReadOutcome::UsageFailed(e),
        };
        let _ = tx.send((gen, outcome));
    };
    tokio::join!(identity, live);
}

/// Fold the two phase-1 reads into one [`ReadOutcome`].
fn fold_phase1(acc: Result<Vec<u8>, String>, com: Result<Vec<u8>, String>) -> ReadOutcome {
    let accounts = match acc {
        Ok(bytes) => match parse_accounts(&bytes) {
            Some(v) => v,
            None => return ReadOutcome::Degraded("accounts list: unparseable output".into()),
        },
        Err(e) => return ReadOutcome::Degraded(format!("accounts list: {e}")),
    };
    let combos = match com {
        Ok(bytes) => match parse_combos(&bytes) {
            Some(v) => v,
            None => return ReadOutcome::Degraded("combos list: unparseable output".into()),
        },
        Err(e) => return ReadOutcome::Degraded(format!("combos list: {e}")),
    };
    ReadOutcome::Ok { accounts, combos }
}

/// One `fno <args>` read shell-out with the fail-open budget. Resolves the `fno`
/// binary from `$FNO_BIN` else PATH (the mux shells the deployed CLI).
async fn read_json(args: &[&str], budget: Duration) -> Result<Vec<u8>, String> {
    let mut command = crate::process_admission::tokio_command(fno_bin());
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut command);
    let output = tokio::time::timeout(budget, fut)
        .await
        .map_err(|_| format!("no answer within {}s", budget.as_secs()))?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("exit {}", output.status.code().unwrap_or(-1)));
    }
    Ok(output.stdout)
}

/// The result of a single-flight mutation command: a human message and whether it
/// succeeded. A failure carries the stderr tail so the modal can surface WHY
/// (AC1-ERR) rather than a bare "failed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub ok: bool,
    pub msg: String,
    /// The pane a login spawn opened, named so the client can focus it.
    pub pane: Option<u64>,
}

/// Run a single-flight `fno <argv>` mutation to completion. Never a read - this
/// is the write path. On non-zero exit the message is the trimmed stderr tail so
/// the modal surfaces the real reason. Longer timeout than a read: a `register`
/// can hit an interactive Keychain access prompt (Domain Pitfalls), so a slow
/// command yields a named timeout error, never a hung modal.
pub async fn run_command(argv: Vec<String>) -> ActionResult {
    run_command_env(argv, Vec::new()).await
}

/// Like [`run_command`] but sets extra child env vars (register needs
/// `CLAUDE_CONFIG_DIR` to pick the account's config dir / Keychain item).
pub async fn run_command_env(argv: Vec<String>, env: Vec<(String, String)>) -> ActionResult {
    let mut cmd = crate::process_admission::tokio_command(fno_bin());
    cmd.args(&argv);
    for (k, v) in &env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let fut = crate::process_admission::tokio_output(&mut cmd);
    let output = match tokio::time::timeout(Duration::from_secs(30), fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return ActionResult {
                ok: false,
                msg: format!("{}: spawn failed: {e}", argv.join(" ")),
                pane: None,
            }
        }
        Err(_) => {
            return ActionResult {
                ok: false,
                msg: format!(
                    "{}: timed out (try the command in a terminal)",
                    argv.join(" ")
                ),
                pane: None,
            }
        }
    };
    action_result(&argv, output.status.success(), &output.stderr)
}

/// The notice for a finished command. A success keeps a closing `warning:`
/// line, and a refusal that needs a login names the mux's own door.
fn action_result(argv: &[String], ok: bool, stderr: &[u8]) -> ActionResult {
    let (cmd, tail) = (argv.join(" "), stderr_tail(stderr));
    if ok {
        let msg = match tail.starts_with("warning:") {
            true => format!("{cmd} ok: {tail}"),
            false => format!("{cmd} ok"),
        };
        return ActionResult {
            ok,
            msg,
            pane: None,
        };
    }
    let reason = match tail.split_once(" Run in a terminal: ") {
        Some((why, _)) => format!("{}: {LOGIN_HINT}", why.trim_end_matches('.')),
        None => tail,
    };
    ActionResult {
        ok,
        msg: format!("{cmd} failed: {reason}"),
        pane: None,
    }
}

/// The last non-empty line of stderr, trimmed - the operator-facing reason.
fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no error output)")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_accounts() -> Vec<Account> {
        parse_accounts(
            br#"[
              {"id":"ccm","name":"CCM","cli":"claude","auth":"managed","priority":10,"active":true,"headroom":"ok","snapshot":"2h"},
              {"id":"ccr","name":"CCR","cli":"claude","auth":"managed","priority":20,"active":false,"headroom":"low","snapshot":"5d"},
              {"id":"glm","name":"GLM","cli":"claude","auth":"api_key","priority":30,"active":false,"headroom":"unknown","snapshot":null,"provider":"zai"}
            ]"#,
        )
        .expect("valid accounts json")
    }

    /// The wire contract with `fno config accounts list -J`, which today emits
    /// `harness`. `sample_accounts` above still sends the pre-rename `cli` and
    /// must keep parsing: this struct and the `fno` binary are deployed
    /// separately, so a mixed pair is a real state, not a hypothetical.
    ///
    /// `parse_accounts` returns `None` on a mismatch and `load_all` renders that
    /// as "unparseable", so a broken field name degrades the whole Connections
    /// modal without any error naming the field. That silence is why both
    /// spellings are asserted rather than assumed.
    #[test]
    fn parse_accounts_rows() {
        let canonical = parse_accounts(
            br#"[{"id":"ccm","name":"CCM","harness":"claude","auth":"managed","priority":10,"active":true,"headroom":"ok","cred-snapshot":"2h"}]"#,
        )
        .expect("canonical `harness` key must parse");
        assert_eq!(canonical[0].harness, "claude");
        assert_eq!(canonical[0].snapshot.as_deref(), Some("2h"));
        assert_eq!(sample_accounts()[0].harness, "claude");

        let accts = sample_accounts();
        assert_eq!(accts.len(), 3);
        assert!(accts[0].active);
        assert_eq!(accts[2].headroom, "unknown");
        assert_eq!(accts[2].snapshot, None);

        // A minimal row (no priority/active/headroom/snapshot) parses.
        let accts =
            parse_accounts(br#"[{"id":"x","cli":"codex","auth":"managed"}]"#).expect("parses");
        assert_eq!(accts[0].headroom, "unknown");
        assert!(!accts[0].active);
        assert_eq!(accts[0].priority, 0);

        assert!(parse_accounts(b"[{not json").is_none());
        assert!(parse_combos(b"nope").is_none());

        assert_eq!(parse_accounts(b"[]").unwrap().len(), 0);
        assert_eq!(parse_combos(b"[]").unwrap().len(), 0);
    }

    fn sample_combos() -> Vec<ComboRow> {
        parse_combos(
            br#"[{"name":"main","strategy":"fallback","members":["ccm","ccr","glm"],"active":true}]"#,
        )
        .expect("valid combos json")
    }

    fn ready_view() -> ConnectionsView {
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: sample_accounts(),
            combos: sample_combos(),
        });
        v
    }

    // AC1-HP: modal lists accounts (badge, cli, auth, headroom incl unknown) and
    // combos with ordered members + active badge.
    #[test]
    fn renders_accounts_with_active_badge_and_unknown_headroom() {
        let v = ready_view();
        let out = v.render().join("\n");
        let line = |id: &str| -> String {
            out.lines()
                .find(|l| l.contains(&format!("  {id}  ")))
                .unwrap_or_else(|| panic!("no {id} row: {out}"))
                .to_string()
        };
        // ACTIVE marks the one credential new spawns use; the others offer u.
        assert!(line("ccm").contains("[ACTIVE]") && line("ccm").contains("subscription login"));
        assert!(!line("ccr").contains("[ACTIVE]"));
        assert!(line("glm").contains("API key") && !line("glm").contains("r: re-authenticate"));
        assert!(out.contains("headroom unknown")); // glm headroom shown, not hidden
        assert!(line("ccm").contains("login saved 2h ago"));
        // Cards in provider order; glm sits under zai, and an empty provider
        // still shows with its add hint. Anthropic's own login is not active.
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {out}"))
        };
        assert!(
            at("Anthropic") < at("  ccm  ")
                && at("  ccr  ") < at("zai")
                && at("zai") < at("  glm  ")
        );
        assert!(at("zai") < at("Amazon Bedrock") && at("DeepSeek") < at("Llama"));
        let default = out.lines().find(|l| l.contains("System default")).unwrap();
        assert!(!default.contains("[ACTIVE]"), "{default}");
        assert!(out.contains("no saved account") && !out.contains("a: add one"));
        // A row names its plan and masked email, and an unproved identity
        // token never reaches it.
        let mut v = ready_view();
        v.accounts[0].plan = Some("Max 20x".into());
        v.accounts[0].email = Some("jn@makersof.xyz".into());
        let out = v.render().join("\n");
        let ccm = out.lines().find(|l| l.contains("  ccm  ")).unwrap();
        assert!(ccm.contains("ccm  Max 20x (j•••@m•••.xyz)"), "{ccm}");
        assert!(!out.contains("?unbound"), "{out}");
        assert_eq!(
            mask_emails("!serves jason@readyrule.com"),
            "!serves j•••@r•••.com"
        );
        assert_eq!(mask_emails("(jn@makersof.xyz) ok"), "(j•••@m•••.xyz) ok");
        // Type levels: the provider title is a heading, ACTIVE a badge, the
        // account name bold, and its details dim.
        use crate::theme::Role;
        let (body, _) = v.render_body(&[], usize::MAX);
        let role_at = |needle: &str| -> Role {
            let line = body.iter().find(|l| l.text.contains(needle)).unwrap();
            let at = line.text[..line.text.find(needle).unwrap()].chars().count();
            line.segs
                .iter()
                .find(|(o, n, _)| at >= *o && at < o + n)
                .map_or(type_scale::BODY, |s| s.2)
        };
        assert_eq!(role_at("Anthropic"), type_scale::HEADING);
        assert_eq!(role_at("[ACTIVE]  ccm"), type_scale::BADGE);
        assert_eq!(role_at("ccr  "), type_scale::NAME);
        assert_eq!(role_at("API key"), type_scale::META);
        assert_eq!(type_scale::level(71), Role::LevelWarn);
        assert_eq!(type_scale::level(95), Role::LevelHigh);
    }

    /// Every row shows its percent, or says in plain words why it cannot; a
    /// re-read keeps the last reading on screen; and a row whose stored login
    /// is another account turns `r` into a login pane, then a register.
    #[test]
    fn usage_rows() {
        let mut v = ready_view();
        let usage = br#"{"ccm": {"windows": [{"label": "5h", "used_pct": 26.4}, {"label": "weekly", "used_pct": 99.0}]},
                         "ccr": {"state": "unknown", "reason": "unattributed"},
                         "glm": {"state": "unknown", "reason": "auth-unsupported"}}"#;
        v.apply_read(ReadOutcome::Usage(parse_usage(usage).expect("usage json")));
        let out = v.render().join("\n");
        assert!(
            out.contains(&format!("5h      {}{}  26%", "━".repeat(5), "─".repeat(15))),
            "{out}"
        );
        assert!(
            out.contains(&format!("weekly  {}  99%", "━".repeat(20))),
            "{out}"
        );
        assert!(
            out.contains("no proven login for this account: r to log in"),
            "{out}"
        );
        assert!(out.contains("no usage meter for this account"), "{out}");
        assert!(parse_usage(b"[]").is_none());

        // accounts_usage: a window names when it resets, the cell keeps when
        // it was probed, and a past reset says nothing.
        let at = parse_usage_at(
            br#"{"ccm": {"probed_at": 1000, "windows": [
                   {"label": "5h", "used_pct": 10, "resets_at": 8200},
                   {"label": "weekly", "used_pct": 13, "resets_at": 900}]}}"#,
            1000,
        )
        .expect("usage json");
        assert_eq!(at[0].1.text, "5h 10% ↻2h weekly 13%");
        assert_eq!(at[0].1.probed_at, Some(1000));
        // 5h with 2h left is 60% through: 10% used is behind an even spend.
        let bar = window_line(&at[0].1.windows[0], 1000);
        assert!(bar.ends_with("10%  resets in 2h  pace slower"), "{bar}");
        // A past reset says nothing.
        assert!(window_line(&at[0].1.windows[1], 1000).ends_with("13%"));

        // Workers: a row naming ccr, plus two default-login claude rows that
        // bill the active claude account (ccm); an exited row counts nowhere.
        let row = |account: Option<&str>, exited: bool| crate::proto::AgentRow {
            harness: Some("claude".into()),
            account: account.map(str::to_string),
            exited,
            ..Default::default()
        };
        let rows = [
            row(Some("ccr"), false),
            row(None, false),
            row(None, false),
            row(None, true),
        ];
        let workers: Vec<usize> = v.accounts.iter().map(|a| workers_on(a, &rows)).collect();
        assert_eq!(workers, vec![2, 1, 0]);
        let out = v.render_with(&rows, usize::MAX).join("\n");
        assert!(
            out.contains("2 workers") && out.contains("1 worker "),
            "{out}"
        );
        // A narrow overlay sheds fields first; the selected row keeps every
        // key.
        let narrow = v.render_with(&rows, 60).join("\n");
        let row = |id: &str| {
            narrow
                .lines()
                .find(|l| l.contains(&format!("  {id}  ")))
                .unwrap()
        };
        assert!(row("ccm").contains("r: re-authenticate"), "{narrow}");
        // A row the cursor is not on keeps its main key, make active.
        assert!(row("ccr").contains("u: make active"), "{narrow}");
        // A row the cursor is not on sheds its least-read fields to fit, so
        // the overlay never wraps it.
        assert!(
            crate::chrome::str_cols(row("ccr").trim_end()) <= 60,
            "{narrow}"
        );

        let five_hour = format!("5h      {}{}  26%", "━".repeat(5), "─".repeat(15));
        v.apply_read(ReadOutcome::Ok {
            accounts: sample_accounts(),
            combos: sample_combos(),
        });
        assert!(
            v.render().join("\n").contains(&five_hour),
            "re-read blanked usage"
        );
        v.apply_read(ReadOutcome::UsageFailed("timed out".into()));
        assert!(v.render().join("\n").contains(&five_hour));
        assert_eq!(v.notice.as_deref(), Some("usage refresh failed: timed out"));

        // r re-authenticates a claude login with both Keychain logins, the
        // vault login `use --login` runs, after a confirm.
        v.acct_sel = 1;
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(v.on_key(b'r'), ConnIntent::Redraw);
        assert!(v
            .render()
            .join("\n")
            .contains("? re-authenticate ccr? two browser logins"));
        assert_eq!(
            v.on_key(b'\r'),
            ConnIntent::SpawnLogin(argv(
                "mux pane run fno config accounts use ccr --scope global --login"
            ))
        );
        assert!(!v.acting && v.relogin.is_empty());
    }

    // piece 1: set-active-account (`s`) is a session-local spawn-routing
    // toggle, distinct from `use` (the global slot-swap). AC1-HP / AC1-UI / AC1-ERR.
    #[test]
    fn set_active_rows() {
        let mut v = ready_view();
        v.acct_sel = 1; // ccr, NOT the globally-active ccm
        let id = v.accounts[1].id.clone();
        let intent = v.on_key(b's');
        assert_eq!(intent, ConnIntent::SetActiveAccount(Some(id.clone())));
        assert_eq!(v.active_account.as_deref(), Some(id.as_str()));
        // The billing marker repaints immediately (no zero-feedback keypress).
        assert!(v.render().join("\n").contains('$'));

        let mut v = ready_view();
        v.acct_sel = 1;
        v.on_key(b's'); // set ccr active for spawns
        let off = v.on_key(b's'); // same row again -> clear to default
        assert_eq!(off, ConnIntent::SetActiveAccount(None));
        assert_eq!(v.active_account, None);

        let mut v = ready_view();
        v.pending.push(PendingLogin {
            id: "x".into(),
            cli: "claude".into(),
            dir: String::new(),
        });
        v.acct_sel = v.accounts.len(); // the synthetic pending row (not an account)
        assert_eq!(v.on_key(b's'), ConnIntent::Bell);
        assert!(v.active_account.is_none());

        // The client seeds the modal with its current active account so the
        // marker is correct on first paint (survives modal close/reopen).
        let id = sample_accounts()[0].id.clone();
        let mut v = ConnectionsView::new().with_active_account(Some(id.clone()));
        v.apply_read(ReadOutcome::Ok {
            accounts: sample_accounts(),
            combos: sample_combos(),
        });
        assert!(v.render().join("\n").contains('$'));
        assert_eq!(v.active_account.as_deref(), Some(id.as_str()));

        // A codex/gemini account is not spawn-routable (mux dispatch is
        // claude-only); selecting it must not mark it active.
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: parse_accounts(
                br#"[{"id":"cdx","name":"CDX","cli":"codex","auth":"oauth_dir","priority":1,"active":false}]"#,
            )
            .unwrap(),
            combos: vec![],
        });
        let intent = v.on_key(b's');
        assert_eq!(intent, ConnIntent::Redraw, "visible feedback, no set");
        assert!(v.active_account.is_none(), "non-claude account not routed");
    }

    #[test]
    fn order_tab_shows_members_in_order_with_active_badge() {
        let mut v = ready_view();
        v.on_key(b'\t'); // -> Order
        assert_eq!(v.tab, Tab::Order);
        let out = v.render().join("\n");
        assert!(out.contains("main"));
        assert!(out.contains("fallback"));
        assert!(out.contains("●")); // active combo badge
                                    // members numbered in rotation order under the selected combo
        assert!(out.contains("1. ccm"));
        assert!(out.contains("2. ccr"));
        assert!(out.contains("3. glm"));
    }

    // AC1-EDGE: empty world renders both tabs' empty states, no read error.
    #[test]
    fn empty_world_renders_add_hints() {
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: vec![],
            combos: vec![],
        });
        let acct = v.render().join("\n");
        assert!(acct.contains("no accounts registered"));
        v.on_key(b'\t');
        let order = v.render().join("\n");
        assert!(order.contains("no combos"));
    }

    // AC2-ERR: a degraded read shows the banner + disables mutation keys.
    #[test]
    fn degraded_read_shows_banner_and_disables_nav() {
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Degraded("fno: not found".into()));
        let out = v.render().join("\n");
        assert!(out.contains("could not read accounts: fno: not found"));
        assert!(out.contains("press R to retry"));
        // j/k are inert while degraded (bell, no move); R still refreshes.
        assert_eq!(v.on_key(b'j'), ConnIntent::Bell);
        assert_eq!(v.on_key(b'R'), ConnIntent::Refresh);
        assert_eq!(v.on_key(0x1b), ConnIntent::Close);
    }

    // Arrows in the add-account search move the pick and leave the query empty.
    #[test]
    fn provider_search_arrows_move_pick_not_query() {
        let mut v = ready_view();
        v.on_key(b'a');
        assert!(v.captures_text());
        // The opening pick follows the selected account's catalog row; pin it.
        v.wizard.as_mut().unwrap().pick = 0;
        // Down Down, Right Right (dropped), Left Left (dropped), Up Up, as the fold emits them.
        for k in [0x0e, 0x0e, 0x10, 0x10] {
            v.on_key(k);
        }
        let w = v.wizard.as_ref().unwrap();
        assert_eq!(w.query, "");
        // Two Down then two Up lands back on the start row.
        assert_eq!(w.pick, 0);
    }

    #[test]
    fn nav_rows() {
        let mut v = ready_view();
        assert_eq!(v.on_key(b'j'), ConnIntent::Redraw);
        assert_eq!(v.acct_sel, 1);
        assert_eq!(v.on_key(b'k'), ConnIntent::Redraw);
        assert_eq!(v.acct_sel, 0);
        // at the top edge, k is a bell (no wrap)
        assert_eq!(v.on_key(b'k'), ConnIntent::Bell);
        assert_eq!(v.on_key(b'\t'), ConnIntent::Redraw);
        assert_eq!(v.tab, Tab::Order);

        let mut v = ready_view();
        assert_eq!(v.on_key(b'z'), ConnIntent::Bell);

        let mut v = ready_view();
        v.acct_sel = 2;
        // A refresh returning fewer accounts must not leave the cursor OOB.
        v.apply_read(ReadOutcome::Ok {
            accounts: vec![sample_accounts()[0].clone()],
            combos: sample_combos(),
        });
        assert_eq!(v.acct_sel, 0);
    }

    // AC1-UI: no zero-feedback keypress - an unbound key rings the bell.

    // `u` on a non-active account stages the switch at global scope, and only
    // Enter runs it.
    #[test]
    fn use_key_rows() {
        let mut v = ready_view();
        v.acct_sel = 1; // ccr, not active
        assert_eq!(v.on_key(b'u'), ConnIntent::Redraw);
        assert!(!v.acting);
        let intent = v.on_key(b'\r');
        assert_eq!(
            intent,
            ConnIntent::Run(vec![
                "config".into(),
                "accounts".into(),
                "use".into(),
                "ccr".into(),
                "--scope".into(),
                "global".into(),
            ])
        );
        assert!(v.acting); // single-flight guard armed

        let mut v = ready_view();
        v.acct_sel = 0; // ccm is active
        assert_eq!(v.on_key(b'u'), ConnIntent::Redraw);
        assert!(!v.acting);
        assert!(v.notice.as_deref().unwrap().contains("already active"));
    }

    // single-flight: a second action while acting is a no-op notice, not a spawn.
    #[test]
    fn second_action_while_acting_is_blocked() {
        let mut v = ready_view();
        v.acct_sel = 1;
        v.on_key(b'u');
        assert!(matches!(v.on_key(b'\r'), ConnIntent::Run(_)));
        assert!(v.acting);
        let intent = v.on_key(b'u');
        assert_eq!(intent, ConnIntent::Redraw);
        assert!(v.notice.as_deref().unwrap().contains("busy"));
    }

    #[test]
    fn a_use_notice_keeps_the_warning_only_on_a_warning() {
        let argv: Vec<String> = vec!["config".into(), "accounts".into(), "use".into()];
        let warned = "warning: swapped under 2 live claude session(s); they keep auth\n";
        let msg = action_result(&argv, true, warned.as_bytes()).msg;
        assert!(
            msg.contains("swapped under 2 live claude session(s)"),
            "{msg}"
        );
        assert_eq!(
            action_result(&argv, true, b"").msg,
            "config accounts use ok"
        );
        assert_eq!(
            action_result(&argv, true, b"a note\n").msg,
            "config accounts use ok"
        );
    }

    #[test]
    fn the_use_prompt_names_the_live_workers_and_any_key_cancels() {
        let mut v = ready_view();
        let row = |harness: &str, account: Option<&str>| crate::proto::AgentRow {
            harness: Some(harness.into()),
            account: account.map(str::to_string),
            ..Default::default()
        };
        let rows = [
            row("claude", None),
            row("claude", Some("ccm")),
            row("claude", Some("ccr")),
            row("codex", None),
        ];
        v.render_with(&rows, usize::MAX);
        v.acct_sel = 1;
        assert_eq!(v.on_key(b'u'), ConnIntent::Redraw);
        let prompt = v.render_with(&rows, usize::MAX).join("\n");
        assert!(
            prompt.contains("switch claude to ccr? 3 live workers stay running"),
            "{prompt}"
        );
        assert_eq!(v.on_key(b'x'), ConnIntent::Redraw);
        assert_eq!(v.notice.as_deref(), Some("cancelled"));
        assert!(!v.acting && v.confirm.is_none());

        let mut codex = sample_accounts()[1].clone();
        codex.harness = "codex".into();
        assert!(use_prompt(&codex, 1).contains("will defer"));
        assert!(use_prompt(&codex, 0).contains("0 live workers stay running"));
        assert!(use_prompt(&sample_accounts()[2], 3).contains("no login moves"));
    }

    #[test]
    fn a_spent_login_refusal_sends_r_to_the_login_pane() {
        let argv: Vec<String> = vec!["config".into(), "accounts".into(), "use".into()];
        let stderr = b"stored credential for 'ccr' is spent (invalid_grant); the slot was not \
            touched Run in a terminal: fno config accounts use ccr\n";
        let msg = action_result(&argv, false, stderr).msg;
        assert!(
            !msg.contains("terminal") && msg.contains(LOGIN_HINT),
            "{msg}"
        );

        let mut v = ready_view();
        v.acct_sel = 1;
        v.notice = Some(msg);
        v.on_key(b'r');
        assert!(
            matches!(v.on_key(b'\r'), ConnIntent::SpawnLogin(argv) if argv.contains(&"--login".to_string()))
        );
    }

    #[test]
    fn a_config_dir_row_names_the_spawn_key_and_runs_nothing() {
        let mut v = ready_view();
        v.accounts[1].spawn_only = true;
        v.acct_sel = 1;
        assert_eq!(v.on_key(b'u'), ConnIntent::Redraw);
        assert!(v.confirm.is_none() && !v.acting);
        assert!(v.notice.as_deref().unwrap().contains("s: spawn-acct"));
        // r opens no pane: the shared-slot login is not this row's login.
        assert_eq!(v.on_key(b'r'), ConnIntent::Redraw);
        assert!(v.confirm.is_none());
        let row = v.render().join("\n");
        let row = row.lines().find(|l| l.contains("  ccr  ")).unwrap();
        assert!(
            row.contains("s: spawn-acct") && !row.contains("r: re-authenticate"),
            "{row}"
        );
    }

    // AC1-ERR path: remove stages a confirm; Enter runs remove, other key cancels.
    #[test]
    fn remove_confirm_rows() {
        let mut v = ready_view();
        v.acct_sel = 1; // ccr
        assert_eq!(v.on_key(b'd'), ConnIntent::Redraw);
        assert!(v.confirm.is_some());
        assert!(v.render().join("\n").contains("remove account ccr?"));
        let intent = v.on_key(b'\r');
        assert_eq!(
            intent,
            ConnIntent::Run(vec![
                "config".into(),
                "accounts".into(),
                "remove".into(),
                "ccr".into()
            ])
        );
        assert!(v.confirm.is_none());

        let mut v = ready_view();
        v.acct_sel = 1;
        v.on_key(b'd');
        assert!(v.confirm.is_some());
        assert_eq!(v.on_key(b'x'), ConnIntent::Redraw); // not Enter -> cancel
        assert!(v.confirm.is_none());
        assert!(v.notice.as_deref().unwrap().contains("cancelled"));
    }

    #[test]
    fn expand_tilde_expands_leading_and_bare_home() {
        use std::ffi::OsStr;
        let home = Some(OsStr::new("/Users/x"));
        assert_eq!(expand_tilde("~/.claude-ccm", home), "/Users/x/.claude-ccm");
        assert_eq!(expand_tilde("~", home), "/Users/x");
        // Anything else -> verbatim: no leading `~/` and not a bare `~`.
        assert_eq!(expand_tilde("/abs/dir", home), "/abs/dir");
        assert_eq!(expand_tilde("~notme", home), "~notme");
        // No home -> verbatim (fail-open).
        assert_eq!(expand_tilde("~/.claude", None), "~/.claude");
        assert_eq!(expand_tilde("~", None), "~");
    }

    #[test]
    fn stderr_tail_picks_last_nonempty_line() {
        assert_eq!(stderr_tail(b"warn\nerror: bad id\n\n"), "error: bad id");
        assert_eq!(stderr_tail(b""), "(no error output)");
        assert_eq!(stderr_tail(b"   \n  \n"), "(no error output)");
    }

    // ── Task 1.4: login wizard + api-key add ────────────────────────────────

    #[test]
    fn account_id_validation_matches_register_contract() {
        assert!(valid_account_id("ccm"));
        assert!(valid_account_id("ccm-2"));
        assert!(!valid_account_id("Ccm")); // uppercase
        assert!(!valid_account_id("2cm")); // leading digit
        assert!(!valid_account_id("cc_m")); // underscore
        assert!(!valid_account_id("")); // empty
    }

    fn type_str(v: &mut ConnectionsView, s: &str) {
        for b in s.bytes() {
            v.on_key(b);
        }
    }

    /// Open the add wizard and take the highlighted provider row.
    fn open_add(v: &mut ConnectionsView) {
        v.on_key(b'a');
        v.on_key(b'\r');
    }

    #[test]
    fn picker_adds_a_catalog_provider_by_key_and_a_local_server_by_url() {
        let catalog = crate::model_catalog::parse(
            r#"{"xai": {"name": "xAI", "env": ["XAI_API_KEY"], "api": "https://api.x.ai/v1", "npm": "@ai-sdk/xai", "models": {"grok-4": {}}}}"#,
        )
        .unwrap();
        let mut v = ready_view();
        v.on_key(b'a');
        v.catalog = Some(catalog);
        type_str(&mut v, "xai");
        v.on_key(b'\r'); // provider -> name (prefilled with the provider id)
        assert_eq!(v.wizard.as_ref().unwrap().id, "xai");
        v.on_key(b'\r'); // name -> key, the kind step is skipped
        assert_eq!(v.wizard.as_ref().unwrap().step, WizardStep::ApiKey);
        type_str(&mut v, "k1");
        match v.on_key(b'\r') {
            ConnIntent::Run(argv) => {
                assert_eq!(
                    argv[3..],
                    [
                        "xai",
                        "--harness",
                        "opencode",
                        "--auth",
                        "api_key",
                        "--route",
                        "xai/grok-4",
                        "--env",
                        "XAI_API_KEY=k1"
                    ]
                );
            }
            _ => panic!("expected a Run intent"),
        }

        let mut v = ready_view();
        v.on_key(b'a');
        type_str(&mut v, "ollama");
        v.on_key(b'\r');
        v.on_key(b'\r'); // name -> base url, prefilled with the default origin
        let w = v.wizard.as_ref().unwrap();
        assert_eq!(w.step, WizardStep::BaseUrl);
        assert_eq!(w.base_url, "http://127.0.0.1:11434");
        assert!(v.render().join("\n").contains("base url"));
    }

    #[test]
    fn provider_step_lists_popular_with_the_harnesses_that_run_each() {
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: Vec::new(),
            combos: Vec::new(),
        });
        v.on_key(b'a');
        let out = v.render().join("\n");
        assert!(out.contains("Popular") && out.contains("OpenRouter"));
        assert!(out.contains("Local") && out.contains("LM Studio"));
        assert!(!out.contains("runs in"));
    }

    // AC3-HP (default): the managed claude account shares ~/.claude - login runs
    // in the shared slot (NO CLAUDE_CONFIG_DIR override, no dir step, no
    // per-account dir), so all accounts reuse one config (the token-swap model).
    #[test]
    fn wizard_spawn_rows() {
        let mut v = ready_view();
        assert_eq!(v.on_key(b'a'), ConnIntent::Redraw);
        assert_eq!(v.wizard.as_ref().unwrap().step, WizardStep::Provider);
        v.on_key(b'\r'); // provider: the selected card's own row
        type_str(&mut v, "ccm2");
        assert_eq!(v.on_key(b'\r'), ConnIntent::Redraw); // id -> kind (default = shared claude)
        let intent = v.on_key(b'\r'); // kind=Claude(shared) -> spawn directly (no dir step)
        match intent {
            // No `env CLAUDE_CONFIG_DIR=...`: login runs in the shared ~/.claude.
            ConnIntent::SpawnLogin(argv) => {
                assert_eq!(argv, vec!["mux", "pane", "run", "claude", "/login"]);
                assert!(!argv.iter().any(|a| a.contains("CLAUDE_CONFIG_DIR")));
            }
            other => panic!("expected SpawnLogin, got {other:?}"),
        }
        assert!(v.wizard.is_none());
        assert_eq!(v.pending.len(), 1);
        assert!(v.pending[0].dir.is_empty()); // shared slot -> register snapshots ~/.claude
        let out = v.render().join("\n");
        assert!(out.contains("ccm2") && out.contains("claude") && out.contains("…login pending"));

        let mut v = ready_view();
        open_add(&mut v);
        type_str(&mut v, "ccm2");
        v.on_key(b'\r'); // id -> kind
        v.on_key(b'\t'); // Claude(shared) -> ClaudeIsolated
        let intent = v.on_key(b'\r'); // -> dir step (prefilled)
        assert_eq!(intent, ConnIntent::Redraw);
        assert_eq!(v.wizard.as_ref().unwrap().dir, "~/.claude-ccm2");
        let intent = v.on_key(b'\r'); // commit dir -> spawn
        let expected_dir = expand_tilde("~/.claude-ccm2", std::env::var_os("HOME").as_deref());
        match intent {
            ConnIntent::SpawnLogin(argv) => {
                assert_eq!(argv[0..4], ["mux", "pane", "run", "env"]);
                assert_eq!(argv[4], format!("CLAUDE_CONFIG_DIR={expected_dir}"));
                assert!(
                    !argv[4].contains('~'),
                    "tilde must be expanded: {}",
                    argv[4]
                );
                assert_eq!(argv[5..], ["claude", "/login"]);
            }
            other => panic!("expected SpawnLogin, got {other:?}"),
        }
        assert_eq!(v.pending[0].dir, expected_dir);

        let mut v = ready_view();
        open_add(&mut v);
        type_str(&mut v, "cdx");
        v.on_key(b'\r'); // id -> kind
        v.on_key(b'\t'); // Claude -> ClaudeIsolated
        v.on_key(b'\t'); // ClaudeIsolated -> Codex
        let intent = v.on_key(b'\r'); // codex has no dir step -> spawn
        assert_eq!(
            intent,
            ConnIntent::SpawnLogin(vec![
                "mux".into(),
                "pane".into(),
                "run".into(),
                "codex".into(),
                "login".into()
            ])
        );
        assert_eq!(v.pending[0].cli, "codex");
        assert!(v.pending[0].dir.is_empty());
    }

    // The opt-in isolated path: an explicit own CLAUDE_CONFIG_DIR (a separate
    // config dir), tilde-expanded before the spawn.

    #[test]
    fn wizard_lifecycle_rows() {
        let mut v = ready_view();
        v.pending.push(PendingLogin {
            id: "ccm2".into(),
            cli: "claude".into(),
            dir: "~/.claude-ccm2".into(),
        });
        // Move the cursor onto the pending row (past the 3 real accounts).
        v.acct_sel = v.accounts.len(); // first pending
        let intent = v.on_key(b'r');
        assert_eq!(
            intent,
            ConnIntent::RunEnv {
                argv: vec![
                    "config".into(),
                    "accounts".into(),
                    "register".into(),
                    "ccm2".into(),
                    "--harness".into(),
                    "claude".into()
                ],
                env: vec![("CLAUDE_CONFIG_DIR".into(), "~/.claude-ccm2".into())],
            }
        );
        assert!(v.acting);

        let mut v = ready_view();
        v.pending.push(PendingLogin {
            id: "ccr".into(), // ccr already a real account in sample
            cli: "claude".into(),
            dir: String::new(),
        });
        v.apply_read(ReadOutcome::Ok {
            accounts: sample_accounts(),
            combos: sample_combos(),
        });
        // ccr became a real record -> its pending row drops.
        assert!(v.pending.is_empty());

        let mut v = ready_view();
        open_add(&mut v);
        type_str(&mut v, "Bad");
        assert_eq!(v.on_key(b'\r'), ConnIntent::Redraw); // stays on Id
        assert_eq!(v.wizard.as_ref().unwrap().step, WizardStep::Id);
        assert!(v.notice.as_deref().unwrap().contains("lowercase"));

        let mut v = ready_view();
        open_add(&mut v);
        type_str(&mut v, "x");
        assert_eq!(v.on_key(0x1b), ConnIntent::Redraw);
        assert!(v.wizard.is_none());
        assert!(v.pending.is_empty());
    }

    #[test]
    fn api_key_wizard_builds_add_command_with_glm_preset() {
        let mut v = ready_view();
        open_add(&mut v);
        type_str(&mut v, "glm2");
        v.on_key(b'\r'); // id -> kind
        v.on_key(b'\t'); // Claude -> ClaudeIsolated
        v.on_key(b'\t'); // ClaudeIsolated -> Codex
        v.on_key(b'\t'); // Codex -> api-key glm
        v.on_key(b'\r'); // -> api key step
        type_str(&mut v, "sk-abc");
        let intent = v.on_key(b'\r');
        match intent {
            ConnIntent::Run(argv) => {
                assert_eq!(argv[0..3], ["config", "accounts", "add"]);
                assert!(argv.contains(&"api_key".to_string()));
                assert!(argv.iter().any(|a| a.contains("ANTHROPIC_BASE_URL=")));
                assert!(argv.iter().any(|a| a == "ANTHROPIC_API_KEY=sk-abc"));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    // ── Task 1.5: Order tab (reorder / activate / remove / new) ─────────────

    fn order_view() -> ConnectionsView {
        let mut v = ready_view();
        v.on_key(b'\t'); // -> Order
        v
    }

    // AC4-HP: J/K buffer locally; Enter commits exactly one combos update.
    #[test]
    fn reorder_rows() {
        let mut v = order_view();
        // members: [ccm, ccr, glm]; cursor on member 0 (ccm). Move it down twice.
        assert_eq!(v.member_sel, 0);
        assert_eq!(v.on_key(b'J'), ConnIntent::Redraw); // ccm <-> ccr
        assert_eq!(v.on_key(b'J'), ConnIntent::Redraw); // ccm <-> glm
        assert_eq!(
            v.dirty_order,
            Some(vec!["ccr".into(), "glm".into(), "ccm".into()])
        );
        // Dirty hint shows; Enter commits one update call with the new order.
        assert!(v.render().join("\n").contains("Enter: commit"));
        let intent = v.on_key(b'\r');
        assert_eq!(
            intent,
            ConnIntent::Run(vec![
                "config".into(),
                "accounts".into(),
                "combos".into(),
                "update".into(),
                "main".into(),
                "--providers".into(),
                "ccr,glm,ccm".into(),
            ])
        );
        assert!(v.dirty_order.is_none());
        assert!(v.acting);

        let mut v = order_view();
        v.on_key(b'J'); // dirty
        assert!(v.dirty_order.is_some());
        assert_eq!(v.on_key(0x1b), ConnIntent::Redraw); // discards, does NOT close
        assert!(v.dirty_order.is_none());
        // A second Esc (nothing dirty) closes.
        assert_eq!(v.on_key(0x1b), ConnIntent::Close);

        let mut v = order_view();
        v.on_key(b'J');
        assert!(v.dirty_order.is_some());
        v.on_key(b'\t'); // -> Accounts, abandons reorder
        assert!(v.dirty_order.is_none());

        let mut v = order_view();
        assert_eq!(v.on_key(b'\r'), ConnIntent::Bell);
    }

    // AC2-FR: Esc on a dirty reorder reverts, no command runs.

    // space activates the selected combo (combos use).
    #[test]
    fn combo_rows() {
        let mut v = order_view();
        let intent = v.on_key(b' ');
        assert_eq!(
            intent,
            ConnIntent::Run(vec![
                "config".into(),
                "accounts".into(),
                "combos".into(),
                "use".into(),
                "main".into()
            ])
        );

        let mut v = ConnectionsView::new();
        let combos = parse_combos(
            br#"[{"name":"main","strategy":"fallback","members":["ccm","gone"],"active":false}]"#,
        )
        .unwrap();
        v.apply_read(ReadOutcome::Ok {
            accounts: vec![sample_accounts()[0].clone()], // only ccm exists
            combos,
        });
        v.on_key(b'\t'); // -> Order
        assert!(v.render().join("\n").contains("gone  (dangling)"));
        assert_eq!(v.on_key(b' '), ConnIntent::Redraw); // activate refused
        assert!(v
            .notice
            .as_deref()
            .unwrap()
            .contains("dangling member gone"));

        let mut v = order_view();
        assert_eq!(v.on_key(b'd'), ConnIntent::Redraw);
        assert!(v.confirm.is_some());
        let intent = v.on_key(b'\r');
        assert_eq!(
            intent,
            ConnIntent::Run(vec![
                "config".into(),
                "accounts".into(),
                "combos".into(),
                "remove".into(),
                "main".into()
            ])
        );
    }

    // AC2-EDGE: a dangling member flags the combo and refuses activate.

    #[test]
    fn combo_form_rows() {
        let mut v = order_view();
        assert_eq!(v.on_key(b'n'), ConnIntent::Redraw);
        assert!(v.combo_input.is_some());
        type_str(&mut v, "backup");
        v.on_key(b'\r'); // name -> members
        type_str(&mut v, "ccr, glm");
        let intent = v.on_key(b'\r');
        assert_eq!(
            intent,
            ConnIntent::Run(vec![
                "config".into(),
                "accounts".into(),
                "combos".into(),
                "add".into(),
                "backup".into(),
                "--providers".into(),
                "ccr,glm".into(),
            ])
        );

        let mut v = ConnectionsView::new();
        let combos = parse_combos(
            br#"[{"name":"a","strategy":"fallback","members":["ccm","ccr"],"active":false},
                {"name":"b","strategy":"fallback","members":["glm"],"active":true}]"#,
        )
        .unwrap();
        v.apply_read(ReadOutcome::Ok {
            accounts: sample_accounts(),
            combos,
        });
        v.on_key(b'\t'); // Order
        v.on_key(b'j'); // member_sel -> 1 on combo a
        assert_eq!(v.member_sel, 1);
        assert_eq!(v.on_key(b'l'), ConnIntent::Redraw); // -> combo b
        assert_eq!(v.combo_sel, 1);
        assert_eq!(v.member_sel, 0); // reset
    }

    // ── identity readout (AC3) + aligned columns (AC4) ──────────────────────

    /// The phase-2 fixture is captured from the real listing's line grammar:
    /// `<marker> <id>  ...  identity=<cell>`, with a footer line that must be
    /// skipped.
    #[test]
    fn identity_cells_rows() {
        let cells = parse_identity_cells(
            b"* makers  [claude] managed  priority=10  headroom=ok  usage=3m  identity=readyrule\n\
              \x20 ccr  [claude] managed  priority=20  headroom=low  identity=-\n\
              \x20 readyrule  [claude] managed  priority=30  headroom=ok  identity=!serves ccr !expired-credential\n\
              \x20 glm  [claude] api_key  priority=40  identity=?unbound-principal\n\
              \nquota observation is OFF: accounts.quota.observe = false and nothing probes.\n",
        )
        .expect("cells parse");
        assert_eq!(
            cells,
            vec![
                ("makers".to_string(), "readyrule".to_string()),
                ("ccr".to_string(), "-".to_string()),
                (
                    "readyrule".to_string(),
                    "!serves ccr !expired-credential".to_string()
                ),
                ("glm".to_string(), "?unbound-principal".to_string()),
            ]
        );

        let out = b"* makers  [claude] managed  priority=10  headroom=ok\n\
                    \x20 ccr  [claude] managed  priority=20\n\
                    quota observation is OFF: nothing probes.\n";
        assert!(parse_identity_cells(out).is_none());

        let mut v = ready_view();
        let out = v.render().join("\n");
        assert!(!out.contains('…')); // a pending identity cell shows nothing
        v.acct_sel = 1;
        v.on_key(b'\t'); // -> Order, build an unsaved reorder buffer
        assert!(matches!(v.on_key(b'J'), ConnIntent::Redraw));
        assert!(v.dirty_order.is_some());
        v.apply_read(ReadOutcome::Identity(identity_cells()));
        assert!(v.dirty_order.is_some()); // untouched by phase 2
        assert_eq!(v.acct_sel, 1);
        assert_eq!(v.state, ModalState::Ready); // still Ready, not Degraded
        assert_eq!(
            v.accounts[0].identity_cell.as_deref(),
            Some("!serves ccr !expired-credential")
        );
        // Back on Accounts, the cells render the list's own tokens verbatim.
        v.on_key(b'\t'); // -> Accounts (discards the dirty buffer)
        let out = v.render().join("\n");
        assert!(out.contains("!serves ccr"));
        assert!(out.contains("!expired-credential"));
        // The api_key row has nothing to state: the list printed `-` and the
        // modal takes it verbatim, not `?reason`.
        let glm_row = out
            .lines()
            .find(|l| l.contains("  glm  "))
            .expect("glm row");
        assert!(
            !glm_row.contains(" · - ") && !glm_row.contains('?'),
            "dash cell: {glm_row}"
        );
        assert!(!out.contains("?api-key-route"));

        let mut v = ready_view();
        v.apply_read(ReadOutcome::Identity(vec![(
            "ccr".to_string(),
            "ccr".to_string(),
        )]));
        assert_eq!(v.identity_phase, IdentityPhase::Ready);
        let out = v.render().join("\n");
        let ccm_row = out
            .lines()
            .find(|l| l.contains("subscription login") && l.contains("ccm"))
            .expect("ccm row");
        // An unproved identity stays off the row; only a warning reaches it.
        assert!(
            !ccm_row.contains('?'),
            "missing id stays off the row: {ccm_row}"
        );
        let ccr_row = out
            .lines()
            .find(|l| l.contains("subscription login") && l.contains("ccr"))
            .expect("ccr row");
        assert!(ccr_row.contains("ccr"));
    }

    // A listing with rows but no `  identity=` column states nothing: no cells
    // parse, which load_all renders as a named phase-2 miss (`?` everywhere).

    fn identity_cells() -> Vec<(String, String)> {
        vec![
            (
                "ccm".to_string(),
                "!serves ccr !expired-credential".to_string(),
            ),
            ("ccr".to_string(), "ccr".to_string()),
            ("glm".to_string(), "-".to_string()),
        ]
    }

    // AC3-HP: phase-1 rows render a pending placeholder, phase 2 fills the
    // list's own cells in without touching the selection or an unsaved order.

    // AC2-EDGE: a phase-1 id absent from the phase-2 output reads `?` while
    // the rows that did land show their cells.

    // AC3-ERR: a failed/late phase 2 renders every cell `?` and stays Ready.
    #[test]
    fn identity_failure_rows() {
        let mut v = ready_view();
        v.apply_read(ReadOutcome::IdentityFailed("identity: timed out".into()));
        assert_eq!(v.state, ModalState::Ready);
        let out = v.render().join("\n");
        for line in out.lines().filter(|l| l.contains("subscription login")) {
            assert!(
                !line.contains('?'),
                "an unproved cell stays off the row: {line}"
            );
        }
        assert!(!out.contains("!serves"));

        let mut v = ready_view();
        let long: String = "a-very-long-account-name".into();
        v.accounts[0].id = long.clone();
        let out = v.render();
        // tab-bar/body seam: exactly one blank line after the tab bar (pad_block
        // pads blanks to the rectangle, so "blank" means whitespace-only)
        assert!(out[1].trim().is_empty());
        assert!(!out[2].trim().is_empty());
        // body/footer seam: exactly one blank line before the footer
        let footer_pos = out.iter().position(|l| l.contains("Tab: order")).unwrap();
        assert!(out[footer_pos - 1].trim().is_empty());
        // a long id renders whole, never cut
        assert!(out.iter().any(|l| l.contains("a-very-long-account-name")));

        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: vec![],
            combos: vec![],
        });
        assert!(v.render().join("\n").contains("Amazon Bedrock"));
        // A key with no provider named groups under its harness's vendor.
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: parse_accounts(br#"[{"id":"glm","harness":"claude","auth":"api_key"}]"#)
                .unwrap(),
            combos: sample_combos(),
        });
        let out = v.render().join("\n");
        assert!(out.find("Anthropic").unwrap() < out.find("  glm  ").unwrap());
        assert!(out.find("  glm  ").unwrap() < out.find("OpenAI").unwrap());
    }
}

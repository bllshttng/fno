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
    /// When the reading was taken (epoch secs), for the AGE column.
    pub probed_at: Option<u64>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
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
}

impl AddKind {
    fn cli(self) -> &'static str {
        match self {
            AddKind::Claude | AddKind::ClaudeIsolated => "claude",
            AddKind::Codex => "codex",
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
        }
    }
    fn next(self) -> AddKind {
        match self {
            AddKind::Claude => AddKind::ClaudeIsolated,
            AddKind::ClaudeIsolated => AddKind::Codex,
            AddKind::Codex => AddKind::ApiKeyGlm,
            AddKind::ApiKeyGlm => AddKind::Claude,
        }
    }
}

/// The GLM/z.ai base_url preset (Claude's Discretion 4: preset-first is
/// fine), read from the compiled-in reach map.
fn glm_base_url() -> String {
    crate::model_catalog::vendor_base_url("zai", "anthropic")
        .expect("model_reach.toml carries the zai anthropic endpoint")
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
    /// The cursor's offset inside the accounts table rows, before they join `out`.
    cursor_rel: std::cell::Cell<Option<usize>>,
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
            pending: Vec::new(),
            gen: 0,
            active_account: None,
            identity_phase: IdentityPhase::Pending,
            relogin: Default::default(),
            last_read: std::time::Instant::now(),
            cursor_row: Default::default(),
            cursor_rel: Default::default(),
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
                self.acting = true;
                self.notice = None;
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
            b'u' => self.act_use(),
            b's' => self.act_set_active(),
            b'd' => self.act_remove(),
            b'a' => {
                self.wizard = Some(Wizard {
                    step: WizardStep::Id,
                    id: String::new(),
                    kind: AddKind::Claude,
                    dir: String::new(),
                    api_key: String::new(),
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
        if needs_login && !self.relogin.contains(&id) {
            let login: Vec<String> = match harness.as_str() {
                "claude" => vec!["claude".into(), "/login".into()],
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
            WizardStep::Id => match key {
                b'\r' | b'\n' => {
                    if !valid_account_id(&w.id) {
                        self.notice =
                            Some("id must be lowercase letters/digits/-, leading letter".into());
                        return ConnIntent::Redraw;
                    }
                    w.step = WizardStep::Kind;
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
                    AddKind::ApiKeyGlm => {
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

    /// Close the wizard and add the GLM api-key provider in one command.
    fn finish_api_key(&mut self) -> ConnIntent {
        let w = self.wizard.take().expect("wizard present");
        self.acting = true;
        self.notice = None;
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
        self.cursor_rel.set(None);
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
            out.push(self.footer());
        }
        if let Some(notice) = &self.notice {
            out.push(format!("! {notice}"));
        }
        (pad_block(out), self.cursor_row.get())
    }

    fn tab_bar(&self) -> String {
        let (a, o) = match self.tab {
            Tab::Accounts => ("[Accounts]", " Order "),
            Tab::Order => (" Accounts ", "[Order]"),
        };
        format!("connections   {a}{o}")
    }

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
            return;
        }
        // The name column is as wide as the longest name.
        let name_w = |id: &str| crate::chrome::str_cols(id);
        let w_name = self
            .accounts
            .iter()
            .map(|a| name_w(&a.id))
            .chain(self.pending.iter().map(|p| name_w(&p.id)))
            .max()
            .unwrap_or(0)
            .max("ACCOUNT".len());
        let names: Vec<String> = self
            .accounts
            .iter()
            .map(|a| a.id.clone())
            .chain(self.pending.iter().map(|p| p.id.clone()))
            .collect();
        let clis: Vec<String> = self
            .accounts
            .iter()
            .map(|a| a.harness.clone())
            .chain(self.pending.iter().map(|p| p.cli.clone()))
            .collect();
        let auths: Vec<String> = self.accounts.iter().map(|a| a.auth.clone()).collect();
        let headrooms: Vec<String> = self.accounts.iter().map(|a| self.usage_text(a)).collect();
        let now = epoch_now();
        let ages: Vec<String> = self.accounts.iter().map(|a| usage_age(a, now)).collect();
        let riders: Vec<String> = self
            .accounts
            .iter()
            .map(|a| workers_on(a, agents).to_string())
            .collect();
        let snaps: Vec<String> = self
            .accounts
            .iter()
            .map(|a| a.snapshot.clone().unwrap_or_default())
            .collect();
        let idents: Vec<String> = self
            .accounts
            .iter()
            .map(|a| self.identity_cell(a))
            .collect();
        let w_cli = clis
            .iter()
            .map(|c| crate::chrome::str_cols(c))
            .max()
            .unwrap_or(0);
        // LOGIN is the stored credential's age (`cred-snapshot`), never the
        // usage reading's: AGE is that one.
        let mut cols: Vec<(&str, Vec<String>, usize)> = vec![
            ("CLI", clis[..self.accounts.len()].to_vec(), w_cli),
            ("AUTH", auths, 0),
            ("USAGE", headrooms, 0),
            ("AGE", ages, 0),
            ("LOGIN", snaps, 0),
            ("WORKERS", riders, 0),
            ("IDENTITY", idents, 0),
        ];
        // A row wider than the overlay wraps and loses its alignment, so the
        // table drops the columns read least, in this order, until it fits.
        for drop in ["AUTH", "IDENTITY", "LOGIN", "AGE", ""] {
            let rows = self.account_rows(&names, w_name, &cols);
            if drop.is_empty() || rows.iter().all(|r| crate::chrome::str_cols(r) <= max_cols) {
                let base = out.len();
                out.extend(rows);
                if let Some(rel) = self.cursor_rel.get() {
                    self.cursor_row.set(Some(base + rel));
                }
                break;
            }
            cols.retain(|(label, _, _)| *label != drop);
        }
        let w_cli = w_cli.max("CLI".len());
        // Session-local pending logins render after the real records; `r`
        // registers the selected one into a managed account.
        for (j, _p) in self.pending.iter().enumerate() {
            let idx = self.accounts.len() + j;
            if idx == self.acct_sel {
                self.cursor_row.set(Some(out.len()));
            }
            out.push(format!(
                "    {name:<wname$}  {cli:<wcli$}  …login pending  (r: register)",
                name = crate::client::pad_to(&names[self.accounts.len() + j], w_name),
                wname = w_name,
                cli = clis[self.accounts.len() + j],
                wcli = w_cli,
            ));
        }
    }

    /// The accounts table's header and one line per record, over the shown
    /// columns. One width per column, shared by the header and every row, so
    /// the body reads as a table; a column with no values still gets its
    /// header's width (AC4-EDGE).
    fn account_rows(
        &self,
        names: &[String],
        w_name: usize,
        cols: &[(&str, Vec<String>, usize)],
    ) -> Vec<String> {
        use crate::chrome::str_cols;
        let widths: Vec<usize> = cols
            .iter()
            .map(|(label, cells, min)| {
                cells
                    .iter()
                    .map(|c| str_cols(c))
                    .fold((*min).max(label.len()), usize::max)
            })
            .collect();
        let line = |lead: String, name: &str, cell: &dyn Fn(usize) -> String| {
            let mut s = lead + &crate::client::pad_to(name, w_name);
            for (k, w) in widths.iter().enumerate() {
                s.push_str("  ");
                s.push_str(&crate::client::pad_to(&cell(k), *w));
            }
            s.trim_end().to_string()
        };
        // Header is inert: no cursor row ever points at it (selection clamps
        // to data rows), so nothing routes a keypress here.
        let mut rows = vec![line("    ".into(), "ACCOUNT", &|k| cols[k].0.to_string())];
        for (i, a) in self.accounts.iter().enumerate() {
            if i == self.acct_sel {
                self.cursor_rel.set(Some(rows.len()));
            }
            let badge = if a.active { "●" } else { " " };
            // A distinct billing marker for the active-for-spawns
            // account, kept separate from the global-active `●` (Locked
            // Decision 1). Always a column so alignment never shifts.
            let spawn = if self.active_account.as_deref() == Some(a.id.as_str()) {
                "$"
            } else {
                " "
            };
            rows.push(line(format!(" {badge}{spawn} "), &names[i], &|k| {
                cols[k].1[i].clone()
            }));
        }
        rows
    }

    /// The usage cell for one row: the latest reading, or the list's headroom
    /// word until one lands. A row whose login pane is open says what `r` does.
    fn usage_text(&self, a: &Account) -> String {
        let text = a.usage.as_ref().map_or(&a.headroom, |u| &u.text);
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
        if w.step != WizardStep::Id {
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
        out.push(String::new());
        out.push("Enter: next   Esc: cancel".to_string());
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
                "Tab: order  j/k: move  u: use  s: spawn-acct  d: remove  a: add  r: register  R: refresh  Esc: close"
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
            let windows: Vec<String> = row["windows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|w| {
                    let mut cell =
                        format!("{} {:.0}%", w["label"].as_str()?, w["used_pct"].as_f64()?);
                    if let Some(at) = w["resets_at"].as_f64().filter(|at| *at as u64 > now) {
                        cell.push_str(" ↻");
                        cell.push_str(&crate::client::humanize_ago(at as u64 - now));
                    }
                    Some(cell)
                })
                .collect();
            let mut cell = if windows.is_empty() {
                unknown_usage(row["reason"].as_str().unwrap_or("no reading"))
            } else {
                UsageCell {
                    text: windows.join(" "),
                    relogin: false,
                    probed_at: None,
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
            }
        }
    };
    UsageCell {
        text: text.to_string(),
        relogin,
        probed_at: None,
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
            }
        }
        Err(_) => {
            return ActionResult {
                ok: false,
                msg: format!(
                    "{}: timed out (try the command in a terminal)",
                    argv.join(" ")
                ),
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
        return ActionResult { ok, msg };
    }
    let reason = match tail.split_once(" Run in a terminal: ") {
        Some((why, _)) => format!("{}: {LOGIN_HINT}", why.trim_end_matches('.')),
        None => tail,
    };
    ActionResult {
        ok,
        msg: format!("{cmd} failed: {reason}"),
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
              {"id":"glm","name":"GLM","cli":"claude","auth":"api_key","priority":30,"active":false,"headroom":"unknown","snapshot":null}
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
        assert!(out.contains("ccm"));
        assert!(out.contains("managed"));
        assert!(out.contains("●")); // active badge on ccm
        assert!(out.contains("unknown")); // glm headroom shown, not hidden
        assert!(out.contains("2h")); // snap age cell, no snap= prefix
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
            out.contains("USAGE") && out.contains("5h 26% weekly 99%"),
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
            out.contains("WORKERS") && out.contains("LOGIN") && out.contains("AGE"),
            "{out}"
        );
        // An overlay one column narrower than the table drops AUTH first and
        // keeps every table row on one line, so the overlay never wraps it.
        let cols = |l: &str| crate::chrome::str_cols(l.trim_end());
        let table = |lines: &[String]| -> Vec<String> {
            lines
                .iter()
                .filter(|l| {
                    l.contains("ACCOUNT") || v.accounts.iter().any(|a| l.contains(a.id.as_str()))
                })
                .cloned()
                .collect()
        };
        let full = table(&v.render_with(&rows, usize::MAX));
        let fit = full.iter().map(|l| cols(l)).max().unwrap() - 1;
        let narrow = table(&v.render_with(&rows, fit));
        assert!(
            !narrow[0].contains("AUTH") && narrow[0].contains("WORKERS"),
            "{narrow:?}"
        );
        assert!(narrow.iter().all(|l| cols(l) <= fit), "{narrow:?}");

        v.apply_read(ReadOutcome::Ok {
            accounts: sample_accounts(),
            combos: sample_combos(),
        });
        assert!(
            v.render().join("\n").contains("5h 26%"),
            "re-read blanked usage"
        );
        v.apply_read(ReadOutcome::UsageFailed("timed out".into()));
        assert!(v.render().join("\n").contains("5h 26%"));
        assert_eq!(v.notice.as_deref(), Some("usage refresh failed: timed out"));

        v.acct_sel = 1; // ccr: its stored login serves another account
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let login = v.on_key(b'r');
        assert_eq!(
            login,
            ConnIntent::SpawnLogin(argv("mux pane run claude /login"))
        );
        assert!(v.render().join("\n").contains("login open, r saves it"));
        let save = v.on_key(b'r');
        let register = argv("config accounts register ccr --harness claude");
        assert_eq!(save, ConnIntent::Run(register));
        assert!(v.relogin.is_empty());
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
        // The usage cell here never set `relogin`, so only the refusal opens it.
        assert!(matches!(v.on_key(b'r'), ConnIntent::SpawnLogin(_)));

        // A refusal about another row leaves this row's re-snapshot alone.
        let mut other = ready_view();
        other.acct_sel = 0;
        other.notice = Some(format!(
            "stored credential for 'ccr' is spent: {LOGIN_HINT}"
        ));
        assert!(matches!(other.on_key(b'r'), ConnIntent::Run(_)));
    }

    #[test]
    fn a_config_dir_row_names_the_spawn_key_and_runs_nothing() {
        let mut v = ready_view();
        v.accounts[1].spawn_only = true;
        v.acct_sel = 1;
        assert_eq!(v.on_key(b'u'), ConnIntent::Redraw);
        assert!(v.confirm.is_none() && !v.acting);
        assert!(v.notice.as_deref().unwrap().contains("s: spawn-acct"));
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

    // AC3-HP (default): the managed claude account shares ~/.claude - login runs
    // in the shared slot (NO CLAUDE_CONFIG_DIR override, no dir step, no
    // per-account dir), so all accounts reuse one config (the token-swap model).
    #[test]
    fn wizard_spawn_rows() {
        let mut v = ready_view();
        assert_eq!(v.on_key(b'a'), ConnIntent::Redraw);
        assert!(v.wizard.is_some());
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
        v.on_key(b'a');
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
        v.on_key(b'a');
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
        v.on_key(b'a');
        type_str(&mut v, "Bad");
        assert_eq!(v.on_key(b'\r'), ConnIntent::Redraw); // stays on Id
        assert_eq!(v.wizard.as_ref().unwrap().step, WizardStep::Id);
        assert!(v.notice.as_deref().unwrap().contains("lowercase"));

        let mut v = ready_view();
        v.on_key(b'a');
        type_str(&mut v, "x");
        assert_eq!(v.on_key(0x1b), ConnIntent::Redraw);
        assert!(v.wizard.is_none());
        assert!(v.pending.is_empty());
    }

    #[test]
    fn api_key_wizard_builds_add_command_with_glm_preset() {
        let mut v = ready_view();
        v.on_key(b'a');
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
        assert!(out.contains('…')); // every identity cell pending
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
        let glm_row = out.lines().find(|l| l.contains("glm")).expect("glm row");
        assert!(glm_row.trim_end().ends_with('-'), "dash cell: {glm_row}");
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
            .find(|l| l.contains(" managed") && l.contains("ccm"))
            .expect("ccm row");
        assert!(ccm_row.contains('?'), "missing id reads ?: {ccm_row}");
        let ccr_row = out
            .lines()
            .find(|l| l.contains(" managed") && l.contains("ccr"))
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
        for line in out.lines().filter(|l| l.contains("managed")) {
            assert!(line.contains('?'), "cell must read ?: {line}");
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
        // header is present and inert (no cursor on it)
        let header = out.iter().find(|l| l.contains("IDENTITY")).expect("header");
        assert!(header.starts_with(' '));
        assert!(!header.contains('>'));
        // name clamped with an ellipsis, never truncated silently
        assert!(out
            .iter()
            .any(|l| l.contains('…') && l.contains("a-very-long")));

        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: vec![],
            combos: vec![],
        });
        assert!(!v.render().join("\n").contains("IDENTITY"));
        // glm is the only snapshot-less row if alone: every snap cell empty.
        let mut v = ConnectionsView::new();
        v.apply_read(ReadOutcome::Ok {
            accounts: parse_accounts(br#"[{"id":"glm","harness":"claude","auth":"api_key"}]"#)
                .unwrap(),
            combos: sample_combos(),
        });
        let out = v.render().join("\n");
        // renders at all and the LOGIN header exists even with no values
        assert!(out.contains("LOGIN"));
    }
}

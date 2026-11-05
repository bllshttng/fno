//! The PR review: a node's PR diff, line comments staged on its rows, and one
//! send that writes them as one blocking finding on the node. It shows full
//! screen on the backlog board, or as a right split beside the focused chat
//! pane. See docs/architecture/pr-review.md.

use super::*;
use crate::backlog_write::{self, ReviewComment};
use crate::client::input_field::{FieldEvent, InputField};
use crate::pr_diff::{self, DiffRow, FileStatus, PrDiff, PrRef, RowKind};
use std::collections::BTreeMap;

/// The bound on the two `gh api` reads together.
const FETCH_BOUND: Duration = Duration::from_secs(20);
const CLAIM_BOUND: Duration = Duration::from_secs(5);
const COMMENT_CHARS: usize = 500;
/// Below this width the gutter keeps one line number.
const WIDE_GUTTER_AT: usize = 80;
/// The header rows above the diff and the hint row under it.
const HEADER_ROWS: usize = 2;
const NO_HOLDER: &str = "no live holder: the finding waits for the next worker";
const NO_PR: &str = "no PR yet";
/// The fold generation of the split's fetch and send, so the board's
/// generation check never drops them.
const PANEL_GEN: u64 = u64::MAX;

/// One selectable row of the flattened diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flat {
    File(usize),
    Row(usize, usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    Loading,
    Failed(String),
    /// The session or its node has no PR to show.
    NoPr(String),
    Loaded {
        diff: PrDiff,
        head: String,
    },
}

/// Where the fleet says the recipient sits, when the review opened from a seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeatName {
    pub(crate) seat: String,
    pub(crate) machine: String,
}

pub(crate) struct Review {
    pub(crate) node: String,
    /// `None` until the split reads the node's PR from the store.
    pub(crate) pr: Option<PrRef>,
    pub(crate) phase: Phase,
    /// Who a send reaches, never blank.
    pub(crate) recipient: String,
    seat: Option<SeatName>,
    flat: Vec<Flat>,
    pub(crate) cursor: usize,
    /// Staged comments by flat row, so a send lists them in diff order.
    pub(crate) drafts: BTreeMap<usize, String>,
    pub(crate) input: Option<(usize, InputField)>,
    esc: Vec<u8>,
    /// The first Esc over unsent drafts warned; the second closes.
    warned: bool,
    fetch_wanted: bool,
    /// The queued send: argv and stdin. One at a time.
    pub(crate) send: Option<(Vec<String>, String)>,
    sending: bool,
    /// Esc goes back to the org view that opened it.
    from_org: bool,
    /// The split holds the keyboard; a click on the chat pane takes it back.
    pub(crate) focused: bool,
}

/// A fetch landing on the board channel.
pub(crate) struct Landed {
    node: String,
    pr: Option<PrRef>,
    read: Result<(PrDiff, String), String>,
    claim: Result<serde_json::Value, String>,
}

impl Review {
    fn new(node: String, pr: Option<PrRef>, seat: Option<SeatName>) -> Review {
        Review {
            node,
            pr,
            phase: Phase::Loading,
            recipient: "reading the claim".into(),
            seat,
            flat: Vec::new(),
            cursor: 0,
            drafts: BTreeMap::new(),
            input: None,
            esc: Vec::new(),
            warned: false,
            fetch_wanted: true,
            send: None,
            sending: false,
            from_org: false,
            focused: true,
        }
    }

    /// Install a read diff; the cursor lands on the first changed row.
    pub(crate) fn load(&mut self, diff: PrDiff, head: String) {
        self.flat = diff
            .files
            .iter()
            .enumerate()
            .flat_map(|(f, file)| {
                std::iter::once(Flat::File(f))
                    .chain((0..file.rows.len()).map(move |r| Flat::Row(f, r)))
            })
            .collect();
        self.cursor = self
            .flat
            .iter()
            .position(|flat| match flat {
                Flat::Row(f, r) => matches!(
                    diff.files[*f].rows[*r].kind,
                    RowKind::Added | RowKind::Removed
                ),
                Flat::File(_) => false,
            })
            .unwrap_or(0);
        self.phase = Phase::Loaded { diff, head };
    }

    fn row_at(&self, at: usize) -> Option<(&str, &DiffRow)> {
        let Phase::Loaded { diff, .. } = &self.phase else {
            return None;
        };
        match self.flat.get(at)? {
            Flat::Row(f, r) => {
                let file = &diff.files[*f];
                let row = &file.rows[*r];
                let path = if row.kind == RowKind::Removed {
                    &file.old_path
                } else {
                    &file.new_path
                };
                Some((path.as_str(), row))
            }
            Flat::File(_) => None,
        }
    }

    /// The row a comment can sit on: a changed or context line, never a header.
    fn commentable(&self, at: usize) -> bool {
        self.row_at(at)
            .is_some_and(|(_, r)| r.kind != RowKind::Hunk)
    }

    /// Every staged comment in diff order, with its anchor.
    pub(crate) fn comments(&self) -> Vec<ReviewComment> {
        self.drafts
            .iter()
            .filter_map(|(at, text)| {
                let (path, row) = self.row_at(*at)?;
                let removed = row.kind == RowKind::Removed;
                Some(ReviewComment {
                    path: path.to_string(),
                    line: if removed { row.old? } else { row.new? },
                    removed,
                    source: row.text.clone(),
                    text: text.clone(),
                })
            })
            .collect()
    }

    fn step(&mut self, by: isize) {
        let last = self.flat.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + by).clamp(0, last) as usize;
    }

    /// The next or previous file header from the cursor.
    fn step_file(&mut self, down: bool) {
        let is_file = |flat: &Flat| matches!(flat, Flat::File(_));
        let found = if down {
            self.flat
                .iter()
                .enumerate()
                .skip(self.cursor + 1)
                .find(|(_, flat)| is_file(flat))
        } else {
            self.flat
                .iter()
                .enumerate()
                .take(self.cursor)
                .rev()
                .find(|(_, flat)| is_file(flat))
        };
        if let Some((at, _)) = found {
            self.cursor = at;
        }
    }

    /// Stage the send, or say why not. Refusals keep the drafts.
    pub(crate) fn queue_send(&mut self, graph: &str) -> Result<(), String> {
        if self.sending || self.send.is_some() {
            return Err("the review is already sending".into());
        }
        let (Phase::Loaded { head, .. }, Some(pr)) = (&self.phase, &self.pr) else {
            return Err("no diff to review yet".into());
        };
        let comments = self.comments();
        if comments.is_empty() {
            return Err("no comments to send: c comments on a row".into());
        }
        let body = backlog_write::review_body(pr, head, &comments)?;
        self.send = Some((backlog_write::review_argv(&self.node, graph, pr), body));
        self.sending = true;
        Ok(())
    }
}

/// The node's PR to review: the newest unmerged one, else the newest.
pub(crate) fn pick_pr(prs: &[crate::backlog_model::Pr]) -> Option<&crate::backlog_model::Pr> {
    let merged = |p: &&crate::backlog_model::Pr| p.merge_status.as_deref() == Some("merged");
    prs.iter()
        .filter(|p| !merged(p))
        .max_by_key(|p| p.number)
        .or_else(|| prs.iter().max_by_key(|p| p.number))
}

/// The recipient line from the `claim status` JSON. A seat from the fleet names
/// itself; otherwise the local roster row joined by full session id, then the
/// bare session id.
pub(crate) fn recipient_line(
    claim: &Result<serde_json::Value, String>,
    seat: Option<&SeatName>,
    local_name: impl Fn(&str) -> Option<String>,
) -> String {
    if let Some(seat) = seat {
        return format!("{} on {}", seat.seat, seat.machine);
    }
    let claim = match claim {
        Ok(claim) => claim,
        Err(e) => return format!("holder not read: {e}"),
    };
    let field = |k: &str| claim.get(k).and_then(serde_json::Value::as_str);
    if !matches!(field("state"), Some("live" | "suspect")) {
        return NO_HOLDER.into();
    }
    let Some(session) = field("session_id") else {
        return field("holder").map_or_else(|| NO_HOLDER.into(), str::to_string);
    };
    let who = local_name(session).unwrap_or_else(|| session.to_string());
    match field("host") {
        Some(host) => format!("{who} on {host}"),
        None => who,
    }
}

/// `d` on the node detail: open the review for the detail node's PR.
pub(crate) fn open_from_detail(view: &mut View) {
    let Some(b) = view.backlog_board.as_ref() else {
        return;
    };
    let Some(id) = b.detail.as_ref().map(|d| d.node_id.clone()) else {
        return;
    };
    let node = b
        .inputs
        .as_ref()
        .and_then(|inputs| crate::backlog_model::node(inputs, &id));
    let Some(node) = node else {
        return view.set_notice(format!("no node {id} in the board read"));
    };
    let Some(pr) = pick_pr(&node.prs) else {
        return view.set_notice(format!("{id} has no PR"));
    };
    let Some(url) = pr.url.clone() else {
        return view.set_notice(format!("{id} PR #{} has no URL", pr.number));
    };
    open(view, id, &url, None);
}

/// Open the review full screen; a bad URL answers with a notice. A seat name
/// means the org view opened it, and Esc goes back there.
pub(crate) fn open(view: &mut View, node: String, url: &str, seat: Option<SeatName>) {
    let pr = match pr_diff::parse_pr_url(url) {
        Ok(pr) => pr,
        Err(e) => return view.set_notice(e),
    };
    let Some(b) = view.backlog_board.as_mut() else {
        return;
    };
    let mut review = Review::new(node, Some(pr), seat);
    review.from_org = review.seat.is_some();
    b.review = Some(review);
}

fn close(view: &mut View) {
    if let Some(review) = view.backlog_board.as_mut().and_then(|b| b.review.take()) {
        if review.from_org {
            view.backlog_board = None;
            crate::client::org_board::open(view);
        }
        return;
    }
    view.viewer.review = None;
}

/// The open review: the board's when it holds one, else the split's.
pub(crate) fn review_mut(view: &mut View) -> Option<&mut Review> {
    if view
        .backlog_board
        .as_ref()
        .is_some_and(|b| b.review.is_some())
    {
        return view.backlog_board.as_mut().and_then(|b| b.review.as_mut());
    }
    view.viewer.review.as_mut()
}

/// The chord on a focused chat pane: open that session's PR review as a right
/// split, or close the split when it is open. The chat stays live on the left.
pub(crate) async fn toggle_split(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if let Some(review) = view.viewer.review.as_mut() {
        // The chord closes as Esc does: unsent drafts warn first.
        review.input = None;
        let unsent = review.drafts.len();
        if unsent > 0 && !review.warned {
            review.warned = true;
            view.set_notice(format!(
                "{unsent} unsent comments: S sends, prefix+I again drops them"
            ));
            return Ok(());
        }
        return crate::client::file_viewer::close(view, sock_w).await;
    }
    let focus = view.layout.focus;
    let agent = view
        .layout
        .agents
        .iter()
        .find(|a| a.pane_id == Some(focus) && !a.exited)
        .cloned();
    let review = match agent.as_ref().and_then(|a| a.node.clone()) {
        Some(node) => Review::new(node, None, None),
        None => {
            let label = agent.map_or_else(|| "this pane".to_string(), |a| a.name);
            let mut review = Review::new(label, None, None);
            review.fetch_wanted = false;
            review.recipient = "no node".into();
            review.phase = Phase::NoPr(NO_PR.into());
            review
        }
    };
    crate::client::file_viewer::open_review(view, review, sock_w).await
}

/// Spawn the board review's queued work off the UI loop: the one fetch and the
/// one send. Runs each run-loop turn from the board kick.
pub(crate) fn kick(b: &mut BoardView, tx: &BoardTx) {
    let gen = b.gen;
    if let Some(review) = b.review.as_mut() {
        kick_one(review, gen, tx);
    }
}

/// The split's queued work, on the same channel at its own generation.
pub(crate) fn kick_split(view: &mut View, tx: &BoardTx) {
    if let Some(review) = view.viewer.review.as_mut() {
        kick_one(review, PANEL_GEN, tx);
    }
}

fn kick_one(review: &mut Review, gen: u64, tx: &BoardTx) {
    if std::mem::take(&mut review.fetch_wanted) {
        let (node, pr, tx) = (review.node.clone(), review.pr.clone(), tx.clone());
        tokio::spawn(async move {
            let landed = fetch(node, pr).await;
            let _ = tx.send((gen, BoardMsg::ReviewLoaded(Box::new(landed))));
        });
    }
    if let Some((args, body)) = review.send.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let bin = crate::digest_overlay::fno_agents_bin();
            let (ok, notice) = backlog_write::run_command_in(bin, &args, Some(body)).await;
            let _ = tx.send((gen, BoardMsg::ReviewSent { ok, notice }));
        });
    }
}

/// A fold for the split lands here and returns `None`; any other passes on.
pub(crate) fn split_fold(view: &mut View, gen: u64, msg: BoardMsg) -> Option<BoardMsg> {
    if gen != PANEL_GEN {
        return Some(msg);
    }
    match msg {
        BoardMsg::ReviewLoaded(landed) => {
            let agents = view.layout.agents.clone();
            if let Some(review) = view.viewer.review.as_mut() {
                land(review, *landed, |session| local_name(&agents, session));
            }
        }
        BoardMsg::ReviewSent { ok, notice } => {
            if let Some(review) = view.viewer.review.as_mut() {
                sent(review, ok);
            }
            view.set_notice(notice);
        }
        _ => {}
    }
    None
}

/// The first line of a failed read's stderr, or a fallback.
fn first_line(text: &str, fallback: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// One bounded read of a command's stdout.
async fn read_command(
    bin: std::path::PathBuf,
    args: Vec<String>,
    bound: Duration,
) -> Result<String, String> {
    use std::process::Stdio;
    let mut command = crate::process_admission::tokio_command(bin);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(bound, command.output())
        .await
        .map_err(|_| format!("timed out after {}s", bound.as_secs()))?
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(first_line(&stderr, "the command refused"))
    }
}

/// The node's PR from the store, for a split opened on a session's node.
async fn resolve_pr(node: &str) -> Option<PrRef> {
    let node = node.to_string();
    let row = tokio::task::spawn_blocking(move || {
        crate::store_client::node(&crate::backlog_view::graph_path(), &node)
    })
    .await
    .ok()?
    .ok()??;
    pr_diff::parse_pr_url(row.get("pr_url")?.as_str()?).ok()
}

async fn fetch(node: String, pr: Option<PrRef>) -> Landed {
    let claim_args = vec![
        "claim".to_string(),
        "status".to_string(),
        format!("node:{node}"),
    ];
    let claim = read_command(
        crate::digest_overlay::fno_agents_bin(),
        claim_args,
        CLAIM_BOUND,
    );
    let reads = async {
        let pr = match pr {
            Some(pr) => Some(pr),
            None => resolve_pr(&node).await,
        };
        let Some(pr) = pr else {
            return (None, Err(NO_PR.to_string()));
        };
        let gh = std::path::PathBuf::from("gh");
        let (diff, head) = tokio::join!(
            read_command(gh.clone(), pr_diff::diff_argv(&pr), FETCH_BOUND),
            read_command(gh, pr_diff::head_argv(&pr), FETCH_BOUND),
        );
        let read = diff.and_then(|text| {
            let head = head?;
            Ok((pr_diff::parse(&text), head.trim().to_string()))
        });
        (Some(pr), read)
    };
    let (claim, (pr, read)) = tokio::join!(claim, reads);
    let claim = claim.and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()));
    Landed {
        node,
        pr,
        read,
        claim,
    }
}

/// The local roster name of a full harness session id.
fn local_name(agents: &[AgentRow], session: &str) -> Option<String> {
    agents
        .iter()
        .find(|a| a.harness_session_id.as_deref() == Some(session))
        .map(|a| a.name.clone())
}

/// A fetch landed: apply it to the board's review, the one its generation
/// names. The split's fetch lands through [`split_fold`].
pub(crate) fn land_board(view: &mut View, landed: Landed) {
    let agents = view.layout.agents.clone();
    let Some(review) = view.backlog_board.as_mut().and_then(|b| b.review.as_mut()) else {
        return;
    };
    land(review, landed, |session| local_name(&agents, session));
}

/// Apply a landed fetch to the review it was read for.
fn land(review: &mut Review, landed: Landed, local_name: impl Fn(&str) -> Option<String>) {
    let other_pr = matches!((&review.pr, &landed.pr), (Some(a), Some(b)) if a.number != b.number);
    if review.node != landed.node || other_pr {
        return;
    }
    if review.pr.is_none() {
        review.pr = landed.pr.clone();
    }
    review.recipient = recipient_line(&landed.claim, review.seat.as_ref(), local_name);
    match landed.read {
        Ok((diff, head)) => review.load(diff, head),
        Err(_) if landed.pr.is_none() => review.phase = Phase::NoPr(NO_PR.into()),
        Err(e) => {
            review.phase = Phase::Failed(first_line(&e, "the read returned nothing"));
        }
    }
}

/// A send finished. Success clears the drafts; a refusal keeps them.
pub(crate) fn sent(review: &mut Review, ok: bool) {
    review.sending = false;
    if ok {
        review.drafts.clear();
        review.warned = false;
    }
}

/// Esc or `q` with no input open: warn once over unsent drafts, else close.
fn esc_pressed(view: &mut View) {
    let Some(review) = review_mut(view) else {
        return;
    };
    let unsent = review.drafts.len();
    if unsent > 0 && !review.warned {
        review.warned = true;
        view.set_notice(format!(
            "{unsent} unsent comments: S sends, Esc again drops them"
        ));
    } else {
        close(view);
    }
}

fn input_keys(view: &mut View, bytes: &[u8]) {
    let Some(review) = review_mut(view) else {
        return;
    };
    let Some((at, field)) = review.input.as_mut() else {
        return;
    };
    let at = *at;
    // A lone Esc chunk closes at once, the detail's rule; the field alone would
    // hold it for a split arrow until a flush that never comes on this path.
    let events = if bytes == [0x1b] {
        vec![FieldEvent::Cancel]
    } else {
        field.feed(bytes)
    };
    for event in events {
        match event {
            FieldEvent::Submit(text) => {
                review.input = None;
                if text.is_empty() {
                    review.drafts.remove(&at);
                } else {
                    review.drafts.insert(at, text);
                }
            }
            FieldEvent::Cancel => review.input = None,
            FieldEvent::Tab => {}
        }
    }
}

/// The review's keys. Everything lands here while the review is open.
pub(crate) fn keys(view: &mut View, bytes: &[u8]) {
    let Some(review) = review_mut(view) else {
        return;
    };
    if review.input.is_some() {
        return input_keys(view, bytes);
    }
    if bytes == [0x1b] && review.esc.is_empty() {
        return esc_pressed(view);
    }
    let tokens = {
        let mut esc = std::mem::take(&mut review.esc);
        let tokens = fold_modal_keys(&mut esc, bytes);
        review.esc = esc;
        tokens
    };
    let page = view.term.0 as isize - HEADER_ROWS as isize - 1;
    let graph = crate::backlog_view::graph_path()
        .to_string_lossy()
        .into_owned();
    for token in tokens {
        // `I` is the split's own chord: prefix+I typed inside it closes it too.
        if matches!(token, ModalKey::Esc | ModalKey::Byte(b'q' | b'I')) {
            esc_pressed(view);
            continue;
        }
        let Some(review) = review_mut(view) else {
            return;
        };
        review.warned = false;
        match token {
            ModalKey::Up | ModalKey::Byte(b'k') => review.step(-1),
            ModalKey::Down | ModalKey::Byte(b'j') => review.step(1),
            ModalKey::PageUp => review.step(-page),
            ModalKey::PageDown => review.step(page),
            ModalKey::Byte(b'n') => review.step_file(true),
            ModalKey::Byte(b'p') => review.step_file(false),
            ModalKey::Byte(b'c' | b'x') if review.sending => {
                view.set_notice("the review is sending: wait for its receipt".to_string());
            }
            ModalKey::Byte(b'c') if review.commentable(review.cursor) => {
                let at = review.cursor;
                let old = review.drafts.get(&at).cloned().unwrap_or_default();
                let field =
                    InputField::new("your comment on this line", COMMENT_CHARS).with_text(&old);
                review.input = Some((at, field));
            }
            ModalKey::Byte(b'x') => {
                let at = review.cursor;
                review.drafts.remove(&at);
            }
            ModalKey::Byte(b'S') => {
                if let Err(refusal) = review.queue_send(&graph) {
                    view.set_notice(refusal);
                }
            }
            ModalKey::Byte(b'?') => view.set_notice(HINT.to_string()),
            _ => {}
        }
    }
}

const HINT: &str =
    "j/k move - PgUp/PgDn page - n/p file - c comment - x remove - S send - esc close";

fn status_word(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Modified => "modified",
        FileStatus::Added => "added",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Binary => "binary",
    }
}

fn num(n: Option<u32>, width: usize) -> String {
    n.map_or_else(|| " ".repeat(width), |n| format!("{n:>width$}"))
}

/// One diff row's styled line, at this width's gutter.
fn row_line(row: &DiffRow, marked: bool, cols: usize) -> TLine {
    let mark = if marked { "\u{25cf}" } else { " " };
    if row.kind == RowKind::Hunk {
        return TLine::meta(row.text.clone());
    }
    let gutter = if cols >= WIDE_GUTTER_AT {
        format!("{} {}", num(row.old, 6), num(row.new, 6))
    } else {
        num(row.new.or(row.old), 6)
    };
    let (sign, role) = match row.kind {
        RowKind::Added => ("+", TextStyle::Added),
        RowKind::Removed => ("-", TextStyle::Removed),
        _ => (" ", TextStyle::Item),
    };
    TLine::of(&[
        TSeg {
            text: mark.to_string(),
            role: TextStyle::Badge,
        },
        TSeg {
            text: format!("{gutter} "),
            role: TextStyle::Meta,
        },
        TSeg {
            text: format!("{sign}{}", row.text),
            role,
        },
    ])
}

/// The header's two lines: what the diff is, and who a send reaches.
fn header(review: &Review) -> (String, String) {
    let head = match &review.pr {
        Some(pr) => format!(
            "review \u{b7} {} \u{b7} {}/{}#{}",
            review.node, pr.owner, pr.repo, pr.number
        ),
        None => format!("review \u{b7} {}", review.node),
    };
    let first = match &review.phase {
        Phase::Loaded { diff, head: sha } => {
            let (added, removed) = diff.totals();
            let short: String = sha.chars().take(8).collect();
            let cut = if diff.truncated {
                " \u{b7} first part only, the diff is too large"
            } else {
                ""
            };
            format!(
                "{head} \u{b7} {short} \u{b7} {count} file{plural} \u{b7} +{added} -{removed}{cut}",
                count = diff.files.len(),
                plural = if diff.files.len() == 1 { "" } else { "s" },
            )
        }
        _ => head,
    };
    (first, format!("sends to: {}", review.recipient))
}

/// The lines of the diff window: starts high enough that the cursor shows even
/// with comment lines between. Returns the lines and the cursor's line.
fn window(review: &Review, h: usize, cols: usize) -> (Vec<TLine>, Option<usize>) {
    let Phase::Loaded { diff, .. } = &review.phase else {
        return (Vec::new(), None);
    };
    let line_at = |at: usize, flat: &Flat| -> Vec<TLine> {
        let mut out = Vec::new();
        match flat {
            Flat::File(f) => {
                let file = &diff.files[*f];
                out.push(TLine::head(format!(
                    "{} ({})",
                    file.new_path,
                    status_word(file.status)
                )));
            }
            Flat::Row(f, r) => {
                let draft = review.drafts.get(&at);
                out.push(row_line(&diff.files[*f].rows[*r], draft.is_some(), cols));
                if let Some(text) = draft {
                    out.push(TLine::of(&[TSeg {
                        text: format!("  \u{25cf} {}", text.lines().next().unwrap_or("")),
                        role: TextStyle::Badge,
                    }]));
                }
            }
        }
        out
    };
    // Walk back from the cursor for a screen of lines, then fill forward.
    let mut start = review.cursor;
    let mut used = 0;
    while start > 0 && used < h / 2 {
        start -= 1;
        used += line_at(start, &review.flat[start]).len();
    }
    let mut lines = Vec::with_capacity(h + 1);
    let mut cursor_line = None;
    for (at, flat) in review.flat.iter().enumerate().skip(start) {
        if lines.len() >= h {
            break;
        }
        if at == review.cursor {
            cursor_line = Some(lines.len());
        }
        let band = at == review.cursor;
        for (i, mut line) in line_at(at, flat).into_iter().enumerate() {
            line.band = band && i == 0;
            lines.push(line.trunc(cols));
        }
    }
    (lines, cursor_line)
}

/// Paint into `rect` (top, left, h, w): the header, the diff window and the foot
/// row. The board passes the whole screen and its notice, because it covers the
/// tab bar that shows one; the split passes no notice and draws a divider,
/// `Some(true)` in the accent while its edge is dragged.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    review: &Review,
    notice: Option<&str>,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    rect: (usize, usize, usize, usize),
    divider: Option<bool>,
    theme: &Theme,
) {
    let (top, mut left, h, mut w) = rect;
    if h < HEADER_ROWS + 2 || w < 2 || top + h > rows || left + w > cols {
        return;
    }
    if let Some(active) = divider {
        let (fg, flags) = match active {
            true => (theme.brand, crate::proto::cell_flags::BOLD),
            false => (crate::proto::Color::Indexed(8), 0),
        };
        for r in top..top + h {
            cells[r * cols + left] = Cell {
                c: '\u{2502}',
                fg,
                bg: crate::proto::Color::Default,
                flags,
            };
        }
        left += 1;
        w -= 1;
    }
    let (title, to) = header(review);
    backlog_style::paint_title_row(
        cells,
        rows,
        cols,
        top,
        left,
        w,
        &title,
        review.focused,
        theme,
    );
    let body_h = h - HEADER_ROWS - 1;
    let (body, follow) = match &review.phase {
        Phase::Loading => (vec![TLine::meta("reading the diff")], None),
        Phase::Failed(why) => (vec![TLine::plain(format!("diff not read: {why}"))], None),
        Phase::NoPr(why) => (vec![TLine::plain(why.clone())], None),
        Phase::Loaded { .. } => window(review, body_h, w),
    };
    let to = [TLine::meta(to).trunc(w)];
    let panel = |cells: &mut [Cell], at: usize, lines: &[TLine], height, follow| {
        backlog_style::paint_panel_at(cells, rows, cols, left, at, w, height, lines, follow, theme)
    };
    panel(cells, top + 1, &to, 1, None);
    panel(cells, top + HEADER_ROWS, &body, body_h, follow);
    let foot = match &review.input {
        Some((_, field)) => TLine::plain(format!("comment: {}", field.display())),
        None if notice.is_some() => TLine::plain(notice.unwrap_or_default()),
        None if review.sending => TLine::meta("sending the review"),
        None => TLine::meta(format!("{} staged - {HINT}", review.drafts.len())),
    };
    panel(cells, top + h - 1, &[foot.trunc(w)], 1, None);
}

/// The `review-diff` snapshot: the fixture diff with two staged comments and no
/// network. An empty parse refuses the shot.
pub(crate) fn fixture(text: &str) -> Result<Review, String> {
    let diff = pr_diff::parse(text);
    if diff.files.is_empty() {
        return Err("review-diff rendered empty: the fixture diff parsed no file".into());
    }
    let pr = PrRef {
        owner: "bllshttng".into(),
        repo: "footnote".into(),
        number: 42,
    };
    let mut review = Review::new("x-demo".into(), Some(pr), None);
    review.fetch_wanted = false;
    review.recipient = "seat-one on host-a".into();
    review.load(diff, "0f80621dea0a1592".into());
    let changed: Vec<usize> = (0..review.flat.len())
        .filter(|at| {
            review
                .row_at(*at)
                .is_some_and(|(_, r)| matches!(r.kind, RowKind::Added | RowKind::Removed))
        })
        .take(4)
        .collect();
    if let (Some(&one), Some(&two)) = (changed.first(), changed.last()) {
        review
            .drafts
            .insert(one, "name this after what it holds".into());
        review.drafts.insert(two, "this branch has no test".into());
    }
    Ok(review)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = include_str!("tests/fixtures/pr_review.diff");

    fn review_view() -> View {
        let mut view = crate::client::tests::two_pane_view();
        view.term = (24, 100);
        view.backlog_board = Some(BoardView::new(0));
        open(
            &mut view,
            "x-1".into(),
            "https://github.com/o/r/pull/7",
            None,
        );
        view
    }

    fn review(view: &View) -> &Review {
        view.backlog_board
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap()
    }

    fn landed(read: Result<(PrDiff, String), String>) -> Landed {
        Landed {
            node: "x-1".into(),
            pr: pr_diff::parse_pr_url("https://github.com/o/r/pull/7").ok(),
            read,
            claim: Err("no claim read".into()),
        }
    }

    #[test]
    fn a_landed_fetch_paints_rows_or_says_why_it_did_not() {
        let mut view = review_view();
        land_board(&mut view, landed(Err("HTTP 404\nmore".into())));
        assert_eq!(review(&view).phase, Phase::Failed("HTTP 404".into()));
        assert_eq!(review(&view).recipient, "holder not read: no claim read");
        keys(&mut view, b"\x1b");
        assert!(
            view.backlog_board.as_ref().unwrap().review.is_none(),
            "Esc closes"
        );

        let mut view = review_view();
        land_board(&mut view, landed(Ok((pr_diff::parse(DIFF), "abc".into()))));
        let r = review(&view);
        let (_, row) = r.row_at(r.cursor).expect("the cursor sits on a row");
        assert!(matches!(row.kind, RowKind::Added | RowKind::Removed));

        // The split beside a chat pane: a node with no PR lands through the
        // split's own fold and says so.
        let mut view = crate::client::tests::two_pane_view();
        view.viewer.review = Some(Review::new("x-1".into(), None, None));
        let none = Landed {
            pr: None,
            ..landed(Err(NO_PR.into()))
        };
        let msg = BoardMsg::ReviewLoaded(Box::new(none));
        assert!(split_fold(&mut view, PANEL_GEN, msg).is_none());
        let split = view.viewer.review.as_ref().expect("the split stays open");
        assert_eq!(split.phase, Phase::NoPr(NO_PR.into()));
        keys(&mut view, b"I");
        assert!(view.viewer.review.is_none(), "I closes the split");
    }

    #[test]
    fn one_send_queues_one_finding_and_esc_warns_before_dropping() {
        let mut view = review_view();
        land_board(
            &mut view,
            landed(Ok((pr_diff::parse(DIFF), "abcdef012".into()))),
        );
        keys(&mut view, b"c");
        keys(&mut view, b"first\r");
        keys(&mut view, b"j");
        keys(&mut view, b"c");
        keys(&mut view, b"second\r");
        assert_eq!(review(&view).drafts.len(), 2);

        keys(&mut view, b"\x1b");
        assert!(view
            .notice
            .as_ref()
            .unwrap()
            .0
            .contains("2 unsent comments"));
        assert!(
            view.backlog_board.as_ref().unwrap().review.is_some(),
            "the first Esc warns"
        );

        keys(&mut view, b"S");
        let (argv, body) = review(&view).send.clone().expect("one write queued");
        assert!(argv.iter().any(|a| a == "--blocking"));
        assert!(argv.iter().any(|a| a == "--stdin"));
        assert!(argv.iter().any(|a| a == "x-1"));
        assert!(
            body.starts_with("review of PR #7 (o/r) at abcdef01: 2 comments."),
            "{body}"
        );
        keys(&mut view, b"S");
        assert!(
            view.notice.as_ref().unwrap().0.contains("already sending"),
            "a second press queues nothing"
        );

        sent(review_mut(&mut view).unwrap(), true);
        assert!(
            review(&view).drafts.is_empty(),
            "a landed send clears the drafts"
        );
        keys(&mut view, b"\x1b");
        assert!(view.backlog_board.as_ref().unwrap().review.is_none());
    }

    #[test]
    fn a_seat_names_the_recipient_and_a_dead_claim_says_so() {
        let seat = SeatName {
            seat: "mercedes".into(),
            machine: "r2d2".into(),
        };
        let mut view = review_view();
        close(&mut view);
        open(
            &mut view,
            "x-1".into(),
            "https://github.com/o/r/pull/7",
            Some(seat.clone()),
        );
        land_board(&mut view, landed(Err("no read".into())));
        assert_eq!(review(&view).recipient, "mercedes on r2d2");
        assert_eq!(
            view.input_owner(),
            crate::client::region_focus::RegionOwner::Board,
            "the review holds the keys"
        );

        let dead = Ok(serde_json::json!({"state": "stale", "holder": "x"}));
        assert_eq!(
            recipient_line(&dead, Some(&seat), |_| None),
            "mercedes on r2d2"
        );
        assert_eq!(recipient_line(&dead, None, |_| None), NO_HOLDER);
        let live = Ok(serde_json::json!({"state": "live", "session_id": "s-1", "host": "bb16"}));
        assert_eq!(
            recipient_line(&live, None, |s| (s == "s-1").then(|| "ada".to_string())),
            "ada on bb16"
        );
    }
}

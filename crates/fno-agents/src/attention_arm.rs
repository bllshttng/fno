//! The `attention` daemon arm: write one page per open question and pin into
//! the vault's questions folder, read answers back after the settle window,
//! record `attention_answer` rows, and move closed pages into `done/`. Pages
//! are always on; `attention.enabled = false` is the kill switch checked on
//! every beat.

use crate::attention::AttentionItem;
use crate::attention_file::{
    self, close_page, parse_page, read_page_answer, render_index, render_page, settle_key,
    DoneEntry, FileAnswer, IndexEntry, PageFront,
};
use crate::attention_route::{Router, Routing};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The arm's beat, matching its KNOWN_ARMS row.
pub const ATTENTION_INTERVAL_S: u64 = 30;

/// How long an edit must hold still before it records. The sync client, the
/// editor and the arm share the page; the wait is insurance against reading
/// a half-settled edit.
pub const DEFAULT_SETTLE_SECS: u64 = 120;

/// The close receipt for a page whose question no store holds any more.
pub const LOST_REASON: &str = "lost, no question row in the stores";

/// `recorded_by` on a lost close. A lost page in `done/` never blocks a
/// redelivery: if the question reads open again, its page comes back.
pub const LOST_BY: &str = "attention_lost";

/// How many times one recorded answer retries its failed clear before the
/// arm gives up and leaves the durable row for a human to finish.
pub const CLEAR_RETRY_CAP: u32 = 5;

/// How long one bounce mail may run before its kill (AC3-ERR).
pub const ATTENTION_SEND_TIMEOUT_S: u64 = 30;

/// How long one Python clear may run before its kill (AC4-ERR).
pub const ATTENTION_CLEAR_TIMEOUT_S: u64 = 180;

/// How long one whole tick may run before the next beat reports it stuck
/// (AC4-HP).
pub const ATTENTION_TICK_BUDGET_S: u64 = 120;

/// The IO seam the tick body takes, so tests drive everything without disk
/// or a real `fno` shellout.
pub trait SinkIo {
    fn read(&mut self, path: &Path) -> std::io::Result<String>;
    /// Whole-file atomic rewrite (tmp + rename in the same directory).
    fn write_atomic(&mut self, path: &Path, content: &str) -> std::io::Result<()>;
    /// Create the file only when absent: tmp file, then hard link onto the
    /// target. `Ok(false)` when the target already exists, so a page is
    /// never overwritten by a second delivery.
    fn create_new(&mut self, path: &Path, content: &str) -> std::io::Result<bool>;
    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()>;
    /// The folder's top-level `.md` files, sorted.
    fn list_md(&mut self, dir: &Path) -> Vec<PathBuf>;
    /// Whether the path exists, through the same seam as every other read.
    fn path_exists(&mut self, path: &Path) -> bool;
    /// Route one item to its team. `Err` names the unread source.
    fn route(&mut self, item: &AttentionItem) -> Result<Routing, String>;
    /// Write one `attention_answer` row. Returns the `Recorded:` receipt text.
    fn record(
        &mut self,
        item: &AttentionItem,
        sink_name: &str,
        answer: &FileAnswer,
    ) -> Result<String, String>;
    /// `fno inbox outstanding clear` with the answer text. `Ok` carries the
    /// clear's posture line (the last stderr line, e.g. `... delivered
    /// (hosted) ...` / `... queued (durable)`), the ladder's mail-rung read.
    fn clear(&mut self, id: &str, answer_text: &str) -> Result<String, String>;
    /// Remove one file: the bounded drain of duplicate lost copies.
    fn remove(&mut self, path: &Path) -> std::io::Result<()>;
    /// The identity stamped into pages this arm delivers: 16 hex over the
    /// machine id and the agents home. `None` leaves pages unstamped, and
    /// an unstamped page closes as lost only once.
    fn arm_id(&mut self) -> Option<String> {
        None
    }
    fn notify(&mut self, title: &str, body: &str);
}

/// Per-tick result.
#[derive(Debug, Default)]
pub struct SinkTick {
    pub delivered: u64,
    pub recorded: u64,
    pub closed: u64,
    pub skip: Option<String>,
    pub detail: Vec<String>,
}

/// One question page found in the folder.
struct FoundPage {
    path: PathBuf,
    stem: String,
    text: String,
    front: PageFront,
}

/// A page stem names its question when the id appears bounded by hyphens (or
/// the stem ends), so a sync client's `q-1 (conflicted copy)` never parses as
/// the page for `q-1`.
pub(crate) fn stem_names_id(stem: &str, id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    let bytes = stem.as_bytes();
    let mut from = 0usize;
    while let Some(pos) = stem[from..].find(id) {
        let abs = from + pos;
        let after = abs + id.len();
        let before_ok = abs == 0 || bytes[abs - 1] == b'-';
        let after_ok = after == bytes.len() || bytes[after] == b'-';
        if before_ok && after_ok {
            return true;
        }
        from = abs + 1;
    }
    false
}

/// The page file name for an item: `<ask date>-<id>-<slug>-<node>`.
fn page_file_name(item: &AttentionItem) -> String {
    let date: String = item
        .created_at
        .chars()
        .filter(|c| *c != '-')
        .take(8)
        .collect();
    let node = item
        .node
        .as_deref()
        .filter(|n| !n.is_empty() && *n != "none")
        .unwrap_or("none");
    format!(
        "{}-{}-{}-{}.md",
        date,
        item.id,
        crate::attention_file::page_slug(&item.title),
        node
    )
}

/// The pure, IO-injected tick body over the questions folder, in the plan's
/// order: read the folder, deliver missing pages, settle + record, close
/// pages whose question closed elsewhere, heal unmoved closed pages, then
/// rewrite the index and the Base.
#[allow(clippy::too_many_arguments)]
pub fn tick_pages(
    items: &[AttentionItem],
    closes: &HashMap<String, crate::attention::Closed>,
    asked: &HashSet<String>,
    dir: &Path,
    state: &mut HashMap<String, BlockState>,
    now: u64,
    settle_secs: u64,
    io: &mut dyn SinkIo,
) -> SinkTick {
    let routed: Vec<&AttentionItem> = items
        .iter()
        .filter(|i| matches!(i.kind.as_str(), "question" | "pin"))
        // Wave 1 has no door that closes an escalation note (`clear` only
        // knows question ids and no-ops on unknown ones), so note items stay
        // on the projection and the mux until the note-closing path ships.
        .filter(|i| !i.id.starts_with("note-"))
        .collect();
    let all_open: std::collections::HashSet<&str> = items.iter().map(|i| i.id.as_str()).collect();
    let mut tick = SinkTick::default();

    // 2. Read the folder. A page is a .md file whose frontmatter question_id
    // equals the id inside its file name.
    let mut pages: Vec<FoundPage> = Vec::new();
    for path in io.list_md(dir) {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let text = match io.read(&path) {
            Ok(t) => t,
            Err(e) => {
                tick.skip = Some("error".to_string());
                tick.detail
                    .push(format!("{}: read failed: {e}", path.display()));
                continue;
            }
        };
        if attention_file::has_conflict_markers(&text) {
            tick.detail
                .push(format!("skipped (conflict markers): {stem}"));
            continue;
        }
        if let Some((front, _)) = parse_page(&text) {
            if stem_names_id(stem, &front.question_id) {
                pages.push(FoundPage {
                    path: path.clone(),
                    stem: stem.to_string(),
                    text,
                    front,
                });
            }
        }
    }
    // Closed pages live in done/ under their id (or <id>-2 when taken).
    let (done_ids, done_pages) = list_done(dir, io);

    // Prune state for ids no longer open (keeps the file bounded).
    state.retain(|id, _| all_open.contains(id.as_str()));

    // 3. Deliver: one page per kept item with no page in the folder or in
    // done/. A router that cannot load delivers nothing this beat.
    let mut routing_err: Option<String> = None;
    let mut fresh: Vec<(String, &AttentionItem, Routing)> = Vec::new();
    for item in &routed {
        if item.recovery_batch.is_some() {
            continue;
        }
        if pages.iter().any(|p| p.front.question_id == item.id) || done_ids.contains(&item.id) {
            continue;
        }
        if routing_err.is_some() {
            continue;
        }
        match io.route(item) {
            Ok(routing) => {
                let mut rendered = render_page(item, &routing);
                if let Some(arm) = io.arm_id() {
                    rendered = attention_file::stamp_arm(&rendered, &arm);
                }
                let name = page_file_name(item);
                let path = dir.join(&name);
                match io.create_new(&path, &rendered) {
                    Ok(true) => {
                        tick.delivered += 1;
                        let stem = name.trim_end_matches(".md").to_string();
                        fresh.push((stem, item, routing));
                    }
                    Ok(false) => {}
                    Err(e) => {
                        tick.skip = Some("error".to_string());
                        tick.detail
                            .push(format!("{}: write failed: {e}", path.display()));
                    }
                }
            }
            Err(e) => routing_err = Some(e),
        }
    }
    // A degraded beat never rewrites the generated views: an empty index
    // would push to the user's devices as "nothing needs you" (AC4-ERR).
    let routing_broken = routing_err.is_some();
    if let Some(e) = routing_err {
        tick.skip = Some("routing_unreadable".to_string());
        tick.detail.push(e);
    }

    // 4 + 5 + 6. Settle open pages, close pages whose question closed
    // elsewhere, heal a closed page a failed move left behind.
    let open_by_id: HashMap<&str, &AttentionItem> =
        routed.iter().map(|i| (i.id.as_str(), *i)).collect();
    for page in &pages {
        let id = page.front.question_id.clone();
        if page.front.status != "open" {
            // Heals a failed move: write nothing, just finish the rename.
            move_to_done(dir, &page.path, &id, io, &mut tick, false, None);
            continue;
        }
        if !all_open.contains(id.as_str()) {
            close_elsewhere(closes, asked, page, dir, &done_pages, io, &mut tick);
            continue;
        }
        let Some(item) = open_by_id.get(id.as_str()) else {
            continue;
        };
        if let Some(batch) = &item.recovery_batch {
            if batch.len() != 64 || !batch.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let questions =
                crate::provider_cap::questions_path(&crate::paths::AgentsHome::from_env());
            let store = crate::event_store::store_path(&questions);
            let Some(root) = store.parent().and_then(Path::parent) else {
                continue;
            };
            let manifest = root
                .join("backups/state-recovery")
                .join(batch)
                .join("snapshots.json");
            let baseline = io
                .read(&manifest)
                .ok()
                .and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok())
                .and_then(|rows| {
                    rows.into_iter().find(|r| {
                        r["source"].as_str().is_some_and(|source| {
                            std::fs::canonicalize(source).unwrap_or_else(|_| PathBuf::from(source))
                                == std::fs::canonicalize(&page.path)
                                    .unwrap_or_else(|_| page.path.clone())
                        })
                    })
                });
            let Some(baseline) = baseline else { continue };
            let Some(snapshot) = baseline["snapshot"].as_str() else {
                continue;
            };
            let Ok(text) = io.read(Path::new(snapshot)) else {
                continue;
            };
            if baseline["sha256"] != format!("{:x}", Sha256::digest(text.as_bytes())) {
                continue;
            }
            if settle_key(&text) == settle_key(&page.text) {
                continue;
            }
        }
        settle_page(page, item, dir, state, now, settle_secs, io, &mut tick);
    }

    // 7. The index and the done listing are rebuilt after the settle pass:
    // a page this beat closed must not linger under Open (AC5-HP).
    let (done_ids_now, done_pages) = list_done(dir, io);
    let done_pages = drain_duplicate_lost(done_pages, io, &mut tick);
    let mut open_entries: Vec<IndexEntry> = Vec::new();
    for page in &pages {
        if page.front.status == "open"
            && all_open.contains(page.front.question_id.as_str())
            && !done_ids_now.contains(page.front.question_id.as_str())
        {
            open_entries.push(IndexEntry {
                stem: page.stem.clone(),
                id: page.front.question_id.clone(),
                title: attention_file::unescape_text(&page.front.title),
                kind: page.front.kind.clone(),
                blocks: page.front.blocks.clone(),
                lead: page.front.lead.clone(),
                created: page.front.asked_at.clone(),
            });
        }
    }
    // Pages delivered this beat are open too: the index lists them on the
    // beat that writes them (AC4-HP).
    for (stem, item, routing) in &fresh {
        open_entries.push(IndexEntry {
            stem: stem.clone(),
            id: item.id.clone(),
            title: attention_file::page_title(&item.title),
            kind: item.kind.clone(),
            blocks: item.blocks.clone(),
            lead: routing.lead.clone().unwrap_or_else(|| "none".to_string()),
            created: item.created_at.clone(),
        });
    }
    let mut done_entries: Vec<DoneEntry> = done_pages
        .iter()
        .map(|(path, front)| DoneEntry {
            stem: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            id: front.question_id.clone(),
            title: attention_file::unescape_text(&front.title),
            status: front.status.clone(),
            answered_at: front.answered_at.clone().unwrap_or_default(),
            answer: front.answer.clone().unwrap_or_default(),
        })
        .collect();
    done_entries.sort_by(|a, b| b.answered_at.cmp(&a.answered_at));
    if !routing_broken {
        write_generated(
            &dir.join("questions.md"),
            &render_index(&open_entries, &done_entries),
            "questions.md",
            io,
            &mut tick,
        );
        write_generated(
            &dir.join("questions.base"),
            attention_file::BASE,
            "questions.base",
            io,
            &mut tick,
        );
    }
    tick
}

/// Settle one open page whose question is still open: hash-compare, wait out
/// the window, record the answer, clear the question, close and move the
/// page.
fn settle_page(
    page: &FoundPage,
    item: &AttentionItem,
    dir: &Path,
    state: &mut HashMap<String, BlockState>,
    now: u64,
    settle_secs: u64,
    io: &mut dyn SinkIo,
    tick: &mut SinkTick,
) {
    let id = page.front.question_id.clone();
    let hash = settle_key(&page.text);
    let entry = state.get(&id).cloned();
    match entry {
        None => {
            state.insert(
                id,
                BlockState {
                    hash,
                    since: now,
                    recorded: false,
                    answer: String::new(),
                    tt: false,
                    retries: 0,
                },
            );
        }
        Some(bs) if bs.hash == hash && bs.recorded => {
            // The row is durable; the clear failed. Retry the clear only
            // (AC5-ERR: no second row), bounded: an answer the door refuses
            // forever must not shell out on every beat for the life of the
            // page.
            if bs.retries >= CLEAR_RETRY_CAP {
                return;
            }
            match io.clear(&id, &bs.answer) {
                Ok(_) => {
                    let receipt = format!("Recorded: {} (file)", short_answer(&bs.answer));
                    close_and_move(
                        page,
                        dir,
                        "answered",
                        &bs.answer,
                        "",
                        "file_edit",
                        &receipt,
                        io,
                        tick,
                        true,
                        None,
                    );
                    state.remove(&id);
                }
                Err(e) => {
                    eprintln!("fno-agents attention: clear retry failed: {e}");
                    if let Some(s) = state.get_mut(&id) {
                        s.retries += 1;
                        if s.retries == CLEAR_RETRY_CAP {
                            io.notify(
                                "Answer could not be recorded",
                                &format!(
                                    "{}: the clear kept failing; the answer row is durable, close it from a terminal.",
                                    item.title
                                ),
                            );
                        }
                    }
                }
            }
        }
        Some(bs) if bs.hash == hash && now.saturating_sub(bs.since) < settle_secs => {
            // Still settling; nothing records yet (AC5-EDGE).
        }
        Some(bs) if bs.hash == hash => match read_page_answer(&page.text) {
            FileAnswer::None => {}
            FileAnswer::TwoTicked => {
                if !bs.tt {
                    io.notify(
                        "Two options ticked",
                        &format!(
                            "You ticked two options on {}. Leave one ticked.",
                            item.title
                        ),
                    );
                    if let Some(s) = state.get_mut(&id) {
                        s.tt = true;
                    }
                }
            }
            other => {
                // A ticked Done means "done" only for a pin; on a question it
                // would close the ask without naming the option.
                if matches!(other, FileAnswer::Done) && item.kind != "pin" {
                    return;
                }
                match io.record(item, "questions", &other) {
                    Ok(receipt) => {
                        let answer_text = answer_text_of(item, &other);
                        tick.recorded += 1;
                        if let Some(s) = state.get_mut(&id) {
                            s.recorded = true;
                            s.answer = answer_text.clone();
                        }
                        if io.clear(&id, &answer_text).is_ok() {
                            close_and_move(
                                page,
                                dir,
                                "answered",
                                &answer_text,
                                "",
                                "file_edit",
                                &receipt,
                                io,
                                tick,
                                true,
                                None,
                            );
                            state.remove(&id);
                        }
                        // A failed clear keeps the page open; the recorded
                        // branch above retries it next beat.
                    }
                    Err(e) => {
                        // A failed append changed nothing durable: leave the
                        // state untouched so the next beat retries the whole
                        // record.
                        eprintln!("fno-agents attention: record failed: {e}");
                    }
                }
            }
        },
        Some(bs) => {
            // The page changed: restart the settle window.
            state.insert(
                id,
                BlockState {
                    hash,
                    since: now,
                    recorded: bs.recorded,
                    answer: bs.answer,
                    tt: false,
                    retries: bs.retries,
                },
            );
        }
    }
}

/// Close a page whose question closed away from the folder (step 5): the
/// fold's answer, else the reason, is the answer text. A close row names
/// the close for every page. The lost branch is owner-scoped: only a page
/// this arm delivered may read as lost.
#[allow(clippy::too_many_arguments)]
fn close_elsewhere(
    closes: &HashMap<String, crate::attention::Closed>,
    asked: &HashSet<String>,
    page: &FoundPage,
    dir: &Path,
    done_pages: &[(PathBuf, PageFront)],
    io: &mut dyn SinkIo,
    tick: &mut SinkTick,
) {
    let id = page.front.question_id.clone();
    let Some(row) = closes.get(&id) else {
        if asked.is_empty() || asked.contains(&id) {
            // Asked but not open and not closed: leave the page for a beat
            // that can name the close. A read that holds no ask row at all
            // proves no loss: an empty store reads clean too.
            return;
        }
        // The stores read clean and hold no row for this id, open or
        // closed: no beat can ever name its close. Without this the page
        // reads open forever in the Base. Close it as lost, answering
        // nothing for the user - but only a page this arm delivered. Two
        // machines on one synced vault each ran an arm whose stores read
        // clean for the other's pages, and the flap lost-closed live
        // questions once a minute. A page stamped by another arm stays
        // open; the arm that owns the stamp names its close. A page with
        // no stamp (delivered before the stamp existed) closes as lost
        // once: done/ already holding its lost copy stays the arm's hand.
        let mine = io.arm_id();
        // The id's one lost copy in done/, if any: an unstamped page with a
        // copy already there stays open, and the close below reuses the
        // copy instead of minting `<id>-N`.
        let lost_copy = done_pages.iter().find_map(|(path, front)| {
            (front.question_id == id && front.recorded_by.as_deref() == Some(LOST_BY))
                .then(|| path.clone())
        });
        if let Some(other) = page.front.arm.as_deref() {
            if mine.as_deref() != Some(other) {
                tick.detail
                    .push(format!("foreign page {} (arm {other})", page.stem));
                return;
            }
        } else if lost_copy.is_some() {
            return;
        }
        close_and_move(
            page,
            dir,
            "closed",
            "",
            "",
            LOST_BY,
            &format!("Recorded: {LOST_REASON} ({LOST_BY})"),
            io,
            tick,
            true,
            lost_copy.as_deref(),
        );
        return;
    };
    let answer = if row.answer.is_empty() {
        row.reason.clone()
    } else {
        row.answer.clone()
    };
    let status = if row.answer.is_empty() {
        "closed"
    } else {
        "answered"
    };
    let recorded_by = if row.closed_by.is_empty() {
        "unknown"
    } else {
        row.closed_by.as_str()
    };
    let shown = short_answer(&answer);
    let shown = if shown.is_empty() {
        "no answer recorded".to_string()
    } else {
        shown
    };
    let receipt = format!("Recorded: {} ({})", shown, recorded_by);
    close_and_move(
        page,
        dir,
        status,
        &answer,
        &row.ts,
        recorded_by,
        &receipt,
        io,
        tick,
        true,
        None,
    );
}

/// The close write (step 6): re-read and compare, write the closed text
/// atomically, then rename into `done/` (or `done/<id>-2.md` when taken).
/// `onto` renames onto an exact existing copy instead (the lost close
/// reuses its one copy).
#[allow(clippy::too_many_arguments)]
fn close_and_move(
    page: &FoundPage,
    dir: &Path,
    status: &str,
    answer: &str,
    answered_at: &str,
    recorded_by: &str,
    receipt: &str,
    io: &mut dyn SinkIo,
    tick: &mut SinkTick,
    count: bool,
    onto: Option<&Path>,
) {
    let stamp = if answered_at.is_empty() {
        chrono::Utc::now().to_rfc3339()
    } else {
        answered_at.to_string()
    };
    let closed = close_page(&page.text, status, answer, &stamp, recorded_by, receipt);
    // Another writer between our read and the write skips this page; the
    // next beat retries (AC5-ERR).
    match io.read(&page.path) {
        Ok(cur) if cur == page.text => {}
        _ => {
            tick.skip = Some("file_changed".to_string());
            return;
        }
    }
    if let Err(e) = io.write_atomic(&page.path, &closed) {
        eprintln!("fno-agents attention: close write failed: {e}");
        tick.skip = Some("error".to_string());
        tick.detail
            .push(format!("{}: close write failed: {e}", page.path.display()));
        return;
    }
    move_to_done(
        dir,
        &page.path,
        &page.front.question_id,
        io,
        tick,
        count,
        onto,
    );
}

/// Read `done/`: the closed ids and their parsed pages, so the delivery
/// dedup and the index agree on what a closed page is. A lost close lists
/// as a page but not as a closed id, so a question that reads open again
/// redelivers.
fn list_done(
    dir: &Path,
    io: &mut dyn SinkIo,
) -> (std::collections::HashSet<String>, Vec<(PathBuf, PageFront)>) {
    let mut ids = std::collections::HashSet::new();
    let mut pages: Vec<(PathBuf, PageFront)> = Vec::new();
    for path in io.list_md(&dir.join("done")) {
        let Ok(text) = io.read(&path) else {
            continue;
        };
        let Some((front, _)) = parse_page(&text) else {
            continue;
        };
        if front.recorded_by.as_deref() != Some(LOST_BY) {
            ids.insert(front.question_id.clone());
        }
        pages.push((path, front));
    }
    (ids, pages)
}

/// Rename a closed page into `done/<id>.md` (`<id>-2.md`, `-3.md`, ... when
/// taken). `onto` renames onto that exact existing copy instead, replacing
/// it in one rename: the lost close reuses its one copy per id.
fn move_to_done(
    dir: &Path,
    path: &Path,
    id: &str,
    io: &mut dyn SinkIo,
    tick: &mut SinkTick,
    count: bool,
    onto: Option<&Path>,
) {
    let done_dir = dir.join("done");
    let target = match onto {
        Some(path) => path.to_path_buf(),
        None => {
            let mut target = done_dir.join(format!("{id}.md"));
            let mut n = 2;
            while io.path_exists(&target) {
                target = done_dir.join(format!("{id}-{n}.md"));
                n += 1;
            }
            target
        }
    };
    if let Err(e) = io.rename(path, &target) {
        eprintln!("fno-agents attention: move to done failed: {e}");
        tick.skip = Some("error".to_string());
        tick.detail
            .push(format!("{}: move to done failed: {e}", path.display()));
        return;
    }
    if count {
        tick.closed += 1;
    }
}

/// One beat removes at most this many duplicate lost copies. `done/` held
/// 2354 numbered copies after the 2026-10-22 flap; the cap keeps each beat
/// short.
pub const DRAIN_DUPLICATE_LOST_CAP: usize = 200;

/// Step 7's drain: one lost copy per question id stays, the newest by
/// `answered_at` (ties keep one deterministically); an answered or
/// closed-elsewhere copy is never touched. Returns the surviving pages so
/// the index lists what is actually on disk.
fn drain_duplicate_lost(
    pages: Vec<(PathBuf, PageFront)>,
    io: &mut dyn SinkIo,
    tick: &mut SinkTick,
) -> Vec<(PathBuf, PageFront)> {
    let victims: Vec<usize> = {
        let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, (_, front)) in pages.iter().enumerate() {
            if front.recorded_by.as_deref() == Some(LOST_BY) {
                by_id.entry(front.question_id.as_str()).or_default().push(i);
            }
        }
        let mut victims: Vec<usize> = Vec::new();
        for idxs in by_id.values() {
            if idxs.len() < 2 {
                continue;
            }
            // Newest answered_at stays; ties fall to the path order.
            let mut order = idxs.clone();
            order.sort_by(|a, b| {
                pages[*a]
                    .1
                    .answered_at
                    .cmp(&pages[*b].1.answered_at)
                    .then(pages[*a].0.cmp(&pages[*b].0))
            });
            victims.extend_from_slice(&order[..order.len() - 1]);
        }
        victims.sort_unstable();
        victims
    };
    if victims.is_empty() {
        return pages;
    }
    let doomed: std::collections::HashSet<usize> =
        victims.into_iter().take(DRAIN_DUPLICATE_LOST_CAP).collect();
    let mut kept = Vec::with_capacity(pages.len() - doomed.len());
    let mut removed = 0usize;
    for (i, entry) in pages.into_iter().enumerate() {
        if doomed.contains(&i) && io.remove(&entry.0).is_ok() {
            removed += 1;
        } else {
            kept.push(entry);
        }
    }
    tick.detail
        .push(format!("done: removed {removed} duplicate lost copies"));
    kept
}

fn short_answer(answer: &str) -> String {
    let one_line: String = answer.lines().collect::<Vec<_>>().join(" ");
    one_line.chars().take(40).collect()
}

/// Rewrite a generated file only when its content changed and the current
/// file is absent or ours. A hand-authored file is named in the detail and
/// never touched (AC7-HP).
fn write_generated(
    path: &Path,
    rendered: &str,
    name: &str,
    io: &mut dyn SinkIo,
    tick: &mut SinkTick,
) {
    match io.read(path) {
        Ok(cur) => {
            if cur == rendered {
                return;
            }
            if !cur.contains("GENERATED") {
                tick.detail
                    .push(format!("{name} is hand-authored; left alone"));
                return;
            }
            if let Err(e) = io.write_atomic(path, rendered) {
                tick.skip = tick.skip.clone().or_else(|| Some("error".to_string()));
                tick.detail.push(format!("{name}: write failed: {e}"));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Err(e) = io.create_new(path, rendered) {
                tick.skip = tick.skip.clone().or_else(|| Some("error".to_string()));
                tick.detail.push(format!("{name}: write failed: {e}"));
            }
        }
        Err(e) => {
            tick.skip = Some("error".to_string());
            tick.detail.push(format!("{name}: read failed: {e}"));
        }
    }
}

/// The answer text `fno inbox outstanding clear` receives: the option's text
/// for a tick, the words for a written answer, `done` for a pin.
fn answer_text_of(item: &AttentionItem, answer: &FileAnswer) -> String {
    match answer {
        FileAnswer::Option(n) => item
            .options
            .iter()
            .find(|o| o.n == *n)
            .map(|o| o.text.clone())
            .unwrap_or_else(|| format!("option {n}")),
        FileAnswer::Words(w) => w.clone(),
        FileAnswer::Done => "done".to_string(),
        FileAnswer::Delegate => crate::attention_file::DELEGATE_TEXT.to_string(),
        FileAnswer::None | FileAnswer::TwoTicked => String::new(),
    }
}

/// The settle state for one delivered page, persisted at
/// `~/.fno/state/attention/questions.json`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockState {
    hash: u64,
    since: u64,
    recorded: bool,
    answer: String,
    tt: bool,
    #[serde(default)]
    retries: u32,
}

/// The arm as the daemon holds it: cadence stamp plus one-in-flight gate.
/// `stage` names the step a live tick stopped in, so a stuck tick can be
/// reported by the next beat instead of sitting silent.
pub struct Arm {
    config_cwd: PathBuf,
    last_tick: Mutex<Option<std::time::Instant>>,
    in_flight: Arc<AtomicBool>,
    stage: Arc<Mutex<Option<(&'static str, std::time::Instant)>>>,
}

impl Arm {
    pub fn new(config_cwd: PathBuf) -> Self {
        Arm {
            config_cwd,
            last_tick: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
            stage: Arc::new(Mutex::new(None)),
        }
    }
}

/// The kill switch (user ruling 2026-09-22, after 38 phantom answers):
/// `attention.enabled = false` stops every page write and answer read, and
/// the beat checks it before any work. Anything but a literal false is on.
fn attention_enabled(cwd: &Path) -> bool {
    crate::agents_config::config_lookup(cwd, &["attention", "enabled"]).and_then(|v| v.as_bool())
        != Some(false)
}

/// Load the settle state from `~/.fno/state/attention/questions.json`.
fn load_state(path: &Path) -> HashMap<String, BlockState> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Save the settle state atomically.
fn save_state(path: &Path, state: &HashMap<String, BlockState>) {
    if let Some(parent) = attention_dir().ok() {
        let _ = std::fs::create_dir_all(&parent);
    }
    let tmp = path.with_extension("json.tmp");
    if serde_json::to_string(state)
        .map(|s| std::fs::write(&tmp, s))
        .unwrap_or(Err(std::io::Error::other("serialize")))
        .is_ok()
    {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// `~/.fno/state/attention/`, folded under `state/` like `notice-route`.
pub fn attention_dir() -> Result<PathBuf, std::io::Error> {
    let home = crate::paths::AgentsHome::from_env();
    let parent = home
        .root()
        .parent()
        .ok_or_else(|| std::io::Error::other("no agents home parent"))?;
    Ok(crate::state_layout::place(parent, "attention"))
}

/// The question folder the arm wrote on its last beat, from items.json's
/// `questions_dir`. A reader in any cwd lands on the one folder the daemon
/// writes. None when items.json is missing, unreadable, empty of the key, or
/// older than the 600s the arm's beat allows: the caller then derives the
/// folder from its own cwd.
pub fn published_questions_dir() -> Option<PathBuf> {
    let raw = std::fs::read_to_string(attention_dir().ok()?.join("items.json")).ok()?;
    let index: Value = serde_json::from_str(&raw).ok()?;
    let as_of = index.get("as_of").and_then(Value::as_u64)?;
    if now_secs().saturating_sub(as_of) > 600 {
        return None;
    }
    let dir = index.get("questions_dir").and_then(Value::as_str)?;
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

/// The not-ready bounce: one wrapped mail to a not-ready item's asker,
/// naming the id and the missing fields, once per item ever (the id saves
/// on every attempt, so a failed send does not re-mail each beat; the
/// second beat stays quiet via the persisted bounce set). Items past the
/// beat deadline are left for the next beat. AC3-HP, AC3-ERR.
fn bounce_not_ready(items: &[AttentionItem], dir: &Path, deadline: std::time::Instant) -> u64 {
    bounce_not_ready_with(items, dir, deadline, &|asker, body| {
        // The command rebuilds per attempt: the bounded helper consumes it.
        let build_cmd = || {
            let mut cmd = crate::loop_dispatch::fno_cmd("fno");
            cmd.args(["agents", "mail", "send", asker, body]);
            cmd.args(["--from-name", "fno/questions"]);
            cmd
        };
        // ETXTBSY keeps its spawn retry: a binary swap mid-beat must not
        // cost the asker their one bounce under the save-on-attempt rule.
        matches!(
            crate::loop_dispatch::retry_etxtbsy(|| {
                crate::bounded_cmd::output_with_timeout_result(
                    build_cmd(),
                    ATTENTION_SEND_TIMEOUT_S,
                )
            }),
            Ok(out) if out.status.success()
        )
    })
}

/// [`bounce_not_ready`] with the send injected, so a test counts sends
/// without a `fno` shellout. Returns the count of eligible items it did
/// not try.
fn bounce_not_ready_with(
    items: &[AttentionItem],
    dir: &Path,
    deadline: std::time::Instant,
    send: &dyn Fn(&str, &str) -> bool,
) -> u64 {
    let path = dir.join("bounced.json");
    let mut bounced: std::collections::HashSet<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mut dirty = false;
    let mut deferred = 0u64;
    for item in items {
        if item.recovery_batch.is_some() {
            continue;
        }
        if item.ready || !matches!(item.kind.as_str(), "question" | "pin") {
            continue;
        }
        let Some(asker) = item
            .asker
            .as_ref()
            .map(|a| a.handle.clone())
            .filter(|h| !h.trim().is_empty())
        else {
            continue;
        };
        if bounced.contains(&item.id) {
            continue;
        }
        if std::time::Instant::now() >= deadline {
            deferred += 1;
            continue;
        }
        let body = format!(
            "Question {} is missing: {}. Re-ask with the fields (docs/architecture/attention-items.md) or clear it.",
            item.id,
            item.missing.join(", ")
        );
        // The id saves on every attempt: a send that fails or is killed
        // must not re-mail the same asker on every later beat (AC3-HP).
        send(&asker, &body);
        bounced.insert(item.id.clone());
        dirty = true;
    }
    if dirty {
        if let Ok(s) = serde_json::to_string(&bounced) {
            let _ = std::fs::write(&path, s);
        }
    }
    deferred
}

/// Fold `user_ask_answered` rows into `done/` as their own answered pages,
/// created once (the acked set persists beside the settle state). AC14-HP.
fn fold_user_ask_answered(
    index: &Path,
    state_dir: &Path,
    questions_dir: &Path,
    io: &mut dyn SinkIo,
) -> u64 {
    let seen_path = state_dir.join("answered.json");
    let mut seen: std::collections::HashSet<String> = std::fs::read_to_string(&seen_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    // Store rows first: Python commits answers to questions.db without
    // touching the raw journal, so a raw read misses them.
    let raw = crate::event_store::journal_text(index, &["user_ask_answered"]);
    let mut created = 0u64;
    let mut dirty = false;
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("user_ask_answered") {
            continue;
        }
        let data = v.get("data");
        let session = data
            .and_then(|d| d.get("session_id"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let turn = data
            .and_then(|d| d.get("turn_id"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if session.is_empty() || turn.is_empty() {
            continue;
        }
        let key = format!("{session}:{turn}");
        if seen.contains(&key) {
            continue;
        }
        let field = |name: &str| -> String {
            data.and_then(|d| d.get(name))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (excerpt, answer) = (field("excerpt"), field("answer"));
        let stem = format!("ask-{:016x}", fnv1a(&key));
        let page = format!(
            "---\nquestion_id: {stem}\nkind: answer\nstatus: answered\nanswer: {}\nanswered_at: {}\nharness_session_id: {session}\n---\n\n# You asked\n\n{}\n\n## Answer\n\n{}\n",
            crate::attention_file::escape_text(&answer),
            v.get("ts").and_then(|x| x.as_str()).unwrap_or(""),
            crate::attention_file::escape_text(&excerpt),
            crate::attention_file::escape_text(&answer),
        );
        if io
            .create_new(
                &questions_dir.join("done").join(format!("{stem}.md")),
                &page,
            )
            .ok()
            .unwrap_or(false)
        {
            created += 1;
        }
        seen.insert(key);
        dirty = true;
    }
    if dirty {
        if let Ok(s) = serde_json::to_string(&seen) {
            let _ = std::fs::write(&seen_path, s);
        }
    }
    created
}

fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// The daemon-facing wrapper: due-check, kill switch, one-in-flight gate
/// (the `merge_close::maybe_tick` shape). The body runs off-loop. A beat
/// that finds the previous tick still running past [`ATTENTION_TICK_BUDGET_S`]
/// emits one timeout row naming the stage it stopped in (AC4-HP).
pub fn maybe_tick(arm: &Arm, home: crate::paths::AgentsHome) {
    let interval = std::time::Duration::from_secs(ATTENTION_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    if arm.in_flight.swap(true, Ordering::SeqCst) {
        let stage = arm.stage.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(d) = stuck_detail(stage, ATTENTION_TICK_BUDGET_S) {
            emit_tick_row(&home, 0, Some("timeout"), &d);
        }
        return;
    }
    let flag = Arc::clone(&arm.in_flight);
    let stage = Arc::clone(&arm.stage);
    let cwd = arm.config_cwd.clone();
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let started = std::time::Instant::now();
        let mark = |name: &'static str| {
            *stage.lock().unwrap_or_else(|e| e.into_inner()) = Some((name, started));
        };
        // Kill switch first: a stopped arm writes nothing and reads nothing.
        if !attention_enabled(&cwd) {
            mark("emit");
            emit_tick_row(&home, 0, Some("disabled"), "attention.enabled is false");
            return;
        }
        mark("read_items");
        let (items, unreadable, stores) = read_items(&cwd);
        if !unreadable.is_empty() {
            // An incomplete projection must never drive delivery or the
            // close-elsewhere close: absent ids would read as closed and
            // close live pages the projection could not see.
            mark("emit");
            emit_tick_row(&home, 0, Some("source_unreadable"), &unreadable.join("; "));
            return;
        }
        let dir = crate::escalation::questions_dir(&cwd);
        write_items_cache(&home, &items, &dir);
        let mut skip: Option<String> = None;
        let mut detail: Vec<String> = Vec::new();
        // A still-present md row is retired: ignored, named once per beat.
        for key in ["attention", "reach_me"] {
            if crate::agents_config::config_lookup(&cwd, &[key])
                .is_some_and(|v| v.as_array().is_some())
            {
                detail.push(format!(
                    "[[{key}]] md rows are retired; pages write to {}",
                    dir.display()
                ));
            }
        }
        mark("bounce");
        let bounce_deferred = bounce_not_ready(
            &items,
            &attention_dir().unwrap_or_else(|_| dir.join(".state")),
            started + std::time::Duration::from_secs(ATTENTION_TICK_BUDGET_S),
        );
        if bounce_deferred > 0 {
            detail.push(format!("bounce: budget spent, {bounce_deferred} deferred"));
            if skip.is_none() {
                skip = Some("timeout".to_string());
            }
        }
        mark("fold_answered");
        let state_dir = attention_dir().unwrap_or_else(|_| dir.join(".state"));
        let answered = fold_user_ask_answered(
            &crate::provider_cap::questions_path(&home),
            &state_dir,
            &dir,
            &mut RealIo::new(&cwd),
        );
        let mut acted = answered;
        mark("settle");
        let state_path = state_dir.join("questions.json");
        let mut state = load_state(&state_path);
        let closes = crate::attention::closes(&stores.concat());
        let asked = crate::attention::asked_ids(&stores);
        let t = tick_pages(
            &items,
            &closes,
            &asked,
            &dir,
            &mut state,
            now_secs(),
            DEFAULT_SETTLE_SECS,
            &mut RealIo::new(&cwd),
        );
        save_state(&state_path, &state);
        acted += t.delivered + t.recorded + t.closed;
        if skip.is_none() {
            skip = t.skip.clone();
        }
        detail.extend(t.detail);
        // The ntfy and webhook sinks ride the same beat: load (a loopback
        // answer_url is refused at load), deliver, close, retry transient
        // failures next beat with the same delivery_id.
        mark("sinks");
        let (sinks, refused) = crate::attention_http::load_sinks(&cwd);
        for r in &refused {
            detail.push(format!("sinks: refused sink {}: {}", r.name, r.reason));
        }
        let http = crate::attention_http::tick_sinks(
            &items,
            &sinks,
            &attention_dir().unwrap_or_else(|_| dir.join(".state")),
            started + std::time::Duration::from_secs(ATTENTION_TICK_BUDGET_S),
            &mut crate::attention_http::CurlPost,
        );
        acted += http.acted();
        if skip.is_none() {
            skip = http.skip.clone();
        }
        detail.extend(http.detail);
        // The chat lane: post, close and read replies on the chat rows this
        // machine owns. A reply lands as an attention_answer row, so the
        // answers stage below clears it in this same beat.
        mark("chat");
        let (chat_sinks, chat_refused, chat_skipped) = crate::attention_chat::load_chat_sinks(&cwd);
        for r in &chat_refused {
            detail.push(format!("chat: refused sink {}: {}", r.name, r.reason));
        }
        detail.extend(chat_skipped);
        let mut http_seams: Vec<crate::attention_chat::CurlHttp> = chat_sinks
            .iter()
            .map(|_| crate::attention_chat::CurlHttp)
            .collect();
        let mut transports: Vec<Box<dyn crate::attention_chat::ChatTransport + '_>> = chat_sinks
            .iter()
            .zip(http_seams.iter_mut())
            .map(|(s, h)| crate::attention_chat::transport_for(s, h))
            .collect();
        let mut lanes: Vec<(
            &crate::attention_chat::ChatSink,
            &mut dyn crate::attention_chat::ChatTransport,
        )> = chat_sinks
            .iter()
            .zip(transports.iter_mut())
            .map(|(s, t)| (s, &mut **t as &mut dyn crate::attention_chat::ChatTransport))
            .collect();
        let chat = crate::attention_chat::tick_chat(
            &items,
            &mut lanes,
            &attention_dir().unwrap_or_else(|_| dir.join(".state")),
            started + std::time::Duration::from_secs(ATTENTION_TICK_BUDGET_S),
        );
        acted += chat.acted();
        if skip.is_none() {
            skip = chat.skip.clone();
        }
        detail.extend(chat.detail);
        // The mux pass: every unsuperseded attention_answer row, whatever its
        // sink, drives one reply ladder (clear while open, mail, resume,
        // team). Runs with no sinks configured - the answer rows are the
        // input, not the sink config.
        mark("answers");
        let (ans_acted, ans_detail) = crate::attention_reply::tick_answers(
            &items,
            &cwd,
            &attention_dir().unwrap_or_else(|_| dir.join(".state")),
            started + std::time::Duration::from_secs(ATTENTION_TICK_BUDGET_S),
            &mut RealIo::new(&cwd),
            &|argv: &[String]| crate::attention_reply::real_runner_pub(argv),
        );
        acted += ans_acted;
        detail.extend(ans_detail);
        if acted == 0 && skip.is_none() {
            // An idle beat must say which kind of idle: a folder of open
            // pages is not an empty projection.
            skip = Some(
                if items.is_empty() {
                    "no_open_items"
                } else {
                    "nothing_new"
                }
                .to_string(),
            );
            detail.push(format!("open={}", items.len()));
        }
        *stage.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let summary = detail.join("; ");
        emit_tick_row(&home, acted, skip.as_deref(), &summary);
    });
}

/// The stuck report for a live tick: the stage name and how long the tick
/// has run, but only once it is past the budget.
fn stuck_detail(
    stage: Option<(&'static str, std::time::Instant)>,
    budget_s: u64,
) -> Option<String> {
    let (name, since) = stage?;
    let secs = since.elapsed().as_secs();
    (secs > budget_s).then(|| format!("tick still in stage {name} after {secs}s"))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The projection read: question journals + escalation notes + user lane,
/// the same three stores `fno-agents needs --items` folds. Returns the items,
/// the names of stores that exist but could not be read (an unreadable store
/// means an INCOMPLETE projection, and the caller must not treat its absent
/// ids as closed), and the raw journal text the `closes` fold reads.
fn read_items(cwd: &Path) -> (Vec<AttentionItem>, Vec<String>, Vec<String>) {
    let fno_dir = crate::paths::AgentsHome::from_env()
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let mark = inputs_mark(&fno_dir, cwd);
    let mut cache = PROJECTION.lock().unwrap_or_else(|e| e.into_inner());
    let (mut items, unreadable, stores) = match (&mark, cache.as_ref()) {
        (Some(mark), Some((held, items, stores))) if mark == held => {
            (items.clone(), Vec::new(), stores.clone())
        }
        _ => {
            let read = project_items(&fno_dir, cwd);
            // Only a complete projection is reused.
            *cache = match (&mark, read.1.is_empty()) {
                (Some(mark), true) => Some((mark.clone(), read.0.clone(), read.2.clone())),
                _ => None,
            };
            read
        }
    };
    drop(cache);
    attach_registry_reach(&mut items);
    (items, unreadable, stores)
}

/// The last complete projection and the input mark it was read at. The
/// projection is pure in its inputs, so an unchanged mark reuses it; reach
/// still reads a fresh registry every tick.
static PROJECTION: Mutex<Option<(Vec<String>, Vec<AttentionItem>, Vec<String>)>> = Mutex::new(None);

/// One line per projection input: each question store's committed seq, each
/// escalation note's mtime, the operator lane's length and mtime. `None`
/// when a journal holds bytes its store has not ingested, or a store cannot
/// be read: then only a full read answers.
fn inputs_mark(fno_dir: &Path, cwd: &Path) -> Option<Vec<String>> {
    let stamp = |path: &Path| {
        std::fs::metadata(path).ok().map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{}", m.len(), mtime)
        })
    };
    let mut mark = Vec::new();
    for journal in crate::needs::question_journals(fno_dir, cwd) {
        let live = crate::event_store::live_journal(&journal);
        let store = crate::event_store::store_path(&live);
        if !live.exists() && !store.exists() {
            mark.push(format!("j:{}:absent", live.display()));
            continue;
        }
        // A store with no raw journal beside it has no bytes left to ingest.
        if live.exists() && crate::event_store::stray_has_uningested_bytes(&live, &store) {
            return None;
        }
        let seq = crate::event_store::max_seq(&live).ok()?;
        mark.push(format!("j:{}:{seq}", live.display()));
    }
    let notes = crate::escalation::dir(cwd);
    if let Ok(entries) = std::fs::read_dir(&notes) {
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| {
                let path = e.path();
                format!("n:{}:{}", path.display(), stamp(&path).unwrap_or_default())
            })
            .collect();
        names.sort();
        mark.extend(names);
    }
    let lane = crate::org_board::scope::operator_lane_path(cwd);
    mark.push(format!("l:{}", stamp(&lane).unwrap_or_default()));
    Some(mark)
}

fn attach_registry_reach(items: &mut [AttentionItem]) {
    if let Ok(registry) =
        crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
    {
        crate::attention::attach_reach(items, &registry);
    }
}

pub(crate) fn read_items_at(
    fno_dir: &Path,
    cwd: &Path,
) -> (Vec<AttentionItem>, Vec<String>, Vec<String>) {
    let (mut items, unreadable, stores) = project_items(fno_dir, cwd);
    attach_registry_reach(&mut items);
    (items, unreadable, stores)
}

/// The uncached projection, before reach: the read `read_items_at` and
/// the arm's cache miss share.
fn project_items(fno_dir: &Path, cwd: &Path) -> (Vec<AttentionItem>, Vec<String>, Vec<String>) {
    let mut stores: Vec<String> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    for path in crate::needs::question_journals(fno_dir, cwd) {
        match crate::event_store::journal_text_checked(
            &path,
            &crate::event_store::EventQuery::of_types(crate::needs::PROJECTION_TYPES),
        ) {
            Ok(mut content) => {
                // A store that holds rows but no question rows never carries
                // asks (the global events store), so it must not read as a
                // lost store to `asked_ids`. A store with no row at all still
                // does.
                if content.trim().is_empty()
                    && crate::event_store::journal_text_checked(
                        &path,
                        &crate::event_store::EventQuery {
                            limit: Some(1),
                            ..Default::default()
                        },
                    )
                    .is_ok_and(|any| !any.trim().is_empty())
                {
                    continue;
                }
                if !content.ends_with('\n') {
                    content.push('\n');
                }
                stores.push(content);
            }
            Err(e) => unreadable.push(format!(
                "{}: {e}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("journal")
            )),
        }
    }
    let notes = read_notes(cwd);
    let lane_path = crate::org_board::scope::operator_lane_path(cwd);
    let lane_text = std::fs::read_to_string(lane_path).unwrap_or_default();
    let items = crate::attention::project(&stores.concat(), &notes, &lane_text, now_secs());
    (items, unreadable, stores)
}

/// Escalation notes as (slug, text) pairs.
fn read_notes(cwd: &Path) -> Vec<(String, String)> {
    let dir = crate::escalation::dir(cwd);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    paths.sort();
    for path in paths {
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("note")
            .to_string();
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.push((slug, text));
        }
    }
    out
}

/// The projection cache the prompt hook reads (wave 4). `{as_of, items,
/// questions_dir}`.
fn write_items_cache(
    _home: &crate::paths::AgentsHome,
    items: &[AttentionItem],
    questions_dir: &Path,
) {
    let Ok(dir) = attention_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let payload = json!({
        "as_of": now_secs(),
        "items": items,
        "questions_dir": questions_dir.display().to_string(),
    });
    let tmp = dir.join(".items.tmp");
    if serde_json::to_string(&payload)
        .map(|s| std::fs::write(&tmp, s))
        .unwrap_or(Err(std::io::Error::other("serialize")))
        .is_ok()
    {
        let _ = std::fs::rename(&tmp, dir.join("items.json"));
    }
}

/// The tick row: one per beat, the same shape every arm writes.
fn emit_tick_row(
    home: &crate::paths::AgentsHome,
    acted: u64,
    skip_reason: Option<&str>,
    detail: &str,
) {
    let journal = crate::loop_runtime::Journal::new_raw(
        home.events_jsonl(),
        crate::daemon::global_events_path(home),
    );
    crate::tick_ledger::emit_tick(
        &journal,
        "attention",
        crate::tick_ledger::SCHED_DAEMON,
        acted,
        skip_reason,
        Some(detail),
        ATTENTION_INTERVAL_S,
    );
}

/// The real IO: disk for the pages, `questions.jsonl` for the answer row,
/// `fno` for the clear, the confirmed notice for the user. The router loads
/// lazily, on the first `route` call of the beat.
struct RealIo {
    cwd: PathBuf,
    router: Option<Result<Router, String>>,
}

impl RealIo {
    fn new(cwd: &Path) -> RealIo {
        RealIo {
            cwd: cwd.to_path_buf(),
            router: None,
        }
    }

    fn router(&mut self) -> Result<&Router, String> {
        if self.router.is_none() {
            let registry = crate::paths::AgentsHome::from_env().registry_json();
            self.router = Some(Router::load(&self.cwd, &registry));
        }
        match self.router.as_ref().unwrap() {
            Ok(r) => Ok(r),
            Err(e) => Err(e.clone()),
        }
    }
}

impl SinkIo for RealIo {
    fn read(&mut self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn write_atomic(&mut self, path: &Path, content: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, content)?;
        let mode = std::fs::metadata(path).ok().map(|m| m.permissions());
        let res = std::fs::rename(&tmp, path);
        if let (Ok(()), Some(mode)) = (&res, mode) {
            let _ = std::fs::set_permissions(path, mode);
        }
        res
    }

    fn create_new(&mut self, path: &Path, content: &str) -> std::io::Result<bool> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, content)?;
        match std::fs::hard_link(&tmp, path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp);
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::rename(from, to)
    }

    fn remove(&mut self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn arm_id(&mut self) -> Option<String> {
        let machine = crate::claims::machine_id();
        if machine.is_empty() {
            return None;
        }
        let root = crate::paths::AgentsHome::from_env().root().to_path_buf();
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Some(format!(
            "{:016x}",
            fnv1a(&format!("{machine}\0{}", root.display()))
        ))
    }

    fn list_md(&mut self, dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(path);
            }
        }
        out.sort();
        out
    }

    fn path_exists(&mut self, path: &Path) -> bool {
        path.exists()
    }

    fn route(&mut self, item: &AttentionItem) -> Result<Routing, String> {
        Ok(self.router()?.route(item))
    }

    fn record(
        &mut self,
        item: &AttentionItem,
        sink_name: &str,
        answer: &FileAnswer,
    ) -> Result<String, String> {
        let (receipt, _) = append_answer_row(
            &item.id,
            sink_name,
            answer,
            "file_edit",
            "attention_arm",
            &format!("file:{}", item.id),
        )?;
        Ok(receipt)
    }

    fn clear(&mut self, id: &str, answer_text: &str) -> Result<String, String> {
        let mut cmd = crate::loop_dispatch::fno_cmd("fno");
        cmd.args(["inbox", "outstanding", "clear", id, "--answer", answer_text]);
        // A kill past ATTENTION_CLEAR_TIMEOUT_S reads as a nonzero status,
        // so it lands in the caller's bounded retry (AC4-ERR) instead of
        // parking the tick on one wedged Python clear.
        let out = crate::bounded_cmd::output_with_timeout_result(cmd, ATTENTION_CLEAR_TIMEOUT_S)
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(last_line(&String::from_utf8_lossy(&out.stderr)))
        } else {
            Err(format!("clear exited {}", out.status.code().unwrap_or(-1)))
        }
    }

    fn notify(&mut self, title: &str, body: &str) {
        crate::operator_notice::notify_operator_confirmed(title, body, None);
    }
}

/// The last non-empty line, trimmed: the posture line a clear prints.
pub(crate) fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// The shared durable half of every answer lane: the first-answer-wins check
/// against `questions.jsonl`, then the `attention_answer` append. Both the
/// file arm and the `needs --answer` door call this, so the two writers can
/// never disagree on the row shape. Returns `(receipt, superseded)`. A failed
/// append fails the caller, so no settle state ever marks a row that did not
/// land.
pub(crate) fn append_answer_row(
    item_id: &str,
    sink: &str,
    answer: &crate::attention_file::FileAnswer,
    authority: &str,
    mapped_by: &str,
    attested_by: &str,
) -> Result<(String, bool), String> {
    let (option, words, done) = match answer {
        crate::attention_file::FileAnswer::Option(n) => (Some(*n as i64), String::new(), false),
        crate::attention_file::FileAnswer::Words(w) => (None, w.clone(), false),
        crate::attention_file::FileAnswer::Done => (None, String::new(), true),
        crate::attention_file::FileAnswer::Delegate => (
            None,
            crate::attention_file::DELEGATE_TEXT.to_string(),
            false,
        ),
        crate::attention_file::FileAnswer::None | crate::attention_file::FileAnswer::TwoTicked => {
            (None, String::new(), false)
        }
    };
    let answered_at = chrono::Utc::now().to_rfc3339();
    // First answer wins across writers: an earlier unsuperseded row for
    // this item makes this one a superseded marker that changes nothing.
    let home = crate::paths::AgentsHome::from_env();
    let path = crate::provider_cap::questions_path(&home);
    // First-answer-wins is check-and-append: two lanes racing both read an
    // empty journal and both append unsuperseded rows. The per-agent flock
    // serializes the whole critical section across processes.
    let _lock = crate::agent_lock::AgentLock::acquire(
        &home,
        "attention-answer",
        std::time::Duration::from_secs(5),
    )
    .map_err(|_| "another answer writer holds the lock; retry".to_string())?;
    let already_won = crate::event_store::journal_text(&path, &[])
        .lines()
        .any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .is_some_and(|v| {
                    v.get("type").and_then(serde_json::Value::as_str) == Some("attention_answer")
                        && v.get("data")
                            .and_then(|d| d.get("item_id"))
                            .and_then(serde_json::Value::as_str)
                            == Some(item_id)
                        && v.get("data")
                            .and_then(|d| d.get("superseded"))
                            .and_then(serde_json::Value::as_bool)
                            != Some(true)
                })
        });
    let receipt = match (answer, already_won) {
        (crate::attention_file::FileAnswer::Option(n), false) => {
            format!("Recorded: option {n} ({sink})")
        }
        (crate::attention_file::FileAnswer::Words(_), false) => {
            format!("Recorded: words ({sink})")
        }
        (crate::attention_file::FileAnswer::Done, false) => format!("Recorded: done ({sink})"),
        (crate::attention_file::FileAnswer::Delegate, false) => {
            format!("Recorded: delegated to the team ({sink})")
        }
        (
            crate::attention_file::FileAnswer::None | crate::attention_file::FileAnswer::TwoTicked,
            false,
        ) => String::new(),
        (_, true) => "Recorded: superseded by an earlier answer".to_string(),
    };
    let mut fields = serde_json::Map::new();
    fields.insert("item_id".into(), serde_json::Value::from(item_id));
    fields.insert("sink".into(), serde_json::Value::from(sink));
    fields.insert("option".into(), serde_json::Value::from(option));
    fields.insert("words".into(), serde_json::Value::from(words));
    fields.insert("done".into(), serde_json::Value::from(done));
    fields.insert("answered_at".into(), serde_json::Value::from(answered_at));
    fields.insert("authority".into(), serde_json::Value::from(authority));
    fields.insert("attested_by".into(), serde_json::Value::from(attested_by));
    fields.insert("mapped_by".into(), serde_json::Value::from(mapped_by));
    fields.insert("superseded".into(), serde_json::Value::from(already_won));
    fields.insert(
        "delegate".into(),
        serde_json::Value::from(matches!(
            answer,
            crate::attention_file::FileAnswer::Delegate
        )),
    );
    fields.insert("decision_id".into(), serde_json::Value::Null);
    // The row is the durable half of the contract: a commit that fails must
    // fail the record so the settle state never marks it recorded.
    crate::events::EventEmitter::new(&path, "daemon")
        .emit_fields("attention_answer", fields)
        .map_err(|e| format!("answer row for {item_id}: {e}"))?;
    Ok((receipt, already_won))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_items_carries_a_store_committed_question() {
        // AC11-ARM: a store-only operator_question reaches the projection.
        let _root = crate::paths::DeclaredRoot::declare("attention_store_questio");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let space = crate::paths::space_dir(&cwd).join("events.jsonl");
        let ask = serde_json::json!({
            "ts": "2026-09-17T12:00:00Z", "type": "operator_question", "source": "test",
            "data": {"question_id": "q-arm-1", "question": "ship?", "blocks": []}
        });
        crate::event_store::append_envelope(&space, &ask.to_string(), None).unwrap();
        // The global events store holds rows but never a question row; the
        // questions store holds the other asks.
        let tick = serde_json::json!({
            "ts": "2026-09-17T12:00:00Z", "type": "control_plane_tick", "source": "test",
            "data": {"arm": "attention", "scheduler": "daemon", "acted": 0, "interval_s": 60}
        });
        crate::event_store::append_envelope(
            &dir.path().join("events.jsonl"),
            &tick.to_string(),
            None,
        )
        .unwrap();
        let ask2 = serde_json::json!({
            "ts": "2026-09-17T12:00:00Z", "type": "operator_question", "source": "test",
            "data": {"question_id": "q-arm-2", "question": "ship?", "blocks": []}
        });
        crate::event_store::append_envelope(
            &dir.path().join("questions.jsonl"),
            &ask2.to_string(),
            None,
        )
        .unwrap();
        let (items, unreadable, stores) = read_items_at(dir.path(), &cwd);
        assert!(unreadable.is_empty(), "{unreadable:?}");
        assert!(
            items.iter().any(|i| i.id.contains("q-arm-1")),
            "the store-only question projects: {items:?}"
        );
        let asked = crate::attention::asked_ids(&stores);
        assert!(
            asked.contains("q-arm-1") && asked.contains("q-arm-2"),
            "a store that carries no questions does not block the asks: {asked:?}"
        );
    }

    /// AC6: unchanged stores, notes and lane keep the input mark, so the
    /// arm reuses its projection; a new question row moves the mark.
    #[test]
    fn the_input_mark_moves_only_when_a_projection_input_moves() {
        let _root = crate::paths::DeclaredRoot::declare("attention_input_mark");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let space = crate::paths::space_dir(&cwd).join("events.jsonl");
        let ask = |id: &str| {
            serde_json::json!({
                "ts": "2026-09-17T12:00:00Z", "type": "operator_question", "source": "test",
                "data": {"question_id": id, "question": "ship?", "blocks": []}
            })
            .to_string()
        };
        crate::event_store::append_envelope(&space, &ask("q-mark-1"), None).unwrap();
        let first = inputs_mark(dir.path(), &cwd).expect("a settled store marks");
        assert_eq!(inputs_mark(dir.path(), &cwd), Some(first.clone()));
        crate::event_store::append_envelope(&space, &ask("q-mark-2"), None).unwrap();
        let second = inputs_mark(dir.path(), &cwd).expect("still settled");
        assert_ne!(first, second, "a new question row moves the mark");
    }

    #[test]
    fn read_items_names_an_unreadable_store() {
        // AC11-ARM: an unreadable store names itself in `unreadable`.
        let _root = crate::paths::DeclaredRoot::declare("attention_unreadable_s");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let space_dir = crate::paths::space_dir(&cwd);
        std::fs::create_dir_all(&space_dir).unwrap();
        std::fs::write(space_dir.join("events.db"), b"not a database").unwrap();
        let (_, unreadable, _) = read_items_at(dir.path(), &cwd);
        assert!(
            unreadable.iter().any(|u| u.contains("events")),
            "{unreadable:?}"
        );
    }

    use crate::attention::project;

    /// One not-ready question with a live asker: every context field but
    /// `unknowns` is present, so `missing` names exactly one field.
    fn not_ready_items() -> Vec<AttentionItem> {
        let row = r#"{"ts":"2026-09-18T12:00:00Z","type":"operator_question","source":"test","data":{"question_id":"q-nr","question":"Which?","ask":"pick","session_id":"s1","cwd":"/repo/fno","node":"x-aaaa","asker":"worker-1","options":[{"n":1,"text":"A","next":"x"},{"n":2,"text":"B","next":"y"}],"context":{"blocked_because":"two repairs are Python edits","options_rationale":"the readings leads acted on","recommendation":{"option":1,"why":"narrowest"},"reversible":"costly","cost_if_wrong":"allowance drops","meanwhile":"stops"}}}"#;
        project(row, &[], "", 0)
    }

    fn bounce_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "attention-bounce-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn bounce_deadline() -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_secs(60)
    }

    #[test]
    fn ac9_hp_the_bounce_mails_once_then_stays_quiet() {
        let items = not_ready_items();
        assert!(!items[0].ready);
        let dir = bounce_dir("once");
        std::fs::create_dir_all(&dir).unwrap();
        let sends = std::sync::atomic::AtomicUsize::new(0);
        {
            let send = |asker: &str, body: &str| {
                use std::sync::atomic::Ordering;
                assert_eq!(asker, "worker-1");
                assert!(body.contains("q-nr"), "names the item id: {body}");
                assert!(
                    body.contains("missing unknowns"),
                    "names the missing field: {body}"
                );
                sends.fetch_add(1, Ordering::SeqCst);
                true
            };
            let deferred = bounce_not_ready_with(&items, &dir, bounce_deadline(), &send);
            assert_eq!(deferred, 0);
        }
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        // Second beat: quiet.
        bounce_not_ready_with(&items, &dir, bounce_deadline(), &|_a, _b| {
            panic!("a second beat must stay quiet");
        });
    }

    #[test]
    fn ac3_err_a_spent_budget_defers_the_eligible_item() {
        // AC3-ERR: past the beat deadline the bounce sends nothing and
        // reports the eligible item as deferred.
        let items = not_ready_items();
        let dir = bounce_dir("budget");
        std::fs::create_dir_all(&dir).unwrap();
        let spent = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let deferred = bounce_not_ready_with(&items, &dir, spent, &|_a, _b| {
            panic!("a spent budget must not send");
        });
        assert_eq!(deferred, 1);
        assert!(!dir.join("bounced.json").exists(), "nothing was sent");
    }

    /// In-memory IO for the tick body tests.
    struct MemIo {
        files: std::collections::BTreeMap<PathBuf, String>,
        broken_routing: bool,
        arm: Option<String>,
    }

    impl Default for MemIo {
        fn default() -> Self {
            MemIo {
                files: std::collections::BTreeMap::new(),
                broken_routing: false,
                arm: None,
            }
        }
    }

    impl MemIo {
        fn path_of(&self, needle: &str) -> Option<PathBuf> {
            self.files
                .keys()
                .find(|p| p.to_string_lossy().contains(needle))
                .cloned()
        }
    }

    impl SinkIo for MemIo {
        fn read(&mut self, path: &Path) -> std::io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))
        }
        fn write_atomic(&mut self, path: &Path, content: &str) -> std::io::Result<()> {
            self.files.insert(path.to_path_buf(), content.to_string());
            Ok(())
        }
        fn create_new(&mut self, path: &Path, content: &str) -> std::io::Result<bool> {
            if self.files.contains_key(path) {
                return Ok(false);
            }
            self.files.insert(path.to_path_buf(), content.to_string());
            Ok(true)
        }
        fn rename(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
            let content = self
                .files
                .remove(from)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))?;
            self.files.insert(to.to_path_buf(), content);
            Ok(())
        }

        fn remove(&mut self, path: &Path) -> std::io::Result<()> {
            self.files.remove(path);
            Ok(())
        }

        fn arm_id(&mut self) -> Option<String> {
            self.arm.clone()
        }
        fn list_md(&mut self, dir: &Path) -> Vec<PathBuf> {
            self.files
                .keys()
                .filter(|p| {
                    p.parent() == Some(dir) && p.extension().and_then(|e| e.to_str()) == Some("md")
                })
                .cloned()
                .collect()
        }
        fn path_exists(&mut self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
        fn route(&mut self, _item: &AttentionItem) -> Result<Routing, String> {
            if self.broken_routing {
                Err("attention_route: graph unreadable (test)".to_string())
            } else {
                Ok(Routing::default())
            }
        }
        fn record(
            &mut self,
            _item: &AttentionItem,
            _sink: &str,
            _answer: &FileAnswer,
        ) -> Result<String, String> {
            Ok("Recorded: option 1 (file)".to_string())
        }
        fn clear(&mut self, _id: &str, _answer_text: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn notify(&mut self, _title: &str, _body: &str) {}
    }

    #[test]
    fn a_lost_close_is_owner_scoped_and_keeps_one_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo::default();
        io.arm = Some("aaaa1111aaaa1111".to_string());
        let items = vec![
            ready_item("q-lost", "question"),
            ready_item("q-kept", "question"),
        ];
        let mut state = HashMap::new();
        let mut beat = |items: &[AttentionItem], asked: &[&str], now: u64, io: &mut MemIo| {
            let asked: HashSet<String> = asked.iter().map(|s| s.to_string()).collect();
            tick_pages(
                items,
                &HashMap::new(),
                &asked,
                dir.path(),
                &mut state,
                now,
                120,
                io,
            )
        };
        beat(&items, &[], 1000, &mut io);
        let stamped = io.files[&io.path_of("q-lost").unwrap()].clone();
        let (front, _) = parse_page(&stamped).unwrap();
        assert_eq!(
            front.arm.as_deref(),
            Some("aaaa1111aaaa1111"),
            "delivery stamps the owner"
        );
        // A read with no ask row at all proves no loss: nothing closes.
        assert_eq!(beat(&[], &[], 1030, &mut io).closed, 0);
        // Only q-kept still has an ask row: q-lost closes, q-kept waits.
        assert_eq!(beat(&[], &["q-kept"], 1060, &mut io).closed, 1);
        let done = io.path_of("done/q-lost").expect("the lost page moved");
        let text = io.files[&done].clone();
        let (front, _) = parse_page(&text).unwrap();
        assert_eq!(front.status, "closed");
        assert_eq!(front.answer, None, "a lost close answers nothing");
        assert_eq!(front.recorded_by.as_deref(), Some(LOST_BY));
        assert_eq!(
            front.arm.as_deref(),
            Some("aaaa1111aaaa1111"),
            "the stamp survives the close"
        );
        assert!(text.contains(LOST_REASON), "{text}");
        assert!(!text.contains("Recorded: Recorded:"), "{text}");
        let kept = io.path_of("q-kept").expect("the asked page stays");
        assert!(!kept.to_string_lossy().contains("done/"));
        // The question reads open again: its page comes back.
        let back = beat(&items[..1], &["q-lost", "q-kept"], 1090, &mut io);
        assert_eq!(back.delivered, 1, "a lost close never blocks redelivery");
        // It reads lost again: the close reuses the one lost copy in done/
        // instead of minting <id>-2 (AC2-HP).
        assert_eq!(beat(&[], &["q-kept"], 1120, &mut io).closed, 1);
        let lost_copies = io
            .files
            .keys()
            .filter(|p| p.to_string_lossy().contains("done/q-lost"))
            .count();
        assert_eq!(lost_copies, 1, "one lost copy per id");

        // Two arms, one folder. Arm B delivers its own page and sees arm
        // A's open page as foreign: it closes neither (AC1-ERR, AC4-HP).
        io.arm = Some("bbbb2222bbbb2222".to_string());
        let own_b = vec![ready_item("q-own", "question")];
        let kept_a = vec![ready_item("q-kept", "question")];
        let t = beat(&own_b, &["q-own"], 1150, &mut io);
        assert_eq!(t.closed, 0, "arm B closes nothing of arm A's");
        assert!(io.path_of("q-own").is_some(), "B delivers its own page");
        for (arm, items, asked, now) in [
            ("aaaa1111aaaa1111", kept_a.as_slice(), &["q-kept"][..], 1180),
            ("bbbb2222bbbb2222", own_b.as_slice(), &["q-own"][..], 1210),
            ("aaaa1111aaaa1111", kept_a.as_slice(), &["q-kept"][..], 1240),
            ("bbbb2222bbbb2222", own_b.as_slice(), &["q-own"][..], 1270),
            ("aaaa1111aaaa1111", kept_a.as_slice(), &["q-kept"][..], 1300),
        ] {
            io.arm = Some(arm.to_string());
            let t = beat(items, asked, now, &mut io);
            assert_eq!(t.closed, 0, "no arm closes the other's pages: {t:?}");
            assert!(
                t.detail.iter().any(|d| d.starts_with("foreign page ")),
                "the beat names the foreign page: {t:?}"
            );
        }
        assert!(io.path_of("q-kept").is_some(), "arm A's page stays open");
        assert!(io.path_of("q-own").is_some(), "arm B's page stays open");
        let lost_copies = io
            .files
            .keys()
            .filter(|p| p.to_string_lossy().contains("done/q-lost"))
            .count();
        assert_eq!(lost_copies, 1, "still one lost copy per id");
    }

    #[test]
    fn ac3_drain_removes_duplicate_lost_copies_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo::default();
        let done = dir.path().join("done");
        // 250 lost copies of one id plus an answered neighbour: the shape
        // done/ held after the 2026-10-22 flap.
        let lost_item = ready_item("q-x", "question");
        let kept_item = ready_item("q-y", "question");
        for n in 1..=250u32 {
            let name = if n == 1 {
                "q-x.md".to_string()
            } else {
                format!("q-x-{n}.md")
            };
            let ts = format!("2026-10-22T{:02}:{:02}:00Z", 5 + n / 60, n % 60);
            let page = close_page(
                &render_page(&lost_item, &Routing::default()),
                "closed",
                "",
                &ts,
                LOST_BY,
                "Recorded: lost, no question row in the stores (attention_lost)",
            );
            io.files.insert(done.join(name), page);
        }
        let answered = close_page(
            &render_page(&kept_item, &Routing::default()),
            "answered",
            "narrow",
            "2026-10-22T06:00:00Z",
            "file_edit",
            "Recorded: option 1 (file)",
        );
        io.files.insert(done.join("q-y.md"), answered);

        let mut state = HashMap::new();
        let t = tick_pages(
            &[],
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            120,
            &mut io,
        );
        assert_eq!(t.closed, 0, "the drain never counts as a close");
        assert!(
            t.detail
                .iter()
                .any(|d| d.contains("removed 200 duplicate lost copies")),
            "the cap names the removal: {t:?}"
        );
        let left = io
            .files
            .keys()
            .filter(|p| p.to_string_lossy().contains("done/q-x"))
            .count();
        assert_eq!(left, 50, "250 copies lose 200 (AC3-ERR)");
        assert!(
            io.files.contains_key(&done.join("q-y.md")),
            "the answered neighbour is untouched (AC3-HP)"
        );
        // The next beat removes the rest.
        let t2 = tick_pages(
            &[],
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1030,
            120,
            &mut io,
        );
        assert!(
            t2.detail
                .iter()
                .any(|d| d.contains("removed 49 duplicate lost copies")),
            "the tail removes next beat: {t2:?}"
        );
        let left2 = io
            .files
            .keys()
            .filter(|p| p.to_string_lossy().contains("done/q-x"))
            .count();
        assert_eq!(left2, 1, "one lost copy per id");
    }

    fn ready_item(id: &str, kind: &str) -> AttentionItem {
        let mut item = not_ready_items().remove(0);
        item.id = id.to_string();
        item.kind = kind.to_string();
        item.ready = true;
        item.missing.clear();
        item
    }

    #[test]
    fn ac4_hp_two_questions_deliver_two_pages_and_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo::default();
        let items = vec![
            ready_item("q-a1", "question"),
            ready_item("q-b2", "question"),
        ];
        let mut state = HashMap::new();
        let t = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            120,
            &mut io,
        );
        assert_eq!(t.delivered, 2, "AC4-HP");
        assert_eq!(t.acted(), 2);
        assert!(io.path_of("q-a1").is_some());
        assert!(io.path_of("q-b2").is_some());
        // The second beat writes nothing new (AC4-EDGE).
        let before: std::collections::BTreeMap<_, _> = io.files.clone();
        let t2 = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1030,
            120,
            &mut io,
        );
        assert_eq!(t2.delivered, 0, "AC4-EDGE: no second page");
        assert_eq!(before, io.files, "byte-identical across beats");
    }

    #[test]
    fn ac4_err_unreadable_graph_delivers_nothing_and_names_the_skip() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo {
            broken_routing: true,
            ..Default::default()
        };
        let items = vec![ready_item("q-a1", "question")];
        let mut state = HashMap::new();
        let t = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            120,
            &mut io,
        );
        assert_eq!(t.delivered, 0, "AC4-ERR");
        assert_eq!(t.skip.as_deref(), Some("routing_unreadable"));
        assert_eq!(io.files.len(), 0);
    }

    #[test]
    fn ac5_hp_ticked_option_settles_records_and_moves_to_done() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo::default();
        let items = vec![ready_item("q-a1", "question")];
        let mut state = HashMap::new();
        tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            120,
            &mut io,
        );
        // The user ticks option 2; the hash changes, restarting the window.
        let path = io.path_of("q-a1").unwrap();
        let ticked = io.files[&path].replace("- [ ] 2.", "- [x] 2.");
        io.files.insert(path.clone(), ticked);
        // Younger than the settle window: nothing records (AC5-EDGE).
        let t2 = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1050,
            120,
            &mut io,
        );
        assert_eq!(t2.recorded, 0, "AC5-EDGE");
        // Past the window: the answer records, the clear runs, the page
        // moves to done/.
        let t3 = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1300,
            120,
            &mut io,
        );
        assert_eq!(t3.recorded, 1, "AC5-HP");
        assert_eq!(t3.closed, 1);
        assert!(
            io.path_of(&format!("done/q-a1")).is_some(),
            "moved to done/"
        );
        assert!(io.path_of("2026-").is_none(), "the open page moved away");
        let done_path = io.path_of("done/q-a1").unwrap();
        let (front, _) = parse_page(&io.files[&done_path]).unwrap();
        assert_eq!(front.status, "answered");
        assert_eq!(front.recorded_by.as_deref(), Some("file_edit"));
        assert_eq!(front.answer.as_deref(), Some("B"));
        // The index lists it under Done.
        let index_path = dir.path().join("questions.md");
        let index = io.files[&index_path].clone();
        assert!(index.contains("## Done"), "{index}");
        assert!(index.contains("[[q-a1|"), "{index}");
        let _declared = crate::paths::DeclaredRoot::declare("recovery_page_baseline");
        let mut io = MemIo::default();
        let mut items = vec![ready_item("q-history", "question")];
        let mut state = HashMap::new();
        tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            0,
            &mut io,
        );
        let page = io.path_of("q-history").unwrap();
        let baseline = io.files[&page].replace("- [ ] 2.", "- [x] 2.");
        io.files.insert(page.clone(), baseline.clone());
        let batch = "a".repeat(64);
        items[0].recovery_batch = Some(batch.clone());
        let store = crate::event_store::store_path(&crate::provider_cap::questions_path(
            &crate::paths::AgentsHome::from_env(),
        ));
        let recovery_dir = store
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("backups/state-recovery")
            .join(&batch);
        let snapshot = recovery_dir.join("sidecar-0");
        io.files.insert(snapshot.clone(), baseline.clone());
        io.files.insert(recovery_dir.join("snapshots.json"),json!([{"source":page,"snapshot":snapshot,"sha256":format!("{:x}",Sha256::digest(baseline.as_bytes()))}]).to_string());
        state.clear();
        for now in [1100, 1300] {
            let tick = tick_pages(
                &items,
                &HashMap::new(),
                &HashSet::new(),
                dir.path(),
                &mut state,
                now,
                0,
                &mut io,
            );
            assert_eq!(
                tick.acted(),
                0,
                "a restored page's historical tick records and delivers nothing"
            );
        }
        io.files.insert(
            page.clone(),
            baseline
                .replace("- [x] 2.", "- [ ] 2.")
                .replace("- [ ] 1.", "- [x] 1."),
        );
        tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1400,
            0,
            &mut io,
        );
        let tick = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1500,
            0,
            &mut io,
        );
        assert_eq!(
            tick.recorded, 1,
            "a genuine new page answer remains actionable"
        );
        let repeated = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1600,
            0,
            &mut io,
        );
        assert_eq!(repeated.recorded, 0, "the new answer records once");
    }

    #[test]
    fn ac6_hp_a_question_closed_elsewhere_closes_from_the_fold() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo::default();
        let items = vec![ready_item("q-a1", "question")];
        let mut state = HashMap::new();
        tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            120,
            &mut io,
        );
        // The question clears at a terminal with answer `narrow` by s9.
        let mut closes: HashMap<String, crate::attention::Closed> = HashMap::new();
        closes.insert(
            "q-a1".to_string(),
            crate::attention::Closed {
                answer: "narrow".to_string(),
                ts: "2026-09-23T01:00:00Z".to_string(),
                closed_by: "s9".to_string(),
                reason: String::new(),
            },
        );
        let t = tick_pages(
            &[],
            &closes,
            &HashSet::new(),
            dir.path(),
            &mut state,
            1300,
            120,
            &mut io,
        );
        assert_eq!(t.closed, 1, "AC6-HP");
        let done_path = io.path_of("done/q-a1").unwrap();
        let (front, _) = parse_page(&io.files[&done_path]).unwrap();
        assert_eq!(front.status, "answered");
        assert_eq!(front.answer.as_deref(), Some("narrow"));
        assert_eq!(front.recorded_by.as_deref(), Some("s9"));
    }

    #[test]
    fn ac7_hp_a_hand_authored_index_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut io = MemIo::default();
        let items = vec![ready_item("q-a1", "question")];
        let mut state = HashMap::new();
        tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1000,
            120,
            &mut io,
        );
        let index_path = dir.path().join("questions.md");
        io.files
            .insert(index_path.clone(), "# my own index\n".to_string());
        let t = tick_pages(
            &items,
            &HashMap::new(),
            &HashSet::new(),
            dir.path(),
            &mut state,
            1030,
            120,
            &mut io,
        );
        assert_eq!(io.files[&index_path], "# my own index\n", "AC7-HP");
        assert!(t.detail.iter().any(|d| d.contains("hand-authored")));
    }
}

impl SinkTick {
    /// `acted` = delivered + recorded + closed.
    pub fn acted(&self) -> u64 {
        self.delivered + self.recorded + self.closed
    }
}

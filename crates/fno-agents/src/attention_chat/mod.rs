//! Chat channel sinks: Telegram, Discord and iMessage (through a
//! BlueBubbles server). Each open question posts once as one message; a
//! reply in that message's thread answers it through the one answer-row
//! writer (`append_answer_row`), and the reply ladder then clears the
//! question and resumes the asker like any other lane. A close line posts
//! in the thread when the question leaves the open set.
//!
//! The lane rides the attention arm beat: no listener port, no second
//! process. Each beat closes, posts, then polls each channel from a stored
//! cursor. Delivery rows reuse the sink shape (`DeliveryRow`, keyed by
//! `delivery_id`) in `chat.json` beside `deliveries.json`.
//!
//! One machine runs each row (`machine`): a Telegram bot allows one poller,
//! and two arms would each post every question. Only senders in `allow` may
//! answer.

mod bluebubbles;
mod discord;
mod telegram;

use crate::attention::AttentionItem;
use crate::attention_file::FileAnswer;
use crate::attention_http::{delivery_id, Deliveries, DeliveryRow, PostResult, RefusedSink};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

/// The row types this loader owns; `load_sinks` skips them.
pub const CHAT_TYPES: [&str; 3] = ["telegram", "discord", "bluebubbles"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChatKind {
    Telegram,
    Discord,
    BlueBubbles,
}

/// One configured chat row, resolved for this machine.
#[derive(Debug, Clone)]
pub struct ChatSink {
    pub name: String,
    pub kind: ChatKind,
    /// The bot token, or the BlueBubbles server password.
    pub token: String,
    /// The sender ids that may answer.
    pub allow: Vec<String>,
    /// Telegram `chat_id`, Discord `channel_id`, BlueBubbles `chat_guid`.
    pub target: String,
    /// BlueBubbles only: the server URL.
    pub url: Option<String>,
}

/// One inbound chat message, normalized by its transport.
#[derive(Debug, Clone, Default)]
pub struct Inbound {
    pub id: String,
    pub sender: String,
    pub text: String,
    /// The external ref of the question message this replies to.
    pub reply_to: Option<String>,
    /// A message the lane itself sent: never an answer.
    pub from_self: bool,
    /// A Telegram button tap to acknowledge.
    pub tap_id: Option<String>,
}

/// One send's outcome: the external ref on success, else retry next beat
/// or drop for good (the sink retry classes).
#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    Posted(String),
    Retry(String),
    Dropped(String),
}

impl Sent {
    /// Classify a response; `id_of` reads the external ref out of a 2xx body.
    fn from_result(r: &PostResult, id_of: impl Fn(&str) -> Option<String>) -> Sent {
        if r.delivered() {
            Sent::Posted(id_of(&r.body).unwrap_or_default())
        } else if r.dropped() {
            Sent::Dropped(format!("status {}", r.status.unwrap_or(0)))
        } else {
            Sent::Retry(match r.status {
                Some(s) => format!("status {s}"),
                None => "no response".to_string(),
            })
        }
    }
}

/// The channel seam. Tests inject a fake; real transports talk HTTP
/// through [`ChatHttp`].
pub trait ChatTransport {
    /// Post a new message with option buttons `(label, data)`.
    fn send(&mut self, text: &str, buttons: &[(String, String)]) -> Sent;
    /// Post into the thread of `external_ref`.
    fn reply(&mut self, external_ref: &str, text: &str) -> Sent;
    /// Read inbound from `cursor`; `open_refs` are the external refs of the
    /// questions still open. An `Err` holds the cursor.
    fn poll(
        &mut self,
        cursor: &str,
        open_refs: &[String],
    ) -> Result<(Vec<Inbound>, String), String>;
    /// Acknowledge a button tap (Telegram only).
    fn ack_tap(&mut self, _tap_id: &str) {}
}

/// The HTTP seam the real transports share. Tests script it.
pub trait ChatHttp {
    fn call(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
    ) -> PostResult;
}

/// The real HTTP: one curl child per call, secrets off argv.
pub struct CurlHttp;

impl ChatHttp for CurlHttp {
    fn call(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<&str>,
    ) -> PostResult {
        crate::attention_http::curl_request(method, url, headers, body)
    }
}

/// The real transport for one sink.
pub fn transport_for<'a>(
    sink: &ChatSink,
    http: &'a mut dyn ChatHttp,
) -> Box<dyn ChatTransport + 'a> {
    match sink.kind {
        ChatKind::Telegram => Box::new(telegram::Telegram::new(sink, http)),
        ChatKind::Discord => Box::new(discord::Discord::new(sink, http)),
        ChatKind::BlueBubbles => Box::new(bluebubbles::BlueBubbles::new(sink, http)),
    }
}

/// Load the chat rows of `[[attention.sinks]]` for this machine. Returns
/// the sinks, the refused rows and one detail line per row that names
/// another machine.
pub fn load_chat_sinks(cwd: &Path) -> (Vec<ChatSink>, Vec<RefusedSink>, Vec<String>) {
    let rows = crate::agents_config::config_lookup(cwd, &["attention", "sinks"])
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect::<Vec<_>>();
    parse_chat_rows(
        &rows,
        &crate::claims::machine_id(),
        &crate::attention_http::resolve_token,
    )
}

/// The pure half of [`load_chat_sinks`].
pub fn parse_chat_rows(
    rows: &[serde_json::Value],
    machine: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> (Vec<ChatSink>, Vec<RefusedSink>, Vec<String>) {
    let mut sinks = Vec::new();
    let mut refused = Vec::new();
    let mut skipped = Vec::new();
    for row in rows {
        let str_of = |key: &str| -> Option<String> {
            row.get(key)
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
        };
        let kind = match str_of("type").as_deref() {
            Some("telegram") => ChatKind::Telegram,
            Some("discord") => ChatKind::Discord,
            Some("bluebubbles") => ChatKind::BlueBubbles,
            _ => continue,
        };
        let name = str_of("name").unwrap_or_default();
        let mut refuse = |reason: String| {
            refused.push(RefusedSink {
                name: name.clone(),
                reason,
            })
        };
        match str_of("machine") {
            None => {
                refuse(format!(
                    "no machine: name the one machine that runs it (this one is {machine})"
                ));
                continue;
            }
            Some(m) if m != machine => {
                skipped.push(format!("chat: {name} runs on machine {m}, not this one"));
                continue;
            }
            Some(_) => {}
        }
        let target_key = match kind {
            ChatKind::Telegram => "chat_id",
            ChatKind::Discord => "channel_id",
            ChatKind::BlueBubbles => "chat_guid",
        };
        let Some(target) = str_of(target_key) else {
            refuse(format!("no {target_key}"));
            continue;
        };
        let url = str_of("url");
        if kind == ChatKind::BlueBubbles && url.is_none() {
            refuse("bluebubbles needs the server url".into());
            continue;
        }
        let allow: Vec<String> = row
            .get("allow")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if allow.is_empty() {
            refuse("allow is empty: name the sender ids that may answer".into());
            continue;
        }
        let env = str_of("token_env").unwrap_or_default();
        let Some(token) = resolve(&env) else {
            refuse(format!("token_env {env} is unset or empty"));
            continue;
        };
        sinks.push(ChatSink {
            name,
            kind,
            token,
            allow,
            target,
            url,
        });
    }
    (sinks, refused, skipped)
}

/// `chat.json`: the delivery rows (one per sink and item) and one poll
/// cursor per sink.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ChatState {
    #[serde(default)]
    deliveries: Deliveries,
    #[serde(default)]
    cursors: BTreeMap<String, String>,
    /// The messages already handled in a batch whose cursor is still held,
    /// so a retried batch never answers, refuses or hints twice.
    #[serde(default)]
    handled: BTreeMap<String, Vec<String>>,
}

/// The tick's per-beat result.
#[derive(Debug, Default)]
pub struct ChatTick {
    pub delivered: u64,
    pub closed: u64,
    pub answered: u64,
    pub skip: Option<String>,
    pub detail: Vec<String>,
}

impl ChatTick {
    pub fn acted(&self) -> u64 {
        self.delivered + self.closed + self.answered
    }
}

/// The chat stage of the arm beat, for every sink and its transport.
pub fn tick_chat(
    items: &[AttentionItem],
    lanes: &mut [(&ChatSink, &mut dyn ChatTransport)],
    state_dir: &Path,
    deadline: Instant,
) -> ChatTick {
    let mut tick = ChatTick::default();
    if lanes.is_empty() {
        return tick;
    }
    let path = state_dir.join("chat.json");
    let mut state: ChatState = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let open: Vec<&AttentionItem> = items
        .iter()
        .filter(|i| i.ready && matches!(i.kind.as_str(), "question" | "pin"))
        .filter(|i| !i.id.starts_with("note-"))
        .collect();
    for (sink, transport) in lanes.iter_mut() {
        if Instant::now() >= deadline {
            tick.skip = Some("timeout".into());
            tick.detail
                .push("chat: budget spent, sinks deferred".into());
            break;
        }
        tick_one(
            &open,
            sink,
            &mut **transport,
            &mut state,
            deadline,
            &mut tick,
        );
    }
    save(&path, &state);
    tick
}

fn tick_one(
    open: &[&AttentionItem],
    sink: &ChatSink,
    transport: &mut dyn ChatTransport,
    state: &mut ChatState,
    deadline: Instant,
    tick: &mut ChatTick,
) {
    let is_open = |id: &str| open.iter().any(|i| i.id == id);
    // Close lines first: a delivered item missing from the open set closed.
    let closing: Vec<String> = state
        .deliveries
        .iter()
        .filter(|(_, r)| r.sink == sink.name && r.status == "delivered" && !is_open(&r.item_id))
        .map(|(id, _)| id.clone())
        .collect();
    for did in closing {
        let row = state.deliveries[&did].clone();
        let ext = row.external_id.clone().unwrap_or_default();
        match transport.reply(&ext, &format!("Closed: {}", row.item_id)) {
            Sent::Posted(_) => {
                state.deliveries.get_mut(&did).expect("row").status = "closed".into();
                tick.closed += 1;
            }
            Sent::Dropped(why) => {
                state.deliveries.get_mut(&did).expect("row").status = "dropped".into();
                tick.detail.push(format!(
                    "chat: {} dropped the close for {} ({why})",
                    sink.name, row.item_id
                ));
            }
            Sent::Retry(why) => tick.detail.push(format!(
                "chat: {} retries the close for {} next beat ({why})",
                sink.name, row.item_id
            )),
        }
    }
    // Post each open item once per sink.
    for item in open {
        if Instant::now() >= deadline {
            tick.skip = Some("timeout".into());
            tick.detail
                .push(format!("chat: {} budget spent, posts deferred", sink.name));
            return;
        }
        let did = delivery_id(&sink.name, &item.id, "item.opened");
        if state.deliveries.contains_key(&did) {
            continue;
        }
        let buttons: Vec<(String, String)> = item
            .options
            .iter()
            .map(|o| {
                let text: String = o.text.chars().take(32).collect();
                (format!("{}. {text}", o.n), format!("{}:{}", item.id, o.n))
            })
            .collect();
        let status = match transport.send(&render_item(item), &buttons) {
            Sent::Posted(ext) => {
                tick.delivered += 1;
                Some(("delivered", Some(ext)))
            }
            Sent::Dropped(why) => {
                tick.detail
                    .push(format!("chat: {} dropped {} ({why})", sink.name, item.id));
                Some(("dropped", None))
            }
            Sent::Retry(why) => {
                tick.detail.push(format!(
                    "chat: {} retries {} next beat ({why})",
                    sink.name, item.id
                ));
                None
            }
        };
        if let Some((status, ext)) = status {
            state.deliveries.insert(
                did,
                DeliveryRow {
                    item_id: item.id.clone(),
                    sink: sink.name.clone(),
                    event: "item.opened".into(),
                    status: status.into(),
                    external_id: ext,
                },
            );
        }
    }
    if Instant::now() >= deadline {
        tick.skip = Some("timeout".into());
        return;
    }
    // Inbound: the delivered, still-open questions of this sink.
    let live: Vec<(String, &AttentionItem)> = state
        .deliveries
        .values()
        .filter(|r| r.sink == sink.name && r.status == "delivered")
        .filter_map(|r| {
            let item = open.iter().find(|i| i.id == r.item_id)?;
            Some((r.external_id.clone().unwrap_or_default(), *item))
        })
        .collect();
    let refs: Vec<String> = live.iter().map(|(r, _)| r.clone()).collect();
    let cursor = state.cursors.get(&sink.name).cloned().unwrap_or_default();
    let (inbound, next) = match transport.poll(&cursor, &refs) {
        Ok(batch) => batch,
        Err(why) => {
            tick.detail
                .push(format!("chat: {} poll holds its cursor ({why})", sink.name));
            return;
        }
    };
    for msg in inbound {
        if msg.from_self
            || state
                .handled
                .get(&sink.name)
                .is_some_and(|h| h.contains(&msg.id))
        {
            continue;
        }
        state
            .handled
            .entry(sink.name.clone())
            .or_default()
            .push(msg.id.clone());
        if let Some(tap) = &msg.tap_id {
            transport.ack_tap(tap);
        }
        let allowed = sink.allow.iter().any(|a| *a == msg.sender);
        let (target, text) = map_reply(&msg, &live);
        let Some((ext, item)) = target else {
            if allowed && !msg.text.trim().is_empty() {
                let ids: Vec<&str> = live.iter().map(|(_, i)| i.id.as_str()).collect();
                let open_list = if ids.is_empty() {
                    "none".to_string()
                } else {
                    ids.join(", ")
                };
                transport.send(
                    &format!(
                        "Reply to a question message, or start with its id. Open: {open_list}"
                    ),
                    &[],
                );
            }
            continue;
        };
        if !allowed {
            transport.reply(
                &ext,
                &format!("Not recorded: {} may not answer.", msg.sender),
            );
            continue;
        }
        let answer = match read_answer(item, &text) {
            Ok(a) => a,
            Err(why) => {
                transport.reply(&ext, &format!("Not recorded: {why}"));
                continue;
            }
        };
        let attested = format!("{}:{}:{}", sink.name, msg.sender, msg.id);
        match crate::attention_arm::append_answer_row(
            &item.id,
            &sink.name,
            &answer,
            "sink",
            "attention_chat",
            &attested,
        ) {
            Ok((receipt, _)) => {
                tick.answered += 1;
                transport.reply(&ext, &format!("{receipt} for {}", item.id));
            }
            Err(e) => {
                // The cursor holds, so the next beat reads this reply again;
                // the replies before it stay handled and are skipped.
                if let Some(h) = state.handled.get_mut(&sink.name) {
                    h.pop();
                }
                tick.detail
                    .push(format!("chat: {} answer for {}: {e}", sink.name, item.id));
                return;
            }
        }
    }
    state.cursors.insert(sink.name.clone(), next);
    state.handled.remove(&sink.name);
}

/// The question a reply answers, and the answer text: the thread or reply
/// reference first, else a leading question id (`q-9 2`).
fn map_reply<'a>(
    msg: &Inbound,
    live: &[(String, &'a AttentionItem)],
) -> (Option<(String, &'a AttentionItem)>, String) {
    if let Some(r) = msg.reply_to.as_deref().filter(|r| !r.is_empty()) {
        if let Some((ext, item)) = live.iter().find(|(e, _)| e == r) {
            return (Some((ext.clone(), *item)), msg.text.trim().to_string());
        }
    }
    let text = msg.text.trim();
    let (head, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    if let Some((ext, item)) = live.iter().find(|(_, i)| i.id == head) {
        return (Some((ext.clone(), *item)), rest.trim().to_string());
    }
    (None, text.to_string())
}

/// The answer a reply carries. `done` answers only a pin; an option must
/// be on the question.
fn read_answer(item: &AttentionItem, text: &str) -> Result<FileAnswer, String> {
    if text.trim().is_empty() {
        return Err("the reply is empty".into());
    }
    if text.trim().eq_ignore_ascii_case("done") {
        return if item.kind == "pin" {
            Ok(FileAnswer::Done)
        } else {
            Err("done answers a pin; this question needs an option or words".into())
        };
    }
    let answer = crate::attention_file::parse_answer_text(text);
    if let FileAnswer::Option(n) = answer {
        if item.kind == "pin" {
            return Err(format!("{} has no options; reply done or words", item.id));
        }
        if !item.options.iter().any(|o| o.n == n) {
            return Err(format!(
                "option {n} is not on {}; pick 1-{}",
                item.id,
                item.options.len()
            ));
        }
    }
    Ok(answer)
}

/// The question as one chat message: title, why, numbered options with the
/// recommended one marked, and how to answer.
pub fn render_item(item: &AttentionItem) -> String {
    let mut lines = vec![item.title.clone()];
    if let Some(b) = &item.blocked_because {
        lines.push(format!("Blocked because: {b}"));
    }
    let rec = item.recommendation.as_ref().map(|r| r.option);
    for o in &item.options {
        let mark = if rec == Some(o.n) {
            " (recommended)"
        } else {
            ""
        };
        lines.push(format!("{}. {}{mark}", o.n, o.text));
    }
    lines.push(if item.kind == "pin" {
        format!("Reply done when it is done. Id: {}", item.id)
    } else {
        format!(
            "Reply to this message with a number, a letter or words. Id: {}",
            item.id
        )
    });
    lines.join("\n")
}

fn save(path: &Path, state: &ChatState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string(state) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted HTTP: responses pop in call order; every call is logged.
    #[derive(Default)]
    pub struct ScriptedHttp {
        pub responses: VecDeque<PostResult>,
        pub calls: Vec<(String, String, Option<String>)>,
    }

    impl ScriptedHttp {
        pub fn ok(mut self, body: &str) -> Self {
            self.responses.push_back(PostResult {
                status: Some(200),
                body: body.to_string(),
            });
            self
        }

        pub fn status(mut self, status: u16) -> Self {
            self.responses.push_back(PostResult {
                status: Some(status),
                body: String::new(),
            });
            self
        }
    }

    impl ChatHttp for ScriptedHttp {
        fn call(
            &mut self,
            method: &str,
            url: &str,
            _headers: &[(String, String)],
            body: Option<&str>,
        ) -> PostResult {
            self.calls
                .push((method.into(), url.into(), body.map(str::to_string)));
            self.responses.pop_front().unwrap_or(PostResult {
                status: Some(200),
                body: "{}".into(),
            })
        }
    }

    pub fn question() -> AttentionItem {
        let raw = r#"{"ts":"2026-09-26T00:00:00Z","type":"operator_question","source":"t","data":{"question_id":"q-9","question":"Which?","ask":"pick","session_id":"s1","cwd":"/r/fno","node":"x-aaaa","asker":"w1","options":[{"n":1,"text":"A","next":"x"},{"n":2,"text":"B","next":"y"},{"n":3,"text":"C","next":"z"}],"context":{"blocked_because":"b","options_rationale":"r","recommendation":{"option":1,"why":"narrow"},"unknowns":"u","reversible":"yes","meanwhile":"stops"}}}"#;
        let items = crate::attention::project(raw, &[], "", 0);
        assert!(items[0].ready, "fixture question is ready");
        items[0].clone()
    }

    pub fn pin() -> AttentionItem {
        let mut item = question();
        item.id = "q-pin".into();
        item.kind = "pin".into();
        item.options.clear();
        item
    }

    pub fn sink(kind: ChatKind) -> ChatSink {
        ChatSink {
            name: "chat".into(),
            kind,
            token: "tok".into(),
            allow: vec!["42".into()],
            target: "100".into(),
            url: Some("http://127.0.0.1:1234".into()),
        }
    }

    pub fn future() -> Instant {
        Instant::now() + std::time::Duration::from_secs(60)
    }

    /// The `attention_answer` rows in the declared root's journal.
    pub fn answer_rows() -> Vec<serde_json::Value> {
        let home = crate::paths::AgentsHome::from_env();
        crate::event_store::journal_text(&crate::provider_cap::questions_path(&home), &[])
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["type"] == "attention_answer")
            .map(|v| v["data"].clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    /// A channel in memory: sends get refs m1, m2...; inbound is staged.
    #[derive(Default)]
    struct FakeChat {
        sent: Vec<String>,
        replies: Vec<(String, String)>,
        inbound: Vec<Inbound>,
    }

    impl ChatTransport for FakeChat {
        fn send(&mut self, text: &str, _buttons: &[(String, String)]) -> Sent {
            self.sent.push(text.to_string());
            Sent::Posted(format!("m{}", self.sent.len()))
        }
        fn reply(&mut self, external_ref: &str, text: &str) -> Sent {
            self.replies.push((external_ref.into(), text.into()));
            Sent::Posted(String::new())
        }
        fn poll(
            &mut self,
            _cursor: &str,
            _open: &[String],
        ) -> Result<(Vec<Inbound>, String), String> {
            Ok((std::mem::take(&mut self.inbound), "c1".into()))
        }
    }

    fn reply(from: &str, to: &str, text: &str) -> Inbound {
        Inbound {
            id: format!("in-{text}"),
            sender: from.into(),
            text: text.into(),
            reply_to: Some(to.into()),
            ..Default::default()
        }
    }

    /// AC3, AC4, AC5, AC9, AC16: two beats post once; the lane's own
    /// messages, a stranger, an option off the question and an unmapped
    /// message land nothing (each stranger or bad option gets one refusal
    /// in the thread, the unmapped one a hint naming the open ids); then a
    /// threaded `2` lands exactly one row. A row for another machine loads
    /// nothing and is named.
    #[test]
    fn a_threaded_reply_answers_and_nothing_else_does() {
        let _root = crate::paths::DeclaredRoot::declare("chat_core");
        let dir = tempfile::tempdir().unwrap();
        let items = vec![question()];
        let s = sink(ChatKind::Telegram);
        let mut chat = FakeChat::default();
        tick_chat(&items, &mut [(&s, &mut chat)], dir.path(), future());
        for text in [
            render_item(&items[0]),
            "Recorded: option 1 (chat) for q-9".into(),
        ] {
            let mut own = reply("42", "m1", &text);
            own.from_self = true;
            chat.inbound.push(own);
        }
        chat.inbound.push(reply("7", "m1", "1"));
        chat.inbound.push(reply("42", "m1", "5"));
        chat.inbound.push(Inbound {
            id: "x".into(),
            sender: "42".into(),
            text: "hello".into(),
            ..Default::default()
        });
        let t = tick_chat(&items, &mut [(&s, &mut chat)], dir.path(), future());
        assert_eq!(t.answered, 0);
        assert!(answer_rows().is_empty());
        assert_eq!(chat.replies.len(), 2, "{:?}", chat.replies);
        assert!(chat
            .replies
            .iter()
            .all(|(r, t)| r == "m1" && t.starts_with("Not recorded")));
        assert_eq!(chat.sent.len(), 2, "one post and one hint: {:?}", chat.sent);
        assert!(chat.sent[1].ends_with("Open: q-9"), "{:?}", chat.sent);

        chat.inbound.push(reply("42", "m1", "2"));
        let t = tick_chat(&items, &mut [(&s, &mut chat)], dir.path(), future());
        assert_eq!(chat.sent.len(), 2, "no second post");
        assert_eq!(t.answered, 1);
        let rows = answer_rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["option"], 2);
        assert_eq!(rows[0]["sink"], "chat");
        assert_eq!(rows[0]["mapped_by"], "attention_chat");
        assert!(chat.replies[2].1.starts_with("Recorded: option 2"));

        let other = vec![serde_json::json!({
            "name": "tg", "type": "telegram", "machine": "other-mac",
            "chat_id": 5, "allow": [42], "token_env": "T",
        })];
        let (sinks, refused, skipped) = parse_chat_rows(&other, "this-mac", &|_| Some("t".into()));
        assert!(sinks.is_empty() && refused.is_empty());
        assert_eq!(
            skipped,
            vec!["chat: tg runs on machine other-mac, not this one"]
        );
        let (sinks, _, _) = parse_chat_rows(&other, "other-mac", &|_| Some("t".into()));
        assert_eq!(
            (sinks[0].target.as_str(), sinks[0].allow[0].as_str()),
            ("5", "42")
        );
    }
}

//! The `ntfy` and `webhook` attention sink types: the arm delivers each open
//! item to every configured sink, dedups on `delivery_id` (sha256 of sink,
//! item id and event), retries transient failures next beat with the same id
//! and never resends a delivered or dropped delivery. The outbound post is
//! one `curl` child per call with the URL, the headers and the body off
//! argv: the URL and the bearer header ride a stdin config, the body a
//! 0600 temp file, so `ps` shows no secret. ntfy carries at most three
//! actions per notification (measured at docs.ntfy.sh/publish): the
//! recommended option first as `http` actions, and the one view link rides
//! the top-level `click` key (a literal `view` button only when a slot under
//! the cap is free).

use crate::attention::AttentionItem;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

/// 4xx codes that are a fixable or transient condition, not a permanent
/// client error: the fanout's retry classes (`status_fanout.py`), so one
/// rule serves both.
const TRANSIENT_4XX: [u16; 4] = [401, 403, 408, 429];

/// How long one post may run before its kill.
pub const HTTP_POST_TIMEOUT_S: u64 = 30;

/// The retry clock for a dropped `item.opened` whose item is still open:
/// first resend 5 minutes after the drop, doubling, at most 6 hours apart.
const RETRY_BASE_S: u64 = 300;
const RETRY_CAP_S: u64 = 21_600;
/// Attempts before a dropped row reads `abandoned` and never resends: the
/// old rule dropped for good on try one; 8 tries over a day give an outage
/// room to heal while capping the noise a sink that never returns makes.
pub(crate) const RETRY_ATTEMPTS_CAP: u64 = 8;
/// Resends one beat may put out, so a healed sink does not get every
/// queued question at once.
pub(crate) const RETRIES_PER_BEAT: u64 = 10;
/// Consecutive failures before a sink reads dead to `stuck_work`.
pub const DEAD_SINK_STREAK_MIN: u64 = 3;

/// One configured sink. `token` is resolved from `token_env` at load.
#[derive(Debug, Clone, PartialEq)]
pub struct Sink {
    pub name: String,
    pub kind: SinkKind,
    /// The post URL: the ntfy server root, or the webhook endpoint.
    pub url: String,
    /// ntfy only: the topic, in the JSON body.
    pub topic: Option<String>,
    pub token: String,
    /// The base every item's answer and view URL builds from. Absent means
    /// the sink delivers one-way (webhook adapters poll nothing).
    pub answer_base_url: Option<String>,
    pub full_body: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SinkKind {
    Ntfy,
    Webhook,
}

/// A sink the loader refused: the name, when parseable, and the reason. The
/// arm names it in the tick detail every beat until the config is fixed.
pub struct RefusedSink {
    pub name: String,
    pub reason: String,
}

/// One post attempt's outcome.
pub struct PostResult {
    pub status: Option<u16>,
    pub body: String,
    /// Why a post failed, when the transport can say: the curl exit with its
    /// first stderr line, or the server's `error` field on a 4xx/5xx. A
    /// delivered post carries None.
    pub error: Option<String>,
}

impl PostResult {
    /// `true` when the delivery is done; `false` with a status in the drop
    /// classes means dropped; everything else retries next beat.
    pub fn delivered(&self) -> bool {
        self.status.is_some_and(|s| (200..300).contains(&s))
    }

    pub fn dropped(&self) -> bool {
        match self.status {
            Some(s) if (400..500).contains(&s) => !TRANSIENT_4XX.contains(&s),
            _ => false,
        }
    }
}

/// The post seam: one call, one child. Tests inject it.
pub trait HttpPost {
    fn post(&mut self, url: &str, headers: &[(String, String)], body: &str) -> PostResult;
}

/// The per-delivery durable state, `deliveries.json` beside the settle
/// state. A `delivered` row never resends. A `dropped` row for an item still
/// open resends past its `next_at` until the attempt cap; a missing row (a
/// transient failure) retries next beat with the same `delivery_id`.
pub type Deliveries = BTreeMap<String, DeliveryRow>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeliveryRow {
    pub item_id: String,
    pub sink: String,
    pub event: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    /// Posts so far, 1 counted at the first send. Rows written before this
    /// field read as one try.
    #[serde(default = "default_attempts")]
    pub attempts: u64,
    /// Unix seconds the next resend may go out at; missing reads due, so
    /// today's dropped rows resend on the first healed beat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<u16>,
}

fn default_attempts() -> u64 {
    1
}

/// The sink-health state, `sink-health.json` beside `deliveries.json`: one
/// row per sink, every post updates it, so a dead sink is legible to
/// `stuck_work` without a separate monitor.
pub type SinkHealth = BTreeMap<String, SinkHealthRow>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SinkHealthRow {
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<u64>,
    /// The streak's start. A success ends the episode: the next failure
    /// after `fail_streak` read 0 starts a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_fail_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fail_at: Option<u64>,
    #[serde(default)]
    pub fail_streak: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// The health file path, beside `deliveries.json`.
pub fn sink_health_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("sink-health.json")
}

/// Record one post's outcome for a sink. A 2xx resets `fail_streak`;
/// anything else adds one and keeps the error text. Read-modify-saved
/// atomically per post.
pub fn record_sink_health(state_dir: &Path, sink_name: &str, url: &str, result: &PostResult) {
    let now = now_unix();
    let path = sink_health_path(state_dir);
    let mut map: SinkHealth = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let row = map
        .entry(sink_name.to_string())
        .or_insert_with(|| SinkHealthRow {
            url: url.to_string(),
            ..Default::default()
        });
    if !url.is_empty() {
        row.url = url.to_string();
    }
    if result.delivered() {
        row.last_ok_at = Some(now);
        row.fail_streak = 0;
        row.last_status = result.status;
        row.last_error = None;
    } else {
        if row.fail_streak == 0 {
            row.first_fail_at = Some(now);
        }
        row.last_fail_at = Some(now);
        row.fail_streak += 1;
        row.last_status = result.status;
        row.last_error = Some(result.error.clone().unwrap_or_else(|| match result.status {
            Some(s) => format!("status {s}"),
            None => "no status".to_string(),
        }));
    }
    if let Ok(s) = serde_json::to_string(&map) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Unix seconds, the one clock every attention state write reads.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The next resend time after `attempts` posts: 5 minutes doubling, 6 hours
/// apart at most.
pub(crate) fn retry_next_at(now: u64, attempts: u64) -> u64 {
    let exp = attempts.saturating_sub(1).min(6);
    now + RETRY_BASE_S.saturating_mul(1u64 << exp).min(RETRY_CAP_S)
}

/// `sha256(sink, item id, event)`, hex. Stable across retries.
pub fn delivery_id(sink: &str, item_id: &str, event: &str) -> String {
    let digest = Sha256::digest(format!("{sink}:{item_id}:{event}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Load `[[attention.sinks]]` rows: config read plus env resolution.
pub fn load_sinks(cwd: &Path) -> (Vec<Sink>, Vec<RefusedSink>) {
    let rows = crate::agents_config::config_lookup(cwd, &["attention", "sinks"]);
    let Some(rows) = rows.and_then(|v| v.as_array().cloned()) else {
        return (vec![], vec![]);
    };
    let mut sinks = Vec::new();
    let mut refused = Vec::new();
    for row in rows {
        let str_of = |key: &str| -> Option<String> {
            row.get(key).and_then(|v| v.as_str()).map(str::to_string)
        };
        let name = str_of("name").unwrap_or_default();
        let mut refuse = |reason: String| {
            refused.push(RefusedSink {
                name: name.clone(),
                reason,
            });
        };
        // The chat loader owns its own row types.
        if str_of("type").is_some_and(|t| crate::attention_chat::CHAT_TYPES.contains(&t.as_str())) {
            continue;
        }
        let Some(url) = str_of("url").filter(|u| !u.is_empty()) else {
            refuse("no url".to_string());
            continue;
        };
        let kind = match str_of("type").as_deref() {
            Some("ntfy") => SinkKind::Ntfy,
            Some("webhook") => SinkKind::Webhook,
            _ => {
                refuse("type must be ntfy, webhook, telegram, discord or bluebubbles".to_string());
                continue;
            }
        };
        let topic = str_of("topic").filter(|t| !t.is_empty());
        if kind == SinkKind::Ntfy && topic.is_none() {
            refuse("ntfy needs a topic".to_string());
            continue;
        }
        let Some(token) = str_of("token_env").and_then(|env| resolve_token(&env)) else {
            let env = str_of("token_env").unwrap_or_default();
            refuse(format!("token_env {env} is unset or empty"));
            continue;
        };
        let base = str_of("answer_base_url").filter(|u| !u.is_empty());
        if let Some(b) = &base {
            if let Some(reason) = loopback_reason(b) {
                refuse(format!(
                    "answer_base_url {b} is a loopback address: {reason}"
                ));
                continue;
            }
        }
        let full_body = str_of("body").as_deref() == Some("full");
        sinks.push(Sink {
            name,
            kind,
            url,
            topic,
            token,
            answer_base_url: base,
            full_body,
        });
    }
    (sinks, refused)
}

/// The dotenv file a sink token falls back to. A daemon started by launchd
/// never sees a var the user exported in a shell, so the file is the only
/// place a token reliably reaches it.
pub const SINK_ENV_FILE: &str = "~/.fno/.env";

/// A sink secret: the process env first, then [`SINK_ENV_FILE`].
pub fn resolve_token(env: &str) -> Option<String> {
    crate::provider_key::resolve_key(env, Some(SINK_ENV_FILE))
}

/// Why the URL cannot serve a phone: a loopback host never reaches another
/// device. `Ok` on a non-loopback host or an unparseable one (the post, not
/// the parser, is the gate).
fn loopback_reason(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.trim_start_matches('[');
    let host = host.split(']').next()?;
    // IPv6 loopback carries colons: compare before any port split.
    if host == "::1" || host == "::" {
        return Some("a phone cannot reach this machine's loopback".to_string());
    }
    let host = host.split(':').next()?;
    if host == "localhost" || host.starts_with("127.") || host == "0.0.0.0" {
        Some("a phone cannot reach this machine's loopback".to_string())
    } else {
        None
    }
}

/// The sink tick: deliver new items, close delivered items whose question
/// closed, retry transient failures next beat. Stops at the deadline and
/// counts what it deferred.
pub fn tick_sinks(
    items: &[AttentionItem],
    sinks: &[Sink],
    state_dir: &Path,
    deadline: Instant,
    post: &mut dyn HttpPost,
) -> SinkTick {
    let mut tick = SinkTick::default();
    if sinks.is_empty() {
        return tick;
    }
    let path = state_dir.join("deliveries.json");
    let mut state: Deliveries = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let mut dirty = false;
    let open: std::collections::HashSet<&str> = items
        .iter()
        .filter(|i| i.ready && matches!(i.kind.as_str(), "question" | "pin"))
        .filter(|i| !i.id.starts_with("note-"))
        .map(|i| i.id.as_str())
        .collect();

    // item.closed first, so a close never races an opened this beat: the
    // open set is the truth, and a delivered item missing from it closed.
    let ids: Vec<String> = state
        .iter()
        .filter(|(_, row)| {
            row.status == "delivered"
                && row.event == "item.opened"
                && !open.contains(row.item_id.as_str())
        })
        .map(|(id, _)| id.clone())
        .collect();
    for did in ids {
        if Instant::now() >= deadline {
            tick.skip = Some("timeout".to_string());
            tick.detail
                .push("sinks: budget spent, closes deferred".to_string());
            break;
        }
        let Some(sink) = sinks.iter().find(|s| s.name == state[&did].sink) else {
            // The sink left the config: its close can never post. Drop the
            // row once instead of retrying it every beat forever.
            let row = state.get(&did).cloned();
            if let Some(row) = row {
                state.entry(did).or_insert(row).status = "dropped".to_string();
                dirty = true;
                tick.dropped += 1;
                tick.detail
                    .push("sinks: a close row names a sink no longer configured".to_string());
            }
            continue;
        };
        let row = state[&did].clone();
        let body = delivery_body(
            Some(sink),
            &did,
            "item.closed",
            row.external_id.as_deref(),
            Some(&json!({"id": row.item_id})),
        );
        let result = post.post(
            &sink.url,
            &sink_headers(Some(sink)).unwrap_or_default(),
            &body,
        );
        record_sink_health(state_dir, &row.sink, &sink.url, &result);
        if result.delivered() {
            let entry = state.entry(did).or_insert_with(|| row.clone());
            entry.status = "closed".to_string();
            dirty = true;
            tick.closed += 1;
        } else if result.dropped() {
            let entry = state.entry(did).or_insert_with(|| row.clone());
            entry.status = "dropped".to_string();
            dirty = true;
            tick.dropped += 1;
            tick.detail.push(format!(
                "sinks: {} dropped item.closed for {} ({})",
                row.sink,
                row.item_id,
                result.status.unwrap_or(0)
            ));
        } else {
            tick.detail.push(format!(
                "sinks: {} retries item.closed for {} next beat",
                row.sink, row.item_id
            ));
        }
    }

    // item.opened: one delivery per (sink, item), deduped on the state. A
    // dropped row for an item still open resends past its next_at, so a
    // healed sink gets the question; at the attempt cap the row reads
    // abandoned and never resends.
    let now = now_unix();
    let mut retries = 0u64;
    for item in items {
        if Instant::now() >= deadline {
            tick.skip = Some("timeout".to_string());
            tick.detail
                .push("sinks: budget spent, deliveries deferred".to_string());
            break;
        }
        if !open.contains(item.id.as_str()) {
            continue;
        }
        for sink in sinks {
            let did = delivery_id(&sink.name, &item.id, "item.opened");
            if let Some(row) = state.get_mut(&did) {
                if row.status == "dropped" && row.attempts >= RETRY_ATTEMPTS_CAP {
                    row.status = "abandoned".to_string();
                    row.next_at = None;
                    dirty = true;
                }
                let due = row.next_at.is_none() || now >= row.next_at.unwrap_or(0);
                if row.status == "dropped"
                    && due
                    && row.attempts < RETRY_ATTEMPTS_CAP
                    && retries < RETRIES_PER_BEAT
                {
                    retries += 1;
                    row.attempts += 1;
                    let body = delivery_body_for(item, sink, &did);
                    let result = post.post(
                        &sink.url,
                        &sink_headers(Some(sink)).unwrap_or_default(),
                        &body,
                    );
                    record_sink_health(state_dir, &sink.name, &sink.url, &result);
                    if result.delivered() {
                        row.status = "delivered".to_string();
                        row.external_id = external_id_of(&result.body);
                        row.next_at = None;
                        row.last_status = result.status;
                        dirty = true;
                        tick.delivered += 1;
                    } else {
                        row.last_status = result.status;
                        if result.dropped() {
                            if row.attempts >= RETRY_ATTEMPTS_CAP {
                                row.status = "abandoned".to_string();
                                row.next_at = None;
                            } else {
                                row.next_at = Some(retry_next_at(now, row.attempts));
                            }
                            tick.dropped += 1;
                        }
                        // Transient: the row stays dropped and due; the next
                        // beat retries without spending another attempt.
                        dirty = true;
                    }
                }
                continue;
            }
            let body = delivery_body_for(item, sink, &did);
            let result = post.post(
                &sink.url,
                &sink_headers(Some(sink)).unwrap_or_default(),
                &body,
            );
            record_sink_health(state_dir, &sink.name, &sink.url, &result);
            if result.delivered() {
                state.insert(
                    did,
                    DeliveryRow {
                        item_id: item.id.clone(),
                        sink: sink.name.clone(),
                        event: "item.opened".to_string(),
                        status: "delivered".to_string(),
                        external_id: external_id_of(&result.body),
                        attempts: 1,
                        next_at: None,
                        last_status: result.status,
                    },
                );
                dirty = true;
                tick.delivered += 1;
            } else if result.dropped() {
                state.insert(
                    did,
                    DeliveryRow {
                        item_id: item.id.clone(),
                        sink: sink.name.clone(),
                        event: "item.opened".to_string(),
                        status: "dropped".to_string(),
                        external_id: None,
                        attempts: 1,
                        next_at: Some(retry_next_at(now, 1)),
                        last_status: result.status,
                    },
                );
                dirty = true;
                tick.dropped += 1;
                tick.detail.push(format!(
                    "sinks: {} dropped {} ({})",
                    sink.name,
                    item.id,
                    result.status.unwrap_or(0)
                ));
            }
            // Transient: no record, the same delivery_id retries next beat.
        }
    }
    if dirty {
        save(&path, &state);
    }
    tick
}

/// The tick's per-beat result.
#[derive(Debug, Default)]
pub struct SinkTick {
    pub delivered: u64,
    pub closed: u64,
    pub dropped: u64,
    pub skip: Option<String>,
    pub detail: Vec<String>,
}

impl SinkTick {
    pub fn acted(&self) -> u64 {
        self.delivered + self.closed + self.dropped
    }
}

fn save(path: &Path, state: &Deliveries) {
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

/// The post URL: the webhook endpoint, or the ntfy server root (the topic
/// rides the JSON body).
fn sink_headers(sink: Option<&Sink>) -> Option<Vec<(String, String)>> {
    sink.map(|s| {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", s.token)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    })
}

/// The adapter's own id, from a `{"external_id": "..."}` response.
fn external_id_of(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("external_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The delivery body for the event: a webhook `Delivery`, or the ntfy
/// notification with its action buttons.
fn delivery_body(
    sink: Option<&Sink>,
    did: &str,
    event: &str,
    external_id: Option<&str>,
    item: Option<&Value>,
) -> String {
    let Some(sink) = sink else {
        return json!({"delivery_id": did, "event": event}).to_string();
    };
    match sink.kind {
        SinkKind::Webhook => {
            let mut body = json!({
                "delivery_id": did,
                "event": event,
                "sink": sink.name,
                "item": item.unwrap_or(&json!({})),
            });
            if let Some(ext) = external_id {
                body["external_id"] = json!(ext);
            }
            if let Some(base) = &sink.answer_base_url {
                // The item id is unavailable on a close row without a lookup;
                // the adapter dedupes on delivery_id, and the open delivery's
                // item_id named the answer path already.
                if let Some(item_id) = item
                    .and_then(|i| i.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    body["answer_url"] = json!(format!(
                        "{}/v1/attention/items/{item_id}/answer",
                        base.trim_end_matches('/')
                    ));
                }
            }
            body.to_string()
        }
        SinkKind::Ntfy => {
            // A close carries no buttons; ntfy needs a topic and, without an
            // item, only the text.
            json!({
                "topic": sink.topic.clone().unwrap_or_default(),
                "title": "Question closed",
                "message": format!("{event} for {did}"),
            })
            .to_string()
        }
    }
}

/// The `item.opened` post body for one sink: the ntfy notification with its
/// action buttons, or the webhook `Delivery`.
fn delivery_body_for(item: &AttentionItem, sink: &Sink, did: &str) -> String {
    match sink.kind {
        SinkKind::Ntfy => ntfy_body(item, sink, did),
        SinkKind::Webhook => delivery_body(
            Some(sink),
            did,
            "item.opened",
            None,
            Some(&serde_json::to_value(item).unwrap_or(json!({}))),
        ),
    }
}

/// ntfy's JSON publish takes an integer 1-5; a word is refused with 400.
fn ntfy_priority(item: &AttentionItem) -> u8 {
    if item.priority == "high" {
        4
    } else {
        3
    }
}

/// The ntfy notification for one item: at most three actions, the
/// recommended option first as `http` actions, the view link in `click`
/// (and a literal `view` button when a slot is free). A pin gets one
/// `Done` action.
pub fn ntfy_body(item: &AttentionItem, sink: &Sink, did: &str) -> String {
    // Without an answer_base_url there is no reachable door: the
    // notification stays text-only, never buttons pointing at a loopback.
    let Some(base) = sink.answer_base_url.as_deref() else {
        return json!({
            "topic": sink.topic.clone().unwrap_or_default(),
            "title": item.title,
            "message": if sink.full_body { full_message(item) } else { item.title.clone() },
            "priority": ntfy_priority(item),
        })
        .to_string();
    };
    let view_url = format!(
        "{}/v1/attention/items/{}",
        base.trim_end_matches('/'),
        item.id
    );
    let message = if sink.full_body {
        full_message(item)
    } else {
        item.title.clone()
    };
    let mut body = json!({
        "topic": sink.topic.clone().unwrap_or_default(),
        "title": item.title,
        "message": message,
        "priority": ntfy_priority(item),
        "click": view_url,
        "actions": [],
    });
    let actions = body["actions"].as_array_mut().expect("actions array");
    let answer_url = format!(
        "{}/v1/attention/items/{}/answer",
        base.trim_end_matches('/'),
        item.id
    );
    let http_action = |n: u32, label: String| {
        // ntfy's JSON publish takes an action body as a string; an object
        // draws 400 code 40024 and the post never reaches the phone.
        json!({
            "action": "http",
            "label": label,
            "url": answer_url,
            "method": "POST",
            "headers": {"Authorization": format!("Bearer {}", sink.token)},
            "body": json!({"option": n, "idempotency_key": format!("{did}:{n}")}).to_string(),
            "clear": true,
        })
    };
    if item.kind == "pin" {
        actions.push(json!({
            "action": "http",
            "label": "Done",
            "url": answer_url,
            "method": "POST",
            "headers": {"Authorization": format!("Bearer {}", sink.token)},
            "body": json!({"done": true, "idempotency_key": format!("{did}:done")}).to_string(),
            "clear": true,
        }));
    } else {
        // Recommended first, then the rest ascending.
        let mut order: Vec<u32> = item.options.iter().map(|o| o.n).collect();
        if let Some(rec) = &item.recommendation {
            order.sort_by_key(|n| (*n == rec.option) == false);
        }
        for n in order.iter().take(3) {
            let label = item
                .options
                .iter()
                .find(|o| o.n == *n)
                .map(|o| {
                    let text: String = o.text.chars().take(32).collect();
                    format!("{}. {}", o.n, text)
                })
                .unwrap_or_else(|| format!("option {n}"));
            actions.push(http_action(*n, label));
        }
    }
    if actions.len() < 3 {
        actions.push(json!({
            "action": "view",
            "label": "Open",
            "url": view_url,
        }));
    }
    body.to_string()
}

/// The full-body message: title, blocked-because, recommendation, downside.
fn full_message(item: &AttentionItem) -> String {
    let mut parts = vec![item.title.clone()];
    if let Some(b) = &item.blocked_because {
        parts.push(format!("Blocked because: {b}"));
    }
    if let Some(r) = &item.recommendation {
        parts.push(format!("Recommended {}: {}", r.option, r.why));
        if let Some(d) = &r.downside {
            parts.push(format!("Downside: {d}"));
        }
    }
    parts.join(" | ")
}

/// Build the curl `--config` payload for one request. Every quoted value is
/// escaped for the config grammar: a backslash or double quote inside a URL,
/// a header or a temp path would otherwise close the quoted string early.
/// No body path means no `data-binary`, so a GET carries nothing.
fn curl_config(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body_path: Option<&Path>,
    resp_path: &Path,
) -> String {
    fn quoted(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }
    let mut config = format!(
        "url = \"{}\"\nrequest = \"{}\"\nmax-time = \"{HTTP_POST_TIMEOUT_S}\"\nsilent\nshow-error\n",
        quoted(url),
        quoted(method)
    );
    for (name, value) in headers {
        config.push_str(&format!(
            "header = \"{}: {}\"\n",
            quoted(name),
            quoted(value)
        ));
    }
    if let Some(body_path) = body_path {
        config.push_str(&format!(
            "data-binary = \"@{}\"\n",
            quoted(&body_path.display().to_string())
        ));
    }
    config.push_str(&format!(
        "output = \"{}\"\nwrite-out = \"%{{http_code}}\"\n",
        quoted(&resp_path.display().to_string())
    ));
    config
}

/// Write a temp file only its owner can read: a body may carry a chat
/// message or a token.
fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(body.as_bytes())
}

/// One `curl` child, config on stdin, the body (when present) from a 0600
/// temp file. Exit non-zero or a timeout reads as no status (transient).
/// The sink posts and the chat lane's reads share this one seam.
pub fn curl_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> PostResult {
    let stem = std::env::temp_dir().join(format!(
        "fno-http-post-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let body_path = body.map(|b| {
        let _ = write_private(&stem, b);
        stem.clone()
    });
    let resp_path = stem.with_extension("resp");
    let config = curl_config(method, url, headers, body_path.as_deref(), &resp_path);
    let output = std::process::Command::new("curl")
        .arg("--config")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            // The piped stdin always takes; a None still closes cleanly
            // instead of panicking inside the daemon beat.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(config.as_bytes());
            }
            child.wait_with_output()
        });
    if let Some(p) = &body_path {
        let _ = std::fs::remove_file(p);
    }
    let resp = std::fs::read_to_string(&resp_path).unwrap_or_default();
    let _ = std::fs::remove_file(&resp_path);
    let failure = match &output {
        Ok(out) if out.status.success() => None,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let line = stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(|l| cap_chars(l.trim(), 120));
            Some(match line {
                Some(l) => format!("curl exited {}: {}", out.status.code().unwrap_or(-1), l),
                None => format!("curl exited {}", out.status.code().unwrap_or(-1)),
            })
        }
        Err(e) => Some(format!("curl failed to run: {e}")),
    };
    if let Some(err) = failure {
        // A resolve failure or timeout: no status, so the delivery reads
        // transient and the error text lands in sink health.
        return PostResult {
            status: None,
            body: String::new(),
            error: Some(err),
        };
    }
    let out = output.expect("output read as success");
    let status: Option<u16> = String::from_utf8_lossy(&out.stdout).trim().parse().ok();
    let error = status
        .filter(|s| !(200..300).contains(s))
        .map(|s| response_error(&resp, s));
    PostResult {
        status,
        body: resp,
        error,
    }
}

/// The error text a non-2xx response carries: ntfy's `error` field, else the
/// first 120 chars of the body.
fn response_error(body: &str, status: u16) -> String {
    let trimmed = body.trim();
    let json_err = serde_json::from_str::<Value>(trimmed)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string));
    json_err.unwrap_or_else(|| {
        if trimmed.is_empty() {
            format!("status {status}")
        } else {
            cap_chars(trimmed, 120)
        }
    })
}

fn cap_chars(text: &str, cap: usize) -> String {
    text.chars().take(cap).collect()
}

/// The real post: [`curl_request`] with a POST.
pub struct CurlPost;

impl HttpPost for CurlPost {
    fn post(&mut self, url: &str, headers: &[(String, String)], body: &str) -> PostResult {
        curl_request("POST", url, headers, Some(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct ScriptedPost {
        responses: RefCell<Vec<PostResult>>,
        bodies: RefCell<Vec<String>>,
    }

    impl HttpPost for ScriptedPost {
        fn post(&mut self, _url: &str, _headers: &[(String, String)], body: &str) -> PostResult {
            self.bodies.borrow_mut().push(body.to_string());
            self.responses.borrow_mut().pop().unwrap_or(PostResult {
                status: Some(200),
                body: String::new(),
                error: None,
            })
        }
    }

    fn webhook_sink() -> Sink {
        Sink {
            name: "notion".to_string(),
            kind: SinkKind::Webhook,
            url: "https://hooks.example/fno".to_string(),
            topic: None,
            token: "tok-2".to_string(),
            answer_base_url: Some("https://reach.example".to_string()),
            full_body: false,
        }
    }

    fn ntfy_sink() -> Sink {
        Sink {
            name: "phone".to_string(),
            kind: SinkKind::Ntfy,
            url: "https://ntfy.example".to_string(),
            topic: Some("fno-questions".to_string()),
            token: "tok-1".to_string(),
            answer_base_url: Some("https://reach.example".to_string()),
            full_body: true,
        }
    }

    fn ready_items() -> Vec<AttentionItem> {
        let raw = r#"{"ts":"2026-09-26T00:00:00Z","type":"operator_question","source":"t","data":{"question_id":"q-9","question":"Which?","ask":"pick","session_id":"s1","cwd":"/r/fno","node":"x-aaaa","asker":"w1","options":[{"n":1,"text":"A","next":"x"},{"n":2,"text":"B","next":"y"}],"context":{"blocked_because":"b","options_rationale":"r","recommendation":{"option":1,"why":"narrow"},"unknowns":"u","reversible":"yes","meanwhile":"stops"}}}"#;
        crate::attention::project(raw, &[], "", 0)
    }

    fn future() -> Instant {
        Instant::now() + std::time::Duration::from_secs(60)
    }

    // The wire contract with ntfy's JSON publish: every action body is a
    // string and priority an integer. Two 400s came from breaking this
    // shape (priority as a word, then the action body as an object).
    #[test]
    fn ntfy_actions_carry_string_bodies_and_an_integer_priority() {
        let sink = ntfy_sink();
        let mut items = ready_items();
        let mut pin = crate::attention::AttentionItem::default();
        pin.id = "p-1".to_string();
        pin.kind = "pin".to_string();
        pin.priority = "high".to_string();
        items.push(pin);
        for item in &items {
            let did = delivery_id(&sink.name, &item.id, "item.opened");
            let body: Value = serde_json::from_str(&ntfy_body(item, &sink, &did)).unwrap();
            assert!(
                body["priority"].is_i64() || body["priority"].is_u64(),
                "priority must be an integer, got {}",
                body["priority"]
            );
            let actions = body["actions"].as_array().expect("actions array");
            assert!(!actions.is_empty());
            for action in actions {
                let text = action["body"].as_str().unwrap_or_else(|| {
                    panic!("action body must be a JSON string, got {}", action["body"])
                });
                let inner: Value = serde_json::from_str(text).expect("body content parses as JSON");
                assert!(
                    inner.get("idempotency_key").is_some(),
                    "body content names the idempotency key"
                );
            }
        }
        let pin = items.last().unwrap();
        let did = delivery_id(&sink.name, &pin.id, "item.opened");
        let body: Value = serde_json::from_str(&ntfy_body(pin, &sink, &did)).unwrap();
        let done = body["actions"][0]["body"]
            .as_str()
            .expect("Done body string");
        assert!(done.contains("\"done\":true"), "{}", done);
    }

    // A permanent 4xx no longer ends the story: the dropped row for an item
    // still open resends past its next_at and reads delivered on a 2xx; at
    // the attempt cap it reads abandoned and never posts again.
    #[test]
    fn a_dropped_opened_resends_past_next_at_then_abandons() {
        let dir = tempfile::tempdir().unwrap();
        let items = ready_items();
        let sinks = vec![webhook_sink()];
        let path = dir.path().join("deliveries.json");
        let did = delivery_id("notion", &items[0].id, "item.opened");
        // Beat 1: the post 404s; the row reads dropped, one try, 5 min out.
        let mut post = ScriptedPost {
            responses: RefCell::new(vec![PostResult {
                status: Some(404),
                body: String::new(),
                error: Some("status 404".to_string()),
            }]),
            bodies: RefCell::new(vec![]),
        };
        let t = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
        assert_eq!(t.dropped, 1);
        let state: Deliveries =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let row = state[&did].clone();
        assert_eq!(row.status, "dropped");
        assert_eq!(row.attempts, 1);
        assert!(row.next_at.is_some());
        // Beat 2 before next_at: nothing re-posts.
        let mut post = ScriptedPost {
            responses: RefCell::new(vec![]),
            bodies: RefCell::new(vec![]),
        };
        let t2 = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
        assert_eq!(t2.acted(), 0);
        assert!(post.bodies.borrow().is_empty());
        // The later beat: next_at in the past, the retry 200s; one resend
        // under the same delivery_id reads delivered.
        let mut state: Deliveries =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        state.get_mut(&did).unwrap().next_at = Some(0);
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
        let mut post = ScriptedPost {
            responses: RefCell::new(vec![PostResult {
                status: Some(200),
                body: String::new(),
                error: None,
            }]),
            bodies: RefCell::new(vec![]),
        };
        let t3 = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
        assert_eq!(t3.delivered, 1);
        assert_eq!(post.bodies.borrow().len(), 1);
        let state: Deliveries =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(state[&did].status, "delivered");
        assert_eq!(state.len(), 1);
        // The cap: a dropped row at the attempt limit spends its 8th try,
        // reads abandoned, and never posts again.
        let mut state: Deliveries = BTreeMap::new();
        state.insert(
            did.clone(),
            DeliveryRow {
                item_id: items[0].id.clone(),
                sink: "notion".to_string(),
                event: "item.opened".to_string(),
                status: "dropped".to_string(),
                external_id: None,
                attempts: 7,
                next_at: Some(0),
                last_status: Some(404),
            },
        );
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
        let mut post = ScriptedPost {
            responses: RefCell::new(vec![PostResult {
                status: Some(404),
                body: String::new(),
                error: Some("status 404".to_string()),
            }]),
            bodies: RefCell::new(vec![]),
        };
        let t4 = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
        assert_eq!(post.bodies.borrow().len(), 1, "the 8th try still posts");
        assert_eq!(t4.dropped, 1);
        let state: Deliveries =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(state[&did].status, "abandoned");
        assert_eq!(state[&did].attempts, 8);
        let mut post = ScriptedPost {
            responses: RefCell::new(vec![]),
            bodies: RefCell::new(vec![]),
        };
        let t5 = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
        assert_eq!(t5.acted(), 0);
        assert!(post.bodies.borrow().is_empty(), "abandoned never resends");
    }

    // Every post updates sink health: three resolve failures stack the
    // streak and keep the error text; one success resets the streak and
    // keeps the episode's start behind it.
    #[test]
    fn failed_posts_stack_fail_streak_and_a_success_resets_it() {
        let dir = tempfile::tempdir().unwrap();
        let items = ready_items();
        let sinks = vec![ntfy_sink()];
        let path = dir.path().join("deliveries.json");
        let did = delivery_id("phone", &items[0].id, "item.opened");
        let resolve_fail = || PostResult {
            status: None,
            body: String::new(),
            error: Some("curl exited 6: Could not resolve host: ntfy.example".to_string()),
        };
        let due_now = |state_path: &Path| {
            let mut state: Deliveries =
                serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
            state.get_mut(&did).unwrap().next_at = Some(0);
            std::fs::write(state_path, serde_json::to_string(&state).unwrap()).unwrap();
        };
        let health = |state_dir: &Path| -> SinkHealth {
            serde_json::from_str(&std::fs::read_to_string(sink_health_path(state_dir)).unwrap())
                .unwrap()
        };
        for beat in 0..3 {
            if beat > 0 {
                due_now(&path);
            }
            let mut post = ScriptedPost {
                responses: RefCell::new(vec![resolve_fail()]),
                bodies: RefCell::new(vec![]),
            };
            let t = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
            assert_eq!(t.dropped, 1, "beat {beat} drops");
            assert_eq!(post.bodies.borrow().len(), 1, "beat {beat} posts once");
        }
        let row = health(dir.path())["phone"].clone();
        assert_eq!(row.fail_streak, 3);
        assert_eq!(
            row.last_error.as_deref(),
            Some("curl exited 6: Could not resolve host: ntfy.example")
        );
        let first_fail = row.first_fail_at.expect("episode start recorded");
        // The healed beat: the retry 200s, the streak reads 0, and the
        // episode's start stays behind the success.
        due_now(&path);
        let mut post = ScriptedPost {
            responses: RefCell::new(vec![PostResult {
                status: Some(200),
                body: String::new(),
                error: None,
            }]),
            bodies: RefCell::new(vec![]),
        };
        let t = tick_sinks(&items, &sinks, dir.path(), future(), &mut post);
        assert_eq!(t.delivered, 1);
        let row = health(dir.path())["phone"].clone();
        assert_eq!(row.fail_streak, 0);
        assert!(row.last_ok_at.expect("success time") >= first_fail);
        assert_eq!(row.last_error, None);
    }

    #[test]
    fn loopback_answer_urls_are_refused_at_load() {
        assert!(loopback_reason("http://127.0.0.1:8724").is_some());
        assert!(loopback_reason("http://localhost:8724").is_some());
        assert!(loopback_reason("http://[::1]:8724/v1").is_some());
        assert!(loopback_reason("https://reach.tailnet-name.ts.net").is_none());
        assert!(loopback_reason("https://ntfy.sh").is_none());
    }
}

//! Discord bot transport over REST. Each question posts to the channel and
//! starts a thread from that message; a thread started from a message takes
//! the message's id, so the message id is the thread id and the external
//! ref. The poll reads only the threads of questions still open, from a
//! per-thread cursor. Reading reply text needs the Message Content intent
//! turned on for the bot (without it Discord returns empty content).

use super::{ChatHttp, ChatSink, ChatTransport, Inbound, Sent};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const API: &str = "https://discord.com/api/v10";

pub struct Discord<'a> {
    channel: String,
    headers: Vec<(String, String)>,
    http: &'a mut dyn ChatHttp,
}

impl<'a> Discord<'a> {
    pub fn new(sink: &ChatSink, http: &'a mut dyn ChatHttp) -> Self {
        Discord {
            channel: sink.target.clone(),
            headers: vec![
                ("Authorization".into(), format!("Bot {}", sink.token)),
                ("Content-Type".into(), "application/json".into()),
            ],
            http,
        }
    }

    fn post(&mut self, path: &str, body: &Value) -> crate::attention_http::PostResult {
        self.http.call(
            "POST",
            &format!("{API}{path}"),
            &self.headers,
            Some(&body.to_string()),
        )
    }
}

fn id_of(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v["id"].as_str().map(str::to_string)
}

/// Discord refuses `content` over 2,000 characters.
const MAX_CONTENT: usize = 2000;

/// A message body: the text bounded to Discord's limit with the last line
/// (how to answer, and the id) kept whole, and mention parsing off, so a
/// question that quotes `@everyone` never pings the channel.
fn message(text: &str) -> Value {
    let content = if text.chars().count() <= MAX_CONTENT {
        text.to_string()
    } else {
        let (head, tail) = text.rsplit_once('\n').unwrap_or(("", text));
        let room = MAX_CONTENT.saturating_sub(tail.chars().count() + 2);
        let cut: String = head.chars().take(room).collect();
        let joined = format!("{cut}…\n{tail}");
        joined.chars().take(MAX_CONTENT).collect()
    };
    json!({"content": content, "allowed_mentions": {"parse": []}})
}

/// Snowflakes order by their numeric value.
fn snowflake(id: &str) -> u64 {
    id.parse().unwrap_or(0)
}

impl ChatTransport for Discord<'_> {
    fn send(&mut self, text: &str, _buttons: &[(String, String)]) -> Sent {
        let path = format!("/channels/{}/messages", self.channel);
        let sent = Sent::from_result(&self.post(&path, &message(text)), id_of);
        if let Sent::Posted(id) = &sent {
            if !id.is_empty() {
                let name: String = text
                    .lines()
                    .next()
                    .unwrap_or("question")
                    .chars()
                    .take(90)
                    .collect();
                // The message exists now: a failed thread start must not
                // re-post it, so its outcome only shows on the next reply.
                let _ = self.post(
                    &format!("/channels/{}/messages/{id}/threads", self.channel),
                    &json!({"name": name, "auto_archive_duration": 10080}),
                );
            }
        }
        sent
    }

    fn reply(&mut self, external_ref: &str, text: &str) -> Sent {
        Sent::from_result(
            &self.post(
                &format!("/channels/{external_ref}/messages"),
                &message(text),
            ),
            id_of,
        )
    }

    fn poll(&mut self, cursor: &str, open: &[String]) -> Result<(Vec<Inbound>, String), String> {
        let mut seen: BTreeMap<String, String> = serde_json::from_str(cursor).unwrap_or_default();
        let mut out = Vec::new();
        for thread in open.iter().filter(|t| !t.is_empty()) {
            let after = seen.get(thread).cloned().unwrap_or_else(|| thread.clone());
            let r = self.http.call(
                "GET",
                &format!("{API}/channels/{thread}/messages?after={after}&limit=50"),
                &self.headers,
                None,
            );
            if r.status == Some(404) {
                // The thread start failed after the message posted: this
                // question has no thread to read, and it must not hold the
                // other threads' replies. It stays answerable on the other
                // lanes (page, mux, endpoint).
                continue;
            }
            if !r.delivered() {
                return Err(format!("thread {thread} status {}", r.status.unwrap_or(0)));
            }
            let mut msgs: Vec<Value> = serde_json::from_str(&r.body).unwrap_or_default();
            msgs.sort_by_key(|m| snowflake(m["id"].as_str().unwrap_or_default()));
            for m in msgs {
                let id = m["id"].as_str().unwrap_or_default().to_string();
                if snowflake(&id) > snowflake(&after) {
                    seen.insert(thread.clone(), id.clone());
                }
                out.push(Inbound {
                    id,
                    sender: m["author"]["id"].as_str().unwrap_or_default().to_string(),
                    text: m["content"].as_str().unwrap_or_default().to_string(),
                    reply_to: Some(thread.clone()),
                    from_self: m["author"]["bot"].as_bool().unwrap_or(false),
                    tap_id: None,
                });
            }
        }
        // Closed threads leave the cursor so it stays bounded.
        seen.retain(|t, _| open.contains(t));
        Ok((out, serde_json::to_string(&seen).unwrap_or_default()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::*;

    /// AC7 then AC6: a 429 on the post records nothing and the next beat
    /// posts once and starts the thread; a 429 on the poll holds the
    /// cursor with no re-post; a thread reply `b` from an allowed user lands
    /// option 2 (the bot's own receipt is skipped); the close line posts in
    /// that thread.
    #[test]
    fn discord_retries_a_429_answers_a_thread_reply_and_closes_there() {
        let _root = crate::paths::DeclaredRoot::declare("chat_dc");
        let dir = tempfile::tempdir().unwrap();
        let s = sink(ChatKind::Discord);
        let items = vec![question()];
        let mut http = ScriptedHttp::default().status(429);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&items, &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.delivered, 0);

        let mut http = ScriptedHttp::default()
            .ok(r#"{"id":"900"}"#)
            .ok(r#"{"id":"900"}"#)
            .ok("[]");
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&items, &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.delivered, 1);
        assert!(http.calls[1]
            .1
            .ends_with("/channels/100/messages/900/threads"));

        let state = std::fs::read_to_string(dir.path().join("chat.json")).unwrap();
        let mut http = ScriptedHttp::default().status(429);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&items, &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(http.calls.len(), 1, "no re-post: {:?}", http.calls);
        assert!(
            tick.detail[0].contains("poll holds its cursor"),
            "{:?}",
            tick.detail
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("chat.json")).unwrap(),
            state
        );

        let mut http = ScriptedHttp::default().ok(
            r#"[{"id":"902","author":{"id":"77","bot":true},"content":"Recorded"},{"id":"901","author":{"id":"42"},"content":"b"}]"#,
        );
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&items, &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.answered, 1, "{:?}", tick.detail);
        assert_eq!(answer_rows()[0]["option"], 2);
        assert!(http.calls[0]
            .1
            .ends_with("/channels/900/messages?after=900&limit=50"));

        let mut http = ScriptedHttp::default().ok(r#"{"id":"903"}"#);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&[], &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.closed, 1);
        assert!(http.calls[0].1.ends_with("/channels/900/messages"));
        let body: serde_json::Value =
            serde_json::from_str(http.calls[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(body["content"], "Closed: q-9");
        assert_eq!(body["allowed_mentions"]["parse"], serde_json::json!([]));

        // A question past Discord's 2,000-character limit is cut, and the
        // answer line survives whole.
        let long = format!("{}\nReply to this message. Id: q-9", "x".repeat(3000));
        let content = super::message(&long)["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(content.chars().count() <= 2000);
        assert!(content.ends_with("\nReply to this message. Id: q-9"));
    }
}

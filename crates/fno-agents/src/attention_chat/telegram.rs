//! Telegram Bot API transport. The token rides the URL path, which curl
//! reads from its stdin config, never argv. `getUpdates` allows one poller
//! per bot (a second gets 409, read as a transient retry) and never returns
//! the bot's own messages. A button tap arrives as a `callback_query` whose
//! data is `<item>:<n>`; it maps like a reply to the tapped message.

use super::{ChatHttp, ChatSink, ChatTransport, Inbound, Sent};
use serde_json::{json, Value};

pub struct Telegram<'a> {
    base: String,
    chat_id: String,
    http: &'a mut dyn ChatHttp,
}

impl<'a> Telegram<'a> {
    pub fn new(sink: &ChatSink, http: &'a mut dyn ChatHttp) -> Self {
        Telegram {
            base: format!("https://api.telegram.org/bot{}", sink.token),
            chat_id: sink.target.clone(),
            http,
        }
    }

    fn call(&mut self, method: &str, body: &Value) -> crate::attention_http::PostResult {
        let headers = [("Content-Type".to_string(), "application/json".to_string())];
        self.http.call(
            "POST",
            &format!("{}/{method}", self.base),
            &headers,
            Some(&body.to_string()),
        )
    }
}

fn message_id(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v["result"]["message_id"].as_i64().map(|n| n.to_string())
}

impl ChatTransport for Telegram<'_> {
    fn send(&mut self, text: &str, buttons: &[(String, String)]) -> Sent {
        let mut body = json!({"chat_id": self.chat_id, "text": text});
        if !buttons.is_empty() {
            let rows: Vec<Value> = buttons
                .iter()
                .map(|(label, data)| json!([{"text": label, "callback_data": data}]))
                .collect();
            body["reply_markup"] = json!({ "inline_keyboard": rows });
        }
        Sent::from_result(&self.call("sendMessage", &body), message_id)
    }

    fn reply(&mut self, external_ref: &str, text: &str) -> Sent {
        let mut body = json!({"chat_id": self.chat_id, "text": text});
        if let Ok(id) = external_ref.parse::<i64>() {
            body["reply_parameters"] =
                json!({"message_id": id, "allow_sending_without_reply": true});
        }
        Sent::from_result(&self.call("sendMessage", &body), message_id)
    }

    fn poll(&mut self, cursor: &str, _open: &[String]) -> Result<(Vec<Inbound>, String), String> {
        // No cursor yet: skip the backlog (a setup test message, old chat)
        // and start after the newest update.
        let first = cursor.is_empty();
        let offset: i64 = if first {
            -1
        } else {
            cursor.parse().unwrap_or(0)
        };
        let r = self.call(
            "getUpdates",
            &json!({
                "offset": offset,
                "timeout": 0,
                "allowed_updates": ["message", "callback_query"],
            }),
        );
        if !r.delivered() {
            return Err(format!("getUpdates status {}", r.status.unwrap_or(0)));
        }
        let v: Value = serde_json::from_str(&r.body).map_err(|e| e.to_string())?;
        let updates = v["result"].as_array().cloned().unwrap_or_default();
        let mut next = offset.max(0);
        let mut out = Vec::new();
        for u in &updates {
            if let Some(id) = u["update_id"].as_i64() {
                next = next.max(id + 1);
            }
            if first {
                continue;
            }
            if let Some(m) = u.get("message") {
                if m["chat"]["id"].to_string().trim_matches('"') != self.chat_id {
                    continue;
                }
                out.push(Inbound {
                    id: m["message_id"].to_string(),
                    sender: m["from"]["id"].to_string(),
                    text: m["text"].as_str().unwrap_or_default().to_string(),
                    reply_to: m["reply_to_message"]["message_id"]
                        .as_i64()
                        .map(|n| n.to_string()),
                    from_self: m["from"]["is_bot"].as_bool().unwrap_or(false),
                    tap_id: None,
                });
            } else if let Some(q) = u.get("callback_query") {
                // A message id is unique only inside its chat: a tap in
                // another chat must never map onto this chat's question.
                if q["message"]["chat"]["id"].to_string().trim_matches('"') != self.chat_id {
                    continue;
                }
                let data = q["data"].as_str().unwrap_or_default();
                let n = data.rsplit(':').next().unwrap_or_default();
                out.push(Inbound {
                    id: q["id"].as_str().unwrap_or_default().to_string(),
                    sender: q["from"]["id"].to_string(),
                    text: n.to_string(),
                    reply_to: q["message"]["message_id"].as_i64().map(|n| n.to_string()),
                    from_self: false,
                    tap_id: q["id"].as_str().map(str::to_string),
                });
            }
        }
        Ok((out, next.to_string()))
    }

    fn ack_tap(&mut self, tap_id: &str) {
        let _ = self.call("answerCallbackQuery", &json!({"callback_query_id": tap_id}));
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::*;

    /// Post with one button per option, skip the backlog on the first poll,
    /// map a tap on option 2 to the question, then post the close line as
    /// a reply to the question message.
    #[test]
    fn telegram_posts_maps_a_tap_and_closes_in_the_thread() {
        let _root = crate::paths::DeclaredRoot::declare("chat_tg");
        let dir = tempfile::tempdir().unwrap();
        let s = sink(ChatKind::Telegram);
        let items = vec![question()];
        let mut http = ScriptedHttp::default()
            .ok(r#"{"ok":true,"result":{"message_id":11}}"#)
            .ok(r#"{"ok":true,"result":[{"update_id":5}]}"#);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&items, &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.delivered, 1);
        let (_, url, body) = &http.calls[0];
        assert_eq!(url, "https://api.telegram.org/bottok/sendMessage");
        let body: serde_json::Value = serde_json::from_str(body.as_deref().unwrap()).unwrap();
        assert_eq!(body["chat_id"], "100");
        assert_eq!(
            body["reply_markup"]["inline_keyboard"][1][0]["callback_data"],
            "q-9:2"
        );
        assert!(body["text"]
            .as_str()
            .unwrap()
            .contains("1. A (recommended)"));

        let mut http = ScriptedHttp::default().ok(
            r#"{"ok":true,"result":[{"update_id":6,"callback_query":{"id":"cb1","from":{"id":42},"data":"q-9:2","message":{"message_id":11,"chat":{"id":100}}}}]}"#,
        );
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&items, &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.answered, 1, "{:?}", tick.detail);
        assert_eq!(answer_rows()[0]["option"], 2);
        let poll: serde_json::Value =
            serde_json::from_str(http.calls[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(poll["offset"], 6, "the first beat skipped the backlog");
        assert!(http.calls[1].1.ends_with("/answerCallbackQuery"));

        let mut http = ScriptedHttp::default().ok(r#"{"ok":true,"result":{"message_id":12}}"#);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&[], &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.closed, 1);
        let body: serde_json::Value =
            serde_json::from_str(http.calls[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(body["text"], "Closed: q-9");
        assert_eq!(body["reply_parameters"]["message_id"], 11);
    }
}

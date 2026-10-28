//! iMessage through a BlueBubbles server on the owning Mac. The server
//! password rides the URL query, and curl reads the URL from its stdin
//! config, never argv. Without the Private API a send cannot thread, so a
//! reply posts plain into the chat and carries the question id; an inline
//! reply the user makes still carries `threadOriginatorGuid`, which maps it.
//! A loopback `url` is legal here: the server runs on this machine.

use super::{ChatHttp, ChatSink, ChatTransport, Inbound, Sent};
use serde_json::{json, Value};

pub struct BlueBubbles<'a> {
    base: String,
    password: String,
    chat_guid: String,
    http: &'a mut dyn ChatHttp,
}

impl<'a> BlueBubbles<'a> {
    pub fn new(sink: &ChatSink, http: &'a mut dyn ChatHttp) -> Self {
        BlueBubbles {
            base: sink
                .url
                .clone()
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_string(),
            password: sink.token.clone(),
            chat_guid: sink.target.clone(),
            http,
        }
    }

    fn post(&mut self, path: &str, body: &Value) -> crate::attention_http::PostResult {
        let url = format!(
            "{}{path}?password={}",
            self.base,
            percent_encode(&self.password)
        );
        let headers = [("Content-Type".to_string(), "application/json".to_string())];
        self.http
            .call("POST", &url, &headers, Some(&body.to_string()))
    }

    fn send_text(&mut self, text: &str) -> Sent {
        let temp = format!(
            "fno-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let body = json!({
            "chatGuid": self.chat_guid,
            "tempGuid": temp,
            "message": text,
            "method": "apple-script",
        });
        Sent::from_result(&self.post("/api/v1/message/text", &body), |b| {
            let v: Value = serde_json::from_str(b).ok()?;
            v["data"]["guid"].as_str().map(str::to_string)
        })
    }
}

/// RFC 3986 unreserved characters pass; everything else is `%XX`.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ChatTransport for BlueBubbles<'_> {
    fn send(&mut self, text: &str, _buttons: &[(String, String)]) -> Sent {
        self.send_text(text)
    }

    fn reply(&mut self, _external_ref: &str, text: &str) -> Sent {
        self.send_text(text)
    }

    fn poll(&mut self, cursor: &str, _open: &[String]) -> Result<(Vec<Inbound>, String), String> {
        // No cursor yet: start now, never replay the chat's history.
        let Ok(after) = cursor.parse::<u64>() else {
            return Ok((vec![], now_ms().to_string()));
        };
        let r = self.post(
            "/api/v1/message/query",
            &json!({
                "chatGuid": self.chat_guid,
                "after": after,
                "sort": "ASC",
                "limit": 100,
                "with": ["handle"],
            }),
        );
        if !r.delivered() {
            return Err(format!("message/query status {}", r.status.unwrap_or(0)));
        }
        let v: Value = serde_json::from_str(&r.body).map_err(|e| e.to_string())?;
        let mut next = after;
        let mut out = Vec::new();
        for m in v["data"].as_array().cloned().unwrap_or_default() {
            let at = m["dateCreated"].as_u64().unwrap_or(0);
            if at <= after {
                continue;
            }
            next = next.max(at);
            out.push(Inbound {
                id: m["guid"].as_str().unwrap_or_default().to_string(),
                sender: m["handle"]["address"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                text: m["text"].as_str().unwrap_or_default().to_string(),
                reply_to: m["threadOriginatorGuid"].as_str().map(str::to_string),
                from_self: m["isFromMe"].as_bool().unwrap_or(false),
                tap_id: None,
            });
        }
        Ok((out, next.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::*;

    /// Post to the chat with the password encoded in the query (the first
    /// poll only starts the cursor); AC8: an inline `Done` from the allowed
    /// handle closes the pin, and the lane's own message is skipped; the
    /// close line names the question.
    #[test]
    fn bluebubbles_posts_takes_an_inline_done_and_closes() {
        let _root = crate::paths::DeclaredRoot::declare("chat_bb");
        let dir = tempfile::tempdir().unwrap();
        let mut s = sink(ChatKind::BlueBubbles);
        s.allow = vec!["+15550001111".into()];
        s.token = "p w&d".into();
        let mut http = ScriptedHttp::default().ok(r#"{"status":200,"data":{"guid":"g-1"}}"#);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&[pin()], &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.delivered, 1);
        assert_eq!(http.calls.len(), 1, "the first poll only starts the cursor");
        assert_eq!(
            http.calls[0].1,
            "http://127.0.0.1:1234/api/v1/message/text?password=p%20w%26d"
        );
        let body: serde_json::Value =
            serde_json::from_str(http.calls[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(body["chatGuid"], "100");
        assert!(body["message"].as_str().unwrap().ends_with("Id: q-pin"));

        let mut http = ScriptedHttp::default().ok(
            r#"{"status":200,"data":[{"guid":"g-2","text":"Reply done when it is done. Id: q-pin","isFromMe":true,"dateCreated":99999999999999},{"guid":"g-3","text":"Done","isFromMe":false,"handle":{"address":"+15550001111"},"threadOriginatorGuid":"g-1","dateCreated":99999999999999}]}"#,
        );
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&[pin()], &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.answered, 1, "{:?}", tick.detail);
        let rows = answer_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["done"], true);
        assert_eq!(rows[0]["attested_by"], "chat:+15550001111:g-3");

        let mut http = ScriptedHttp::default().ok(r#"{"status":200,"data":[]}"#);
        let mut t = transport_for(&s, &mut http);
        let tick = tick_chat(&[], &mut [(&s, &mut *t)], dir.path(), future());
        drop(t);
        assert_eq!(tick.closed, 1);
        let body: serde_json::Value =
            serde_json::from_str(http.calls[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(body["message"], "Closed: q-pin");
    }
}

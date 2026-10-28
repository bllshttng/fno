//! The `messaging` setup step's data layer: the chat channels, which ones
//! this machine has configured, the one curl seam and the test message.
//! BlueBubbles (iMessage), Telegram and Discord are each off until a
//! `[[attention.sinks]]` row of that type exists for this machine and its
//! token resolves.
//!
//! The attention arm on this machine delivers each open question to the
//! channel and reads replies back (`fno-agents` `attention_chat`).

use serde_json::{json, Value};
use std::io::Write;

/// The re-entry command every skip names.
pub const REENTER: &str = "fno config setup run --only messaging";

/// The channels in the order the checklist shows them: row type, label.
pub const CHANNELS: [(&str, &str); 3] = [
    ("bluebubbles", "BlueBubbles (iMessage)"),
    ("telegram", "Telegram"),
    ("discord", "Discord"),
];

const ENV_FILE: &str = "~/.fno/.env";

/// Each channel and whether this machine has a row of that type whose
/// token resolves.
pub fn channel_states(rows: &[Value], machine: &str) -> Vec<(&'static str, bool)> {
    CHANNELS
        .iter()
        .map(|(kind, _)| {
            let configured = rows.iter().any(|r| {
                r["type"] == *kind
                    && r["machine"] == machine
                    && r["token_env"]
                        .as_str()
                        .is_some_and(|env| crate::provider_key::key_present(env, Some(ENV_FILE)))
            });
            (*kind, configured)
        })
        .collect()
}

/// The `attention.sinks` rows of the global config, as JSON. A missing
/// file is no rows; an unreadable one is an error, never "unconfigured".
pub fn sink_rows() -> Result<Vec<Value>, String> {
    let global = crate::config_defaults::global_config_path();
    let map = crate::config_defaults::read_flat(&global)
        .map_err(|e| format!("cannot read {}: {e}", global.display()))?;
    Ok(map
        .and_then(|map| map.get("attention.sinks").cloned())
        .map(|v| match crate::setup_run::toml_as_json(&v) {
            Value::Array(a) => a,
            _ => Vec::new(),
        })
        .unwrap_or_default())
}

/// A row's id as text, string or integer, as the runtime loader reads it.
fn id_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// This machine's id: the one the attention arm compares against `machine`.
pub fn machine() -> String {
    crate::agents_view::machine_id()
}

/// The no-prompt report: each channel's state and the re-entry command.
pub fn summary_line(rows: &[Value], machine: &str) -> String {
    let states: Vec<String> = channel_states(rows, machine)
        .iter()
        .map(|(k, on)| format!("{k} {}", if *on { "configured" } else { "unconfigured" }))
        .collect();
    format!(
        "messaging: chat channels ({}) - set one up with `{REENTER}`",
        states.join(", ")
    )
}

/// The HTTP the setup needs: one call, secrets off argv.
pub trait Http {
    fn call(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&str>,
    ) -> Option<(u16, String)>;
}

/// One curl child: the config (URL, headers, body) on stdin, never argv.
pub struct Curl;

impl Http for Curl {
    fn call(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&str>,
    ) -> Option<(u16, String)> {
        fn quoted(v: &str) -> String {
            v.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        }
        let mut config = format!(
            "url = \"{}\"\nrequest = \"{method}\"\nmax-time = \"20\"\nsilent\nwrite-out = \"\\n%{{http_code}}\"\n",
            quoted(url)
        );
        for (name, value) in headers {
            config.push_str(&format!("header = \"{name}: {}\"\n", quoted(value)));
        }
        if let Some(b) = body {
            config.push_str(&format!("data = \"{}\"\n", quoted(b)));
        }
        let out = std::process::Command::new("curl")
            .args(["--config", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(config.as_bytes());
                }
                child.wait_with_output()
            })
            .ok()?;
        // A failed transfer still prints `000`: no answer, not a refusal.
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n')?;
        Some((code.trim().parse().ok()?, body.to_string()))
    }
}

/// Send one test message and say what landed.
pub fn test_send(kind: &str, row: &Value, token: &str, http: &mut dyn Http) -> String {
    let text =
        "fno: this chat now receives open questions. Reply in a question's thread to answer it.";
    let json_h = ("Content-Type", "application/json".to_string());
    let call = match kind {
        "telegram" => http.call(
            "POST",
            &format!("https://api.telegram.org/bot{token}/sendMessage"),
            &[json_h],
            Some(&json!({"chat_id": row["chat_id"], "text": text}).to_string()),
        ),
        "discord" => http.call(
            "POST",
            &format!(
                "https://discord.com/api/v10/channels/{}/messages",
                id_text(&row["channel_id"])
            ),
            &[json_h, ("Authorization", format!("Bot {token}"))],
            Some(&json!({ "content": text }).to_string()),
        ),
        _ => http.call(
            "POST",
            &format!(
                "{}/api/v1/message/text?password={}",
                row["url"].as_str().unwrap_or_default().trim_end_matches('/'),
                token
                    .bytes()
                    .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    })
                    .collect::<String>()
            ),
            &[json_h],
            Some(&json!({"chatGuid": row["chat_guid"], "tempGuid": "fno-setup", "message": text, "method": "apple-script"}).to_string()),
        ),
    };
    match call {
        Some((code, _)) if (200..300).contains(&code) => "sent".into(),
        Some((code, _)) => format!("refused with {code}; check the ids and the token"),
        None => "no answer from the server".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted(Vec<String>);

    impl Http for Scripted {
        fn call(
            &mut self,
            _method: &str,
            url: &str,
            _headers: &[(&str, String)],
            _body: Option<&str>,
        ) -> Option<(u16, String)> {
            self.0.push(url.into());
            Some((200, "{}".into()))
        }
    }

    /// AC12: the list carries the messaging step with every channel
    /// unconfigured, the report names the re-entry command, and the test
    /// message reaches each channel's own send endpoint.
    #[test]
    fn ac12_messaging_lists_unconfigured_channels_and_sends_one_test_message() {
        let list = crate::setup_run::list_json();
        let row = list
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "messaging")
            .expect("messaging step listed");
        assert_eq!(row["kind"], "human");
        let channels: Vec<&str> = row["channels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["type"].as_str().unwrap())
            .collect();
        assert_eq!(channels, ["bluebubbles", "telegram", "discord"]);
        assert!(channel_states(&[], "this-mac").iter().all(|(_, on)| !on));
        assert!(summary_line(&[], "this-mac").ends_with(&format!("`{REENTER}`")));

        let mut http = Scripted(Vec::new());
        let row = json!({"url": "http://localhost:1234/", "chat_guid": "iMessage;-;a@b.c"});
        assert_eq!(test_send("bluebubbles", &row, "p w&", &mut http), "sent");
        let row = json!({"chat_id": "100"});
        assert_eq!(test_send("telegram", &row, "tok", &mut http), "sent");
        let row = json!({"channel_id": 200});
        assert_eq!(test_send("discord", &row, "tok", &mut http), "sent");
        assert_eq!(
            http.0,
            [
                "http://localhost:1234/api/v1/message/text?password=p%20w%26",
                "https://api.telegram.org/bottok/sendMessage",
                "https://discord.com/api/v10/channels/200/messages",
            ]
        );
    }
}

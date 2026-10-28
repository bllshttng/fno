//! The `messaging` setup step: turn on a chat channel for open questions.
//! BlueBubbles (iMessage), Telegram and Discord are each off until the user
//! picks one here. For each pick the step walks the channel's own setup
//! inline, writes the secret to `~/.fno/.env` (0600), appends one
//! `[[attention.sinks]]` row stamped with this machine's id, and sends one
//! test message. The no-prompt path (`--yes`, a non-TTY stdin, an agent
//! env) writes nothing and names the re-entry command.
//!
//! The attention arm on this machine delivers each open question to the
//! channel and reads replies back (`fno-agents` `attention_chat`).

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

/// The re-entry command every skip names.
pub const REENTER: &str = "fno config setup run --only messaging";

/// The channels in the order the checklist shows them: row type, label.
pub const CHANNELS: [(&str, &str); 3] = [
    ("bluebubbles", "BlueBubbles (iMessage)"),
    ("telegram", "Telegram"),
    ("discord", "Discord"),
];

const ENV_FILE: &str = "~/.fno/.env";

/// What the step did, folded into the setup report by the caller.
#[derive(Debug, Default)]
pub struct Outcome {
    pub done: Vec<String>,
    pub skipped: Vec<String>,
    pub needs_human: Vec<String>,
    pub paths: Vec<String>,
}

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

/// The `attention.sinks` rows of the global config, as JSON.
pub fn sink_rows() -> Vec<Value> {
    let global = crate::config_defaults::global_config_path();
    crate::config_defaults::read_flat(&global)
        .ok()
        .flatten()
        .and_then(|map| map.get("attention.sinks").cloned())
        .map(|v| match crate::setup_run::toml_as_json(&v) {
            Value::Array(a) => a,
            _ => Vec::new(),
        })
        .unwrap_or_default()
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

/// The terminal asks the step makes. Tests answer from a queue.
pub trait Ask {
    fn say(&mut self, text: &str);
    fn line(&mut self, prompt: &str) -> Option<String>;
    fn secret(&mut self, prompt: &str) -> Option<String>;
}

/// The prompting step; the no-prompt path prints `summary_line` instead.
/// `picked` carries the wizard's channel picks; `None` shows the checklist.
/// `set` writes `attention.sinks` (the validated `fno config set` path).
pub fn step(
    picked: Option<&[&str]>,
    ask: &mut dyn Ask,
    set: &dyn Fn(&str) -> Result<PathBuf, String>,
) -> Outcome {
    let mut out = Outcome::default();
    let mut rows = sink_rows();
    let me = machine();
    let states = channel_states(&rows, &me);
    let picks: Vec<usize> = match picked {
        Some(kinds) => (0..CHANNELS.len())
            .filter(|i| kinds.contains(&CHANNELS[*i].0))
            .collect(),
        None => {
            ask.say("Chat channels deliver open questions; a reply in the thread answers it.");
            for (i, ((_, label), (_, on))) in CHANNELS.iter().zip(&states).enumerate() {
                let mark = if *on { "configured" } else { "not configured" };
                ask.say(&format!("  {}. {label} ({mark})", i + 1));
            }
            ask.line("Set up which? Numbers, comma separated; Enter skips:")
                .unwrap_or_default()
                .split(',')
                .filter_map(|w| w.trim().parse::<usize>().ok())
                .filter(|n| (1..=CHANNELS.len()).contains(n))
                .map(|n| n - 1)
                .collect()
        }
    };
    if picks.is_empty() {
        out.skipped.push(format!(
            "messaging: skipped - set one up later with `{REENTER}`"
        ));
        return out;
    }
    for i in picks {
        let kind = CHANNELS[i].0;
        match setup_channel(kind, ask, &me, &mut Curl) {
            Ok((row, env, token)) => {
                if let Err(e) = upsert_env(env, &token) {
                    out.needs_human
                        .push(format!("messaging: {kind}: cannot write {ENV_FILE}: {e}"));
                    continue;
                }
                rows.retain(|r| !(r["type"] == kind && r["machine"] == me.as_str()));
                rows.push(row.clone());
                match set(&Value::Array(rows.clone()).to_string()) {
                    Ok(path) => {
                        out.done
                            .push(format!("messaging: {kind} added to attention.sinks"));
                        out.paths.push(path.display().to_string());
                        out.done.push(format!(
                            "messaging: {kind} test message: {}",
                            test_send(kind, &row, &token, &mut Curl)
                        ));
                    }
                    Err(e) => out.needs_human.push(format!("messaging: {kind}: {e}")),
                }
            }
            Err(why) => out
                .needs_human
                .push(format!("messaging: {kind}: {why} - rerun `{REENTER}`")),
        }
    }
    out
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
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n')?;
        Some((code.trim().parse().ok()?, body.to_string()))
    }
}

/// Walk one channel's setup. Returns the row, its token env name and the
/// secret.
fn setup_channel(
    kind: &str,
    ask: &mut dyn Ask,
    machine: &str,
    http: &mut dyn Http,
) -> Result<(Value, &'static str, String), String> {
    match kind {
        "telegram" => {
            ask.say("Telegram: message @BotFather, send /newbot, and copy the token it gives you.");
            let token = ask.secret("Bot token:").ok_or("no token")?;
            ask.say("Now send your new bot any message from your own Telegram account.");
            ask.line("Press Enter once you have sent it:");
            let url = format!("https://api.telegram.org/bot{token}/getUpdates");
            let (code, body) = http
                .call(
                    "POST",
                    &url,
                    &[("Content-Type", "application/json".into())],
                    Some("{}"),
                )
                .ok_or("cannot reach api.telegram.org")?;
            if code != 200 {
                return Err(format!("getUpdates answered {code}: check the token"));
            }
            let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            let last = v["result"]
                .as_array()
                .and_then(|a| a.iter().rev().find(|u| u.get("message").is_some()))
                .ok_or("the bot has no message yet: send it one first")?;
            let chat = last["message"]["chat"]["id"].to_string();
            let from = last["message"]["from"]["id"].to_string();
            Ok((
                json!({"name": "telegram", "type": "telegram", "machine": machine,
                       "token_env": "FNO_CHAT_TELEGRAM_TOKEN", "chat_id": chat, "allow": [from]}),
                "FNO_CHAT_TELEGRAM_TOKEN",
                token,
            ))
        }
        "discord" => {
            ask.say("Discord: at discord.com/developers/applications create an application, open Bot, reset and copy the token, and turn on Message Content Intent.");
            ask.say("Then under OAuth2 > URL Generator pick the bot scope with Send Messages, Create Public Threads, Send Messages in Threads and Read Message History, and open the URL to invite the bot.");
            let token = ask.secret("Bot token:").ok_or("no token")?;
            ask.say(
                "Turn on Developer Mode (User Settings > Advanced) to copy ids with a right click.",
            );
            let channel = ask
                .line("Channel id for questions:")
                .filter(|s| !s.is_empty())
                .ok_or("no channel id")?;
            let user = ask
                .line("Your user id:")
                .filter(|s| !s.is_empty())
                .ok_or("no user id")?;
            Ok((
                json!({"name": "discord", "type": "discord", "machine": machine,
                       "token_env": "FNO_CHAT_DISCORD_TOKEN", "channel_id": channel, "allow": [user]}),
                "FNO_CHAT_DISCORD_TOKEN",
                token,
            ))
        }
        _ => {
            ask.say("BlueBubbles: install BlueBubbles Server on this Mac from github.com/BlueBubblesApp/bluebubbles-server and sign in to iMessage there.");
            ask.say("The server URL and password are in BlueBubbles Server under Settings > API.");
            let url = ask
                .line("Server URL [http://localhost:1234]:")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "http://localhost:1234".into());
            let password = ask.secret("Server password:").ok_or("no password")?;
            let handle = ask
                .line("Your phone number or Apple ID email (questions go to this chat):")
                .filter(|s| !s.is_empty())
                .ok_or("no handle")?;
            Ok((
                json!({"name": "bluebubbles", "type": "bluebubbles", "machine": machine,
                       "token_env": "FNO_CHAT_BLUEBUBBLES_PASSWORD", "url": url,
                       "chat_guid": format!("iMessage;-;{handle}"), "allow": [handle]}),
                "FNO_CHAT_BLUEBUBBLES_PASSWORD",
                password,
            ))
        }
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
                row["channel_id"].as_str().unwrap_or_default()
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

/// Write `key=value` to `~/.fno/.env`, replacing an existing line, mode 0600.
fn upsert_env(key: &str, value: &str) -> std::io::Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| std::io::Error::other("HOME is unset"))?;
    let path = PathBuf::from(home).join(".fno/.env");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = old
        .lines()
        .filter(|l| {
            let l = l.trim().strip_prefix("export ").unwrap_or(l.trim());
            l.split_once('=').map(|(k, _)| k.trim()) != Some(key)
        })
        .map(str::to_string)
        .collect();
    lines.push(format!("{key}={value}"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&path)?
        .write_all(format!("{}\n", lines.join("\n")).as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The terminal: plain line reads, and a masked read for a secret. A raw
/// mode the wizard left on is paused for the step and restored after.
pub struct TermAsk {
    was_raw: bool,
}

impl TermAsk {
    pub fn new() -> Self {
        let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
        if was_raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
        TermAsk { was_raw }
    }
}

impl Default for TermAsk {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TermAsk {
    fn drop(&mut self) {
        if self.was_raw {
            let _ = crossterm::terminal::enable_raw_mode();
        }
    }
}

impl Ask for TermAsk {
    fn say(&mut self, text: &str) {
        println!("{text}");
    }

    fn line(&mut self, prompt: &str) -> Option<String> {
        print!("{prompt} ");
        let _ = std::io::stdout().flush();
        let mut s = String::new();
        match std::io::stdin().read_line(&mut s) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(s.trim().to_string()),
        }
    }

    fn secret(&mut self, prompt: &str) -> Option<String> {
        use crossterm::event::{read, Event, KeyCode, KeyEventKind, KeyModifiers};
        print!("{prompt} ");
        let _ = std::io::stdout().flush();
        let _ = crossterm::terminal::enable_raw_mode();
        let mut value = String::new();
        let ok = loop {
            match read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                    KeyCode::Enter => break true,
                    KeyCode::Esc => break false,
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        break false
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(c) => value.push(c),
                    _ => {}
                },
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        let _ = crossterm::terminal::disable_raw_mode();
        println!();
        (ok && !value.is_empty()).then_some(value)
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
        assert_eq!(
            http.0,
            [
                "http://localhost:1234/api/v1/message/text?password=p%20w%26",
                "https://api.telegram.org/bottok/sendMessage",
            ]
        );
    }

    struct Answers(Vec<&'static str>);

    impl Ask for Answers {
        fn say(&mut self, _t: &str) {}
        fn line(&mut self, _p: &str) -> Option<String> {
            Some(self.0.remove(0).into())
        }
        fn secret(&mut self, _p: &str) -> Option<String> {
            Some(self.0.remove(0).into())
        }
    }

    struct Updates;

    impl Http for Updates {
        fn call(
            &mut self,
            _m: &str,
            url: &str,
            _h: &[(&str, String)],
            _b: Option<&str>,
        ) -> Option<(u16, String)> {
            assert_eq!(url, "https://api.telegram.org/bottok/getUpdates");
            Some((
                200,
                r#"{"result":[{"message":{"chat":{"id":100},"from":{"id":42}}}]}"#.into(),
            ))
        }
    }

    /// AC13: the Telegram walk reads the chat and the sender from the
    /// user's first message, stamps this machine, and allows only that
    /// sender.
    #[test]
    fn ac13_telegram_walk_builds_the_row_from_the_first_message() {
        let (row, env, token) = setup_channel(
            "telegram",
            &mut Answers(vec!["tok", ""]),
            "this-mac",
            &mut Updates,
        )
        .unwrap();
        assert_eq!((env, token.as_str()), ("FNO_CHAT_TELEGRAM_TOKEN", "tok"));
        assert_eq!(row["chat_id"], "100");
        assert_eq!(row["allow"], json!(["42"]));
        assert_eq!(row["machine"], "this-mac");
    }
}

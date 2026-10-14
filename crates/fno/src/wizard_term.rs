//! The wizard's terminal host: `fno do wizard`.
//!
//! A person at a terminal answers the pure flow in [crate::wizard] through
//! raw-mode keys when stdin is a tty, and through plain numbered lines
//! otherwise (a pane host piping stdin, for one). The host owns the real
//! subprocesses, the harness and model probes, and applies the committed
//! answers through the same lanes `fno config setup run` uses, so the two
//! surfaces can never disagree about what a setup writes.
//!
//! Sidecar files live beside the model catalog state, all best-effort:
//! `wizard-report.json` carries status, screens, checks and the redacted
//! apply JSON, `wizard-answers.toml` carries the committed answers keyed by
//! screen id, and `wizard-lock` carries the pid of the live run. A refusal
//! at the flag line or the lock writes nothing; a refusal at the stdin gate
//! writes `status: refused` and nothing else.

use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::cursor::MoveUp;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::queue;
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use serde_json::json;

use crate::model_catalog;
use crate::setup_autowire::HARNESS_LIST;
use crate::setup_detect;
use crate::setup_profile;
use crate::setup_run;
use crate::wizard::{
    self, Answer, Conf, Found, Hidden, Next, Pick, Preset, Probe, Probes, Prompter, State, Ticks,
};

const USAGE: &str = "fno do wizard [--preset aggressive|balanced|user-reviewed] [--join <profile>] [--json]
Interactive onboarding: asks, probes one harness until one works, then applies through the same lanes as fno config setup run. Needs a terminal on stdin; an unattended machine uses fno config setup run --dry-run --json.";

/// Below this many options a filter input is noise; a list that long earns
/// the `/`-free always-on search line.
const SEARCH_MIN: usize = 12;

fn sidecar() -> PathBuf {
    model_catalog::state_dir().join("sidecar")
}

fn report_path() -> PathBuf {
    sidecar().join("wizard-report.json")
}

fn answers_path() -> PathBuf {
    sidecar().join("wizard-answers.toml")
}

fn lock_path() -> PathBuf {
    sidecar().join("wizard-lock")
}

fn parse_preset(word: &str) -> Option<Preset> {
    match word {
        "aggressive" => Some(Preset::Aggressive),
        "balanced" => Some(Preset::Balanced),
        "user-reviewed" => Some(Preset::UserReviewed),
        _ => None,
    }
}

/// One output line that works in both terminal modes: raw mode needs the
/// explicit carriage return or every line staircases one column right.
fn term_line(raw: bool, text: &str) {
    if raw {
        let mut out = io::stdout();
        let _ = queue!(out, Print(format!("{text}\r\n")));
        let _ = out.flush();
    } else {
        println!("{text}");
    }
}

/// The keys the flow cares about. Everything else (resize, paste, function
/// keys) is filtered out at the read boundary.
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    Char(char),
    CtrlC,
    Space,
}

fn read_key_press() -> Option<Key> {
    loop {
        let Ok(ev) = event::read() else { return None };
        let Event::Key(key) = ev else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let k = match key.code {
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Char(' ') => Key::Space,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Key::CtrlC,
            KeyCode::Char(c) => Key::Char(c),
            // Home, End, function and media keys: not part of the flow, so
            // the read loop waits for the next press instead of mapping them.
            _ => continue,
        };
        return Some(k);
    }
}

/// One draw of a list screen. `drawn` tracks the previous draw's line count
/// so a key press repaints in place: move up over the old block, clear down,
/// print the new one. Raw mode needs `\r\n`, not `\n`.
fn draw_list(
    rows: &[(bool, bool, &str)],
    prompt: &str,
    hint: &str,
    search: Option<&str>,
    drawn: &mut usize,
) {
    let total = 1 + rows.len() + 1 + usize::from(search.is_some());
    let mut out = io::stdout();
    if *drawn > 0 {
        let _ = queue!(
            out,
            MoveUp((*drawn as u16).saturating_sub(1)),
            Clear(ClearType::FromCursorDown)
        );
    }
    let _ = queue!(out, Print(format!("{prompt}\r\n")));
    for (cursor, tick, text) in rows {
        let marker = if *cursor { "> " } else { "  " };
        let ticked = if *tick { "[x] " } else { "[ ] " };
        let _ = queue!(out, Print(format!("{marker}{ticked}{text}\r\n")));
    }
    if let Some(q) = search {
        let _ = queue!(out, Print(format!("search: {q}\r\n")));
    }
    let _ = queue!(out, SetAttribute(Attribute::Dim));
    let _ = queue!(out, Print(format!("{hint}\r\n")));
    let _ = queue!(out, SetAttribute(Attribute::Reset));
    let _ = out.flush();
    *drawn = total;
}

/// The real prompter. Raw mode gets single-key answers with an in-place
/// redrawn cursor; line mode gets the numbered-list transcript any pane can
/// read. Back is offered only while `back_live`, which the host refreshes
/// from the flow's write latch before every screen.
struct TermPrompter {
    raw: bool,
    back_live: bool,
    json: bool,
}

impl Prompter for TermPrompter {
    fn confirm(&mut self, _screen: &'static str, prompt: &str, default: bool) -> Conf {
        if self.raw {
            self.confirm_raw(prompt, default)
        } else {
            self.confirm_line(prompt, default)
        }
    }

    fn select(
        &mut self,
        _screen: &'static str,
        prompt: &str,
        options: &[&str],
        default: usize,
    ) -> Pick {
        if self.raw {
            self.select_raw(prompt, options, default)
        } else {
            self.select_line(prompt, options, default)
        }
    }

    fn checklist(
        &mut self,
        _screen: &'static str,
        prompt: &str,
        options: &[&str],
        defaults: &[usize],
    ) -> Ticks {
        if self.raw {
            self.checklist_raw(prompt, options, defaults)
        } else {
            self.checklist_line(prompt, options, defaults)
        }
    }

    fn secret(&mut self, _screen: &'static str, prompt: &str) -> Hidden {
        if self.raw {
            self.secret_raw(prompt)
        } else {
            self.secret_line(prompt)
        }
    }

    fn note(&mut self, screen: &'static str, text: &str) {
        if self.json {
            return;
        }
        term_line(self.raw, &format!("[{screen}] {text}"));
    }

    fn progress(&mut self, screen: &'static str, label: &str) {
        self.note(screen, label);
    }
}

impl TermPrompter {
    fn confirm_raw(&mut self, prompt: &str, default: bool) -> Conf {
        let d = if default { "Y/n" } else { "y/N" };
        let mut hint = String::from("enter = default, esc = cancel");
        if self.back_live {
            hint.push_str(", b or left = back");
        }
        term_line(true, &format!("{prompt} ({d})  [{hint}]"));
        loop {
            match read_key_press() {
                None | Some(Key::Esc) | Some(Key::CtrlC) => return Conf::Cancel,
                Some(Key::Char('y' | 'Y')) => return Conf::Yes,
                Some(Key::Char('n' | 'N')) => return Conf::No,
                Some(Key::Enter) => return if default { Conf::Yes } else { Conf::No },
                Some(Key::Left) | Some(Key::Char('b')) if self.back_live => return Conf::Back,
                _ => {}
            }
        }
    }

    fn confirm_line(&mut self, prompt: &str, default: bool) -> Conf {
        let d = if default { "Y/n" } else { "y/N" };
        loop {
            println!("{prompt} ({d})");
            let answer = match read_line() {
                Some(a) => a,
                None => return Conf::Cancel,
            };
            match answer.as_str() {
                "" => return if default { Conf::Yes } else { Conf::No },
                "y" | "Y" | "yes" => return Conf::Yes,
                "n" | "N" | "no" => return Conf::No,
                "b" | "back" if self.back_live => return Conf::Back,
                "cancel" | "q" | "quit" => return Conf::Cancel,
                _ => {
                    let words = if self.back_live {
                        "y, n, back or cancel"
                    } else {
                        "y, n or cancel"
                    };
                    println!("  answer {words}.");
                }
            }
        }
    }

    fn select_raw(&mut self, prompt: &str, options: &[&str], default: usize) -> Pick {
        let searching = options.len() >= SEARCH_MIN;
        let mut query = String::new();
        let mut drawn = 0usize;
        let mut sel = default.min(options.len().saturating_sub(1));
        let mut hint = String::from("up/down = move, enter = choose, esc = cancel");
        if self.back_live {
            hint.push_str(", b or left = back");
        }
        if searching {
            hint.push_str(", type = filter, backspace = erase");
        }
        loop {
            let view = filtered_view(options, searching, &query);
            if sel >= view.len() {
                sel = view.len().saturating_sub(1);
            }
            let rows: Vec<(bool, bool, &str)> = view
                .iter()
                .enumerate()
                .map(|(i, oi)| (i == sel, false, options[*oi]))
                .collect();
            draw_list(
                &rows,
                prompt,
                &hint,
                if searching {
                    Some(query.as_str())
                } else {
                    None
                },
                &mut drawn,
            );
            match read_key_press() {
                None | Some(Key::Esc) | Some(Key::CtrlC) => return Pick::Cancel,
                Some(Key::Up) => sel = sel.saturating_sub(1),
                Some(Key::Down) => {
                    if sel + 1 < view.len() {
                        sel += 1;
                    }
                }
                Some(Key::Enter) => {
                    return match view.get(sel) {
                        Some(oi) => Pick::Ok(*oi),
                        None => Pick::Cancel,
                    };
                }
                Some(Key::Char(c)) if searching && c.is_ascii_alphanumeric() => {
                    query.push(c.to_ascii_lowercase());
                }
                Some(Key::Backspace) if searching => {
                    query.pop();
                }
                Some(Key::Left) | Some(Key::Char('b')) if self.back_live => return Pick::Back,
                Some(Key::Char(c)) if !searching && c.is_ascii_digit() => {
                    let n = c.to_digit(10).unwrap_or(0) as usize;
                    if n >= 1 {
                        if let Some(pos) = view.iter().position(|oi| *oi == n - 1) {
                            sel = pos;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn select_line(&mut self, prompt: &str, options: &[&str], default: usize) -> Pick {
        println!("{prompt}");
        for (i, o) in options.iter().enumerate() {
            println!("  {}) {o}", i + 1);
        }
        loop {
            print!(
                "  number 1-{}, or b = back, cancel = quit [{}] ",
                options.len(),
                default + 1
            );
            let _ = io::stdout().flush();
            let answer = match read_line() {
                Some(a) => a,
                None => return Pick::Cancel,
            };
            if answer.is_empty() {
                return Pick::Ok(default);
            }
            if let Ok(n) = answer.parse::<usize>() {
                if n >= 1 && n <= options.len() {
                    return Pick::Ok(n - 1);
                }
            }
            if (answer == "b" || answer == "back") && self.back_live {
                return Pick::Back;
            }
            if answer == "cancel" || answer == "q" || answer == "quit" {
                return Pick::Cancel;
            }
            let words = if self.back_live {
                "a number, b or cancel"
            } else {
                "a number or cancel"
            };
            println!("  answer {words}.");
        }
    }

    fn checklist_raw(&mut self, prompt: &str, options: &[&str], defaults: &[usize]) -> Ticks {
        let mut ticks: Vec<bool> = (0..options.len()).map(|i| defaults.contains(&i)).collect();
        let searching = options.len() >= SEARCH_MIN;
        let mut query = String::new();
        let mut drawn = 0usize;
        let mut sel = 0usize;
        let mut hint = String::from("space = toggle, enter = confirm, esc = cancel");
        if self.back_live {
            hint.push_str(", b or left = back");
        }
        if searching {
            hint.push_str(", type = filter, backspace = erase");
        }
        loop {
            let view = filtered_view(options, searching, &query);
            if sel >= view.len() {
                sel = view.len().saturating_sub(1);
            }
            let rows: Vec<(bool, bool, &str)> = view
                .iter()
                .enumerate()
                .map(|(i, oi)| (i == sel, ticks[*oi], options[*oi]))
                .collect();
            draw_list(
                &rows,
                prompt,
                &hint,
                if searching {
                    Some(query.as_str())
                } else {
                    None
                },
                &mut drawn,
            );
            match read_key_press() {
                None | Some(Key::Esc) | Some(Key::CtrlC) => return Ticks::Cancel,
                Some(Key::Space) => {
                    if let Some(oi) = view.get(sel) {
                        ticks[*oi] = !ticks[*oi];
                    }
                }
                Some(Key::Enter) => {
                    let picked: Vec<usize> = (0..options.len()).filter(|i| ticks[*i]).collect();
                    return Ticks::Ok(picked);
                }
                Some(Key::Char(c)) if searching && c.is_ascii_alphanumeric() => {
                    query.push(c.to_ascii_lowercase());
                }
                Some(Key::Backspace) if searching => {
                    query.pop();
                }
                Some(Key::Left) | Some(Key::Char('b')) if self.back_live => return Ticks::Back,
                _ => {}
            }
        }
    }

    fn checklist_line(&mut self, prompt: &str, options: &[&str], defaults: &[usize]) -> Ticks {
        let mut ticks: Vec<bool> = (0..options.len()).map(|i| defaults.contains(&i)).collect();
        println!("{prompt}");
        loop {
            for (i, o) in options.iter().enumerate() {
                let t = if ticks[i] { "x" } else { " " };
                println!("  {}) [{t}] {o}", i + 1);
            }
            print!("  numbers to toggle, space-separated; enter = done; b = back; cancel = quit: ");
            let _ = io::stdout().flush();
            let answer = match read_line() {
                Some(a) => a,
                None => return Ticks::Cancel,
            };
            if answer.is_empty() {
                return Ticks::Ok((0..options.len()).filter(|i| ticks[*i]).collect());
            }
            if answer == "cancel" || answer == "q" || answer == "quit" {
                return Ticks::Cancel;
            }
            if (answer == "b" || answer == "back") && self.back_live {
                return Ticks::Back;
            }
            let mut ok = true;
            for tok in answer
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
            {
                match tok.parse::<usize>() {
                    Ok(n) if n >= 1 && n <= options.len() => {
                        ticks[n - 1] = !ticks[n - 1];
                    }
                    _ => {
                        println!("  no option {tok}.");
                        ok = false;
                    }
                }
            }
            if !ok {
                let words = if self.back_live {
                    "numbers, b or cancel"
                } else {
                    "numbers or cancel"
                };
                println!("  answer {words}.");
            }
        }
    }

    fn secret_raw(&mut self, prompt: &str) -> Hidden {
        let mut out = io::stdout();
        let _ = queue!(out, Print(format!("{prompt}\r\n")));
        let _ = out.flush();
        let mut value = String::new();
        loop {
            match read_key_press() {
                None | Some(Key::Esc) | Some(Key::CtrlC) => {
                    let _ = queue!(out, Print("\r\n"));
                    let _ = out.flush();
                    return Hidden::Cancel;
                }
                Some(Key::Enter) => {
                    let _ = queue!(out, Print("\r\n"));
                    let _ = out.flush();
                    return if value.is_empty() {
                        Hidden::Skipped
                    } else {
                        Hidden::Value(value)
                    };
                }
                Some(Key::Backspace) => {
                    value.pop();
                }
                Some(Key::Char(c)) => {
                    value.push(c);
                    let _ = queue!(out, Print("*"));
                    let _ = out.flush();
                }
                _ => {}
            }
        }
    }

    fn secret_line(&mut self, prompt: &str) -> Hidden {
        println!("{prompt}");
        match read_line() {
            None => Hidden::Cancel,
            Some(w) if w == "cancel" || w == "q" || w == "quit" => Hidden::Cancel,
            Some(w) if (w == "b" || w == "back") && self.back_live => Hidden::Back,
            Some(w) if w.is_empty() => Hidden::Skipped,
            Some(w) => Hidden::Value(w),
        }
    }
}

/// The visible option indices under the current filter. An empty or short
/// query shows everything, so the no-search path and the cleared-filter path
/// are the same list.
fn filtered_view(options: &[&str], searching: bool, query: &str) -> Vec<usize> {
    if searching && !query.is_empty() {
        let q = query.to_lowercase();
        options
            .iter()
            .enumerate()
            .filter(|(_, o)| o.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    } else {
        (0..options.len()).collect()
    }
}

fn read_line() -> Option<String> {
    let mut buf = String::new();
    match io::stdin().read_line(&mut buf) {
        Ok(0) => None,
        Ok(_) => Some(buf.trim().to_string()),
        Err(_) => None,
    }
}

/// Restores the terminal even on a panic: raw mode left on would leave the
/// person's shell echoing nothing.
struct RawGuard {
    active: bool,
}

impl RawGuard {
    fn enable_if(active: bool) -> RawGuard {
        let enabled = active && enable_raw_mode().is_ok();
        RawGuard { active: enabled }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
        }
    }
}

/// One wizard at a time per user: the pid file is both the lock and the
/// recovery hint. Removed on every exit path, panics included; only a hard
/// kill leaks it, and then the refusal names the remove command.
struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn take(path: PathBuf) -> Result<LockGuard, ()> {
        if let Ok(text) = fs::read_to_string(&path) {
            eprintln!(
                "fno do wizard: another wizard run may be live: pid {}",
                text.trim()
            );
            eprintln!(
                "remove {} if that run is gone, then start again.",
                path.display()
            );
            return Err(());
        }
        if let Err(e) = fs::write(&path, format!("{}\n", std::process::id())) {
            eprintln!(
                "fno do wizard: cannot write the lock {}: {e}",
                path.display()
            );
            return Err(());
        }
        Ok(LockGuard { path })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The one detection pass behind the flow's first screens: markers from the
/// sidecar, the live band clause parsed by the same parser the run step
/// uses, harness presence from `setup_detect`, and env names plus models
/// from the bundled catalog. Read-only; a missing catalog just means empty
/// screens, which the flow self-skips legally.
fn real_detect() -> wizard::Detect {
    let mut detect = wizard::Detect::fresh();
    detect.markers_present =
        setup_profile::profile_path().exists() || setup_profile::in_progress_flag().exists();
    detect.band = setup_detect::budget_clause().and_then(|c| setup_detect::parse_budget_clause(&c));
    detect.harnesses = detect_harnesses();
    let (keys, models) = catalog_keys_and_models();
    detect.env_keys = keys;
    detect.models = models;
    detect
}

fn detect_harnesses() -> Vec<Found> {
    let doc = setup_detect::detect(&setup_run::repo_root(), None);
    let rows = doc
        .get("harnesses")
        .and_then(|h| h.as_array())
        .cloned()
        .unwrap_or_default();
    let mut found = Vec::new();
    for row in rows {
        let name = row.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let on_path = row
            .get("on_path")
            .and_then(|p| p.as_bool())
            .unwrap_or(false);
        if let Some(h) = HARNESS_LIST.iter().find(|h| h.name == name) {
            found.push(Found {
                name: h.name,
                by: if on_path { "on PATH" } else { "not on PATH" },
            });
        }
    }
    found
}

fn catalog_keys_and_models() -> (Vec<(&'static str, &'static str)>, Vec<String>) {
    let mut keys = Vec::new();
    let mut models: Vec<String> = Vec::new();
    let dir = model_catalog::state_dir();
    let _ = model_catalog::refresh_if_stale(&dir);
    let Ok(catalog) = model_catalog::load(&dir) else {
        return (keys, models);
    };
    for (harness, provider) in [
        ("claude", "anthropic"),
        ("codex", "openai"),
        ("gemini", "google"),
    ] {
        let Some(p) = catalog.get(provider) else {
            continue;
        };
        if let Some(env0) = p.env.first() {
            if std::env::var_os(env0).is_some() {
                // State owns &'static strs; the env name outlives the run,
                // so leak the one short string rather than rework the flow.
                let leaked: &'static str = Box::leak(env0.clone().into_boxed_str());
                keys.push((harness, leaked));
            }
        }
        let mut ids = p.model_ids();
        ids.truncate(8);
        models.extend(ids);
    }
    let mut seen = std::collections::HashSet::new();
    models.retain(|m| seen.insert(m.clone()));
    (keys, models)
}

/// Real subprocess probes: one tiny prompt per harness or model through the
/// same bounded runner detection uses, so a hung CLI can never hang the
/// wizard past the killer's timeout.
struct RealProbes {
    exe: PathBuf,
    raw: bool,
    quiet: bool,
}

impl RealProbes {
    fn spawn_probe(&mut self, harness: &str, model: Option<&str>) -> Probe {
        let exe = self.exe.to_string_lossy().into_owned();
        let mut argv: Vec<&str> = vec![
            exe.as_str(),
            "agents",
            "spawn",
            "-H",
            harness,
            "-p",
            "-t",
            "60",
        ];
        if let Some(m) = model {
            argv.push("-m");
            argv.push(m);
        }
        argv.push("Reply with the one word ok.");
        if !self.quiet {
            let with = model.map(|m| format!(" {m}")).unwrap_or_default();
            term_line(self.raw, &format!("  probing {harness}{with} ..."));
        }
        let started = std::time::Instant::now();
        match setup_detect::bounded_for(&argv, Duration::from_secs(60)) {
            Ok(_) => {
                let ms = started.elapsed().as_millis() as u64;
                if !self.quiet {
                    term_line(self.raw, &format!("  {harness}: pass ({ms}ms)"));
                }
                Probe::Pass { ms }
            }
            Err(e) => {
                let why = probe_reason(&e);
                if !self.quiet {
                    term_line(self.raw, &format!("  {harness}: fail ({why})"));
                }
                Probe::Fail(why)
            }
        }
    }
}

impl Probes for RealProbes {
    fn harness_check(&mut self, harness: &str) -> Probe {
        self.spawn_probe(harness, None)
    }

    fn model_check(&mut self, harness: &str, model: &str) -> Probe {
        self.spawn_probe(harness, Some(model))
    }
}

/// Sorts a probe failure into the four words the report and the flow
/// understand. Matched on the bounded runner's own error text, so the
/// mapping lives in one place.
fn probe_reason(message: &str) -> &'static str {
    if message.contains("timed out") {
        return "timeout";
    }
    let low = message.to_lowercase();
    const AUTH: [&str; 6] = [
        "unauthorized",
        "login",
        "auth",
        "token",
        "api key",
        "apikey",
    ];
    if AUTH.iter().any(|k| low.contains(k)) {
        return "auth";
    }
    const MISSING: [&str; 6] = [
        "not found",
        "no such",
        "not on path",
        "enoent",
        "not installed",
        "command not found",
    ];
    if MISSING.iter().any(|k| low.contains(k)) {
        return "not_installed";
    }
    "unknown"
}

fn screens_json(state: &State) -> serde_json::Value {
    serde_json::Value::Array(
        state
            .screens
            .iter()
            .map(|(id, mark)| json!({ "id": id, "mark": mark }))
            .collect(),
    )
}

fn checks_json(state: &State) -> serde_json::Value {
    serde_json::Value::Array(
        state
            .checks
            .iter()
            .map(|(harness, result, ms)| json!({ "harness": harness, "result": result, "ms": ms }))
            .collect(),
    )
}

fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The committed answers, keyed by screen id (the preset is a launch flag,
/// not an answer). A BTreeMap iterates sorted, so the file is stable across
/// identical runs and diffable.
fn answers_toml(state: &State) -> String {
    let mut out = String::new();
    for (screen, answer) in &state.answers {
        let key = toml_quote(screen);
        match answer {
            Answer::Yes => out.push_str(&format!("{key} = true\n")),
            Answer::No => out.push_str(&format!("{key} = false\n")),
            Answer::Picked(picked) => {
                let items: Vec<String> = picked.iter().map(|p| toml_quote(p)).collect();
                out.push_str(&format!("{key} = [{}]\n", items.join(", ")));
            }
            Answer::Text(text) => out.push_str(&format!("{key} = {}\n", toml_quote(text))),
        }
    }
    out
}

fn write_report(value: &serde_json::Value) {
    let _ = fs::create_dir_all(sidecar());
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    if let Err(e) = fs::write(report_path(), text) {
        eprintln!("fno do wizard: cannot write the report: {e}");
    }
}

fn refuse_word(message: &str) -> i32 {
    eprintln!("fno do wizard: {message}");
    eprintln!(
        "usage: fno do wizard [--preset aggressive|balanced|user-reviewed] [--join <profile>] [--json]"
    );
    2
}

/// The entry point behind `fno do wizard`. Exit codes: 0 finished, 130 the
/// person cancelled, 2 refused (bad flag, no terminal, stale lock, bad
/// profile). The `wizard-report: ` sentinel is always the last line on
/// stdout, in every mode, so a pane host can wait for one pattern.
pub fn run(tail: &[OsString]) -> i32 {
    let mut preset_text: Option<String> = None;
    let mut join_file: Option<String> = None;
    let mut json = false;
    let mut it = tail.iter();
    while let Some(a) = it.next() {
        let word = a.to_string_lossy();
        if word == "--json" {
            json = true;
        } else if word == "--help" || word == "-h" {
            println!("{USAGE}");
            return 0;
        } else if let Some(v) = word.strip_prefix("--preset=") {
            preset_text = Some(v.to_string());
        } else if word == "--preset" {
            match it.next() {
                Some(v) => preset_text = Some(v.to_string_lossy().into_owned()),
                None => return refuse_word("--preset needs aggressive, balanced or user-reviewed"),
            }
        } else if let Some(v) = word.strip_prefix("--join=") {
            join_file = Some(v.to_string());
        } else if word == "--join" {
            match it.next() {
                Some(v) => join_file = Some(v.to_string_lossy().into_owned()),
                None => return refuse_word("--join needs a profile file path"),
            }
        } else {
            return refuse_word(&format!("unknown flag {word}"));
        }
    }
    let preset = match &preset_text {
        None => None,
        Some(word) => match parse_preset(word) {
            Some(p) => Some(p),
            None => return refuse_word("--preset takes aggressive, balanced or user-reviewed"),
        },
    };
    if join_file.as_deref() == Some("-") {
        return refuse_word(
            "the wizard reads a terminal on stdin; pass a profile file path, not -",
        );
    }

    // Preflight: a person must be on stdin. Without a terminal the raw
    // prompts would read EOF forever, so refuse with the unattended door
    // spelled out. This one refusal writes the report; the flag and lock
    // refusals above write nothing.
    if !io::stdin().is_terminal() {
        write_report(&json!({
            "status": "refused",
            "run_id": setup_run::run_id(),
            "host": "wizard",
            "preset": preset_text,
            "join": join_file.is_some(),
            "skip_all": false,
            "screens": [],
            "checks": [],
            "next_action": serde_json::Value::Null,
            "files_written": [],
            "apply": serde_json::Value::Null,
            "refused": "stdin is not a terminal",
        }));
        println!("fno do wizard needs a person on stdin.");
        println!("For an unattended machine: fno config setup run --dry-run --json");
        return 2;
    }

    // A join profile validates before anything runs: a bad file must not
    // leave a running report behind. The diff banner always prints so the
    // person sees what joining would change before the first question.
    let profile_join = match &join_file {
        None => None,
        Some(path) => {
            let profile = match setup_profile::load_profile(path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("fno do wizard: --join {path}: {e}");
                    return 2;
                }
            };
            let here = match setup_profile::here_config() {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("fno do wizard: the global config does not parse: {e}");
                    return 2;
                }
            };
            let (rows, secrets) = setup_profile::diff(&profile, &here);
            println!(
                "{}",
                setup_profile::format_diff(&rows, secrets, &profile.host, &profile.exported_at)
            );
            Some((profile, rows))
        }
    };

    if let Err(e) = fs::create_dir_all(sidecar()) {
        eprintln!("fno do wizard: cannot create {}: {e}", sidecar().display());
        return 2;
    }
    let _lock = match LockGuard::take(lock_path()) {
        Ok(l) => l,
        Err(()) => return 2,
    };

    let raw = io::stdout().is_terminal() && std::env::var_os("CI").is_none();
    let _raw_guard = RawGuard::enable_if(raw);
    let rid = setup_run::run_id();
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("fno"));
    write_report(&json!({
        "status": "running",
        "run_id": rid,
        "host": "wizard",
        "preset": preset_text,
        "join": join_file.is_some(),
        "skip_all": false,
        "screens": [],
        "checks": [],
        "next_action": serde_json::Value::Null,
        "files_written": [],
        "apply": serde_json::Value::Null,
    }));

    let mut state = State::new(preset, real_detect(), join_file.is_some());
    let mut pr = TermPrompter {
        raw,
        back_live: false,
        json,
    };
    let mut pb = RealProbes {
        exe: exe.clone(),
        raw,
        quiet: json,
    };
    let cancelled = loop {
        pr.back_live = state.back_allowed();
        match wizard::step(&mut state, &mut pr, &mut pb) {
            Next::Forward | Next::Back => continue,
            Next::Cancel => break true,
            Next::SkipToDone | Next::Done => break false,
        }
    };

    if cancelled {
        write_report(&json!({
            "status": "cancelled",
            "run_id": rid,
            "host": "wizard",
            "preset": preset_text,
            "join": join_file.is_some(),
            "skip_all": state.skip_all,
            "screens": screens_json(&state),
            "checks": checks_json(&state),
            "next_action": serde_json::Value::Null,
            "files_written": [],
            "apply": serde_json::Value::Null,
        }));
        setup_run::append_setup_event(
            "wizard",
            json!({
                "run_id": rid,
                "status": "cancelled",
                "preset": preset_text,
                "join": join_file.is_some(),
                "skip_all": state.skip_all,
                "screens": state.screens.len(),
                "checks": state.checks.len(),
            }),
        );
        if !json {
            println!();
            println!("Cancelled. The config is untouched.");
        }
        println!("wizard-report: {}", report_path().display());
        return 130;
    }

    // Apply through the run lanes. The join lane backs up and restores
    // inside `step_join`; on the wizard's own rows the host backs up the
    // global config first, because the wizard is the only writer here.
    let skip = state.skip_all;
    let rows = wizard::pending(&state);
    let mut apply_join = serde_json::Value::Null;
    let mut apply_wizard = serde_json::Value::Null;
    if !skip {
        if let Some((profile, diff_rows)) = &profile_join {
            apply_join = setup_run::apply_wizard_join(&exe, profile, diff_rows, &rid);
        }
        if !rows.is_empty() {
            let global = crate::config_defaults::global_config_path();
            match setup_profile::backup(&global) {
                Ok(Some(path)) => term_line(raw, &format!("backup: {}", path.display())),
                Ok(None) => {}
                Err(e) => term_line(raw, &format!("backup failed, continuing: {e}")),
            }
            apply_wizard =
                setup_run::apply_wizard_pending(&exe, &setup_run::repo_root(), &rid, &rows);
        }
    }

    let next_action = apply_wizard
        .get("next_action")
        .and_then(|v| v.as_str())
        .or_else(|| apply_join.get("next_action").and_then(|v| v.as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            if skip {
                "install or log in to one harness, then rerun: fno do wizard".to_string()
            } else {
                "/fno:target \"<your first feature>\"".to_string()
            }
        });

    let answers_file = answers_path();
    let mut files: Vec<String> = Vec::new();
    match fs::write(&answers_file, answers_toml(&state)) {
        Ok(()) => files.push(answers_file.display().to_string()),
        Err(e) => eprintln!(
            "fno do wizard: cannot write {}: {e}",
            answers_file.display()
        ),
    }
    let report_file = report_path();
    files.push(report_file.display().to_string());

    let report = setup_profile::redact(&json!({
        "status": "finished",
        "run_id": rid,
        "host": "wizard",
        "preset": preset_text,
        "join": join_file.is_some(),
        "skip_all": skip,
        "screens": screens_json(&state),
        "checks": checks_json(&state),
        "next_action": next_action.clone(),
        "files_written": files,
        "apply": { "join": apply_join, "wizard": apply_wizard },
    }));
    if let Err(e) = fs::write(
        &report_file,
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    ) {
        eprintln!("fno do wizard: cannot write {}: {e}", report_file.display());
    }

    setup_run::append_setup_event(
        "wizard",
        json!({
            "run_id": rid,
            "status": "finished",
            "preset": preset_text,
            "join": join_file.is_some(),
            "skip_all": skip,
            "screens": state.screens.len(),
            "checks": state.checks.len(),
        }),
    );

    if !json {
        println!();
        println!("Setup wizard finished.");
        println!("Next: {next_action}");
        println!("Report:  {}", report_file.display());
        println!("Answers: {}", answers_file.display());
    }
    println!("wizard-report: {}", report_file.display());
    0
}

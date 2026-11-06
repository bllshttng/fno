//! The buddy status line: the user's own status line on the left, the buddy
//! flush right. `plugins/buddy/hooks/statusline.sh` runs this on the ticks
//! where its cached output is stale, with `BUDDY_HOME` set to the state folder
//! the mod writes frames into. It reads only files the mod wrote there, so it
//! stays fast, and it always exits 0: a status line never blocks a session.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\]8;[^\x07\x1b]*(?:\x07|\x1b\\)").unwrap()
});
const MIN_ROWS: usize = 3;
const MAX_ROWS: usize = 6;
/// Under this many columns the original buddy showed a one-line face.
const NARROW: i64 = 60;
/// The original speech bubble held about 30 columns of text.
const BUBBLE_W: i64 = 30;
/// A frame older than this belongs to a session that stopped drawing.
const STALE_S: f64 = 45.0;
/// The user's own status line reruns at most this often unless its input
/// changes, or at its own refreshInterval when it has one.
const INNER_MAX_AGE_S: f64 = 30.0;
/// Claude Code trims a row's leading spaces; a braille blank holds the column.
const LEAD: &str = "\u{2800}";
const RESET: &str = "\x1b[0m";
/// The ANSI names Claude Code's -ansi themes use for the rarity colors. The
/// bare names are what a mod from before the theme table still writes.
const ANSI_NAMES: [(&str, u8); 12] = [
    ("blackBright", 90),
    ("white", 37),
    ("green", 32),
    ("greenBright", 92),
    ("blue", 34),
    ("blueBright", 94),
    ("magenta", 35),
    ("magentaBright", 95),
    ("yellow", 33),
    ("yellowBright", 93),
    ("gray", 90),
    ("cyan", 36),
];
/// East Asian Wide and Fullwidth ranges: the cells a terminal draws two wide.
const WIDE: [(u32, u32); 14] = [
    (0x1100, 0x115F),
    (0x231A, 0x231B),
    (0x2E80, 0x303E),
    (0x3041, 0x33FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xA000, 0xA4CF),
    (0xAC00, 0xD7A3),
    (0xF900, 0xFAFF),
    (0xFE30, 0xFE4F),
    (0xFF00, 0xFF60),
    (0xFFE0, 0xFFE6),
    (0x1F300, 0x1FAFF),
    (0x20000, 0x3FFFD),
];

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn cell(c: char) -> i64 {
    let u = c as u32;
    if WIDE.iter().any(|&(lo, hi)| (lo..=hi).contains(&u)) {
        2
    } else {
        1
    }
}

fn width(s: &str) -> i64 {
    ANSI.replace_all(s, "").chars().map(cell).sum()
}

fn spaces(n: i64) -> String {
    " ".repeat(n.max(0) as usize)
}

/// The longest prefix of `s` that fits `n` cells, its last char swapped for
/// an ellipsis when anything was cut.
fn cut(s: &str, n: i64) -> String {
    let (mut out, mut w) = (Vec::new(), 0);
    for c in s.chars() {
        let cw = cell(c);
        if w + cw > n {
            if out.is_empty() {
                return String::new();
            }
            out.pop();
            return out.into_iter().chain(['…']).collect();
        }
        out.push(c);
        w += cw;
    }
    out.into_iter().collect()
}

/// The escape for a frame color: rgb(r,g,b) as 24-bit, ansi:<name> in the
/// terminal's own palette, else dim gray.
fn sgr(color: &str) -> String {
    if let Some(inner) = color.strip_prefix("rgb(").and_then(|c| c.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        {
            return format!("\x1b[38;2;{}m", parts.join(";"));
        }
    }
    let name = color.strip_prefix("ansi:").unwrap_or(color);
    let code = ANSI_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(90, |(_, c)| *c);
    format!("\x1b[{code}m")
}

/// Greedy word wrap by terminal cells, so full-width text stays inside the bubble.
fn wrap(text: &str, n: i64) -> Vec<String> {
    let (mut lines, mut line) = (Vec::new(), String::new());
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        while width(&word) > n {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut head: Vec<char> = cut(&word, n + 1).chars().collect();
            head.pop();
            if head.is_empty() {
                head = word.chars().take(1).collect();
            }
            word = word.chars().skip(head.len()).collect();
            lines.push(head.into_iter().collect());
        }
        if !line.is_empty() && width(&line) + 1 + width(&word) > n {
            lines.push(std::mem::replace(&mut line, word));
        } else if line.is_empty() {
            line = word;
        } else {
            line = format!("{line} {word}");
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// A thought bubble over `room` rows, bottom-aligned, its trail of dots
/// pointing at the buddy. Three rows or more get the outline; fewer get the
/// bare words.
fn bubble(text: &str, room: usize, widest: i64) -> Vec<String> {
    // The outline needs its full width; a terminal too narrow for it gets the bare words.
    let framed = room >= 3 && widest >= BUBBLE_W + 4;
    let rows = if framed { room as i64 - 2 } else { room as i64 };
    if rows < 1 || text.trim().is_empty() {
        return vec![String::new(); room];
    }
    let rows = rows as usize;
    // Start at the original's 30 columns and widen until the words fit the rows beside the buddy.
    let mut w = BUBBLE_W.min(widest.max(8));
    let mut lines = wrap(text, w);
    while lines.len() > rows && w < widest {
        w = widest.min(w + 4);
        lines = wrap(text, w);
    }
    if lines.len() > rows {
        let tail = cut(&lines[rows - 1..].join(" "), w);
        lines.truncate(rows - 1);
        lines.push(tail);
    }
    let bw = lines.iter().map(|l| width(l)).max().unwrap_or(0);
    let mut body: Vec<String> = lines
        .iter()
        .map(|l| format!("{l}{}", spaces(bw - width(l))))
        .collect();
    if framed {
        let bar = "─".repeat((bw + 2) as usize);
        let mut boxed = vec![format!("╭{bar}╮")];
        boxed.extend(body.iter().map(|b| format!("│ {b} │")));
        boxed.push(format!("╰{bar}╯ ◦ ·"));
        let last = boxed.len() - 1;
        body = boxed
            .into_iter()
            .enumerate()
            .map(|(i, b)| if i == last { b } else { b + "    " })
            .collect();
    }
    let mut out = vec![String::new(); room.saturating_sub(body.len())];
    out.extend(body);
    out
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn str_at<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_str)
        .unwrap_or("")
}

struct Buddy {
    home: PathBuf,
    columns: String,
}

impl Buddy {
    fn frames(&self) -> PathBuf {
        self.home.join("frames")
    }

    fn inner_line(&self) -> Value {
        read_json(&self.home.join("inner.json"))
            .and_then(|v| v.get("statusLine").filter(|s| s.is_object()).cloned())
            .unwrap_or_else(|| json!({}))
    }

    fn inner_max_age(&self) -> f64 {
        match self
            .inner_line()
            .get("refreshInterval")
            .and_then(Value::as_f64)
        {
            Some(every) if every >= 1.0 => every,
            _ => INNER_MAX_AGE_S,
        }
    }

    /// The payload minus the clocks Claude Code advances on every tick, plus
    /// the width the rows were sized for.
    fn inner_key(&self, data: &Value) -> String {
        let mut stable = data.as_object().cloned().unwrap_or_default();
        let cost: Map<String, Value> = data
            .get("cost")
            .and_then(Value::as_object)
            .map(|c| {
                c.iter()
                    .filter(|(k, _)| !k.ends_with("duration_ms"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        stable.insert("cost".into(), Value::Object(cost));
        stable.insert("columns".into(), Value::String(self.columns.clone()));
        let sorted: std::collections::BTreeMap<String, Value> = stable.into_iter().collect();
        serde_json::to_string(&sorted).unwrap_or_default()
    }

    fn inner_rows(&self, stdin: &str, data: &Value) -> Vec<String> {
        let inner = self.inner_line();
        let command = str_at(&inner, &["command"]);
        if str_at(&inner, &["type"]) != "command" || command.is_empty() {
            return vec![default_left(data)];
        }
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", command]);
        let out = crate::bounded_cmd::output_with_timeout_stdin(cmd, 5, stdin)
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if out.trim().is_empty() {
            return Vec::new();
        }
        out.trim_end_matches('\n')
            .split('\n')
            .map(str::to_string)
            .collect()
    }

    /// The user's status line reruns on a real session change or once its
    /// rows age out; other ticks reuse them. The age bound keeps a clock or a
    /// background git change from going stale for good. A failed run never
    /// replaces good rows: the last good rows show and the next tick retries.
    fn cached_inner_rows(&self, stdin: &str, data: &Value, session: &str) -> Vec<String> {
        if session.is_empty() {
            return self.inner_rows(stdin, data);
        }
        let path = self.frames().join(format!("{session}.inner.json"));
        let key = self.inner_key(data);
        let cached = read_json(&path).unwrap_or(Value::Null);
        let rows_of = |v: &Value| -> Option<Vec<String>> {
            v.get("rows")?
                .as_array()?
                .iter()
                .map(|r| r.as_str().map(str::to_string))
                .collect()
        };
        let at = cached.get("at").and_then(Value::as_f64).unwrap_or(0.0);
        if cached.get("key").and_then(Value::as_str) == Some(key.as_str())
            && now() - at < self.inner_max_age()
        {
            if let Some(rows) = rows_of(&cached) {
                return rows;
            }
        }
        let rows = self.inner_rows(stdin, data);
        if rows.is_empty() {
            return rows_of(&cached).unwrap_or(rows);
        }
        let _ = std::fs::write(
            &path,
            json!({"key": key, "at": now(), "rows": rows}).to_string(),
        );
        rows
    }

    fn read_frame(&self, session: &str) -> Option<Value> {
        let frame = read_json(&self.frames().join(format!("{session}.json")))?;
        let at = frame.get("at").and_then(Value::as_f64).unwrap_or(0.0);
        (now() - at / 1000.0 <= STALE_S).then_some(frame)
    }

    /// The heartbeat tells the mod this session's status line runs the
    /// wrapper, whichever settings file set it. A session's first run sweeps
    /// files that no session has touched for a day.
    fn heartbeat(&self, session: &str) {
        let frames = self.frames();
        if std::fs::create_dir_all(&frames).is_err() {
            return;
        }
        let seen = frames.join(format!("{session}.seen"));
        if !seen.exists() {
            let cutoff = SystemTime::now() - std::time::Duration::from_secs(86_400);
            for entry in std::fs::read_dir(&frames).into_iter().flatten().flatten() {
                if entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t < cutoff)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let _ = std::fs::write(&seen, ((now() * 1000.0) as u64).to_string());
    }
}

/// A plain left side for someone who had no status line.
fn default_left(data: &Value) -> String {
    let mut parts = vec![str_at(data, &["model", "display_name"]).to_string()];
    let cwd = Some(str_at(data, &["workspace", "current_dir"]))
        .filter(|c| !c.is_empty())
        .unwrap_or(str_at(data, &["cwd"]));
    if !cwd.is_empty() {
        parts.push(
            Path::new(cwd)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    if let Some(used) = data
        .get("context_window")
        .and_then(|c| c.get("used_percentage"))
        .and_then(Value::as_f64)
    {
        parts.push(format!("ctx {}%", used.round_ties_even()));
    }
    if let Some(cost) = data
        .get("cost")
        .and_then(|c| c.get("total_cost_usd"))
        .and_then(Value::as_f64)
        .filter(|c| *c != 0.0)
    {
        parts.push(format!("${cost:.2}"));
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

/// Python's str.center: the odd pad cell goes left only when both the pad and the width are odd.
fn center(s: &str, w: usize) -> String {
    let len = s.chars().count();
    if w <= len {
        return s.to_string();
    }
    let marg = w - len;
    let left = marg / 2 + (marg & w & 1);
    format!("{}{s}{}", " ".repeat(left), " ".repeat(marg - left))
}

/// The rows to print. Inner text stays on top; the buddy stands on the bottom row.
fn layout(left: Vec<String>, frame: Option<&Value>, cols: i64) -> Vec<String> {
    let Some(frame) = frame else { return left };
    if cols + 4 < NARROW {
        return face_row(left, frame, cols);
    }
    let color = sgr(str_at(frame, &["color"]));
    let mut art: Vec<String> = frame
        .get("sprite")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(Value::as_str)
                .map(|r| r.trim_end().to_string())
                .collect()
        })
        .unwrap_or_default();
    while art.first().is_some_and(|a| a.trim().is_empty()) {
        art.remove(0);
    }
    let name = str_at(frame, &["name"]);
    // Like the original: the full sprite with its name row below, at 60 columns or more.
    let aw = art
        .iter()
        .map(|a| width(a))
        .chain([name.chars().count() as i64])
        .max()
        .unwrap_or(0);
    art.push(center(name, aw as usize).trim_end().to_string());
    // Speech wraps beside the body like the original bubble, bottom-aligned;
    // the fleet line sits beside the name row. The bubble can take the whole
    // width beside the buddy, over the user's rows, while it shows.
    let mut labels = vec![String::new(); art.len()];
    *labels.last_mut().unwrap() = str_at(frame, &["fleet"]).to_string();
    let widest = (BUBBLE_W * 2).min(cols - aw - 12);
    for (i, line) in bubble(str_at(frame, &["speech"]), art.len() - 1, widest)
        .into_iter()
        .enumerate()
    {
        labels[i] = line;
    }
    let least = left.len().max(art.len()).max(MIN_ROWS);
    if least > MAX_ROWS {
        return face_row(left, frame, cols);
    }
    // Prefer a row count where the user's rows fit whole; else keep the full buddy and cut the rows beside it.
    let tries: Vec<usize> = (least..=MAX_ROWS).chain([least]).collect();
    for (n, &rows) in tries.iter().enumerate() {
        let mut lefts = left.clone();
        lefts.resize(rows.max(left.len()), String::new());
        let top = rows - art.len();
        let fits = (0..art.len()).all(|i| width(&lefts[top + i]) + aw + 2 <= cols);
        if n + 1 < tries.len() && !fits {
            continue;
        }
        let mut out = lefts[..top].to_vec();
        for (i, a) in art.iter().enumerate() {
            let mut l = if lefts[top + i].is_empty() {
                LEAD.to_string()
            } else {
                lefts[top + i].clone()
            };
            let label = cut(&labels[i], cols - aw - 4);
            // Only the buddy wears its rarity color; its words use the terminal's own text color.
            let words = if label.is_empty() {
                String::new()
            } else {
                format!("{label} ")
            };
            // While the buddy talks, its bubble covers the user's row; the row comes back when it fades.
            let room = cols - width(&words) - aw - 1;
            if width(&l) > room {
                l = if room > 1 {
                    cut(&ANSI.replace_all(&l, ""), room)
                } else {
                    LEAD.to_string()
                };
            }
            let pad = cols - width(&l) - width(&words) - aw;
            let ljust = spaces(aw - a.chars().count() as i64);
            out.push(format!("{l}{}{words}{color}{a}{ljust}{RESET}", spaces(pad)));
        }
        return out;
    }
    face_row(left, frame, cols)
}

/// The one-line face on the lowest row that has room.
fn face_row(left: Vec<String>, frame: &Value, cols: i64) -> Vec<String> {
    let color = sgr(str_at(frame, &["color"]));
    let (glyph, name, speech) = (
        str_at(frame, &["face"]),
        str_at(frame, &["name"]),
        str_at(frame, &["speech"]),
    );
    let mut face = format!("{glyph} {name}");
    if !speech.is_empty() {
        face = format!("{face}: {speech}");
    }
    let mut lefts = left.clone();
    if lefts.len() < MAX_ROWS {
        lefts.push(String::new());
    }
    for i in (0..lefts.len()).rev() {
        let l = if lefts[i].is_empty() {
            LEAD.to_string()
        } else {
            lefts[i].clone()
        };
        let free = cols - width(&l) - 2;
        if free >= width(glyph) + 1 {
            let right = cut(&face, free);
            let head = cut(&format!("{glyph} {name}"), free);
            let rest: String = right.chars().skip(head.chars().count()).collect();
            lefts[i] = format!(
                "{l}{}{color}{head}{RESET}{rest}",
                spaces(cols - width(&l) - width(&right))
            );
            if lefts.last().is_some_and(String::is_empty) {
                lefts.pop();
            }
            return lefts;
        }
    }
    left
}

pub fn run(_args: &[String]) -> i32 {
    let mut stdin = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin);
    let data: Value = serde_json::from_str(&stdin).unwrap_or_else(|_| json!({}));
    let Some(home) = std::env::var_os("BUDDY_HOME").map(PathBuf::from) else {
        return 0;
    };
    let buddy = Buddy {
        home,
        columns: std::env::var("COLUMNS").unwrap_or_default(),
    };
    let session = str_at(&data, &["session_id"]);
    if !session.is_empty() {
        buddy.heartbeat(session);
    }
    let left = buddy.cached_inner_rows(&stdin, &data, session);
    // Claude Code's usable status width runs a few columns under COLUMNS.
    let cols = buddy.columns.parse::<i64>().unwrap_or(120) - 4;
    let frame = if session.is_empty() {
        None
    } else {
        buddy.read_frame(session)
    };
    let out = layout(left, frame.as_ref(), cols).join("\n") + "\n";
    print!("{out}");
    // statusline.sh prints this again, with no process start, until the input
    // or the width changes or a new frame lands. The key, a NUL, then the
    // output, swapped in whole so a reader never sees half of it.
    if let (false, Some(key)) = (session.is_empty(), std::env::var_os("BUDDY_KEY")) {
        use std::os::unix::ffi::OsStrExt as _;
        let cache = buddy.frames().join(format!("{session}.cache"));
        let tmp = cache.with_extension(format!("cache.{}", std::process::id()));
        let mut bytes = key.as_bytes().to_vec();
        bytes.push(0);
        bytes.extend_from_slice(out.as_bytes());
        if std::fs::write(&tmp, bytes).is_err() || std::fs::rename(&tmp, &cache).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    0
}

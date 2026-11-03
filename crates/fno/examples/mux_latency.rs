//! Keypress-to-echo latency through the mux, measured at the host pty.
//!
//! `mux_latency` plays the host terminal: it owns a pty, writes one key, and
//! timestamps the echo coming back. Ghostty's paint is on both sides of the
//! comparison, so the difference between scenarios is what the mux adds.
//!
//! Run a release build against a release `fno` when the machine is calm:
//! `cargo run --release -p fno --example mux_latency -- --fno <release fno>`
//! Flags: `--samples N` (default 2000), `--stages` (per-stage cost only),
//! `--scenario B,C,D,E` (default all). A and the control always run: they prove
//! the clock.
//!
//! Exit codes: 0 measured, 1 harness failure, 2 the 10 ms control missed, so
//! the run is void and prints no mux numbers.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fno::proto::{self, ServerMsg};
use fno::vt::Pane;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

const CONTROL_DELAY_MS: u64 = 10;
const CONTROL_TOLERANCE_MS: f64 = 2.0;
/// More lost samples than this means the numbers describe a broken run.
const MAX_LOST_FRACTION: f64 = 0.01;
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(2);
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Clear often enough that the echo never scrolls: a scroll repaints every row
/// and would measure a redraw, not a keypress.
const CLEAR_EVERY: usize = 150;

/// Where a scenario's `yes` runs. A hidden flood costs no frames, because the
/// server builds none for a pane nobody views, so it isolates the core's feed
/// and drain from the frame path.
#[derive(Clone, Copy)]
enum Flood {
    None,
    Sibling,
    HiddenTab,
}

/// What the reader thread looks for, and when it first saw it.
#[derive(Default)]
struct Watch {
    needle: Vec<u8>,
    seen: Vec<u8>,
    hit: Option<Instant>,
    closed: bool,
}

struct Host {
    watch: Arc<(Mutex<Watch>, Condvar)>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
}

impl Host {
    fn spawn(cmd: CommandBuilder, rows: u16, cols: u16) -> Result<Host, String> {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty: {e}"))?;
        let child = pty
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn: {e}"))?;
        drop(pty.slave);
        let writer = pty.master.take_writer().map_err(|e| e.to_string())?;
        let mut reader = pty.master.try_clone_reader().map_err(|e| e.to_string())?;
        let watch = Arc::new((Mutex::new(Watch::default()), Condvar::new()));
        let shared = watch.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let now = Instant::now();
                let (lock, cv) = &*shared;
                let mut w = lock.lock().unwrap();
                if w.hit.is_some() {
                    continue;
                }
                if w.needle.is_empty() {
                    // Nothing asked yet: hold startup output for `wait_for`.
                    w.seen.extend_from_slice(&buf[..n]);
                    let over = w.seen.len().saturating_sub(buf.len());
                    w.seen.drain(..over);
                    continue;
                }
                // Keep only what a needle split across two reads could need,
                // so a flooding sibling pane costs this thread nothing.
                let keep = w.seen.len().min(w.needle.len());
                let start = w.seen.len() - keep;
                w.seen.drain(..start);
                w.seen.extend_from_slice(&buf[..n]);
                let needle = w.needle.clone();
                if w.seen.windows(needle.len()).any(|win| win == needle) {
                    w.hit = Some(now);
                    cv.notify_all();
                }
            }
            let (lock, cv) = &*shared;
            lock.lock().unwrap().closed = true;
            cv.notify_all();
        });
        Ok(Host {
            watch,
            writer,
            child,
            _master: pty.master,
        })
    }

    /// Wait for startup output that may have landed before this call.
    fn wait_for(&mut self, needle: &[u8], limit: Duration) -> bool {
        let start = Instant::now();
        let (lock, cv) = &*self.watch.clone();
        let mut w = lock.lock().unwrap();
        w.needle = needle.to_vec();
        loop {
            if w.hit.is_some() || w.seen.windows(needle.len()).any(|win| win == needle) {
                return true;
            }
            let Some(left) = limit.checked_sub(start.elapsed()) else {
                return false;
            };
            if w.closed {
                return false;
            }
            w = cv.wait_timeout(w, left).unwrap().0;
        }
    }

    /// Write `bytes`, return how long `needle` took to come back.
    fn round_trip(&mut self, bytes: &[u8], needle: &[u8], limit: Duration) -> Option<Duration> {
        let (lock, cv) = &*self.watch.clone();
        {
            let mut w = lock.lock().unwrap();
            w.needle = needle.to_vec();
            w.seen.clear();
            w.hit = None;
        }
        let start = Instant::now();
        self.writer.write_all(bytes).ok()?;
        self.writer.flush().ok()?;
        let mut w = lock.lock().unwrap();
        while w.hit.is_none() && !w.closed {
            let left = limit.checked_sub(start.elapsed())?;
            let (guard, timeout) = cv.wait_timeout(w, left).unwrap();
            w = guard;
            if timeout.timed_out() && w.hit.is_none() {
                return None;
            }
        }
        w.hit.map(|hit| hit.duration_since(start))
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A scratch mux: its own dir, socket and home, never the live fleet.
struct IsolatedMux {
    dir: std::path::PathBuf,
    bin: String,
}

impl IsolatedMux {
    fn new(tag: &str, bin: &str) -> Result<IsolatedMux, String> {
        // A short path: the session socket lives here and sun_path is small.
        let dir = std::path::PathBuf::from(format!("/tmp/fml-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(dir.join("home")).map_err(|e| e.to_string())?;
        // zsh stops to ask for setup when a home has no rc file.
        std::fs::write(dir.join("home/.zshrc"), "PS1='$ '\n").map_err(|e| e.to_string())?;
        Ok(IsolatedMux {
            dir,
            bin: bin.to_string(),
        })
    }

    fn env(&self) -> Vec<(String, String)> {
        let d = |leaf: &str| self.dir.join(leaf).to_string_lossy().into_owned();
        let tag = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        vec![
            ("HOME".into(), d("home")),
            ("USERPROFILE".into(), d("home")),
            ("FNO_MUX_DIR".into(), d("")),
            ("FNO_AGENTS_HOME".into(), d("iso-agents")),
            ("FNO_CLAUDE_DAEMON_DIR".into(), d("iso-daemon")),
            ("FNO_GRAPH_JSON".into(), d("iso-graph.json")),
            ("FNO_CLAIMS_ROOT".into(), d("home")),
            (
                "FNO_GLOBAL_SETTINGS_PATH".into(),
                d("iso-cfg/settings.json"),
            ),
            ("FNO_MUX_ADMISSION_NAMESPACE".into(), tag),
            ("FNO_PROCESS_ADMISSION_MAX".into(), "512".into()),
            ("FNO_E2E".into(), "1".into()),
            ("FNO_BIN".into(), self.bin.clone()),
            // A real login shell, so the pane takes the path a user's pane takes.
            ("SHELL".into(), "/bin/zsh".into()),
            ("TERM".into(), "xterm-256color".into()),
            // The harness answers no terminal query; a probe would stall attach.
            ("FNO_MUX_IMAGE_PROTOCOL".into(), "halfblocks".into()),
        ]
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        for (key, _) in std::env::vars() {
            if key.starts_with("FNO_") {
                cmd.env_remove(key);
            }
        }
        cmd.args(args).envs(self.env()).stdin(Stdio::null());
        cmd
    }

    fn output(&self, args: &[&str]) -> Result<String, String> {
        let out = self
            .command(args)
            .output()
            .map_err(|e| format!("fno {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "fno {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn attach(&self, rows: u16, cols: u16) -> Result<Host, String> {
        let mut cmd = CommandBuilder::new(&self.bin);
        for (key, _) in std::env::vars() {
            if key.starts_with("FNO_") {
                cmd.env_remove(key);
            }
        }
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        Host::spawn(cmd, rows, cols)
    }

    /// Start `yes` beside the first pane, or in a new tab nobody views.
    fn start_flood(&self, flood: Flood) -> Result<(), String> {
        if matches!(flood, Flood::None) {
            return Ok(());
        }
        let listed = self.output(&["mux", "pane", "ls", "--json"])?;
        let panes: serde_json::Value =
            serde_json::from_str(&listed).map_err(|e| format!("pane ls: {e}: {listed}"))?;
        let first = panes
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("pane_id").or_else(|| row.get("id")))
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| format!("pane ls names no pane: {listed}"))?
            .to_string();
        let flooding = match flood {
            Flood::HiddenTab => self.output(&["mux", "tab", "create"])?,
            _ => self.output(&["mux", "pane", "split", &first, "--direction", "right"])?,
        };
        self.output(&["mux", "pane", "send", &flooding, "--text", "yes\r", "--raw"])?;
        Ok(())
    }
}

impl Drop for IsolatedMux {
    fn drop(&mut self) {
        let _ = self
            .command(&["mux", "kill-server", "main", "--end-unkept"])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A small xorshift: the gaps only need to avoid a fixed beat with any timer.
struct Gaps(u64);

impl Gaps {
    fn new() -> Gaps {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Gaps(seed | 1)
    }

    fn next(&mut self) -> Duration {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        Duration::from_millis(20 + self.0 % 41)
    }
}

struct Stats {
    p50: f64,
    p95: f64,
    p99: f64,
    lost: usize,
    taken: usize,
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[index]
}

/// Each key is a different Hangul syllable, so its bytes appear in the output
/// only when this key's echo lands. The client redraws whole changed rows, so a
/// repeated letter would match an old key or a neighbouring pane's text.
fn measure(host: &mut Host, samples: usize) -> Result<Stats, String> {
    let mut gaps = Gaps::new();
    let mut taken = Vec::with_capacity(samples);
    let mut lost = 0;
    for i in 0..samples {
        if i > 0 && i % CLEAR_EVERY == 0 {
            host.writer
                .write_all(b"\x1b[2J\x1b[H")
                .map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(100));
        }
        let ch = char::from_u32(0xAC00 + i as u32).ok_or("sample count over the alphabet")?;
        let mut key = [0u8; 4];
        let key = ch.encode_utf8(&mut key).as_bytes().to_vec();
        match host.round_trip(&key, &key, SAMPLE_TIMEOUT) {
            Some(took) => taken.push(took.as_secs_f64() * 1000.0),
            None => lost += 1,
        }
        std::thread::sleep(gaps.next());
    }
    if taken.is_empty() {
        return Err("every sample was lost".into());
    }
    let lost_fraction = lost as f64 / samples as f64;
    if lost_fraction > MAX_LOST_FRACTION {
        return Err(format!("{lost} of {samples} samples lost"));
    }
    taken.sort_by(f64::total_cmp);
    Ok(Stats {
        p50: percentile(&taken, 0.50),
        p95: percentile(&taken, 0.95),
        p99: percentile(&taken, 0.99),
        lost,
        taken: taken.len(),
    })
}

fn report(name: &str, s: &Stats) {
    println!(
        "{name}: p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} samples={} lost={}",
        s.p50, s.p95, s.p99, s.taken, s.lost
    );
}

fn report_over_alone(name: &str, flood: &Stats, alone: Option<&Stats>) {
    if let Some(c) = alone {
        println!(
            "{name}: p50_ms={:.2} p95_ms={:.2} p99_ms={:.2}",
            flood.p50 - c.p50,
            flood.p95 - c.p95,
            flood.p99 - c.p99
        );
    }
}

fn bare_echo(argv: &str, rows: u16, cols: u16, samples: usize) -> Result<Stats, String> {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(argv);
    let mut host = Host::spawn(cmd, rows, cols)?;
    if !host.wait_for(b"lat-ready", READY_TIMEOUT) {
        return Err("bare echo never came ready".into());
    }
    measure(&mut host, samples)
}

fn through_mux(
    tag: &str,
    bin: &str,
    rows: u16,
    cols: u16,
    flood: Flood,
    samples: usize,
) -> Result<Stats, String> {
    let mux = IsolatedMux::new(tag, bin)?;
    let mut host = mux.attach(rows, cols)?;
    // The attach makes the server, so the first ask may find no socket yet.
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let listed = mux.output(&["mux", "pane", "ls", "--json"]);
        if let Ok(rows) = &listed {
            if rows != "[]" {
                break;
            }
        }
        if Instant::now() > deadline {
            return Err("the isolated server never answered".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    mux.start_flood(flood)?;
    // The printed word differs from the typed one, so the shell's echo of this
    // line cannot satisfy the wait.
    let setup = b"printf 'lat-%s\\n' ready; stty raw -echo; exec cat\r";
    host.round_trip(setup, b"lat-ready", READY_TIMEOUT)
        .ok_or("the mux pane never came ready")?;
    // `lat-ready` prints before stty runs; let exec land before the first key.
    std::thread::sleep(Duration::from_millis(500));
    measure(&mut host, samples)
}

/// One frame's cost at each stage a keypress pays on the wire path. Compose and
/// the client's row diff are `pub(super)`, so they stay in the end-to-end number.
fn stages() -> Result<(), String> {
    const ITERATIONS: usize = 500;
    let (rows, cols) = (50u16, 200u16);
    let mut pane = Pane::new(rows, cols);
    let blank = pane.frame();
    let blank_bytes = proto::encode(&ServerMsg::Frame {
        pane_id: 1,
        frame: blank,
    })
    .map_err(|e| e.to_string())?
    .len();
    for row in 0..rows {
        let line = format!("{row:>4} the quick brown fox jumps over the lazy dog\r\n");
        pane.feed(line.as_bytes());
    }
    let mut times: [Vec<f64>; 5] = Default::default();
    let mut encoded_bytes = 0;
    for i in 0..ITERATIONS {
        let key = [b'a' + (i % 26) as u8];
        let t = Instant::now();
        pane.feed(&key);
        times[0].push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let frame = pane.frame();
        times[1].push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let copy = frame.clone();
        times[2].push(t.elapsed().as_secs_f64() * 1e3);
        let msg = ServerMsg::Frame {
            pane_id: 1,
            frame: copy,
        };
        let t = Instant::now();
        let wire = proto::encode(&msg).map_err(|e| e.to_string())?;
        times[3].push(t.elapsed().as_secs_f64() * 1e3);
        encoded_bytes = wire.len();
        let t = Instant::now();
        let decoded: ServerMsg = serde_json::from_slice(&wire[4..]).map_err(|e| e.to_string())?;
        times[4].push(t.elapsed().as_secs_f64() * 1e3);
        std::hint::black_box(decoded);
    }
    println!("stage grid: {rows}x{cols} iterations={ITERATIONS}");
    println!("stage bytes: blank_frame={blank_bytes} text_frame={encoded_bytes}");
    for (name, samples) in ["feed", "frame", "clone", "encode", "decode"]
        .iter()
        .zip(times.iter_mut())
    {
        samples.sort_by(f64::total_cmp);
        println!(
            "stage {name}: p50_ms={:.3} p95_ms={:.3}",
            percentile(samples, 0.50),
            percentile(samples, 0.95)
        );
    }
    flood_stage();
    Ok(())
}

/// What one full pane-output queue costs the core loop when a sibling floods.
/// The queue holds 256 chunks of 8 KB at most, shared by every pane, so a key's
/// echo can wait behind all of them.
fn flood_stage() {
    const QUEUE_CHUNKS: usize = 256;
    let chunk = b"y\n".repeat(4096);
    let mut pane = Pane::new(50, 100);
    for pass in 0..40 {
        let start = Instant::now();
        for _ in 0..QUEUE_CHUNKS {
            pane.feed(&chunk);
        }
        if pass == 0 || pass == 39 {
            println!(
                "stage flood_queue pass {pass}: feed_ms={:.2} chunks={QUEUE_CHUNKS}",
                start.elapsed().as_secs_f64() * 1e3
            );
        }
    }
}

/// The control's echo program: raw stdin to stdout, a spun delay in between.
/// A sleep overshoots on a busy Mac, and the control must not carry that noise.
fn echo_delay(delay_ms: u64) -> Result<(), String> {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    println!("lat-ready");
    stdout.flush().map_err(|e| e.to_string())?;
    let mut buf = [0u8; 256];
    loop {
        let n = stdin.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(delay_ms) {
            std::hint::spin_loop();
        }
        stdout.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        stdout.flush().map_err(|e| e.to_string())?;
    }
}

fn run() -> Result<i32, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(at) = args.iter().position(|a| a == "--echo-delay-ms") {
        let ms = args
            .get(at + 1)
            .and_then(|v| v.parse().ok())
            .ok_or("--echo-delay-ms needs a number")?;
        echo_delay(ms)?;
        return Ok(0);
    }
    if args.iter().any(|a| a == "--stages") {
        stages()?;
        return Ok(0);
    }
    let samples = match args.iter().position(|a| a == "--samples") {
        Some(at) => args
            .get(at + 1)
            .and_then(|v| v.parse().ok())
            .ok_or("--samples needs a number")?,
        None => 2000,
    };
    let bin = match args.iter().position(|a| a == "--fno") {
        Some(at) => args.get(at + 1).ok_or("--fno needs a path")?.clone(),
        None => "fno".to_string(),
    };
    let scenarios: Vec<String> = match args.iter().position(|a| a == "--scenario") {
        Some(at) => args
            .get(at + 1)
            .ok_or("--scenario needs a list")?
            .split(',')
            .map(str::to_string)
            .collect(),
        None => ["B", "C", "D", "E"].map(str::to_string).to_vec(),
    };
    if let Some(bad) = scenarios
        .iter()
        .find(|s| !["A", "B", "C", "D", "E"].contains(&s.as_str()))
    {
        return Err(format!("unknown scenario {bad}: want A, B, C, D or E"));
    }
    println!("binary: {bin}");

    let bare = "stty raw -echo; printf 'lat-%s\\n' ready; exec cat";
    let a = bare_echo(bare, 50, 200, samples)?;
    report("A bare", &a);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let control = format!(
        "stty raw -echo; exec '{}' --echo-delay-ms {CONTROL_DELAY_MS}",
        exe.display()
    );
    let c = bare_echo(&control, 50, 200, samples)?;
    report("control bare+10ms", &c);
    let added = c.p95 - a.p95;
    if (added - CONTROL_DELAY_MS as f64).abs() > CONTROL_TOLERANCE_MS {
        eprintln!(
            "control void: read {added:.2} ms above bare, want {CONTROL_DELAY_MS} within \
             {CONTROL_TOLERANCE_MS}. The machine is too noisy to measure."
        );
        return Ok(2);
    }

    let wanted = |name: &str| scenarios.iter().any(|s| s == name);
    if wanted("B") {
        let b = through_mux("b", &bin, 24, 80, Flood::None, samples)?;
        report("B mux 80x24", &b);
    }
    let mux_c = if wanted("C") {
        let s = through_mux("c", &bin, 50, 200, Flood::None, samples)?;
        report("C mux 200x50", &s);
        println!("mux_added_p95_ms: {:.2}", s.p95 - a.p95);
        Some(s)
    } else {
        None
    };
    if wanted("D") {
        let d = through_mux("d", &bin, 50, 200, Flood::Sibling, samples)?;
        report("D mux 200x50 + yes sibling", &d);
        println!("flood_added_p95_ms: {:.2}", d.p95 - a.p95);
        report_over_alone("D-C", &d, mux_c.as_ref());
    }
    if wanted("E") {
        let e = through_mux("e", &bin, 50, 200, Flood::HiddenTab, samples)?;
        report("E mux 200x50 + yes in a hidden tab", &e);
        report_over_alone("E-C", &e, mux_c.as_ref());
    }
    Ok(0)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("mux_latency: {error}");
            std::process::exit(1);
        }
    }
}

//! The store keeper's socket lifecycle: serving, shutdown mid-commit,
//! concurrent writers, the double-keeper refusal and the ready board. The protocol
//! shape tests mirror keeper_survival.rs (the pane keeper's own).

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WORKER_BIN: &str = env!("CARGO_BIN_EXE_fno-agents-worker");

const TAG_REQUEST: u8 = 1;
const TAG_SHUTDOWN: u8 = 2;
const TAG_IDENTIFY: u8 = 3;
const TAG_RESPONSE: u8 = 4;
const TAG_IDENTIFY_REPLY: u8 = 5;

struct Keeper {
    child: Child,
}

impl Drop for Keeper {
    fn drop(&mut self) {
        // Group kill, the fn-47ea driver pattern: the store keeper leads its
        // own process group, so the kill takes its descendants.
        let pid = self.child.id();
        let _ = self.child.kill();
        // SAFETY: SIGKILL to the group of a keeper this test spawned.
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

fn seed_empty_store(graph: &Path) {
    fno_agents::graph_store::seed_rows(graph, &[]).unwrap();
}

fn short_home(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("fno-gsk{}_{}_{}", std::process::id(), tag, n));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_keeper(tag: &str, graph: &Path, sock: &Path) -> Keeper {
    let child = Command::new(WORKER_BIN)
        .args([
            "--store-keeper",
            "--sock",
            sock.to_str().unwrap(),
            "--graph",
            graph.to_str().unwrap(),
            "--session",
            tag,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn store keeper");
    Keeper { child }
}

/// Like [`spawn_keeper`], plus one env var the keeper process reads at
/// startup (the claims-root override the refusal test needs).
fn spawn_keeper_with_env(tag: &str, graph: &Path, sock: &Path, key: &str, val: &str) -> Keeper {
    let child = Command::new(WORKER_BIN)
        .args([
            "--store-keeper",
            "--sock",
            sock.to_str().unwrap(),
            "--graph",
            graph.to_str().unwrap(),
            "--session",
            tag,
        ])
        .env(key, val)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn store keeper");
    Keeper { child }
}

fn wait_for_socket(sock: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if UnixStream::connect(sock).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "store socket never appeared: {}",
            sock.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn write_frame(stream: &mut UnixStream, tag: u8, payload: &[u8]) {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(tag);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame).unwrap();
    stream.flush().unwrap();
}

fn read_frame(stream: &mut UnixStream) -> Option<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    stream.read_exact(&mut header).ok()?;
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).ok()?;
    Some((header[0], payload))
}

fn rpc(stream: &mut UnixStream, id: u64, method: &str, params: Value) -> Value {
    let req = json!({"id": id, "method": method, "params": params});
    write_frame(
        stream,
        TAG_REQUEST,
        serde_json::to_vec(&req).unwrap().as_slice(),
    );
    let (tag, payload) = read_frame(stream).expect("a response frame");
    assert_eq!(tag, TAG_RESPONSE, "responses ride the response tag");
    let v: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(v.get("id"), Some(&json!(id)), "reply correlates");
    v
}

fn ok_result(reply: Value) -> Value {
    assert_eq!(
        reply.get("ok"),
        Some(&json!(true)),
        "rpc must succeed: {reply}"
    );
    reply["result"].clone()
}

#[test]
fn keeper_serves_reads_ops_and_shutdown_over_its_socket() {
    let home = short_home("serve");
    let graph = home.join("graph.json");
    seed_empty_store(&graph);
    let sock = home.join("graph.json.store.sock");
    let mut keeper = spawn_keeper("serve-test", &graph, &sock);
    wait_for_socket(&sock);

    let mut stream = UnixStream::connect(&sock).unwrap();

    // Identify carries the protocol version.
    write_frame(&mut stream, TAG_IDENTIFY, &[]);
    let (tag, payload) = read_frame(&mut stream).unwrap();
    assert_eq!(tag, TAG_IDENTIFY_REPLY);
    let id: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(id["v"], 1, "protocol version rides Identify");
    assert_eq!(id["graph"], graph.display().to_string());

    // A read of an empty graph returns entries, never a refusal.
    let result = ok_result(rpc(&mut stream, 1, "read", json!({})));
    assert_eq!(result["entries"].as_array().unwrap().len(), 0);

    // An op against an absent node answers found=false, never an error.
    let result = ok_result(rpc(
        &mut stream,
        2,
        "op",
        json!({
            "name": "append_progress_note",
            "params": {"node_id": "ab-missing", "note": {"ts": "t", "text": "x"}}
        }),
    ));
    assert_eq!(result["op"]["found"], false);
    drop(stream);

    // Explicit shutdown: ack, exit, unlink.
    let mut stream = UnixStream::connect(&sock).unwrap();
    write_frame(&mut stream, TAG_SHUTDOWN, &[]);
    let (tag, payload) = read_frame(&mut stream).unwrap();
    assert_eq!(tag, TAG_RESPONSE);
    let reply: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(reply["result"], "shutdown");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match keeper.child.try_wait().expect("reap check") {
            Some(_) => break,
            None => {
                assert!(Instant::now() < deadline, "keeper did not exit on shutdown");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    assert!(!sock.exists(), "an explicit shutdown unlinks its socket");
}

// x-f188 change 2: one store keeper per socket.
// ---------------------------------------------------------------

#[test]
fn concurrent_spawns_settle_on_one_keeper_and_losers_exit_three() {
    // AC2-HP: four keepers race for one graph's socket; one seat wins and
    // the other three exit 3 rather than each binding a second socket object
    // onto an unlinked path. Measured 2026-09-10: three losers were still
    // running on one socket, each holding a parsed 15MB graph.
    let home = short_home("seat");
    let graph = home.join("graph.json");
    seed_empty_store(&graph);
    let sock = home.join("graph.json.store.sock");
    // Each racer's stderr lands in its own file: an unexpected exit names its
    // path (seat refusal, self-retire, bind failure) instead of a bare code.
    let stderr_of = |i: usize| home.join(format!("racer-{i}.stderr"));
    let mut keepers: Vec<Child> = (0..4)
        .map(|i| {
            Command::new(WORKER_BIN)
                .args([
                    "--store-keeper",
                    "--sock",
                    sock.to_str().unwrap(),
                    "--graph",
                    graph.to_str().unwrap(),
                    "--session",
                    "seat-race",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::from(std::fs::File::create(stderr_of(i)).unwrap()))
                .spawn()
                .expect("spawn racing keeper")
        })
        .collect();
    let explain =
        |i: usize| -> String { std::fs::read_to_string(stderr_of(i)).unwrap_or_default() };
    wait_for_socket(&sock);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut exited_three = 0;
    while Instant::now() < deadline {
        exited_three = 0;
        for (i, k) in keepers.iter_mut().enumerate() {
            if let Some(status) = k.try_wait().unwrap() {
                assert_eq!(
                    status.code(),
                    Some(3),
                    "racer {i} exits EXIT_SEAT_OWNED=3, got {status}: {}",
                    explain(i)
                );
                exited_three += 1;
            }
        }
        if exited_three == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The seat winner still serves BEFORE any cleanup kill.
    let mut stream = UnixStream::connect(&sock).unwrap();
    let result = ok_result(rpc(&mut stream, 1, "read", json!({})));
    assert_eq!(result["entries"].as_array().unwrap().len(), 0);
    drop(stream);
    for k in keepers.iter_mut() {
        let _ = k.kill();
        let _ = k.wait();
    }
    assert_eq!(
        exited_three,
        3,
        "three losers must exit 3; racer stderr: 0={:?} 1={:?} 2={:?} 3={:?}",
        explain(0),
        explain(1),
        explain(2),
        explain(3)
    );
}

// x-f188 change 3: build drift self-report, self-retire, busy Shutdown.
// ---------------------------------------------------------------

// The 2026-09-14 temp repro, ported. Two idea-style
// appenders race two note loops on one keeper; no ok-replied append may be
// lost and no note write may be reverted.
// ---------------------------------------------------------------

#[test]
fn two_concurrent_idea_commits_survive_concurrent_note_writes() {
    let home = short_home("race");
    let graph = home.join("graph.json");
    // 500 seed rows: enough that a whole-file publish can drop foreign rows,
    // small enough that a debug build's publish stays well inside the 10s
    // lock deadline (the 3000-row repro form held the flock for seconds per
    // write and starved the keeper instead of racing it).
    let seed: Vec<Value> = (0..500)
        .map(|i| {
            json!({
                "id": format!("r-{i:04}"),
                "slug": format!("slug-r-{i:04}"),
                "title": format!("seed {i}"),
                "type": "feature",
                "status": "ready",
                "priority": "p2",
            })
        })
        .collect();
    fno_agents::graph_store::seed_rows(&graph, &seed).unwrap();
    let sock = home.join("graph.json.store.sock");
    let keeper = spawn_keeper("race-test", &graph, &sock);
    wait_for_socket(&sock);

    // Two note loops: 12 in-process replace_state writes each, on rows the
    // appenders never touch. This is the shape every `fno backlog note`
    // takes: read the whole graph outside the lock, publish it whole.
    let note_graph = graph.clone();
    let note_handles: Vec<_> = ["r-0000", "r-0001"]
        .iter()
        .map(|node| {
            let g = note_graph.clone();
            let node = node.to_string();
            std::thread::spawn(move || {
                let mut ok = 0u64;
                for i in 0..12 {
                    let rev =
                        fno_agents::backlog::node_state::current_revision(&g, &node).unwrap_or(0);
                    let input = fno_agents::backlog::node_state::StateWriteInput {
                        node_id: node.clone(),
                        body: format!("race note {i}"),
                        if_revision: Some(rev),
                        source_session_id: None,
                        source_harness: None,
                        reads: None,
                    };
                    if fno_agents::backlog::node_state::replace_state(&g, &input).is_ok() {
                        ok += 1;
                    }
                    // Yield the file lock between writes: a tight in-process
                    // loop re-acquires flock before a starving waiter is
                    // scheduled, and the keeper's 10s deadline fires instead
                    // of the race this test exists to exercise.
                    std::thread::sleep(Duration::from_millis(5));
                }
                (node, ok)
            })
        })
        .collect();

    // Two idea-style appenders: 20 begin + commit_rows appends each through
    // the keeper socket, retrying kind conflict like cmd_idea does. They send
    // no base_digests, so every commit takes the whole-graph fallback, where
    // any concurrent publish is a conflict. A row can starve through all 30
    // tries on a slow runner; it never answered ok, so the contract below
    // does not cover it.
    let appender_handles: Vec<_> = (0..2)
        .map(|w| {
            let s = sock.clone();
            std::thread::spawn(move || {
                let prefix = ['a', 'b'][w];
                let mut stream = UnixStream::connect(&s).unwrap();
                let mut landed: Vec<String> = Vec::new();
                let mut conflicts = 0u64;
                for i in 0..20 {
                    let id = format!("{prefix}-app-{i:04}");
                    let row = json!({
                        "id": id,
                        "slug": format!("slug-{id}"),
                        // The title must stay off the leak grammar the
                        // store gate enforces at write time: the raw id
                        // (prefix-app-0004) carries a node-id match.
                        "title": format!("appended {prefix}{i:04}"),
                        "type": "feature",
                        "status": "intake",
                        "priority": "p2",
                    });
                    for attempt in 0..30u64 {
                        let begin = ok_result(rpc(&mut stream, attempt, "begin", json!({})));
                        let version = begin["version"].as_str().unwrap().to_string();
                        let reply = rpc(
                            &mut stream,
                            attempt,
                            "commit_rows",
                            json!({
                                "base_version": version,
                                "changed": [row],
                                "removed": [],
                                "attempt": attempt + 1,
                            }),
                        );
                        if reply.get("ok") == Some(&json!(true)) {
                            landed.push(id.clone());
                            break;
                        }
                        let kind = reply["error"]["kind"].as_str().unwrap_or("");
                        assert_eq!(
                            kind, "conflict",
                            "appender {w} row {id}: unexpected error: {reply}"
                        );
                        conflicts += 1;
                    }
                }
                (landed, conflicts)
            })
        })
        .collect();

    let note_out: Vec<(String, u64)> = note_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let appender_out: Vec<(Vec<String>, u64)> = appender_handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();

    // Positive control: the race must have produced contention. The repro
    // measured 19 commit_rows conflicts; zero means the threads never
    // interleaved and this run proves nothing.
    let total_conflicts: u64 = appender_out.iter().map(|(_, c)| c).sum();
    assert!(
        total_conflicts > 0,
        "did not race: zero commit_rows conflicts across both appenders"
    );
    let total_landed: usize = appender_out.iter().map(|(landed, _)| landed.len()).sum();
    assert!(
        total_landed > 0,
        "positive control: no append ever answered ok, so the check below proves nothing"
    );

    // Every append that answered ok must be in the store: the json file is
    // a frozen mirror under graph.db.
    let final_ids: std::collections::BTreeSet<String> = fno_agents::graph_store::read_rows(&graph)
        .unwrap()
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_string))
        .collect();
    for (landed, _) in &appender_out {
        for id in landed {
            assert!(
                final_ids.contains(id),
                "append {id} answered ok but is missing from the final graph"
            );
        }
    }

    // Each acknowledged write must be present in the final revision. A write
    // can commit before its transport outcome is reported, so the revision
    // may exceed the count of successful replies but must never be lower.
    for (node, ok) in &note_out {
        let rows = fno_agents::graph_store::read_rows(&graph).unwrap();
        let row = rows
            .iter()
            .find(|r| r["id"].as_str() == Some(node.as_str()))
            .unwrap();
        let revision = row["current_state"]["revision"].as_u64().unwrap();
        assert!(
            revision >= *ok,
            "node {node} answered ok {ok} times but its final revision is {revision}"
        );
    }

    let _ = std::fs::remove_file(home.join("graph.json.store.sock.lock"));
    drop(keeper);
}

// A shutdown never cuts an in-flight request, and a
// request that read an ok reply always has its row in the file.
// ---------------------------------------------------------------

#[test]
fn a_shutdown_mid_commit_never_loses_an_ok_reply() {
    let home = short_home("shutrace");
    let graph = home.join("graph.json");
    seed_empty_store(&graph);
    let sock = home.join("graph.json.store.sock");
    let mut keeper = spawn_keeper("shutrace-test", &graph, &sock);
    wait_for_socket(&sock);

    // Pre-stage six commit_rows requests on separate connections. Each
    // handler thread holds an inflight guard from handling through the reply
    // write, so a Shutdown that acks proves every reply was already written.
    // A connection whose request never entered handling is cut legally: it
    // records as a hangup outcome, never a panic.
    let staged_sock = sock.clone();
    let (staged_tx, staged_rx) = std::sync::mpsc::channel::<()>();
    let staged = std::thread::spawn(move || {
        let mut conns: Vec<(UnixStream, String)> = Vec::new();
        let mut outcomes: Vec<(String, bool)> = Vec::new();
        for i in 0..6 {
            let mut s = match UnixStream::connect(&staged_sock) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let id = format!("m-app-{i:04}");
            write_frame(
                &mut s,
                TAG_REQUEST,
                &serde_json::to_vec(&json!({"id": i, "method": "begin", "params": {}})).unwrap(),
            );
            let (tag, payload) = match read_frame(&mut s) {
                Some(frame) => frame,
                None => {
                    // Cut before the begin was served: a hangup outcome.
                    outcomes.push((id, false));
                    continue;
                }
            };
            assert_eq!(tag, TAG_RESPONSE);
            let reply: Value = serde_json::from_slice(&payload).unwrap();
            if reply.get("ok") != Some(&json!(true)) {
                outcomes.push((id, false));
                continue;
            }
            let begin = &reply["result"];
            let version = begin["version"].as_str().unwrap_or_default().to_string();
            let row = json!({
                "id": id,
                "slug": format!("slug-{id}"),
                // Off the leak grammar: the raw id (m-app-0004) carries a
                // node-id match the store gate refuses at write time.
                "title": format!("mid-commit {i:04}"),
                "type": "feature",
                "status": "intake",
                "priority": "p2",
            });
            let req = json!({
                "id": 100 + i,
                "method": "commit_rows",
                "params": {
                    "base_version": version,
                    "changed": [row],
                    "removed": [],
                },
            });
            let payload = serde_json::to_vec(&req).unwrap();
            let mut f = vec![TAG_REQUEST];
            f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            f.extend_from_slice(&payload);
            s.write_all(&f).unwrap();
            s.flush().unwrap();
            conns.push((s, id));
        }
        // Signal main BEFORE reading replies: every commit frame is staged
        // and every handler that took one is mid-drain, which is the exact
        // window the contract is about.
        let _ = staged_tx.send(());
        // Read every staged reply with its outcome kind.
        for (mut s, id) in conns {
            match read_frame(&mut s) {
                Some((tag, payload)) => {
                    assert_eq!(tag, TAG_RESPONSE);
                    let reply: Value = serde_json::from_slice(&payload).unwrap();
                    outcomes.push((id, reply.get("ok") == Some(&json!(true))));
                }
                None => outcomes.push((id, false)),
            }
        }
        outcomes
    });
    // Shut down while the staged handlers drain.
    let _ = staged_rx.recv_timeout(Duration::from_secs(30));
    let mut s = UnixStream::connect(&sock).unwrap();
    write_frame(&mut s, TAG_SHUTDOWN, &[]);
    let (tag, payload) = read_frame(&mut s).expect("shutdown reply");
    assert_eq!(tag, TAG_RESPONSE);
    let reply: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(
        reply["result"], "shutdown",
        "an unblocked keeper acks: {reply}"
    );
    let outcomes = staged.join().unwrap();

    // Late arrivals: sent after the ack, they meet the dying keeper. Either
    // the frame round-trips (keeper still draining) or the stream is cut;
    // both are legal, so neither is asserted. rpc() would panic on the cut.
    for i in 0..2u64 {
        let Ok(mut s) = UnixStream::connect(&sock) else {
            continue; // socket already unlinked: hangup by refusal
        };
        let req = json!({"id": 900 + i, "method": "begin", "params": {}});
        let payload = serde_json::to_vec(&req).unwrap();
        let mut frame = vec![TAG_REQUEST];
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        if s.write_all(&frame).is_ok() {
            let _ = read_frame(&mut s);
        }
    }

    // Reap: the keeper exits 0 on its own.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match keeper.child.try_wait().expect("reap check") {
            Some(status) => {
                assert_eq!(status.code(), Some(0), "shutdown exits 0: {status}");
                break;
            }
            None => {
                assert!(Instant::now() < deadline, "keeper did not exit");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    assert!(!sock.exists(), "shutdown unlinks its socket");

    // THE CONTRACT: every staged connection that read an ok reply has its
    // row in the store. A connection cut before its reply has no row (it
    // never read ok). The json file is a frozen mirror under graph.db.
    let final_ids: std::collections::BTreeSet<String> = fno_agents::graph_store::read_rows(&graph)
        .unwrap()
        .iter()
        .filter_map(|row| row["id"].as_str().map(str::to_string))
        .collect();
    let mut ok_replies = 0;
    for (id, ok) in &outcomes {
        if *ok {
            ok_replies += 1;
            assert!(
                final_ids.contains(id),
                "commit {id} answered ok but is missing from the store"
            );
        }
    }
    assert!(
        ok_replies >= 1,
        "positive control: at least one staged commit must have answered ok, got {outcomes:?}"
    );
    let _ = std::fs::remove_file(home.join("graph.json.store.sock.lock"));
}

#[test]
fn ready_board_mode_refuses_when_claims_are_unreadable() {
    let home = short_home("board-claims");
    let graph = home.join("graph.json");
    seed_empty_store(&graph);
    let claims_root = home.join("claims-root");
    std::fs::create_dir_all(claims_root.join(".fno")).unwrap();
    // A regular file where read_dir expects a directory: ENOTDIR, the
    // unknown-claim-state the ready op fails closed on.
    std::fs::write(claims_root.join(".fno/claims"), b"not a directory").unwrap();
    let sock = home.join("graph.json.store.sock");
    let keeper = spawn_keeper_with_env(
        "board-claims",
        &graph,
        &sock,
        "FNO_CLAIMS_ROOT",
        claims_root.to_str().unwrap(),
    );
    wait_for_socket(&sock);

    let mut stream = UnixStream::connect(&sock).unwrap();
    let reply = rpc(&mut stream, 1, "ready", json!({"board": true}));
    assert_eq!(reply.get("ok"), Some(&json!(false)), "refused: {reply}");
    assert_eq!(
        reply["error"]["kind"],
        json!("claims_unavailable"),
        "kind names the claims store: {reply}"
    );
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("live claim state is unavailable"),
        "the refusal names the claims store: {reply}"
    );
    drop(keeper);
}

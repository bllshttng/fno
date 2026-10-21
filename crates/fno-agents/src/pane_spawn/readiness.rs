//! The interactive readiness gate and the post-readiness seed submit
//! (`_await_interactive_readiness` mux_spawn.py:2517, `_submit_spawn_seed`
//! :2811), ported as spine steps an arm opts into. Liveness is necessary but
//! never sufficient: a pane is READY only when the manifest engine returns
//! the harness's configured positive rule id, and a seed is submitted only
//! once a frame can witness it.

use super::{pane_ls_detailed, MuxCli};

/// `_WAIT_EXITED` (mux_spawn.py:1783): `pane wait`'s code for a child that
/// exited before readiness.
const WAIT_EXITED: i32 = 12;

/// `_SEED_RETRY_DELAY_S` (mux_spawn.py:2678): the single seed retry's pause.
pub(crate) const SEED_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// The readiness verdict: Python's `(word, detail)` tuple. The row stands in
/// every state except `Failed`.
#[derive(Debug, Clone, PartialEq)]
pub struct Readiness {
    pub state: ReadinessState,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReadinessState {
    Ready,
    Live,
    Blocked,
    Failed,
}

/// The seed submit outcome; the splits are load-bearing (mux_spawn.py:2829):
/// `Unattempted` is the only retried state (nothing can already be in the
/// pane's buffer), `Unconfirmed` fails the spawn, `Unknown` keeps the row.
#[derive(Debug, Clone, PartialEq)]
pub enum SeedOutcome {
    Submitted,
    Unconfirmed { detail: String },
    Unattempted { detail: String },
    Unknown { detail: String },
}

/// `_pane_observation` (mux_spawn.py:2777): a fact about the INSTRUMENT. A
/// read that exited non-zero and a live pane that has not painted are
/// byte-identical but for the exit code, so the code is checked first.
pub fn pane_observation(code: Option<i32>, stdout: &str) -> &'static str {
    if code != Some(0) {
        return "unreadable";
    }
    if stdout.trim().is_empty() {
        "blank"
    } else {
        "painted"
    }
}

/// `_pane_osc_title` (mux_spawn.py:2496): the pane title carried by mux
/// metadata; unreadable means unknown.
fn pane_osc_title(session: &str, pane_id: u64, mux: &dyn MuxCli) -> Option<String> {
    let (rows, _) = pane_ls_detailed(session, mux);
    let rows = rows?;
    rows.into_iter()
        .find(|row| row.get("pane_id").and_then(serde_json::Value::as_u64) == Some(pane_id))
        .and_then(|row| {
            row.get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn lossy_trim(out: &std::process::Output) -> String {
    let raw = if out.stderr.is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    String::from_utf8_lossy(raw).trim().to_string()
}

/// The gate head: `pane wait --timeout 1`. Codes 0 and 11 mean the child is
/// alive; 12 (`_WAIT_EXITED`) means it died before readiness.
fn wait_alive(session: &str, pane_id: u64, mux: &dyn MuxCli) -> Result<(), Readiness> {
    let args: Vec<String> = [
        "mux",
        "pane",
        "wait",
        "--server",
        session,
        &pane_id.to_string(),
        "--timeout",
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let Some(output) = mux.run(&args, &[]).ok() else {
        return Err(Readiness {
            state: ReadinessState::Failed,
            detail: "readiness probe failed: could not run mux pane wait".to_string(),
        });
    };
    let code = output.status.code().unwrap_or(-1);
    if code == WAIT_EXITED {
        return Err(Readiness {
            state: ReadinessState::Failed,
            detail: "provider exited before readiness".to_string(),
        });
    }
    if code != 0 && code != 11 {
        let raw = lossy_trim(&output);
        return Err(Readiness {
            state: ReadinessState::Failed,
            detail: format!("readiness probe failed: pane wait exited {code}: {raw}"),
        });
    }
    Ok(())
}

/// The screen half: `pane read --lines 20`, empty means unpainted (live),
/// painted goes to the manifest verdict.
fn screen_readiness(provider: &str, session: &str, pane_id: u64, mux: &dyn MuxCli) -> Readiness {
    let args: Vec<String> = [
        "mux",
        "pane",
        "read",
        "--server",
        session,
        &pane_id.to_string(),
        "--lines",
        "20",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let Some(output) = mux.run(&args, &[]).ok() else {
        return Readiness {
            state: ReadinessState::Failed,
            detail: "readiness probe failed: could not run mux pane read".to_string(),
        };
    };
    if output.status.code() != Some(0) {
        let raw = lossy_trim(&output);
        let code = output.status.code().unwrap_or(-1);
        return Readiness {
            state: ReadinessState::Failed,
            detail: format!("readiness probe failed: pane read exited {code}: {raw}"),
        };
    }
    let screen = String::from_utf8_lossy(&output.stdout).to_string();
    if screen.trim().is_empty() {
        return Readiness {
            state: ReadinessState::Live,
            detail: "ready marker not observed: screen is unpainted".to_string(),
        };
    }
    let osc_title = pane_osc_title(session, pane_id, mux);
    manifest_readiness(provider, &screen, osc_title.as_deref())
}

/// The manifest half: the contract's pinned ready marker against the Rust
/// manifest engine's verdict - the same `manifest-eval` the Python leg
/// shells out to, called in-process here.
fn manifest_readiness(provider: &str, screen: &str, osc_title: Option<&str>) -> Readiness {
    let expected = || -> Option<String> {
        let contract = crate::harness_capabilities::HarnessContract::packaged().ok()?;
        let caps = contract.capabilities(provider).ok()?;
        Some(caps.ready_marker.clone())
    };
    let Some(expected) = expected() else {
        return Readiness {
            state: ReadinessState::Live,
            detail: format!("harness {provider:?} has no pinned ready marker"),
        };
    };
    if expected == "unsupported" {
        return Readiness {
            state: ReadinessState::Live,
            detail: format!("harness {provider:?} has no pinned ready marker"),
        };
    }
    let verdict = crate::manifest::evaluate_screen_json_with_osc(provider, screen, osc_title, None);
    let Ok(verdict) = verdict else {
        return Readiness {
            state: ReadinessState::Live,
            detail: "readiness manifest evaluation failed".to_string(),
        };
    };
    let observed = verdict.get("rule_id").and_then(serde_json::Value::as_str);
    if verdict.get("matched").and_then(serde_json::Value::as_bool) == Some(true)
        && observed == Some(expected.as_str())
    {
        return Readiness {
            state: ReadinessState::Ready,
            detail: format!("ready-marker={expected}"),
        };
    }
    if verdict.get("state").and_then(serde_json::Value::as_str) == Some("blocked")
        && observed.is_some()
    {
        return Readiness {
            state: ReadinessState::Blocked,
            detail: format!("blocked-rule={}", observed.unwrap_or_default()),
        };
    }
    let mut detail = format!(
        "expected {expected}; observed {}",
        observed.unwrap_or("no matching manifest rule")
    );
    if let Some(err) = verdict.get("error").and_then(serde_json::Value::as_str) {
        detail.push_str(&format!(" ({err})"));
    }
    Readiness {
        state: ReadinessState::Live,
        detail,
    }
}

/// The gate, whole: wait, screen, manifest.
pub fn await_interactive_readiness(
    provider: &str,
    receipt_session: &str,
    pane_id: u64,
    mux: &dyn MuxCli,
) -> Readiness {
    if let Err(failed) = wait_alive(receipt_session, pane_id, mux) {
        return failed;
    }
    screen_readiness(provider, receipt_session, pane_id, mux)
}

/// The seed's frame read (`pane read --lines 40`) plus the observation word.
struct FrameRead {
    observation: &'static str,
    frame: String,
}

fn read_frame(session: &str, pane_id: u64, mux: &dyn MuxCli) -> FrameRead {
    let args: Vec<String> = [
        "mux",
        "pane",
        "read",
        "--server",
        session,
        &pane_id.to_string(),
        "--lines",
        "40",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match mux.run(&args, &[]).ok() {
        Some(output) => {
            let code = output.status.code();
            let frame = String::from_utf8_lossy(&output.stdout).to_string();
            FrameRead {
                observation: pane_observation(code, &frame),
                frame,
            }
        }
        None => FrameRead {
            observation: "unreadable",
            frame: String::new(),
        },
    }
}

/// One `pane send --submit --raw`: a bare submit keystroke when `payload`
/// is empty, the seed verbatim otherwise.
fn pane_send_submit(
    session: &str,
    pane_id: u64,
    payload: &str,
    mux: &dyn MuxCli,
) -> Option<std::process::Output> {
    let args: Vec<String> = [
        "mux",
        "pane",
        "send",
        "--server",
        session,
        &pane_id.to_string(),
        "--text",
        payload,
        "--submit",
        "--raw",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    mux.run(&args, &[]).ok()
}

/// The agy trust-modal test (`trust (?:this )?folder|do you trust`,
/// mux_spawn.py:2917), as string probes.
fn agy_trust_modal(frame: &str) -> bool {
    frame.contains("trust this folder") || frame.contains("do you trust")
}

/// The seed outcome receipt: the four states, the detail, the source word
/// and the frame observation. The receipt tuple Python returns as
/// `(state, detail, source, observation)`; `source` is "" only when nothing
/// was typed and nothing rode the argv.
pub struct SeedReceipt {
    pub outcome: SeedOutcome,
    pub source: String,
    pub observation: &'static str,
}

impl SeedReceipt {
    fn of(outcome: SeedOutcome, source: &str, observation: &'static str) -> Self {
        SeedReceipt {
            outcome,
            source: source.to_string(),
            observation,
        }
    }
}

/// `_submit_spawn_seed`: submit the spawn seed after the shared readiness
/// probe has painted. Faithful port including the agy trust-modal clear, the
/// argv-seed receipt, the exit-22 unconfirmed family and the `unknown`
/// keep-the-row case.
pub fn submit_spawn_seed(
    provider: &str,
    session: &str,
    pane_id: u64,
    seed: &str,
    seed_in_argv: bool,
    mux: &dyn MuxCli,
) -> SeedReceipt {
    let first = read_frame(session, pane_id, mux);
    if first.observation == "unreadable" {
        if seed_in_argv {
            // Delivery happened at exec time; the doubt about the pane rides
            // the observation, never the seed word.
            return SeedReceipt::of(SeedOutcome::Submitted, "argv", "unreadable");
        }
        return SeedReceipt::of(
            SeedOutcome::Unattempted {
                detail: "pane frame unreadable, seed submission not attempted".to_string(),
            },
            "",
            "unreadable",
        );
    }
    let observation = first.observation;
    let mut trust_source = String::new();
    if provider == "agy" && agy_trust_modal(&first.frame) {
        match clear_agy_trust(session, pane_id, mux) {
            Some(outcome) => return SeedReceipt::of(outcome, "", observation),
            None => trust_source = "trust-cleared".to_string(),
        }
    }
    let _ = observation;
    submit_after_trust(
        provider,
        session,
        pane_id,
        seed,
        seed_in_argv,
        mux,
        trust_source,
    )
}

/// The agy trust clear: send the bare submit, read back, and answer. `Some`
/// carries a terminal receipt; `None` means the modal cleared and the seed
/// flow continues with the `trust-cleared` source.
fn clear_agy_trust(session: &str, pane_id: u64, mux: &dyn MuxCli) -> Option<SeedOutcome> {
    let Some(cleared) = pane_send_submit(session, pane_id, "", mux) else {
        return Some(SeedOutcome::Unconfirmed {
            detail: "mux did not answer the agy trust submit; the modal may still be up"
                .to_string(),
        });
    };
    if cleared.status.code() != Some(0) {
        return Some(SeedOutcome::Unconfirmed {
            detail: "agy trust gate submit refused".to_string(),
        });
    }
    let second = read_frame(session, pane_id, mux);
    if second.observation == "unreadable" {
        return Some(SeedOutcome::Unconfirmed {
            detail: "mux did not answer the agy trust read-back; the modal may still be up"
                .to_string(),
        });
    }
    if agy_trust_modal(&second.frame) {
        return Some(SeedOutcome::Unconfirmed {
            detail: "agy trust gate did not clear; the modal outlived the clearing submit. \
                     Grant the directory in ~/.gemini/trustedFolders.json and spawn again - \
                     this pane is reaped, so there is no modal left to answer"
                .to_string(),
        });
    }
    None
}

/// The submit half: classify the frame, send, classify the send.
fn submit_after_trust(
    provider: &str,
    session: &str,
    pane_id: u64,
    seed: &str,
    seed_in_argv: bool,
    mux: &dyn MuxCli,
    trust_source: String,
) -> SeedReceipt {
    let _ = provider;
    let fresh = read_frame(session, pane_id, mux);
    let observation = fresh.observation;
    let frame = fresh.frame;
    if observation == "unreadable" {
        if seed_in_argv {
            return SeedReceipt::of(SeedOutcome::Submitted, "argv", "unreadable");
        }
        return SeedReceipt::of(
            SeedOutcome::Unattempted {
                detail: "pane frame unreadable, seed submission not attempted".to_string(),
            },
            "",
            "unreadable",
        );
    }
    if frame.trim().is_empty() {
        if seed_in_argv {
            // An unpainted TUI cannot witness a delivery that happened at
            // exec time; the seed word stays honest and the instrument's
            // state rides the observation.
            return SeedReceipt::of(SeedOutcome::Submitted, "argv", observation);
        }
        return SeedReceipt::of(
            SeedOutcome::Unattempted {
                detail: "pane frame blank, seed submission not attempted".to_string(),
            },
            "",
            observation,
        );
    }
    if seed_in_argv && !frame.contains(seed) {
        // The argv fact outranks the frame heuristic; the enriched seed is
        // multi-line and will not appear verbatim in a 40-line frame.
        return SeedReceipt::of(SeedOutcome::Submitted, "argv", observation);
    }
    let payload = if frame.contains(seed) { "" } else { seed };
    let source: String = if !trust_source.is_empty() {
        trust_source
    } else if payload.is_empty() {
        // `_send_source`: `delivered` is the word reserved for text that WAS
        // typed; an argv-committed seed sends only the submit keystroke.
        "preloaded".to_string()
    } else {
        "delivered".to_string()
    };
    let Some(submitted) = pane_send_submit(session, pane_id, payload, mux) else {
        return SeedReceipt::of(
            SeedOutcome::Unknown {
                detail: "mux did not answer the submit; the keystroke may have landed".to_string(),
            },
            &source,
            observation,
        );
    };
    let code = submitted.status.code();
    if code != Some(0) {
        let mux_detail = lossy_trim(&submitted);
        let label = if code == Some(22) {
            "no ready marker, submission unconfirmed"
        } else {
            "pane send failed"
        };
        let cause = format!(
            "{label} (exit {}): {}",
            code.unwrap_or(-1),
            if mux_detail.is_empty() {
                "no diagnostic from mux"
            } else {
                &mux_detail
            }
        );
        return SeedReceipt::of(
            SeedOutcome::Unconfirmed {
                detail: format!("submission refused, {cause}"),
            },
            &source,
            observation,
        );
    }
    SeedReceipt::of(SeedOutcome::Submitted, &source, observation)
}

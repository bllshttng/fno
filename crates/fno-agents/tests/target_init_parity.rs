//! parity-stage: characterization
//! parity-oracle: fno.target_cli._print_orientation_report
//!
//! The oracle names a deleted init-only helper: the Python init leg's
//! decision body lived inline in `init()` (now a thin forward to this door),
//! and the init-only helpers around it are deleted with it.
//!
//! Golden parity for the `fno do target init` port. The case tables come from
//! `tests/capture/tables.json`, the dump of `target_capture.py
//! --dump-tables` (one table, zero hand transcription); the shared harness
//! is `tests/target_parity/mod.rs`.
//!
//! The port commit flipped the leg: the goldens were captured from the live
//! Python CLI at its recorded sha, and this test now drives the native
//! `target-init` door against the same bytes, with the golden files
//! untouched.

#[path = "target_parity/mod.rs"]
mod parity;

#[path = "common/mod.rs"]
mod common;

use parity::Leg;

/// The stage's leg: the native door, in-process.
const LEG: Leg = Leg::NativeDoor("target-init");

#[test]
fn target_init_golden_parity() {
    parity::run_all("init", "target_init", &LEG);
}

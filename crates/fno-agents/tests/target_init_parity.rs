//! parity-stage: differential
//! parity-oracle: fno.target_cli.init
//!
//! Golden parity for the `fno do target init` port, frozen against the live
//! Python leg before its deletion. The case tables come from
//! `tests/capture/tables.json`, the dump of `target_capture.py
//! --dump-tables` (one table, zero hand transcription); the shared harness
//! is `tests/target_parity/mod.rs`.
//!
//! This file's leg is the one that flips: at the goldens commit the test
//! drives the live Python CLI (differential); the port commit flips the leg
//! to the native door and this header to `characterization`, with the
//! golden files untouched.

#[path = "target_parity/mod.rs"]
mod parity;

#[path = "common/mod.rs"]
mod common;

use parity::Leg;

/// The stage's leg. Differential: the live Python CLI through the shim. The
/// port commit flips this to `Leg::NativeDoor("target-init")` and this
/// comment with it.
const LEG: Leg = Leg::PythonCli;

#[test]
fn target_init_golden_parity() {
    // The differential stage needs the Python leg; when this machine has no
    // usable interpreter (no venv), skip - it never fails (house rule).
    if matches!(LEG, Leg::PythonCli) && !parity::python_leg_available() {
        eprintln!("skip: no usable Python leg for the differential stage");
        return;
    }
    parity::run_all("init", "target_init", &LEG);
}

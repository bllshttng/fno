//! parity-stage: characterization
//! parity-oracle: cli/src/fno/target_cli.py
//!
//! Golden parity for the `fno do target start` port. The goldens were
//! captured from the live Python CLI at its recorded sha; this test now
//! drives the native `target-start` door against the same bytes, with the
//! golden files untouched. The case tables come from
//! `tests/capture/tables.json`, the dump of `target_capture.py
//! --dump-tables`; the shared harness is `tests/target_parity/mod.rs`.

#[path = "target_parity/mod.rs"]
mod parity;

#[path = "common/mod.rs"]
mod common;

use parity::Leg;

/// The stage's leg: the native door, in a subprocess.
const LEG: Leg = Leg::NativeDoor("target-start");

#[test]
fn target_start_golden_parity() {
    parity::run_all("start", "target_start", &LEG);
}

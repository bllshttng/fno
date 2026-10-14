//! parity-stage: differential
//! parity-oracle: file:cli/src/fno/target_cli.py
//!
//! Golden parity for the `fno do target start` contract, captured from the
//! live Python leg. `start` stays Python through this group (the init port is
//! this group's one verb, per the one-verb-per-PR law), so this file stays
//! differential: the oracle resolves, the leg is the live Python CLI through
//! the front-door shim, and the start port later reuses the start goldens
//! captured here. The case tables come from `tests/capture/tables.json`, the
//! dump of `target_capture.py --dump-tables`; the shared harness is
//! `tests/target_parity/mod.rs`.

#[path = "target_parity/mod.rs"]
mod parity;

#[path = "common/mod.rs"]
mod common;

use parity::Leg;

/// start stays Python through this group, so the leg never flips here.
const LEG: Leg = Leg::PythonCli;

#[test]
fn target_start_golden_parity() {
    // The differential stage needs the Python leg; under `cargo test` on a
    // machine with no cli/.venv the leg is unavailable: skip, never fail
    // (house rule).
    if matches!(LEG, Leg::PythonCli) && !parity::python_leg_available() {
        eprintln!("skip: no usable Python leg for the differential stage");
        return;
    }
    parity::run_all("start", "target_start", &LEG);
}

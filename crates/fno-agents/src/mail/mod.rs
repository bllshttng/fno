//! The mail protocol core: nothing in this module names a fleet concept.
//! `scripts/ci/check-mail-module-deps.sh` holds that rule.

pub mod address;
pub mod envelope;
pub mod frame;
pub mod lane;
pub mod log;
pub mod state;

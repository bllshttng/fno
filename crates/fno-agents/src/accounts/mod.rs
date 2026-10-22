//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]) and the managed slot stamp ([slot]). Ported
//! from `fno.adapters.providers` (model.py, the read side of loader.py and
//! `managed.active_slot_id`); the parity cases live in
//! `tests/accounts_records_parity.rs`.
pub mod records;
pub mod slot;

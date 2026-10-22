//! The account fact set on the Rust leg: the `config.accounts` records and
//! their validation ([records]), the managed slot stamp ([slot]), and the
//! proven identity behind a launch's credential ([binding]). Ported from
//! `fno.adapters.providers` (model.py, the read side of loader.py,
//! binding.py and the managed.py reads); the parity cases live in
//! `tests/accounts_records_parity.rs` and `tests/accounts_binding_parity.rs`.
pub mod binding;
pub mod records;
pub mod slot;

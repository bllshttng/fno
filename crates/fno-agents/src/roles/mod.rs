//! Business roles: layered role-definition discovery and the
//! `fno agents roles` verbs, ported from the Python `fno.roles` package.
//!
//! - [`models`]: the role-definition schema, checked over raw JSON.
//! - [`registry`]: discovery under the layered root, and the
//!   `spawn-overlay` kind `roles-discover` the Python package reads its
//!   records through.
//! - [`cli`]: the verbs, reached as `fno-agents spawn-overlay roles <op>`.

pub mod cli;
pub mod models;
pub mod registry;

//! Business roles: layered role-definition discovery and the
//! `fno agents roles` commands, ported from the Python `fno.roles` package.
//!
//! - [`models`]: the role-definition schema, checked over raw JSON.
//! - [`registry`]: discovery under the layered root, and the
//!   `spawn-overlay` kind `roles-discover` the Python package reads its
//!   records through.
//! - [`resolver`]: business-role resolution, and the `spawn-overlay` kind
//!   `roles-resolve` the Python package resolves through.
//! - [`context`]: the catalog of configured `[context.artifacts]` files.
//! - [`cli`]: the commands, reached as `fno-agents spawn-overlay roles <op>`.

pub mod cli;
pub mod context;
pub mod models;
pub mod registry;
pub mod resolver;

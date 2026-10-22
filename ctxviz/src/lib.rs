//! ctxviz library: shared by the binary and the integration tests.

pub mod commands;
pub mod kitty;
pub mod model;
pub mod pb;
pub mod render;
pub mod schema;
pub mod session;
pub mod store;
pub mod text;
pub mod theme;
pub mod tui;

/// Shared test helpers (temp dirs, store builder, synthetic fixture), the
/// same file the integration tests use as `mod common`.
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
pub(crate) mod test_support;

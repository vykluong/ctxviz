//! ctxviz library: shared by the binary and the integration tests.

/// Version and build identifiers embedded at compile time.
pub mod build_info {
    /// The crate version from `Cargo.toml`.
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
    /// The git commit (`<sha>`, `<sha>-dirty`, or `unknown`), set by `build.rs`.
    pub const BUILD: &str = env!("CTXVIZ_BUILD");
    /// The `--version` text clap prints after the binary name: `<version> (<build>)`.
    pub const LONG: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CTXVIZ_BUILD"), ")");
}

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

//! One module per subcommand. Each exposes `Args` (clap) and
//! `run(Args) -> anyhow::Result<i32>` returning the process exit code.

pub mod doctor;
pub mod notify_test;
pub mod open;
pub mod snapshot;
pub mod statusline;
pub mod tui;
pub mod watch;

use std::path::PathBuf;

/// Store selection flags shared by snapshot, tui, watch, doctor.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct StoreArgs {
    /// Agent id or path to store.db (default: latest chat for --cwd).
    #[arg(long)]
    pub chat: Option<String>,
    /// Workspace whose latest chat to use (default: current directory).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Use the most recently written chat in any workspace (overrides --cwd).
    #[arg(long)]
    pub latest: bool,
}

impl StoreArgs {
    pub fn resolve(&self) -> anyhow::Result<PathBuf> {
        let cwd = match &self.cwd {
            Some(c) => c.clone(),
            None => std::env::current_dir()?,
        };
        crate::store::find_store(self.chat.as_deref(), &cwd, self.latest)
    }
}

/// Theme selection shared by statusline, tui, notify-test, snapshot, watch
/// (and forwarded by open).
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ThemeArgs {
    /// Color theme: auto, light, dark, ansi, none, or a family
    /// (modus, catppuccin, rose-pine, tokyo-night, gruvbox, optionally
    /// suffixed -light/-dark). Env: CTXVIZ_THEME. See also --theme-file.
    #[arg(long)]
    pub theme: Option<String>,
    /// Derive the theme from a Kitty .conf (foreground, background, colorN)
    #[arg(long, value_name = "PATH", conflicts_with = "theme")]
    pub theme_file: Option<PathBuf>,
}

impl ThemeArgs {
    /// Resolve the theme (NO_COLOR, --theme-file, --theme, CTXVIZ_THEME, auto)
    /// and install it.
    pub fn init(&self) {
        crate::theme::init(crate::theme::resolve_file_or(self.theme.as_deref(), self.theme_file.as_deref()));
    }

    /// [`ThemeArgs::init`] with a terminal background probe for auto detection.
    pub fn init_with_probe(&self, probe: &dyn Fn() -> Option<crate::theme::Mode>) {
        crate::theme::init(crate::theme::resolve_file_or_probe(
            self.theme.as_deref(),
            self.theme_file.as_deref(),
            probe,
        ));
    }
}

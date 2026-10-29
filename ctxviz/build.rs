//! Embeds the git commit in the binary as `CTXVIZ_BUILD` for `--version`.
//!
//! The value is `<sha>` or `<sha>-dirty` (tracked files modified), or
//! `unknown` when git or the repository is unavailable (for example a
//! source tarball). Uses only std so the dependency graph is unchanged.

use std::path::Path;
use std::process::Command;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let dir = Path::new(&dir);

    // Declaring any rerun key disables Cargo's default rerun-on-any-change,
    // so the dirty flag can lag until HEAD moves. `cargo install` from a
    // clean checkout is exact.
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(head) = git(dir, &["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={}", abs(dir, &head));
        if let Some(name) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
            if let Some(reff) = git(dir, &["rev-parse", "--git-path", &name]) {
                println!("cargo:rerun-if-changed={}", abs(dir, &reff));
            }
        }
    }

    let build = match git(dir, &["rev-parse", "--short=10", "HEAD"]) {
        Some(sha) => match git(dir, &["status", "--porcelain", "--untracked-files=no"]) {
            Some(status) if status.is_empty() => sha,
            Some(_) => format!("{sha}-dirty"),
            None => "unknown".into(),
        },
        None => "unknown".into(),
    };
    println!("cargo:rustc-env=CTXVIZ_BUILD={build}");
}

/// Runs `git -C <dir> <args>` and returns trimmed stdout on success.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}

/// `--git-path` output is relative to `dir` unless already absolute.
fn abs(dir: &Path, p: &str) -> String {
    dir.join(p).display().to_string()
}

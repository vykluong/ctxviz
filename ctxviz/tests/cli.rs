//! End-to-end checks against the built `ctxviz` binary.

use std::process::Command;

#[test]
fn version_flag_shows_build_info() {
    let out = Command::new(env!("CARGO_BIN_EXE_ctxviz")).arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // Shape only: the dirty flag depends on the checkout that built the binary.
    let pattern =
        format!(r"^ctxviz {} \(([0-9a-f]{{7,}}(-dirty)?|unknown)\)\n$", regex::escape(env!("CARGO_PKG_VERSION")));
    assert!(regex::Regex::new(&pattern).unwrap().is_match(&stdout), "unexpected --version output: {stdout:?}");
}

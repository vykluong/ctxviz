//! Sanitizing untrusted text before it reaches a terminal.
//!
//! Chat store contents (tool results, web pages, file contents) are written by
//! parties the user does not control. Every path that prints store-derived
//! text to a terminal must go through [`sanitize_terminal_text`] so that
//! control characters, and therefore any escape sequence (CSI, OSC, DCS,
//! APC), can never reach the emulator.

use std::borrow::Cow;

/// Replacement glyph for every control character that is not `\n` or `\t`.
pub const REPLACEMENT: char = '\u{FFFD}';

/// True for characters that must never be sent to a terminal verbatim:
/// C0 controls other than newline and tab, DEL, C1 controls (U+0080..=U+009F),
/// and the Unicode line/paragraph separators U+2028 and U+2029.
pub fn is_dangerous(c: char) -> bool {
    match c {
        '\n' | '\t' => false,
        '\u{0000}'..='\u{001F}' => true,
        '\u{007F}' => true,
        '\u{0080}'..='\u{009F}' => true,
        '\u{2028}' | '\u{2029}' => true,
        _ => false,
    }
}

/// Replace every [`is_dangerous`] character with U+FFFD. Returns the input
/// unchanged (borrowed) when nothing needs replacing.
pub fn sanitize_terminal_text(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_dangerous) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.chars().map(|c| if is_dangerous(c) { REPLACEMENT } else { c }).collect())
}

/// Like [`sanitize_terminal_text`] but also replaces `\n` and `\t`, for
/// single-line sinks (titles, labels, status lines).
pub fn sanitize_single_line(s: &str) -> Cow<'_, str> {
    if !s.chars().any(|c| c == '\n' || c == '\t' || is_dangerous(c)) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(
        s.chars()
            .map(|c| match c {
                '\n' | '\t' => ' ',
                c if is_dangerous(c) => REPLACEMENT,
                c => c,
            })
            .collect(),
    )
}

/// Owned convenience wrapper.
pub fn sanitized(s: &str) -> String {
    sanitize_terminal_text(s).into_owned()
}

/// Assertion helper for tests: true when `s` contains nothing a terminal
/// could interpret as a control sequence.
pub fn is_clean(s: &str) -> bool {
    !s.chars().any(is_dangerous)
}

/// Identifiers that are used to build file system paths (chat agent ids,
/// session ids, cache file names). Accepts `[A-Za-z0-9._-]{1,128}` with no
/// leading `.` (which also rejects `.`, `..`, and hidden files).
pub fn valid_id(s: &str) -> bool {
    let n = s.len();
    (1..=128).contains(&n)
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text_through_borrowed() {
        let s = "hello\tworld\nline two, ünïcödé 日本";
        assert!(matches!(sanitize_terminal_text(s), Cow::Borrowed(_)));
        assert!(is_clean(s));
    }

    #[test]
    fn strips_osc_dcs_csi_and_c1() {
        let cases = [
            "\x1b]52;c;QUJD\x07",
            "\x1bP@kitty-cmd{\"cmd\":\"launch\"}\x1b\\",
            "\u{9b}31m",
            "\u{9d}0;title\u{9c}",
            "a\u{2028}b\u{2029}c",
            "\x7f\x00\x08",
        ];
        for c in cases {
            let out = sanitize_terminal_text(c);
            assert!(is_clean(&out), "{c:?} -> {out:?}");
            assert!(!out.contains('\x1b'));
            assert!(out.contains(REPLACEMENT));
        }
    }

    #[test]
    fn keeps_newline_and_tab_but_single_line_flattens_them() {
        assert_eq!(sanitize_terminal_text("a\nb\tc"), "a\nb\tc");
        assert_eq!(sanitize_single_line("a\nb\tc\x1b"), "a b c\u{FFFD}");
    }

    #[test]
    fn valid_ids() {
        for ok in ["abc", "a-b_c.d", "0123456789", "A", &"x".repeat(128)] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", ".", "..", "../x", ".hidden", "a/b", "a\\b", "a b", "a\0", "é", &"x".repeat(129), "/abs"] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }
}

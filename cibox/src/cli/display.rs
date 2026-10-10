//! Printing project facts to a terminal.
//!
//! Facts are read out of files the project ships — the package name in
//! `Cargo.toml`, the module path in `go.mod`, the origin URL in
//! `.git/config` — so their contents are whatever that repository's author
//! put there, and `cibox detect` writes them to a terminal that *acts* on
//! what it is sent. A package name holding `\u{1b}[2K` erases the line cibox
//! just wrote and prints whatever follows in its place; one holding
//! `\u{1b}]0;…\u{7}` retitles the window; `\n` forges extra output lines.
//! None of that needs the user to do anything but clone a repository and ask
//! what cibox makes of it.
//!
//! So untrusted strings go through [`untrusted`] on the way to stdout, the
//! same way they go through [`crate::config::image::coerce_reference`] on the
//! way to a shell command.

/// Whether `c` changes how the surrounding text is displayed instead of
/// displaying as itself.
fn is_display_control(c: char) -> bool {
    // C0 (ESC above all, plus the newlines and carriage returns that would
    // forge or overwrite output lines), DEL, and C1
    c.is_control()
        // Bidirectional formatting: not control characters, and invisible,
        // but they reorder the text around them, so a name can be made to
        // read as something it does not say ("Trojan Source")
        || matches!(c,
            '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}')
}

/// Render a string that came out of the project for printing: anything a
/// terminal would act on becomes a visible `\u{..}` escape, everything else —
/// including ordinary non-ASCII text — is left exactly as it was.
pub fn untrusted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if is_display_control(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ordinary_text_is_untouched() {
        for value in [
            "app",
            "github.com/owner/repo",
            "crate_name-2.0",
            "café",
            "日本語",
        ] {
            assert_eq!(untrusted(value), value);
        }
    }

    #[test]
    fn test_escape_sequences_cannot_reach_the_terminal() {
        // Erase the line cibox wrote and print something else in its place
        assert_eq!(untrusted("poc\u{1b}[2Kall clear"), "poc\\u{1b}[2Kall clear");
        // Retitle the window (OSC, terminated by BEL)
        assert_eq!(
            untrusted("poc\u{1b}]0;pwned\u{7}"),
            "poc\\u{1b}]0;pwned\\u{7}"
        );
        // Forge further output lines
        assert_eq!(
            untrusted("poc\n    publishable: no"),
            "poc\\n    publishable: no"
        );
        assert_eq!(untrusted("poc\rapp"), "poc\\rapp");
        // C1: the 8-bit CSI, which starts a sequence without any ESC
        assert_eq!(untrusted("poc\u{9b}2K"), "poc\\u{9b}2K");
    }

    #[test]
    fn test_bidi_overrides_cannot_reorder_the_line() {
        assert_eq!(untrusted("poc\u{202e}dangerous"), "poc\\u{202e}dangerous");
    }

    #[test]
    fn test_output_is_free_of_anything_a_terminal_acts_on() {
        let hostile = "\u{1b}[31m\u{9b}2K\r\n\u{7}\u{202e}\u{2066}\u{7f}";
        let rendered = untrusted(hostile);
        assert!(
            rendered.chars().all(|c| !is_display_control(c)),
            "{rendered:?}"
        );
    }
}

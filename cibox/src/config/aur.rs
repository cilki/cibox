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

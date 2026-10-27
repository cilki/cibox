pub mod android;
pub mod aur;
pub mod image;
pub mod infer;
pub mod platform;
pub mod ron_types;

pub use infer::{infer_platform, platform_names, target_platforms, TargetSource};
pub use platform::Platform;
pub use ron_types::*;

/// RON options shared by every cibox.ron read and write.
///
/// `IMPLICIT_SOME` lets optional fields be written without `Some(...)`,
/// e.g. `enabled: false` instead of `enabled: Some(false)`.
pub fn ron_options() -> ron::Options {
    ron::Options::default()
        .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
}

/// Read and parse the cibox.ron at `path`: `None` when there is no file to
/// read, an error when there is one and it doesn't parse. What a missing
/// file means — detection's defaults, or a mistake to report — is the
/// caller's call; a malformed one is never silently the defaults.
pub fn load_config(path: &std::path::Path) -> crate::error::Result<Option<CiboxConfig>> {
    use anyhow::Context;
    std::fs::read_to_string(path)
        .ok()
        .map(|ron_str| {
            parse_config(&ron_str).with_context(|| format!("Failed to parse {}", path.display()))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_config_distinguishes_missing_from_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cibox.ron");
        assert!(load_config(&path).unwrap().is_none());

        std::fs::write(&path, "(rust_test: (enabled: false))").unwrap();
        let config = load_config(&path).unwrap().unwrap();
        assert_eq!(config.rust_test.enabled, Some(false));

        std::fs::write(&path, "(rust_test: (enabled: false)").unwrap();
        let err = format!("{:#}", load_config(&path).unwrap_err());
        assert!(err.contains("Failed to parse"), "{err}");
        assert!(err.contains("cibox.ron"), "{err}");
    }
}

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
/// read, an error when there is one and it can't be read or doesn't parse.
/// What a missing file means — detection's defaults, or a mistake to
/// report — is the caller's call; a file that is there is never silently
/// the defaults. Only `NotFound` is "missing": a cibox.ron that isn't UTF-8
/// (a Latin-1 comment, PowerShell's UTF-16 `>`) or can't be opened still
/// holds the user's overrides, and treating it as absent would have update
/// re-add the jobs it disables and the editor's first save overwrite it.
pub fn load_config(path: &std::path::Path) -> crate::error::Result<Option<CiboxConfig>> {
    use anyhow::Context;
    let ron_str = match std::fs::read_to_string(path) {
        Ok(ron_str) => ron_str,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    parse_config(&ron_str)
        .with_context(|| format!("Failed to parse {}", path.display()))
        .map(Some)
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

    #[test]
    fn test_load_config_reports_a_file_it_cannot_read() {
        // A cibox.ron that is there but not UTF-8 is the user's overrides,
        // not the absence of any: read as missing, update would re-add the
        // jobs it disables and the editor would save over it
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cibox.ron");
        std::fs::write(&path, b"(\n    // d\xe9sactiv\xe9\n    rust_audit: (enabled: false),\n)\n")
            .unwrap();
        let err = format!("{:#}", load_config(&path).unwrap_err());
        assert!(err.contains("Failed to read"), "{err}");
        assert!(err.contains("cibox.ron"), "{err}");

        // Nor is a path that exists as something other than a file
        let dir_path = dir.path().join("dir.ron");
        std::fs::create_dir(&dir_path).unwrap();
        assert!(load_config(&dir_path).is_err());
    }
}

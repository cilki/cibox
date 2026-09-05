use cibox_macros::Preset;

mod circleci;
mod gitea;
mod github;
mod gitlab;
mod jenkins;

/// CI pipeline for Rust projects (binaries, libraries, and workspaces)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Preset)]
#[preset(category = "Languages")]
#[serde(default)]
pub struct Rust {
    #[preset_field(hidden = true)]
    pub(super) rust_version: String,

    /// Enable code coverage reporting with tarpaulin
    #[preset_field(display = "Code Coverage")]
    pub(super) enable_coverage: bool,

    /// Run Clippy linter for code quality
    #[preset_field(display = "Clippy Linter")]
    pub(super) enable_linter: bool,

    /// Run cargo-audit for dependency vulnerabilities
    #[preset_field(display = "Security Scan")]
    pub(super) enable_security_scan: bool,

    /// Check code formatting with rustfmt
    #[preset_field(display = "Rustfmt Check")]
    pub(super) enable_format_check: bool,

    /// Build optimized release binary in CI
    #[preset_field(display = "Build Release")]
    pub(super) build_release: bool,
}

impl Default for Rust {
    fn default() -> Self {
        Self {
            rust_version: "stable".to_string(),
            enable_coverage: false,
            enable_linter: false,
            enable_security_scan: false,
            enable_format_check: false,
            build_release: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::ToGitHub;

    #[test]
    fn test_default() {
        let preset = Rust::default();
        assert_eq!(preset.rust_version, "stable");
        assert!(!preset.enable_coverage);
        assert!(!preset.enable_linter);
        assert!(!preset.enable_security_scan);
        assert!(!preset.enable_format_check);
        assert!(!preset.build_release);
    }

    #[test]
    fn test_with_options() {
        let preset = Rust {
            rust_version: "1.75.0".to_string(),
            enable_coverage: true,
            enable_linter: true,
            enable_security_scan: false,
            enable_format_check: false,
            build_release: true,
        };

        assert_eq!(preset.rust_version, "1.75.0");
        assert!(preset.enable_coverage);
        assert!(preset.enable_linter);
        assert!(preset.build_release);
    }

    #[test]
    fn test_to_github_basic() {
        let preset = Rust {
            rust_version: "stable".to_string(),
            enable_coverage: false,
            enable_linter: false,
            enable_security_scan: false,
            enable_format_check: false,
            build_release: false,
        };
        let workflow = preset.to_github().unwrap();

        assert_eq!(workflow.name, "CI");
        assert!(workflow.jobs.contains_key("rust/test"));
        assert!(!workflow.jobs.contains_key("rust/lint"));
    }

    #[test]
    fn test_to_github_with_lint() {
        let preset = Rust {
            rust_version: "stable".to_string(),
            enable_coverage: false,
            enable_linter: true,
            enable_security_scan: false,
            enable_format_check: false,
            build_release: false,
        };
        let workflow = preset.to_github().unwrap();

        assert!(workflow.jobs.contains_key("rust/test"));
        assert!(workflow.jobs.contains_key("rust/lint"));
    }
}

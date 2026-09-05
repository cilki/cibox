use cibox_macros::Preset;

mod circleci;
mod gitea;
mod github;
mod gitlab;
mod jenkins;

/// CI pipeline for Go applications with testing and linting
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Preset)]
#[preset(category = "Languages")]
#[serde(default)]
pub struct GoApp {
    #[preset_field(hidden = true)]
    pub(super) go_version: String,

    /// Run golangci-lint for code quality
    #[preset_field(display = "Enable Linter")]
    pub(super) enable_linter: bool,

    /// Run gosec for security vulnerabilities
    #[preset_field(display = "Security Scan")]
    pub(super) enable_security_scan: bool,
}

impl Default for GoApp {
    fn default() -> Self {
        Self {
            go_version: "1.21".to_string(),
            enable_linter: true,
            enable_security_scan: true,
        }
    }
}

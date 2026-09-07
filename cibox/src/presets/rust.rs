use crate::ir::{Job, Stage, Step, ToJobs};
use cibox_macros::Preset;

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

impl Rust {
    fn image(&self) -> String {
        // "stable" is not a docker tag; the official image tracks stable
        match self.rust_version.as_str() {
            "stable" => "rust:latest".to_string(),
            version => format!("rust:{version}"),
        }
    }
}

impl ToJobs for Rust {
    fn jobs(&self) -> Vec<Job> {
        let mut jobs = Vec::new();

        let mut test_steps = vec![
            Step::checkout(),
            Step::run("Run tests", "cargo test --all-features"),
        ];
        if self.enable_coverage {
            test_steps.push(Step::run(
                "Install tarpaulin",
                "cargo install cargo-tarpaulin",
            ));
            test_steps.push(Step::run(
                "Generate coverage",
                "cargo tarpaulin --out Xml --all-features",
            ));
        }
        let mut test = Job::new("test", "Test", Stage::Test)
            .with_image(self.image())
            .with_timeout(30)
            .with_cache(
                "rust-cache",
                vec!["target/".to_string(), ".cargo/".to_string()],
            )
            .with_steps(test_steps);
        if self.enable_coverage {
            test = test.with_artifacts(vec!["cobertura.xml".to_string()]);
        }
        jobs.push(test);

        if self.enable_linter {
            jobs.push(
                Job::new("lint", "Lint", Stage::Lint)
                    .with_image(self.image())
                    .with_timeout(15)
                    .with_cache(
                        "rust-cache",
                        vec!["target/".to_string(), ".cargo/".to_string()],
                    )
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run("Run clippy", "cargo clippy --all-features -- -D warnings"),
                    ]),
            );
        }

        if self.enable_format_check {
            jobs.push(
                Job::new("format", "Format Check", Stage::Lint)
                    .with_image(self.image())
                    .with_timeout(10)
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run("Check formatting", "cargo fmt -- --check"),
                    ]),
            );
        }

        if self.enable_security_scan {
            jobs.push(
                Job::new("security", "Security Audit", Stage::Security)
                    .with_image(self.image())
                    .with_timeout(10)
                    .with_cache("cargo-audit-cache", vec![".cargo/".to_string()])
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run("Install cargo-audit", "cargo install cargo-audit"),
                        Step::run("Run audit", "cargo audit"),
                    ]),
            );
        }

        if self.build_release {
            jobs.push(
                Job::new("build", "Build Release", Stage::Build)
                    .with_image(self.image())
                    .with_timeout(30)
                    .with_cache("rust-cache", vec!["target/".to_string()])
                    .with_artifacts(vec!["target/release/".to_string()])
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run("Build release", "cargo build --release"),
                    ]),
            );
        }

        jobs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default() {
        let preset = Rust::default();
        assert_eq!(preset.rust_version, "stable");
        assert!(!preset.enable_coverage);
        assert!(!preset.enable_linter);
    }

    #[test]
    fn test_basic_produces_only_test_job() {
        let jobs = Rust::default().jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "test");
        assert_eq!(jobs[0].image.as_deref(), Some("rust:latest"));
    }

    #[test]
    fn test_versioned_image() {
        let preset = Rust {
            rust_version: "1.75.0".to_string(),
            ..Rust::default()
        };
        assert_eq!(preset.jobs()[0].image.as_deref(), Some("rust:1.75.0"));
    }

    #[test]
    fn test_optional_jobs() {
        let preset = Rust {
            enable_coverage: true,
            enable_linter: true,
            enable_security_scan: true,
            enable_format_check: true,
            build_release: true,
            ..Rust::default()
        };
        let ids: Vec<String> = preset.jobs().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["test", "lint", "format", "security", "build"]);
    }

    #[test]
    fn test_coverage_adds_artifacts() {
        let preset = Rust {
            enable_coverage: true,
            ..Rust::default()
        };
        assert_eq!(preset.jobs()[0].artifacts, vec!["cobertura.xml"]);
    }
}

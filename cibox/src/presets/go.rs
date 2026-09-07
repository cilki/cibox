use crate::ir::{Job, Stage, Step, ToJobs};
use cibox_macros::Preset;

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

impl GoApp {
    fn image(&self) -> String {
        format!("golang:{}", self.go_version)
    }
}

impl ToJobs for GoApp {
    fn jobs(&self) -> Vec<Job> {
        let mut jobs = vec![Job::new("test", "Test", Stage::Test)
            .with_image(self.image())
            .with_timeout(30)
            .with_cache("go-cache", vec!["/go/pkg/mod".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Download dependencies", "go mod download"),
                Step::run("Run tests", "go test -v ./..."),
                Step::run("Build", "go build -v ./..."),
            ])];

        if self.enable_linter {
            jobs.push(
                Job::new("lint", "Lint", Stage::Lint)
                    .with_image(self.image())
                    .with_timeout(15)
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run(
                            "Install golangci-lint",
                            "go install github.com/golangci/golangci-lint/cmd/golangci-lint@latest",
                        ),
                        Step::run("Run golangci-lint", "golangci-lint run"),
                    ]),
            );
        }

        if self.enable_security_scan {
            jobs.push(
                Job::new("security", "Security Scan", Stage::Security)
                    .with_image(self.image())
                    .with_timeout(10)
                    .with_steps(vec![
                        Step::checkout(),
                        Step::run(
                            "Install gosec",
                            "go install github.com/securego/gosec/v2/cmd/gosec@latest",
                        ),
                        Step::run("Run gosec", "gosec ./..."),
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
    fn test_default_produces_all_jobs() {
        let ids: Vec<String> = GoApp::default().jobs().iter().map(|j| j.id.clone()).collect();
        assert_eq!(ids, vec!["test", "lint", "security"]);
    }

    #[test]
    fn test_toggles_disable_jobs() {
        let preset = GoApp {
            enable_linter: false,
            enable_security_scan: false,
            ..GoApp::default()
        };
        assert_eq!(preset.jobs().len(), 1);
    }

    #[test]
    fn test_image_version() {
        assert_eq!(
            GoApp::default().jobs()[0].image.as_deref(),
            Some("golang:1.21")
        );
    }
}

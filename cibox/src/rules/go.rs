use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

const IMAGE: &str = "golang:1.23";

fn is_go(facts: &ProjectFacts) -> bool {
    facts.go.is_some()
}

/// Run the test suite with `go test`
pub struct GoTest;

impl Rule for GoTest {
    fn id(&self) -> &'static str {
        "go-test"
    }

    fn name(&self) -> &'static str {
        "Go test"
    }

    fn description(&self) -> &'static str {
        "Run go test on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_go(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Test)
            .with_image(IMAGE)
            .with_timeout(30)
            .with_cache("go-cache", vec!["/go/pkg/mod".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Download dependencies", "go mod download"),
                Step::run("Run tests", "go test -v ./..."),
            ])]
    }
}

/// Compile all packages
pub struct GoBuild;

impl Rule for GoBuild {
    fn id(&self) -> &'static str {
        "go-build"
    }

    fn name(&self) -> &'static str {
        "Go build"
    }

    fn description(&self) -> &'static str {
        "Compile all packages"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_go(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Build)
            .with_image(IMAGE)
            .with_timeout(15)
            .with_cache("go-cache", vec!["/go/pkg/mod".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Build", "go build -v ./..."),
            ])]
    }
}

/// Lint with golangci-lint
pub struct GoLint;

impl Rule for GoLint {
    fn id(&self) -> &'static str {
        "go-lint"
    }

    fn name(&self) -> &'static str {
        "golangci-lint"
    }

    fn description(&self) -> &'static str {
        "Lint with golangci-lint"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_go(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(IMAGE)
            .with_timeout(15)
            .with_steps(vec![
                Step::checkout(),
                Step::run(
                    "Install golangci-lint",
                    "go install github.com/golangci/golangci-lint/cmd/golangci-lint@latest",
                ),
                Step::run("Run golangci-lint", "golangci-lint run"),
            ])]
    }
}

/// Scan for security problems with gosec
pub struct GoAudit;

impl Rule for GoAudit {
    fn id(&self) -> &'static str {
        "go-audit"
    }

    fn name(&self) -> &'static str {
        "Gosec"
    }

    fn description(&self) -> &'static str {
        "Scan for security problems with gosec"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_go(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Security)
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                Step::run(
                    "Install gosec",
                    "go install github.com/securego/gosec/v2/cmd/gosec@latest",
                ),
                Step::run("Run gosec", "gosec ./..."),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_detect_from_go_mod() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module example.com/m\n\ngo 1.23\n").unwrap();
        let facts = crate::detection::gather_facts(dir.path());
        assert!(GoTest.detect(&facts));
        assert!(GoBuild.detect(&facts));
        assert!(GoLint.detect(&facts));
        assert!(GoAudit.detect(&facts));
        assert!(!GoTest.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_job_shapes() {
        let jobs = GoTest.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "go-test");
        assert_eq!(jobs[0].image.as_deref(), Some("golang:1.23"));
        assert_eq!(GoBuild.jobs(&ProjectFacts::default())[0].stage, Stage::Build);
    }
}

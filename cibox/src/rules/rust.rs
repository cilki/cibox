use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// The official image tracks stable; "rust:stable" is not a docker tag
const IMAGE: &str = "rust:latest";

fn is_rust(facts: &ProjectFacts) -> bool {
    facts.rust.is_some()
}

/// Run the test suite with `cargo test`
pub struct RustTest;

impl Rule for RustTest {
    fn id(&self) -> &'static str {
        "rust-test"
    }

    fn name(&self) -> &'static str {
        "Cargo test"
    }

    fn description(&self) -> &'static str {
        "Run cargo test with all features on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Test)
            .with_image(IMAGE)
            .with_timeout(30)
            .with_cache(
                "rust-cache",
                vec!["target/".to_string(), ".cargo/".to_string()],
            )
            .with_steps(vec![
                Step::checkout(),
                Step::run("Run tests", "cargo test --all-features"),
            ])]
    }
}

/// Check formatting with rustfmt
pub struct RustFmt;

impl Rule for RustFmt {
    fn id(&self) -> &'static str {
        "rust-fmt"
    }

    fn name(&self) -> &'static str {
        "Rustfmt"
    }

    fn description(&self) -> &'static str {
        "Check code formatting with cargo fmt"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                // The official rust image ships rustup's minimal profile
                Step::run("Install rustfmt", "rustup component add rustfmt"),
                Step::run("Check formatting", "cargo fmt -- --check"),
            ])]
    }
}

/// Lint with clippy, denying warnings
pub struct RustClippy;

impl Rule for RustClippy {
    fn id(&self) -> &'static str {
        "rust-clippy"
    }

    fn name(&self) -> &'static str {
        "Clippy"
    }

    fn description(&self) -> &'static str {
        "Run clippy with warnings denied"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(IMAGE)
            .with_timeout(15)
            .with_cache(
                "rust-cache",
                vec!["target/".to_string(), ".cargo/".to_string()],
            )
            .with_steps(vec![
                Step::checkout(),
                // The official rust image ships rustup's minimal profile
                Step::run("Install clippy", "rustup component add clippy"),
                Step::run("Run clippy", "cargo clippy --all-features -- -D warnings"),
            ])]
    }
}

/// Audit dependencies for known vulnerabilities
pub struct RustAudit;

impl Rule for RustAudit {
    fn id(&self) -> &'static str {
        "rust-audit"
    }

    fn name(&self) -> &'static str {
        "Cargo audit"
    }

    fn description(&self) -> &'static str {
        "Audit dependencies for known vulnerabilities"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Security)
            .with_image(IMAGE)
            .with_timeout(10)
            .with_cache("cargo-audit-cache", vec![".cargo/".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Install cargo-audit", "cargo install cargo-audit"),
                Step::run("Run audit", "cargo audit"),
            ])]
    }
}

/// Publish to crates.io when a version tag is pushed
pub struct RustRelease;

impl Rule for RustRelease {
    fn id(&self) -> &'static str {
        "rust-release"
    }

    fn name(&self) -> &'static str {
        "Cargo publish"
    }

    fn description(&self) -> &'static str {
        "Publish to crates.io on version tags (requires CARGO_REGISTRY_TOKEN)"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        // Workspaces without a root package need multi-crate publish
        // ordering, which this rule doesn't attempt; force-enable if wanted
        facts.rust.as_ref().is_some_and(|r| r.publishable)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Deploy)
            .with_image(IMAGE)
            .with_timeout(15)
            .tags_only()
            .with_secrets(vec!["CARGO_REGISTRY_TOKEN".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Publish to crates.io", "cargo publish"),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn facts(cargo_toml: &str) -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_rules_detect_any_rust_project() {
        let facts = facts("[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n");
        assert!(RustTest.detect(&facts));
        assert!(RustFmt.detect(&facts));
        assert!(RustClippy.detect(&facts));
        assert!(RustAudit.detect(&facts));
        assert!(!RustTest.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_release_requires_publishable_package() {
        assert!(RustRelease.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"
        )));
        assert!(!RustRelease.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n"
        )));
        assert!(!RustRelease.detect(&facts("[workspace]\nmembers = []\n")));
    }

    #[test]
    fn test_release_job_is_tags_only_with_token() {
        let jobs = RustRelease.jobs(&ProjectFacts::default());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "rust-release");
        assert!(jobs[0].tags_only);
        assert_eq!(jobs[0].stage, Stage::Deploy);
        assert_eq!(jobs[0].secrets, vec!["CARGO_REGISTRY_TOKEN"]);
        assert!(jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "cargo publish")
        ));
    }

    #[test]
    fn test_test_job_shape() {
        let jobs = RustTest.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "rust-test");
        assert_eq!(jobs[0].image.as_deref(), Some("rust:latest"));
        assert!(jobs[0].cache.is_some());
        assert!(!jobs[0].tags_only);
    }
}

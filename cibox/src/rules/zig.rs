use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

fn is_zig(facts: &ProjectFacts) -> bool {
    facts.zig.is_some()
}

/// Compiling jobs share a project-relative cache; the global (package) cache
/// is redirected into the workspace so GitLab can cache it too
fn cached(job: Job) -> Job {
    job.with_env("ZIG_GLOBAL_CACHE_DIR", ".zig-global-cache")
        .with_cache(
            "zig-cache",
            vec![".zig-cache/".to_string(), ".zig-global-cache/".to_string()],
        )
}

/// Run the test suite with `zig build test`
pub struct ZigTest;

impl Rule for ZigTest {
    fn id(&self) -> &'static str {
        "zig-test"
    }

    fn name(&self) -> &'static str {
        "Zig test"
    }

    fn description(&self) -> &'static str {
        "Run zig build test on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_zig(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![cached(
            Job::new(self.id(), self.name(), Stage::Test)
                .with_image(super::CIBOX_IMAGE)
                .with_timeout(30)
                .with_steps(vec![
                    Step::checkout(),
                    Step::run("Run tests", "zig build test"),
                ]),
        )]
    }
}

/// Check formatting with zig fmt
pub struct ZigFmt;

impl Rule for ZigFmt {
    fn id(&self) -> &'static str {
        "zig-fmt"
    }

    fn name(&self) -> &'static str {
        "Zig fmt"
    }

    fn description(&self) -> &'static str {
        "Check formatting with zig fmt"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_zig(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(super::CIBOX_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Check formatting", "zig fmt --check ."),
            ])]
    }
}

/// Compile the default build steps
pub struct ZigBuild;

impl Rule for ZigBuild {
    fn id(&self) -> &'static str {
        "zig-build"
    }

    fn name(&self) -> &'static str {
        "Zig build"
    }

    fn description(&self) -> &'static str {
        "Compile the project with zig build"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_zig(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![cached(
            Job::new(self.id(), self.name(), Stage::Build)
                .with_image(super::CIBOX_IMAGE)
                .with_timeout(15)
                .with_steps(vec![Step::checkout(), Step::run("Build", "zig build")]),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_detect_from_build_zig() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("build.zig"), "pub fn build() void {}\n").unwrap();
        let facts = crate::detection::gather_facts(dir.path());
        assert!(ZigTest.detect(&facts));
        assert!(ZigFmt.detect(&facts));
        assert!(ZigBuild.detect(&facts));
        assert!(!ZigTest.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_job_shapes() {
        let jobs = ZigTest.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "zig-test");
        assert_eq!(jobs[0].image.as_deref(), Some(super::super::CIBOX_IMAGE));
        assert!(jobs[0]
            .env
            .contains(&("ZIG_GLOBAL_CACHE_DIR".into(), ".zig-global-cache".into())));
        assert_eq!(ZigFmt.jobs(&ProjectFacts::default())[0].stage, Stage::Lint);
        assert_eq!(
            ZigBuild.jobs(&ProjectFacts::default())[0].stage,
            Stage::Build
        );
    }
}

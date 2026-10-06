use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

fn is_cmake(facts: &ProjectFacts) -> bool {
    facts.cmake.is_some()
}

/// Run the ctest suite
pub struct CmakeTest;

impl Rule for CmakeTest {
    fn id(&self) -> &'static str {
        "cmake-test"
    }

    fn name(&self) -> &'static str {
        "CTest"
    }

    fn description(&self) -> &'static str {
        "Configure, build, and run ctest"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.cmake.as_ref().is_some_and(|c| c.has_tests)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Test)
            .with_image(super::CIBOX_IMAGE)
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Configure", "cmake -B build -DCMAKE_BUILD_TYPE=Debug"),
                Step::run("Build", "cmake --build build -j$(nproc)"),
                Step::run("Run tests", "ctest --test-dir build --output-on-failure"),
            ])]
    }
}

/// Compile with CMake
pub struct CmakeBuild;

impl Rule for CmakeBuild {
    fn id(&self) -> &'static str {
        "cmake-build"
    }

    fn name(&self) -> &'static str {
        "CMake build"
    }

    fn description(&self) -> &'static str {
        "Configure and build with CMake in Release mode"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_cmake(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Build)
            .with_image(super::CIBOX_IMAGE)
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                Step::run("Configure", "cmake -B build -DCMAKE_BUILD_TYPE=Release"),
                Step::run("Build", "cmake --build build -j$(nproc)"),
            ])]
    }
}

/// Check formatting with clang-format
pub struct CmakeFmt;

impl Rule for CmakeFmt {
    fn id(&self) -> &'static str {
        "cmake-fmt"
    }

    fn name(&self) -> &'static str {
        "clang-format"
    }

    fn description(&self) -> &'static str {
        "Check formatting with clang-format (requires .clang-format)"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.cmake.as_ref().is_some_and(|c| c.has_clang_format)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(super::CIBOX_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                Step::run(
                    "Check formatting",
                    "git ls-files '*.c' '*.cc' '*.cpp' '*.cxx' '*.h' '*.hh' '*.hpp' \
                     | xargs -r clang-format --dry-run -Werror",
                ),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn facts_with(files: &[(&str, &str)]) -> ProjectFacts {
        let dir = tempdir().unwrap();
        for (name, contents) in files {
            fs::write(dir.path().join(name), contents).unwrap();
        }
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_detect_from_cmakelists() {
        let facts = facts_with(&[("CMakeLists.txt", "project(app)\n")]);
        assert!(CmakeBuild.detect(&facts));
        assert!(!CmakeTest.detect(&facts));
        assert!(!CmakeFmt.detect(&facts));
        assert!(!CmakeBuild.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_detect_tests_and_fmt() {
        let facts = facts_with(&[
            ("CMakeLists.txt", "project(app)\nenable_testing()\n"),
            (".clang-format", "BasedOnStyle: LLVM\n"),
        ]);
        assert!(CmakeTest.detect(&facts));
        assert!(CmakeFmt.detect(&facts));
    }

    #[test]
    fn test_job_shapes() {
        let jobs = CmakeTest.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "cmake-test");
        assert_eq!(jobs[0].image.as_deref(), Some(super::super::CIBOX_IMAGE));
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command.starts_with("ctest")
        )));
        assert_eq!(
            CmakeBuild.jobs(&ProjectFacts::default())[0].stage,
            Stage::Build
        );
        assert_eq!(
            CmakeFmt.jobs(&ProjectFacts::default())[0].stage,
            Stage::Lint
        );
    }
}

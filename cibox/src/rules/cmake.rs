use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

fn is_cmake(facts: &ProjectFacts) -> bool {
    facts.cmake.is_some()
}

/// DEBIAN_IMAGE has the compilers but neither cmake nor clang-format
fn apt_install(package: &str) -> Step {
    Step::run(
        format!("Install {package}"),
        format!("apt-get update && apt-get install -y --no-install-recommends {package}"),
    )
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
            .with_image(super::DEBIAN_IMAGE)
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                apt_install("cmake"),
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
            .with_image(super::DEBIAN_IMAGE)
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                apt_install("cmake"),
                Step::run("Configure", "cmake -B build -DCMAKE_BUILD_TYPE=Release"),
                Step::run("Build", "cmake --build build -j$(nproc)"),
            ])]
    }
}

const FMT_COMMAND: &str = "git ls-files -z '*.c' '*.cc' '*.cpp' '*.cxx' '*.h' '*.hh' '*.hpp' \
                           | xargs -0 -r clang-format --dry-run -Werror";

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
            .with_image(super::DEBIAN_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                apt_install("clang-format"),
                // NUL-delimited: newline-separated output gets split on spaces
                // by xargs, and git quotes non-ASCII names as octal escapes,
                // so clang-format would be handed paths that don't exist.
                Step::run("Check formatting", FMT_COMMAND),
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
        assert_eq!(jobs[0].image.as_deref(), Some(super::super::DEBIAN_IMAGE));
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

    /// DEBIAN_IMAGE has the compilers but not the tools the jobs call
    #[test]
    fn test_jobs_install_their_tools_first() {
        let facts = ProjectFacts::default();
        for (job, package) in [
            (&CmakeTest.jobs(&facts)[0], "cmake"),
            (&CmakeBuild.jobs(&facts)[0], "cmake"),
            (&CmakeFmt.jobs(&facts)[0], "clang-format"),
        ] {
            assert_eq!(job.steps[1], apt_install(package), "{}", job.id);
        }
    }

    /// Run the format step in a repo with awkward file names, against a
    /// clang-format stub that fails on any path that doesn't exist and
    /// prints the ones it was handed
    #[test]
    fn test_fmt_step_hands_clang_format_real_paths() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        let bin = dir.path().join("bin");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("clang-format");
        fs::write(
            &stub,
            "#!/bin/sh\nfor f; do case \"$f\" in -*) ;; *) [ -f \"$f\" ] || exit 1; echo \"$f\";; esac; done\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let names = ["src/main.c", "src/my file.c", "src/café.h"];
        for name in names {
            fs::write(repo.join(name), "int x;\n").unwrap();
        }
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);

        let command = CmakeFmt.jobs(&ProjectFacts::default())[0]
            .steps
            .iter()
            .find_map(|step| match step {
                Step::Run { name, command } if name == "Check formatting" => Some(command.clone()),
                _ => None,
            })
            .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new("bash")
            .args(["-eo", "pipefail", "-c", &command])
            .env("PATH", path)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "format step failed: {output:?}");
        let mut seen: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        seen.sort();
        let mut expected: Vec<_> = names.iter().map(|n| n.to_string()).collect();
        expected.sort();
        assert_eq!(seen, expected);
    }
}

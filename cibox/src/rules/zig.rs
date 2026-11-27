use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

fn is_zig(facts: &ProjectFacts) -> bool {
    facts.zig.is_some()
}

/// The zig release the jobs install, pinned because build.zig's API breaks
/// between minor versions; the checksums are ziglang.org's for this release
const ZIG_VERSION: &str = "0.15.1";
const ZIG_SHA256_X86_64: &str = "c61c5da6edeea14ca51ecd5e4520c6f4189ef5250383db33d01848293bfafe05";
const ZIG_SHA256_AARCH64: &str = "bb4a8d2ad735e7fba764c497ddf4243cb129fece4148da3222a7046d3f1f19fe";

/// Zig has no official image, so every job installs the pinned release into
/// DEBIAN_IMAGE
fn install_zig() -> Step {
    Step::run(
        "Install zig",
        format!(
            "case \"$(uname -m)\" in \
             x86_64) arch=x86_64; sum={ZIG_SHA256_X86_64} ;; \
             aarch64) arch=aarch64; sum={ZIG_SHA256_AARCH64} ;; \
             *) echo \"No zig build for $(uname -m)\" >&2; exit 1 ;; esac; \
             curl -fsSL \"https://ziglang.org/download/{ZIG_VERSION}/zig-$arch-linux-{ZIG_VERSION}.tar.xz\" -o /tmp/zig.tar.xz && \
             echo \"$sum  /tmp/zig.tar.xz\" | sha256sum -c - && \
             mkdir -p /usr/local/zig && \
             tar -xJf /tmp/zig.tar.xz -C /usr/local/zig --strip-components=1 && \
             ln -sf /usr/local/zig/zig /usr/local/bin/zig && \
             rm /tmp/zig.tar.xz"
        ),
    )
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
        // `zig build test` fails outright when build.zig declares no such step
        facts.zig.as_ref().is_some_and(|z| z.has_test_step)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![cached(
            Job::new(self.id(), self.name(), Stage::Test)
                .with_image(super::DEBIAN_IMAGE)
                .with_timeout(30)
                .with_steps(vec![
                    Step::checkout(),
                    install_zig(),
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
            .with_image(super::DEBIAN_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                install_zig(),
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
        "Compile with zig build --release=safe and upload zig-out as CI artifacts"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_zig(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![cached(
            Job::new(self.id(), self.name(), Stage::Build)
                .with_image(super::DEBIAN_IMAGE)
                .with_timeout(15)
                // Everything the build script installs lands in zig-out
                .with_artifacts(vec!["zig-out/".to_string()])
                .with_steps(vec![
                    Step::checkout(),
                    install_zig(),
                    Step::run("Build", "zig build --release=safe"),
                ]),
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
        // No test step: `zig build test` would fail with "no step named 'test'"
        assert!(!ZigTest.detect(&facts));
        assert!(ZigFmt.detect(&facts));
        assert!(ZigBuild.detect(&facts));
        assert!(!ZigTest.detect(&ProjectFacts::default()));

        fs::write(
            dir.path().join("build.zig"),
            "pub fn build(b: *std.Build) void {\n    \
             const test_step = b.step(\"test\", \"Run unit tests\");\n}\n",
        )
        .unwrap();
        let facts = crate::detection::gather_facts(dir.path());
        assert!(ZigTest.detect(&facts));
    }

    #[test]
    fn test_job_shapes() {
        let jobs = ZigTest.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "zig-test");
        assert_eq!(jobs[0].image.as_deref(), Some(super::super::DEBIAN_IMAGE));
        assert!(jobs[0]
            .env
            .contains(&("ZIG_GLOBAL_CACHE_DIR".into(), ".zig-global-cache".into())));
        assert_eq!(ZigFmt.jobs(&ProjectFacts::default())[0].stage, Stage::Lint);
        let build = &ZigBuild.jobs(&ProjectFacts::default())[0];
        assert_eq!(build.stage, Stage::Build);
        // Build jobs compile in release mode and upload their outputs
        assert!(build.steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command == "zig build --release=safe"
        )));
        assert_eq!(build.artifacts, vec!["zig-out/"]);
    }

    /// DEBIAN_IMAGE has no zig: every job installs it before running it
    #[test]
    fn test_jobs_install_zig_first() {
        let facts = ProjectFacts::default();
        for job in [
            &ZigTest.jobs(&facts)[0],
            &ZigFmt.jobs(&facts)[0],
            &ZigBuild.jobs(&facts)[0],
        ] {
            let names: Vec<&str> = job
                .steps
                .iter()
                .filter_map(|s| match s {
                    Step::Run { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(names[0], "Install zig", "{}: {names:?}", job.id);
        }
        let Step::Run { command, .. } = install_zig() else {
            unreachable!()
        };
        assert!(command.contains(ZIG_SHA256_X86_64) && command.contains(ZIG_SHA256_AARCH64));
        assert!(command.contains("sha256sum -c"));
    }
}

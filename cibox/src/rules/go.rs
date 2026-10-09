use super::{with_versions, Rule};
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// Default toolchain image. Kept on a supported Go release: the go command
/// honours the `go` directive of whatever it is asked to build, and anything
/// newer than the image makes it fetch a second toolchain mid-job (see the
/// tool pins below), which is one more unpinned download in the pipeline.
pub(crate) const IMAGE: &str = "golang:1.27";

/// Workspace-local directory holding both Go caches
const CACHE_DIR: &str = ".go-cache";

/// The pinned linter, installed by `go-lint`.
///
/// `go install <module>@latest` is a mutable pointer: what a user's pipeline
/// executes changes without any change in their repository and without a diff
/// for anyone to review, so an upstream release that is broken or compromised
/// lands in CI on the next run. A version pin is immutable, the module proxy
/// and checksum database verify it, and an upgrade becomes a reviewable change
/// to this line.
///
/// The pin also repairs the module path. golangci-lint published v2 under
/// `/v2`, and `@latest` on the old path cannot resolve past the last v1 — so
/// the unpinned command has been installing v1.64.8 (March 2025) ever since,
/// and would have kept installing it forever.
const GOLANGCI_LINT: &str = "github.com/golangci/golangci-lint/v2/cmd/golangci-lint@v2.14.0";

/// The pinned security scanner installed by `go-audit`, pinned for the
/// reasons on [`GOLANGCI_LINT`]
const GOSEC: &str = "github.com/securego/gosec/v2/cmd/gosec@v2.29.0";

fn is_go(facts: &ProjectFacts) -> bool {
    facts.go.is_some()
}

/// Declare the shared workspace-local cache. Paired with
/// [`redirect_caches`], which has to run before any other go command.
fn cached(job: Job) -> Job {
    job.with_cache("go-cache", vec![format!("{CACHE_DIR}/")])
}

/// Point the module and build caches into the workspace.
///
/// The golang images keep GOMODCACHE under `/go`, outside the checkout, and
/// GitLab only caches paths inside the project directory — so a `/go/pkg/mod`
/// cache entry never survived a job there. The go tool rejects relative cache
/// directories, so the redirect goes through `go env -w` with an absolute
/// `$PWD` path instead of a plain job env var. Package patterns like `./...`
/// skip dot-directories, so the cache stays invisible to the build itself.
fn redirect_caches() -> Step {
    Step::run(
        "Redirect Go caches into the workspace",
        format!(
            "go env -w GOMODCACHE=\"$PWD/{CACHE_DIR}/mod\" \
             GOCACHE=\"$PWD/{CACHE_DIR}/build\""
        ),
    )
}

/// Run the test suite with `go test`
#[derive(Default)]
pub struct GoTest {
    /// Toolchain versions to matrix over; empty = the default pinned image
    pub versions: Vec<String>,
}

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
        let job = cached(Job::new(self.id(), self.name(), Stage::Test))
            .with_image(IMAGE)
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                redirect_caches(),
                Step::run("Download dependencies", "go mod download"),
                Step::run("Run tests", "go test -v ./..."),
            ]);
        vec![with_versions(job, &self.versions, |v| format!("golang:{v}"))]
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
        vec![cached(Job::new(self.id(), self.name(), Stage::Build))
            .with_image(IMAGE)
            .with_timeout(15)
            .with_steps(vec![
                Step::checkout(),
                redirect_caches(),
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
        vec![cached(Job::new(self.id(), self.name(), Stage::Lint))
            .with_image(IMAGE)
            .with_timeout(15)
            .with_steps(vec![
                Step::checkout(),
                redirect_caches(),
                Step::run(
                    "Install golangci-lint",
                    format!("go install {GOLANGCI_LINT}"),
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
        vec![cached(Job::new(self.id(), self.name(), Stage::Security))
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                redirect_caches(),
                Step::run("Install gosec", format!("go install {GOSEC}")),
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
        assert!(GoTest::default().detect(&facts));
        assert!(GoBuild.detect(&facts));
        assert!(GoLint.detect(&facts));
        assert!(GoAudit.detect(&facts));
        assert!(!GoTest::default().detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_job_shapes() {
        let jobs = GoTest::default().jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "go-test");
        assert_eq!(jobs[0].image.as_deref(), Some(IMAGE));
        assert_eq!(GoBuild.jobs(&ProjectFacts::default())[0].stage, Stage::Build);
    }

    /// Every command the go rules emit, flattened
    fn commands(rule: &dyn Rule) -> Vec<String> {
        rule.jobs(&ProjectFacts::default())
            .iter()
            .flat_map(|job| job.steps.clone())
            .filter_map(|step| match step {
                Step::Run { command, .. } => Some(command),
                Step::Checkout { .. } => None,
            })
            .collect()
    }

    #[test]
    fn test_installed_tools_are_pinned_to_a_version() {
        // `@latest` is a mutable pointer: the pipeline would run whatever
        // upstream published since, with no change in the user's repository
        // and no diff for anyone to review
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(GoTest::default()),
            Box::new(GoBuild),
            Box::new(GoLint),
            Box::new(GoAudit),
        ];
        for rule in rules {
            for command in commands(rule.as_ref()) {
                let Some(module) = command.strip_prefix("go install ") else {
                    continue;
                };
                let (_, version) = module
                    .rsplit_once('@')
                    .unwrap_or_else(|| panic!("{}: {command} names no version", rule.id()));
                assert!(
                    version.starts_with('v')
                        && version[1..].starts_with(|c: char| c.is_ascii_digit()),
                    "{}: {command} is not pinned to a release",
                    rule.id()
                );
            }
        }
    }

    #[test]
    fn test_golangci_lint_comes_from_the_v2_module_path() {
        // v2 lives under a new module path, so the old one can only ever
        // resolve to the final v1 release
        let install = commands(&GoLint)
            .into_iter()
            .find(|c| c.starts_with("go install "))
            .expect("go-lint installs golangci-lint");
        assert!(
            install.contains("/golangci-lint/v2/cmd/golangci-lint@v2."),
            "{install}"
        );
    }

    #[test]
    fn test_caches_live_in_the_workspace() {
        // GitLab drops cache entries outside the project directory, so every
        // go job has to redirect GOMODCACHE/GOCACHE before it runs anything
        // else and cache the workspace-local directory it redirected them to.
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(GoTest::default()),
            Box::new(GoBuild),
            Box::new(GoLint),
            Box::new(GoAudit),
        ];
        for rule in rules {
            let jobs = rule.jobs(&ProjectFacts::default());
            let cache = jobs[0].cache.as_ref().unwrap_or_else(|| {
                panic!("{} has no cache", rule.id());
            });
            assert_eq!(cache.paths, vec![".go-cache/".to_string()], "{}", rule.id());

            let commands: Vec<&str> = jobs[0]
                .steps
                .iter()
                .filter_map(|s| match s {
                    Step::Run { command, .. } => Some(command.as_str()),
                    Step::Checkout { .. } => None,
                })
                .collect();
            assert!(
                commands[0].starts_with("go env -w ")
                    && commands[0].contains("GOMODCACHE=\"$PWD/.go-cache/mod\"")
                    && commands[0].contains("GOCACHE=\"$PWD/.go-cache/build\""),
                "{} does not redirect its caches first: {commands:?}",
                rule.id()
            );
        }
    }

    #[test]
    fn test_versions_pin_or_matrix() {
        let one = GoTest {
            versions: vec!["1.24".to_string()],
        };
        let jobs = one.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image.as_deref(), Some("golang:1.24"));
        assert_eq!(jobs[0].matrix, None);

        let two = GoTest {
            versions: vec!["1.23".to_string(), "1.24".to_string()],
        };
        let jobs = two.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image, None);
        let entries = jobs[0].matrix.as_ref().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].version, "1.23");
        assert_eq!(entries[0].image, "golang:1.23");
        assert_eq!(entries[1].image, "golang:1.24");
    }
}

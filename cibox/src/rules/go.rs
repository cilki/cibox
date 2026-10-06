use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, MatrixEntry, Stage, Step};

const IMAGE: &str = "golang:1.23";

/// Workspace-local directory holding both Go caches
const CACHE_DIR: &str = ".go-cache";

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
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                redirect_caches(),
                Step::run("Download dependencies", "go mod download"),
                Step::run("Run tests", "go test -v ./..."),
            ]);
        vec![match self.versions.as_slice() {
            [] => job.with_image(IMAGE),
            [v] => job.with_image(format!("golang:{v}")),
            vs => job.with_matrix(
                vs.iter()
                    .map(|v| MatrixEntry {
                        version: v.clone(),
                        image: format!("golang:{v}"),
                    })
                    .collect(),
            ),
        }]
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
        vec![cached(Job::new(self.id(), self.name(), Stage::Security))
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                redirect_caches(),
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
        assert_eq!(jobs[0].image.as_deref(), Some("golang:1.23"));
        assert_eq!(GoBuild.jobs(&ProjectFacts::default())[0].stage, Stage::Build);
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

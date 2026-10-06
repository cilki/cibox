//! Platform-neutral intermediate representation of CI jobs.
//!
//! Rules describe their jobs once as [`Job`]s; the per-platform backends in
//! `crate::platforms` lower them into each platform's config model.

/// Pipeline phase; variant order is the GitLab stage order
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Test,
    Lint,
    Security,
    Build,
    Deploy,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Test => "test",
            Stage::Lint => "lint",
            Stage::Security => "security",
            Stage::Build => "build",
            Stage::Deploy => "deploy",
        }
    }
}

/// Operating system of the runner a job needs. Windows is honored only by
/// GitHub/Gitea lowering (`runs-on: windows-latest`); the other platforms
/// refuse to generate windows jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerOs {
    #[default]
    Linux,
    Windows,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cache {
    pub key: String,
    pub paths: Vec<String>,
}

/// One leg of a toolchain-version matrix: the user-facing version string and
/// the container image that provides it
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixEntry {
    pub version: String,
    pub image: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Fetch the repository. Implicit on GitLab; `full_history`
    /// disables shallow cloning where the platform defaults to it.
    Checkout { full_history: bool },
    /// Run a shell command
    Run { name: String, command: String },
}

impl Step {
    pub fn checkout() -> Self {
        Step::Checkout {
            full_history: false,
        }
    }

    pub fn checkout_full_history() -> Self {
        Step::Checkout { full_history: true }
    }

    pub fn run(name: impl Into<String>, command: impl Into<String>) -> Self {
        Step::Run {
            name: name.into(),
            command: command.into(),
        }
    }
}

/// Platform-neutral description of one CI job
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// Unique job key, by convention the id of the rule that produced it
    /// (e.g. "rust-test")
    pub id: String,
    /// Human name used for step grouping
    pub name: String,
    pub stage: Stage,
    /// Container image to run in; None = the platform's default runner
    pub image: Option<String>,
    /// Job invokes the docker CLI and needs a daemon: GitHub runs on the host
    /// runner, GitLab uses docker:latest, CircleCI adds setup_remote_docker
    pub needs_docker: bool,
    pub steps: Vec<Step>,
    pub timeout_minutes: Option<u32>,
    /// Only run for `v*` version tags. The backends gate it their own way:
    /// GitHub/Gitea move the job to `release.yml`, GitLab adds a
    /// `$CI_COMMIT_TAG` rule, CircleCI a tag filter.
    pub tags_only: bool,
    /// Toolchain versions to run this job against, lowered to the platform's
    /// native job matrix. Replaces `image`; the backends substitute each
    /// entry's image in their own interpolation syntax and suffix the cache
    /// key with the version. Incompatible with `needs_docker` and `artifacts`
    /// (artifact names would collide across matrix legs).
    pub matrix: Option<Vec<MatrixEntry>>,
    pub cache: Option<Cache>,
    /// Paths preserved as build artifacts
    pub artifacts: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Secret names the job reads as env vars. GitHub maps each to
    /// `${{ secrets.NAME }}`; other platforms expect CI-level variables.
    pub secrets: Vec<String>,
    /// Extra scopes the job's ambient CI token needs, beyond reading the
    /// repository (e.g. `("packages", "write")` to push to ghcr.io). Honored
    /// by the GitHub/Gitea backends, which otherwise emit a read-only token;
    /// the other platforms have no per-job token scoping.
    pub permissions: Vec<(String, String)>,
    /// Ids of other jobs this one depends on
    pub needs: Vec<String>,
    /// Runner operating system; Linux unless the job can only run on Windows
    pub runs_on: RunnerOs,
}

impl Job {
    pub fn new(id: impl Into<String>, name: impl Into<String>, stage: Stage) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            stage,
            image: None,
            needs_docker: false,
            steps: Vec::new(),
            timeout_minutes: None,
            tags_only: false,
            matrix: None,
            cache: None,
            artifacts: Vec::new(),
            env: Vec::new(),
            secrets: Vec::new(),
            permissions: Vec::new(),
            needs: Vec::new(),
            runs_on: RunnerOs::default(),
        }
    }

    pub fn with_image(mut self, image: impl Into<String>) -> Self {
        self.image = Some(image.into());
        self
    }

    pub fn with_docker(mut self) -> Self {
        self.needs_docker = true;
        self
    }

    pub fn with_steps(mut self, steps: Vec<Step>) -> Self {
        self.steps = steps;
        self
    }

    pub fn with_timeout(mut self, minutes: u32) -> Self {
        self.timeout_minutes = Some(minutes);
        self
    }

    pub fn tags_only(mut self) -> Self {
        self.tags_only = true;
        self
    }

    /// Replaces any pinned image: each leg brings its own.
    pub fn with_matrix(mut self, entries: Vec<MatrixEntry>) -> Self {
        debug_assert!(!self.needs_docker, "matrix jobs run in containers");
        self.image = None;
        self.matrix = Some(entries);
        self
    }

    pub fn with_cache(mut self, key: impl Into<String>, paths: Vec<String>) -> Self {
        self.cache = Some(Cache {
            key: key.into(),
            paths,
        });
        self
    }

    pub fn with_artifacts(mut self, paths: Vec<String>) -> Self {
        self.artifacts = paths;
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn with_secrets(mut self, secrets: Vec<String>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn with_permission(mut self, scope: impl Into<String>, level: impl Into<String>) -> Self {
        self.permissions.push((scope.into(), level.into()));
        self
    }

    pub fn with_needs(mut self, needs: Vec<String>) -> Self {
        self.needs = needs;
        self
    }

    pub fn on_windows(mut self) -> Self {
        self.runs_on = RunnerOs::Windows;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_order() {
        assert!(Stage::Test < Stage::Lint);
        assert!(Stage::Security < Stage::Build);
        assert!(Stage::Build < Stage::Deploy);
    }

    #[test]
    fn test_with_matrix_replaces_the_pinned_image() {
        let job = Job::new("a", "A", Stage::Test).with_image("rust:latest");
        assert_eq!(job.matrix, None);
        let entries = vec![MatrixEntry {
            version: "1.90".to_string(),
            image: "rust:1.90".to_string(),
        }];
        let job = job.with_matrix(entries.clone());
        assert_eq!(job.matrix, Some(entries));
        assert_eq!(job.image, None);
    }
}

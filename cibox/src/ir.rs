//! Platform-neutral intermediate representation of CI jobs.
//!
//! Presets describe their jobs once as [`Job`]s; the lowering layer in
//! `crate::platforms::lower` turns them into each platform's config model.

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

#[derive(Debug, Clone, PartialEq)]
pub struct Cache {
    pub key: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Fetch the repository. Implicit on GitLab/Jenkins; `full_history`
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
    /// Short id, unique within the preset (e.g. "test"). The lowering layer
    /// prefixes it with the preset slug ("rust-test").
    pub id: String,
    /// Human name used for Jenkins stage names and step grouping
    pub name: String,
    pub stage: Stage,
    /// Container image to run in; None = the platform's default runner
    pub image: Option<String>,
    /// Job invokes the docker CLI and needs a daemon: GitHub runs on the host
    /// runner, GitLab uses docker:latest, CircleCI adds setup_remote_docker,
    /// Jenkins runs directly on the agent
    pub needs_docker: bool,
    pub steps: Vec<Step>,
    pub timeout_minutes: Option<u32>,
    /// Only run for version tags (GitLab only:refs:[tags]; adds a v* tag
    /// trigger on GitHub)
    pub tags_only: bool,
    pub cache: Option<Cache>,
    /// Paths preserved as build artifacts (no-op on Jenkins)
    pub artifacts: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Secret names the job reads as env vars. GitHub maps each to
    /// `${{ secrets.NAME }}`; other platforms expect CI-level variables.
    pub secrets: Vec<String>,
    /// Ids of other jobs in the same preset this one depends on
    pub needs: Vec<String>,
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
            cache: None,
            artifacts: Vec::new(),
            env: Vec::new(),
            secrets: Vec::new(),
            needs: Vec::new(),
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

    pub fn with_secrets(mut self, secrets: Vec<String>) -> Self {
        self.secrets = secrets;
        self
    }
}

/// What each preset produces
pub trait ToJobs {
    fn jobs(&self) -> Vec<Job>;
}

/// A preset's jobs plus provenance, the unit the lowering layer consumes
#[derive(Debug, Clone)]
pub struct PresetJobs {
    /// e.g. "Rust", "PythonApp"
    pub preset_id: String,
    /// e.g. "Rust", "Python App" — used for Jenkins stage prefixes
    pub display_name: String,
    /// Job-key prefix, e.g. "rust", "python"
    pub slug: String,
    pub jobs: Vec<Job>,
}

impl PresetJobs {
    pub fn new(preset_id: &str, display_name: &str, jobs: Vec<Job>) -> Self {
        Self {
            preset_id: preset_id.to_string(),
            display_name: display_name.to_string(),
            slug: slug_of(preset_id),
            jobs,
        }
    }
}

/// Lowercased first CamelCase word of a preset id: "PythonApp" → "python"
pub fn slug_of(preset_id: &str) -> String {
    let end = preset_id
        .char_indices()
        .skip(1)
        .find(|(_, c)| c.is_uppercase())
        .map(|(i, _)| i)
        .unwrap_or(preset_id.len());
    preset_id[..end].to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_of() {
        assert_eq!(slug_of("Rust"), "rust");
        assert_eq!(slug_of("PythonApp"), "python");
        assert_eq!(slug_of("GoApp"), "go");
        assert_eq!(slug_of("Gitleaks"), "gitleaks");
    }

    #[test]
    fn test_stage_order() {
        assert!(Stage::Test < Stage::Lint);
        assert!(Stage::Security < Stage::Build);
        assert!(Stage::Build < Stage::Deploy);
    }
}

use crate::ir::{Job, Stage, Step, ToJobs};
use cibox_macros::Preset;

/// Container registry options for Docker image pushing
#[derive(
    Debug,
    Clone,
    PartialEq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    strum::VariantNames,
)]
#[serde(rename_all = "lowercase")]
pub enum DockerRegistry {
    /// Push to Docker Hub (requires DOCKER_USERNAME and DOCKER_PASSWORD secrets)
    #[strum(serialize = "dockerhub")]
    DockerHub,
    /// Push to GitHub Container Registry (uses GITHUB_TOKEN)
    #[strum(serialize = "github")]
    GitHubRegistry,
    /// Don't push images (build only)
    #[default]
    #[strum(serialize = "none")]
    None,
}

impl DockerRegistry {
    fn login_command(&self) -> Option<&'static str> {
        match self {
            DockerRegistry::DockerHub => {
                Some("echo $DOCKER_PASSWORD | docker login -u $DOCKER_USERNAME --password-stdin")
            }
            DockerRegistry::GitHubRegistry => Some(
                "echo $GITHUB_TOKEN | docker login ghcr.io -u $GITHUB_USERNAME --password-stdin",
            ),
            DockerRegistry::None => None,
        }
    }

    fn secrets(&self) -> Vec<String> {
        match self {
            DockerRegistry::DockerHub => {
                vec!["DOCKER_USERNAME".to_string(), "DOCKER_PASSWORD".to_string()]
            }
            DockerRegistry::GitHubRegistry => {
                vec!["GITHUB_USERNAME".to_string(), "GITHUB_TOKEN".to_string()]
            }
            DockerRegistry::None => Vec::new(),
        }
    }
}

/// CI pipeline for building and pushing Docker images to registries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Preset)]
#[preset(category = "Packaging")]
#[serde(default)]
pub struct Docker {
    /// Docker image name (e.g., "myorg/myapp")
    #[preset_field(display = "Image Name")]
    pub(super) image_name: String,

    /// Choose where to push Docker images
    #[preset_field(display = "Registry Type")]
    pub(super) registry: DockerRegistry,

    /// Dockerfile path (default: "./Dockerfile")
    #[preset_field(hidden = true)]
    pub(super) dockerfile_path: String,

    /// Docker build context (default: ".")
    #[preset_field(hidden = true)]
    pub(super) build_context: String,

    /// Use the last pushed image as a layer cache for faster builds
    #[preset_field(display = "Enable Cache")]
    pub(super) enable_cache: bool,

    /// Only push images on git tags (not on branch pushes)
    #[preset_field(display = "Tags Only")]
    pub(super) push_on_tags_only: bool,
}

impl Default for Docker {
    fn default() -> Self {
        Self {
            image_name: "myapp".to_string(),
            registry: DockerRegistry::None,
            dockerfile_path: "./Dockerfile".to_string(),
            build_context: ".".to_string(),
            enable_cache: true,
            push_on_tags_only: false,
        }
    }
}

impl ToJobs for Docker {
    fn jobs(&self) -> Vec<Job> {
        let mut steps = vec![Step::checkout()];

        if let Some(login) = self.registry.login_command() {
            steps.push(Step::run("Login to registry", login));
        }

        // Layer caching needs a previously pushed image to pull from
        let use_cache = self.enable_cache && self.registry != DockerRegistry::None;
        if use_cache {
            steps.push(Step::run(
                "Pull cache image",
                format!("docker pull {} || true", self.image_name),
            ));
        }

        let cache_flag = if use_cache {
            format!(" --cache-from {}", self.image_name)
        } else {
            String::new()
        };
        steps.push(Step::run(
            "Build image",
            format!(
                "docker build{} -t {} -f {} {}",
                cache_flag, self.image_name, self.dockerfile_path, self.build_context
            ),
        ));

        if self.registry != DockerRegistry::None {
            steps.push(Step::run(
                "Push image",
                format!("docker push {}", self.image_name),
            ));
        }

        let mut job = Job::new("build", "Docker Build", Stage::Build)
            .with_docker()
            .with_timeout(30)
            .with_secrets(self.registry.secrets())
            .with_steps(steps);
        if self.push_on_tags_only {
            job = job.tags_only();
        }
        vec![job]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_builds_without_push() {
        let jobs = Docker::default().jobs();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].needs_docker);
        assert!(jobs[0].secrets.is_empty());
        assert!(!jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.starts_with("docker push"))
        ));
    }

    #[test]
    fn test_dockerhub_login_and_push() {
        let preset = Docker {
            registry: DockerRegistry::DockerHub,
            image_name: "fossable/cibox".to_string(),
            ..Docker::default()
        };
        let job = &preset.jobs()[0];
        assert_eq!(job.secrets, vec!["DOCKER_USERNAME", "DOCKER_PASSWORD"]);
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.contains("docker login") && !command.contains("ghcr.io"))
        ));
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "docker push fossable/cibox")
        ));
    }

    #[test]
    fn test_cache_from_previous_image() {
        let preset = Docker {
            registry: DockerRegistry::DockerHub,
            ..Docker::default()
        };
        let job = &preset.jobs()[0];
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.contains("--cache-from myapp"))
        ));
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "docker pull myapp || true")
        ));
    }

    #[test]
    fn test_no_cache_without_registry() {
        let job = &Docker::default().jobs()[0];
        assert!(!job
            .steps
            .iter()
            .any(|s| matches!(s, Step::Run { command, .. } if command.contains("--cache-from"))));
    }

    #[test]
    fn test_tags_only() {
        let preset = Docker {
            push_on_tags_only: true,
            ..Docker::default()
        };
        assert!(preset.jobs()[0].tags_only);
    }
}

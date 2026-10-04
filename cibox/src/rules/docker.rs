use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

fn dockerfile_flag(facts: &ProjectFacts) -> String {
    match &facts.docker {
        Some(d) if d.dockerfile != "Dockerfile" => format!(" -f {}", d.dockerfile),
        _ => String::new(),
    }
}

/// Build the Dockerfile on every push
pub struct DockerBuild {
    /// Image name, e.g. "fossable/cibox"
    pub image: String,
}

impl Rule for DockerBuild {
    fn id(&self) -> &'static str {
        "docker-build"
    }

    fn name(&self) -> &'static str {
        "Docker build"
    }

    fn description(&self) -> &'static str {
        "Build the Dockerfile on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.docker.is_some()
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Build)
            .with_docker()
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                Step::run(
                    "Build image",
                    format!("docker build{} -t {} .", dockerfile_flag(facts), self.image),
                ),
            ])]
    }
}

/// Push the image to a registry when a version tag is pushed.
/// The registry is inferred from the image name: a `ghcr.io/` prefix logs in
/// to the GitHub Container Registry with GITHUB_TOKEN, anything else uses
/// Docker Hub credentials.
pub struct DockerRelease {
    pub image: String,
}

impl DockerRelease {
    fn uses_ghcr(&self) -> bool {
        self.image.starts_with("ghcr.io/")
    }

    fn login_command(&self) -> String {
        if self.uses_ghcr() {
            "echo $GITHUB_TOKEN | docker login ghcr.io -u $GITHUB_ACTOR --password-stdin"
                .to_string()
        } else {
            "echo $DOCKER_PASSWORD | docker login -u $DOCKER_USERNAME --password-stdin".to_string()
        }
    }

    fn secrets(&self) -> Vec<String> {
        if self.uses_ghcr() {
            vec!["GITHUB_TOKEN".to_string()]
        } else {
            vec!["DOCKER_USERNAME".to_string(), "DOCKER_PASSWORD".to_string()]
        }
    }
}

impl Rule for DockerRelease {
    fn id(&self) -> &'static str {
        "docker-release"
    }

    fn name(&self) -> &'static str {
        "Docker push"
    }

    fn description(&self) -> &'static str {
        "Build and push the image to a registry on version tags"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.docker.is_some()
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Deploy)
            .with_docker()
            .with_timeout(30)
            .tags_only()
            .with_secrets(self.secrets())
            .with_steps(vec![
                Step::checkout(),
                Step::run("Login to registry", self.login_command()),
                Step::run(
                    "Build image",
                    format!("docker build{} -t {} .", dockerfile_flag(facts), self.image),
                ),
                Step::run("Push image", format!("docker push {}", self.image)),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn docker_facts() -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_detect_requires_dockerfile() {
        let rule = DockerBuild {
            image: "a/b".to_string(),
        };
        assert!(rule.detect(&docker_facts()));
        assert!(!rule.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_build_does_not_push() {
        let rule = DockerBuild {
            image: "owner/app".to_string(),
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(job.needs_docker);
        assert!(!job.tags_only);
        assert!(job.secrets.is_empty());
        assert!(!job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.starts_with("docker push"))
        ));
    }

    #[test]
    fn test_release_pushes_on_tags_with_dockerhub() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(job.tags_only);
        assert_eq!(job.secrets, vec!["DOCKER_USERNAME", "DOCKER_PASSWORD"]);
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "docker push owner/app")
        ));
    }

    #[test]
    fn test_ghcr_image_uses_github_token() {
        let rule = DockerRelease {
            image: "ghcr.io/owner/app".to_string(),
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert_eq!(job.secrets, vec!["GITHUB_TOKEN"]);
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.contains("docker login ghcr.io"))
        ));
    }
}

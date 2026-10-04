use super::Rule;
use crate::config::DockerPlatform;
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
///
/// With `platforms` set, linux targets build in a single buildx+QEMU job.
/// `WindowsAmd64` cannot be emulated, so it gets its own Windows-runner job
/// (the same Dockerfile must support a Windows base image); when linux and
/// windows are mixed, both jobs push staging tags and a final job merges them
/// into one multi-platform manifest. Staging tags are appended to `image`, so
/// the image name must not already carry a tag.
pub struct DockerRelease {
    pub image: String,
    /// Target platforms; empty = single-arch build/push on the host runner
    pub platforms: Vec<DockerPlatform>,
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

    /// Selected platforms deduped and in `DockerPlatform::ALL` order
    fn normalized_platforms(&self) -> Vec<DockerPlatform> {
        DockerPlatform::ALL
            .iter()
            .copied()
            .filter(|p| self.platforms.contains(p))
            .collect()
    }

    /// Steps for a buildx job that builds `linux` platforms and pushes `tag`
    fn buildx_steps(
        &self,
        facts: &ProjectFacts,
        linux: &[DockerPlatform],
        tag: &str,
    ) -> Vec<Step> {
        let mut steps = vec![
            Step::checkout(),
            Step::run("Login to registry", self.login_command()),
        ];
        // QEMU is only needed to emulate foreign architectures
        if linux.iter().any(|p| *p != DockerPlatform::LinuxAmd64) {
            steps.push(Step::run(
                "Set up QEMU",
                "docker run --privileged --rm tonistiigi/binfmt --install all",
            ));
        }
        steps.push(Step::run("Set up buildx", "docker buildx create --use"));
        let platform_list = linux
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(",");
        steps.push(Step::run(
            "Build and push image",
            format!(
                "docker buildx build --platform {platform_list}{} -t {tag} --push .",
                dockerfile_flag(facts)
            ),
        ));
        steps
    }

    /// Steps for the Windows-runner job: a plain build/push of `tag`
    fn windows_steps(&self, facts: &ProjectFacts, tag: &str) -> Vec<Step> {
        vec![
            Step::checkout(),
            Step::run("Login to registry", self.login_command()),
            Step::run(
                "Build image",
                format!("docker build{} -t {tag} .", dockerfile_flag(facts)),
            ),
            Step::run("Push image", format!("docker push {tag}")),
        ]
    }

    fn release_job(&self, suffix: &str, name: &str) -> Job {
        let id = format!("{}{suffix}", self.id());
        Job::new(id, name, Stage::Deploy)
            .with_docker()
            .tags_only()
            .with_secrets(self.secrets())
    }
}

impl Rule for DockerRelease {
    fn id(&self) -> &'static str {
        "docker-release"
    }

    fn owns_job_id(&self, id: &str) -> bool {
        // Per-OS variant jobs emitted when `platforms` mixes Linux and Windows
        id == self.id() || id == "docker-release-linux" || id == "docker-release-windows"
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
        let platforms = self.normalized_platforms();
        let (windows, linux): (Vec<DockerPlatform>, Vec<DockerPlatform>) =
            platforms.into_iter().partition(|p| p.is_windows());

        match (linux.is_empty(), windows.is_empty()) {
            // No platform selection: single-arch build/push on the host runner
            (true, true) => {
                vec![self
                    .release_job("", self.name())
                    .with_timeout(30)
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
            // Linux only: one buildx job pushes the multi-arch manifest
            (false, true) => {
                vec![self
                    .release_job("", self.name())
                    .with_timeout(60)
                    .with_steps(self.buildx_steps(facts, &linux, &self.image))]
            }
            // Windows only: one plain build/push on a Windows runner
            (true, false) => {
                vec![self
                    .release_job("", self.name())
                    .on_windows()
                    .with_timeout(30)
                    .with_steps(self.windows_steps(facts, &self.image))]
            }
            // Mixed: stage per-OS images, then merge into one manifest
            (false, false) => {
                let linux_tag = format!("{}:linux", self.image);
                let windows_tag = format!("{}:windows-amd64", self.image);
                vec![
                    self.release_job("-linux", "Docker push (linux)")
                        .with_timeout(60)
                        .with_steps(self.buildx_steps(facts, &linux, &linux_tag)),
                    self.release_job("-windows", "Docker push (windows)")
                        .on_windows()
                        .with_timeout(30)
                        .with_steps(self.windows_steps(facts, &windows_tag)),
                    self.release_job("", self.name())
                        .with_timeout(10)
                        .with_needs(vec![
                            format!("{}-linux", self.id()),
                            format!("{}-windows", self.id()),
                        ])
                        .with_steps(vec![
                            Step::run("Login to registry", self.login_command()),
                            Step::run(
                                "Merge manifests",
                                format!(
                                    "docker buildx imagetools create -t {} {linux_tag} {windows_tag}",
                                    self.image
                                ),
                            ),
                        ]),
                ]
            }
        }
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
            platforms: vec![],
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(job.tags_only);
        assert_eq!(job.secrets, vec!["DOCKER_USERNAME", "DOCKER_PASSWORD"]);
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "docker push owner/app")
        ));
    }

    fn run_command<'a>(job: &'a Job, name: &str) -> &'a str {
        job.steps
            .iter()
            .find_map(|s| match s {
                Step::Run { name: n, command } if n == name => Some(command.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("job {} has no step '{name}'", job.id))
    }

    #[test]
    fn test_linux_multiarch_uses_one_buildx_job() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            // Deliberately unsorted; the emitted list is in ALL order
            platforms: vec![DockerPlatform::LinuxArm64, DockerPlatform::LinuxAmd64],
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, "docker-release");
        assert_eq!(job.runs_on, crate::ir::RunnerOs::Linux);
        assert!(job.tags_only);
        assert_eq!(
            run_command(job, "Build and push image"),
            "docker buildx build --platform linux/amd64,linux/arm64 -t owner/app --push ."
        );
        assert_eq!(
            run_command(job, "Set up QEMU"),
            "docker run --privileged --rm tonistiigi/binfmt --install all"
        );
        assert_eq!(run_command(job, "Set up buildx"), "docker buildx create --use");
    }

    #[test]
    fn test_amd64_only_skips_qemu() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::LinuxAmd64],
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(!job
            .steps
            .iter()
            .any(|s| matches!(s, Step::Run { name, .. } if name == "Set up QEMU")));
        assert_eq!(
            run_command(job, "Build and push image"),
            "docker buildx build --platform linux/amd64 -t owner/app --push ."
        );
    }

    #[test]
    fn test_mixed_platforms_stage_and_merge() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64, DockerPlatform::LinuxArm64],
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 3);

        let linux = &jobs[0];
        assert_eq!(linux.id, "docker-release-linux");
        assert_eq!(linux.runs_on, crate::ir::RunnerOs::Linux);
        assert_eq!(
            run_command(linux, "Build and push image"),
            "docker buildx build --platform linux/arm64 -t owner/app:linux --push ."
        );

        let windows = &jobs[1];
        assert_eq!(windows.id, "docker-release-windows");
        assert_eq!(windows.runs_on, crate::ir::RunnerOs::Windows);
        assert_eq!(
            run_command(windows, "Build image"),
            "docker build -t owner/app:windows-amd64 ."
        );
        assert_eq!(
            run_command(windows, "Push image"),
            "docker push owner/app:windows-amd64"
        );

        let merge = &jobs[2];
        assert_eq!(merge.id, "docker-release");
        assert_eq!(
            merge.needs,
            vec!["docker-release-linux", "docker-release-windows"]
        );
        assert_eq!(
            run_command(merge, "Merge manifests"),
            "docker buildx imagetools create -t owner/app owner/app:linux owner/app:windows-amd64"
        );
        // imagetools needs no source checkout
        assert!(!merge
            .steps
            .iter()
            .any(|s| matches!(s, Step::Checkout { .. })));

        // Every job logs in, so every job needs the registry secrets
        for job in &jobs {
            assert!(job.tags_only, "{}", job.id);
            assert_eq!(
                job.secrets,
                vec!["DOCKER_USERNAME", "DOCKER_PASSWORD"],
                "{}",
                job.id
            );
        }
    }

    #[test]
    fn test_windows_only_is_single_windows_job() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64],
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, "docker-release");
        assert_eq!(job.runs_on, crate::ir::RunnerOs::Windows);
        assert_eq!(run_command(job, "Push image"), "docker push owner/app");
    }

    #[test]
    fn test_ghcr_image_uses_github_token() {
        let rule = DockerRelease {
            image: "ghcr.io/owner/app".to_string(),
            platforms: vec![],
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert_eq!(job.secrets, vec!["GITHUB_TOKEN"]);
        assert!(job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.contains("docker login ghcr.io"))
        ));
    }
}
